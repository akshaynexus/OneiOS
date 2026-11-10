// SPDX-License-Identifier: MIT
// The OneiOS bridge channel: one virtio-mmio device ("oneios,bridge" in /arm-io) that carries small
// messages between the guest drivers in drivers/ and the host bridges in runtime/bridges/.
// Freestanding C, included unchanged by the kernel side and by xnuhost.
//
// Queue 0 (guest to host): one readable buffer per message. Queue 1 (host to guest): the guest posts
// writable buffers of OB_MAX_MESSAGE bytes; the host fills one per message. A message is this header
// and `length` payload bytes. All values little-endian.
#ifndef ONEIOS_BRIDGE_PROTO_H
#define ONEIOS_BRIDGE_PROTO_H

#include <stdint.h>

#define OB_VIRTIO_DEVICE_ID 0x4f42u      // not a registered VirtIO id; the guest matches the node, not the id
#define OB_PROTO_VERSION    1u
#define OB_MAX_MESSAGE      4096u
#define OB_MAX_PAYLOAD      (OB_MAX_MESSAGE - 8u)
#define OB_QUEUE_TX         0u           // guest to host
#define OB_QUEUE_RX         1u           // host to guest

struct ob_header {
  uint16_t channel;                      // OB_CH_*
  uint16_t type;                         // per channel
  uint32_t length;                       // payload bytes after the header
};

enum { OB_CH_CONTROL = 0, OB_CH_HAPTIC = 1, OB_CH_HCI = 2, OB_CH_CAMERA = 3, OB_CH_COUNT = 4 };

// ---- control ----------------------------------------------------------------------------------
// guest -> host OB_CTL_HELLO { u32 version }; host -> guest OB_CTL_HELLO { u32 version, u32 channels }
// where bit n of channels says the host serves channel n.
enum { OB_CTL_HELLO = 1 };
struct ob_ctl_hello { uint32_t version, channels; };

// ---- haptics (guest -> host only) -------------------------------------------------------------
enum {
  OB_HAPTIC_VIB = 1,                     // struct ob_haptic_vib: vibrate from now; on_ms 0 stops
  OB_HAPTIC_STOP = 2,                    // no payload
};
struct ob_haptic_vib {
  uint32_t on_ms;                        // 0 = stop
  uint16_t intensity;                    // 0..1000 (iOS's 0.0..1.0)
  uint16_t frequency_hz;                 // 0 = the actuator's own (resonance)
};

// ---- Bluetooth HCI ----------------------------------------------------------------------------
// Types 1..5 are the H4 packet indicators and carry one whole HCI packet (without the indicator).
enum {
  OB_HCI_COMMAND = 1, OB_HCI_ACL = 2, OB_HCI_SCO = 3, OB_HCI_EVENT = 4, OB_HCI_ISO = 5,
  OB_HCI_OPEN = 16,                      // guest -> host: power the controller, no payload
  OB_HCI_CLOSE = 17,                     // guest -> host: release it
  OB_HCI_STATE = 18,                     // host -> guest: struct ob_hci_state
};
enum { OB_HCI_STATE_OFF = 0, OB_HCI_STATE_READY = 1, OB_HCI_STATE_FAILED = 2 };
struct ob_hci_state { uint32_t state; uint8_t bd_addr[6]; uint8_t reserved[2]; };

#endif
