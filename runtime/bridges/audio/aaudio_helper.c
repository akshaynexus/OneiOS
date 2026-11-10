// SPDX-License-Identifier: MIT
// oneios-audio: the AAudio end of the audio bridge (audio/backend.c explains why it is a process).
// Run by xnuhost as `oneios-audio FD`: FD is a SOCK_SEQPACKET socket carrying audio_proto.h.
// Output: 48 kHz S16 stereo periods from the guest go into a ring the AAudio data callback empties;
// an empty ring plays silence, a full one drops the oldest audio (the guest's clock and the DAC's
// are not the same clock). Input: the microphone's callback sends each burst to xnuhost.
//
// One UI's audioserver and its AAudio service (media.aaudio) keep running in a session. A root
// process is a "service uid" for audioserver, so no app-op is asked for playback; recording needs
// the microphone's permission check to pass for uid 0, which is not yet seen on the phone.
#include "audio_proto.h"
#include <aaudio/AAudio.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#define RING_FRAMES 9600u                        // 200 ms of stereo at 48 kHz
#define TARGET_FRAMES 1920u                      // what the ring is cut back to after an overflow (40 ms)

static int sock;
static int16_t ring[RING_FRAMES * 2];
static unsigned ring_head, ring_count;           // frames
static pthread_mutex_t ring_lock = PTHREAD_MUTEX_INITIALIZER;
static AAudioStream *out_stream, *in_stream;
static unsigned in_channels = 1;

static void report(bool output, int error)
{
  struct au_header header = { .kind = AU_STATE, .output = output, .value = (uint32_t)error };
  (void)send(sock, &header, sizeof header, MSG_NOSIGNAL);
}

static aaudio_data_callback_result_t play(AAudioStream *stream, void *user, void *audio, int32_t frames)
{
  (void)stream; (void)user;
  int16_t *out = audio;
  pthread_mutex_lock(&ring_lock);
  unsigned take = (unsigned)frames < ring_count ? (unsigned)frames : ring_count;
  for (unsigned i = 0; i < take; i++) {
    out[2 * i] = ring[2 * ring_head];
    out[2 * i + 1] = ring[2 * ring_head + 1];
    ring_head = (ring_head + 1) % RING_FRAMES;
  }
  ring_count -= take;
  pthread_mutex_unlock(&ring_lock);
  memset(out + 2 * take, 0, ((unsigned)frames - take) * 4);
  return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

static aaudio_data_callback_result_t record(AAudioStream *stream, void *user, void *audio, int32_t frames)
{
  (void)stream; (void)user;
  struct { struct au_header header; int16_t pcm[AU_MAX_FRAMES]; } packet;
  const int16_t *in = audio;
  while (frames > 0) {
    unsigned n = (unsigned)frames > AU_MAX_FRAMES ? AU_MAX_FRAMES : (unsigned)frames;
    packet.header = (struct au_header){ .kind = AU_PCM, .output = 0, .channels = 1, .value = n };
    for (unsigned i = 0; i < n; i++) packet.pcm[i] = in[i * in_channels];          // the first channel
    (void)send(sock, &packet, sizeof packet.header + n * 2, MSG_DONTWAIT | MSG_NOSIGNAL);
    in += n * in_channels;
    frames -= (int32_t)n;
  }
  return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

static AAudioStream *open_stream(bool output, unsigned rate, unsigned channels)
{
  AAudioStreamBuilder *builder;
  AAudioStream *stream = NULL;
  if (AAudio_createStreamBuilder(&builder) != AAUDIO_OK) return NULL;
  AAudioStreamBuilder_setDirection(builder, output ? AAUDIO_DIRECTION_OUTPUT : AAUDIO_DIRECTION_INPUT);
  AAudioStreamBuilder_setSampleRate(builder, (int32_t)rate);
  AAudioStreamBuilder_setChannelCount(builder, (int32_t)channels);
  AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
  AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
  AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
  AAudioStreamBuilder_setDataCallback(builder, output ? play : record, NULL);
  if (output) { AAudioStreamBuilder_setUsage(builder, AAUDIO_USAGE_MEDIA); AAudioStreamBuilder_setContentType(builder, AAUDIO_CONTENT_TYPE_MUSIC); }
  else AAudioStreamBuilder_setInputPreset(builder, AAUDIO_INPUT_PRESET_VOICE_RECOGNITION);
  aaudio_result_t result = AAudioStreamBuilder_openStream(builder, &stream);
  AAudioStreamBuilder_delete(builder);
  if (result != AAUDIO_OK) { fprintf(stderr, "oneios-audio: open %s: %s\n", output ? "output" : "input", AAudio_convertResultToText(result)); return NULL; }
  if (!output) in_channels = (unsigned)AAudioStream_getChannelCount(stream);
  result = AAudioStream_requestStart(stream);
  if (result != AAUDIO_OK) { fprintf(stderr, "oneios-audio: start: %s\n", AAudio_convertResultToText(result)); AAudioStream_close(stream); return NULL; }
  return stream;
}

static void close_stream(AAudioStream **stream)
{
  if (!*stream) return;
  AAudioStream_requestStop(*stream);
  AAudioStream_close(*stream);
  *stream = NULL;
}

int main(int argc, char **argv)
{
  if (argc != 2) { fprintf(stderr, "usage: oneios-audio SOCKET_FD (started by xnuhost)\n"); return 2; }
  sock = atoi(argv[1]);
  static struct { struct au_header header; int16_t pcm[AU_MAX_FRAMES * 2]; } packet;
  for (;;) {
    ssize_t got = recv(sock, &packet, sizeof packet, 0);
    if (got == 0) break;                                   // xnuhost closed its end
    if (got < 0) { if (errno == EINTR) continue; break; }
    if ((size_t)got < sizeof packet.header) continue;
    bool output = packet.header.output;
    AAudioStream **stream = output ? &out_stream : &in_stream;
    switch (packet.header.kind) {
    case AU_START:
      close_stream(stream);
      if (output) { pthread_mutex_lock(&ring_lock); ring_head = ring_count = 0; pthread_mutex_unlock(&ring_lock); }
      *stream = open_stream(output, packet.header.value, packet.header.channels);
      report(output, *stream ? 0 : EIO);
      break;
    case AU_STOP:
      close_stream(stream);
      break;
    case AU_PCM: {
      unsigned frames = (unsigned)(((size_t)got - sizeof packet.header) / 4);
      if (frames > packet.header.value) frames = packet.header.value;
      pthread_mutex_lock(&ring_lock);
      if (ring_count + frames > RING_FRAMES) {             // the DAC is slower than the guest: drop the oldest audio
        unsigned keep = TARGET_FRAMES < ring_count ? TARGET_FRAMES : ring_count;
        ring_head = (ring_head + ring_count - keep) % RING_FRAMES;
        ring_count = keep;
      }
      for (unsigned i = 0; i < frames; i++) {
        unsigned at = (ring_head + ring_count + i) % RING_FRAMES;
        ring[2 * at] = packet.pcm[2 * i];
        ring[2 * at + 1] = packet.pcm[2 * i + 1];
      }
      ring_count += frames;
      pthread_mutex_unlock(&ring_lock);
      break;
    }
    }
  }
  close_stream(&out_stream);
  close_stream(&in_stream);
  return 0;
}
