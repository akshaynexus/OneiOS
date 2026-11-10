# runtime/bridges/brightness: the panel's backlight and the light sensor

Owner: fable-brightness. The iOS side: `drivers/brightness/`. The phone's facts: `docs/hardware.md` section 7a.

iOS's brightness (Control Center, Settings, the dim before the display sleeps, auto-brightness) arrives as one
message on the bridge channel (`OB_CH_BRIGHTNESS`, `drivers/bridge/oneios_bridge_proto.h`); this bridge writes the
panel. The phone's light sensor goes the other way.

**Never run on the phone.** Host tests pass; the first run is `brightctl` below.

```sh
make test          # 48 checks: the curve, the fade, the sensor's events, a session on a fake backlight device
make               # build/android/brightctl
```

## The route to the panel, and why

**The backlight class device, written as root**: `/sys/class/backlight/panel/brightness` (Samsung's name for the
panel's device; `panel0-backlight`, Qualcomm's name, is tried next, then any device there).

| Route | Verdict |
|---|---|
| Backlight sysfs node as root | **Chosen.** It is the kernel's own interface to the panel driver, under every other route. It needs no process: not system_server (gone in a session), not surfaceflinger or the composer (the return restarts both), no binder. A write is one `pwrite` from xnuhost, so nothing lives outside the session's cgroup and SIGKILL leaves nothing behind but the last level. |
| `SurfaceControl.setDisplayBrightness` from the display helper's dex | Not used. It ends in the composer HAL's `setDisplayBrightness`, which is the path AOSP's display manager uses; on One UI the level is written by system_server through Samsung's lights HAL, so the composer path is unproven on this phone [I]. It would also put brightness in another process (the helper runs outside xnuhost's namespace) with a socket between, and tie it to hidden framework methods on Android 17. |
| Lights HAL (`ILights`, binder) | Not used. A binder client without Android's libraries is what `bt/` had to write for one HAL; the HAL only writes the same sysfs node. |

What is not known until `brightctl info` runs on the phone: the device's name and `max_brightness`. Nothing is
assumed: the node is found and its range is read at open.

## The curve

- The channel's level is 0..65535. A level from iOS's `brightness` property is the slider's position (perceived):
  it is **squared**, as on the Mi A3 (`laurel_level_for`: half the slider is a quarter of the light). A level
  from `brightness-nits` is a fraction of the light and is taken as it is.
- The result is spread over `floor..ceiling` of the node. Floor 1: level 0 is the dimmest setting, **never 0**. iOS
  sets 0 before it sleeps the display and draws black; an OLED panel is dark then, and a killed session cannot
  leave the panel off for the return's screen.
- Ceiling: `max_brightness`, except that 510 and 51000 are halved. Samsung's node carries the high-brightness
  levels above the manual range on some panels (2 x 255, 2 x 25500); those stay One UI's [I: the rule is from
  Samsung's older trees, not read on this phone]. `ONEIOS_BRIGHT_MAX` sets the ceiling by hand.
- Fades (`FadePeriod`): the worker walks the level in 16 ms steps. `br_bright_set` never waits for the panel.
- iOS is told the top is 500 nits (`ONEIOS_BRIGHT_NITS`) [I]; it uses the number for its own model only.

## Restore

`br_bright_open` reads One UI's value; `br_bright_close` (xnuhost's SIGTERM path) writes it back and takes at
most 300 ms. After a SIGKILL the panel keeps iOS's last level, at least the floor; One UI's system_server sets
its own level when it starts again.

## The light sensor

`als_rear`, an evdev device (`/dev/input/event9` on the owner's phone; found by name). It is a Sensortek STK6D2x
on I2C, Samsung's rear flicker sensor (`drivers/optics/stk6d2x_sec.c`). Opened read-only and never grabbed.

- It sends nothing until it is turned on: `1` to `/sys/class/input/eventN/device/enable`. The bridge does that
  if it was off and writes `0` at close. The camera HAL uses the same file; if the reports stop for 3 s the
  bridge turns it on again.
- A report: `REL_RY` clear, `REL_X` infrared, `REL_RX` UV, each the channel's average divided by its gain, + 1;
  then the gains on `EV_ABS`; `EV_SYN`.
- Lux = clear counts x `ONEIOS_ALS_SCALE` millilux (default 1000). **Not calibrated**: the driver has no lux for
  this mode. To calibrate: `brightctl als` in two known lights (a lux meter app on another phone beside it),
  divide.
- Sent to the guest when it changes by a tenth (or 1 lux), at most 5 a second, and every 2 s.
- **It looks out of the back of the phone.** Face up on a table it reads darkness and iOS's auto-brightness
  would dim the screen. The front sensor is on the sensor hub (SSC, behind the sensors HAL): not bridged.

## Into xnuhost

Done in `runtime/xnuhost/src/main.c`: `br_bright_open(NULL)` beside `br_haptic_open`, `.brightness` in
`br_bridge_config`, `br_bright_close` at the stop. `devices/bridge_dev.c` serves the channel: the hello's bit,
`OB_BRIGHT_QUERY` -> `OB_BRIGHT_STATE`, `OB_BRIGHT_SET`, and `OB_BRIGHT_ALS` to the guest.

It grabs no input device, has no realtime thread, writes nothing under `/sys/power`, starts no process.

Needed in the session's namespace (oneos): `/sys/class/backlight/` writable, `/sys/class/input/` writable for the
sensor's `enable`, `/dev/input/event*` as today.

## The phone test: `brightctl`

As root, One UI running. Nothing is installed; the panel's value is restored at the end and on Ctrl-C.

```sh
make -C runtime/bridges/brightness
adb -s R5CX315X6FT push runtime/bridges/brightness/build/android/brightctl /data/local/tmp/brightctl
adb -s R5CX315X6FT shell su -c 'chmod 0755 /data/local/tmp/brightctl'

# 1. What is there: the node, its range, One UI's value, the light sensor
adb -s R5CX315X6FT shell su -c '/data/local/tmp/brightctl info'

# 2. The panel: slider 5, 25, 50, 75, 100, 10 percent, 2 s each, then One UI's value again
adb -s R5CX315X6FT shell su -c '/data/local/tmp/brightctl levels'

# 3. The light sensor, live for 15 s: cover the back of the phone, then point it at a lamp
adb -s R5CX315X6FT shell su -c '/data/local/tmp/brightctl als 15'

# the raw facts, for docs/hardware.md
adb -s R5CX315X6FT shell su -c 'ls -l /sys/class/backlight/; for d in /sys/class/backlight/*; do echo $d $(cat $d/max_brightness) $(cat $d/brightness); done; getevent -pl /dev/input/event9; ls /sys/class/input/event9/device/'

# clean up
adb -s R5CX315X6FT shell su -c 'rm /data/local/tmp/brightctl'
```

What to see: in `levels` the screen steps from very dim to full and back to dim, each with a short fade, and
ends where it was. In `als` a line whenever the light changes (and every 2 s), `clear` near 0 with the back
covered and rising under a lamp. Send back the whole output of each.

Variants: `brightctl levels --ms 1000 1 50 100`; `ONEIOS_BRIGHT_MAX=N` (the ceiling, if full is not as bright as
One UI's slider at its top, or brighter); `ONEIOS_ALS_SCALE=N`.

Turn One UI's adaptive brightness off for the `levels` test, or system_server may write the node between steps.

## Only a session can show

- That CoreBrightness takes the service, and what iOS 27 sends (`drivers/brightness/README.md`).
- That the node is writable in the session's namespace and for xnuhost's SELinux context.
- Whether the squared curve feels right against iOS's slider, and the fades look smooth at 16 ms steps.
- That nothing else writes the node while system_server is gone (the composer, a vendor service).
