/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS Bluetooth: H4 framing and the HCI answers given on the controller's behalf for iOS bluetoothd.
 * No hardware access.
 *
 * Ported as is from iOSPlus10 (patches/laurel/xnu/2600-laurel-bluetooth-wcn3990.patch,
 * bsd/dev/laurel_bt_h4.c; H4 parser from ios-apollo xnu 0136, MIT; answers from negroni 0333/0336/0337).
 * Only names changed. The WCN3990 EDL parsers (version, TLV answer, baud index) are kept though this
 * board never downloads firmware (the phone's Bluetooth HAL does): the S24 Ultra's WCN785x is the same
 * Qualcomm family, with the same vendor opcodes, which is why the answers below apply unchanged.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include "oneios_bt.h"

void
obt_h4_reset(struct obt_h4 *p)
{
	p->used = p->wanted = p->header = 0;
	p->complete = false;
}

static uint32_t
header_len(uint8_t type)
{
	switch (type) {
	case 1:                                 /* command: type, opcode (2), length (1) */
	case 3:                                 /* SCO: type, handle (2), length (1) */
		return 4;
	case 2:                                 /* ACL: type, handle (2), length (2) */
	case 5:                                 /* ISO: type, handle (2), length (2, 14 bits) */
		return 5;
	case 4:                                 /* event: type, code, length (1) */
		return 3;
	default:
		return 0;
	}
}

static uint32_t
payload_len(const uint8_t *b)
{
	switch (b[0]) {
	case 1:
	case 3:
		return b[3];
	case 4:
		return b[2];
	case 5:
		return obt_le16(b + 3) & 0x3fffu;
	default:
		return obt_le16(b + 3);
	}
}

int
obt_h4_frame_len(const uint8_t *p, uint32_t n)
{
	uint32_t h;

	if (n == 0) {
		return 0;
	}
	if ((h = header_len(p[0])) == 0) {
		return -1;
	}
	if (n < h) {
		return 0;
	}
	h += payload_len(p);
	return h > OBT_H4_MAX ? -1 : (int)h;
}

int
obt_h4_push(struct obt_h4 *p, uint8_t byte)
{
	if (p->complete) {
		obt_h4_reset(p);
	}
	if (p->used == 0) {
		if (byte >= OBT_IBS_WAKE_ACK && byte <= OBT_IBS_SLEEP_IND) {
			return 2;               /* in-band sleep byte, only between packets */
		}
		if ((p->header = header_len(byte)) == 0) {
			return -1;
		}
		p->wanted = p->header;
	}
	p->bytes[p->used++] = byte;
	if (p->used == p->header) {
		uint32_t len = payload_len(p->bytes);

		if (len > OBT_H4_MAX - p->header) {
			obt_h4_reset(p);
			return -2;
		}
		p->wanted = p->header + len;
	}
	if (p->used == p->wanted) {
		p->complete = true;
		return 1;
	}
	return 0;
}

int
obt_cc_status(const uint8_t *e, size_t n, uint16_t opcode)
{
	if (n < 7 || e[0] != 4 || e[1] != 0x0e || (size_t)e[2] + 3 != n || obt_le16(e + 4) != opcode) {
		return -1;
	}
	return e[6];
}

bool
obt_is_vendor_event(const uint8_t *e, size_t n)
{
	return n >= 3 && e[0] == 4 && e[1] == 0xff && (size_t)e[2] + 3 == n;
}

/* 04 ff len 00 02 <product u32> <patch u16> <rom u16> <soc u32> [build string] (btqca.c qca_read_soc_version) */
int
obt_qca_version_parse(const uint8_t *e, size_t n, struct obt_qca_version *v)
{
	if (n < 17 || !obt_is_vendor_event(e, n) || e[3] != 0x00 || e[4] != 0x02) {
		return -1;
	}
	v->product = obt_le32(e + 5);
	v->patch = obt_le16(e + 9);
	v->rom = obt_le16(e + 11);
	v->soc = obt_le32(e + 13);
	return v->soc && v->rom ? 0 : -1;
}

/* 04 ff 03 00 04 <result> (btqca.c qca_tlv_send_segment) */
int
obt_qca_tlv_ack(const uint8_t *e, size_t n)
{
	if (n != 6 || !obt_is_vendor_event(e, n) || e[3] != 0x00 || e[4] != 0x04) {
		return -1;
	}
	return e[5] == 0;
}

/* Linux enum qca_baudrate */
int
obt_qca_baud_index(uint32_t baud)
{
	static const struct { uint32_t baud; uint8_t idx; } t[] = {
		{ 115200, 0 }, { 230400, 5 }, { 460800, 7 }, { 921600, 10 }, { 1000000, 11 }, { 2000000, 13 },
		{ 3000000, 14 }, { 3200000, 17 },
	};

	for (unsigned i = 0; i < sizeof(t) / sizeof(t[0]); i++) {
		if (t[i].baud == baud) {
			return t[i].idx;
		}
	}
	return -1;
}

/*
 * bluetoothd (20H392) was written for Broadcom chips. Its vendor commands (OGF 0x3f) mean other things on a
 * Qualcomm chip (0xfc48 = baud rate, 0xfc00 = EDL, 0xfc14 = BD address): none may reach the WCN3990. Each gets a
 * Command Complete here: status from the table (0 = Broadcom writes without data, else 0x01 Unknown Command),
 * then 8 bytes (byte 0 = the first parameter byte; _BCM_ByteStreamEventParser needs >= 2 bytes, negroni 0337).
 * Also (negroni 0336, apollo 0141/0143): 0x0c5a/0x0c5b when the chip's mask lacks them (status 0), and 0x204a
 * (extended advertising; once used, legacy LE commands get Command Disallowed): status 0, size 0.
 */
static const struct { uint16_t op; uint8_t status; } vsc[] = {
	{ 0xfc01, 0 }, { 0xfc18, 0 }, { 0xfc1c, 0 }, { 0xfc1e, 0 }, { 0xfc27, 0 }, { 0xfc2e, 0 }, { 0xfc45, 0 },
	{ 0xfc4c, 0 }, { 0xfc4e, 0 }, { 0xfc6d, 0 }, { 0xfd3c, 0 }, { 0xfce9, 0 }, { 0xfce2, 0 }, { 0xfd48, 0 },
	{ 0xfd47, 0 },
};

unsigned
obt_local_answer(struct obt_shim *s, const uint8_t *cmd, size_t n, uint8_t ev[OBT_LOCAL_EVENT_MAX])
{
	uint16_t op;
	unsigned len;
	uint8_t status = 0;

	if (n < 4 || cmd[0] != 1 || (size_t)cmd[3] + 4 != n) {
		return 0;
	}
	op = obt_le16(cmd + 1);
	bzero(ev, OBT_LOCAL_EVENT_MAX);
	ev[0] = 4;
	ev[1] = 0x0e;
	ev[3] = 1;
	ev[4] = cmd[1];
	ev[5] = cmd[2];
	if ((op >> 10) == 0x3f) {
		status = 0x01;
		for (unsigned i = 0; i < sizeof(vsc) / sizeof(vsc[0]); i++) {
			if (vsc[i].op == op) {
				status = vsc[i].status;
			}
		}
		ev[7] = n > 4 ? cmd[4] : 0;
		len = 7 + 8;
	} else if (op == 0x204a) {
		len = 8;
	} else if (s->mask_known && ((op == 0x0c5a && !s->err_rd) || (op == 0x0c5b && !s->err_wr))) {
		len = op == 0x0c5a ? 8 : 7;
	} else {
		return 0;
	}
	ev[6] = status;
	ev[2] = (uint8_t)(len - 3);
	return len;
}

/*
 * Chip -> host fixes (in place): Read Local Supported Commands gets octet 18 bits 2/3 (answered here);
 * 0x0c6d Write LE Host Support and 0x2007 LE Read Advertising Channel TX Power errors become status 0
 * (negroni 0336/0337: bluetoothd stops its init on them).
 */
unsigned
obt_fix_event(struct obt_shim *s, uint8_t *e, size_t n)
{
	unsigned r = 0;

	if (n == 7 + 64 && obt_cc_status(e, n, 0x1002) == 0) {
		s->err_rd = (e[7 + 18] & 0x04) != 0;
		s->err_wr = (e[7 + 18] & 0x08) != 0;
		s->mask_known = true;
		e[7 + 18] |= 0x0c;
		r |= 1;
	}
	if (n >= 7 && e[0] == 4 && e[1] == 0x0e && e[2] + 3u == n && e[6] != 0) {
		uint16_t op = obt_le16(e + 4);

		if (op == 0x0c6d || (op == 0x2007 && e[6] == 0x0c)) {
			e[6] = 0;
			if (op == 0x2007 && n >= 8) {
				e[7] = 0;
			}
			r |= 2;
		}
	}
	return r;
}

/* Write LE Host Support: Simultaneous_LE_Host must be 0x00 since Core 4.1 (negroni 0337) */
int
obt_fix_command(uint8_t *c, size_t n)
{
	if (n >= 6 && c[0] == 1 && obt_le16(c + 1) == 0x0c6d && c[3] >= 2 && c[5] != 0) {
		c[5] = 0;
		return 1;
	}
	return 0;
}
