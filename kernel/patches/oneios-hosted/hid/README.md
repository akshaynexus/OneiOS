# kernel/patches/oneios-hosted/hid: touch, buttons and the light sensor's HID device

Owner: fable-hid (job Q4b of the board's queue of 2026-11-11). One archive of the hosted kernel,
`build/oneios-hid/liboneioshid.a`, hooked into `kernel/build-hosted.sh` like `aes/`. There is no numbered patch: the
start is called from `oneios_drivers_start` (`drivers/bridge/OneiOSBridge.c`, a weak default there).

```
 S24's evdev (event5 touch, event0/2/4 keys; read, never grabbed)
   -> xnuhost input.c -> virtio-input (/arm-io/input-0)            runtime/xnuhost
   -> DroidVMVirtioInput.c (staged) + OneiOSSideKeys.inc           the transport, in the kernel
   -> NativeHidInput (staged) x 2, under the collection's IOHIDDevice
        "OneiOS touch screen"   Digitizer / Touch Screen, 10 fingers, Built-In, DisplayIntegrated
        "OneiOS buttons"        Consumer Menu (Home), Power, Volume +/-, Telephony Ring Enable, boot keyboard
   -> Apple's IOHIDInterface -> IOHIDEventDriver (unchanged, in kernel.kc; matched by two personalities
      this archive adds to the catalogue, see "Which event driver") -> backboardd
 OneiOSALSDevice (drivers/brightness)   Sensor / Ambient Light, same parent class, same archive
```

## Files

| File | What |
|---|---|
| `OriginalHidDeviceABI.h` | **The layout of the original `IOHIDDevice`** of the 24A446 iPhone18,2 collection ("v54"): members, every virtual in vtable order, the types their names mangle with. Its head lists each slot with its pointer-authentication discriminator and says where each fact was read. Any driver that subclasses `IOHIDDevice` includes it |
| `include/IOKit/hid/IOHIDDevice.h` | The same header under the public header's name, for sources written against IOHIDFamily (NativeHidInput's arm64e build) |
| `layout.py` | Reads the class from the kernelcache into `IOHIDDevice-24A446-v54.json` (run by hand; needs `out/kernel-inputs/24A446/kernelcache.macho` and a `kernel.kc.prelink.json`) |
| `IOHIDDevice-24A446-v54.json` | What the kernelcache says: size, metaclass, vtable, the 207 slots (target, discriminator, whether the target is the family's code), constructor, base destructor. Addresses and numbers only |
| `build-oneios-hid.py` | Checks the header against the record, stages and compiles the sources, generates the bindings, makes the archive |
| `OneiOSHidInput.cpp` | `oneios_hid_input_start`: loads IOHIDFamily and IOHIDEventDriver, registers the classes below the live `IOHIDDevice`, starts the transport, publishes the two devices under `IOHIDResource`, starts the light sensor's device; the log lines |
| `OneiOSSideKeys.inc` | The side keys of the transport: evdev key codes to the buttons device's report bits |

Sources taken unchanged and staged with anchored edits by the build (a changed anchor stops the build):
`external/NativeHidInput/NativeHidInput.cpp` (UniXNU/ShengHidInput **24f1f3f**, its arm64e source; BSD-2-Clause-Patent)
and `external/UniXNU-droidvm/package/DroidVMVirtioInput.c` (BSD-2-Clause-Patent). The edits: report 2 gets the
ringer bit and takes Home and the ringer from the button sink; the edge band is 3 mm of the phone's panel (54 of 1320 px across, 54 of 2868 down, as shares of
the axes), not of the iPad's; the edge wrapper follows a change of event driver; product names; the transport reads the side keys from any device, and takes a device with side
keys and no touch screen.

## The layout header: evidence

A kext in the kernelcache has no symbols. `layout.py` reads, for `IOHIDDevice` in `com.apple.iokit.IOHIDFamily`:

| Fact | Read from | Value |
|---|---|---|
| Instance size | the class's static initializer, `mov w3, #size` before the OSMetaClass constructor (site `0xfffffe000a0bb29c`) | `0xd0` |
| Metaclass object | the same initializer's `x0` | `0xfffffe000b9789e0` |
| Parent | its `x2` | `0xfffffe000b7b2e18` (IOService) |
| Vtable | the table whose slot 7 is the function returning the metaclass (`0xfffffe000a0b3874`) | `0xfffffe0008314580`, 207 slots |
| Each slot's discriminator | the authenticated fixup of the slot (IA key, address diversity) | in the record and in the header's table |
| Which slots are the class's own | the slot's target lies in the family's `__TEXT_EXEC` (`0xfffffe000a0a60e0`..`0xfffffe000a1231b0`) | 57: 0, 1, 7, 12, 14, 21, 23, 76, 77, 81, 82, 86, 87, 91, 92, 93, 101, 132, 140, and 168..206 but 180 |
| Abstract slot | the one slot from 168 on whose target is the kernel's (`__cxa_pure_virtual`) | 180, `newReportDescriptor` |
| Constructor `IOHIDDevice(const OSMetaClass *)` | the one function that stores vtable + 16; `bti c; pacibsp; ...; bl <parent's>` | `0xfffffe000a0b3834` |
| Base destructor | between it and getMetaClass: `bti c; b <parent's>` | `0xfffffe000a0b386c` |
| Abstract class | slots 0 and 1 are `bti c; brk #1` | yes |

The names come from Apple's public `IOHIDDevice.h` at the tag `IOHIDFamily-2238.120.5` (the OS profile's pin;
`external/IOHIDFamily`, `scripts/setup/get-references.sh IOHIDFamily`). A slot's discriminator is a hash of the
mangled name of the function that introduced it, so the header is right about a slot exactly when the compiler,
given the header, signs that slot as the kernelcache does. **`build-oneios-hid.py` checks this at every build**: it
compiles a class that overrides only `newReportDescriptor`, and requires 207 slots, the record's discriminator in
each, an `IOHIDDevice` function exactly in the family's slots, and `sizeof(IOHIDDevice) == 0xd0` (0x88 of this
kernel's IOService + the 0x48 of the members). It also checks the header's comment table against the record.
What that does not prove: the meaning of a member (none is touched), and the argument types of a function whose
mangled name would hash the same by accident (1 in 65535 per slot).

Differences from the public header, each needed for the check to pass: no reserved slots (a kernel for arm64 has
`APPLE_KEXT_VTABLE_PADDING` 0); `IOHIDDevice_Methods` (the `Dispatch` override, from the generated DriverKit header
that is not in the source tree) is spelled out; `didTerminate` (slot 77) is an override in this build, as in the
source, though UniXNU's iOS 17 header did not have it.

The reference (RELEASE) build of the prelink was not needed for this class: the family's own functions are in no
build of ours, and IOService's 168 slots are checked by the same compile (their discriminators too must match).

## The bindings

For each original function the objects name, a stub in kernel text `bti c; b <name>.original`, with
`<name>.original` an absolute symbol at the function's address as linked in `kernel.kc`; XnuPrelink's emitter
retargets the branch when it places the kernel (as `aes/`). 56 functions: the 54 the family has in its slots, the
constructor, the base destructor. The slot of a function is the one its name fills in the check class's vtable.

Two things differ from `aes/`, both because the collection runs slid under xnuhost:

- **Calls by qualified name** (`IOHIDDevice::start(provider)` in a subclass). A kernel compiles these as a load from
  the named class's vtable (`-fapple-kext`), reached through the GOT. A GOT entry for an absolute symbol holds the
  link address, so the original vtable cannot be named. The archive defines `__ZTV11IOHIDDevice` itself: a table
  with every slot signed for its place there with the original discriminator, pointing at the stub (or at the
  kernel's function where the class inherits it). No object's vtable pointer points at it.
- **The metaclass** is an absolute symbol; `OneiOSHidInput.cpp` compares it with the live class after adding the
  slide and gives each owned metaclass the live one as its superclass before the classes register.
  `OSDynamicCast(IOHIDDevice, x)` in a source would use the unslid address: the build refuses it.

## Buttons

| Report 2 bit | Usage | Host key (evdev code) | On the S24 Ultra |
|---|---|---|---|
| 0 | Consumer / Menu (Home) | `KEY_HOMEPAGE` 172 | no such key; Home is the swipe from the bottom edge |
| 1 | Consumer / Power | `KEY_POWER` 116 | event2 |
| 2 | Consumer / Volume Increment | `KEY_VOLUMEUP` 115 | event0 (gpio-keys) |
| 3 | Consumer / Volume Decrement | `KEY_VOLUMEDOWN` 114 | event4 |
| 4 | Telephony / Ring Enable | `KEY_MUTE` 113 flips it; `oneios_hid_ringer_set()` | no ringer switch |

xnuhost reads the keys' devices without `EVIOCGRAB` (`runtime/xnuhost/src/input.c`, `docs/booter.md` 2.9): the
switch's Volume Up + Volume Down watcher sees every press too. iOS also sees both keys of that chord pressed.
Whether iOS 27 takes Ring Enable = 1 as "ring" is a guess from older releases; nothing sets the bit on this phone.

## Touch

The transport takes the panel's own axis ranges from the device (`ABS_MT_POSITION_X/Y` minimum and maximum, as
xnuhost copies them from `EVIOCGABS`) and scales each axis to the digitizer's 0..32767, which iOS maps onto the
whole display: the panel's full range covers the guest's full mode, whatever the two resolutions are. This is right
when the guest's frame fills the panel (the display helper's layer does); a letterboxed frame would need the
offset here. Ten fingers by slot and tracking id; a finger that comes down in the bottom 4 % starts on the edge
(the Home swipe); edge contacts carry `kIOHIDDigitizerEventFromEdgeTip` (NativeHidInput's wrapper of the event
driver's dispatch), without which SpringBoard's edge gestures ignore them.

## Which event driver (lead, 2026-11-11)

The collection would give an `IOHIDInterface` of a Consumer or Digitizer device to the DriverKit event driver
(`AppleUserHIDEventDriver` of `com.apple.AppleUserHIDDrivers.dext`, `DeviceUsagePairs` with pages 12 and 13), and
defers its own kernel personalities (`IOMatchDefer`) until the IOKit daemon has sent every plist. On the S24 Ultra
(session of 2026-11-11, devices started at 14652.6):

| Time | Log |
|---|---|
| 14652.65 | `IOHIDInterface(...): matching deferred by IOHIDEventDriver` |
| 14764.6 | `DK: AppleUserHIDEventDriver-... waiting for server com.apple.driverkit.AppleUserHIDDrivers-...`; `AppleUserHIDEventService attached`, `edge wrapper 0` |
| 14781..14858 | gestures: reports accepted, `0 contacts begun`, nothing queued for backboardd |
| 14884.6 | `DK: ... failed to launch server` (the kernel's 120 s check-in limit, `kIOUserServerCheckInTimeoutMSecs`) |
| 14885.5 | `IOHIDEventDriver attached`, `edge wrapper 3`, backboardd's user client |
| 14896.5 | the DriverKit driver's process is there (pid 160): 12 s too late |
| 14927.. | `46 HID events queued for the service's client(s)` ... `447`, `11 contacts begun (7 at an edge)` |

So touch and buttons were dead for the first 233 s of that boot, and a test in that time shows nothing.
`oneios_hid_personalities` (in `OneiOSHidInput.cpp`, called when the classes are registered) adds two personalities
to the catalogue: `IOHIDEventDriver` on `IOHIDInterface`, `IOProbeScore` 100000, no `IOMatchDefer`, one with
`Transport` `SPI` and usage page 13 (the touch screen), one with `Transport` `Virtual` and usage page 12 (the
buttons). They match as the DriverKit personality does (`IOHIDDevice::matchPropertyTable`), start first, and take
the match category, so neither the DriverKit personality nor Apple's deferred ones start for these two devices.
The light sensor (`Virtual`, Sensor page) is not matched by them. An iPhone's own touch screen and buttons are
kernel event services too (the multitouch and button kexts), not DriverKit ones. `oneios-hid-driverkit=1` adds
nothing (the old order). Why the DriverKit driver's process needs more than 120 s is not looked at; nothing here
uses it.

## What Droid-VM's touch fixes are here (lead, 2026-11-11)

| Droid-VM fix | Here |
|---|---|
| A held finger is sent again every 8 ms with x one axis unit off (`touch_still`, `DROIDVM_TOUCH_STILL_MS`) | in: it is in the transport, which is staged unchanged there. The log line counts the sets resent |
| The touch screen as its own Digitizer device (`sheng-touch-split=1` on its board) | in since Q4b: `native_hid_touch_allocate`, a second `NativeHidInput`, with no boot argument |
| A finger down in the bottom 4 % starts on the edge (`DROIDVM_TOUCH_BOTTOM_SNAP`) | in, same file |
| A swipe from outside the display's black border starts at the border (its host's `EvdevEncoder`) | not here and not needed while the guest's frame fills the panel: there is no border inside the touch area. If the display helper ever letterboxes, xnuhost's `input.c` needs it |

## Build, boot arguments, log

Built by `kernel/build-hosted.sh` (needs `external/NativeHidInput` at 24f1f3f or later: `scripts/setup/get-references.sh
NativeHidInput`, after removing an older checkout). Boot arguments: `oneios-hid=0` starts nothing; `oneios-hid-log=0`
silences the state lines; `oneios-hid-driverkit=1` adds no personalities; `droidvm-input=0` leaves xnuhost's input
device alone.

Console lines (`guest` lines of xnuhost's log), in the order of a good start:

```
oneios-hid: IOHIDEventDriver personalities added for the touch screen and the buttons: the kernel's event driver takes them when they start, the DriverKit one is not waited for
droidvm input /arm-io/input-0: OneiOS touchscreen, touch
oneios-hid: virtio-input: 1 device(s) in the device tree, 1 started
oneios-hid: buttons: HID device started under IOHIDResource
oneios-hid: touch screen: HID device started under IOHIDResource
oneios-hid: touch screen: event driver IOHIDEventDriver attached (registry id 0x...)
oneios-hid: touch screen: Digitizer/Touch Screen, Built-In, DisplayIntegrated published on the event service; edge wrapper 3
oneios-hid: touch screen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid N, backboardd)
oneios-hid: touch screen: N HID events queued for the service's client(s), 0 dropped
oneios-hid: touch screen: N virtio-input events, N frames, N finger sets, N HID reports (last result 0x0), N contacts begun (N at an edge); the last began at x 49.8% y 100.0%, the last lift was at x 51.2% y 41.0%; 212 sets resent for a held finger
```

The two positions are what Apple's event driver put in its events (read by the edge wrapper, 0 % at the left and
at the top of the guest's screen): a swipe up from the middle of the bottom edge begins near x 50 % y 100 % and
lifts at a smaller y. A begin that is not where the finger came down is a mapping fault (axes swapped or turned).

The user client line is how the kernel shows that backboardd took the service: it names the process that opened
it. "HID events queued" is the count each such user client keeps of the events Apple's event driver handed it
(its `DebugState`), read once a second while reports arrive. With `oneios-hid-driverkit=1`, between the two
"event driver" lines (`AppleUserHIDEventService`, then `IOHIDEventDriver` 120 s later) the reports are accepted
and go nowhere, and `contacts begun` stays 0.

## Tested, not tested (2026-11-11)

On the PC, with main at `3202197` plus this work (series up to 0024, the display and the key store in), xnuhost
under `qemu-aarch64 -cpu cortex-a710`, 8 vCPUs, the real image set, guest mode 1440 x 3120, `--no-input
--input-script runtime/xnuhost/tools/input-demo.script` (a 1440 x 3120 panel; a tap, a swipe up from the bottom
edge and the three side keys, repeated), `docs/building.md` section 5:

| What | Seen |
|---|---|
| The header against the kernelcache | every build: 207 slots with the kernelcache's discriminators, 54 family functions placed, 0xd0 bytes |
| The transport | `droidvm input /arm-io/input-0: OneiOS touchscreen, touch`; `1 device(s) in the device tree, 1 started` |
| The devices | both `HID device started under IOHIDResource`, 10 s into the boot |
| Apple's event driver | the interfaces wait for driverkitd (`matching deferred by IOHIDEventDriver`); then the DriverKit driver is tried (`AppleUserHIDEventService`, `waiting for server com.apple.driverkit.AppleUserHIDDrivers`), its process does not start in this guest and after 120 s the kernel's `IOHIDEventDriver` attaches to both |
| backboardd | `touch screen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid 53, backboardd)`, and the same for the buttons: it found the digitizer and holds it open |
| Events | the script's frames become HID reports the family accepts (`last result 0x0`), and Apple's event driver queues its events for backboardd: `touch screen: 444 HID events queued for the service's client(s), 0 dropped`, `buttons: 66 HID events queued ..., 0 dropped` (the user clients' own `DebugState` counts), growing at the script's pace |
| Edge contacts | `21 contacts begun (10 at an edge)`: the taps in the middle and the swipes from the bottom edge, the latter flagged |
| The light sensor | `oneios-hid: light sensor: OneiOSALSDevice(IOHIDInterface(AppleUSBALSService))` |
| xnuhost | `make -C runtime/xnuhost test`: 13190 checks, among them the script's parser and `xh_virtio_ready` |

Not shown: that backboardd or SpringBoard acts on an event (no user-space log was read; no frame is drawn yet).
The two minutes before the kernel's driver takes over are spent waiting for the DriverKit driver's process;
why it does not start is not looked at. Nothing has run on the phone: the real panel's axis ranges, the side
keys' devices and the chord beside iOS are untested.

## Where the start runs (lead, 2026-11-11)

`oneios_hid_input_start` loads IOHIDFamily and IOHIDEventDriver and registers the device classes on the caller's
thread (the board's late start, bsd_init's thread), then logs `oneios-hid: IOHIDFamily and IOHIDEventDriver loaded,
the device classes registered below IOHIDDevice; ...`. Only the wait for IOHIDResource and the devices' start are on
a thread of their own. Reason: class registration (`preModLoad` .. `postModLoad`) on a thread beside bsd_init's kext
loads can deadlock (class-loading lock against the kext lock; native-adreno met it). It was not seen to hang here.

A kernel with **no `oneios-hid` line at all** was built without the call: its `drivers/` came from another checkout
(`docs/building.md` 2, `ONEIOS_REPO`). That, not a hang, is what the first testing image's kernel had.
Tested: five 70 s boots with `--input-script`, both devices start in each.

## The phone (lead, 2026-11-11)

Read from the session log of the S24 Ultra (xnuhost `b528fd2`): xnuhost took `/dev/input/event8 "sec_touchscreen"`
(x 0..4095, y 0..4095) and left `event9 "sec_touchpad"`; both devices started 0.6 s into the boot; the table under
"Which event driver" is that session. Once the kernel's driver had the device, backboardd was sent 447 events and
11 contacts began, 7 of them at an edge. Whether the lock screen moved in that time was not recorded.
The personalities and the two positions in the log are built (kernel `xnu.macho` links) and **not run**: no boot
on the PC since the owner moved all boots to the phone.
