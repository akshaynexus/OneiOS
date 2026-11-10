/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: shared declarations. The controller is the phone's (Galaxy S24 Ultra: Qualcomm
 * WCN785x), reached through xnuhost's Bluetooth bridge and the phone's Bluetooth HAL; this side is the
 * HCI transport iOS's bluetoothd expects.
 *
 * Ported from iOSPlus10 (patches/laurel/xnu/2600-laurel-bluetooth-wcn3990.patch, bsd/dev/laurel_bt.h).
 * Files:
 *   oneios_bt_h4.c       H4 framing, answers on the controller's behalf (as is)
 *   oneios_bt_ios.c      iOS transport core: queue, TX framing; packets go to and come from the host
 *                        bridge channel instead of a UART (the seam that changed)
 *   oneios_bt_ctl.c      kernel control com.apple.uart.bluetooth (as is)
 *   oneios_bt_dev.c      /dev/uart.bluetooth, /dev/cu.bluetooth, /dev/btwake (as is)
 *   oneios_bt.c          start, kill switch oneios_bt=0, the result line
 *   OneiOSBluetooth.cpp  the device tree's "bluetooth" nub: the address in use, hidden when off
 * Not ported (the phone's HAL does them): UART, clocks, rails, power pulses, firmware download.
 */
#ifndef ONEIOS_BT_H
#define ONEIOS_BT_H
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#define OBT_SAY(...)            printf("ONEIOS BT: " __VA_ARGS__)
#define OBT_LINE_SPEED          3000000u        /* what TIOCGETA reports; nothing depends on it */
const char *oneios_bt_result(void);      /* oneios_bt.c: "on: ..." / "off: <why>" */

/* ---- small helpers -------------------------------------------------------------------------- */

static inline uint16_t
obt_le16(const uint8_t *p)
{
	return (uint16_t)(p[0] | p[1] << 8);
}

static inline uint32_t
obt_le32(const uint8_t *p)
{
	return obt_le16(p) | (uint32_t)obt_le16(p + 2) << 16;
}

uint64_t obt_us(void);                          /* uptime in microseconds */
void obt_sleep_ms(uint32_t ms);

/* A Bluetooth address that a device can have: not zero, not all ones, not a group address (display order). */
static inline bool
obt_addr_ok(const uint8_t a[6])
{
	return (a[0] | a[1] | a[2] | a[3] | a[4] | a[5]) != 0 && (a[0] & 1) == 0 &&
	       (a[0] & a[1] & a[2] & a[3] & a[4] & a[5]) != 0xff;
}

/* ---- H4 framing + answers (oneios_bt_h4.c) --------------------------------------------------- */

#define OBT_H4_MAX              4104u           /* ACL 5 + 4095 + slack */
#define OBT_IBS_WAKE_ACK        0xfcu
#define OBT_IBS_WAKE_IND        0xfdu
#define OBT_IBS_SLEEP_IND       0xfeu

struct obt_h4 {
	uint8_t bytes[OBT_H4_MAX];
	uint32_t used, wanted, header;
	bool complete;
};

/* 0 more bytes needed, 1 packet complete (bytes[0..used)), 2 an in-band sleep byte, -1 bad type, -2 too long */
int obt_h4_push(struct obt_h4 *p, uint8_t byte);
void obt_h4_reset(struct obt_h4 *p);
/* The length of an H4 frame that begins at p (n bytes known), or 0 when more bytes are needed, -1 bad */
int obt_h4_frame_len(const uint8_t *p, uint32_t n);
int obt_cc_status(const uint8_t *e, size_t n, uint16_t opcode);   /* Command Complete status, or -1 */

#define OBT_EDL_OP              0xfc00u
#define OBT_EDL_VERSION         0x19u
#define OBT_EDL_TLV             0x1eu
#define OBT_EDL_SEGMENT         243u
#define OBT_QCA_BDADDR_OP       0xfc14u
#define OBT_QCA_BAUD_OP         0xfc48u

struct obt_qca_version {
	uint32_t product, soc;
	uint16_t patch, rom;
};

int obt_qca_version_parse(const uint8_t *e, size_t n, struct obt_qca_version *v);  /* 0 or -1 */
int obt_qca_tlv_ack(const uint8_t *e, size_t n);       /* 1 accepted, 0 refused, -1 not a TLV answer */
bool obt_is_vendor_event(const uint8_t *e, size_t n);
int obt_qca_baud_index(uint32_t baud);

/* Answers on the controller's behalf for bluetoothd (negroni 0336/0337) */
#define OBT_LOCAL_EVENT_MAX     16u
struct obt_shim {
	bool mask_known, err_rd, err_wr;
};
unsigned obt_local_answer(struct obt_shim *s, const uint8_t *cmd, size_t n, uint8_t ev[OBT_LOCAL_EVENT_MAX]);
unsigned obt_fix_event(struct obt_shim *s, uint8_t *e, size_t n);     /* bit 0 mask seen, bit 1 status rewritten */
int obt_fix_command(uint8_t *c, size_t n);                             /* 1 if changed */

/* ---- iOS transport (oneios_bt_ios.c, oneios_bt_ctl.c, oneios_bt_dev.c) ------------------------ */

void obt_ctl_attach(void);                      /* kernel control com.apple.uart.bluetooth */
void obt_ios_start(void);                       /* the host serves HCI: the pump thread runs, clients may connect */
void obt_ios_fail(const char *why);             /* Bluetooth off: connects are refused */
/* the two client kinds share one owner slot: 0 none, 1 kernel control, 2 device */
int obt_ios_claim(uint32_t owner, uint32_t unit);       /* 0 or EBUSY */
void obt_ios_release(uint32_t owner);
int obt_ios_send(const uint8_t *b, uint32_t n);         /* userland H4 bytes toward the controller; 0 or errno */
uint32_t obt_ios_take(uint8_t *b, uint32_t max);        /* queued H4 bytes toward userland */
uint32_t obt_ios_queued(void);
bool obt_ios_ready(void);
void obt_dev_attach(void);
void obt_dev_detach(void);                      /* Bluetooth off: the /dev nodes go */
void obt_dev_wakeup(void);                      /* data queued for the device client */
void obt_ctl_wakeup(void);                      /* data queued for the control client: obt_ctl_pump soon */
void obt_ctl_pump(void);                        /* pump thread only: queued bytes -> the control client */

/* ---- device-tree nub (OneiOSBluetooth.cpp) --------------------------------------------------- */

void oneios_bt_adt_publish(const uint8_t addr[6]);
void oneios_bt_adt_hide(void);

#endif /* ONEIOS_BT_H */
