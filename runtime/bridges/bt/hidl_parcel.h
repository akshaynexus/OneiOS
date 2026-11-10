// SPDX-License-Identifier: MIT
// The wire format of a HIDL transaction on /dev/hwbinder, as far as the Bluetooth HAL needs it:
// an interface token, hidl_string and hidl_vec<uint8_t> arguments (scatter-gather buffer objects),
// strong binders, and the replies of a callback object. Written from the kernel's binder UAPI
// (include/uapi/linux/android/binder.h) and libhwbinder's Parcel layout; no Android library is
// used, so xnuhost stays a static binary.
#ifndef ONEIOS_HIDL_PARCEL_H
#define ONEIOS_HIDL_PARCEL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Kernel object headers (binder protocol version 8, 64-bit).
#define HB_TYPE_BINDER 0x73622a85u       // B_PACK_CHARS('s','b','*',0x85)
#define HB_TYPE_HANDLE 0x73682a85u
#define HB_TYPE_PTR    0x70742a85u
#define HB_BUFFER_HAS_PARENT 1u

struct hb_flat_object { uint32_t type, flags; uint64_t binder_or_handle, cookie; };          // 24 bytes
struct hb_buffer_object { uint32_t type, flags; uint64_t buffer, length, parent, parent_offset; };   // 40 bytes

// hidl_string and hidl_vec<T> have the same 16-byte shape.
struct hb_hidl_ref { uint64_t pointer; uint32_t size; uint8_t owns; uint8_t pad[3]; };

#define HB_PARCEL_DATA 8192u
#define HB_PARCEL_OBJECTS 16u
#define HB_PARCEL_SCRATCH 1024u

// An outgoing parcel. Buffers passed to the add_* calls must stay valid until the transaction is
// written (the kernel copies them then); refs for strings and vectors live in `scratch`.
typedef struct {
  uint8_t  data[HB_PARCEL_DATA];
  size_t   size;
  uint64_t offsets[HB_PARCEL_OBJECTS];
  unsigned nobjects;
  uint64_t buffers_size;                 // sum of the buffer objects' lengths, each rounded up to 8
  uint8_t  scratch[HB_PARCEL_SCRATCH];
  size_t   scratch_used;
  bool     overflow;
} hb_parcel;

void hb_parcel_init(hb_parcel *p);
void hb_put_token(hb_parcel *p, const char *interface);       // writeInterfaceToken
void hb_put_i32(hb_parcel *p, int32_t value);
// A top-level buffer. Returns its handle (index among the parcel's objects).
unsigned hb_put_buffer(hb_parcel *p, const void *buffer, size_t length);
unsigned hb_put_embedded(hb_parcel *p, const void *buffer, size_t length, unsigned parent, size_t parent_offset);
void hb_put_string(hb_parcel *p, const char *text);           // hidl_string
void hb_put_bytes(hb_parcel *p, const uint8_t *bytes, size_t length);   // hidl_vec<uint8_t>
void hb_put_string_vec(hb_parcel *p, const char *const *texts, unsigned count);   // hidl_vec<hidl_string>
void hb_put_binder(hb_parcel *p, uint64_t pointer, uint64_t cookie);    // a local object

// An incoming parcel: data and the offsets of its objects, as the kernel hands them over (buffer
// pointers already point into this process).
typedef struct {
  const uint8_t  *data;
  size_t          size;
  const uint64_t *offsets;
  unsigned        nobjects;
  size_t          position;
  unsigned        next_object;
  bool            bad;
} hb_reader;

void hb_reader_init(hb_reader *r, const void *data, size_t size, const void *offsets, size_t offsets_bytes);
// The interface token; NULL (and bad) when it is not a terminated string.
const char *hb_get_token(hb_reader *r);
int32_t hb_get_i32(hb_reader *r);
// The next hidl_vec<uint8_t>: its bytes and length. false (and bad) on a malformed parcel.
bool hb_get_bytes(hb_reader *r, const uint8_t **bytes, size_t *length);
// The next strong binder: a handle (true) or a null/local object (false, *handle = 0).
bool hb_get_handle(hb_reader *r, uint32_t *handle);

// HIDL transaction codes.
enum {
  HB_FIRST_CALL = 1,
  HB_INTERFACE_CHAIN = 0x0f43484e, HB_DEBUG = 0x0f444247, HB_INTERFACE_DESCRIPTOR = 0x0f445343,
  HB_GET_HASH_CHAIN = 0x0f485348, HB_SET_HAL_INSTRUMENTATION = 0x0f494e54, HB_PING = 0x0f504e47,
  HB_GET_DEBUG_INFO = 0x0f524546, HB_NOTIFY_SYSPROPS_CHANGED = 0x0f535953,
};
#define HB_UNKNOWN_TRANSACTION (-74)    // -EBADMSG, android::UNKNOWN_TRANSACTION

// android.hardware.bluetooth@1.0/1.1
#define BT_HCI_1_0        "android.hardware.bluetooth@1.0::IBluetoothHci"
#define BT_HCI_1_1        "android.hardware.bluetooth@1.1::IBluetoothHci"
#define BT_CALLBACKS_1_0  "android.hardware.bluetooth@1.0::IBluetoothHciCallbacks"
#define BT_CALLBACKS_1_1  "android.hardware.bluetooth@1.1::IBluetoothHciCallbacks"
#define HIDL_BASE         "android.hidl.base@1.0::IBase"
#define HIDL_MANAGER      "android.hidl.manager@1.0::IServiceManager"
enum { BT_INITIALIZE = 1, BT_SEND_COMMAND = 2, BT_SEND_ACL = 3, BT_SEND_SCO = 4, BT_CLOSE = 5, BT_INITIALIZE_1_1 = 6, BT_SEND_ISO = 7 };
enum { BT_CB_INIT_COMPLETE = 1, BT_CB_EVENT = 2, BT_CB_ACL = 3, BT_CB_SCO = 4, BT_CB_ISO = 5 };

// The request parcels of the HAL client.
void bt_parcel_get_service(hb_parcel *p, const char *fq_name, const char *instance);
void bt_parcel_initialize(hb_parcel *p, bool v1_1, uint64_t callback_pointer);
void bt_parcel_send(hb_parcel *p, unsigned code, const uint8_t *packet, size_t length);
// What the callback object answers to transaction `code` with request `r`. For the four data
// callbacks *type gets the H4 indicator and *packet/*length the HCI packet; for
// initializationComplete *type is 0 and *status the HAL's status (0 = success). Returns 0 when the
// reply parcel is a normal one, else the status code to reply with (HB_UNKNOWN_TRANSACTION).
int bt_callback_transact(uint32_t code, hb_reader *r, hb_parcel *reply, uint8_t *type, const uint8_t **packet, size_t *length, int32_t *status);

#endif
