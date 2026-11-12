// SPDX-License-Identifier: MIT
// Host test of oneios_bl_request.h: what a DisplayBrightness request may hold, decoded with integers, against the
// host's own floating point.
#include "oneios_bl_request.h"
#include <math.h>
#include <stdio.h>
#include <string.h>

static int failures, checks;
#define CHECK(c) do { checks++; if (!(c)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

static uint32_t f32(float f) { uint32_t b; memcpy(&b, &f, 4); return b; }
static uint64_t f64(double d) { uint64_t b; memcpy(&b, &d, 8); return b; }
static int32_t want(double f) { return (int32_t)floor(f * OBL_FRACTION_FULL + 0.5); }

int main(void)
{
  // A float's bits: every slider step the host's arithmetic agrees with, to one unit of the rounding.
  for (int i = 0; i <= 1000; i++) {
    float f = (float)i / 1000.0f;
    int32_t got = obl_fraction_of(obl_f32_scale(f32(f), OBL_FRACTION_FULL));
    CHECK(got >= want(f) - 1 && got <= want(f) + 1);
    int32_t got64 = obl_fraction_of(obl_f64_scale(f64((double)i / 1000.0), OBL_FRACTION_FULL));
    CHECK(got64 >= want((double)i / 1000.0) - 1 && got64 <= want((double)i / 1000.0) + 1);
  }
  CHECK(obl_fraction_of(obl_f32_scale(f32(0.5f), OBL_FRACTION_FULL)) == 32768);
  CHECK(obl_fraction_of(obl_f32_scale(f32(1.0f), OBL_FRACTION_FULL)) == 65535);
  CHECK(obl_fraction_of(obl_f32_scale(f32(1.0001f), OBL_FRACTION_FULL)) == 65535);     // a float's 1.0 and a little
  CHECK(obl_fraction_of(obl_f32_scale(f32(1.5f), OBL_FRACTION_FULL)) == OBL_NOT_A_NUMBER);
  CHECK(obl_f32_scale(f32(-0.5f), OBL_FRACTION_FULL) == -1 && obl_f32_scale(f32(-0.0f), OBL_FRACTION_FULL) == 0);
  CHECK(obl_f32_scale(f32(NAN), 1) == -1 && obl_f32_scale(f32(INFINITY), 1) == -1 && obl_f32_scale(f32(1e30f), 1) == -1);
  CHECK(obl_f32_scale(f32(1e-30f), OBL_FRACTION_FULL) == 0 && obl_f32_scale(0x00000001u, OBL_FRACTION_FULL) == 0);
  CHECK(obl_f64_scale(f64(0.25), OBL_FRACTION_FULL) == 16384 && obl_f64_scale(f64(-1.0), 1) == -1 && obl_f64_scale(f64(NAN), 1) == -1);

  // A number: the bits of a float or double between 2^-27 and 1; 0 and 1 are ambiguous; other integers are not used.
  CHECK(obl_number_fraction(f32(0.75f), 32) == 49151);
  CHECK(obl_number_fraction(f32(0.75f), 64) == 49151);                                 // an OSNumber of 64 bits, a float's value
  CHECK(obl_number_fraction(f64(0.75), 64) == 49151);
  CHECK(obl_number_fraction(f32(1.0f), 32) == 65535);
  CHECK(obl_number_fraction(0, 32) == OBL_AMBIGUOUS && obl_number_fraction(1, 32) == OBL_AMBIGUOUS && obl_number_fraction(1, 64) == OBL_AMBIGUOUS);
  CHECK(obl_number_fraction(128, 32) == OBL_NOT_A_NUMBER);                             // a count of 255 steps is not a float's bits
  CHECK(obl_number_fraction(255, 32) == OBL_NOT_A_NUMBER && obl_number_fraction(65535, 32) == OBL_NOT_A_NUMBER);
  CHECK(obl_number_fraction(500 << 16, 32) == OBL_NOT_A_NUMBER);                        // 16.16 nits
  CHECK(obl_number_fraction(f32(2.0f), 32) == OBL_NOT_A_NUMBER && obl_number_fraction(f32(-0.5f), 32) == OBL_NOT_A_NUMBER);
  CHECK(obl_number_fraction(f32(1e-9f), 32) == OBL_NOT_A_NUMBER);                       // below 2^-27: not taken for a float
  CHECK(obl_number_fraction(f64(2.0), 64) == OBL_NOT_A_NUMBER);

  // OSData of a float or a double, little-endian.
  uint32_t b = f32(0.5f);
  uint8_t four[4] = { (uint8_t)b, (uint8_t)(b >> 8), (uint8_t)(b >> 16), (uint8_t)(b >> 24) }, eight[8];
  uint64_t d = f64(0.25);
  for (int i = 0; i < 8; i++) eight[i] = (uint8_t)(d >> (8 * i));
  CHECK(obl_data_fraction(four, 4) == 32768 && obl_data_fraction(eight, 8) == 16384 && obl_data_fraction(four, 3) == OBL_NOT_A_NUMBER);

  // Text.
  CHECK(obl_string_fraction("0.5") == 32768 && obl_string_fraction("1") == 65535 && obl_string_fraction(".25") == 16384);
  CHECK(obl_string_fraction("0") == 0 && obl_string_fraction("1.0") == 65535 && obl_string_fraction("0.333333") == 21845);
  CHECK(obl_string_fraction("1.5") == OBL_NOT_A_NUMBER && obl_string_fraction("2") == OBL_NOT_A_NUMBER && obl_string_fraction("") == OBL_NOT_A_NUMBER);
  CHECK(obl_string_fraction("abc") == OBL_NOT_A_NUMBER && obl_string_fraction("0.5s") == OBL_NOT_A_NUMBER && obl_string_fraction(".") == OBL_NOT_A_NUMBER);

  // Period: seconds as a float's bits, or whole seconds.
  CHECK(obl_period_ms(f32(0.25f), 32) == 250 && obl_period_ms(f32(1.5f), 32) == 1500 && obl_period_ms(f64(0.5), 64) == 500);
  CHECK(obl_period_ms(0, 32) == 0 && obl_period_ms(1, 32) == 1000 && obl_period_ms(f32(60.0f), 32) == -1 && obl_period_ms(1000, 32) == -1);

  printf("bl request: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
