// SPDX-License-Identifier: MIT
// oneios-audio: the AAudio end of the audio bridge (audio/backend.c explains why it is a process).
// Run by xnuhost as `oneios-audio FD`: FD is a SOCK_SEQPACKET socket carrying audio_proto.h.
//
// Output: 48 kHz S16 stereo periods from the guest go into a ring. A writer thread of this process (normal
// priority) moves them into AAudio in chunks of 240 frames with a blocking AAudioStream_write. What decides the
// latency is the level: the frames in the ring plus those AAudio holds and its server has not read yet.
//   - The server's pulls: a normal mixer track is read by AudioFlinger's mixer a whole mixer period at a time
//     (AAudioStream_getFramesRead jumps by it: 1600 frames, 33 ms, on the phone's primary output [I]), and the
//     mixer takes a track's frames only when it holds a whole period. The writer watches the jumps: their size
//     (the median of the last 16) and the time of the last one. The level the stream needs is then the frames the
//     next pull takes that have come in since the last one, plus a margin (the target): right after a pull only
//     the target. Until the pulls are seen (a new stream) it is the old, phase-blind rule: one AAudio burst plus
//     the target at every moment, which averages half a period more (and up to a whole one) than needed.
//   - Before the guest's first chunk, and after AAudio ran dry (an underrun: the missing part is silence), silence
//     fills the level to that need (priming). Each underrun raises the target by 5 ms, to at most 80 ms; each 20 s
//     (ONEIOS_AUDIO_TARGET_DECAY_S) without one lowers it by 5 ms, back to ONEIOS_AUDIO_TARGET_MS (20).
//   - Clock drift and excess: while the level (averaged over about 1 s) is more than 5 ms above the need, each
//     chunk of 240 ring frames is played as 239 frames (237 above 10 ms: 1.25 %), and as 241 while it is 5 ms
//     below (linear interpolation, never by cutting sound). A new stream starts 20-40 ms above (AudioFlinger's start
//     threshold and the phase of its first pull), which goes in 2-3 s. Until the pulls are seen, the old reference
//     stands: the level 1 s after the start, and a ring above the target plus 10 ms is played faster.
//   - AAudio's buffer size (AudioFlinger's start threshold, and the most a blocking write leaves queued) is a burst
//     plus the target plus one chunk (ONEIOS_AUDIO_BURSTS=N: N bursts instead), set again when the target moves.
//   - Silence is cut, sound is not: while the level is more than 10 ms above the need, a chunk at the ring's head
//     that is digital silence (every sample within +-1) is dropped. A backlog (a new track's stall of about 200 ms,
//     a burst of late periods) thus goes at the first pause of the sound instead of over 16 s of faster playing.
//   - A ring that holds more than the target plus 60 ms drops its oldest audio down to the target (an overflow).
//     Until the server has taken the stream's first frames the limit is the whole ring (500 ms), then what the
//     stall left plus 40 ms, shrinking as it plays: on the phone a cut there lost the first 120 ms of iOS's first
//     sound;
//   - the guest's STOP keeps the stream open for ONEIOS_AUDIO_LINGER_S (60 s): what the ring still holds plays,
//     then silence; iOS starts and stops its device around each sound, and a reopened track stalls again (the
//     open and the new track's first ~200 ms before the server takes frames: a UI sound after a pause came that
//     much later than the rest). Then it is closed. 60 s covers a typing or a browsing session's pauses; the
//     price is the phone's speaker path kept on with silence for that minute.
// Input: a reader thread reads the microphone in periods of 240 frames (blocking AAudioStream_read) and sends each
// one to xnuhost.
//
// The way back to One UI (docs/hardware.md section 6): no thread of this process may run realtime. AAudio gives
// a realtime (SCHED_FIFO) callback thread to an MMAP stream and to a FAST track with a callback, and it asks
// system_server's scheduling_policy service for it; in a session that service is gone. So: no data callback,
// AAudio's MMAP path off for this process (AAudio_setMMapPolicy NEVER). The performance mode is NONE; LOW_LATENCY
// (ONEIOS_AUDIO_PERF=low, not run on the phone yet) is asked only when that MMAP refusal worked: AAudio tries MMAP only for LOW_LATENCY,
// and a stream that reports MMAP anyway is closed and opened again with NONE. Without a callback, libaudioclient
// takes the FAST flag off itself (AOSP AudioTrack::createTrack_l: "AUDIO_OUTPUT_FLAG_FAST denied by client" for a
// synchronous native writer), so the track is a normal mixer track and no priority boost is ever asked; what
// LOW_LATENCY still sends is AUDIO_OUTPUT_FLAG_RAW, which lets the audio policy pick an output that has it
// (docs/hardware.md section 6). And a guard: every thread of the process is checked after each start and every
// 10 s, and one found SCHED_FIFO or SCHED_RR is set back to SCHED_OTHER (logged).
//
// One UI's audioserver and its AAudio service (media.aaudio) keep running in a session. A root process is a
// "service uid" for audioserver, so no app-op is asked for playback; recording needs the microphone's permission
// check to pass for uid 0 (docs/hardware.md section 6 says what the phone showed).
//
// Log lines: "oneios-audio: ..." on stderr (xnuhost's log): each stream's open (its mode), the server's pulls once
// they are seen, target changes, a report every 10 s while a stream runs, and one at its stop: frames, underruns,
// overflows, slips, silence cut, the ring's fill, the level against its need, the latency.
#define _GNU_SOURCE
#include "audio_proto.h"
#include <aaudio/AAudio.h>
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define RATE 48000u
#define RING_FRAMES 24000u                       // 500 ms of stereo at 48 kHz (a new track's stall and a burst)
#define MAX_BURST 9600u                          // the largest AAudio burst taken as it is
#define IN_PERIOD 240u
#define CHUNK 240u                               // the writer's unit (one guest period)
#define FRAMES_MS(f) ((unsigned)((f) * 1000ull / RATE))
#define MS_FRAMES(ms) ((unsigned)((ms) * (RATE / 1000u)))

static int sock;
static unsigned target_ms = 20, target_max_ms = 80, target_decay_s = 20;
static unsigned buffer_bursts;                   // ONEIOS_AUDIO_BURSTS; 0: the buffer follows the need (buffer_fit)
static aaudio_performance_mode_t perf_mode = AAUDIO_PERFORMANCE_MODE_NONE;   // ONEIOS_AUDIO_PERF=low: LOW_LATENCY (not run on the phone yet)
static bool mmap_refused;                        // AAudio_setMMapPolicy(NEVER) worked

// ---- the speaker ring (guarded by ring_lock) ----------------------------------------------------
static int16_t ring[RING_FRAMES * 2];
static unsigned ring_head, ring_count, target;   // frames
static bool priming = true, guest_on;            // guest_on: between the guest's START and STOP
static bool consuming;                           // the server has taken frames from this stream (after its start stall)
static unsigned allowance = RING_FRAMES;         // the most the ring may hold now: the stall's backlog, shrinking as it plays
static unsigned generation;                      // counts the guest's STARTs: the writer measures its level anew
static unsigned linger_s = 60;
static pthread_mutex_t ring_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ring_more = PTHREAD_COND_INITIALIZER;   // put_pcm wakes the writer

struct out_stats {
  uint64_t in, played, underruns, overflows, dropped, slips_up, slips_down, silence;
  unsigned fill_min, fill_max;
  int margin;                                    // the level less what the next pull takes (frames; averaged once the pulls are known)
  unsigned target_peak;                          // the highest target since the start (frames; 0: never raised)
};
static struct out_stats ostat;

// The server's pulls, as AAudioStream_getFramesRead shows them (the writer's own; read by out_report).
typedef struct {
  int64_t read;                                  // frames read when last looked
  double group_at, step_at;                      // the current pull's first and last step
  unsigned group;                                // frames in the current pull so far
  unsigned sizes[16], count;                     // the last complete pulls
  unsigned size;                                 // their median (0: fewer than 4 seen)
  double period;                                 // the time between pulls, averaged (s)
  bool told;                                     // logged once per stream
} pull_state;
static pull_state pulls;

typedef struct {
  AAudioStream *stream;
  pthread_t thread;
  bool thread_on;
  atomic_bool run;
  unsigned channels, burst;
  uint64_t frames;                               // written (output) or read (input) through AAudio
  struct timespec started, reported;
} dir_state;
static dir_state out_dir, in_dir;
static uint64_t in_sent, in_lost, in_short;

static double now_s(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (double)t.tv_sec + t.tv_nsec / 1e9; }
static double since(const struct timespec *t) { return now_s() - ((double)t->tv_sec + t->tv_nsec / 1e9); }

static void report(bool output, int error)
{
  struct au_header header = { .kind = AU_STATE, .output = output, .value = (uint32_t)error };
  // Never block: xnuhost reads its end only while the microphone runs (backend.c), and a full socket would
  // stop this process's receive loop.
  (void)send(sock, &header, sizeof header, MSG_DONTWAIT | MSG_NOSIGNAL);
}

// ---- no realtime threads ------------------------------------------------------------------------
static unsigned demote_realtime(void)
{
  DIR *tasks = opendir("/proc/self/task");
  unsigned demoted = 0;
  if (!tasks) return 0;
  for (struct dirent *e; (e = readdir(tasks));) {
    pid_t tid = (pid_t)atoi(e->d_name);
    if (tid <= 0) continue;
    int policy = sched_getscheduler(tid);
    if (policy < 0) continue;
    policy &= ~SCHED_RESET_ON_FORK;
    if (policy != SCHED_FIFO && policy != SCHED_RR) continue;
    struct sched_param normal = { 0 };
    int rc = sched_setscheduler(tid, SCHED_OTHER, &normal);
    fprintf(stderr, "oneios-audio: thread %d was %s: %s\n", (int)tid, policy == SCHED_FIFO ? "SCHED_FIFO" : "SCHED_RR",
            rc ? strerror(errno) : "set back to SCHED_OTHER");
    demoted++;
  }
  closedir(tasks);
  return demoted;
}

// ---- AAudio ---------------------------------------------------------------------------------------
// AAudioTesting.h: exported by libaaudio (Android 17 on the phone: AAudio_setMMapPolicy, AAudioStream_isMMapUsed);
// looked up at run time, the NDK's stub library does not list them.
static void *aaudio_symbol(const char *name)
{
  void *lib = dlopen("libaaudio.so", RTLD_NOW | RTLD_NOLOAD);
  void *f = lib ? dlsym(lib, name) : NULL;
  return f ? f : dlsym(RTLD_DEFAULT, name);
}

static void mmap_off(void)
{
  mmap_refused = false;
  aaudio_result_t (*set_policy)(int32_t) = (aaudio_result_t (*)(int32_t))aaudio_symbol("AAudio_setMMapPolicy");
  if (!set_policy) { fprintf(stderr, "oneios-audio: AAudio_setMMapPolicy not found: performance mode none (MMAP is only refused by it)\n"); return; }
  aaudio_result_t rc = set_policy(1 /* AAUDIO_POLICY_NEVER */);
  if (rc != AAUDIO_OK) fprintf(stderr, "oneios-audio: AAudio_setMMapPolicy(NEVER): %s: performance mode none\n", AAudio_convertResultToText(rc));
  else mmap_refused = true;
}

static int mmap_used(AAudioStream *stream)
{
  static bool (*used)(AAudioStream *);
  static bool looked;
  if (!looked) { used = (bool (*)(AAudioStream *))aaudio_symbol("AAudioStream_isMMapUsed"); looked = true; }
  return used ? used(stream) : -1;
}

static const char *perf_name(aaudio_performance_mode_t mode)
{
  return mode == AAUDIO_PERFORMANCE_MODE_LOW_LATENCY ? "low latency" : mode == AAUDIO_PERFORMANCE_MODE_POWER_SAVING ? "power saving" : "none";
}

static AAudioStream *open_with(bool output, unsigned rate, unsigned channels, aaudio_performance_mode_t mode)
{
  AAudioStreamBuilder *builder;
  AAudioStream *stream = NULL;
  if (AAudio_createStreamBuilder(&builder) != AAUDIO_OK) return NULL;
  AAudioStreamBuilder_setDirection(builder, output ? AAUDIO_DIRECTION_OUTPUT : AAUDIO_DIRECTION_INPUT);
  AAudioStreamBuilder_setSampleRate(builder, (int32_t)rate);
  AAudioStreamBuilder_setChannelCount(builder, (int32_t)channels);
  AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
  AAudioStreamBuilder_setPerformanceMode(builder, mode);
  AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
  if (output) { AAudioStreamBuilder_setUsage(builder, AAUDIO_USAGE_MEDIA); AAudioStreamBuilder_setContentType(builder, AAUDIO_CONTENT_TYPE_MUSIC); }
  else AAudioStreamBuilder_setInputPreset(builder, AAUDIO_INPUT_PRESET_VOICE_RECOGNITION);
  aaudio_result_t result = AAudioStreamBuilder_openStream(builder, &stream);
  AAudioStreamBuilder_delete(builder);
  if (result != AAUDIO_OK) { fprintf(stderr, "oneios-audio: open %s: %s\n", output ? "speaker" : "microphone", AAudio_convertResultToText(result)); return NULL; }
  return stream;
}

static AAudioStream *open_stream(bool output, unsigned rate, unsigned channels)
{
  // The speaker asks LOW_LATENCY only with MMAP refused; the microphone stays NONE.
  aaudio_performance_mode_t asked = output && mmap_refused ? perf_mode : AAUDIO_PERFORMANCE_MODE_NONE;
  AAudioStream *stream = open_with(output, rate, channels, asked);
  if (stream && asked != AAUDIO_PERFORMANCE_MODE_NONE && mmap_used(stream) == 1) {
    fprintf(stderr, "oneios-audio: %s: MMAP in use despite the policy: opened again with performance mode none\n", output ? "speaker" : "microphone");
    AAudioStream_close(stream);
    asked = AAUDIO_PERFORMANCE_MODE_NONE;
    stream = open_with(output, rate, channels, asked);
  }
  if (!stream) return NULL;
  int32_t burst = AAudioStream_getFramesPerBurst(stream);
  // AAudio's own buffer: ONEIOS_AUDIO_BURSTS bursts, or (by default) a burst plus the target plus a chunk, as the
  // writer keeps it (buffer_fit).
  if (output && burst > 0) {
    int32_t frames = buffer_bursts ? burst * (int32_t)buffer_bursts : burst + (int32_t)(MS_FRAMES(target_ms) + CHUNK);
    int32_t capacity = AAudioStream_getBufferCapacityInFrames(stream);
    AAudioStream_setBufferSizeInFrames(stream, capacity > 0 && frames > capacity ? capacity : frames);
  }
  fprintf(stderr, "oneios-audio: %s open: %d Hz, %d channel(s), %s asked, %s given, sharing %s, mmap %d, burst %d, buffer %d of %d frames, device %d, session %d\n",
          output ? "speaker" : "microphone", AAudioStream_getSampleRate(stream), AAudioStream_getChannelCount(stream),
          perf_name(asked), perf_name(AAudioStream_getPerformanceMode(stream)),
          AAudioStream_getSharingMode(stream) == AAUDIO_SHARING_MODE_EXCLUSIVE ? "exclusive" : "shared", mmap_used(stream), burst,
          AAudioStream_getBufferSizeInFrames(stream), AAudioStream_getBufferCapacityInFrames(stream), AAudioStream_getDeviceId(stream),
          AAudioStream_getSessionId(stream));
  aaudio_result_t result = AAudioStream_requestStart(stream);
  if (result != AAUDIO_OK) { fprintf(stderr, "oneios-audio: start %s: %s\n", output ? "speaker" : "microphone", AAudio_convertResultToText(result)); AAudioStream_close(stream); return NULL; }
  return stream;
}

// Frames written but not yet played by the DAC, from AAudio's timestamp (-1: no timestamp yet).
static int pipeline_frames(dir_state *d)
{
  int64_t position, when;
  if (AAudioStream_getTimestamp(d->stream, CLOCK_MONOTONIC, &position, &when) != AAUDIO_OK) return -1;
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  int64_t now = (int64_t)t.tv_sec * 1000000000 + t.tv_nsec;
  int64_t played = position + (now - when) * (int64_t)RATE / 1000000000;
  int64_t queued = (int64_t)d->frames - played;
  return queued < 0 ? 0 : (int)queued;
}

static void out_report(const char *when)
{
  pthread_mutex_lock(&ring_lock);
  struct out_stats s = ostat;
  unsigned fill = ring_count, aim = target, pull = pulls.size;
  double period = pulls.period;
  ostat.fill_min = ostat.fill_max = ring_count;
  pthread_mutex_unlock(&ring_lock);
  int queued = out_dir.stream ? pipeline_frames(&out_dir) : -1;
  int xruns = out_dir.stream ? AAudioStream_getXRunCount(out_dir.stream) : 0;
  char pulled[64];
  if (pull) snprintf(pulled, sizeof pulled, "server pulls %u frames every %.1f ms", pull, period * 1e3);
  else snprintf(pulled, sizeof pulled, "server pulls not seen");
  fprintf(stderr, "oneios-audio: speaker %s: %.1f s, %llu frames in, %llu played, %llu underruns, %llu overflows (%llu frames dropped), "
          "slips +%llu/-%llu, %llu frames of silence cut, ring %u ms (%u..%u, target %u, peak %u), margin %d ms, %s, AAudio %d ms queued, %d xruns; "
          "latency about %d ms\n",
          when, since(&out_dir.started), (unsigned long long)s.in, (unsigned long long)s.played, (unsigned long long)s.underruns,
          (unsigned long long)s.overflows, (unsigned long long)s.dropped, (unsigned long long)s.slips_up, (unsigned long long)s.slips_down,
          (unsigned long long)s.silence, FRAMES_MS(fill), FRAMES_MS(s.fill_min), FRAMES_MS(s.fill_max), FRAMES_MS(aim),
          FRAMES_MS(s.target_peak > aim ? s.target_peak : aim), s.margin * 1000 / (int)RATE, pulled, queued < 0 ? -1 : (int)FRAMES_MS(queued), xruns,
          queued < 0 ? -1 : (int)FRAMES_MS((unsigned)queued + fill));
}

// One burst of output from the ring: `want` frames played from `take` frames of the ring (take = want, or one
// more or fewer to steer the fill toward the target), linear interpolation between them.
static void resample(int16_t *out, unsigned want, const int16_t *in, unsigned take)
{
  // A slip (a few frames more or fewer) is interpolated; a short chunk (an underrun, a sound's tail) is padded.
  if (take == want || take < 2 || want < 2 || take + 4 < want || take > want + 4) { memcpy(out, in, (take < want ? take : want) * 4u); if (take < want) memset(out + 2 * take, 0, (want - take) * 4u); return; }
  for (unsigned i = 0; i < want; i++) {
    uint64_t pos = (uint64_t)i * (take - 1) * 65536u / (want - 1);
    unsigned a = (unsigned)(pos >> 16), b = a + 1 < take ? a + 1 : a, f = (unsigned)(pos & 0xffff);
    for (unsigned c = 0; c < 2; c++) out[2 * i + c] = (int16_t)((in[2 * a + c] * (int32_t)(65536 - f) + in[2 * b + c] * (int32_t)f) >> 16);
  }
}

// Frames written to AAudio and not yet read by its server (what the next mixer cycles take).
static int64_t queued_frames(dir_state *d, int64_t read) { int64_t q = AAudioStream_getFramesWritten(d->stream) - read; return q < 0 ? 0 : q; }

// ---- the server's pulls ----------------------------------------------------------------------------
static int cmp_unsigned(const void *a, const void *b) { unsigned x = *(const unsigned *)a, y = *(const unsigned *)b; return x < y ? -1 : x > y; }

static void pulls_reset(pull_state *p, int64_t read) { *p = (pull_state){ .read = read }; }

// A step of frames read: steps less than 3 ms apart are one pull (a mixer may release a period in pieces).
static void pulls_saw(pull_state *p, int64_t read, double now)
{
  if (read < p->read) { pulls_reset(p, read); return; }
  if (read == p->read) return;
  unsigned step = (unsigned)(read - p->read);
  p->read = read;
  if (p->group && now - p->step_at < 0.003) { p->group += step; p->step_at = now; return; }
  if (p->group) {
    double gap = now - p->group_at;
    p->period = p->count ? p->period + (gap - p->period) / 8 : gap;
    p->sizes[p->count++ % 16] = p->group;
    unsigned n = p->count < 16 ? p->count : 16, sorted[16];
    memcpy(sorted, p->sizes, n * sizeof *sorted);
    qsort(sorted, n, sizeof *sorted, cmp_unsigned);
    p->size = p->count >= 4 ? sorted[n / 2] : 0;
  }
  p->group = step;
  p->group_at = p->step_at = now;
}

// The frames the next pull takes that have come in since the last one (the need above the target), or -1 when
// the pulls are not known (not yet seen, or stopped for longer than three of them).
static int pulls_ahead(const pull_state *p, double now)
{
  if (!p->size) return -1;
  double elapsed = now - p->group_at;
  if (elapsed > 3.0 * p->size / RATE + 0.01) return -1;
  double frames = elapsed * RATE;
  return frames < p->size ? (int)frames : (int)p->size;
}

// AAudio's buffer size is AudioFlinger's start threshold (a new track is not read until it holds that much) and
// the most a blocking write leaves in it. Fitted to the most the level needs (a burst, which is at least a pull,
// plus the target) and a chunk: the start then adds no more than that chunk, and a backlog waits in the ring,
// where its silence can be cut. Set again when the target moves; ONEIOS_AUDIO_BURSTS fixes it instead.
static void buffer_fit(dir_state *d, unsigned aim)
{
  if (buffer_bursts) return;
  int32_t capacity = AAudioStream_getBufferCapacityInFrames(d->stream), frames = (int32_t)(d->burst + aim + CHUNK);
  if (capacity > 0 && frames > capacity) frames = capacity;
  AAudioStream_setBufferSizeInFrames(d->stream, frames);
}

static bool silent(unsigned at, unsigned frames)
{
  for (unsigned i = 0; i < frames; i++) {
    const int16_t *f = &ring[2 * ((at + i) % RING_FRAMES)];
    if (f[0] > 1 || f[0] < -1 || f[1] > 1 || f[1] < -1) return false;
  }
  return true;
}

// The writer moves CHUNK frames at a time from the ring to AAudio (a blocking write: the server paces it). It
// waits while the ring has less than a chunk and AAudio still holds what its next pull needs; only when AAudio is
// about to run dry does it play what the ring has and silence for the rest (an underrun), and prime again.
// Priming (and while the guest is stopped): silence keeps AAudio's level at the need (the pull's part plus the
// target); the guest's audio starts on that level, so the pull after it still finds a margin of the target.
static void *writer(void *arg)
{
  (void)arg;
  dir_state *d = &out_dir;
  static int16_t chunk[(CHUNK + 4) * 2], from[(CHUNK + 4) * 2];
  const int64_t low_blind = d->burst / 2;
  double level = -1, reference = -1;                     // the level (less the pull's part), averaged over about 200 chunks (1 s)
  unsigned settled = 0, seen = 0;
  double ring_average = 0, quiet_since = now_s();        // quiet_since: the last underrun or target change
  bool phase = false;
  pulls_reset(&pulls, AAudioStream_getFramesRead(d->stream));
  pthread_mutex_lock(&ring_lock);
  unsigned fitted = target;
  pthread_mutex_unlock(&ring_lock);
  buffer_fit(d, fitted);
  while (atomic_load(&d->run)) {
    int64_t read = AAudioStream_getFramesRead(d->stream);
    int64_t queued = queued_frames(d, read);
    double now = now_s();
    unsigned take = 0, length = CHUNK;
    bool play = false, cut = false;
    pthread_mutex_lock(&ring_lock);
    pulls_saw(&pulls, read, now);
    int ahead = pulls_ahead(&pulls, now);
    if (ahead >= 0 && !pulls.told) {
      pulls.told = true;
      fprintf(stderr, "oneios-audio: speaker: the server pulls %u frames every %.1f ms: the level follows its pulls\n", pulls.size, pulls.period * 1e3);
    }
    if ((ahead >= 0) != phase) { phase = ahead >= 0; level = reference = -1; settled = 0; }
    bool taken = read > 0;
    if (taken && !consuming) allowance = ring_count;    // the stall is over: what it left is the allowance
    consuming = taken;
    if (consuming) {
      unsigned floor = target + MS_FRAMES(60), keep = ring_count > floor ? ring_count : floor;
      if (keep < allowance) allowance = keep;
    }
    if (seen != generation) { seen = generation; level = reference = -1; settled = 0; ring_average = 0; }
    // The target back toward its floor after a quiet while.
    if (target > MS_FRAMES(target_ms) && now - quiet_since >= target_decay_s) {
      target = target > MS_FRAMES(target_ms + 5) ? target - MS_FRAMES(5) : MS_FRAMES(target_ms);
      quiet_since = now;
      level = reference = -1; settled = 0;
      fprintf(stderr, "oneios-audio: speaker: target %u ms (%u s without an underrun)\n", FRAMES_MS(target), target_decay_s);
    }
    // The need: what the next pull takes from what came in since the last one, plus the target. Blind: a burst
    // plus the target, at every moment.
    const int64_t need = (int64_t)target + (phase ? ahead : (int64_t)d->burst);
    const int64_t low = phase ? ahead : low_blind;
    const int64_t margin = (int64_t)ring_count + queued - (phase ? ahead : (int64_t)d->burst);
    ostat.margin = level >= 0 && phase ? (int)level : (int)margin;
    if (!priming && ring_count >= CHUNK && margin > (int64_t)target + MS_FRAMES(10) && silent(ring_head, CHUNK)) {
      cut = true;                                        // a backlog's silence: dropped, the level comes down at once
      ring_head = (ring_head + CHUNK) % RING_FRAMES;
      ring_count -= CHUNK;
      ostat.silence += CHUNK;
      if (level >= 0) level -= CHUNK;
      ring_average -= CHUNK;
    }
    else if (priming) {
      // Before the guest's first chunk (and while it is stopped) AAudio is kept at the need with silence: a track
      // left empty is disabled by the mixer, and the level the guest's audio starts on is that.
      if (ring_count >= CHUNK && (int64_t)ring_count + queued >= need) priming = false;
      else if (queued < need) {                                                       // silence, as much as is missing
        play = true;
        int64_t missing = need - queued;
        length = missing >= CHUNK ? CHUNK : missing < 16 ? 16 : (unsigned)missing;
      }
    }
    if (play || priming || cut) {}
    else if (ring_count >= CHUNK) {
      play = true;
      double now_level = (double)margin;
      level = level < 0 ? now_level : level + (now_level - level) / 200.0;
      ring_average += (ring_count - ring_average) / 200.0;
      take = CHUNK;
      // A slip: the chunk's 240 ring frames are played as 239 or 237 (the level comes down by 1 or 3) or as 241 (up).
      if (phase) {
        // The level's aim is the target above what the next pull takes: one frame fewer per chunk while it is 5 ms
        // above (drift), three while 10 ms above (a start: the track's start threshold and its first pull's phase
        // leave 20-40 ms; a backlog), one more while 5 ms below.
        if (level > target + MS_FRAMES(10)) { length = CHUNK - 3; ostat.slips_down += 3; level -= 3; }
        else if (level > target + MS_FRAMES(5)) { length = CHUNK - 1; ostat.slips_down++; level -= 1; }
        else if (level + MS_FRAMES(5) < target) { length = CHUNK + 1; ostat.slips_up++; level += 1; }
      } else {
        // Blind (the pulls not seen yet): the level's reference is its value 1 s after the start; fewer frames
        // while the ring holds more than it needs (after a stall or a burst of late periods: that is latency),
        // three fewer while it holds 40 ms more.
        if (reference < 0 && ++settled == 200) reference = level;
        if (ring_average > target + MS_FRAMES(40)) { length = CHUNK - 3; ostat.slips_down += 3; level -= 3; ring_average -= 3; }
        else if ((reference >= 0 && level > reference + MS_FRAMES(5)) || ring_average > target + MS_FRAMES(10)) {
          length = CHUNK - 1; ostat.slips_down++; level -= 1; ring_average -= 1;
        }
        else if (reference >= 0 && level + MS_FRAMES(5) < reference) { length = CHUNK + 1; ostat.slips_up++; level += 1; }
      }
    } else if (!guest_on) {                              // the guest stopped: its tail, then silence
      play = true;
      take = ring_count;
      if (!take) priming = true;
    } else if (queued < low) {                           // AAudio is about to run dry
      play = true;
      take = ring_count;
      priming = true;
      ostat.underruns++;
      unsigned raised = target + MS_FRAMES(5);
      target = raised > MS_FRAMES(target_max_ms) ? MS_FRAMES(target_max_ms) : raised;
      if (target > ostat.target_peak) ostat.target_peak = target;
      quiet_since = now;
      level = reference = -1;
      settled = 0;
    }
    for (unsigned i = 0; i < take; i++) {
      from[2 * i] = ring[2 * ring_head];
      from[2 * i + 1] = ring[2 * ring_head + 1];
      ring_head = (ring_head + 1) % RING_FRAMES;
    }
    ring_count -= take;
    ostat.played += take;
    if (play && ring_count < ostat.fill_min) ostat.fill_min = ring_count;
    if (!play && !cut) {                                 // nothing to do: wait for the guest's next period (or 2 ms)
      struct timespec until;
      clock_gettime(CLOCK_REALTIME, &until);
      until.tv_nsec += 2000000;
      if (until.tv_nsec >= 1000000000) { until.tv_sec++; until.tv_nsec -= 1000000000; }
      pthread_cond_timedwait(&ring_more, &ring_lock, &until);
    }
    unsigned aim = target;
    pthread_mutex_unlock(&ring_lock);
    if (aim != fitted) { fitted = aim; buffer_fit(d, aim); }
    if (play) {
      resample(chunk, length, from, take);
      for (unsigned done = 0; done < length && atomic_load(&d->run);) {
        aaudio_result_t n = AAudioStream_write(d->stream, chunk + 2 * done, (int32_t)(length - done), 200000000);   // 200 ms
        if (n < 0) {
          fprintf(stderr, "oneios-audio: speaker write: %s\n", AAudio_convertResultToText(n));
          report(true, EIO);
          struct timespec nap = { 0, 50000000 };
          nanosleep(&nap, NULL);
          break;
        }
        done += (unsigned)n;                             // 0: the server took nothing for 200 ms (a stall): again
        d->frames += (uint64_t)n;
      }
    }
    if (since(&d->reported) >= 10) { clock_gettime(CLOCK_MONOTONIC, &d->reported); out_report("running"); demote_realtime(); }
  }
  return NULL;
}

static void *reader(void *arg)
{
  (void)arg;
  dir_state *d = &in_dir;
  struct { struct au_header header; int16_t pcm[AU_MAX_FRAMES]; } packet;
  static int16_t frames[IN_PERIOD * 8];
  unsigned channels = d->channels ? d->channels : 1;
  if (channels > 8) channels = 8;
  while (atomic_load(&d->run)) {
    aaudio_result_t n = AAudioStream_read(d->stream, frames, IN_PERIOD, 200000000);
    if (n < 0) {
      fprintf(stderr, "oneios-audio: microphone read: %s\n", AAudio_convertResultToText(n));
      report(false, EIO);
      struct timespec nap = { 0, 50000000 };
      nanosleep(&nap, NULL);
      continue;
    }
    if (n == 0) continue;
    if ((unsigned)n < IN_PERIOD) in_short++;
    d->frames += (uint64_t)n;
    packet.header = (struct au_header){ .kind = AU_PCM, .output = 0, .channels = 1, .value = (uint32_t)n };
    for (int i = 0; i < n; i++) packet.pcm[i] = frames[(unsigned)i * channels];         // the first channel
    if (send(sock, &packet, sizeof packet.header + (size_t)n * 2, MSG_DONTWAIT | MSG_NOSIGNAL) < 0) in_lost++;
    else in_sent++;
    if (since(&d->reported) >= 10) {
      clock_gettime(CLOCK_MONOTONIC, &d->reported);
      fprintf(stderr, "oneios-audio: microphone running: %.1f s, %llu frames, %llu periods sent, %llu lost (xnuhost behind), %llu short, %d xruns\n",
              since(&d->started), (unsigned long long)d->frames, (unsigned long long)in_sent, (unsigned long long)in_lost,
              (unsigned long long)in_short, AAudioStream_getXRunCount(d->stream));
      demote_realtime();
    }
  }
  return NULL;
}

static void stop_dir(bool output)
{
  dir_state *d = output ? &out_dir : &in_dir;
  if (!d->stream) return;
  if (output) { pthread_mutex_lock(&ring_lock); guest_on = false; pthread_mutex_unlock(&ring_lock); }
  atomic_store(&d->run, false);
  if (d->thread_on) { pthread_join(d->thread, NULL); d->thread_on = false; }
  if (output) out_report("stopped");
  else fprintf(stderr, "oneios-audio: microphone stopped: %.1f s, %llu frames, %llu periods sent, %llu lost, %llu short\n", since(&d->started),
               (unsigned long long)d->frames, (unsigned long long)in_sent, (unsigned long long)in_lost, (unsigned long long)in_short);
  AAudioStream_requestStop(d->stream);
  AAudioStream_close(d->stream);
  d->stream = NULL;
}

// The guest's STOP of the speaker: the stream plays silence and stays open for linger_s.
static struct timespec idle_since;
static void pause_output(void)
{
  if (!out_dir.stream || !guest_on) return;
  out_report("stopped by the guest");
  pthread_mutex_lock(&ring_lock);
  guest_on = false;                                      // the writer plays what the ring holds, then silence
  pthread_mutex_unlock(&ring_lock);
  clock_gettime(CLOCK_MONOTONIC, &idle_since);
}

static int start_dir(bool output, unsigned rate, unsigned channels)
{
  dir_state *d = output ? &out_dir : &in_dir;
  if (output && d->stream && rate == (unsigned)AAudioStream_getSampleRate(d->stream) && channels == d->channels) {
    pthread_mutex_lock(&ring_lock);                      // the lingering stream: on at once (a tail still in the ring plays first)
    guest_on = true;
    generation++;
    memset(&ostat, 0, sizeof ostat);
    pthread_mutex_unlock(&ring_lock);
    clock_gettime(CLOCK_MONOTONIC, &d->started);
    return 0;
  }
  stop_dir(output);
  if (output) {
    pthread_mutex_lock(&ring_lock);
    consuming = false;
    allowance = RING_FRAMES;
    ring_head = ring_count = 0;
    priming = true;
    guest_on = true;
    generation++;
    target = MS_FRAMES(target_ms);
    memset(&ostat, 0, sizeof ostat);
    pthread_mutex_unlock(&ring_lock);
  } else in_sent = in_lost = in_short = 0;
  d->stream = open_stream(output, rate, channels);
  if (!d->stream) return EIO;
  int32_t burst = AAudioStream_getFramesPerBurst(d->stream);
  d->burst = burst >= 16 && burst <= (int32_t)MAX_BURST ? (unsigned)burst : 240u;
  d->channels = (unsigned)AAudioStream_getChannelCount(d->stream);
  d->frames = 0;
  clock_gettime(CLOCK_MONOTONIC, &d->started);
  d->reported = d->started;
  atomic_store(&d->run, true);
  d->thread_on = pthread_create(&d->thread, NULL, output ? writer : reader, NULL) == 0;
  if (!d->thread_on) { stop_dir(output); return EAGAIN; }
  demote_realtime();
  return 0;
}

static void put_pcm(const int16_t *pcm, unsigned frames)
{
  pthread_mutex_lock(&ring_lock);
  ostat.in += frames;
  if (!guest_on) { pthread_mutex_unlock(&ring_lock); return; }
  // Before the server takes its first frames (a new track's stall, about 200 ms) the whole ring holds: the start of
  // a short sound is all there is of it.
  unsigned limit = !consuming ? RING_FRAMES : allowance + MS_FRAMES(40) > target + MS_FRAMES(60) ? allowance + MS_FRAMES(40) : target + MS_FRAMES(60);
  if (limit > RING_FRAMES) limit = RING_FRAMES;
  if (ring_count + frames > limit) {                   // far ahead of the DAC: drop the oldest audio down to the target
    unsigned keep = target < ring_count ? target : ring_count;
    if (keep + frames > RING_FRAMES) keep = RING_FRAMES - frames;
    ring_head = (ring_head + ring_count - keep) % RING_FRAMES;
    ostat.overflows++;
    generation++;                                      // the level after a stall is no reference for the drift
    ostat.dropped += ring_count - keep;
    ring_count = keep;
  }
  for (unsigned i = 0; i < frames; i++) {
    unsigned at = (ring_head + ring_count + i) % RING_FRAMES;
    ring[2 * at] = pcm[2 * i];
    ring[2 * at + 1] = pcm[2 * i + 1];
  }
  ring_count += frames;
  if (ring_count > ostat.fill_max) ostat.fill_max = ring_count;
  pthread_cond_signal(&ring_more);
  pthread_mutex_unlock(&ring_lock);
}

static unsigned env_ms(const char *name, unsigned fallback, unsigned low, unsigned high)
{
  const char *v = getenv(name);
  if (!v || !*v) return fallback;
  unsigned n = (unsigned)strtoul(v, NULL, 10);
  return n < low ? low : n > high ? high : n;
}

int main(int argc, char **argv)
{
  if (argc != 2) { fprintf(stderr, "usage: oneios-audio SOCKET_FD (started by xnuhost)\n"); return 2; }
  sock = atoi(argv[1]);
  setvbuf(stderr, NULL, _IOLBF, 0);
  target_ms = env_ms("ONEIOS_AUDIO_TARGET_MS", 20, 5, 150);
  if (target_max_ms < target_ms) target_max_ms = target_ms;
  target_decay_s = env_ms("ONEIOS_AUDIO_TARGET_DECAY_S", 20, 1, 3600);
  buffer_bursts = env_ms("ONEIOS_AUDIO_BURSTS", 0, 1, 8);
  linger_s = env_ms("ONEIOS_AUDIO_LINGER_S", 60, 0, 600);
  const char *perf = getenv("ONEIOS_AUDIO_PERF");
  perf_mode = perf && !strcmp(perf, "low") ? AAUDIO_PERFORMANCE_MODE_LOW_LATENCY : AAUDIO_PERFORMANCE_MODE_NONE;
  mmap_off();
  char buffer[48];
  if (buffer_bursts) snprintf(buffer, sizeof buffer, "%u bursts", buffer_bursts);
  else snprintf(buffer, sizeof buffer, "a burst + the target + 5 ms");
  fprintf(stderr, "oneios-audio: ready: performance mode %s (a blocking writer: no FAST track, no realtime thread), MMAP %s, AAudio buffer %s, "
          "ring %u ms, target %u ms (at most %u, down 5 ms per %u s without an underrun), linger %u s\n",
          perf_name(mmap_refused ? perf_mode : AAUDIO_PERFORMANCE_MODE_NONE), mmap_refused ? "off" : "not refused", buffer,
          FRAMES_MS(RING_FRAMES), target_ms, target_max_ms, target_decay_s, linger_s);
  static struct { struct au_header header; int16_t pcm[AU_MAX_FRAMES * 2]; } packet;
  for (;;) {
    struct pollfd wait = { .fd = sock, .events = POLLIN };
    if (poll(&wait, 1, 1000) == 0) {                       // a quiet second: close a stream that lingered long enough
      if (out_dir.stream && !guest_on && since(&idle_since) >= linger_s) {
        stop_dir(true);
        fprintf(stderr, "oneios-audio: speaker closed after %u s of silence\n", linger_s);
      }
      continue;
    }
    ssize_t got = recv(sock, &packet, sizeof packet, MSG_DONTWAIT);
    if (got < 0 && errno == EAGAIN) continue;
    if (got == 0) break;                                   // xnuhost closed its end (or exited)
    if (got < 0) { if (errno == EINTR) continue; break; }
    if ((size_t)got < sizeof packet.header) continue;
    bool output = packet.header.output;
    switch (packet.header.kind) {
    case AU_START: {
      int rc = start_dir(output, packet.header.value, packet.header.channels);
      if (rc) report(output, rc);
      break;
    }
    case AU_STOP:
      if (output && linger_s) pause_output();
      else stop_dir(output);
      break;
    case AU_PCM: {
      unsigned frames = (unsigned)(((size_t)got - sizeof packet.header) / 4);
      if (frames > packet.header.value) frames = packet.header.value;
      if (output && out_dir.stream && frames) put_pcm(packet.pcm, frames);
      break;
    }
    }
  }
  stop_dir(true);
  stop_dir(false);
  fprintf(stderr, "oneios-audio: socket closed: exit\n");
  return 0;
}
