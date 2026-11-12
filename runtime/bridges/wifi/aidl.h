// SPDX-License-Identifier: MIT
// A stable-AIDL binder client (and, for the tests, server) with no Android library: xnuhost is a static binary, and
// the vendor's Wi-Fi supplicant is an AIDL service on /dev/binder (docs/hardware.md section 3).
//   aidl_parcel.c   the parcels: the interface token, int, String (UTF-16 on the wire), byte[], binder objects with
//                   their stability word, the status header of a reply. Pure: unit-tested on any host.
//   aidl_binder.c   the kernel's binder protocol (include/uapi/linux/android/binder.h): calls, one looper thread for
//                   the calls into our objects, references, death notifications.
// Written from libbinder's Parcel.cpp, Status.cpp, Stability.cpp and IPCThreadState.cpp (AOSP, Apache 2.0) as the
// wire format of Android 13 and later: the stability word is the level alone (VINTF 0x3f).
#ifndef ONEIOS_BRIDGES_AIDL_H
#define ONEIOS_BRIDGES_AIDL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define AB_TYPE_BINDER 0x73622a85u       // B_PACK_CHARS('s','b','*',0x85): an object of the sender
#define AB_TYPE_HANDLE 0x73682a85u       // a reference to someone's object
#define AB_HEADER_SYST 0x53595354u       // 'SYST': the interface token's marker of a system libbinder
#define AB_STABILITY_VINTF 0x3f          // what a vendor process may call
#define AB_LOCAL_FLAGS 0x113u            // accepts descriptors, SCHED_NORMAL at nice 19 or better: never realtime

// Transaction codes every binder object answers.
#define AB_FIRST_CALL 1u
#define AB_PING 0x5f504e47u              // '_PNG'
#define AB_INTERFACE 0x5f4e5446u         // '_NTF': the reply is the descriptor
#define AB_GET_VERSION 0x00ffffffu       // stable AIDL: getInterfaceVersion
#define AB_GET_HASH 0x00fffffeu          // stable AIDL: getInterfaceHash
#define AB_ONE_WAY 1u                    // TF_ONE_WAY in a transaction's flags

// android::status_t values a transaction can be answered with.
#define AB_UNKNOWN_TRANSACTION (-74)
#define AB_BAD_TYPE (-2147483647)
// Exception codes of a reply's status header.
#define AB_EX_SERVICE_SPECIFIC (-8)
#define AB_EX_HAS_NOTED_APPOPS (-127)
#define AB_EX_HAS_REPLY_HEADER (-128)

#define AB_PARCEL_DATA 4096u
#define AB_PARCEL_OBJECTS 8u

// An outgoing parcel.
typedef struct {
  uint8_t  data[AB_PARCEL_DATA];
  size_t   size;
  uint64_t offsets[AB_PARCEL_OBJECTS];
  unsigned nobjects;
  bool     overflow;
} ab_parcel;

void ab_init(ab_parcel *p);
void ab_put_token(ab_parcel *p, const char *descriptor);       // writeInterfaceToken
void ab_put_i32(ab_parcel *p, int32_t value);
void ab_put_string(ab_parcel *p, const char *text);            // String: UTF-16, NULL is a null string; text is ASCII
void ab_put_bytes(ab_parcel *p, const uint8_t *bytes, size_t length);   // byte[] (also byte[N])
void ab_put_local(ab_parcel *p, uint64_t object);              // one of our objects (the id comes back with its calls)
void ab_put_handle(ab_parcel *p, uint32_t handle);             // someone's object we hold
void ab_put_null_binder(ab_parcel *p);
// A reply's status header: no exception, or a service-specific error.
void ab_put_ok(ab_parcel *p);
void ab_put_service_error(ab_parcel *p, int32_t code);
void ab_wipe(ab_parcel *p);                                    // a parcel that carried a key

// An incoming parcel.
typedef struct {
  const uint8_t  *data;
  size_t          size, position;
  const uint64_t *offsets;
  unsigned        nobjects;
  bool            bad;
} ab_reader;

void    ab_reader_init(ab_reader *r, const void *data, size_t size, const void *offsets, size_t offsets_bytes);
int32_t ab_get_i32(ab_reader *r);
// A String: its text as ASCII (other characters become '?'), cut to size - 1. false and bad on a malformed parcel;
// a null string gives "".
bool    ab_get_string(ab_reader *r, char *out, size_t size);
// A byte[]: a null array gives length 0.
bool    ab_get_bytes(ab_reader *r, const uint8_t **bytes, size_t *length);
// A binder object: *handle for someone's object (true), or *local for one of the reader's own objects, 0 for null
// (false). bad on a malformed parcel.
bool    ab_get_binder(ab_reader *r, uint32_t *handle, uint64_t *local);
// The interface token of a call: true when it names descriptor.
bool    ab_get_token(ab_reader *r, const char *descriptor);
// A reply's status header: 0, or the exception code with *specific the service's error (EX_SERVICE_SPECIFIC).
int32_t ab_get_status(ab_reader *r, int32_t *specific);
// The start of a parcelable (its non-null marker and size): the position after it, or 0 for a null or malformed one.
size_t  ab_get_parcelable(ab_reader *r);
// What every object answers: the descriptor, a ping, the interface's version and hash. true when code was one of them.
bool    ab_standard(uint32_t code, const char *descriptor, int32_t version, const char *hash, ab_parcel *reply);

// ---- the connection ----
typedef struct ab ab;
// A call into one of our objects (object: its id; 0 with a context manager: a call to handle 0). Returns 0 with
// *reply made (not sent for a one-way call), or the status to answer with (AB_UNKNOWN_TRANSACTION).
typedef int (*ab_incoming_fn)(void *ctx, uint64_t object, uint32_t code, uint32_t flags, ab_reader *r, ab_parcel *reply);
typedef void (*ab_death_fn)(void *ctx, uint64_t cookie);
typedef int (*ab_reply_fn)(void *ctx, ab_reader *r);            // the reply after its status header; 0 or -errno

// Opens the binder device (NULL: ONEIOS_BINDER, else /dev/binder) and starts the looper thread.
ab  *ab_open(const char *device, ab_incoming_fn incoming, ab_death_fn death, void *ctx);
int  ab_context_manager(ab *b);                                 // the tests' service manager
// A call and its reply. 0; -errno (-ETIMEDOUT: no reply in 5 s, -EPIPE: the object's process died); or -EREMOTEIO
// with *specific the service's error code (or the exception's, when it is not a service-specific one). on_reply sees
// the reply before its buffer is given back: it takes its references to the handles it keeps (ab_acquire) there.
int  ab_call(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p, int32_t *specific, ab_reply_fn on_reply, void *reply_ctx);
int  ab_oneway(ab *b, uint32_t handle, uint32_t code, const ab_parcel *p);
int  ab_acquire(ab *b, uint32_t handle);
void ab_release(ab *b, uint32_t handle);
int  ab_watch(ab *b, uint32_t handle, uint64_t cookie);         // death tells cookie when the object's process dies
void ab_close(ab *b);                                           // the driver drops our objects: their holders see them die

#endif
