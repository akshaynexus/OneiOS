// SPDX-License-Identifier: MIT
// The part of the NDK's <aaudio/AAudio.h> that audio/aaudio_helper.c uses, for its host test
// (tests/test_audio_helper.c implements the functions: a fake DAC and microphone). Values as in the NDK.
#ifndef ONEIOS_FAKE_AAUDIO_H
#define ONEIOS_FAKE_AAUDIO_H
#include <stdbool.h>
#include <stdint.h>
#include <time.h>
typedef int32_t aaudio_result_t, aaudio_direction_t, aaudio_format_t, aaudio_sharing_mode_t, aaudio_performance_mode_t,
                aaudio_usage_t, aaudio_content_type_t, aaudio_input_preset_t;
typedef struct AAudioStreamStruct AAudioStream;
typedef struct AAudioStreamBuilderStruct AAudioStreamBuilder;
enum { AAUDIO_OK = 0, AAUDIO_ERROR_TIMEOUT = -885, AAUDIO_ERROR_INVALID_STATE = -895 };
enum { AAUDIO_DIRECTION_OUTPUT = 0, AAUDIO_DIRECTION_INPUT = 1 };
enum { AAUDIO_FORMAT_PCM_I16 = 1 };
enum { AAUDIO_SHARING_MODE_EXCLUSIVE = 0, AAUDIO_SHARING_MODE_SHARED = 1 };
enum { AAUDIO_PERFORMANCE_MODE_NONE = 10, AAUDIO_PERFORMANCE_MODE_POWER_SAVING = 11, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY = 12 };
enum { AAUDIO_USAGE_MEDIA = 1 };
enum { AAUDIO_CONTENT_TYPE_MUSIC = 2 };
enum { AAUDIO_INPUT_PRESET_VOICE_RECOGNITION = 6 };
aaudio_result_t AAudio_createStreamBuilder(AAudioStreamBuilder **builder);
const char *AAudio_convertResultToText(aaudio_result_t result);
void AAudioStreamBuilder_setDirection(AAudioStreamBuilder *b, aaudio_direction_t v);
void AAudioStreamBuilder_setSampleRate(AAudioStreamBuilder *b, int32_t v);
void AAudioStreamBuilder_setChannelCount(AAudioStreamBuilder *b, int32_t v);
void AAudioStreamBuilder_setFormat(AAudioStreamBuilder *b, aaudio_format_t v);
void AAudioStreamBuilder_setPerformanceMode(AAudioStreamBuilder *b, aaudio_performance_mode_t v);
void AAudioStreamBuilder_setSharingMode(AAudioStreamBuilder *b, aaudio_sharing_mode_t v);
void AAudioStreamBuilder_setUsage(AAudioStreamBuilder *b, aaudio_usage_t v);
void AAudioStreamBuilder_setContentType(AAudioStreamBuilder *b, aaudio_content_type_t v);
void AAudioStreamBuilder_setInputPreset(AAudioStreamBuilder *b, aaudio_input_preset_t v);
aaudio_result_t AAudioStreamBuilder_openStream(AAudioStreamBuilder *b, AAudioStream **stream);
aaudio_result_t AAudioStreamBuilder_delete(AAudioStreamBuilder *b);
aaudio_result_t AAudioStream_requestStart(AAudioStream *s);
aaudio_result_t AAudioStream_requestStop(AAudioStream *s);
aaudio_result_t AAudioStream_close(AAudioStream *s);
aaudio_result_t AAudioStream_write(AAudioStream *s, const void *buffer, int32_t frames, int64_t timeout_ns);
aaudio_result_t AAudioStream_read(AAudioStream *s, void *buffer, int32_t frames, int64_t timeout_ns);
aaudio_result_t AAudioStream_setBufferSizeInFrames(AAudioStream *s, int32_t frames);
aaudio_result_t AAudioStream_getTimestamp(AAudioStream *s, clockid_t clock, int64_t *position, int64_t *time_ns);
int32_t AAudioStream_getFramesPerBurst(AAudioStream *s);
int32_t AAudioStream_getBufferSizeInFrames(AAudioStream *s);
int32_t AAudioStream_getBufferCapacityInFrames(AAudioStream *s);
int32_t AAudioStream_getSampleRate(AAudioStream *s);
int32_t AAudioStream_getChannelCount(AAudioStream *s);
int32_t AAudioStream_getDeviceId(AAudioStream *s);
int32_t AAudioStream_getSessionId(AAudioStream *s);
int32_t AAudioStream_getXRunCount(AAudioStream *s);
aaudio_sharing_mode_t AAudioStream_getSharingMode(AAudioStream *s);
aaudio_performance_mode_t AAudioStream_getPerformanceMode(AAudioStream *s);
int64_t AAudioStream_getFramesWritten(AAudioStream *s);
int64_t AAudioStream_getFramesRead(AAudioStream *s);
#endif
