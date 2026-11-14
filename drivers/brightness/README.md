# drivers/brightness: iOS's backlight service and light sensor

Owner: fable-brightness. The host side: `runtime/bridges/brightness/`. No kernel patch: the service is made by
hand from `oneios_drivers_start()`, like the vibrator.

Marks: [V] read in a file here, [I] inference, [U] unknown. The service has run on the phone (sessions of
2026-11-12): backboardd finds it; the slider did not reach it until the fix below (not yet run).
`OneiOSBacklight.cpp` compiles with the kernel's own flags (`oneios-tool kernel drivers --check`).

| File | What | State |
|---|---|---|
| `OneiOSBacklight.cpp` | the backlight service CoreBrightness drives; the host's light readings (`oneios_als_read`, `oneios_als_listen`) | built into `liboneiosdrivers.a` |
| `oneios_als_hid.h` | the light sensor's HID report descriptor and report | host test (`test_als_hid.c`, run by `drivers/tests/host.sh`) |
| `oneios_bl_request.h` | what a `DisplayBrightness` request holds, decoded with integers (the kernel has no floating point) | host test (`test_bl_request.c`, 2028 checks, run by `drivers/tests/host.sh`) |
| `BacklightMode.inc` | backboardd only: CoreBrightness's BLControl in place of VMBLControl (below, "Why the slider did nothing") | user space, appended to the Metal plug-in by `userland/metal/build.sh stage`; not in `liboneiosdrivers.a` |
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

It subclasses the collection's own `IOHIDDevice`, so it is built by `oneios-tool kernel gen hid`
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

## Why the slider did nothing (phone sessions of 2026-11-12, 12:30 and 14:12)

Read in iOS's own log (the 14:12 session: the owner moved the slider) and in 24A446's CoreBrightness (shared cache,
`ipsw dyld disass`; addresses are the cache's):

1. The kernel side worked: `ONEIOS BACKLIGHT: on`, `the service conforms to AppleARMBacklight now`; backboardd found
   the service (`BCAppleBacklightBrtControl: got back service 60675`, `driver params = {brightness = {max = 255; min =
   0; value = 121}}`).
2. The freeze is not the cause: `Freeze brightness:YES ... BKSystemApplicationBacklightLockReason` at 08:42:58.161 is
   undone by backboardd itself 1.2 s later (`Freeze brightness:NO`, `FreezeBrightnessEnable: 0`).
3. The cause: `Set VM Mode 3 BLControl`, then `VMBLControl started`. `-[BrightnessSystemInternal init]` (0x22aa5fe70)
   calls `_CBU_IsVirtualMachine` (0x22aa8e8c0: `MGGetBoolAnswer("IsVirtualDevice")`; libMobileGestalt answers it from
   `sysctlbyname("kern.hv_vmm_present")`, which is the device tree's `/defaults/vmm-present` = 1) and passes
   `initBLControl:` mode 3 on a virtual device, 1 otherwise (2, test mode, only on internal builds). In
   `initBLControl:` mode 3 allocates `VMBLControl`, any other mode `BLControl`. `-[VMBLControl init]` makes a remote
   XPC listener (`xpc_remote_connection_create_remote_service_listener`, `com.apple.virtualization.corebrightness`)
   for a Virtualization.framework host: brightness goes there, never to this service. Without BLControl there is no
   display container: SpringBoard logged `Failed to get initial brightness ... No container for handle Display(id: 1)`
   at each slider look-up, and no `DisplayBrightness` came from the slider.
4. `CBBrightnessBroadcaster init failed; UIBacklightLevelChangedNotification takeover disabled` and
   `power-lut-data-y not found` are logged on this path too; neither stops a request [I].
5. Also seen: `capabilities = {MaxNits = 0; MinNits = 0; ...}`: iOS 27 takes the nits range from
   `IODisplayParameters.BrightnessMilliNits`, which the service did not publish.

The same finding, on iPadOS 27.0.1 with real hardware, is UniXNU-sheng's (`porting.toml` display area,
`external/UniXNU/docs/porting/brightness.md`): vmm-present selects VMBLControl, "which drops every brightness
request"; their fix, `external/UniXNU/scripts/appsync/BacklightMode.m`, gives backboardd mode 1.

### The fix

- **`BacklightMode.inc`**: in backboardd only, `-[BrightnessSystemInternal initBLControl:]` gets mode 1 for 3
  (BLControl, as on an iPhone). vmm-present stays: AMFI, VirtualAudio and bluetoothd read it, and QuartzCore asks
  `IsVirtualDevice` in about 40 places of its window server (framebuffer format, compression, EDR, modes), so hiding
  it from backboardd (`owner-hide-vmm`) would change the display path; droidvm's notes say a non-virtual backboardd
  crashed on its first start. The code runs in backboardd as part of the Metal plug-in: `userland/metal/build.sh stage`
  appends the file to the staged `NativeAdrenoMetalPlugin.mm`. The plug-in is loaded on backboardd's main thread when
  the window server makes its Metal device, 20-90 ms before the same thread instantiates BrightnessSystem (every
  session of 2026-11-12 that had the plug-in). **A session without the plug-in keeps VMBLControl.**
  `oneios-bl-mode=0` (kernel boot argument) turns it off.
- **The service** (`OneiOSBacklight.cpp`), what iOS 27 reads besides, from UniXNU-sheng's working service:
  `IODisplayParameters.BrightnessMilliNits {min, max, value}` in millinits (38, 500000, the current level's);
  `LmaxProduct` (4 bytes, 16.16 nits), `LmidProduct` (125) and `LminProduct` (1) as numbers in nits;
  `NativeSandboxGrantProcess = backboardd` (Droid-VM-UniXNU `zzwb-native-sandbox-grant-set-properties.patch`, in
  the arm64e series: backboardd's profile refuses `FadePeriod` on a backlight that is not Apple's, sheng int-276).
  `brightness-nits` is read as signed 16.16 (zero or less: level 0, the display dark); before, a negative value was
  taken as unsigned and became the top. A `brightness-nits` request now updates `IODisplayParameters` too.

### The next boot's lines

In iOS's log (process backboardd), in this order:

```
OneiOS backlight: physical brightness control selected (initBLControl: mode 3 becomes 1)
OneiOS backlight: initBLControl: mode 3 -> 1 (BLControl, not VMBLControl)
Set Default 1 BLControl                      (CoreBrightness; it said "Set VM Mode 3 BLControl")
capabilities = {... MaxNits = 500; MinNits = 0.038 ...}   [I: the exact numbers are CoreBrightness's]
```

No `VMBLControl started`, no `No container for handle Display(id: 1)` from SpringBoard. In the kernel's log, for
each slider step: `ONEIOS BACKLIGHT: request N from backboardd {brightness-nits} -> level L of 65535 (of the light),
fade 0 ms` (or `{brightness, FadePeriod}` with `(of the slider)`); in xnuhost's log the bridge's `brightness:` lines
with the panel's new value. If the plug-in's line is missing, the plug-in did not load in backboardd (`docs/gpu.md`).
If it is there and no request comes, look for `native-sandbox-grant: backboardd not in ...` in the kernel's log.

## DisplayBrightness (15:35 session, 2026-11-12; read in 24A446's CoreBrightness on the Linux PC)

In the 15:35 session the service got requests whose only key was `DisplayBrightness`, and the driver said `no brightness
in it, nothing sent`. What the key holds, read with `ipsw dyld disass` in the 24A446 shared cache (the SystemOS
cryptex `043-70037-715.dmg`; addresses are the cache's) [V]:

- `__DisplaySetProperty_block_invoke` (0x22a9eeac8): for the key `DisplayBrightness` (`CFEqual`, 0x22a9eeaec), a
  **CFNumber** is read with `CFNumberGetValue(..., kCFNumberFloatType)` (`mov w1, #0xc`, 0x22a9eeb20) and used as
  the **slider's position 0..1**: multiplied by the slider table's points - 1 and interpolated in it (0x22a9eebbc-
  0x22a9eebf8). A value that is not a number goes to 0x22a9eed40: a **CFDictionary** with `Brightness` (a number:
  `CFNumberIsFloatType`, then Float32 or converted to float), `Commit` (a bool) and `Period` (a number, seconds),
  which feed `__DisplaySetLogicalBrightnessWithFade` and `__DisplayReportCommit`. (UniXNU's port notes for another
  board say the same: "scalar DisplayBrightness consumed by CBSliderCommitTelemetry as dictionary".)
- `-[CBDisplayModuleHIDiOS setNits:withPeriod:]` (0x22aac5e20) posts `DisplayBrightness` as a notification and sets
  the panel through an `IOHIDTransaction` (`IOHIDTransactionCommit` in its block): that module drives a HID display,
  not this service.

Not known [U]: how IOKit's serialization gives the kernel a CFNumber of a float type: its IEEE 754 bits, or the
integer the value truncates to (then every position below the top arrives as 0). So the driver
(`OneiOSBacklight::displayBrightness`, `oneios_bl_request.h`) takes a `DisplayBrightness` number, data or text as the
slider's position only when it is unambiguous: a number whose bits are a float or double between 2^-27 and 1, 4 or 8
bytes of data holding one, a decimal text, or the dictionary's `Brightness` in any of these forms with `Period` as the
fade. It is sent as `brightness` is, a level of the slider (`(of the slider)`; the host's curve squares it). The
integers 0 and 1 are not used (0 would put the panel at its floor for every position below the top), nor other
integers. Every such request is logged with what was received:

```
ONEIOS BACKLIGHT: DisplayBrightness number 0x3f400000 (32 bits) -> slider 49151 of 65535
ONEIOS BACKLIGHT: DisplayBrightness dictionary: Brightness number 0x3f000000 (32 bits), Period 0x3e800000 (250 ms), Commit true -> slider 32768 of 65535
ONEIOS BACKLIGHT: DisplayBrightness number 0x0 (32 bits): the integer 0 or 1 (a float that lost its fraction on the way?), nothing sent
```

The third form means the float did not survive the serialization: the fix is then in backboardd (`BacklightMode.inc`:
send `brightness-nits` from the same request) [I]. Written and host-tested on the Linux PC (the decoder); the driver
compiles: `oneios-tool kernel drivers --check` (then `drivers/build.py --check`) against the records of the Linux PC's `HOSTED_OUT=pc` build (2026-11-12, all 16
sources), and it is in that kernel. Not run on the phone.

**Answered (fix-brightness-touch, 2026-11-14) [F]:** the float does not survive. IOKit.framework's binary serializer
(`DoCFSerializeBinary`, 24A446 0x18f07c8f4) reads every CFNumber with `CFNumberGetValue(kCFNumberLongLongType)`
(`mov w1, #0xb` at 0x18f07cb34) and takes the OSNumber's width from a table by the CFNumber's type (0x18f113ec8:
SInt8 8, SInt16 16, SInt32 32, SInt64 64, **Float32 31, Float64 63**). So `Brightness number 0x0 (31 bits)` (phone,
2026-11-13, 550 a second in a Control Center drag) is a Float32 truncated to 0: no form of it reaches the kernel.
No iOS-side code is needed: see the next section.

## The slider, the cap and nits 0 (fix-brightness-touch, 2026-11-14)

The phone, 2026-11-13 (main 9f6dc38): at +4805 CoreBrightness sent, in one millisecond, `{brightness-nits-cap}`,
`{brightness-nits}` -> 27496, `{DisplayBrightness}` (a truncated float), `{brightness-nits}` -> 0 and
`{DisplayBrightnessSliderPosition}`; the panel went to its floor and the host slept the touch IC. A Control Center
drag then sent only `{DisplayBrightnessSliderPosition}` and `{DisplayBrightness}` pairs, both dropped. Read in 24A446's
CoreBrightness [F]:

- `DisplayBrightnessSliderPosition`: `CFNumberCreate(kCFNumberSInt32Type)` of `fcvtzs(position, #16)`, i.e. **signed
  16.16 of the slider's position 0..1** (`__DisplaySetLogicalBrightnessInternal` 0x22aa0d784 and 0x22aa0de14, from
  `__DisplayLogicalToSliderBrightnessInternal`), set on the service alone (`IORegistryEntrySetCFProperty` in
  `__DisplaySetBLDriverProperty_block_invoke` 0x22aa0f7fc). The driver now takes it as the slider's level
  (`(of the slider)`, the host's curve squares it): **the Control Center and Settings sliders move the panel.**
- `brightness-nits-cap`: also `kCFNumberSInt32Type`, 16.16 nits (`SetBLDriverNitsCap` 0x22aa0fdd8; one caller
  computes it with `fcvtzs w1, s0, #0x10` at 0x22a9f31d0). The driver keeps it and holds `brightness-nits` under it when
  it is above 0 (0 or less: no cap); it is logged with its value (`ONEIOS BACKLIGHT: brightness-nits-cap 0x... (N.NNN
  nits, 32 bits)`).
- `brightness-nits`: `kCFNumberSInt32Type` 16.16 (`SetBLDriverNitsValue`), as before. **Nits of 0 or less are no
  level now**: nothing is sent and the panel keeps its level (`ONEIOS BACKLIGHT: brightness-nits 0x0 (0 or less) is no
  level ...`). The display's off is iOS's framebuffer power request (below), not a brightness.
- Why CoreBrightness sent nits 0 at +4805 is not known [U]. Candidates: a cap of 0 (the request-3 value, unlogged
  then, is logged now), or CoreBrightness's own minimum (`capabilities ... MinNits`). The next boot's cap line and
  the slider position that follows say which. Auto-brightness and the ALS are not the cause on that boot's evidence
  [I: AppleUSBALS was refused, so no lux reached CoreBrightness].
- Unknown keys are logged with each value (`... no level in it, nothing sent (key = number 0x... (32 bits); ...)`);
  requests are logged for the first 64, then every 64th (a drag sends some 550 a second).

**The display's power.** `IOMobileFramebufferRequestPowerChange(fb, state)` is `IOConnectCallScalarMethod(selector 12,
{ state })` (24A446 IOMobileFramebuffer `kern_RequestPowerChange` 0x22ac117d0; QuartzCore's
`CA::IOMobileFramebuffer::set_power_state` calls it when backboardd turns the display off or on) [F]. This framebuffer
family answers it unsupported, so hosted `0069` passes the request, after the method, to
`oneios_display_power_request` here, which logs `ONEIOS BACKLIGHT: display off: backboardd[N] asked its framebuffer
(power change 0, answered 0x...)` on each change and sends `OB_BRIGHT_POWER`: the host sleeps the touch IC and floors
the panel at off, brings the level back and wakes the IC at on (`runtime/bridges/brightness/README.md`). Whether
backboardd makes that call at the lock in this guest [U] is what that line says; without it the IC is never slept.

Tests: `test_bl_request.c` (`drivers/tests/host.sh`): 2038 checks, with the 31/63-bit floats, the 16.16 positions
(0, 0.42, 1, a hair over 1, 2, negative) and the nits levels with and without a cap. **Not compiled into a kernel
yet** at the time of writing (`docs/hardware.md` 7a has the state); not run on the phone.

## Only a session can show

1. CoreBrightness finds the service and creates its display (no fault in `_DisplayCreateWithCapabilitiesAndID`).
2. The conformance line, and that nothing calls the original class's methods on the service.
3. The request log: which keys iOS 27 sets from the slider, from Settings, and at idle (`FadePeriod`).
4. That the panel follows (the host's `brightness:` lines in xnuhost's log).
5. The light sensor's path past `AppleUSBALSService`: a report with the host's reading, and CoreBrightness using it.
