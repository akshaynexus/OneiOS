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
   -> Apple's IOHIDInterface -> IOHIDEventDriver (unchanged, in kernel.kc) -> backboardd
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

## Build, boot arguments, log

Built by `kernel/build-hosted.sh` (needs `external/NativeHidInput` at 24f1f3f or later: `scripts/setup/get-references.sh
NativeHidInput`, after removing an older checkout). Boot arguments: `oneios-hid=0` starts nothing; `oneios-hid-log=0`
silences the state lines; `droidvm-input=0` leaves xnuhost's input device alone.

Console lines (`guest` lines of xnuhost's log), in the order of a good start:

```
droidvm input /arm-io/input-0: OneiOS touchscreen, touch
oneios-hid: virtio-input: 1 device(s) in the device tree, 1 started
oneios-hid: buttons: HID device started under IOHIDResource
oneios-hid: touch screen: HID device started under IOHIDResource
oneios-hid: touch screen: event driver IOHIDEventDriver attached (registry id 0x...)
oneios-hid: touch screen: Digitizer/Touch Screen, Built-In, DisplayIntegrated published on the event service; edge wrapper 3
oneios-hid: touch screen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid N, backboardd)
oneios-hid: touch screen: N HID events queued for the service's client(s), 0 dropped
oneios-hid: touch screen: N virtio-input events, N frames, N finger sets, N HID reports (last result 0x0), N contacts begun (N at an edge)
```

The user client line is how the kernel shows that backboardd took the service: it names the process that opened
it. "HID events queued" is the count each such user client keeps of the events Apple's event driver handed it
(its `DebugState`), read once a second while reports arrive. Between the two "event driver" lines of a boot in
which the DriverKit driver does not come up (`AppleUserHIDEventService`, then `IOHIDEventDriver` 120 s later) the
reports are accepted and go nowhere.

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
