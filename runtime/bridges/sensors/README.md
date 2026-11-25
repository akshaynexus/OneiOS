# runtime/bridges/sensors: the sensor hub (SSC) over QRTR

The Galaxy S24 Ultra's accelerometer, gyroscope, magnetometer and **front** ambient light sensor are on Qualcomm's
sensor hub (SSC: the ADSP's sensors PD, its Sensors Execution Environment "SEE"), not on `/dev/input`. The rear light
sensor (`als_rear`, STK6D2x, evdev) gives 0 lux on the S24 (`docs/TODO.md`). This bridge reads the hub directly, with
no Android framework (system_server is gone in a session): Linux's own QRTR sockets (`AF_QIPCRTR`), QMI service 400
(the SNS client service), protobuf messages. The iOS side: `kernel/patches/oneios-hosted/hid` (the motion devices,
`oneios-motion=`) and `drivers/brightness` (the SPU light sensor, `oneios-als-spu=1`). The switches: `sensors=`,
`sensors-hz=`, `sensors-light=` (`docs/switches.md`, `docs/booter.md` "Sensors"); `sensors=auto` is the default since the
merge into main (2026-11-25, the owner: every new fix on), `sensors=off` opens no socket (as before).

**Not run on the phone.** Host tests pass (below); built on the build laptop; not booted. The first command on the
phone is `oneios-tool phone qrtr` (below): it says whether this route is open on One UI at all.

Marks: [F] read in a file or a source named here, [I] inference, [U] unknown until the phone runs it.

## Files

| File | What |
|---|---|
| `pb.rs` | a protobuf (proto2 wire) coder: varint, zigzag, fixed32/64, float, bytes, nested messages, by field number |
| `sns.rs` | the SEE client protocol: QMI framing (7-byte header, TLVs), `SNS_CLIENT_REQ` (0x20), reports (0x21, jumbo 0x22); SUID lookup (512/768), attributes (1/128), stream config (513), on-change (514), disable (10), samples (1025), errors (130) |
| `qrtr.rs` | Linux's QRTR datagram socket: the name server's lookup (`NEW_LOOKUP` to the control port, `NEW_SERVER` answers, an empty one at the end), `sendto`/`recvfrom`, `poll` |
| `mod.rs` | the hub: [`Hub::open`] (service 400, each sensor's SUID and attributes, the front light chosen), [`Hub::start`] (one thread, the streams), [`display`] (pause while iOS's display is off), the 60 s line; the unit conversions for the guest |
| `tests.rs` | host tests (below) |
| `../tools/sensorctl.rs` | `sensorctl qrtr [--sensors]` (read only) and `sensorctl stream [--seconds N] [--hz N]` on the phone |

Ported from [DenialOfGravity/ios-redfin](https://github.com/DenialOfGravity/ios-redfin) 23019ab4 `phone/sensors/`
(MIT, Copyright (c) 2026 dipa-studio): `sns.c`, `pb.c` and `phone/wifi/qmi.c` (message build and parse: `sns.rs`,
`pb.rs`), `sensor.c` (the lookup, attribute and stream sequence, the data types `accel`, `gyro`, `mag`, `ambient_light`:
`mod.rs`), and the byte vectors of `test_sensd.c` (`tests.rs`). redfin's own QRTR device for XNU (`rqrtr.c`) is not
needed: Linux has `AF_QIPCRTR`.

## The route, and why

| Route | Verdict |
|---|---|
| **QRTR from xnuhost (this bridge)** | **Chosen.** One UI's own sensors HAL reaches the hub the same way; QRTR is in One UI's kernel (the Wi-Fi firmware's WLFW and the modem services use it [F `docs/hardware.md` 3]). Each sensor is one QMI client of ours (its own socket, as redfin's sensd and the HAL); the hub serves every client, so One UI's HAL clients are not touched [I]. Our requests end when our sockets close (the kernel tells the hub `DEL_CLIENT`), so a killed xnuhost leaves nothing streaming and the way back changes nothing. No Android library, no binder, no process outside xnuhost's namespace and cgroup |
| The Android sensors HAL (AIDL `android.hardware.sensors.ISensors/default`, or HIDL 2.x) | The fallback, **not built** (below) |
| Samsung's `/dev/input` sensor nodes | Not there for these: only the rear light sensor is on evdev [F `docs/hardware.md` 7a] |

[U] whether a root process (KernelSU's domain) may open `AF_QIPCRTR` under One UI's SELinux, whether One UI's kernel
lists service 400 to it, and the S24's SEE data type names and light sensors (Samsung may name its own). `oneios-tool
phone qrtr --sensors` answers all three in one command, beside One UI, read only.

### If QRTR is closed on One UI: the sensors HAL (outline, not built)

`sensors=auto` then logs `sensors: off: source old (sensors=auto): ...` and boots as before (no motion device, the rear
light sensor). The way to the same samples would be the vendor's sensors HAL through our binder layer
(`runtime/bridges/hal`, already a client of AIDL and HIDL HALs for the vibrator, thermal and Bluetooth):

1. `servicemanager`'s `android.hardware.sensors.ISensors/default` (AIDL; HIDL `android.hardware.sensors@2.1::ISensors`
   on older vendors), `getSensorsList()` for the handles of `TYPE_ACCELEROMETER` (1), `TYPE_GYROSCOPE` (4),
   `TYPE_MAGNETIC_FIELD` (2) and `TYPE_LIGHT` (5) (Samsung lists its front light sensor there).
2. `initialize(eventQueue, wakeLockQueue, callback)`: two FMQs (AIDL `MQDescriptor`: an ashmem or memfd region, a read
   and a write counter, and an `EventFlag` futex word for the "events written" bit), and an `ISensorsCallback` binder
   object of ours.
3. `batch(handle, samplingPeriodNs, 0)` and `activate(handle, true)` per sensor; then a thread that waits on the
   EventFlag futex (a blocking wait, no polling), reads `Event` records (type, handle, timestamp, 16 floats) from the
   queue and acknowledges wake-up events on the wake lock queue.
4. The HAL serves one client: system_server's sensorservice is stopped in a session, and at the return One UI's
   sensorservice calls `initialize` again, which resets the HAL's queues to its own [I, to test]. So `activate(false)`
   for each sensor and the queues dropped at the stop, and the return's restart of the framework is the restore.

Its cost is the FMQ and `EventFlag` code (`runtime/bridges/hal` has neither yet) and the `ISensorsCallback` service;
`sensors=hal` would be its switch. Not started until `phone qrtr` shows the QRTR route closed.

## What it does

`Hub::open` (xnuhost's boot, before the devices are counted; `docs/booter.md` "Sensors"):

1. A lookup of service 400 at the local node's name server (1.5 s at most). None: `Err`, and `sensors=auto` takes the
   old path.
2. Per data type, one socket: a SUID lookup (`default_only` for the motion sensors; every SUID of `ambient_light`), then
   the attributes of each (name, vendor, stream type, rates). The light used is `sensors-light=N`, else the first whose
   name and vendor do not say "rear" (the S24's rear sensor is not on the hub [F 7a], so this is a guard). Each found
   sensor is logged.
3. Nothing streams yet.

`Hub::start` (with the bridges): the accelerometer and gyroscope at `sensors-hz` (default 50), the magnetometer at
min(hz, 25), the light on change; one thread blocks in `poll()` on the sockets and an eventfd. Each accelerometer sample
makes one motion frame for the guest (with the last gyroscope and magnetometer values); each light reading goes to the
brightness bridge's listener (`Bright::als_feed`: a change of a tenth or 1 lux, or 2 s, then `OB_BRIGHT_ALS`).

- **Pause:** iOS's display off (the bridge device's `OB_BRIGHT_POWER`) stops every stream on the hub (`disable`), its
  display on asks for them again and re-sends the last light reading at once.
- **Close:** at xnuhost's stop every stream is disabled and the sockets closed (a SIGKILL closes them too).
- **A failed socket** (the hub's restart, `ENETRESET`): that sensor streams no more, one error line. [I] An ADSP restart
  during a session is rare; the next session finds it again.

**CPU:** about 125 wake-ups a second of the bridge's thread while iOS's display is on (50 accelerometer, 50 gyroscope,
25 magnetometer reports; the light a few a second at most), each a `recvfrom` and a protobuf parse of under 200 bytes,
and 50 frames a second to the guest (10 virtio-input events each); one wake-up a minute while the display is off. No
timer, no busy loop. [I] well under 1 % of one core; the `oneios-cpu` and `oneios-top` lines of a session show it.

### Units and frame

| | The hub (Android's frame) | The guest's motion device |
|---|---|---|
| Acceleration | m/s^2, the reaction to gravity (face up: z = +9.8) | `ABS_X/Y/Z`: the gravity vector, **256 per g**, iOS's sign (face up: z = -256); +-16 g |
| Angular velocity | rad/s, counter-clockwise positive | `ABS_RX/RY/RZ`: **16 per degree per second**, same sign; +-2000 deg/s |
| Magnetic field | microtesla | `ABS_HAT0X/HAT0Y/HAT1X`: **16 per microtesla**, same sign; +-2000 uT (only with a magnetometer) |
| Light | lux, on change | `OB_BRIGHT_ALS` millilux |

The phone's frame is iOS's panel frame (both portrait, y up the panel) [I: the S24's sensors are in Android's device
frame; a rotated install would show as a wrong rotation in an app]. The device's units are UniXNU-droidvm's virtio motion
device's (`DroidVMMotionHid.inc`: 256 per g, 16 per deg/s); the magnetometer's are ours.

## Lines (cheap, always on)

- `sensors: on: ssc (sensors=auto): service 400 version V at node N port P; accelerometer SUID "name" (vendor), streaming
  A..B Hz, ...; motion device /arm-io/input-K at 50 Hz, the light for the brightness bridge, on change; oneios-motion=15
  oneios-als-spu=1`, or `sensors: off: source old (sensors=auto): WHY: no motion device, the rear light sensor as
  before`, or `sensors: off: sensors=ssc: WHY: ...`.
- `sensors: motion: the kernel's AppleSPUHIDDevice (com.apple.driver.AppleSPU is not in KERNEL): the accelerometer is
  the guest's motion device`, or `sensors: motion: not given to the guest: KERNEL has com.apple.driver.AppleSPU, whose
  AppleSPUHIDDevice would clash with the kernel's (v0.1.29's panic); prelink it again. The light sensor only` (a collection
  prelinked before 2026-11-25's roots: `docs/hardware.md` 7i, "Motion").
- `sensors: light N: SUID "name" (vendor) ...: used`: each light sensor the hub has.
- `sensors: streaming: accelerometer, gyroscope, magnetometer, light (paused while iOS's display is off)`.
- `sensors: paused: iOS's display is off, every stream stopped on the hub` / `sensors: resumed: ...`.
- Every 60 s: `sensors: 60 s: accelerometer N, gyroscope N, magnetometer N samples; N motion frames to the guest, N
  dropped (the guest had no buffers); light N readings, last N.NNN lux; paused N s` (no line for a minute that was all
  paused and quiet).
- `brightness: light sensor on: the sensor hub's ambient_light (SSC, on change; N light sensor(s) there) (the evdev
  sensor is not opened)`.

## sensorctl (on the phone, root, beside One UI)

```sh
sensorctl qrtr             # read only: socket(AF_QIPCRTR), the local node, every QRTR service, service 400 and one lookup
sensorctl qrtr --sensors   # also every SUID of accel, gyro, mag, ambient_light, proximity, pressure, gravity, rotv
sensorctl stream --seconds 10 --hz 50   # the bridge for 10 s: one line a second, then every stream stopped
```

`oneios-tool phone qrtr [--sensors] [--binary FILE] [--save FILE]` runs the first two from the Mac (the staged
runtime's `sensorctl`, or this machine's copy pushed to `/data/local/tmp/oneios-sensorctl` and removed after). A good
phone prints `qrtr: SSC: service 400 version 1 at node N port 0x.. answers: accel lookup gave 1 SUID(s): sensors=auto
takes the hub` and `ok`; a closed one `qrtr: socket(AF_QIPCRTR): ...` or `qrtr: SSC: no service 400 ...` and `failed`.

## Tests

```sh
cd runtime
cargo test -p onebridges sensors::      # 7 tests: the protobuf coder, the requests against redfin's independently
                                        # encoded bytes, responses, lookups, attributes, samples (unpacked, packed,
                                        # jumbo), errors, cut messages; QRTR control packets; the guest's units; the
                                        # front light's choice; samples to the listeners, the counters, the 60 s line
cargo test -p onebridges brightness::external_light   # the hub's light through the brightness bridge
cargo test -p xnuhost motion            # the guest's motion device: its frames, the drop when the guest is behind
```

Not tested on the host: a real hub (no QRTR on the build laptop). The stand-in phone tests `oneios-tool phone qrtr`
(`tools/oneios-tool/tests/phone/qrtr.sh`).
