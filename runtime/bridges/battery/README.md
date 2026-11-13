# runtime/bridges/battery: the phone's battery for iOS

The iOS side: `drivers/battery/`. The mapping table: `docs/hardware.md` section 7c.

iOS's battery UI comes from `powerd`, which reads an `IOPMPowerSource`. This bridge reads Android's `power_supply`
class and tells the guest what it finds. It writes nothing to the phone.

**One phone session** (2026-11-12 23:45, on the charger): `battery: on: /sys/class/power_supply, status "Full", 100%,
plugged in; read every 3000 ms while iOS listens` at +0.2 s, and iOS played a "plugged in" sound before its lock
screen (`drivers/battery/README.md`, "The charging chime"). The nodes were also read on the phone by hand
(`docs/hardware.md` 7c) and are a test case here; `batteryctl` below has not run there.

```sh
make test          # 65 checks: the sysfs parse, the mapping, the change test, sessions on a fake sysfs (one on the charger)
make               # build/android/batteryctl
```

## What it reads

`/sys/class/power_supply/battery/{capacity,status,present,temp,voltage_now,current_now,charge_full,charge_full_design,cycle_count or battery_cycle}`,
and `online` in each of `usb`, `ac`, `wireless`, `pogo`. `capacity` is the only node it cannot do without. `otg` and
vendor charger nodes are left out on purpose: a device on the port draws power from the phone.

Units are normalized, because drivers disagree. On the owner's phone (2026-11-11, on USB at Full) `voltage_now` is µV, `charge_full` is µAh and `cycle_count` is empty, so `battery_cycle` is used; `current_now` read 74, which does not settle its unit:

- `voltage_now`: >= 100000 is µV, otherwise mV.
- `current_now`: >= 20000 is µA and from then on the supply stays in µA; below that it is mA. The sign comes
  from `status` (Charging is +, Discharging is -), since drivers differ; other statuses keep the node's own sign.
- `charge_full`, `charge_full_design`: >= 100000 is µAh, otherwise mAh. Neither there: 5000 mAh.
- The level is `capacity`; `charge_counter` is not used. Remaining mAh is `charge_full` x level.

## When it reports

One worker thread, SCHED_OTHER, which sleeps until the guest's first query and every 3 s reads the nodes. The query
is answered with a fresh reading (the guest publishes it before its service registers) and counts as sent: the same
state is never reported again, so the start is never a change. A change
of level, charging state, power or presence goes to the guest at once; temperature, voltage, current and the time
estimate go at most every 30 s. A failed read logs once and keeps the last value. Only reads, so a killed session
leaves nothing behind.

Environment (xnuhost's): `ONEIOS_BATTERY_SUPPLIES=usb,ac,...`, `ONEIOS_BATTERY_MAH=N` (rated capacity),
`ONEIOS_BATTERY_CURRENT_UNIT=u|m` (fixes the unit).

## Into xnuhost

Done in `runtime/xnuhost/src/main.c`: `br_battery_open(NULL)`, `.battery` in `br_bridge_config`, `br_battery_close`
at the stop. `devices/bridge_dev.c` serves `OB_CH_BATTERY`: the hello's bit, `OB_BATTERY_QUERY` -> `OB_BATTERY_STATE`,
then changes until a reset.

Needed in the session's namespace: `/sys/class/power_supply/` readable. It grabs no input device, has no realtime
thread, writes nothing under `/sys/power`, starts no process.

## On the phone

As root, One UI running. Reads only.

```sh
make -C runtime/bridges/battery
adb push runtime/bridges/battery/build/android/batteryctl /data/local/tmp/batteryctl
adb shell su -c 'chmod 0755 /data/local/tmp/batteryctl'
adb shell su -c '/data/local/tmp/batteryctl'            # one sample: last line "batteryctl: ok"
adb shell su -c '/data/local/tmp/batteryctl watch 60'   # plug and unplug in that minute: a line for each change
# the raw facts, for docs/hardware.md
adb shell su -c 'cd /sys/class/power_supply; ls; for d in *; do echo "$d: type=$(cat $d/type 2>/dev/null) online=$(cat $d/online 2>/dev/null)"; done; for n in capacity status present temp voltage_now current_now current_avg charge_full charge_full_design cycle_count battery_cycle; do echo "$n=$(cat battery/$n 2>/dev/null)"; done'
adb shell su -c 'rm /data/local/tmp/batteryctl'
```

What to see: `sample:` with One UI's percent, `plugged=0` on battery and `plugged=1 charging=1` on a charger, voltage
about 3500..4500 mV, temperature about 200..450, current in hundreds of mA with the sign following the charger.

The whole path works when an iOS session's lock screen shows the same percent and the bolt on a charger. In
xnuhost's log: `battery: on: ...`; in the guest's: `ONEIOS BATTERY: on: IOPMPowerSource published at N%`.
