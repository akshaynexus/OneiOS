// SPDX-License-Identifier: MIT
// The ambient light sensor as a HID device: the report descriptor and the input report of
// OneiOSALSDevice.cpp. Freestanding C, so the bytes are tested on the build host (test_als_hid.c).
//
// Why this shape: OneiOS's kernel collection has com.apple.driver.AppleHIDALSService. Its class
// AppleUSBALSService matches an IOHIDInterface with PrimaryUsagePage 0x20 (Sensor), PrimaryUsage
// 0x41 (Ambient Light) and VendorID 0x05ac: the light sensor of Apple's USB displays. It attaches to
// this device on a PC boot (q4b-hid, 32bb762). In this kernelcache it is a direct IOService subclass
// with "HIDServiceSupport" in its personality, not an IOHIDEventService, so whether its readings
// reach CoreBrightness as ambient light events is not known.
// The descriptor is the HID Sensor Usage Tables' ambient light sensor with one field, the
// illuminance in millilux. What Apple's displays really send is not known here (README.md).
#ifndef ONEIOS_ALS_HID_H
#define ONEIOS_ALS_HID_H

#include <stdint.h>

#define ONEIOS_ALS_VENDOR_ID   0x05acu
#define ONEIOS_ALS_PRODUCT_ID  0x4f41u       // "OA": not a product of Apple's
#define ONEIOS_ALS_REPORT_ID   1u
#define ONEIOS_ALS_REPORT_SIZE 5u

static const uint8_t oneios_als_descriptor[] = {
  0x05, 0x20,                          // Usage Page (Sensor)
  0x09, 0x41,                          // Usage (Environmental: Ambient Light)
  0xa1, 0x00,                          // Collection (Physical)
  0x85, ONEIOS_ALS_REPORT_ID,          //   Report ID
  0x05, 0x20,                          //   Usage Page (Sensor)
  0x0a, 0xd1, 0x04,                    //   Usage (Data Field: Light Illuminance)
  0x17, 0x00, 0x00, 0x00, 0x00,        //   Logical Minimum (0)
  0x27, 0xff, 0xff, 0xff, 0x7f,        //   Logical Maximum (2^31 - 1)
  0x55, 0x0d,                          //   Unit Exponent (-3): millilux
  0x75, 0x20,                          //   Report Size (32)
  0x95, 0x01,                          //   Report Count (1)
  0x81, 0x02,                          //   Input (Data, Variable, Absolute)
  0xc0,                                // End Collection
};

// The input report for a reading: the report ID, then the millilux, little-endian, capped at the
// logical maximum.
static inline void oneios_als_report(uint32_t millilux, uint8_t report[ONEIOS_ALS_REPORT_SIZE])
{
  if (millilux > 0x7fffffffu) millilux = 0x7fffffffu;
  report[0] = ONEIOS_ALS_REPORT_ID;
  report[1] = (uint8_t)millilux;
  report[2] = (uint8_t)(millilux >> 8);
  report[3] = (uint8_t)(millilux >> 16);
  report[4] = (uint8_t)(millilux >> 24);
}

// ---- oneios-als-spu=1 (sensors-als, 2026-11-25): an iPhone's SPU ambient light sensor ----------------------------
// CoreBrightness takes ambient light events from the service of the Apple vendor pair {0xff00, 4} (an iPhone's sensor
// processor's ALS; ios-redfin 23019ab4 xnu/patches/0971 and Jesvi-iOS a13e406 userland/src/inject_sensors.c, both MIT,
// use that pair), Built-In, VendorID 0x5ac. The collection's IOHIDEventDriver attaches to it (OneiOSHidInput.cpp's
// personality: Transport SPU, {0xff00, 4}) but makes no ambient light event of any field, so the lux rides in the three
// fields of an accelerometer, which it does turn into an event (handleAccelReport), and OneiOSALSEvents.cpp's wrapper of
// that driver's dispatch makes the event an ambient light event (setType 12, the lux in its level and illuminance):
//   report 1 (input):   X = lux & 0x7fff, Y = lux >> 15 (both 0..32767), Z = millilux % 1000; 16 bits each, unit
//                       exponent 0 and the physical range the logical one, so the event's IOFixed is each whole number
//   report 2 (feature): Report Interval (Sensor page 0x030e), microseconds, 32 bits: IOHIDEventDriver writes
//                       CoreBrightness's ReportInterval into it (setProperties), which reaches the device's setReport
#define ONEIOS_ALS_SPU_PAGE         0xff00u
#define ONEIOS_ALS_SPU_USAGE        0x0004u
#define ONEIOS_ALS_SPU_INPUT_ID     1u
#define ONEIOS_ALS_SPU_FEATURE_ID   2u
#define ONEIOS_ALS_SPU_REPORT_SIZE  7u
#define ONEIOS_ALS_SPU_FEATURE_SIZE 5u

static const uint8_t oneios_als_spu_descriptor[] = {
  0x06, 0x00, 0xff,                    // Usage Page (Apple vendor 0xff00)
  0x09, 0x04,                          // Usage (Ambient Light Sensor)
  0xa1, 0x01,                          // Collection (Application)
  0x85, ONEIOS_ALS_SPU_INPUT_ID,       //   Report ID 1
  0x05, 0x20,                          //   Usage Page (Sensor)
  0x09, 0x73,                          //   Usage (Accelerometer 3D)
  0xa1, 0x00,                          //   Collection (Physical)
  0x0a, 0x53, 0x04,                    //     Usage (Acceleration Axis X): lux & 0x7fff
  0x0a, 0x54, 0x04,                    //     Usage (Acceleration Axis Y): lux >> 15
  0x0a, 0x55, 0x04,                    //     Usage (Acceleration Axis Z): millilux % 1000
  0x15, 0x00,                          //     Logical Minimum (0)
  0x26, 0xff, 0x7f,                    //     Logical Maximum (32767)
  0x35, 0x00,                          //     Physical Minimum (0)
  0x46, 0xff, 0x7f,                    //     Physical Maximum (32767)
  0x55, 0x00,                          //     Unit Exponent (0)
  0x75, 0x10,                          //     Report Size (16)
  0x95, 0x03,                          //     Report Count (3)
  0x81, 0x02,                          //     Input (Data, Variable, Absolute)
  0xc0,                                //   End Collection
  0x85, ONEIOS_ALS_SPU_FEATURE_ID,     //   Report ID 2
  0x0a, 0x0e, 0x03,                    //   Usage (Property: Report Interval), microseconds
  0x17, 0x00, 0x00, 0x00, 0x00,        //   Logical Minimum (0)
  0x27, 0xff, 0xff, 0xff, 0x7f,        //   Logical Maximum (2^31 - 1)
  0x75, 0x20,                          //   Report Size (32)
  0x95, 0x01,                          //   Report Count (1)
  0xb1, 0x02,                          //   Feature (Data, Variable, Absolute)
  0xc0,                                // End Collection
};

// The input report for a reading: the report ID, then X, Y, Z (little-endian 16 bits). Lux past 2^30 - 1 is capped.
static inline void oneios_als_spu_report(uint32_t millilux, uint8_t report[ONEIOS_ALS_SPU_REPORT_SIZE])
{
  uint32_t lux = millilux / 1000u, rest = millilux % 1000u;
  if (lux > 0x3fffffffu) lux = 0x3fffffffu;
  uint32_t x = lux & 0x7fffu, y = lux >> 15;
  report[0] = ONEIOS_ALS_SPU_INPUT_ID;
  report[1] = (uint8_t)x; report[2] = (uint8_t)(x >> 8);
  report[3] = (uint8_t)y; report[4] = (uint8_t)(y >> 8);
  report[5] = (uint8_t)rest; report[6] = (uint8_t)(rest >> 8);
}

// The lux the wrapper rebuilds from the event's X and Y (OneiOSALSEvents.cpp does the same with the event's fields).
static inline uint32_t oneios_als_spu_lux(uint32_t x, uint32_t y) { return (y << 15) | (x & 0x7fffu); }

// The feature report's Report Interval (microseconds), or -1 when the report is not that one.
static inline int64_t oneios_als_spu_interval(const uint8_t *report, uint32_t length)
{
  if (length < ONEIOS_ALS_SPU_FEATURE_SIZE || report[0] != ONEIOS_ALS_SPU_FEATURE_ID) return -1;
  return (int64_t)((uint32_t)report[1] | (uint32_t)report[2] << 8 | (uint32_t)report[3] << 16 | (uint32_t)report[4] << 24);
}

// A wake, after redfin's 1298: ReportInterval going from 0 (the sensor off) to more. Then the cached reading is sent again at
// once, before CoreBrightness's 250 ms FirstALSSampleTimeout.
static inline int oneios_als_spu_is_wake(uint32_t before_us, uint32_t now_us) { return before_us == 0 && now_us != 0; }

#endif
