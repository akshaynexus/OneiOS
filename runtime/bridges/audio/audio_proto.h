// SPDX-License-Identifier: MIT
// Messages between xnuhost's audio back end (audio/backend.c) and the AAudio helper process
// (audio/aaudio_helper.c) on their socket pair (SOCK_SEQPACKET: one message per packet).
#ifndef ONEIOS_AUDIO_PROTO_H
#define ONEIOS_AUDIO_PROTO_H
#include <stdint.h>
enum { AU_START = 1, AU_STOP = 2, AU_PCM = 3, AU_STATE = 4 };
struct au_header {
  uint8_t  kind;          // AU_*
  uint8_t  output;        // 1 = the speaker stream, 0 = the microphone stream
  uint8_t  channels;
  uint8_t  reserved;
  uint32_t value;         // START: sample rate; PCM: frames that follow (S16LE interleaved); STATE: 0 ok, else errno
};
#define AU_MAX_FRAMES 960u
#endif
