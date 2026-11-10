// SPDX-License-Identifier: MIT
// virtio-snd (VirtIO 1.2, 5.14) for the guest's speaker and microphone, as UniXNU-droidvm's
// DroidVMVirtioSnd.c drives it: stream 0 is the output (48 kHz S16 stereo), stream 1 the input
// (48 kHz S16 mono), periods of 240 frames. Queues: 0 control, 1 event (unused), 2 transmit,
// 3 receive. Output periods go to the back end as they arrive (the guest's audio engine paces them);
// a capture thread fills the receive buffers the guest has posted with what the back end recorded.
#include "bridges/devices.h"
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { Q_CONTROL = 0, Q_EVENT = 1, Q_TX = 2, Q_RX = 3 };
enum { R_PCM_INFO = 0x0100, R_PCM_SET_PARAMS, R_PCM_PREPARE, R_PCM_RELEASE, R_PCM_START, R_PCM_STOP,
       S_OK = 0x8000, S_BAD_MSG, S_NOT_SUPP, S_IO_ERR };
enum { FMT_S16 = 5, RATE_48000 = 7, D_OUTPUT = 0, D_INPUT = 1 };
#define PERIOD_FRAMES 240u

struct pcm_info { uint32_t hda_fn_nid, features; uint64_t formats, rates; uint8_t direction, channels_min, channels_max, padding[5]; };
struct set_params { uint32_t code, stream_id, buffer_bytes, period_bytes, features; uint8_t channels, format, rate, padding; };

typedef struct {
  xh_virtio *dev;
  const br_audio_backend *backend;
  void *ctx;
  bool running[2];
  uint8_t channels[2];
  pthread_t capture;
  bool capture_thread;
  uint64_t tx_periods, rx_periods, rx_dropped;
} snd_state;

static uint32_t snd_config_read(xh_virtio *dev, uint32_t offset, unsigned size)
{
  uint32_t config[3] = { 0, 2, 0 }, v = 0;        // jacks, streams, chmaps
  (void)dev;
  if (offset < sizeof config && size <= sizeof config - offset) memcpy(&v, (uint8_t *)config + offset, size < 4 ? size : 4);
  return v;
}

static void *capture(void *arg)
{
  snd_state *s = arg;
  int16_t pcm[PERIOD_FRAMES];
  while (__atomic_load_n(&s->running[D_INPUT], __ATOMIC_ACQUIRE)) {
    // The back end blocks until a period is recorded (or returns less when it is stopped).
    int got = s->backend->read(s->ctx, pcm, PERIOD_FRAMES);
    if (got <= 0) { struct timespec nap = { 0, 5000000 }; nanosleep(&nap, NULL); continue; }
    if ((unsigned)got < PERIOD_FRAMES) memset(pcm + got, 0, (PERIOD_FRAMES - (unsigned)got) * 2);
    xh_viov iov[4];
    uint16_t head;
    xh_virtio_lock(s->dev);
    unsigned n = xh_virtio_pop(s->dev, Q_RX, &head, iov, 4);
    if (!n) { s->rx_dropped++; xh_virtio_unlock(s->dev); continue; }
    // header (readable), data (writable), status (writable, 8 bytes: status, latency)
    uint32_t written = 0;
    unsigned channels = s->channels[D_INPUT] ? s->channels[D_INPUT] : 1;
    for (unsigned i = 0; i < n; i++) {
      if (!iov[i].writable) continue;
      if (i + 1 < n) {                                   // the data part
        unsigned frames = iov[i].length / (2 * channels);
        if (frames > PERIOD_FRAMES) frames = PERIOD_FRAMES;
        int16_t *out = (int16_t *)(void *)iov[i].data;
        for (unsigned f = 0; f < frames; f++) for (unsigned c = 0; c < channels; c++) memcpy(&out[f * channels + c], &pcm[f], 2);
        written += frames * 2 * channels;
      } else if (iov[i].length >= 8) {
        uint32_t status[2] = { S_OK, 0 };
        memcpy(iov[i].data, status, 8);
        written += 8;
      }
    }
    s->rx_periods++;
    xh_virtio_push(s->dev, Q_RX, head, written);
    xh_virtio_unlock(s->dev);
  }
  return NULL;
}

// Gives back the receive buffers the guest posted (a stopped stream returns what it holds).
static void return_rx(snd_state *s)
{
  xh_viov iov[4];
  uint16_t head;
  while (xh_virtio_pop(s->dev, Q_RX, &head, iov, 4)) xh_virtio_push(s->dev, Q_RX, head, 0);
}

static uint32_t pcm_command(snd_state *s, uint32_t code, uint32_t stream)
{
  if (stream > 1) return S_BAD_MSG;
  bool output = stream == 0;
  unsigned d = output ? D_OUTPUT : D_INPUT;
  switch (code) {
  case R_PCM_PREPARE: case R_PCM_RELEASE: return S_OK;
  case R_PCM_START:
    if (s->running[d]) return S_OK;
    if (s->backend->start(s->ctx, output, 48000, s->channels[d] ? s->channels[d] : (output ? 2 : 1))) return S_IO_ERR;
    __atomic_store_n(&s->running[d], true, __ATOMIC_RELEASE);
    if (!output && !s->capture_thread) s->capture_thread = pthread_create(&s->capture, NULL, capture, s) == 0;
    return S_OK;
  case R_PCM_STOP:
    if (!s->running[d]) return S_OK;
    __atomic_store_n(&s->running[d], false, __ATOMIC_RELEASE);
    (void)s->backend->stop(s->ctx, output);
    if (!output) {
      if (s->capture_thread) {
        // The capture thread takes the device's lock; let go of it while it ends.
        xh_virtio_unlock(s->dev);
        pthread_join(s->capture, NULL);
        xh_virtio_lock(s->dev);
        s->capture_thread = false;
      }
      return_rx(s);
    }
    return S_OK;
  }
  return S_NOT_SUPP;
}

static int64_t snd_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  snd_state *s = xh_virtio_opaque(dev);
  (void)head;
  if (queue == Q_CONTROL) {
    uint8_t request[64] = { 0 };
    size_t size = 0;
    xh_viov *reply = NULL;
    for (unsigned i = 0; i < n; i++) {
      if (iov[i].writable) { if (!reply) reply = &iov[i]; continue; }
      size_t part = iov[i].length < sizeof request - size ? iov[i].length : sizeof request - size;
      memcpy(request + size, iov[i].data, part);
      size += part;
    }
    if (!reply || reply->length < 4 || size < 4) return 0;
    uint32_t code, status = S_NOT_SUPP, written = 4;
    memcpy(&code, request, 4);
    if (code == R_PCM_INFO && size >= 16) {
      uint32_t first, count, each;
      memcpy(&first, request + 4, 4); memcpy(&count, request + 8, 4); memcpy(&each, request + 12, 4);
      if (first + count > 2 || each < sizeof(struct pcm_info) || reply->length < 4 + (uint64_t)count * each) status = S_BAD_MSG;
      else {
        memset(reply->data, 0, 4 + count * each);
        for (uint32_t i = 0; i < count; i++) {
          bool output = first + i == 0;
          struct pcm_info info = { .formats = 1ull << FMT_S16, .rates = 1ull << RATE_48000, .direction = output ? D_OUTPUT : D_INPUT,
                                   .channels_min = output ? 2 : 1, .channels_max = output ? 2 : 1 };
          memcpy(reply->data + 4 + i * each, &info, sizeof info);
        }
        written = 4 + count * each;
        status = S_OK;
      }
    } else if (code == R_PCM_SET_PARAMS && size >= sizeof(struct set_params)) {
      struct set_params p;
      memcpy(&p, request, sizeof p);
      bool output = p.stream_id == 0;
      if (p.stream_id > 1 || p.format != FMT_S16 || p.rate != RATE_48000 || p.channels != (output ? 2 : 1)) status = S_NOT_SUPP;
      else { s->channels[output ? D_OUTPUT : D_INPUT] = p.channels; status = S_OK; }
    } else if (code >= R_PCM_PREPARE && code <= R_PCM_STOP && size >= 8) {
      uint32_t stream;
      memcpy(&stream, request + 4, 4);
      status = pcm_command(s, code, stream);
    }
    memcpy(reply->data, &status, 4);
    return written;
  }
  if (queue == Q_TX) {
    // header { stream_id } then PCM (readable), then the status (writable).
    uint32_t stream = 0xffffffff, status[2] = { S_OK, 0 };
    size_t skip = 4;
    for (unsigned i = 0; i < n; i++) {
      if (iov[i].writable) { if (iov[i].length >= 8) { memcpy(iov[i].data, status, 8); return 8; } continue; }
      const uint8_t *data = iov[i].data;
      size_t length = iov[i].length;
      if (skip) {
        if (length < skip) return 0;
        memcpy(&stream, data, 4);
        data += skip; length -= skip; skip = 0;
      }
      if (stream == 0 && s->running[D_OUTPUT] && length >= 4) {
        (void)s->backend->write(s->ctx, (const int16_t *)(const void *)data, (unsigned)(length / 4));
        s->tx_periods++;
      }
    }
    return 0;
  }
  return 0;
}

static void snd_kick(xh_virtio *dev, unsigned queue) { (void)dev; (void)queue; }

static void snd_reset(xh_virtio *dev)
{
  snd_state *s = xh_virtio_opaque(dev);
  (void)pcm_command(s, R_PCM_STOP, 0);
  (void)pcm_command(s, R_PCM_STOP, 1);
}

static const xh_virtio_ops snd_ops = { .device_id = 25, .nqueues = 4, .handle = snd_handle, .config_read = snd_config_read, .reset = snd_reset,
                                       .manual = 1u << Q_EVENT | 1u << Q_RX, .kick = snd_kick };

xh_virtio *br_virtio_snd_create(uint64_t base, unsigned spi, const br_audio_backend *backend, void *backend_ctx)
{
  snd_state *s = calloc(1, sizeof *s);
  if (!s) return NULL;
  s->backend = backend;
  s->ctx = backend_ctx;
  s->dev = xh_virtio_create(base, spi, &snd_ops, s, "virtio-snd");
  if (!s->dev) { free(s); return NULL; }
  return s->dev;
}
