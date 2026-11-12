// SPDX-License-Identifier: MIT
// Host test of the AAudio helper (audio/aaudio_helper.c, compiled in here with its main renamed) against a fake
// AAudio: a DAC that takes bursts of 1600 frames from a buffer of 2 bursts at its own rate (the phone's
// deep-buffer track: burst 1600, buffer 3200), and a microphone that gives 48 kHz in real time. A producer plays
// the guest: 240-frame periods every 5 ms on a clock that runs faster or slower than the DAC's.
// Checks: no underrun of the helper or of the DAC after the start, no overflow, the drift steered the right way,
// the latency bounded, the microphone's periods reach the socket, the stop is clean.
#define main helper_main
#include "../audio/aaudio_helper.c"
#undef main
#include <math.h>

static int failures, checks;
#define CHECK(c) do { checks++; if (!(c)) { failures++; fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

// ---- the fake AAudio ----------------------------------------------------------------------------
struct AAudioStreamBuilderStruct { int direction, rate, channels; };
struct AAudioStreamStruct {
  int direction, rate, channels, burst, buffer;
  bool started;
  struct timespec t0;
  int64_t written, skipped, read;
  uint64_t dac_underflows, nonzero;
};
static double dac_ppm;                                  // the DAC's rate against the producer's clock
static double dac_stall_s;                              // a new track takes nothing for this long after its first write

static double elapsed(const struct timespec *t0) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return (t.tv_sec - t0->tv_sec) + (t.tv_nsec - t0->tv_nsec) / 1e9; }
// Frames the DAC has taken: whole bursts on its clock; when it finds less than a burst, it plays silence (an
// underflow) and the missing frames are skipped.
static int64_t pulled(AAudioStream *s)
{
  if (!s->started) return 0;
  double t = elapsed(&s->t0) - (s->direction == AAUDIO_DIRECTION_OUTPUT ? dac_stall_s : 0);
  if (t < 0) return 0;
  int64_t at = (int64_t)(t * s->rate * (1 + dac_ppm / 1e6) / s->burst) * s->burst - s->skipped;
  if (at > s->written) { s->dac_underflows++; s->skipped += at - s->written; at = s->written; }
  return at;
}

aaudio_result_t AAudio_createStreamBuilder(AAudioStreamBuilder **b) { *b = calloc(1, sizeof **b); return AAUDIO_OK; }
const char *AAudio_convertResultToText(aaudio_result_t r) { return r ? "error" : "ok"; }
void AAudioStreamBuilder_setDirection(AAudioStreamBuilder *b, aaudio_direction_t v) { b->direction = v; }
void AAudioStreamBuilder_setSampleRate(AAudioStreamBuilder *b, int32_t v) { b->rate = v; }
void AAudioStreamBuilder_setChannelCount(AAudioStreamBuilder *b, int32_t v) { b->channels = v; }
void AAudioStreamBuilder_setFormat(AAudioStreamBuilder *b, aaudio_format_t v) { (void)b; (void)v; }
void AAudioStreamBuilder_setPerformanceMode(AAudioStreamBuilder *b, aaudio_performance_mode_t v) { (void)b; (void)v; }
void AAudioStreamBuilder_setSharingMode(AAudioStreamBuilder *b, aaudio_sharing_mode_t v) { (void)b; (void)v; }
void AAudioStreamBuilder_setUsage(AAudioStreamBuilder *b, aaudio_usage_t v) { (void)b; (void)v; }
void AAudioStreamBuilder_setContentType(AAudioStreamBuilder *b, aaudio_content_type_t v) { (void)b; (void)v; }
void AAudioStreamBuilder_setInputPreset(AAudioStreamBuilder *b, aaudio_input_preset_t v) { (void)b; (void)v; }
aaudio_result_t AAudioStreamBuilder_delete(AAudioStreamBuilder *b) { free(b); return AAUDIO_OK; }
static AAudioStream *last_out, *last_in;
aaudio_result_t AAudioStreamBuilder_openStream(AAudioStreamBuilder *b, AAudioStream **out)
{
  AAudioStream *s = calloc(1, sizeof *s);
  s->direction = b->direction; s->rate = b->rate; s->channels = b->channels;
  s->burst = s->direction == AAUDIO_DIRECTION_OUTPUT ? 1600 : 960;
  s->buffer = 3 * s->burst;
  *out = s;
  if (s->direction == AAUDIO_DIRECTION_OUTPUT) last_out = s; else last_in = s;
  return AAUDIO_OK;
}
aaudio_result_t AAudioStream_requestStart(AAudioStream *s) { clock_gettime(CLOCK_MONOTONIC, &s->t0); s->started = s->direction == AAUDIO_DIRECTION_INPUT; return AAUDIO_OK; }
aaudio_result_t AAudioStream_requestStop(AAudioStream *s) { (void)s; return AAUDIO_OK; }
aaudio_result_t AAudioStream_close(AAudioStream *s) { (void)s; return AAUDIO_OK; }    // kept for the checks
aaudio_result_t AAudioStream_write(AAudioStream *s, const void *buffer, int32_t frames, int64_t timeout_ns)
{
  // The DAC starts with the first write, as a track starts when its buffer gets data.
  if (!s->started) { clock_gettime(CLOCK_MONOTONIC, &s->t0); s->started = true; }
  struct timespec t0;
  clock_gettime(CLOCK_MONOTONIC, &t0);
  while (s->written + frames - pulled(s) > s->buffer) {
    if (elapsed(&t0) * 1e9 > timeout_ns) return 0;
    struct timespec nap = { 0, 500000 };
    nanosleep(&nap, NULL);
  }
  const int16_t *pcm = buffer;
  for (int i = 0; i < frames * s->channels; i++) if (pcm[i]) s->nonzero++;
  s->written += frames;
  return frames;
}
aaudio_result_t AAudioStream_read(AAudioStream *s, void *buffer, int32_t frames, int64_t timeout_ns)
{
  (void)timeout_ns;
  while ((int64_t)(elapsed(&s->t0) * s->rate) < s->read + frames) { struct timespec nap = { 0, 500000 }; nanosleep(&nap, NULL); }
  int16_t *pcm = buffer;
  for (int i = 0; i < frames * s->channels; i++) pcm[i] = (int16_t)(100 + (s->read + i / s->channels) % 1000);
  s->read += frames;
  return frames;
}
aaudio_result_t AAudioStream_setBufferSizeInFrames(AAudioStream *s, int32_t frames) { if (frames > 0 && frames <= 3 * s->burst) s->buffer = frames; return s->buffer; }
aaudio_result_t AAudioStream_getTimestamp(AAudioStream *s, clockid_t clock, int64_t *position, int64_t *time_ns)
{
  struct timespec t;
  clock_gettime(clock, &t);
  *position = pulled(s);
  *time_ns = (int64_t)t.tv_sec * 1000000000 + t.tv_nsec;
  return s->started ? AAUDIO_OK : AAUDIO_ERROR_INVALID_STATE;
}
int32_t AAudioStream_getFramesPerBurst(AAudioStream *s) { return s->burst; }
int32_t AAudioStream_getBufferSizeInFrames(AAudioStream *s) { return s->buffer; }
int32_t AAudioStream_getBufferCapacityInFrames(AAudioStream *s) { return 3 * s->burst; }
int32_t AAudioStream_getSampleRate(AAudioStream *s) { return s->rate; }
int32_t AAudioStream_getChannelCount(AAudioStream *s) { return s->channels; }
int32_t AAudioStream_getDeviceId(AAudioStream *s) { (void)s; return 3; }
int32_t AAudioStream_getSessionId(AAudioStream *s) { (void)s; return -1; }
int32_t AAudioStream_getXRunCount(AAudioStream *s) { return (int32_t)s->dac_underflows; }
aaudio_sharing_mode_t AAudioStream_getSharingMode(AAudioStream *s) { (void)s; return AAUDIO_SHARING_MODE_SHARED; }
aaudio_performance_mode_t AAudioStream_getPerformanceMode(AAudioStream *s) { (void)s; return AAUDIO_PERFORMANCE_MODE_NONE; }
int64_t AAudioStream_getFramesWritten(AAudioStream *s) { return s->written; }
int64_t AAudioStream_getFramesRead(AAudioStream *s) { return pulled(s); }

// ---- the test -----------------------------------------------------------------------------------
static int guest_fd;
static void *run_helper(void *arg)
{
  char fd[16];
  snprintf(fd, sizeof fd, "%d", *(int *)arg);
  char *argv[] = { (char *)"oneios-audio", fd, NULL };
  return (void *)(intptr_t)helper_main(2, argv);
}

static void send_header(uint8_t kind, bool output, uint8_t channels, uint32_t value)
{
  struct au_header h = { .kind = kind, .output = output, .channels = channels, .value = value };
  CHECK(send(guest_fd, &h, sizeof h, MSG_NOSIGNAL) == sizeof h);
}

// The guest: SECONDS of 240-frame periods of a quiet sine, its clock `ppm` faster than the DAC's.
static void play(double seconds, double ppm)
{
  struct { struct au_header h; int16_t pcm[480]; } p;
  double period_ns = 5e6 / (1 + ppm / 1e6), phase = 0;
  struct timespec start, at;
  clock_gettime(CLOCK_MONOTONIC, &start);
  for (unsigned n = 0; n < seconds * 200; n++) {
    p.h = (struct au_header){ .kind = AU_PCM, .output = 1, .channels = 2, .value = 240 };
    for (unsigned i = 0; i < 240; i++) { p.pcm[2 * i] = p.pcm[2 * i + 1] = (int16_t)(1000 * sin(phase)); phase += 2 * M_PI * 440 / 48000; }
    CHECK(send(guest_fd, &p, sizeof p, MSG_NOSIGNAL) == sizeof p);
    int64_t next = (int64_t)((n + 1) * period_ns);
    at.tv_sec = start.tv_sec + (time_t)((start.tv_nsec + next) / 1000000000);
    at.tv_nsec = (start.tv_nsec + next) % 1000000000;
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &at, NULL);
  }
}

static void speaker_case(double ppm)
{
  fprintf(stderr, "-- the guest's clock %+.0f ppm against the DAC's\n", ppm);
  send_header(AU_START, true, 2, 48000);
  play(6, ppm);
  pthread_mutex_lock(&ring_lock);
  struct out_stats s = ostat;
  unsigned fill = ring_count;
  pthread_mutex_unlock(&ring_lock);
  AAudioStream *dac = last_out;
  int64_t queued = dac->written - pulled(dac);
  send_header(AU_STOP, true, 0, 0);
  CHECK(s.underruns == 0);
  CHECK(s.overflows == 0);
  CHECK(dac->dac_underflows == 0);
  CHECK(s.in == 6 * 48000);
  CHECK(dac->nonzero > 2 * 5 * 48000);                  // the tone reached the DAC
  if (ppm > 0) CHECK(s.slips_down > 0 && s.slips_up == 0);
  if (ppm < 0) CHECK(s.slips_up > 0 && s.slips_down == 0);
  if (ppm == 0) CHECK(s.slips_up + s.slips_down < 50);
  CHECK(FRAMES_MS((unsigned)queued + fill) < 120);       // ring + AAudio's buffer of 67 ms + a burst
}

static pthread_t helper;
static void helper_up(const char *linger)
{
  static int pair[2];
  setenv("ONEIOS_AUDIO_LINGER_S", linger, 1);
  CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair) == 0);
  guest_fd = pair[0];
  CHECK(pthread_create(&helper, NULL, run_helper, &pair[1]) == 0);
}
static void helper_down(void)
{
  close(guest_fd);                                       // xnuhost goes: the helper stops its streams and returns
  void *rc;
  pthread_join(helper, &rc);
  CHECK(rc == 0);
  CHECK(!out_dir.stream && !in_dir.stream);
}

int main(void)
{
  setvbuf(stderr, NULL, _IOLBF, 0);
  // 1. Drift, one stream per case (no linger).
  helper_up("0");
  dac_ppm = 0; speaker_case(0);
  dac_ppm = 2000; speaker_case(-2000);                    // the DAC 2000 ppm fast: the guest is slow against it
  dac_ppm = -2000; speaker_case(2000);
  // The microphone: about a second of 240-frame periods comes back, in order.
  send_header(AU_START, false, 1, 48000);
  struct { struct au_header h; int16_t pcm[AU_MAX_FRAMES]; } p;
  unsigned periods = 0, frames = 0;
  int16_t first = 0;
  struct timespec t0;
  clock_gettime(CLOCK_MONOTONIC, &t0);
  while (elapsed(&t0) < 1.0) {
    ssize_t got = recv(guest_fd, &p, sizeof p, 0);
    if (got < (ssize_t)sizeof p.h || p.h.kind != AU_PCM) continue;
    if (!periods) first = p.pcm[0];
    periods++;
    frames += p.h.value;
  }
  send_header(AU_STOP, false, 0, 0);
  CHECK(periods >= 180 && periods <= 210);
  CHECK(frames == periods * 240);
  CHECK(first == 100);
  helper_down();

  // 2. Linger: a START soon after a STOP takes the open stream (no new open, no stall); the stream closes after
  // the silence.
  fprintf(stderr, "-- linger 1 s; the new track stalls 200 ms\n");
  dac_ppm = 0;
  dac_stall_s = 0.2;
  helper_up("1");
  send_header(AU_START, true, 2, 48000);
  play(1, 0);
  pthread_mutex_lock(&ring_lock);
  CHECK(ostat.overflows == 0 && ostat.dropped == 0);      // the stall's audio waits in the ring, none is cut
  pthread_mutex_unlock(&ring_lock);
  send_header(AU_STOP, true, 0, 0);
  struct timespec nap = { 0, 500000000 };
  nanosleep(&nap, NULL);
  pthread_mutex_lock(&ring_lock);
  CHECK(ostat.played == ostat.in && ring_count == 0);      // the tail played after the STOP, nothing was cleared
  pthread_mutex_unlock(&ring_lock);
  AAudioStream *first_stream = last_out;
  CHECK(out_dir.stream != NULL);
  send_header(AU_START, true, 2, 48000);
  play(1, 0);
  CHECK(last_out == first_stream);
  pthread_mutex_lock(&ring_lock);
  CHECK(ostat.underruns == 0 && ostat.overflows == 0 && ostat.in == 48000);
  pthread_mutex_unlock(&ring_lock);
  CHECK(first_stream->dac_underflows == 0);
  send_header(AU_STOP, true, 0, 0);
  nap.tv_sec = 2; nap.tv_nsec = 600000000;
  nanosleep(&nap, NULL);
  CHECK(out_dir.stream == NULL);
  helper_down();
  printf("test_audio_helper: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
