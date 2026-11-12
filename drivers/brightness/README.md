# drivers/brightness: iOS's backlight service and light sensor

Owner: fable-brightness. The host side: `runtime/bridges/brightness/`. No kernel patch: the service is made by
hand from `oneios_drivers_start()`, like the vibrator.

Marks: [V] read in a file here, [I] inference, [U] unknown. **Nothing here has run**: no session reaches
backboardd yet. `OneiOSBacklight.cpp` compiles with the kernel's own flags (`drivers/build.py --check`).

| File | What | State |
|---|---|---|
| `OneiOSBacklight.cpp` | the backlight service CoreBrightness drives; the host's light readings (`oneios_als_read`, `oneios_als_listen`) | built into `liboneiosdrivers.a` |
| `oneios_als_hid.h` | the light sensor's HID report descriptor and report | host test (`test_als_hid.c`, run by `drivers/tests/host.sh`) |
| `OneiOSALSDevice.cpp` | the HID device that carries the readings to the original `AppleUSBALSService` | built into the hosted kernel's HID archive (`kernel/patches/oneios-hosted/hid`, against its `OriginalHidDeviceABI.h`), not into `liboneiosdrivers.a` |

## The backlight service

Ported from iOSPlus10's Mi A3 work: `2201-laurel-backlight-panel-power.patch` (the `AppleARMBacklight` service
and its requests) and `xnu-12377/post/9005-laurel-backlight-calibration.patch` (what iOS 26's CoreBrightness
must find). The panel code is replaced by one message to the host.

What CoreBrightness does [V iOSPlus10 `patches/xnu/README-brightness.md` for 20H392,
`docs/laurel_sprout/ios26-backboardd.md` for 23F84; for 24A446 only the strings were read: `backlight-control`,
`AppleARMBacklight`, `IODisplayParameters`, `FadePeriod`, `brightness-nits`, `backlight-calibration-parameters`,
`backlight-marketing-table` are all in its CoreBrightness, `CBDisplayContaineriOS` next to the first two]:

| iOS | Here |
|---|---|
| finds the service: property `backlight-control` = true, and `IOObjectConformsTo(service, "AppleARMBacklight")` | the property; conformance: below |
| reads `IODisplayParameters {brightness {min, max, value}}`, `max-nits`, `min-nits`, `brightness-nits` (16.16) | published; brightness 0..255, the host's level at start as the value |
| reads `backlight-calibration-parameters` (`lMax/lMid/lMin` x `Product/Panel`, three currents) and `backlight-marketing-table`; without them 23F84 faults in `_DisplayCreateWithCapabilitiesAndID` | published, 16.16: 17 points, nits = top x s^2, not under the lowest level; currents 0; no `digital-dimming-min-nits` (digital dimming stays off) |
| sets `{brightness, FadePeriod}` (the slider in Control Center and Settings, the dim before sleep, auto-brightness) | `OB_BRIGHT_SET {level = value x 65535 / 255, fade_ms}`; the host squares it |
| sets `brightness-nits` alone | `OB_BRIGHT_SET` with `OB_BRIGHT_LINEAR` |

The first 12 requests are logged whole (`ONEIOS BACKLIGHT: request N from backboardd {keys} -> level ...`),
then every 200th. That log is how a session shows what iOS 27 really sends.

### The class name

[V kernelcache 24A446] The original `AppleARMBacklight` is a class of `com.apple.driver.AppleARMPlatform`
(personality: `IOProviderClass IOPlatformDevice`, `IONameMatch backlight`), and that kext is in OneiOS's kernel
collection. The Mi A3 kernel had no Apple kexts and named its own class `AppleARMBacklight`. Here a second class
of that name would make AppleARMPlatform's start fail (duplicate class). So:

- the class is `OneiOSBacklight`, the service's name is `AppleARMBacklight`;
- `IOObjectConformsTo` is a walk up the object's metaclass chain to the metaclass registered under the name. A
  thread waits (up to 10 min) for the original metaclass, then moves `OneiOSBacklight`'s superclass link from
  `IOService` to it. The original comes from `IOService`, so every cast that held still holds. The link is found
  by value (the one word of the metaclass that points at `IOService`'s); anything unexpected leaves it alone and
  says so. No object of the original class is made and none of its code runs.
- **Risk** [I]: code that looks services up by class `AppleARMBacklight` and calls the original's own methods
  on what it finds would call into this object. Nothing that does so is known to start here (the string is also
  in the ALS color sensor kext, which matches I2C sensors OneiOS does not have). `oneios_bl_conform=0` turns the
  link off; `oneios_bl=0` the whole service.
- Do not add a `backlight` node to the device tree: the original driver would start on it, for hardware that is
  not there.
- If CoreBrightness still refuses the service, the other way is the original driver itself on a `backlight` node
  with its `AppleARMBacklightFunction` served by us; that needs its code read (prelink's tools).

## The light sensor

The host sends `OB_BRIGHT_ALS {millilux, clear, ir, uv}`. The kernel keeps the last one (`oneios_als_read`),
hands each to a listener (`oneios_als_listen`) and shows it on the service (`oneios-als-millilux`).

CoreBrightness reads ambient light from a HID event service [V iOSPlus10 `docs/sensors-plan.md`: `CBALSService`,
`kIOHIDEventTypeAmbientLightSensor`]. On an iPhone that is the AOP's `AppleSPUHIDDriver`. OneiOS's kernel
collection also has `com.apple.driver.AppleHIDALSService` [V]: class `AppleUSBALSService`, an event service for
an `IOHIDInterface` with usage page 0x20 (Sensor), usage 0x41 (Ambient Light), vendor 0x05ac: the sensor of
Apple's USB displays. `OneiOSALSDevice.cpp` is a HID device of that shape (`oneios_als_hid.h`: one input field,
illuminance in millilux), fed by `oneios_als_listen`.

It subclasses the collection's own `IOHIDDevice`, so it is built by `kernel/patches/oneios-hosted/hid/build-oneios-hid.py`
against that folder's `OriginalHidDeviceABI.h` (the class's layout for 24A446 iPhone18,2), its class registers after
IOHIDFamily has loaded, and `oneios_hid_input_start` (the last call of `oneios_drivers_start`) calls
`oneios_als_device_start()` after the touch screen and the buttons are published.

Seen in a PC boot (qemu-aarch64, 2026-11-11, no host sensor): `ONEIOS ALS: on`, then
`oneios-hid: light sensor: OneiOSALSDevice(IOHIDInterface(AppleUSBALSService))`: the original service matches and
attaches to a device that is not on USB. No report was sent (xnuhost had no bridge on the PC), so what the service
does with one is still not seen.

[U] which report fields `AppleUSBALSService` reads, and whether CoreBrightness uses its events for the built-in
display. Until that is seen in a session: the userland should set
`BKEnableALS` false in `com.apple.backboardd` (as the Mi A3's `root-brightness.py`), so backboardd does not
wait for a sensor; auto-brightness is then off and the slider sets the level.

## Start, stop

`oneios_backlight_start()` is the last call of `oneios_drivers_start()`. It needs the bridge's hello with the
brightness bit, asks the host (`OB_BRIGHT_QUERY`) and waits up to 1 s for the answer. Each way out prints one
`ONEIOS BACKLIGHT: on|off: <why>` line. The guest holds nothing of the host's: when the session ends xnuhost
writes One UI's level back.

## The phone, 2026-11-12 (session from 11:53, kernel.kc 8eff915f, runtime 0.1.9-dev)

`ONEIOS BACKLIGHT: on` came 1 s into the boot; then, in 18 minutes, no conformance line of either kind (the thread
waits 600 s at most and then says so) and no request line, though the owner moved iOS's brightness slider twice, and
the host's panel stayed at One UI's 2890. So CoreBrightness never set a property on the service, most likely because it
never found it: without conformance `IOObjectConformsTo(service, "AppleARMBacklight")` is false. Why the conformance
thread said nothing is not known (it may never have run, or it hangs in its look-up of the class). Since then the
driver logs: the conformance thread's start (`waiting for AppleARMBacklight's class ...`) and every 60 s while it
waits (`still no AppleARMBacklight class after N s`), a thread that could not start, every request (no longer the
first 12 and every 200th), and a process that opens the service (`NAME opened the service (type N): 0x...`).
The next boot shows which of these it is.

## Only a session can show

1. CoreBrightness finds the service and creates its display (no fault in `_DisplayCreateWithCapabilitiesAndID`).
2. The conformance line, and that nothing calls the original class's methods on the service.
3. The request log: which keys iOS 27 sets from the slider, from Settings, and at idle (`FadePeriod`).
4. That the panel follows (the host's `brightness:` lines in xnuhost's log).
5. The light sensor's path past `AppleUSBALSService`: a report with the host's reading, and CoreBrightness using it.
