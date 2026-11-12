// SPDX-License-Identifier: MIT
// Host tests of the battery bridge: a fake power_supply tree and the mapping.
#define _GNU_SOURCE
#include "battery.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int checks, failures;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static void write_node(const char *root, const char *supply, const char *name, const char *text)
{
  char path[600];
  snprintf(path, sizeof path, "%s/%s", root, supply);
  mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/%s/%s", root, supply, name);
  FILE *f = fopen(path, "w");
  if (!f) { perror(path); exit(2); }
  fputs(text, f);
  fclose(f);
}

static br_battery_raw raw_base(void)
{
  br_battery_raw r;
  memset(&r, 0, sizeof r);
  r.capacity = 50; r.present = 1; r.temp = 305; r.voltage = 4000000; r.current = BR_BATTERY_ABSENT;
  r.charge_full = 4900000; r.charge_full_design = 5000000; r.cycles = BR_BATTERY_ABSENT;
  strcpy(r.status, "Discharging");
  return r;
}

static void test_map(void)
{
  br_battery_units u = { 0 };
  br_battery_sample s;
  br_battery_raw r = raw_base();

  br_battery_map(&r, &u, 0, &s);
  CHECK(s.percent == 50 && s.present && !s.plugged && !s.charging && !s.full);
  CHECK(s.temp_deci_c == 305 && s.voltage_mv == 4000 && s.full_mah == 4900 && s.design_mah == 5000 && s.cycles == 0);
  CHECK(s.current_ma == BR_BATTERY_UNKNOWN && s.minutes == BR_BATTERY_MINUTES_UNKNOWN);

  // Charging says plugged in even when no online node is 1; Full says plugged and full, not charging.
  strcpy(r.status, "Charging");
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.plugged && s.charging && !s.full);
  r.capacity = 100; strcpy(r.status, "FULL");
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.percent == 100 && s.plugged && !s.charging && s.full);

  // Not charging: plugged only if a supply says so; full only at 100.
  strcpy(r.status, "Not charging"); r.capacity = 80; r.plugged = true;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.plugged && !s.charging && !s.full);
  r.capacity = 100;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.plugged && !s.charging && s.full);
  r.plugged = false;
  br_battery_map(&r, &u, 0, &s);
  CHECK(!s.plugged && !s.full);

  // A cable that does not keep up: discharging but plugged.
  r = raw_base(); r.plugged = true;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.plugged && !s.charging);

  // Out of range, unknown, missing.
  r = raw_base(); r.capacity = 130; r.temp = 9999; r.voltage = BR_BATTERY_ABSENT; r.present = BR_BATTERY_ABSENT;
  r.charge_full = BR_BATTERY_ABSENT; r.charge_full_design = BR_BATTERY_ABSENT; r.status[0] = 0;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.percent == 100 && s.present && s.temp_deci_c == BR_BATTERY_UNKNOWN && s.voltage_mv == 0 && s.full_mah == BR_BATTERY_DEFAULT_MAH && s.design_mah == BR_BATTERY_DEFAULT_MAH);
  r.capacity = -3; r.present = 0;
  br_battery_map(&r, &u, 4000, &s);
  CHECK(s.percent == 0 && !s.present && s.full_mah == 4000);
  r = raw_base(); r.voltage = 3850;              // millivolts on a driver that reports them
  r.charge_full = 4800; r.charge_full_design = 5000;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.voltage_mv == 3850 && s.full_mah == 4800 && s.design_mah == 5000);
  r = raw_base(); r.charge_full_design = BR_BATTERY_ABSENT; r.cycles = 123;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.design_mah == 4900 && s.cycles == 123);
}

static void test_current(void)
{
  br_battery_sample s;
  br_battery_raw r = raw_base();

  // Milliamps stay milliamps, the sign from the status, either way the node reports it.
  br_battery_units u = { 0 };
  r.current = 300;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.current_ma == -300 && !u.current_ua);
  r.current = -300;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.current_ma == -300);
  strcpy(r.status, "Charging"); r.current = -2000; r.capacity = 90;
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.current_ma == 2000 && !u.current_ua);
  // 10 percent of 4900 mAh is 490 mAh; at 2000 mA that is 14 minutes (490 * 60 / 2000).
  CHECK(s.minutes == 14);

  // Microamps are learned from the size, and then small readings stay microamps.
  u = (br_battery_units){ 0 };
  r = raw_base(); r.current = -350000;
  br_battery_map(&r, &u, 0, &s);
  CHECK(u.current_ua && s.current_ma == -350);
  // 50 percent of 4900 is 2450 mAh; at 350 mA that is 420 minutes.
  CHECK(s.minutes == 420);
  r.current = -15000;
  br_battery_map(&r, &u, 0, &s);
  CHECK(u.current_ua && s.current_ma == -15 && s.minutes == BR_BATTERY_MINUTES_UNKNOWN);      // too little for an estimate
  // A fixed unit is not learned over.
  br_battery_units m = { .current_fixed = true, .current_ua = false };
  r.current = 25000;
  br_battery_map(&r, &m, 0, &s);
  CHECK(!m.current_ua && s.current_ma == -25000);
  // Not charging keeps the node's own sign, and no estimate.
  r = raw_base(); strcpy(r.status, "Not charging"); r.plugged = true; r.current = -40;
  br_battery_units n = { 0 };
  br_battery_map(&r, &n, 0, &s);
  CHECK(s.current_ma == -40 && s.minutes == BR_BATTERY_MINUTES_UNKNOWN);
}

static void test_changed(void)
{
  br_battery_units u = { 0 };
  br_battery_raw r = raw_base();
  br_battery_sample a, b;
  br_battery_map(&r, &u, 0, &a);
  b = a;
  CHECK(!br_battery_changed(&a, &b, 0) && !br_battery_changed(&a, &b, 999999));
  b.percent = 49;
  CHECK(br_battery_changed(&a, &b, 0));
  b = a; b.plugged = true;
  CHECK(br_battery_changed(&a, &b, 0));
  b = a; b.charging = true;
  CHECK(br_battery_changed(&a, &b, 0));
  b = a; b.full = true;
  CHECK(br_battery_changed(&a, &b, 0));
  b = a; b.temp_deci_c = 310;
  CHECK(!br_battery_changed(&a, &b, BR_BATTERY_TELEMETRY_MS - 1) && br_battery_changed(&a, &b, BR_BATTERY_TELEMETRY_MS));
  b = a; b.voltage_mv = 3999;
  CHECK(!br_battery_changed(&a, &b, 1000) && br_battery_changed(&a, &b, BR_BATTERY_TELEMETRY_MS));
  b = a; b.current_ma = -1;
  CHECK(br_battery_changed(&a, &b, BR_BATTERY_TELEMETRY_MS));
}

static void fake_phone(const char *root)
{
  write_node(root, "battery", "capacity", "73\n");
  write_node(root, "battery", "status", "Discharging\n");
  write_node(root, "battery", "present", "1\n");
  write_node(root, "battery", "temp", "312\n");
  write_node(root, "battery", "voltage_now", "4012000\n");
  write_node(root, "battery", "current_now", "-420000\n");
  write_node(root, "battery", "charge_full", "4800000\n");
  write_node(root, "battery", "charge_full_design", "5000000\n");
  write_node(root, "battery", "battery_cycle", "212\n");
  write_node(root, "usb", "online", "0\n");
  write_node(root, "ac", "online", "0\n");
  write_node(root, "wireless", "online", "0\n");
  write_node(root, "otg", "online", "1\n");            // a device on the port: the phone gives power, it does not get it
}

static void test_sysfs(const char *root)
{
  static const char *const supplies[] = { "usb", "ac", "wireless", "pogo", NULL };
  br_battery_raw r;
  fake_phone(root);
  CHECK(br_battery_read_raw(root, supplies, &r) == 0);
  CHECK(r.capacity == 73 && !strcmp(r.status, "Discharging") && r.present == 1 && r.temp == 312);
  CHECK(r.voltage == 4012000 && r.current == -420000 && r.charge_full == 4800000 && r.charge_full_design == 5000000);
  CHECK(r.cycles == 212 && !r.plugged);
  write_node(root, "usb", "online", "1\n");
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && r.plugged);
  write_node(root, "usb", "online", "0\n");
  write_node(root, "ac", "online", "2\n");
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && r.plugged);
  write_node(root, "ac", "online", "0\n");

  // A number that is not one counts as missing; no capacity is no battery.
  write_node(root, "battery", "temp", "cold\n");
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && r.temp == BR_BATTERY_ABSENT);
  write_node(root, "battery", "temp", "312\n");
  write_node(root, "battery", "cycle_count", "7\n");
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && r.cycles == 7);       // cycle_count first
  CHECK(br_battery_read_raw("/nonexistent/power_supply", supplies, &r) == -1);
}

// The owner's S24 Ultra on USB, as read on 2026-11-11: Full at 100 %, an empty cycle_count, the cycles in battery_cycle,
// and battery/online holding a cable type, which must not be read as a supply.
static void test_probe(const char *root)
{
  static const char *const supplies[] = { "usb", "ac", "wireless", "pogo", NULL };
  write_node(root, "battery", "capacity", "100\n");
  write_node(root, "battery", "status", "Full\n");
  write_node(root, "battery", "present", "1\n");
  write_node(root, "battery", "temp", "389\n");
  write_node(root, "battery", "voltage_now", "4294000\n");
  write_node(root, "battery", "current_now", "74\n");
  write_node(root, "battery", "charge_full", "5000000\n");
  write_node(root, "battery", "charge_full_design", "5000000\n");
  write_node(root, "battery", "cycle_count", "");
  write_node(root, "battery", "battery_cycle", "1103\n");
  write_node(root, "battery", "online", "19\n");
  write_node(root, "usb", "online", "1\n");
  write_node(root, "ac", "online", "0\n");
  write_node(root, "wireless", "online", "0\n");
  write_node(root, "pogo", "online", "0\n");
  br_battery_raw r;
  br_battery_units u = { 0 };
  br_battery_sample s;
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && r.cycles == 1103 && r.plugged);
  br_battery_map(&r, &u, 0, &s);
  CHECK(s.percent == 100 && s.present && s.plugged && !s.charging && s.full);
  CHECK(s.temp_deci_c == 389 && s.voltage_mv == 4294 && s.full_mah == 5000 && s.design_mah == 5000 && s.cycles == 1103);
  CHECK(s.current_ma == 74 && s.minutes == BR_BATTERY_MINUTES_UNKNOWN);
  // Unplugged, usb/online 0 and Discharging: nothing says plugged, though battery/online still holds a number.
  write_node(root, "usb", "online", "0\n");
  write_node(root, "battery", "status", "Discharging\n");
  CHECK(br_battery_read_raw(root, supplies, &r) == 0 && !r.plugged);
  br_battery_map(&r, &u, 0, &s);
  CHECK(!s.plugged && !s.charging && !s.full);
  write_node(root, "battery", "cycle_count", "7\n");    // later tests start from a clean tree
  char path[600];
  snprintf(path, sizeof path, "%s/battery/online", root);
  remove(path);
}

static struct { pthread_mutex_t lock; int count; br_battery_sample last; } heard = { PTHREAD_MUTEX_INITIALIZER, 0, { 0 } };

static void listener(void *ctx, const br_battery_sample *s)
{
  (void)ctx;
  pthread_mutex_lock(&heard.lock);
  heard.count++;
  heard.last = *s;
  pthread_mutex_unlock(&heard.lock);
}

static int heard_count(void)
{
  pthread_mutex_lock(&heard.lock);
  int n = heard.count;
  pthread_mutex_unlock(&heard.lock);
  return n;
}

static bool wait_heard(int count)
{
  for (int i = 0; i < 400; i++) {
    if (heard_count() >= count) return true;
    usleep(5000);
  }
  return false;
}

static void test_session(const char *root)
{
  fake_phone(root);
  br_battery_options o = { .dir = root, .poll_ms = 10 };
  br_battery *b = br_battery_open(&o);
  CHECK(b != NULL);
  if (!b) return;
  br_battery_sample s;
  CHECK(br_battery_query(b, &s) == 0 && s.percent == 73 && !s.plugged && s.current_ma == -420);

  // The worker is idle until a listener comes, then reports only what changed.
  br_battery_listen(b, listener, NULL);
  usleep(60000);
  CHECK(heard_count() == 0);
  write_node(root, "battery", "status", "Charging\n");
  write_node(root, "usb", "online", "1\n");
  CHECK(wait_heard(1));
  pthread_mutex_lock(&heard.lock);
  CHECK(heard.last.plugged && heard.last.charging && heard.last.current_ma == 420);
  pthread_mutex_unlock(&heard.lock);
  int after_plug = heard_count();
  write_node(root, "battery", "capacity", "74\n");
  CHECK(wait_heard(after_plug + 1));
  pthread_mutex_lock(&heard.lock);
  CHECK(heard.last.percent == 74);
  pthread_mutex_unlock(&heard.lock);

  // Telemetry alone waits for its interval.
  int n = heard_count();
  write_node(root, "battery", "temp", "333\n");
  usleep(60000);
  CHECK(heard_count() == n);

  // A battery node that vanishes costs a log line; the last sample stays.
  char path[600];
  snprintf(path, sizeof path, "%s/battery/capacity", root);
  CHECK(remove(path) == 0);
  CHECK(br_battery_query(b, &s) == -1 && s.percent == 74);
  write_node(root, "battery", "capacity", "60\n");

  // Listening stops and close joins promptly.
  br_battery_listen(b, NULL, NULL);
  n = heard_count();
  usleep(40000);
  CHECK(heard_count() == n);
  br_battery_close(b);
}

// The boot of 2026-11-12 23:45: the phone on its charger, Full at 100 %. The guest's query gets the real state (plugged
// and full) before its service registers; nothing follows while it stays so; an unplug and a plug are each reported.
static void test_session_on_charger(const char *root)
{
  fake_phone(root);
  write_node(root, "battery", "capacity", "100\n");
  write_node(root, "battery", "status", "Full\n");
  write_node(root, "battery", "current_now", "74\n");
  write_node(root, "usb", "online", "1\n");
  br_battery_options o = { .dir = root, .poll_ms = 10 };
  br_battery *b = br_battery_open(&o);
  CHECK(b != NULL);
  if (!b) return;
  br_battery_sample s;
  CHECK(br_battery_query(b, &s) == 0 && s.percent == 100 && s.present && s.plugged && s.full && !s.charging);
  pthread_mutex_lock(&heard.lock);
  heard.count = 0;
  pthread_mutex_unlock(&heard.lock);
  br_battery_listen(b, listener, NULL);
  usleep(80000);                                         // eight polls of the same state
  CHECK(heard_count() == 0);

  write_node(root, "usb", "online", "0\n");
  write_node(root, "battery", "status", "Discharging\n");
  CHECK(wait_heard(1));
  pthread_mutex_lock(&heard.lock);
  CHECK(!heard.last.plugged && !heard.last.full && !heard.last.charging);
  pthread_mutex_unlock(&heard.lock);

  int n = heard_count();
  write_node(root, "usb", "online", "1\n");
  write_node(root, "battery", "status", "Charging\n");
  CHECK(wait_heard(n + 1));
  pthread_mutex_lock(&heard.lock);
  CHECK(heard.last.plugged && heard.last.charging);
  pthread_mutex_unlock(&heard.lock);
  n = heard_count();
  usleep(60000);
  CHECK(heard_count() == n);                             // one report per change
  br_battery_listen(b, NULL, NULL);
  br_battery_close(b);
}

int main(void)
{
  char root[200];
  snprintf(root, sizeof root, "%s/oneios-battery-test.%d", getenv("TMPDIR") ? getenv("TMPDIR") : "/tmp", (int)getpid());
  mkdir(root, 0755);
  test_map();
  test_current();
  test_changed();
  test_sysfs(root);
  test_probe(root);
  br_battery_options none = { .dir = "/nonexistent/power_supply" };
  CHECK(br_battery_open(&none) == NULL);
  test_session(root);
  test_session_on_charger(root);
  char cmd[260];
  snprintf(cmd, sizeof cmd, "rm -rf %s", root);
  if (system(cmd)) printf("could not remove %s\n", root);
  printf("battery: %d checks, %d failures\n", checks, failures);
  return failures ? 1 : 0;
}
