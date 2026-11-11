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

#endif
