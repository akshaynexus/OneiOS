/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: what a DisplayBrightness request to the backlight service holds, decoded with integers only (the kernel
 * builds with no floating point). Portable C and C++, unit-tested on the host (test_bl_request.c, drivers/tests/host.sh).
 *
 * iOS 27's CoreBrightness (24A446, read with ipsw in the shared cache; README.md "DisplayBrightness"):
 * __DisplaySetProperty_block_invoke takes the key DisplayBrightness either as a number, the slider's position 0..1
 * read as kCFNumberFloatType (it then interpolates its slider-to-nits table at position * (points - 1)), or as a
 * dictionary { Brightness = 0..1 (Float32, or an integer converted to float), Commit = bool, Period = seconds }.
 * How IOKit's serialization hands a CFNumber of a float type to the kernel is not known here [U]: as its 32 or 64
 * bits of IEEE 754, or as the integer the value truncates to. So a number is taken as a fraction only when its bits
 * are a float or double between 2^-27 and 1 (an integer of that size is a float's bit pattern, not a count); the
 * integers 0 and 1 are ambiguous (a truncated float would give 0 for every position below the top: the panel at its
 * floor) and are not used; other integers are not a fraction.
 */
#ifndef ONEIOS_BL_REQUEST_H
#define ONEIOS_BL_REQUEST_H

#include <stdint.h>

#define OBL_FRACTION_FULL 65535u

enum {
	OBL_DECODED = 0,
	OBL_NOT_A_NUMBER = -1,     /* NaN, infinity, negative, out of range, not a float's bits */
	OBL_AMBIGUOUS = -2,        /* the integer 0 or 1: maybe a float that lost its fraction */
};

/* value * scale, rounded, of an IEEE 754 single; -1 for NaN, infinity, a negative number or one of 2^23 or more. */
static inline int64_t
obl_f32_scale(uint32_t bits, uint32_t scale)
{
	uint32_t e = (bits >> 23) & 0xffu, m = bits & 0x7fffffu;
	if (e == 0xffu) {
		return -1;
	}
	if (bits >> 31) {
		return (e | m) ? -1 : 0;
	}
	if (e == 0) {
		return 0;                    /* zero or a denormal */
	}
	int shift = 150 - (int)e;        /* value = (2^23 | m) * 2^-shift */
	if (shift <= 0) {
		return -1;
	}
	if (shift > 63) {
		return 0;
	}
	uint64_t product = (uint64_t)(0x800000u | m) * scale;
	return (int64_t)((product + (1ull << (shift - 1))) >> shift);
}

/* The same for an IEEE 754 double (its mantissa cut to 24 bits). */
static inline int64_t
obl_f64_scale(uint64_t bits, uint32_t scale)
{
	uint32_t e = (uint32_t)(bits >> 52) & 0x7ffu;
	uint64_t m = bits & 0xfffffffffffffull;
	if (e == 0x7ffu) {
		return -1;
	}
	if (bits >> 63) {
		return (e | m) ? -1 : 0;
	}
	if (e == 0) {
		return 0;
	}
	int shift = 1023 + 23 - (int)e;  /* value = ((2^52 | m) >> 29) * 2^-shift */
	if (shift <= 0) {
		return -1;
	}
	if (shift > 63) {
		return 0;
	}
	uint64_t product = (((1ull << 52) | m) >> 29) * scale;
	return (int64_t)((product + (1ull << (shift - 1))) >> shift);
}

/* A scaled value as a fraction 0..OBL_FRACTION_FULL; up to 1% over the top is the top (a float's 1.0000001). */
static inline int32_t
obl_fraction_of(int64_t scaled)
{
	if (scaled < 0 || scaled > (int64_t)OBL_FRACTION_FULL * 101 / 100) {
		return OBL_NOT_A_NUMBER;
	}
	return scaled > (int64_t)OBL_FRACTION_FULL ? (int32_t)OBL_FRACTION_FULL : (int32_t)scaled;
}

/* A number's bits (an OSNumber's value, `bits` wide) as the slider's fraction, or OBL_AMBIGUOUS / OBL_NOT_A_NUMBER. */
static inline int32_t
obl_number_fraction(uint64_t value, unsigned bits)
{
	if (value <= 1) {
		return OBL_AMBIGUOUS;
	}
	if (bits > 32 && value > 0xffffffffull) {
		uint32_t e = (uint32_t)(value >> 52) & 0x7ffu;
		return e >= 1023 - 27 && e <= 1023 ? obl_fraction_of(obl_f64_scale(value, OBL_FRACTION_FULL)) : OBL_NOT_A_NUMBER;
	}
	uint32_t e = (uint32_t)(value >> 23) & 0xffu;
	return value <= 0xffffffffull && e >= 127 - 27 && e <= 127 ? obl_fraction_of(obl_f32_scale((uint32_t)value, OBL_FRACTION_FULL))
	                                                          : OBL_NOT_A_NUMBER;
}

/* 4 or 8 bytes (an OSData, little-endian) of a float or double as the fraction. */
static inline int32_t
obl_data_fraction(const uint8_t *bytes, unsigned length)
{
	uint64_t v = 0;
	if (length != 4 && length != 8) {
		return OBL_NOT_A_NUMBER;
	}
	for (unsigned i = 0; i < length; i++) {
		v |= (uint64_t)bytes[i] << (8 * i);
	}
	return obl_fraction_of(length == 4 ? obl_f32_scale((uint32_t)v, OBL_FRACTION_FULL) : obl_f64_scale(v, OBL_FRACTION_FULL));
}

/* A decimal text ("0.5", "1", ".25") as the fraction. */
static inline int32_t
obl_string_fraction(const char *s)
{
	uint64_t whole = 0, micro = 0, place = 100000;
	int digits = 0;
	while (*s >= '0' && *s <= '9') {
		whole = whole * 10 + (uint64_t)(*s++ - '0');
		digits++;
		if (whole > 1) {
			return OBL_NOT_A_NUMBER;
		}
	}
	if (*s == '.') {
		s++;
		while (*s >= '0' && *s <= '9') {
			micro += place * (uint64_t)(*s++ - '0');
			place /= 10;
			digits++;
		}
	}
	if (*s != 0 || digits == 0) {
		return OBL_NOT_A_NUMBER;
	}
	return obl_fraction_of((int64_t)(((whole * 1000000 + micro) * OBL_FRACTION_FULL + 500000) / 1000000));
}

/* A dictionary's Period (seconds) as milliseconds: a float's bits, or a whole number of seconds; -1 if neither or
 * over 10 s. */
static inline int32_t
obl_period_ms(uint64_t value, unsigned bits)
{
	int64_t ms;
	if (value < 100) {
		ms = (int64_t)value * 1000;
	} else if (bits > 32 && value > 0xffffffffull) {
		ms = obl_f64_scale(value, 1000);
	} else if (value <= 0xffffffffull && ((value >> 23) & 0xffu) >= 100) {
		ms = obl_f32_scale((uint32_t)value, 1000);
	} else {
		return -1;
	}
	return ms < 0 || ms > 10000 ? -1 : (int32_t)ms;
}

#endif
