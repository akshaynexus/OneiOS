# Hardware: what the Galaxy S24 Ultra has and how iOS reaches it

Owner of this file: the bridges agent (`drivers/`, `runtime/bridges/`). Date: 2026-11-10.

iOS 27 runs on a source-built XNU inside xnuhost, a process on One UI's kernel. One UI's kernel, init services,
vendor HALs, audioserver, cameraserver and surfaceflinger keep running in a session; zygote, system_server and every
app are gone. So each device has two halves:

- a **driver in the kernel** (`drivers/`, or one taken unchanged from UniXNU-droidvm) that iOS's daemons talk to;
- a **host bridge** in xnuhost (`runtime/bridges/`) that uses the Linux or Android interface. The host side (xnuhost,
  the bridges, the helpers) is Rust since 2026-11-23 (`runtime/RUST.md`: a port of the C, one module per C file, every
  log line byte for byte); **no phone run of the Rust runtime yet**: every phone result in this file is of the C build.
  So are the host-test names and check counts given with a date (`test_bridges` 377, `test_wifi` 301, `test_xnuhost`,
  ASan and UBSan, `make test`): each C test is now a Rust test with the same cases (`cd runtime && cargo test -p
  onebridges`, `-p xnuhost`, ...; `runtime/tests/host.sh` runs them all), and the C files named in dated records are
  now the Rust modules of the same names (`hci.c` -> `runtime/bridges/bt/hci.rs`).

Marks: **[F]** read in a source at a pinned commit, or reported from the owner's phone (SM-S928B, kernel
`6.1.162-android14-11`, board notes of 2026-11-10 in `docs/agents.md`). **[I]** inference. **[U]** unknown until tried
on the phone. **Nothing in this file has run on the phone**: the host bridges pass their unit tests on the PC, build
for Android, and the kernel-side drivers compile and link-check against the kernel build.

## 0. State at a glance

| Device | Kernel side | Host side | State |
|---|---|---|---|
| CPU | `droidvm-amp.patch` + `DroidVMCpufreq.c` (UniXNU-droidvm, unchanged) | `runtime/bridges/cpu`: topology, vCPU plan, thread placement, the cpufreq device | host built and tested; kernel side waits for release-kernel (series, archive) and booter (device tree, placement calls) |
| GPU | UniXNU-droidvm's virtio-gpu 3D + NativeAdreno user client (unchanged) | booter's `runtime/xnuhost/src/virtio_gpu.rs` + drm2kgsl | booter's; not owned here (section 2) |
| Wi-Fi / network | `kernel/patches/oneios-hosted/net`: `IO80211Controller`/`IO80211Interface` under the collection's `IOEthernetController`/`IOEthernetInterface`, a station that asks the host, UniXNU-droidvm's virtio-net transport (section 3) | `runtime/bridges/wifi`: wlan0 is iOS's for the session: frames by AF_PACKET, scans by nl80211, joins by the vendor's supplicant (AIDL V2 over `/dev/binder`); `wifictl`; the old tap/NAT route only with `wifi=tap` | both built; host tested (132 + 35 checks, the binder client on a real binder driver); on the phone beside One UI: the supplicant's service and an nl80211 scan (53 networks) answer our client; **not yet run in a session** (wifid, a join, DHCP) |
| Bluetooth | `drivers/bluetooth` (port of iOSPlus10's transport) on `drivers/bridge` | `runtime/bridges/bt`: the HAL over `/dev/hwbinder` (HIDL), or over `/dev/binder` when the phone has only the AIDL HAL (the Galaxy A56, 2026-11-24; host-tested, not run on an A56) | both built; on by default in a session since 2026-11-11; the HAL (HIDL 1.1) is found in every session, but until 2026-11-23 bluetoothd never powered on (in a VM it powers on only with transport 10). Fixed 2026-11-23 (section 4, "Bluetooth powers on"): the nub's transport-encoding 10 and `BT_HCI_TRANSPORT=H4:POSIX` in bluetoothd's environment (hosted 0087); host-tested and built, not yet booted |
| AirDrop | none: the guest's Wi-Fi driver refuses wifip2pd's `VIRTUAL_IF_*` and `AWDL_*` requests, so iOS has no `awdl0` | `wifictl p2p` (read only): every radio's peer-to-peer paths and Google's Wonder radio (its subcommands, who holds it); `wifictl wonder`: a session on Wonder's raw path beside wlan0, AWDL's frames counted, everything closed after it and wlan0's link checked (`oneios-tool phone wifi p2p\|wonder`) | does not work: iOS has no `awdl0`; Bluetooth (sharingd needs it: the toggle could not switch) was off in every session until the fix of 2026-11-23 (section 4, not yet booted). The radio path exists: One UI's driver has Google's open Wonder raw 802.11 path (PASSTHRU vdev, nl80211 vendor commands under `00:1a:11`, source in Samsung's `S928BXXU5DZDP` release); Google's AWDL engine is closed, so ours must be OWL or our own. The Wonder tools are built and host-tested, not run on the phone (section 4a) |
| Haptics | `drivers/haptics` (port of iOSPlus10's LaurelVibrator and haptic node) and, since 2026-11-12, the Actuator IOAudio2Device of the audio archive (UI and keyboard taps) | `runtime/bridges/haptics`: evdev force feedback | both built; host tested with a fake actuator; the `vibrator` path ran in sessions (alerts), the Actuator path: Core Haptics' server gave up at its start for want of a preset, fixed by hosted 0078; then the Actuator ran I/O in silence for want of its DSP chain, hosted 0086, not yet on the phone (section 5) |
| Audio | `DroidVMVirtioSnd.c` + `DroidVMAudio.cpp`: the hosted kernel's audio archive since 2026-11-12 (before: not built, no device in iOS) | `devices/virtio_snd.rs`, `audio/aaudio_helper.rs` (`oneios-audio`) | helper run on the phone beside One UI (plays and records, 0 underruns, about 130 ms); host-tested against a fake AAudio; in sessions iOS's I/O thread failed for large buffers (system sounds 3 s late or lost): hosted 0052, not yet run; no volume (audiomxd threw at every volume set: no hardware volume control on the Speaker) and the host at One UI's media volume: the control and the host at full scale, 2026-11-22, not yet on the phone (section 6, "Volume") |
| Camera | `drivers/camera`: the `oneios,camera` transport, `/dev/oneios-camera`; `userland/camera`: the capture plug-in (`H9ISPCaptureDeviceCreate`) | `runtime/bridges/camera`: helper `oneios-camera` (provider HAL in a session, Camera2 NDK with One UI), ring, device, `camctl` | host and kernel sides built and host-tested; in iOS (2026-11-22) the plug-in makes its device, CMCapture made no source (no `/product unique-model`: fixed with the camera nodes, not run yet, section 7); one phone run (2026-11-11): cameraserver lists 4 cameras and opens one for a root client, no frame arrived (no binder threads in the process: fixed, the fix not yet run on the phone); the HAL back end never run (section 7) |
| Backlight and light sensor | `drivers/brightness`: the backlight service (port of iOSPlus10's `AppleARMBacklight`), the light readings; the HID light sensor device, built into the hosted kernel's HID archive | `runtime/bridges/brightness`: the backlight sysfs node, `als_rear` by evdev, `brightctl` | both built; host tested; in a PC boot the HID device starts and Apple's `AppleUSBALSService` attaches to it; on the phone backboardd finds the service; the slider reached no request because CoreBrightness ran VMBLControl (fixed 2026-11-12 in backboardd through the Metal plug-in, not yet run; section 7a) |
| Motion sensors and the front light (2026-11-25; on by default since the merge into main: xnuhost `sensors=auto`, `sensors=off` turns it off; the motion half was off in v0.1.30: its kernel class `AppleSPUHIDDevice` clashed with `com.apple.driver.AppleSPU`'s and v0.1.29 panicked at kernel start on the S24 Ultra; on again on fix/motion-spu-clash, where `prelink roots` leaves AppleSPU and the kexts that link it out of both collections and xnuhost gives the motion device only to a collection without it; the front light sensor stays either way) | the HID archive: UniXNU-droidvm's `AppleSPUHIDDevice`s (accelerometer, gyroscope, orientation) and a compass (`oneios-motion=MASK`); the light sensor as an iPhone's SPU ALS (`oneios-als-spu=1`) | `runtime/bridges/sensors`: the sensor hub (SSC) over QRTR (QMI 400, ported from ios-redfin), xnuhost's motion device `/arm-io/input-N`; `sensorctl`, `oneios-tool phone qrtr` | host-tested and built. On the S24 Ultra (v0.1.29, 2026-11-25 05:05) the hub answered a root process (`sensors: on: ssc (sensors=auto): service 400 ...`, `sensors: streaming: accelerometer, gyroscope, magnetometer, light`), then the kernel panicked on the AppleSPU clash; the fix (fix/motion-spu-clash) is not booted (section 7i) |
| Battery | `drivers/battery`: an `IOPMPowerSource` powerd reads | `runtime/bridges/battery`: Android's `power_supply` nodes, polled, changes sent over the bridge channel; `batteryctl` | both built; host tested with a fake sysfs; one phone session (2026-11-12 23:45, on the charger): published, and a "plugged in" sound before the lock screen (the startup chime: section 7c) |
| Thermal | `drivers/thermal`: `OneiOSThermal` (the readings and the level in the registry); `userland/helpers/oneios-thermal` sets iOS's `com.apple.system.thermalpressurelevel` from it | `runtime/bridges/thermal`: the thermal zones, Samsung's thermistors, the battery's sensors; a level on iOS's scale; xnuhost's CPU step when hot; `thermalctl` | host built and tested; the bridge and `thermalctl` run on the phone (2026-11-12); the step, the driver and the helper not yet run; an idle session's heat traced to the guest's idle cache flush (xnuhost's `setway-skip`: -40% CPU) (section 7d) |
| Hardware backends (2026-11-15) | - | `runtime/bridges/hw`, `hal`: `hw-backend=samsung\|treble` (default samsung, unchanged), per bridge `hw-backend-<bridge>`; treble = the AIDL HALs through servicemanager, evdev by capabilities, standard sysfs classes by named paths | treble done for the backlight, battery, thermal sensors and vibrator (host and stand-in phone tests, both backends); touch and keys share one capability lookup; the light sensor, Wi-Fi, Bluetooth, display next; not run on a phone (section 7e, the audit table) |
| Host device (2026-11-21) | - | `runtime/bridges/hw/host_device.rs`: which known phone xnuhost runs on (properties, soc0, the device tree) and its defaults from one table; `host-device=auto\|none\|NAME` (section 7h) | host-tested; not run on a phone |
| Touch screen and buttons | `kernel/patches/oneios-hosted/hid`: UniXNU's NativeHidInput (digitizer, buttons) under the collection's `IOHIDDevice`, UniXNU-droidvm's virtio-input transport with the side keys | xnuhost's `input.rs` (evdev, no grab) and `virtio_input.rs`; `input_script.rs` for a host with no panel | built; in a PC boot backboardd opens both services and scripted touches and keys are queued for it as HID events; never run on the phone (section 7b) |
| S Pen (roottweak writes `pen=auto` for non-20F66 images; xnuhost `pen=auto`, guest `oneios-pen=1`) | the HID archive's third NativeHidInput, Digitizer / Stylus (`staged/hid_pen.rs`): backboardd's pencil digitizer; the transport's pen code (UniXNU-droidvm, unchanged) | xnuhost's `pen.rs`: one replaceable lookup by bits (`pen::lookup`), evdev read only, no grab, a second virtio-input device `/arm-io/input-1` | built; host-tested; PC boot with the demo script's pen: backboardd "*** acquired pencil digitizer", its touchstats count the strokes as pencil touches (section 7f); first phone session 2026-11-22: device found, the pen service published and opened by backboardd, no pen event yet (the pen IC asleep [I]: the brightness bridge now wakes it, `phone pen wake` by hand; section 7f) |

Common pieces:

- **The bridge channel** (`drivers/bridge/oneios_bridge_proto.h`): one virtio-mmio device, `/arm-io/bridge`, compatible
  `oneios,bridge`, carrying small messages (haptic requests, HCI packets). Guest end `drivers/bridge/OneiOSBridge.c`,
  host end `runtime/bridges/devices/bridge_dev.rs`. Devices that already have a virtio driver in UniXNU-droidvm
  (network, sound) use standard virtio devices instead.
- **Tests**: `cd runtime && cargo test -p onebridges` (part of `runtime/tests/host.sh`, which CI runs; the C suite had 198
  checks, and 48 in `brightness/`, 65 in `battery/`, with ASan and UBSan: every one is a Rust test now), the virtio devices driven
  through xnuhost's real transport (`xh_core::virtio`, `runtime/core/src/virtio.rs`) by a guest made of plain memory; the Android
  build is `runtime/build.sh STAGE`. `oneios-tool kernel drivers --check` compiles the kernel sources with the kernel build's own compile records
  (xnu-12377, arm64e, `-Wall -Wextra -Werror`); the archive's 292 undefined symbols all resolve in `xnu.macho`.
- **On the phone**: `bridgectl` (static; `bin/bridgectl` of `runtime/build.sh STAGE`) tries each bridge without xnuhost:
  `bridgectl cpu|haptic|haptic-pattern|bt|net|audio`. It is the first thing to run when the phone is attached.

### 0.1 Baseline on the phone (2026-11-12, bringup)

Measured beside real sessions on the S24 Ultra (SM-S928B, One UI's kernel, KernelSU), with
`scripts/phone/survey.sh [session]` and the session's `xnuhost.log`. Raw output: `out/phone-baseline/2026-11-12/`
(git-ignored: `oneui-idle-before.txt`, `oneui-after-session3.txt`, `session4-survey.txt`, `session5-survey.txt`,
`session1..5-xnuhost.log`). Read only: nothing was changed that is not put back. The phone was on USB, battery
100 % (full). Kernel for sessions 3-5: the Linux-built `kernel.kc` 9476a867 of 2026-11-11 (no 0035-0037); the Mac
`kernel.kc` panics in the first 2 s (`kernel/prelink/README.md`, "On the phone").

| | One UI idle, before | Session 4: runtime 0.1.7-dev, lock screen, about 6 min after ready | Session 5: runtime 0.1.6, owner unlocked to the Home Screen | One UI, 1 min after session 3 |
|---|---|---|---|---|
| cpuss-0 / gpuss-0 / ddr / xo-therm | 29.3 / 26.8 / 27.7 / - C | 60.6 / 50.6 / 53.2 / 43.5 C | 51.7 / 45.5 / 47.7 / 41.0 C | 64.5 / 54.5 / 56.7 / 44.8 C |
| battery (thermal zone `battery`, `power_supply/battery/temp`) | 23.6 C | 36.8 C | 35.7 C | 34.9 C |
| CPU clocks (cur, kHz, cpu0-1 / 2-4 / 5-6 / 7) | 1804800 / 614400-3148800 / 1190400 / 672000 (walt) | every core at its top: 2265600 / 3148800 / 2956800 / 3340800; load 6.6 | **capped**: scaling_max 1689600 / 2035200 / 1920000 / 2035200, every core at it; load 8.0 (27.5 over 5 min) | |
| GPU (`kgsl-3d0` gpuclk, devfreq) | 231 MHz, msm-adreno-tz, 0 % busy | **1000 MHz, 0 % busy** (drm2kgsl `context 5: power constraint at the highest level`) | 231 MHz (`max_gpuclk` 500 MHz per the coordinator's read) | 231 MHz |
| render server frames per 10 s (`oneios-gpu-report`) | | 0-9, mean 8-24 ms | 0-20, mean 22-69 ms | |
| swaps (`oneios-display-report`, gap between swaps) | | 124 swaps, median 0.76 s, max 60 s | 174 swaps, median 0.17 s, max 77 s | |

On 2026-11-11 (runtime 0.1.6 with the 12:49 xnuhost, the same `kernel.kc`, Data store and seed) the same report
showed 224-544 frames per 10 s once the lock screen was up. So on 2026-11-12 the GPU path drew a frame only every few
seconds in both runtimes; the cause is not known (passed to agents gpu and thermal). Session 5's caps on the CPU
were in place after four sessions in 40 minutes: the phone was hot (cpuss-0 64.5 C one minute after session 3).

Per area, in the sessions (lines of `xnuhost.log`):

- **CPU**: `bridgectl cpu`: 8 CPUs, 2 E (cpu0-1, capacity 369, 364800..2265600 kHz) and 6 P (cpu2-4 capacity 897 to
  3148800 kHz, cpu5-6 capacity 842 to 2956800 kHz, cpu7 capacity 1024 to 3398400 kHz); plan 2 E then 6 P vCPUs; its
  thread placed and `uclamp.min 1024` set (both done). xnuhost: `cpu: 2 E + 6 P vCPUs on the host's 2 E + 6 P cores`;
  every xnuhost thread `SCHED_OTHER` (policy 0), nice 0, `oom_score_adj` 500, its own cgroup.
  No `oneios-cpu` heat lines: that helper is in no seed yet.
- **GPU**: `gpuctl nodes`: `/dev/kgsl-3d0`, `/dev/udmabuf`, `/dev/dma_heap/system` open; udmabuf `list_limit`
  1024, `size_limit_mb` 64. Metal plug-in on (`oneios-gpu-plugin`, five GPU clients). Frame numbers above.
- **Touch**: delivered, none dropped: session 4 `6165 virtio-input events, 1387 frames, 1463 HID reports, 39
  contacts`, session 5 `2751 events, 757 HID reports, 7 contacts`, `0 dropped` in both. Runtime 0.1.7-dev wakes the
  touch IC (`brightness: touch: 1 IC under /sys/class/sec (/sys/class/sec/tsp/enabled) woken`); 0.1.6 does not
  (the coordinator wrote `2,0` and `2,1` by hand in session 5). The buttons reached iOS (`buttons: 6 key events`),
  but the Volume Up + Volume Down return chord did not return in sessions 3-5; xnuhost holds `/dev/input/event0`
  and `event2` (the two volume-key devices) open (agent touch).
- **Audio**: `audio: on: helper /data/runtime/bin/oneios-audio`; no sound was played or checked.
- **Haptics**: `haptics: on: /dev/input/event5 (sine effects, period 4..10 ms, level 40..255)`; iOS's `ONEIOS HAPTIC
  UI: off: no Actuator HAL plug-in in the root` (ringtones and alerts only). A kernel from 2026-11-12 (media) on has the
  Actuator audio device (section 5).
- **Bluetooth**: 0.1.7-dev: `bluetooth: on: the phone's controller through its HAL 1.1, address 02:d7:be:af:18:ea
  (made up: /mnt/vendor/efs/bluetooth/bt_addr not read)`; no line shows bluetoothd opening it. 0.1.6: `ONEIOS BT: off:
  the host serves no Bluetooth controller`.
- **Wi-Fi / network**: every session: `xnuhost E network: oneios-net.sh up ended with status 256: the guest has no
  route out` (Wi-Fi was on in One UI before the switch).
- **Battery**: `ONEIOS BATTERY: 100%, on power, full (284/10 C, 4311 mV, 12 mA)`.

Not run, and why: `bridgectl bt`, `net`, `haptic` and `audio` (during a session xnuhost holds the Bluetooth HAL, the
tap, the vibrator and the audio helper; beside One UI, `bt` needs Bluetooth off, which the owner had on).

## 1. CPU

### The phone [F]

| Linux CPU | Core | `capacity-dmips-mhz` | Cluster (`cpu-map`) | Frequency domain |
|---|---|---|---|---|
| cpu0, cpu1 | Cortex-A520 | 1024 | cluster0 | 0 |
| cpu2, cpu3, cpu4 | Cortex-A720 | 1792 | cluster1 | 3 |
| cpu5, cpu6 | Cortex-A720 | 1792 | cluster2 | 1 |
| cpu7 | Cortex-X4 | 1894 | cluster3 | 2 |

Source: `qcom/pineapple-v2.dts` lines 419-660 in
[samsung-sm8650/android_kernel_samsung_sm8650-devicetrees@2f6a973](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650-devicetrees/blob/2f6a9734e556f2f7680b0094bec4ad4671157f8b/qcom/pineapple-v2.dts)
(decompiled stock DTBs). The core names are the SoC's (SM8650: 1 X4, 5 A720, 2 A520); the DT says `qcom,kryo`. All
eight CPUs have `paca` and `pacg` [F, owner's phone]. Top frequencies are not in the DT (they come from the
`qcom,cpufreq-epss` tables at run time): read them with `bridgectl cpu` [U].

### iOS side

- XNU reads `/cpus/cpuN` `cluster-type` (`'E'`, `'P'`) in `ml_parse_cpu_topology`
  (`osfmk/arm64/machine_routines.c`). With `__ARM_AMP__` it makes one processor set per cluster type, runs the Edge
  scheduler, and `hw.nperflevels` / `hw.perflevel0.*` (P) / `hw.perflevel1.*` (E) follow. Without it the kernel is
  SMP: one performance level, whatever the tree says. `__ARM_AMP__` for this machine configuration is UniXNU-droidvm's
  `ipados27/patches/xnu-12377/droidvm-amp.patch` (19 lines; it applies on today's series tree, checked with
  `git apply --check`). The kernel series leaves it out today (`kernel/profiles/iphone18-2-ios27.md`).
- Without CLPC every thread group would stay on the boot CPU's cluster. `DroidVMCpufreq.c` (UniXNU-droidvm,
  unchanged; it compiles with our records) registers a performance-control callout that gives each thread group its
  preferred cluster (UI, above-UI and real-time classes on P) and writes the frequency a vCPU wants into a per-vCPU
  register window, `/arm-io/cpufreq` (Linux's virtual-cpufreq layout).
- The Edge scheduler's own edges allow no move from E to P (`sched_edge_cpu_init_completed`: E -> P `no_spill`), and
  without CLPC nothing changes them: until 2026-11-22 every thread outside those three classes stayed on the two E
  vCPUs (0% idle on the phone) while the six P vCPUs were 85-99% idle. Hosted `0085` opens E -> P migration and steal
  once all CPUs are up: weight 0 for fixed-priority to default, 64 for utility, 1000 for background (us of
  scheduling latency); `oneios-edge-spill=0|1|2`, one `oneios-edge:` line (kernel/patches/oneios-hosted/README.md,
  "0085"). It runs at the end of the scheduler's completion step (`sched_cpu_init_completed`), which on this board
  nothing called until hosted `0089` (2026-11-23: the last `ml_processor_register` starts it; `oneios-smp:` lines).
- `l2-cache-size`, `l3-cache-size` of a cpu node are read by XNU when present.

### The bridge (`runtime/bridges/cpu/topology.rs`)

- `Topology::read("/sys")`: per CPU `cpu_capacity`, `cpufreq/cpuinfo_{min,max}_freq`,
  `scaling_available_frequencies`, `related_cpus`, `regs/identification/midr_el1`, `cache/index*`. CPUs of the lowest
  capacity are E when capacities differ: on this phone cpu0-1 are E, cpu2-7 P.
- `CpuPlan::make(&t, nvcpu, pin)`: the guest's vCPUs, **E first** (an iPhone's boot CPU is an E core), then P.
  8 vCPUs: 2 E + 6 P. Fewer: one E per four vCPUs, at least one. Each vCPU carries `type`, `cluster_id` (0 E, 1 P),
  the host CPUs it may use (`affinity`: all cores of its kind; with `pin` one core each, the X4 first), the frequency
  table and cache sizes of the core cpu-pin gives it (the rank-th of its kind, vCPU *i* on CPU *i* here; before
  2026-11-15 the fastest core in the mask), the boost frequencies included (`scaling_boost_frequencies`).
- `cpu::set_affinity(tid, mask)` with the plan's mask (`CpuPlan::pin_mask` with cpu-pin): `sched_setaffinity`. For the vCPU thread, and for the Linux thread of the iOS
  thread a vCPU runs (iOS threads are threads of other processes; booter's `thread_run`, `runtime/xnuhost/src/task.rs`).
- `Cpufreq` (an `xh_core::mmio::MmioDevice`): the register window. A write of a frequency sets the utilisation clamp
  (`sched_setattr`, `SCHED_FLAG_UTIL_CLAMP_MIN`, capacity x kHz / max kHz) of the vCPU's thread; the lowest table
  entry drops the request. Linux's governor keeps clock control. `Cpufreq::requested(vcpu)` lets booter apply
  the same clamp to the iOS thread it runs (`CpuPlan::request`, `CpuPlan::util_for`). xnuhost makes it with
  `Cpufreq::with_apply` (2026-11-23): the device records the request and hands its clamp to xnuhost, which keeps each
  vCPU thread's clamp itself (`docs/booter.md` 2.6e).

### Missing

- release-kernel: `droidvm-amp.patch` in the series, `DroidVMCpufreq.c` in the board archive.
- booter: `cpu_cluster[]` and the cache properties of the cpu nodes from the plan; the `/arm-io/cpufreq` node
  (`reg` = nvcpu x 0x1000) and its MMIO region on the `Cpufreq` device; the placement on its threads. [Done since: xnuhost's
  `plan_cpus` in `runtime/xnuhost/src/session/bridges.rs` does all three.]
- [U] whether `uclamp` is writable from KernelSU's domain on Samsung's kernel (`CONFIG_UCLAMP_TASK`); `bridgectl cpu`
  says.
- [I] `hw.perflevel*` cache sizes come from the kernel reading `CCSIDR_EL1` on the running CPU, which under xnuhost
  is the host core the vCPU thread happens to be on: correct per kind as long as placement holds.

## 2. GPU

Not owned here. The references agent found the working path on this SoC (`docs/references.md` section 1): the guest's
NativeMetal plug-in, metal2vulkan and Turnip over virtio-gpu's DRM native context, served on the host by
virglrenderer's `drm2kgsl` on `/dev/kgsl-3d0`. That is the 3D half of booter's `runtime/xnuhost/src/virtio_gpu.rs`;
the KGSL proxy (NativeAdreno's own user client) was retired upstream and our metal2vulkan/reims-vgpu path is the
fallback only. Facts for it from the phone [F]: Adreno 750 v2 (`Adreno750v2`, 1.0 GHz top), `/dev/kgsl-3d0`
(crw-rw-rw-, `msm_kgsl`), `/dev/udmabuf` present (`CONFIG_UDMABUF=y`), `/dev/dma_heap/system` and the qcom heaps
present. Offer on the board: this agent takes the host GPU bridge only if booter asks.

GPU memory on this phone (2026-11-11, `gpuctl` of runtime 0.1.3 as root; `docs/gpu.md` section 4.1 has the run):

- [F] **udmabuf limits**: `/sys/module/udmabuf/parameters/size_limit_mb` **64**, `list_limit` **1024** (the kernel's
  defaults; built in, parameters mode 0644). One udmabuf is at most 64 MiB and 1024 runs of adjacent pages, so
  xnuhost splits a larger or more scattered guest buffer into several (`docs/gpu.md` section 2).
- [F] **KGSL**: chip `0x43051400`, gpu id 6770, gmem 3072 KiB, msm protocol 1.12 through drm2kgsl, one priority;
  `KGSL_MEMFLAGS_VBO` works. A 64-bit process's GPU addresses are `0x4000000000`..`0x8000000000` (256 GiB,
  `kgsl_iommu.h` of [the modules repo@3e11ddc](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650-modules/tree/3e11ddcb2b63ce5af65e229e4f340ba6221bddee/qcom/opensource/graphics-kernel)),
  and a VBO of all 256 GiB is granted.
- [F] `IOCTL_KGSL_GPUOBJ_IMPORT` of a 16 KiB udmabuf returned ENOMEM beside that 256 GiB VBO; `dmesg` showed only
  `kgsl kgsl-3d0: Skipped access check` (the mem-buf permission check passing for a foreign exporter).
  [I] The cause is the VBO filling the address range, not udmabuf; drm2kgsl now leaves room. [U] until `gpuctl`
  runs again: `gpuctl routes` tests the udmabuf, `system` heap, `qcom,system` heap and `GPUOBJ_ALLOC` routes.
- [F] dma heaps present: `system`, `qcom,system`, `qcom,display`, `qcom,camera`, `qcom,secure-*`, `qcom,sp-hlos`,
  `qcom,qseecom`.

## 3. Wi-Fi and network

**Switches (2026-11-23, docs/switches.md):** `wifi-ring-wait=0` (a frame for iOS on a full ring is dropped at once),
`wifi-send-wait=0` (EAGAIN fails the frame), `wifi-buffers=0` (the kernel's socket buffers), `wifi-joined-scan=low|plain`
(no cached answer; plain: no scan flags) and the kernel's `oneios-wifi-rssi=0` (no RSSI_CHANGED) each turn one change of
cbf97ea or b704922 back. The defaults are those changes.

Design of 2026-11-12 (owner): **no NAT**. Wi-Fi works the way a Treble GSI gets it: in a session iOS is the only OS
that uses the radio, and it drives the vendor's own Wi-Fi stack. The tap, the private subnet and `oneios-net.sh` are
the old route; xnuhost runs them only with `wifi=tap`.

### The phone [F, read on the owner's phone 2026-11-12 unless marked]

- Chip: Qualcomm WCN785x / FastConnect 7800 ("kiwi"): DT node `compatible = "qcom,cnss-kiwi"`, `supported-ids =
  <0x1107>` (`samsung/e3q_eur_openx_w00_r09.dts` line 17994); modules `qca_cld3_kiwi_v2` + `cnss2` (`ethtool -i
  wlan0`: driver `cnss_pci`); firmware `vendor/firmware/kiwi/` ([android_device_samsung_e3q@fb2ba30](https://github.com/samsung-sm8650/android_device_samsung_e3q/blob/fb2ba3010d59c0f38ddf81aef7fd9c78e2172860/proprietary-files.txt)).
- Android 17 (SDK 37) with the vendor image's HALs. `wlan0`: ifindex 44, MTU 1500. Offloads (`ethtool -k`): GRO on
  (can be changed), TSO and TX scatter-gather on and fixed, RX checksum on and fixed, LRO off and fixed. While One UI
  is not joined to a network, `disable_ipv6` is 1 on wlan0 and the interface has no address. **wlan0's address is a
  random one while One UI is not joined** (`06:e3:b9:7d:95:16` at 09:30, `8e:f6:ae:01:7e:c9` after a return restarted
  the framework): the address iOS's en0 gets is the one wlan0 has at the switch.
- **HALs (all AIDL, none HIDL)**, from `service list`, `lshal` and `/vendor/etc/vintf/manifest/`:

  | Service | Interface, version | Process (domain) |
  |---|---|---|
  | `android.hardware.wifi.IWifi/default` | AIDL `android.hardware.wifi` V1 (no version tag in the manifest; `android.hardware.wifi-V1-ndk.so`) | `android.hardware.wifi-service` (`hal_wifi_default`) |
  | `android.hardware.wifi.supplicant.ISupplicant/default` | AIDL `android.hardware.wifi.supplicant` **V2**: `getInterfaceHash` answers `5e5c6d9e5565999f73fa7f797f5b94616174745b`, the hash of AOSP's frozen `aidl_api/.../2` | `/vendor/bin/hw/wpa_supplicant` (init service `wpa_supplicant`, `disabled oneshot`, `interface aidl`: started on the first lookup; uid wifi) |
  | `vendor.samsung.hardware.wifi.ISehWifi/default`, `vendor.samsung.hardware.wifi.nan.IWifiVendorNan/default` | Samsung's AIDL extensions (`vendor.samsung.hardware.wifi-V1-ndk.so`, `...wifi.supplicant-V1-ndk.so`) | `vendor.samsung.hardware.wifi-service` |
  | `wifinl80211` (`android.net.wifi.nl80211.IWificond`) | the platform's nl80211 client (scans, station info) | `wificond` |

  The framework scans through wificond (nl80211), not through the supplicant: ISupplicant has no scan call.
- The supplicant's **control socket is closed to root** on this user build: `/data/vendor/wifi/wpa/sockets/wlan0`
  and `@android:wpa_wlan0` exist, but every `wpa_cli` command from KernelSU's `ksu` domain (PING, STATUS,
  LIST_NETWORKS, SCAN_RESULTS) timed out after 10 s; the client socket was created with the right owner (wifi:wifi,
  0660). Cause [I]: AOSP lets the supplicant send to a client socket only of the `su` domain and only on userdebug
  and eng builds (`hal_wifi_supplicant.te`, `userdebug_or_eng(unix_socket_send(hal_wifi_supplicant, wpa, su))`), so
  its replies to `ksu` are refused. Not used.
- The supplicant's **AIDL service answers root**, and so does nl80211 (both run with our own client on 2026-11-12,
  One UI running, SELinux Enforcing, no policy change; "On the phone" below).
- No SIM in the phone on 2026-11-12 (`gsm.sim.state` ABSENT), mobile data off. One UI's Wi-Fi was on and joined to no
  network (its last network, a phone hotspot, left on 2026-11-11 15:36).

### The design: iOS drives the vendor stack

```
 iOS: wifid, Settings, configd (DHCP, ARP, IPv6, DNS)
   |  Apple80211 ioctls (SIOCGA80211/SIOCSA80211) on en0, IEEE 802.11 kernel events
 guest kernel: OneiOSWifi.cpp: IO80211Controller/IO80211Interface (the classes wifid knows) under the collection's
               IOEthernetController/IOEthernetInterface, a station that asks the host, UniXNU-droidvm's virtio-net transport
   |  one virtio-net device (/arm-io/net): frames, and the station's messages as frames of their own type
 xnuhost: runtime/bridges/wifi
   joins: ISupplicant V2 (AIDL over /dev/binder): getStaInterface(wlan0), addNetwork, set*, select, disconnect,
          removeNetwork; one callback object for state changes, disconnects, rejections
   scans and signal: nl80211 on wlan0 (TRIGGER_SCAN, GET_SCAN, GET_STATION, GET_INTERFACE), as wificond does
   frames: AF_PACKET on wlan0 (ETH_P_ALL, PACKET_IGNORE_OUTGOING), the guest's MAC = wlan0's
 One UI's kernel: cfg80211 + qca_cld3, the vendor's wpa_supplicant (keys, 4-way handshake, rekeys)
```

1. **Control.** The supplicant keeps doing what only it can do on this chip (WPA2/WPA3 handshakes, PMF, rekeys,
   roaming inside an ESS). The bridge is its only client in a session, as the framework is in One UI:
   - join: `addNetwork`, then on the network `setSsid`, `setKeyMgmt`, `setProto`, `setPairwiseCipher`,
     `setGroupCipher`, the secret, `select`. What to set comes from the key iOS gave and from the network's RSN and
     WPA elements in the last scan (`supplicant::plan_join`):

     | The network offers | iOS gave | Supplicant network |
     |---|---|---|
     | no RSN or WPA element | no key | `NONE` |
     | OWE only | no key | `OWE`, PMF required |
     | PSK (also beside SAE: a transition network) | a passphrase | `WPA_PSK | WPA_PSK_SHA256`, `setPskPassphrase`; WPA and TKIP allowed too when the network has a WPA element or TKIP |
     | PSK | a 32-byte key (iOS's own PBKDF2 of the passphrase) | the same with `setPsk` |
     | SAE only | a passphrase | `SAE`, `setSaePassword`, PMF required, `setSaeH2eMode(optional)` |
     | SAE only | a 32-byte key | refused before the supplicant is asked: WPA3 needs the password itself |
     | not in the last scan | either | as PSK or open, with `setScanSsid(true)` (a probe for the name) |

     The outcome comes from the callback: `onStateChanged` or `onSupplicantStateChanged` to COMPLETED is the join;
     `onAssociationRejected` (not timed out) fails it with the access point's status; a disconnect or an
     authentication timeout after the 4-way handshake began, with a key, is a refused key; two `onNetworkNotFound`
     are a network that is not there; nothing in 25 s is a timeout. After any failure the network is removed, so the
     supplicant does not try again by itself.
   - leave: `disconnect`, `removeNetwork`.
   - the link drops (`onDisconnected` while joined): the network is removed and iOS is told (`OW_STATE`, link down
     with the 802.11 reason). **iOS decides when to join again**, not the supplicant.
   - scan: nl80211 `TRIGGER_SCAN` on wlan0 with one wildcard SSID (an active scan, as wificond's; while joined a
     low-priority one: section "After the join") and `GET_SCAN`
     after `NEW_SCAN_RESULTS` (or after 8 s without the event): BSSID, frequency, signal (mBm), capability, beacon
     interval, and the information elements as they are, cut to whole elements within the 1024 bytes of iOS's scan
     record. A busy radio (`EBUSY`: the supplicant's own scan) is not an error: its results are taken.
   - signal: nl80211 `GET_STATION` (signal, transmit rate) and `GET_INTERFACE` (the channel), every 2 s while joined.
   - iOS keeps its own known networks (wifid's store in the Data volume); the supplicant holds only the one network
     iOS is joining or joined to.
   - At the start of a session the bridge disconnects the supplicant and removes the networks in it, so the
     supplicant does not rejoin One UI's network on its own; One UI's framework adds its networks again when it
     starts (it does that after every framework restart).
   - The supplicant is a lazy service: when it is not running (its client, One UI's framework, is gone) the
     lookup starts it (waited for up to 8 s), and it then has no station interface: the bridge adds `wlan0`
     (`addStaInterface`) and removes it again at the stop (`removeInterface`). When it is running with `wlan0`, the
     bridge takes that interface and leaves it at the stop.
2. **Data.** One IP stack on the link: iOS's. The bridge opens `AF_PACKET`/`SOCK_RAW`/`ETH_P_ALL` bound to wlan0,
   with `PACKET_IGNORE_OUTGOING` and `PACKET_VNET_HDR`. The guest's en0 has wlan0's MAC (a station can send only
   with its own address; iOS's private addresses are refused by the driver: `setHardwareAddress` is not supported).
   Frames from the guest with that source address go out as they are; frames to wlan0's MAC, broadcast and
   multicast come in (the socket asks for every multicast group, `PACKET_MR_ALLMULTI`: iOS's IPv6 solicited-node group and
   mDNS are groups Linux has not joined on wlan0; the membership ends with the socket). EAPOL (0x888e) is not
   passed either way: it is the supplicant's. GRO is turned off on wlan0
   for the session (`ETHTOOL_SGRO`); a coalesced TCP frame that still arrives (its `virtio_net_hdr` says GSO or
   "needs checksum") is cut back into MTU-sized segments with fresh IPv4/TCP checksums. The Linux side keeps quiet
   on wlan0: its IPv4 addresses are removed (`ip -4 addr flush dev wlan0`) and `disable_ipv6` is 1. `ip_forward`
   stays as it is (0 on the phone: a frame for iOS's address is dropped by Linux, not forwarded or answered), and
   with no address on wlan0 Linux answers no ARP request for iOS. netd, which keeps running, is not asked for
   anything.
3. **Return.** xnuhost's stop (`wifi::wlan::close`): `disconnect` and `removeNetwork` for its network, `removeInterface`
   if it added `wlan0`, the binder closed (the supplicant drops a dead client's callback), then GRO and
   `disable_ipv6` back to the values in `/data/session/wifi.state` (written before anything was changed), wlan0 left
   with no address. One UI's framework then sets wlan0 up itself when it starts: IpClient enables IPv6 and runs
   DHCP, the framework adds its saved networks and joins (to be checked on the phone: "One UI's Wi-Fi joins again by
   itself").
   **After a SIGKILL** of xnuhost nothing of xnuhost runs, and what is left is: GRO off and `disable_ipv6` 1 on
   wlan0, and the supplicant with iOS's network in it (unless it ended with its client [I: AOSP's AIDL supplicant
   ends itself when a client that registered a callback dies; not seen on this phone yet]). Neither setting breaks
   One UI's Wi-Fi: `disable_ipv6` 1 is how the framework itself leaves wlan0 between networks (IpClient turns IPv6 on
   when it joins), GRO off only costs receive speed, and the framework removes the supplicant's networks before it
   joins one. That is why the bridge does **not** set `arp_ignore` (the first design did): left at 8 after a kill it
   would stop One UI answering ARP on wlan0. `bin/wifictl restore /data/local/tmp/oneios/session/wifi.state` puts
   both settings back; the fork's return can run it after its SIGKILL step (`oneos/interface.md` section 6; not in
   the fork yet).
4. **Wi-Fi off in One UI at the switch** [U]: then the chip is stopped and wlan0 does not exist. The bridge says
   `wifi: off: no wlan0 (One UI's Wi-Fi was off at the switch?)` and iOS gets no network device. Starting the chip
   itself (IWifi `start`, `createStaIface`) is not written.

### The guest's side (`kernel/patches/oneios-hosted/net`, in the hosted kernel since 2026-11-12)

Before 2026-11-12 the hosted kernel had **no network driver at all** (UniXNU-droidvm's network sources are built only
by its own script), so iOS had no en0 in any boot. Now `build-hosted.sh` builds one more archive,
`liboneiosnet.a` (`kernel/patches/oneios-hosted/net/README.md`):

- `OneiOSWifi.cpp`: `IO80211Controller` and `IO80211Interface` (the class names configd and wifid know a Wi-Fi
  interface by; Apple's IO80211Family is not in the collection) as subclasses of **the collection's own**
  `IOEthernetController` and `IOEthernetInterface`. Apple's `com.apple.iokit.IONetworkingFamily` is a root of the
  collection and 25 of the 130 roots link it, so it cannot be replaced by a copy compiled into the kernel (what
  Droid-VM does). The classes are compiled against IONetworkingFamily-188's headers (APSL; fetched into
  `out/sources/IONetworkingFamily`), and the build checks those headers against the kernelcache's classes
  (`IONetworking-24A446-v54.json`, made by `oneios-tool kernel layout net`): 250 and 221 vtable slots, every slot's
  pointer-authentication discriminator, and whether its function is the family's or the kernel's. 159 original
  functions are bound at their addresses in the collection through `.original` stubs (`prelink stubs-check`: good).
- The Apple80211 requests and events of DroidVMWifi.inc (below), with its made-up station replaced: `SCAN_REQ` sends
  `OW_SCAN` and the records come back as `OW_BSS`; `ASSOCIATE` sends `OW_JOIN` and `ASSOCIATE_RESULT` follows
  `OW_JOIN_RESULT`; `DISASSOCIATE` sends `OW_LEAVE`; `OW_STATE` carries the link, the signal, the rate and the joined
  network's record. The link status configd's DHCP waits for (`setLinkStatus`) is active only while joined. Frames
  pass only while joined.
- The messages (`drivers/net/oneios_wifi_proto.h`) travel on the virtio-net queues as Ethernet frames of type 0x88b5
  (IEEE 802 local experimental) to and from the address `02:4f:57:49:46:49`: the bridge never sends such a frame to
  the radio, the driver never gives one to iOS's network stack.
- `DroidVMVirtioNet.c` (UniXNU-droidvm f9121cd, the transport), staged with one change: its receive filter passes
  multicast too (IPv6 neighbour discovery, mDNS).
- Boot argument `oneios-wifi=0` leaves it out. Log lines: `ONEIOS WIFI: classes registered below the collection's
  IOEthernetController and IOEthernetInterface`, `ONEIOS WIFI: on: the phone's Wi-Fi, address ...`, `the host has
  the phone's radio`, `wifid gets|sets N: ...` (each request for the first 150, then a type's first), `scan done: N
  networks`, `join: ...`, `join done: joined` or `join failed`.

### What wifid needs from the interface (Apple80211)

From DroidVMWifi.inc (UniXNU-droidvm f9121cd, written for iPadOS 27 24A446 and shown in Settings by Droid-VM on
2026-11-02 with `droidvm-wifi-ssid=`), UniXNU-sheng's `package/WifiNet.cpp` (6bc0ce1: a driver that joins real
networks on 24A446, so its join details are the ones used) and iOSPlus10's 20H392 trace (`patches/xnu/README-wifi.md`,
"How wifid uses Apple80211"; iOS 16, so what only it says is [I] until seen):

- **IO80211.framework lists an interface as Wi-Fi by its media** [F, 24A446's `__getIfListCopy`, disassembled from
  the image's shared cache]: it sends `SIOCGIFMEDIA` and takes the interface when the current medium's type is IEEE
  802.11 (`(ifm_current & 0xe0) == 0x80`); only when that request fails does it ask `SIOCGIFTYPE` for `IFT_ETHER`
  with `IFNET_SUBFAMILY_WIFI`. IONetworkingFamily answers `SIOCGIFMEDIA` from the controller's selected and active
  medium, and `IFM_NONE` when neither is set. The driver set a medium only while joined (as DroidVMWifi.inc, which
  Droid-VM showed in Settings with a joined made-up network), so on the phone on 2026-11-12 wifid never took en0:
  no scan request in the whole session, Settings' Wi-Fi page searching for ever. Fixed: the controller selects its
  IEEE 802.11 medium at the start and keeps it the active medium while the link is down.
- Found as Wi-Fi only if: the controller's class is `IO80211Controller` (configd), the interface is `IFT_ETHER` with
  `IFNET_SUBFAMILY_WIFI` (IO80211.framework), it has the property `IOInterfaceName` (wifid matches on it), and the
  device tree has `/arm-io/wlan` (wifid's launchd plist has `_LimitLoadToDeviceTree=wlan`; MobileGestalt's `wifi`
  reads it too; booter's tree has the node). A refused `APPLE80211_IOC_FILTER_MONITOR_EVENTS` makes wifid give up
  the device: accept it.
- The request numbers' names on 24A446 are in IO80211.framework (`_convertApple80211IOCTLToString`'s table of 607
  entries). Seen from clients that are not wifid's own attach: 96 VIRTUAL_IF_ROLE and 97 VIRTUAL_IF_PARENT (refused:
  no virtual interfaces), 560 PROTOCOL_CLEANUP_COMPLETE (a set of 4 bytes, accepted), 103 CURRENT_NETWORK asked with
  a 2280-byte buffer.
- **The scan record is 2280 bytes on 24A446** [F, IO80211.framework's `__addScanResultToList`]: the 1164-byte
  record's fields at the same offsets (channel flags 0x0c, RSSI 0x16, capability 0x1a, rate count 0x22, SSID length
  0x60, SSID 0x61, flags 0x88, element length 0x8a, elements from 0x8c), a longer element area, and a tail the
  framework also reads (the BSS's TIMESTAMP at 0x8d0, 64-bit ns of the continuous clock: "Wi-Fi does not join" below; a flag at 0x8d8, two 32-bit values at 0x8dc and 0x8e0, three bytes at
  0x8e4 to 0x8e6; their meaning is not known). The driver gives a caller whose buffer is that large the whole
  record with a zero tail (SCAN_RESULT and CURRENT_NETWORK); a smaller buffer gets the 1164 bytes.
- **The framework first tries a user client** [F, `Apple80211BindToInterfaceWithService`]: `IOServiceOpen` of type 0
  on the interface's service, and with it every request goes through `IOConnectCallStructMethod`; when the open is
  refused it uses the socket requests (`SIOCGA80211`/`SIOCSA80211`) this driver answers. The family's
  `IONetworkInterface` refuses type 0 [I: IONetworkingFamily-188's source; the driver logs the first eight opens
  and their results].
- Requests (type numbers): 1 SSID, 2 AUTH_TYPE, 4 CHANNEL, 5 POWERSAVE, 8 RATE, 9 BSSID, 10 SCAN_REQ, 11 SCAN_RESULT
  (1164-byte records, 2280 on 24A446; EIO after the last), 12 CARD_CAPABILITIES, 13 STATE, 14 PHY_MODE, 15 OP_MODE, 16 RSSI, 17 NOISE,
  19 POWER, 20 ASSOCIATE (**656 bytes on 24A446**: version 1, mode, 32-bit lower and upper authentication, then the
  network at 0x10: SSID, BSSID, key length, key cipher, key; the older layout is 592 bytes with 16-bit authentication
  fields and the network at 0x0c. The driver takes either by its length and refuses any other. Key cipher 6 is the
  32-byte pairwise master key the daemon derived, 10 the passphrase itself), 21 ASSOCIATE_RESULT (0 while the join
  runs, 1 success, 2 failed, 9 timed out, 21 wrong key; polled every 20 ms for 8 s), 22 DISASSOCIATE, 27/254 SUPPORTED_CHANNELS, 51 COUNTRY_CODE, 86
  SCAN_REQ_MULTIPLE, 103 CURRENT_NETWORK, 106 RSSI_BOUNDS, 207 CHANNELS_INFO.
- Sets: a set of settings or notes is accepted with no effect (383 SET_PROPERTY, 365 DISPLAY_STATE, 564
  FILTER_MONITOR_EVENTS, which Droid-VM saw the daemon give the device up over when refused, 560
  PROTOCOL_CLEANUP_COMPLETE). A set the driver cannot carry out and that names another way of doing a thing is
  **refused**, so the daemon falls back to the plain requests instead of waiting for an answer that never comes: 287
  request numbers of 24A446's table (every WCL_*, AWDL_*, NAN*, P2P_*, VIRTUAL_IF_*, OFFLOAD_*, GAS_*, SOFTAP_* and
  ranging request, the access point modes, ROAM, REASSOCIATE, PRIVATE_MAC and SET_MAC_ADDRESS, RESET_CHIP and a few
  more; `kRefusedSets` in `OneiOSWifi.cpp`). UniXNU-sheng's driver refuses every set it does not know and joins with
  the plain ASSOCIATE (20). [I: which of the two rules wifid needs for each request is not seen yet.]
- A leave while a join runs ends the join for the daemon too (ASSOC_DONE with a failure), as UniXNU-sheng's.
- Events (kernel event class 6, subclass 1, the interface name first): 1 POWER_CHANGED, 2 SSID_CHANGED, 3
  BSSID_CHANGED, 4 LINK_CHANGED (32 bytes: down, RSSI, SNR, noise, reason), 9 ASSOC_DONE (**on 24A446 with 540
  bytes**: ten 52-byte access point records, then the result at 0x214 and a flag that it is valid at 0x218; the
  daemon's `_dispatchAsyncAssocEvent` reads them, so an empty event is not safe), 10 SCAN_DONE, 39 RSSI_CHANGED (the
  592-byte link quality record: section "After the join" below); and the link state on
  the IOKit side (`setLinkStatus`, which configd's DHCP waits for). A join that worked sends, in this order: the
  IOKit link, ASSOC_DONE, SSID_CHANGED, BSSID_CHANGED, LINK_CHANGED, RSSI_CHANGED.
- The regulatory country (51 COUNTRY_CODE) is the Country element of the strongest network of the last scan that
  has one (the host's station finds it and sends it in `OW_STATE`); "XX" until a scan.

### The host bridge (`runtime/bridges/wifi`)

| File | What |
|---|---|
| `frames.rs` | which frames cross (above), and the cut of a coalesced TCP frame |
| `traffic.rs` | the counts of frames and bytes each way, the drops and why, the waits for the send buffer, and the `wifi: 60 s:` line (section "After the join") |
| `uplink.rs` | the guest's frames that find the AF_PACKET socket's send buffer full: they wait in order for `POLLOUT`, 20 ms at most (section "The uplink's full send buffer") |
| `station.rs` | the station the guest's driver talks to: its messages become calls on a radio back end, the radio's reports become messages; while joined, a scan soon after the last one is answered from its networks |
| `wlan.rs` | the back end on the phone: the AF_PACKET socket (a 4 MiB receive buffer, a 1 MiB send buffer; both doubled by the kernel), nl80211 (low-priority scans while joined), wlan0's settings kept and given back. Three threads: frames, nl80211 events, control. The guest's vCPU only sends a frame or queues a message; netlink, binder and the join's timeouts are the control thread's |
| `sim.rs` | a made-up radio (`wifi=sim`) |
| `p2p.rs`, `wonder.rs` | what the radio offers for a peer-to-peer link (interface types and combinations, action frames, NAN, vendor commands, every interface), for `wifictl p2p` (section 4a) |
| `supplicant.rs` | the supplicant's client: the lookup, the interface, the callback object, the join's plan and its outcome |
| `runtime/bridges/hal/aidl.rs`, `binder.rs` | stable AIDL over `/dev/binder` with no Android library (xnuhost is static): the interface token (strict mode word, work source, `SYST`), `String` as UTF-16, `byte[]`, binder objects with the stability word (VINTF 0x3f), the status header of a reply; the kernel's binder protocol, one looper thread for the calls into our callback, death notifications |
| `runtime/bridges/tools/wifictl.rs` | the bridge without xnuhost: `status`, `scan`, `join`, `restore`, `p2p`, `wonder` (`runtime/bridges/README.md`) |

xnuhost: `wifi=auto` (the default; `xnuhost.conf` or `--set wifi=...`) takes wlan0 when the bridges are on, `wifi=off`
makes no network device, `wifi=tap` is the old route, `wifi=sim` is `sim.rs`: a made-up radio with two networks
("OneiOS open", and "OneiOS WPA2" with the passphrase `password`) for a boot on a PC, where the guest's side can be
run and read in the log; no frame goes anywhere (unit-tested; not yet run in a boot). Log lines: `wifi: wlan0 is iOS's for the session: ...`, `wifi:
on: wlan0 (..) is the guest's en0`, `wifi: the vendor's supplicant is the bridge's`, `wifi: the guest's driver is up`,
`wifi: join: network N in the supplicant, WPA2 (passphrase): selected`, `wifi: supplicant: <state>`, `wifi: join done`
or `join failed (status S, code C)`, `wifi: link down (reason R)`, `wifi: 60 s: to iOS ...` (the traffic line, section "After
the join" below), `wifi: scans while joined: flags 0x...`, `wifi: wlan0's AF_PACKET socket: receive buffer N KiB, send buffer N KiB (...)`, `wifi: closed (...)`, `wifi: wlan0 given back`.

Old (only with `wifi=tap`): `runtime/bridges/net/net.rs` (tap `oneios0`, DHCP answered in the bridge: 192.168.77.2/24)
and `runtime/bridges/net/oneios-net.sh` (NAT). The script's `ip rule del priority 16000` would delete netd's own rule
of that priority (netd's `fwmark 0x10063/0x1ffff iif lo lookup local_network` is at 16000 on this phone) when ours is
absent: a reason not to run it on the phone as it is.

### Tested

- Host (arm64 Linux container, `runtime/host-tests.sh`; the C tree's counts, every case now a Rust test beside its module and `tests/test_aidl_binder.rs`): `test_wifi` 162 checks (frame rules; the netlink group reader; the nl80211 band reader (`OW_CHANNELS`); the DHCP reader, with the REQUEST forms
  SELECTING, INIT-REBOOT, RENEWING and REBINDING, iOS's and another station's, and the log line of each; a 4000-byte coalesced
  IPv4 frame and a 3000-byte IPv6 one cut into three with right lengths, sequence numbers and checksums; the station
  against a fake radio; the AIDL parcels byte for byte; the join's plan for WPA2, WPA3, transition, WPA with TKIP,
  open, OWE and a network not seen; the supplicant's callbacks decoded); `test_aidl_binder` 35 checks: the client
  against a fake service manager and supplicant in another process on a real binder driver (a lazy service found on
  the third lookup, the interface added and removed, the callback's descriptor and version asked for inside
  `registerCallback`, a join, a wrong key, a network not found, a rejection, a link that drops, a look without a
  callback, a supplicant that dies). Both ends of that test use `hal::aidl`.
- **Third session (2026-11-12, about 14:00, with `04ab894`)**: the networks show in Settings (`scan done: 64
  networks`, `event 10 for en0, 0 bytes -> 0`, `wifid[41] gets 11: len 2280 -> 0` for each). The owner joined an
  open network: `join: layout 656, authentication lower 0x1 upper 0x0, key of 0 bytes of kind 0`, xnuhost `join:
  network 0 in the supplicant, open: selected`, `supplicant: scanning`, 2.9 s later `associating`, `associated`,
  `completed: joined`, `join done (status 0, code 0)`, the guest `join done: joined`. At the return: `wifi: wlan0
  given back: IPv6 off, GRO on` and `wifi: closed (96 frames to the guest, 45 from it, 0 cut, 0 dropped, 0 refused)`:
  **the join through the vendor's supplicant, frames both ways and the give-back work.** Not working, fixed after it:
  - **ASSOC_DONE was not posted**: `event 9 for en0, 540 bytes -> 40` (`EMSGSIZE`): a kernel event must fit one
    mbuf. Settings stayed at "connecting". Hosted patch `0049-wifi-association-event.patch` lets an IEEE 802.11
    event of Apple's fill an mbuf cluster (UniXNU-sheng's patch for the same event).
  - **Two scans ended by the guest's 10 s timeout** (`scan: no end from the host in 10000 ms`): a scan's records
    and its end are up to 65 frames in one burst, the guest's receive ring has 64 buffers, and the frame that found
    no buffer was dropped: the end. The virtio-net device now holds a frame of the station's own type (0x88b5) for
    up to 50 ms until the guest has a buffer; and in the guest a state message that says "up" for the network a
    join waits for completes that join, so a lost result costs at most the 2 s to the next state.
  - Whether iOS got an address is not in that log. The bridge now logs iOS's DHCP as it crosses (`wifi: DHCP: iOS
    sends DISCOVER for 0.0.0.0`, `wifi: DHCP: OFFER of a.b.c.d from e.f.g.h`, `iOS sends REQUEST SELECTING for
    a.b.c.d`, `ACK of a.b.c.d from e.f.g.h`; a REQUEST's state as RFC 2131 4.3.2 tells them apart: SELECTING,
    INIT-REBOOT, RENEWING, REBINDING) and, 10 s after a join, `wifi: 10 s after the join: N frames from iOS, M to it;
    K DHCP messages of other stations`.
  - **`DHCP: REQUEST of 0.0.0.0 from 0.0.0.0`, repeated** (the boot before 2026-11-12 23:45, after the join and the
    ACK of 172.16.235.71): not iOS's renewals and not a wrong offset. The decoder read the BOOTP header right; the line
    was the one for a message **from the radio** (iOS's own were logged as `iOS sends ...`), and it printed yiaddr and
    option 54, which a REQUEST never has (yiaddr) or has only in the SELECTING state (option 54). So each such line was
    an INIT-REBOOT or REBINDING REQUEST broadcast by another station on the network (a RENEWING one is unicast to the
    server and does not reach iOS): proven for the decoder by `test_wifi`'s DHCP checks (the three forms, built as
    clients send them, all decode with yiaddr 0.0.0.0 and no server); that they were other stations' is [I] (their
    client address was not logged). They also used up the 16 lines a join gets, so iOS's own later messages were not
    logged. Since 2026-11-13 a message whose client (chaddr) is not en0's address is counted, only the first after a
    join is logged (`wifi: DHCP: another station's REQUEST INIT-REBOOT for a.b.c.d (client 3c:22:fb:..) (passed to iOS
    as any broadcast; ...)`), and the count is in the 10-second line; iOS's own message back from the radio would say
    `iOS's own REQUEST ... came back from the radio`.
- **Second session (the diagnostic session of 2026-11-12 11:53, `kernel.kc` 8eff915f from `bd7c1fb`, with the
  medium fix)**: wifid takes en0 (`the interface is enabled (en0 is up)`, its whole attach: CARD_CAPABILITIES, POWER,
  RSSI_BOUNDS, HW_SUPPORTED_CHANNELS, `sets 564` FILTER_MONITOR_EVENTS with 33 bytes, STATE, PHY_MODE, OP_MODE ...)
  and scans: `wifid[41] sets 10: len 5528 -> 0`, and 8 s later `scan done: 45 networks (host error 0)`. But it never
  fetched the results (no `gets 11`), twice. Two causes, both fixed after it:
  - **No IEEE 802.11 event of the driver was ever posted.** The driver (as DroidVMWifi.inc) posted with
    `kev_msg_post`, the function for kernel extensions, which refuses every vendor code that is not a registered
    third party's: `KEV_VENDOR_APPLE` gets `EINVAL`. The kernel's own `kev_post_msg` is the one (UniXNU-sheng's
    driver uses it). So SCAN_DONE never reached IO80211.framework's event reader (`_eventRead`: the event's first 16
    bytes must be the interface's name, its code must be one the client monitors; for SCAN_DONE it then fetches
    SCAN_RESULT records of 2280 bytes until `EIO`). The driver now logs the first events with their outcome.
  - **The bridge listened to the wrong nl80211 group**, so each scan ended by the 8 s timeout and not by the
    radio's event: the kernel writes a multicast group's id before its name, and the reader kept "the last name
    seen", which gave it the id of the group after `scan` (`regulatory`). Fixed and unit-tested
    (`frames::nl_group`).
- **First session with it (prelink's session 2, 2026-11-12 10:28, `kernel.kc` 66faa3e8 and runtime 0.1.8-dev from
  `924a0d9`)**: worked: `ONEIOS WIFI: classes registered ...`, `on: the phone's Wi-Fi, address a6:a4:20:b8:00:0c`,
  `the host has the phone's radio`; xnuhost: `the vendor's supplicant is the bridge's: joins by its AIDL service`,
  `the guest's driver is up; it gets the radio` (so the binder client, the callback registration and the station's
  messages on the virtio-net queues work in a session). Did not work: no scan. 41 requests reached the driver in 20
  minutes, the first 261 s after the driver's start (en0 is named that late: for the owner of the boot), none of
  them a scan or wifid's attach sequence. Cause: the media rule above. Not seen yet with the fix.
- **On the phone, beside One UI (2026-11-12, SELinux Enforcing, no policy change)**: `wifictl status`: the service
  manager's lookup, `getStaInterface("wlan0")`, `getMacAddress` (wlan0's own address), `listNetworks` (0) all
  answered by Android 17's servicemanager and the vendor's supplicant; `wifictl scan`: 53 networks with their
  security from nl80211 in 6 s.

### After the join: traffic, scans while joined, the signal (2026-11-23)

The session of 2026-11-23 18:24 (runtime 0.1.18, `kernel.kc` aef14856 = main + 0092; `out/session-logs/2026-11-23-mac-s1824/`)
joined (`supplicant: completed: joined`, `join done (status 0, code 0)`) and got its lease (DHCP DISCOVER, OFFER,
REQUEST, ACK of 192.168.2.192 from 192.168.2.1), and then web pages did not load or loaded very slowly. What the log
has, and what was missing:

- **No line said anything about the traffic after DHCP.** The only counts were virtio's 60 s line: `net#0 q0 +430 q1
  +452` to `q0 +759 q1 +785`, about 7 to 13 frames a second each way, while the owner tried to load pages. Nothing
  said whether frames were dropped, where, or why.
- **iOS itself was slow to answer**: 11 s from the join to iOS's DISCOVER, and 2.6 s from the OFFER to its REQUEST,
  with every guest CPU busy (`oneios-cpu: ... (all 800%)`: the poster extensions, osanalyticshelper) and the host at
  700-735% of 800 (section 7, the daemon policy's `telemetry` group in `docs/userland.md`). [I: a large part of the
  slowness is iOS's CPU, not the radio.]
- **A frame that found the guest's receive ring full was dropped at once** (`devices/virtio_net.rs`). The ring has
  64 buffers and the guest's worker gives them back in a batch; with iOS's CPUs busy a TCP burst from the radio
  could fill it, and each loss costs TCP a retransmit.
- **A frame the kernel could not describe was lost silently**: `recv` on the AF_PACKET socket answered with an error
  (for example `EINVAL` when a coalesced frame's GSO type is not TCP) and the loop went on without a count.
- **wifid scans every channel about every 35 s while joined** (`wifid[48] sets 10: len 5528`, then `event 10` 5.2-5.4 s
  later with 20-29 networks): each scan takes the radio off the joined network's channel channel by channel.
- **The bars kept the join's signal** (fault 2 of that session): the host sends the RSSI every 2 s (`OW_STATE`) and
  the driver kept it for the RSSI request (16), but posted only the join's LINK_CHANGED. wifid follows the signal by
  APPLE80211_M_RSSI_CHANGED (39) [F: 24A446's `/usr/sbin/wifid` logs `monitoring APPLE80211_M_RSSI_CHANGED failed`
  when it cannot watch it, and `Initializing lqm value: rawRssi %d, scaledRssi %f, noise %ld, snr %ld` once at the
  join; it does not watch LINK_QUALITY (17)].

Changed (runtime and the Wi-Fi archive of the hosted kernel):

1. **The traffic line** (`wifi/traffic.rs`, on the control thread's 2 s wake), once a minute while anything moved:
   `wifi: 60 s: to iOS 1200 frames (1.5 MiB, 3 cut from coalesced), from iOS 400 frames (39.1 KiB); dropped to iOS: 2
   the guest's ring full; from iOS: none; ring full and waited 9 (longest wait that got a buffer 1500 us); 2 scans
   while joined (scan flags 0x1)`. The drops to iOS: `the guest's ring full` (the wait below ran out), `too long`
   (longer than the link and not TCP), `receive errors` (with `last errno N`); from iOS: `refused` (not a frame the
   radio may send), `send failed` (with the errno: `ENOBUFS`/`EAGAIN` is wlan0's queue full). The stop's `wifi:
   closed (...)` line has the same totals with bytes. Since the 19:40 session an `EAGAIN` waits instead and the line
   counts the waits (section "The uplink's full send buffer" below).
2. **The receive ring waits instead of dropping** (`devices/virtio_net.rs`): a data frame that finds no buffer waits
   for the guest's kick of the receive queue (a condition variable, not a poll) up to 20 ms, a frame of the station's
   own type up to 50 ms (it polled every millisecond before). The bridge's receive thread blocks meanwhile and the
   AF_PACKET socket's buffer, now 4 MiB (`SO_RCVBUFFORCE`, as root; about 200 KiB before), holds what comes from
   wlan0, as a NIC's ring and its host's queue would. A frame made on the guest's own transmit (the tap backend's
   DHCP answer, on the vCPU) never waits. The receive function now says whether the guest took the frame.
3. **Receive errors are counted** (above), with the errno.
4. **Scans while joined are low priority**: nl80211 `TRIGGER_SCAN` with `NL80211_ATTR_SCAN_FLAGS` low priority and
   low span (0x101), low priority (0x1) when the driver refuses that (cfg80211 answers a flag the wiphy does not offer
   with `EOPNOTSUPP`, an older kernel `EINVAL`), none after that; the flags the driver took are kept for the next
   scan and logged once: `wifi: scans while joined: flags 0x1 (...)`. A low-priority scan is one the driver may pause
   or cut short for the link's traffic. The phone's driver takes neither (19:40 session: `flags 0x0`): such scans are
   answered from the cache since (section "The uplink's full send buffer" below).
5. **The signal** (`kernel/patches/oneios-hosted/net/OneiOSWifi.cpp`, net/README.md "The signal"): the driver posts
   RSSI_CHANGED (39) with the 592-byte link quality record at the join and whenever the host's 2 s state moves the
   RSSI by 3 dB or more, or the transmit rate by a quarter. The record's layout is the one 24A446's CoreWiFi reads
   (`CWFLinkQualityMetric`: RSSI valid at 0x00, RSSI int32 at 0x04, noise valid at 0x0e, noise int16 at 0x10, CCA at
   0x12/0x13, transmit and receive rate in kb/s at 0x90 and 0x98; an event 39 shorter than 0x250 bytes is refused
   with `Unexpected event payload length`). The event buffer holds 608 bytes (16 + 592); 0049 lets an IEEE 802.11
   event fill an mbuf cluster.

Proof lines on the phone: `wifi: 60 s: to iOS ...` every minute while browsing, with its drops (none or few expected
with the wait); `wifi: scans while joined: flags 0x...`; `ONEIOS WIFI: signal: -56 dBm, noise -95, 433 Mb/s:
RSSI_CHANGED record posted (592 B)` at the join and when walking away from the access point; the guest's `ONEIOS
WIFI: 60 s: events ... 15+xN ...; signal records N (last ... dBm, ... Mb/s)`; the status bar's bars changing with the
distance.

Not done or not known (2026-11-23): not booted on the phone yet; host-tested only. iOS's own log of that session
could not be read (`phone pull-log` wrote 20 bytes: an empty text), so DNS, the captive check and iOS's TCP
retransmits are not seen. The guest's transport (UniXNU-droidvm's `DroidVMVirtioNet.c`) passes only broadcast and
frames to its own address: multicast is dropped there (its comment says so), so iOS gets no IPv6 router
advertisement and no mDNS; IPv4 is not affected. [I: not a cause of the slow pages.]

### The uplink's full send buffer, and scans while joined from the cache (2026-11-23 19:40)

The session of 2026-11-23 19:40-19:44 (the Wi-Fi fix cbf97ea; `out/session-logs/2026-11-23-mac-wifi/xnuhost.log`)
joined, got its lease, posted `RSSI_CHANGED` at the join and on each change (`signal records 4`, then 8), and moved
`to iOS 421751 frames (605.3 MiB ...)` in a minute. Three faults were left in its lines:

- **The uplink dropped frames**: `from iOS: 70 send failed, last errno 11`, then 1624 (88416.6), then 112. Errno 11
  is `EAGAIN`: the AF_PACKET socket's send buffer was full (the frames wlan0's driver has not sent yet count against
  it; the default, `wmem_default`, is about 200 KiB, about 100 frames [I: 1-2 KiB of the kernel's memory each, an ACK
  too]), and the bridge dropped the frame at once. Most were TCP ACKs and requests: downloads stalled and sent again.
- **`13 receive errors, last errno 22`** in the first minute (`EINVAL`). The kernel could not write a
  `virtio_net_hdr` for a coalesced frame, and the frame is lost: One UI's 6.1 kernel writes one only for TCP
  (`virtio_net_hdr_from_skb`; UDP_L4 came in Linux 6.2). GRO is off on wlan0 and no TCP frame came coalesced in that
  session (`0 cut from coalesced` in every line), so these are UDP frames that the driver itself joined [I:
  Qualcomm's FISA, the driver's own UDP aggregation, which ethtool's GRO switch does not turn off; iOS's QUIC]. Not
  fixed: the frame is gone before the bridge sees it, and AF_PACKET on 6.1 cannot say a UDP frame's segment size.
  They are counted, and few (13 in 4 minutes).
- **Scans while joined**: `wifi: scans while joined: flags 0x0`: this phone's driver takes neither low-priority
  flag, so each of wifid's scans (about every 35 s while joined) takes the radio off the network's channel for about
  5 s.

Changed (runtime only):

1. **A frame that finds the send buffer full waits** (`wifi/uplink.rs`). It goes on a list, in order; the guest's
   next frames wait behind it, so the radio still sends them in the guest's order. The frames thread watches the
   socket's `POLLOUT` while any frame waits (an eventfd wakes it for the first one; poll(2), not a loop) and sends
   them then. A frame that waited 20 ms (the receive ring's bound for a data frame) is dropped; 256 frames wait at
   most. The guest's vCPU never waits on the socket: it sends, or puts the frame on the list; its next frame first
   sends the waiting ones as far as the socket takes them (the frames thread may be in the receive ring's wait for
   the guest's buffers; while the buffer stays full this costs the one refused send a frame cost before).
2. **A larger send buffer**: 1 MiB asked with `SO_SNDBUFFORCE` (as root; `SO_SNDBUF`, capped by `wmem_max`,
   otherwise); the kernel doubles it. The start logs what the kernel gave (expected: `wifi: wlan0's AF_PACKET
   socket: receive buffer 8192 KiB, send buffer 2048 KiB (4096 KiB and 1024 KiB asked; the kernel doubles them)`).
3. **The traffic line counts the waits**: `...; from iOS: none; ring full and waited 9 (longest wait that got a buffer
   1500 us); send buffer full and waited 12 (9 sent, longest wait that sent 850 us)`; its drops from iOS add `N
   dropped after a 20 ms wait` and `N dropped with 256 waiting`, and `send failed` is now another error (`ENOBUFS`:
   the interface's queue full). The stop's `wifi: closed (...)` line has the totals.
4. **Scans while joined from the cache**: while joined, a scan iOS asks within 120 s of the end of the last good
   scan is answered from that scan's networks (`station.rs`, `Radio::joined_scan_reuse`; only the phone's radio
   reuses, the stand-in `wifi=sim` scans each time): the radio stays on its channel. A real driver answers a scan
   soon after the last one from its cache. Chosen over a scan of the current channel only: that scan still costs the
   driver a dwell, and its result would lose every network of the other channels from the list iOS shows; with the
   cache the list stays whole and at most 2 minutes old, and one scan of every channel comes about every 140 s
   instead of every 35 s. The answer does not make the kept networks younger; a failed scan is not kept. The traffic
   line: `; 4 scans while joined (3 answered from the last scan's networks, 1 on the radio with scan flags 0x0)`.
5. **The frames thread reads a socket error** (`POLLERR` without `POLLIN`, for example `ENETDOWN` when wlan0 went
   down) as a receive error: before, poll answered at once again and again until a frame came.

Host tests (`cargo test -p onebridges`, in `runtime/host-tests.sh`): `wifi::uplink` `eagain_then_pollout` (held in
order, sent on POLLOUT, the counts and the longest wait), `eagain_to_the_bound` (dropped at 20 ms, the list full,
ENOBUFS while flushing), `the_next_frame_sends_the_held_ones_first` (room before POLLOUT: the held frames go with
the guest's next one, in order; one past its bound is dropped, not sent late), `a_real_socket` (a datagram socket
pair with the smallest send buffer: a real `EAGAIN`, then poll's `POLLOUT` once the peer reads, then the held
frames in order); `wifi::station` `cached_answer_rules` and
`a_scan_while_joined_from_the_cache`; `wifi::traffic` `the_line`. `wifi::wonder` `wonder_session` now looks only
for its own error lines (`E wonder:`): the log capture holds every thread's lines, and another test's error line (the
station test logs `join failed`) made it fail once on the build laptop.

Proof lines on the phone (expected, not yet seen): `wifi: wlan0's AF_PACKET socket: ... send buffer 2048 KiB`; in
the `wifi: 60 s:` lines during a download, `send failed` near 0, `send buffer full and waited N (M sent, ...)` with
M close to N, few or no `dropped after a 20 ms wait`; `N scans while joined (M answered from the last scan's
networks, ...)` with most of them answered. Not tested on the phone: whether 2 MiB is enough for this driver, how
long a wait for `POLLOUT` takes on it (the line's longest wait says), and whether wifid accepts an answer this quick
while joined (the guest's driver already answers within 8 s of a scan from its own copy, `reused` in its line).

### en0 late: two minutes before iOS turns Wi-Fi on (fix-wifi-late, 2026-11-13)

On the phone (boot of 2026-11-12 23:45, main ea06d46, kernel.kc through 0058, the Data store kept) xnuhost had wlan0
from +0.1 s, but `ONEIOS WIFI: the interface is enabled (en0 is up)` came at +130 s (+162 s the boot before, 261 s
after the driver's start in prelink's session 2), while the join and DHCP took 2 s. What it waited for, read in 24A446
and configd's source (configd's `Plugins/InterfaceNamer`, apple-oss-distributions):

1. **configd's InterfaceNamer names a new interface only once IOKit is quiet** (`copyUnitForEstablishedInterface`:
   `waiting for quiet`). An interface is not new when its naming database (`NetworkInterfaces.plist`) has its prefix
   and hardware address, or when the IORegistry reserves a unit for it: `network-interface-unit-en` on the interface
   or a parent. 24A446's `WaitQuietTimeout` is 240 s and `WaitStackTimeout` 300 s (configd's `__data`; the bundle's
   Info.plist sets neither).
2. **en0 was always new.** Its address is wlan0's, a random one while One UI is not joined, so it changes between
   sessions and the database did not match it ([I]: the phone's `NetworkInterfaces.plist` was not read; the
   database is also written only once IOKit is quiet, so a session that ended earlier left none). The iPhone's own Wi-Fi node reserves the unit (24A446's device tree:
   `/arm-io/apcie/pci-bridge0/centauri-alpha`, `network-interface-unit-en = <0>`); our nub `oneios-wlan` did not.
3. **IOKit is quiet only after driverkitd.** The only busy service is the service root, `oneios,xnuhost` (class
   `IOPlatformExpertDevice`; 0042's `oneios-iobusy:` lines on the PC at 30, 60, 120, 240 s and on the earlier PC boot
   until between 240 and 480 s): `StartIOKitMatching` holds one busy count on it until the IOKit daemon (driverkitd
   on iOS) has sent its dexts' personalities (`IOService::iokitDaemonLaunched`; the kernel then logs `KextLog:
   driverkitd is active`). So en0's name, and with it wifid's attach and the `enable` of en0, waited for driverkitd,
   which on the phone runs in the first two busy minutes (lsd, backboardd, iconservicesagent). [I] That driverkitd's
   time is the +130 s: the phone's log of that boot was not read here; the lines to read are below.

**Fix (merged in #8; on the phone on 2026-11-13 it did not make Wi-Fi earlier, below):** the nub has `network-interface-unit-en = <0>` (32 bits), as the
iPhone's node, so InterfaceNamer names en0 as soon as the interface appears (`OneiOSWifi.cpp`, `net/README.md`, "en0's
name"). InterfaceNamer still refuses the unit when its database has en0 at another registry path (`builtinAvailable`;
configd then says `has reserved unit 0, but it's not available`). No XNU source changed, so no numbered patch.

**New lines** (the driver): `ONEIOS WIFI: first naming (en0 attached to the network stack): by configd[PID], +S s
after the driver's start, IOKit busy N`, and the same `first ...` line for `enable (IFF_UP)`, the first `Apple80211
request`, `power on`, `scan request`, `join request` (who, when, IOKit's busy count).

**On the PC** (qemu-aarch64 under a load of 12-24 on 4 CPUs, `--set wifi=sim`, so every time is many times the
phone's), the image set `out/image-pc/24A446`, a Data store with no naming database (no earlier boot on it got as
far as naming):
- With the fix (`kernel.kc` `ee5a85d7`, from this branch; times after the driver's start): launchd +769 s, configd
  spawned +834 s, `KextLog: driverkitd is active` +872 s, `first naming ...: by configd[34], +1009.874 s ..., IOKit busy
  0`, `first Apple80211 request: by configd[34], +1142 s`, `first enable (IFF_UP): by configd[34], +1144.6 s` (so
  IPConfiguration in configd brings en0 up, not wifid), wifid's first request +1350 s (its attach: 12, 19, 106, 254,
  207, 103, `sets 564` ...), `first scan request: by wifid[40], +1775 s`, `scan done: 2 networks` (the made-up
  radio). configd's own log (read from the running boot's `data.img` with `oneios-tool ioslog show ... --proc
  configd`): `copyUnitForEstablishedInterface: <SCNetworkInterface ... address = 02:d7:36:0c:64:80, builtin = TRUE, path
  = IOService:/IOResources/oneios-wlan/OneiOSWifiController/IO80211Interface ...> has reserved unit 0`, then `Known
  interface named unit = 0`, and `*QUIET*` only after that: **the reserved-unit path is taken**. On the PC driverkitd
  was done before configd's InterfaceNamer started (configd's plugins load slowly under qemu), so this boot cannot show
  the time the fix saves; the phone's order is the open question (below).
- Without it (`kernel.kc` `ee88c1b1`: the same `first ...` lines, no reserved unit): the boot was stopped by its
  4200 s `timeout` with configd running for about 1300 s, driverkitd not yet active, and no naming. Consistent with
  `waiting for quiet`, not proven: configd's log of that boot was not on the volume after the kill.
- driverkitd on 24A446 does not ask LaunchServices on this hardware (`Hardware does not support third party
  drivers`, `Using cached application list`, in its log of both PC boots), so lsd is not what it waits for.

**On the phone (2026-11-13 ~10:50, main c1223f8 with the fix, runtime 0.1.9-dev, `kernel.kc` `4b9a91ae`): the fix
did not make Wi-Fi earlier.** Guest seconds: the driver's start 787.6, launchd 809.9, first frame 832.3; `first naming
...: by configd[34], +111.079 s ..., IOKit busy 0` (898.7, launchd + 89 s), `first Apple80211 request: by configd[34],
+129.3 s`, `first enable (IFF_UP): by configd[34], +131.1 s` (918.8), `first scan request: by wifid[40], +200.8 s`
(988.5), then scans of 20 to 39 networks (host error 0). The 0.1.8 boot had en0 up at +128 s. IOKit was already quiet
at the naming, so in this boot InterfaceNamer did not wait for driverkitd: the wait is elsewhere in configd, before
InterfaceNamer names en0 (configd's open source, `plugin_support.c` and `ifnamer.c` at its main branch, not 24A446's build: the plugin thread first loads every plugin
bundle, dlopen()ing the ones that are not built in (IPConfiguration, EAPOLController and the others outside configd's
binary), then calls the `load()` functions in its allow-list order, InterfaceNamer's first; that starts the
`InterfaceNamer thread`, which reads `NetworkInterfaces.plist` (`*START*`), opens IONetworkStack (`*STACK*`), and names
the interfaces that have arrived, while PreferencesMonitor's `load()` runs `__SCNetworkConfigurationUpgrade` on the
plugin thread; the two share the SCPreferences locks). The PC shows the same shape, many times slower: configd spawned +834 s, named +1010 s, `IFF_UP` +1145 s,
wifid's first request +1350 s, first scan +1775 s. What holds configd for those 89 s on the phone is not known yet [U];
the next boot's lines below say it. No join came by +275 s; whether a known network was in range is open (the owner
was asked; 35 networks against the hotel's 64).

**Where configd was, on the PC** (2026-11-13, `kernel.kc` `84d3304c` with 0061, `oneios-ubt=configd
oneios-ubt-at=850 oneios-ubt-every=45 oneios-ubt-count=6`, a fresh Data store; six reports, then `oneios-ubt: 6
reports, the last (oneios-ubt-count)`). configd was spawned about 625 s after the driver's start and named en0 at
+1568 s; its two threads, named with `oneios-tool sym` (slide `0xbb54000`):
- reports 1 and 2 (+850, +895 s): the main thread in `_os_state_add_handler` -> `__os_trace_init_slow` ->
  `_firehose_client_reconnect` -> `_firehose_send_register` -> `mach_msg`: **waiting for logd** to register its log
  buffer (logd's first lines came after configd's in that boot);
- reports 3 to 6 (+940 to +1075 s): the main thread **loading the plugin bundles**: `CFBundleLoadExecutableAndReturnError`
  -> dyld `dlopen_from` -> `notifyLoad` -> libobjc `map_images_nolock`, mostly in state 0x9 (an uninterruptible wait;
  [I] page-ins of the bundle's pages), and the second thread, configd's server (`_dispatch_mach_mig_demux`), waiting
  in `objc_lookUpImpOrForward` for the Objective-C runtime lock the load holds: **configd's server does not answer
  its clients** (wifid, IPConfiguration's own clients) while a plugin bundle loads.
[I] The phone's 89 s are the same two waits (logd, the bundles' load under the first minutes' CPU and storage load);
the next boot's reports say. The PC cannot rank them for the phone.

**wifid's `sets 25` -> 102** is `APPLE80211_IOC_HOST_AP_MODE` (24A446's table; the PC's iOS log names it: `IOCTL
type 25/'APPLE80211_IOC_HOST_AP_MODE', len[0] return -1`, the framework's `-3903`). 102 is `EOPNOTSUPP`: the driver
refuses the access point modes on purpose (`kRefusedSets`, bit 25), and wifid goes on with its attach and scans after
it (PC and phone). Right as it is.

**Who brings en0 up and what starts a join** [F 24A446, PC boot]: configd's IPConfiguration sets `IFF_UP` (the
`first enable` line names configd) after its first Apple80211 requests (`gets 12` CARD_CAPABILITIES, `gets 1` SSID),
not wifid. wifid joins only by its auto-join: a network of its known-networks store
(`/private/var/preferences/com.apple.wifi.known-networks.plist` on the Data volume) that the last scan has; with none
in range it scans and does not join, which is iOS's normal behaviour. Its log says which (`AUTO-JOIN:` lines; the
PC's first auto-join was `trigger=power_on`, then `Cancelled active auto-join request` with no known network). wifid
also has an auto-join deferral (`AJDeferralTimer`, `in autojoin deferral period`, `Auto-join deferral is active, will
not continue auto-join`) and first-unlock rules (`enabling autojoin before first unlock`): its log tells those apart.

**For the next phone boot**, in the image's `xnuhost.conf` boot arguments: `oneios-ubt=configd,wifid,logd oneios-ubt-at=20
oneios-ubt-every=10 oneios-ubt-count=18` (hosted 0061: a report of every configd and wifid thread every 10 s from +20 s
to +190 s after bsd_init, each headed `oneios-ubt: report K at +S s`), and `oneios-sysfail=configd` (0034: its failing
path calls). Then:
- `oneios-tool sym` on the `oneios-ubt: configd[34] thread ...` lines of the reports before `first naming`: the thread
  named `InterfaceNamer thread` and configd's plugin thread show where they wait (an SCPreferences lock, a plugin's
  `load()`, an IOKit call, a Mach message to another daemon); the same for wifid between `first enable` and `first
  scan request`.
- `xnuhost.log`: `first naming`, `first Apple80211 request`, `first enable`, `first scan request` against launchd's
  start and the `oneios-ubt: report` times; `KextLog: driverkitd is active`; `wifi: join` / `ONEIOS WIFI: join:` if a
  join comes.

**Searches in iOS's own log** (`oneios-tool ioslog show data.img --boot last --sort ...`; the times of configd's
`addTimestamp:` lines are its own clock, `CFAbsoluteTime`):
- `--proc configd --grep 'addPlugin|loading |adding |load failed|InterfaceNamer|addTimestamp|STACK|QUIET|COMPLETE|reserved unit|Known interface|waiting for quiet|Assigned unit|lock held|lock delayed|Hardware model changed|Network configuration|currentSetExists'`:
  when configd started, when InterfaceNamer started (`*START*`), when it had the stack (`*STACK*`), when en0 was named,
  and which SCPreferences lock was held for how long.
- `--proc configd --grep 'IPConfiguration|en0|DHCP|link'`: when IPConfiguration started the en0 service and brought
  it up.
- `--proc wifid --grep 'Now attempting to add AirPort Interface|WiFi network interface added|AUTO-JOIN|deferral|Deferral|first unlock|First unlock|known network|KnownNetwork|power|Power|scan'`:
  when wifid took en0, why its first scan came 70 s after `IFF_UP`, and what its auto-join found (a known network in
  range or none).
- `--proc wifid --type error,fault` and `--proc airportd`: anything that failed.
- The known networks themselves: copy `/private/var/preferences/com.apple.wifi.known-networks.plist` out of the Data
  volume (`oneios-tool image apfs-copy --volume 2 --newest data.img /preferences/com.apple.wifi.known-networks.plist DIR`:
  the Data volume is `/private/var`) and compare its SSIDs with the scan.
Not checked without the phone: everything in this list; whether the phone's configd waits on the same thing the PC's
does.

### Wi-Fi "off" for three minutes: en0 up only when configd's plugins have started (fix-wifi-late-enable, 2026-11-15)

**The phone** (main 8644ae9, 2026-11-15; Mac: `out/session-logs/2026-11-15/main-8644ae9`): Settings shows Wi-Fi off
and its switch does nothing for about three minutes, then Wi-Fi comes on by itself. `addPlugin: InterfaceNamer` at
10:35:31.07, `first naming (en0 attached ...): by configd[34], +125.535 s after the driver's start`, `*QUIET&NAMED*`
at 10:35:32.62, then `first enable (IFF_UP): by configd[34], +174.342 s`: **49 s after the naming**. Before that wifid
already had en0 and scanned (`60 s: events 10x24; scans asked 24 (started 5, reused 19, ...)`, POWER read 10 times as
on, no `sets 19` even at the owner's taps). One of those 49 s: `SCPreferences(InterfaceNamer:writeInterfaceList:
NetworkInterfaces.plist) lock held for 6.427 seconds`. wifid's first power set: `first power on: by wifid[40],
+376.703 s`, at the owner's toggle.

**Who sets IFF_UP, read in the open source** (apple-oss-distributions `bootp-534.120.2`, `configd-1405.120.5`; nothing
of Apple's binaries disassembled for this):
- Only IPConfiguration, as configd's plugin: `IFState_service_add()` -> `inet_attach_interface(if_name(if_p), TRUE)`
  -> `interface_set_flags(s, ifname, IFF_UP, 0)` (`IPConfiguration.bproj/ipconfigd.c` and `ifutil.c`), when it adds the
  interface's network service from the `Setup:` keys; `DisableUntilNeededProcess` is the only other caller
  (`interface_up_down`). IPConfiguration's `start()` (its first line, `IPConfiguration: IPv6 linklocal modifier
  expires`, from `CGAInit`) and `prime()` (`start_initialization`) run on configd's plugin thread, after the `load()`
  of every plugin, then every `start()` in order (`configd.tproj/plugin_support.c`); so en0 is up only once all of
  configd's plugins have loaded and IPConfiguration's turn has come.
- PreferencesMonitor creates a default configuration only when InterfaceNamer posts `*QUIET*` (or a timeout), and only
  when there is no current set (`prefsmon.c` `storeCallback`, `establishNewPreferences`); InterfaceNamer posts
  `*COMPLETE*` (iOS only) once the previous boot's interfaces are named and `*QUIET*` when IOKit's busy state goes to 0
  (`ifnamer.c` `quietCallback`, `updateInterfaces`); `SCNetworkInterfaceCopyAll()` blocks until one of the three
  (`SCNetworkInterface.c` `__wait_for_IOKit_to_quiesce`).
- On the PC (`wifi=sim`, this branch's first two boots, configd's own `addTimestamp:` CFAbsoluteTimes): a fresh Data
  store: `*COMPLETE*` 816414947.4, en0 named 968.5, `*QUIET*` 982.5, `*QUIET&NAMED*` 988.2 and IPConfiguration's first
  line right after; `first naming` +919.4 s and `first enable` +1007.8 s after the driver's start (88 s). A kept store
  (en0 in `NetworkInterfaces.plist`): en0 named 790.98, `*COMPLETE*` 863.1, `*QUIET*` 863.8, IPConfiguration right
  after; `first naming` +316.0 s, `first enable` +440.1 s (124 s). In both, `IOKit busy 0` at the naming
  (`KextLog: driverkitd is active` before it), and the IFF_UP came 3 s after configd's `Apple80211BindToInterfaceWithService`
  and `_getIfListCopy`, once IPConfiguration had the service (`IPMonitor prime 6 keys (... /Network/Service/<id>/IPv4 ...)`).
- So nothing of our interface is what configd waits for: it names en0 at once (the reserved unit, or the database),
  and en0 is up when configd's plugin start-up reaches IPConfiguration's service. On the phone that took 49 s; why
  configd's plugins themselves start only +125 s after our driver (launchd's order, the boot's CPU and disk load) is
  [U]: the Mac's session folder has the lines, this box (load 18-36 from other agents' builds on 2026-11-15) could not
  run a PC boot to the naming in the time it had.

**Fixed: the driver brings en0 up itself when it is attached** (`OneiOSWifi.cpp`: `IO80211Interface::
attachToDataLinkLayer` after the family's, then 50 ms later on a thread call `ifnet_set_flags(IFF_UP)` and
`ifnet_ioctl(SIOCSIFFLAGS)`, which makes the family's `syncSIOCSIFFLAGS` enable the controller as the socket ioctl
does). IFF_UP is only the administrative flag: IPConfiguration still attaches IPv4 and IPv6 and runs DHCP when its turn
comes (and sets IFF_UP again, a no-op). [I] That wifid shows Wi-Fi on and takes the switch once en0 is up (its
auto-join lists `Interface not up` among its reasons to hold, and Wi-Fi came on by itself at the IFF_UP on the phone).
`oneios-wifi-up=0` leaves it to configd. Line: `ONEIOS WIFI: en0 brought up by the driver when attached, +S s after the
driver's start -> 0 (...)`; `first enable (IFF_UP)` then names `kernel_task` at about the naming's time.

**A lasting disk line** (xnuhost `blk.rs`): every 30 s per disk with requests, `disk data: 30 s: reads N (M MiB), writes N
(M MiB), flushes N in T ms (max X ms)`, at the stop the same `since the start`, and the first 16 flushes of 500 ms or
more as they happen (`disk data: a flush took N ms (fdatasync; the guest's other requests to this disk waited)`). A
guest flush is an `fdatasync` of the whole `data.img`; on a kernel that takes `VIRTIO_BLK_F_LATE_FLUSH`
(main's "Disks" in `docs/booter.md`) the flush thread serves it off the vCPU, otherwise inside the guest's notify
while the guest's disk driver holds its one gate, so every other request of the guest to that disk waits behind it;
whether the phone's 6.4 s plist
lock was a flush is what this line says next session. [F, PC, a write-and-flush test on this box's btrfs SSD] a flush
of 20 to 200 MiB written since the last took 40 to 350 ms, and starting the host's writeback early
(`sync_file_range`) did not shorten it, so that was not added.

**Phone proof**: `ONEIOS WIFI: en0 brought up by the driver when attached` within a second of `first naming`,
`first enable (IFF_UP): by kernel_task[0] +N s` with N the naming's; Settings' switch answering at once (a `sets 19` at
the owner's tap); the `disk data:` lines and any `a flush took` around configd's `lock held for` lines.

### Wi-Fi does not join: every scan result filtered out (fix-wifi-join, 2026-11-13; partly done)

**The phone** (c1223f8 session, iOS's own log): wifid knows 2 networks (`Fetched 2 known networks from the cache`),
auto-join scans (`AUTO-JOIN: Discovering known networks (maxScanCycles=1, maxScanChannelCount=3, maxScanSSIDCount=9,
minRSSI=-80)`), and CoreWiFi drops every result: `CWFFilteredScanResults ... All scan results are filtered out.
mutableScanResults count: 0, scanResults count: 1..7` (32 times), `AUTO-JOIN: Scan Results (0)` (21) and `Scan Cache
Results (0)` (12); also `Could not read APPLE80211_IOC_CHANNELS_INFO err=-3903` (13) and 17 wifid entries of the fault
type (their messages were not read here).

**Read in 24A446** (the dyld cache, with `ipsw dyld disass/xref/str` and `ipsw class-dump`; nothing of Apple's kept):
- `_CWFFilteredScanResults` (CoreWiFi) drops a result for, in this order: `age` above the filter's `maximumAge` (when no
  `minimumTimestamp`), `timestamp` below `minimumTimestamp`, `RSSI` below `minimumRSSI`, a channel (number and band) not
  in `channels`, a FILS discovery frame, not in `includeScanResults`, a background scan result, a hidden network, BSS
  type, SSID, short SSID, network-name prefix, the predicate, duplicates and the merge policy. Each reason has its own
  line (`[corewifi] Filtering based on maxAge (...)` and so on) at the info level, so the phone's log, which keeps the
  default level, does not have them.
- `-[CWFScanResult timestamp]` is the scan record's `TIMESTAMP`; `-[CWFScanResult age]` is
  `(clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - timestamp) / 1000000` (ms).
- IO80211.framework's `__addScanResultToList` reads `AGE` (32 bits) at 0x84 and **`TIMESTAMP` (64 bits) at 0x8d0** of the
  2280-byte record (this doc called it "an address" before), plus a byte at 0x8d8 and 32 bits at 0x8dc when a
  property of its own is set. Our driver leaves 0x8d0 zero, so every result's `age` is the phone's uptime.
- Auto-join (`-[CWFAutoJoinManager __performScanWithChannelList:SSIDList:passive:dwellTime:maxCacheAge:cacheOnly:...]`)
  calls `setMaximumAge:` on its filter twice right before `_CWFFilteredScanResults`.
- So [I, strong; the info-level reason line would prove it]: **every result fails the `maxAge` filter because its
  `TIMESTAMP` is 0**. The Settings list does not filter by age, which is why networks showed there.
- `APPLE80211_IOC_CHANNELS_INFO` (207, refused by our driver): IO80211's `Apple80211GetWithIOCTL` reads a 10016-byte
  record: the count (16 bits) at 0x0c, then per channel i (at most 400) the number at 0x32e+i, indoor-only 0x4be+i, DFS
  0x64e+i, passive 0x7de+i, 40 MHz 0x96e+i, 80 MHz 0xafe+i (bytes), a 32-bit bitmap at 0x12d0+4i, a 6 GHz byte at
  0x1aa0+i (`CHANNELINFO_BAND_NUM`: 1 with it, else 0 below channel 15 and 3 above). wifid reads CH_NUM, DFS,
  BAND_NUM, 40MHZ and 80MHZ, and retries 6 times, 0.5 s apart, on EBUSY, ECONNREFUSED or -3905.
- wifid's `sets 25 -> 102` is `APPLE80211_IOC_HOST_AP_MODE`, refused on purpose (see "en0 late").

**Done here (host-tested, in the runtime):** the bridge sends the radio's channel list to the guest after `OW_HELLO`
(`OW_CHANNELS`, `drivers/net/oneios_wifi_proto.h`): on the phone from nl80211 (a split `NL80211_CMD_GET_WIPHY` dump for
wlan0, `nl_bands` in `frames.rs`: each channel that is not disabled, with passive, radar, indoor-only, no-40
and no-80 MHz flags), with `wifi=sim` a fixed list (38 channels). Log line: `wifi: N channels of the radio for the guest
(F MHz to L MHz)`. The guest drivers of main ignore the message (an unknown type), so it is harmless until the driver
below is in. `test_wifi`: 162 checks (the band parser on a constructed attribute tree, cut and split cases; the sim's
channel message).

**The guest driver's half** is in `OneiOSWifi.cpp` since the hand-over of 2026-11-13: it was kept as a diff until
then, applied in the integration tree of that day and **compiled** in its kernel (`HOSTED_OUT=integ`, the prelink
audit 0 failures, `stubs-check` 666 good); **not booted yet**. It: stamps each BSS with the continuous-clock time the radio saw it
(`mach_continuous_time` in ns minus the host's `age_ms`) and writes it at 0x8d0 of every 2280-byte `SCAN_RESULT`
(and "now" for `CURRENT_NETWORK`); answers `CHANNELS_INFO` from the host's list (built-in list until one came);
builds `SUPPORTED_CHANNELS` from the same list (active unless passive, DFS flag 0x100 on radar channels); logs
`the host's channels: N, F MHz to L MHz`, `channels info: N channels (the host's|the built-in list)` and once per boot
`scan record 1 (...): channel, flags, RSSI, noise, SNR, capability, beacon, BSSID, name length, rates, age, TIMESTAMP ..
(.. ms before now), element bytes`; the log line buffer grows to 384 bytes.

**Next, in order:**
1. Boot the PC with `--set wifi=sim` (a kernel built from `main`; the archive is `oneios-tool kernel gen net`):
   `scan record 1` must show
   a TIMESTAMP a few ms before now, `channels info: 38 channels (the host's list)`, and wifid's log (`ioslog --proc wifid`)
   `APPLE80211_IOC_CHANNELS_INFO returned 38 channels in list` instead of `Could not read`.
2. On the phone: a join to a known network in range. Lines: `ONEIOS WIFI: scan record 1`, `wifi: N channels of the
   radio`, `ONEIOS WIFI: join: ...`; in iOS's log `AUTO-JOIN: Scan Results (N)` with N > 0 and `Found matching network`.
   If results are still filtered: turn on CoreWiFi's info level for the session (or read `oneios-tool ioslog show
   --type info`, if the store keeps it) for the `Filtering based on ...` reason.
3. The 17 fault entries of wifid: `ioslog show --proc wifid --type fault` (their messages); not looked at yet.
4. configd's 22 s between `*STACK*` and en0's name (InterfaceNamer): the next boot's `oneios-ubt` reports of configd
   (0061) name where its InterfaceNamer thread is in those seconds.


**On the phone with the driver's half (main 9f6dc38, 2026-11-14): Wi-Fi still "just keeps scanning".** The driver's
side works: `scan record 1 (2280 bytes asked): channel 6 flags 0xa, RSSI -65, noise -95, SNR 30, capability 0x1431,
beacon 100 TU, ..., age 2328 ms, TIMESTAMP 16830238088697 ns (3951 ms before now), 302 element bytes`, `channels info:
63 channels (the host's list)`, 20 `scan done` of 7 to 25 networks (host error 0); `first naming` +96.5 s, `first
enable` +134.0 s, `first scan request` +248.1 s after the driver's start. Still no `sets 20` (join).

**Read for it (fix-wifi-join-2, 24A446's dyld cache):**
- The record's numbers agree: age 2328 ms is the host's (nl80211's) at the moment the guest got it, TIMESTAMP is that
  moment minus the age, and the record was asked for 1.6 s later (3951 ms). The clock: `TIMESTAMP` is
  `mach_continuous_time` in ns (kernel), `-[CWFScanResult age]` is `clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW)` minus
  it, and CLOCK_MONOTONIC_RAW is `mach_continuous_time` scaled by `mach_timebase_info`, which is the kernel's
  `rtclock_timebase_const`, set from the same frequency as `absolutetime_to_nanoseconds` (`osfmk/arm/rtclock.c`); at the
  time of that trace the guest's timebase was unscaled (19.2 MHz), and since 2026-11-28 xnuhost scales it to 24 MHz via
  `droidvm-timebase-hz=24000000` + the `xs_handler` stub fast-path (`docs/booter.md` 2.4).
  [I] So the age CoreWiFi computes is the right one; not seen in a log.
- Auto-join's filter (`__performScanWithChannelList:...`) is the scan's own `CWFScanParameters`: BSS type, PHY mode,
  include-hidden, `addScanDurationToMaxAge`, the channels (the known networks' recent ones), the SSID list, dwell time,
  scan flags and type, acceptable cache age; then `maximumAge` = the `maxCacheAge` argument, plus the scan's duration
  in ms (CLOCK_UPTIME_RAW) when `addScanDurationToMaxAge`. The value of `maxScanCacheAge` comes from wifid's
  auto-join parameters and was not found statically. A result seen during the scan is younger than the scan.
- `_CWFFilteredScanResults` reads of each result: `age`, `timestamp`, `RSSI`, `channel` (number and band: CoreWiFi's
  `channelWithNumber:band:width:` with width 0, the band from the channel flags; 0xa is 20 MHz and 2.4 GHz, right),
  `scanFlags`-dependent background and FILS checks, `BSSType` (`isESS`/`isIBSS`: IO80211 makes `AP_MODE` 2 from the
  capability's ESS bit, so 0x1431 is ESS), `SSID`, `shortSSID`, `networkName`, `isPasspoint`. Not the security, the
  PHY mode or the known network's state: those come later, in auto-join's own candidate checks.
- **Fixed here: `PHY_MODE` at 0x8e0** of the record (IO80211 reads 32 bits there after `AP_MODE`) was zero (no mode).
  The driver now gives the modes the network's band, rates and elements show (`APPLE80211_MODE_11A` 0x2, 11B 0x4,
  11G 0x8, 11N 0x10 with HT Capabilities, 11AC 0x80 with VHT Capabilities, 11AX 0x100 with the HE extension element),
  in `SCAN_RESULT` and `CURRENT_NETWORK`, and `AGE` (0x84) is now the age as of the answer (now minus TIMESTAMP), not
  the host's at arrival. `scan record 1` says `PHY 0x..` (the line is shorter since: `scan record 1 (2280 B): ch .. flags .. RSSI .. noise .. SNR .. cap .. beacon .. BSSID .. name .. B .. rates age .. ms TIMESTAMP .. ns (.. ms ago) PHY 0x.. IEs .. B`; the console cut the longer one at about 250 characters). [I] Whether PHY mode 0 was what auto-join refused: not
  known (it is not one of the filter's checks; auto-join's scan sets a PHY mode of its own). On the PC: `scan record 1 (2280 bytes asked): channel 6 flags 0xa, RSSI -48, ..., age 34 ms, TIMESTAMP .. (34 ms before now), PHY mode 0xc` for the made-up 2.4 GHz network (11b and 11g; the line of that build was cut by the console after it).
- **Fixed here: the reason becomes visible.** CoreWiFi's reason lines are at the info level, which the store does not
  keep. Hosted 0070 (`oneios-log-persist=com.apple.WiFiManager`, off by default) writes the Data volume's logging
  override before launchd so that info-level messages of that subsystem are kept (`kernel/patches/oneios-hosted/README.md`,
  0070). On the PC it works: with `oneios-log-persist=com.apple.WiFiManager` iOS's log kept wifid's info-level `[corewifi]` lines (14 in the first minutes of a `wifi=sim` boot, none expected without it: the system has no preference of its own for that subsystem).

**For the next phone boot:** `oneios-log-persist=com.apple.WiFiManager` in the image's `xnuhost.conf` (with 0061's
`oneios-ubt=configd,wifid,logd oneios-ubt-at=20 oneios-ubt-every=10 oneios-ubt-count=18`). In `xnuhost.log`:
`oneios-logpersist: com.apple.WiFiManager: info kept (...)`, `ONEIOS WIFI: scan record 1 ... PHY 0x..`. The Mac's
manual tap of a network tests the join path apart from auto-join (`ONEIOS WIFI: wifid[..] sets 20` and `join: layout
656 ...`).

**Searches after the return** (`oneios-tool ioslog show data.img --boot last --sort`, plus):
- `--proc wifid --grep 'Filtering based on|Filtering BG|Filtering FILS|Filtering hidden|Filtering duplicate|All scan results are filtered out'`:
  the reason for each dropped result (needs 0070's file; otherwise add `--type info` and see whether the store kept any).
- `--proc wifid --grep 'AUTO-JOIN: (Discovering|Scanning|Scan Channels|Scan Hidden|Scan .*Results|-- |Found matching|Skipping)'`:
  what auto-join scanned for (the channels and hidden SSIDs it asked), what came back, and why a candidate was skipped.
- `--proc wifid --grep 'age=[0-9]+ms'`: CoreWiFi's own description of a result (`age=%lums (%llu)`, the age and the
  timestamp it computed); an age of hours would be a clock mismatch after all.
- `--proc wifid --grep 'known networks|KnownNetwork|disabled|lastJoined|autoJoin'`: the 2 known networks and whether
  either is disabled for auto-join.
- `--proc wifid --grep 'ASSOCIATE|Apple80211Associate|join'` and xnuhost.log's `sets 20`: the join path, for the manual
  tap.
- `--proc wifid --type fault`: the 17 fault entries' messages.
- The known networks: `oneios-tool image apfs-copy --volume 2 --newest data.img /preferences/com.apple.wifi.known-networks.plist DIR`
  (the Data volume is `/private/var`), then compare their SSIDs and channels with the networks in range (the last
  `wifi:` scan lines, or `wifictl scan` in One UI).

**iOS's own log of the same session: auto-join is off by the user's setting** (`AUTO-JOIN: Auto-join aborted (...
error=(1 'Auto-join is disabled by user (and not in priority link setup)'), reason=DisabledByUser)` 19 times,
`WiFiManagerGetUserAutoJoinState: user auto join state 0` 18 times and `... state 1` 19 times, `WiFiManagerIsAlwaysOnWiFiEnabled:
user auto-join disabled.` 14 times; `CWFFilteredScanResults` no longer among the top lines). Read in 24A446's wifid
(`/usr/sbin/wifid`, `llvm-objdump` and its strings): `WiFiManagerSetUserAutoJoinState(manager, state, trigger)` has 10
callers. All but two set it to 1, each with its own log line: trigger 1 and 6 and 8 (enabling paths), 2 `Forcing
auto-join to be enabled for network %@`, 3 `reseting user auto join due to restart` and `... after first unlock due to
device restart`, 4 `enabling user auto join due to motion state`, 5 `enabling user auto join due to time (date: %@)`.
**Only trigger 0, a client's request over XPC, sets it to 0**, logged as `Set user auto join enabled requested by "%@"
enabled %d` (the client's name in the quotes; Control Center's Wi-Fi button is such a client: its strings `Control
Center`, `Control Center/3rd Party Client`, `Control Center Widget`, `ControlCenterToggled: state:%d`). The state is
kept in wifid's plist with the place and date of the disabling (`UserAutoJoinState`, `UserAutoJoinDisabledLoc`,
`UserAutoJoinDisableTimestamp`), so it survives the session in the kept Data store; wifid turns it back on by itself by
time, by motion or by a new location (location needs location services: [I] ours may never give one). Nothing of
ours sets it: no driver request or event reaches that setter. [I] The 0 came from a tap of Control Center's Wi-Fi
button (which on iOS means "disconnect, no auto-join until tomorrow") in this or an earlier session; the client's line
says it.

**`maxScanChannelCount=-1`** (3 before #22): -1 is "no limit": with the channel list from `CHANNELS_INFO` (63) auto-join
may scan all of them. [I] Right; it was limited to 3 while wifid had no channel information.

**For the Mac, after the return** (`oneios-tool ioslog show data.img --boot last --sort --proc wifid`, plus):
- `--grep 'Set user auto join enabled requested by|user auto join state [01] trigger|ControlCenterToggled|control center state changed'`:
  who turned user auto-join off and when, and each change with its trigger number (0 a client, 1-8 the enabling paths
  above).
- `--grep 'date to enable user auto join|disabled location for user auto join|enabling user auto join due to'`: when
  wifid will turn it on again by itself.
- To turn it on now: in iOS, tap Control Center's Wi-Fi button once more, or turn Wi-Fi off and on in Settings, or tap
  a network in Settings (a manual join does not need auto-join).
- **Is a known network in range** (without sending the names anywhere): copy the store's
  `/preferences/com.apple.wifi.known-networks.plist` (`oneios-tool image apfs-copy --volume 2 --newest data.img
  /preferences/com.apple.wifi.known-networks.plist DIR`) and read it on the Mac (`plutil -p`). For each of the 2 entries
  (`wifi.network.ssid.NAME`): its `SSID`, its security (`SupportedSecurityTypes` or the profile's security keys), its
  `BSSList` channels, `AddedAt`, `JoinedBySystemAt`, `JoinedByUserAt`, and any `Disabled`, `AutoJoinDisabled` or
  `__OSSpecific__` auto-join keys. Compare the SSIDs with the networks One UI sees at the same place (`wifictl scan` on
  the phone in One UI prints each network's name and security) or with iOS's Settings list. What the answers mean: no
  known SSID in the scan: nothing to join, iOS behaves as designed. A known SSID in range but `AutoJoinDisabled` or
  `Disabled` set: iOS will not join it by itself (a per-network setting). A known SSID in range, enabled, and user
  auto-join back at 1, but still no `sets 20`: a fault on our side; then the `Filtering based on` lines (0070) and the
  `AUTO-JOIN: Skipping` lines say why.

### Settings uses a full core while the password sheet is open (fix-settings-wifi-cpu, 2026-11-15)

**Superseded (fix-hapticd, 2026-11-15):** the 8644ae9 sessions had #37's scan reuse and Preferences stayed at
102-110 %, in every text field, not only this sheet: the load is each key's Core Haptics call waiting for a server that
gave up at its start (section 5, "Core Haptics' server gave up"; hosted 0078). The scan findings below stand as Wi-Fi
fixes.

**The phone** (main 3233fed, 2026-11-15): `oneios-cpu: 10 s: Preferences[181] 106-111%` for minutes (kernel share 20-25 %),
backboardd 30-50 %, kbd 18 %, typing lags; touch is not it (`706 HID events queued ... 0 dropped`, 0.4-0.7 ms).
Preferences' top messages in iOS's log: 724 pairs of `ProcessSignpostStart/End [com.apple.UIKit:TableView]`, 493
`Create send gesture actions`, 17 `Error __WiFiManagerClientAddDevice: for device ... using server port`, 8 `WiFi
Sharing functionality not available from Sharing`; 63 `AUTO-JOIN: Scan SUCCEEDED` in the session; `ONEIOS WIFI: event
10 for en0` among the driver's first 24 events.

**What our side sends, read in the code and in 24A446 (proven unless marked):**
- **Event 10 is `APPLE80211_M_SCAN_DONE`** (1 POWER_CHANGED, 2 SSID_CHANGED, 3 BSSID_CHANGED, 4 LINK_CHANGED, 9
  ASSOC_DONE). The driver posts it once per scan of the host's, and only for a scan a client asked for (`SCAN_REQ` 10 or
  `SCAN_REQ_MULTIPLE` 86); a request while a scan runs gets that scan's one event. On the PC with main's driver
  (`wifi=sim`, 2026-11-14) wifid asked for 33 scans in 102 s, in pairs 0.1 s apart every 6.1 s, and got 33 SCAN_DONE: the
  event rate is wifid's request rate, one for one. On the phone the 63 auto-join scans match. No BSSID, RSSI or link
  event is posted outside a join or a leave (SSID_CHANGED, BSSID_CHANGED and LINK_CHANGED only when the link was up,
  ASSOC_DONE once per join), and the host's 2-second `OW_STATE` while joined posts nothing.
- **Who asks: locationd, two scans every 6 s, for ever** [F, the PC boot of this branch, iOS's own log]: every scan
  request in the boot was wifid's `Async scan requested by "locationd" for 1 iterations with maxage=0` (116 of 116 in
  the first minutes of scanning), a 2.4 GHz scan of channels 1, 6, 11 and then one of 2 to 13 0.1 s later
  (`WiFiDeviceManagerIs5GScanRequest: 2GHz Scan request because channel 1|2 is requested`). locationd's Wi-Fi
  positioning gets no fix (`@WifiAps, lookiter, ..., notindb`, `@WifiFlow, outcome, nofix`, `wifi fix is insufficient as
  it is invalid`: its access point database has none of them and there is no network to ask) and no GNSS to fall back
  on, so it sets the next scan 6 s on (`@WifiTimer, set, locreq, 6`, `WifiTimer, nextscan, ..., haveLoc, 0, codepath,
  Unassociated`), with location services off system-wide (`location disabled system-wide, denying executable ...`,
  `BigSwitch 0`); its Wi-Fi provider was started by its own location controller (`WlpReg, 1, loccontroller`,
  `@ClxProvider, start, wifi, desiredAccuracy, 100.0`; which client holds it is private in the log). Each answer goes
  through wifid's whole scan path (`{SCAN+} Attempting Apple80211ScanAsync`, `WiFiScanObserver ingestScanResults`,
  `WiFiSettlementObserver _handleScanResults`, `Locationd scan done.`). The rate line of that boot, every minute for 8
  minutes: `events 10x20; scans asked 20 (started 20, joined 0, refused 0), done 20 (2 networks), ...; records 40; power
  sets 0 (changed 0); by wifid 20/40`. [I] On the phone, where a radio scan takes seconds, that is a radio that scans back
  to back, and auto-join's, Settings' and a join's commands wait in wifid's queue behind them.
- **Fixed: a scan request within 8 s of the end of the last good scan gets that scan's records again** (their own
  ages and TIMESTAMPs, so an age filter sees how old they are), its SCAN_DONE 20 ms later (IO80211 marks the async scan
  pending around the ioctl), and no radio scan. The rate line counts them as `reused`. A scan that timed out or failed,
  and power off, end the reuse. The events stay one per request (wifid asked for each): what goes is the radio time.
  The PC boot with it (`wifi=sim`, kernel.kc d846437d), 5 minutes of rate lines: `events 10x20; scans asked 20 (started
  5, reused 15, joined 0, refused 0), done 20 (2 networks), timed out 0; records 40; ...; by wifid 20/40` (18 and 19 in
  two of them): 5 radio scans a minute instead of 20, and every request still ended in wifid (`Async scan requested by
  "locationd"` 49, `Completed Apple80211ScanAsync` 48 with the 49th running, no `Thread mismatch` or `scan loop
  unexpected exit`), each reused answer read in full (2 records and the EIO).
- **Rates, before and after, on the PC** (`wifi=sim`, SCAN_DONE and radio scans per minute): main's driver 19.5 and
  19.5 (2026-11-14: 33 in 102 s); the first commit (per-reader cursor, power and no-radio fixes) 20 and 20; with the
  reuse 20 and 5. POWER sets: none in any PC boot (nobody turned Wi-Fi off and on). Not run on the phone.
- **On the phone, the lines that prove it**: `ONEIOS WIFI: 60 s: ... scans asked N (started S, reused R, ...)` with S
  about one per 8 s at most while locationd scans, `events 10xN` equal to `scans asked` minus `refused`, power events
  only with `(changed N)` and `power 1 (as it was: no event)` for a set that changed nothing; in iOS's log
  (`ioslog --proc wifid --grep 'Async scan requested by'`) who asks, and `--proc locationd --grep 'WifiTimer, set'` its
  interval. [I] Whether Preferences' load falls with it is for that session's `oneios-cpu:` lines.
- What a SCAN_DONE does in a client [F, IO80211.framework `_eventRead`]: a client with an asynchronous scan of its own
  pending reads the records (`_getAsyncScanResults`: `SCAN_RESULT` until `EIO`, which is the expected end); any other
  client that monitors the event gets its callback with code 10 and no data. The event goes to every client's event
  socket (wifid, wifip2pd, ...).
- **Fixed: one `SCAN_RESULT` cursor for every reader.** Two loops that ran together split the list between them, and a
  loop that stopped early made the next one start in the middle, so a reader could get a different part of the same
  scan each time. Now each reading thread has its own cursor (eight slots, freed at its `EIO`, the least recently used
  taken when all are in use), reset when a new scan finishes. [I] On the PC only wifid read records; whether two readers
  ever overlapped on the phone is not in its log.
- **Fixed: `POWER_CHANGED` at every `POWER` set**, also a set to the state the driver already had. wifid reads the power
  on that event and tells its clients (Settings' switch, Control Center), which redraw. Now the event (and `OW_POWER` to
  the host) only when the state changes; the first four sets that change nothing say `power 1 (as it was: no event)`.
  [I] How often wifid set it on the phone is not in the lines we have.
- **Fixed: a scan with no radio got an empty SCAN_DONE at once** (power off, or the host not answered yet): an event for
  a scan that never ran, which a daemon that scans again on an empty list answers at once. Now the request is refused
  (`ENETDOWN`) and nothing is posted.
- **A lasting rate line**: `ONEIOS WIFI: 60 s: events 10x6 4x1; scans asked 6 (started 3, reused 3, joined 0, refused 0), done 6
  (2 networks), timed out 0; records 18; power sets 0 (changed 0); by wifid 6/18` (scans asked / records read per
  process), one a minute while anything happened. It says on the next phone boot whether iOS asks again and again and
  who.
- **`__WiFiManagerClientAddDevice` error** [F, MobileWiFi]: the client logs it when it is told a device is attached
  that its own device table already has (`CFDictionaryContainsKey` on the interface name, then `.cold.3`); the calls
  come from `WiFiManagerClientDispatchAttachmentEvent` (wifid's attach notice) and from the reconnect in
  `__WiFiManagerClientGetOrReconnectServerPort` (after wifid's port died, the client copies the device list and adds
  each again). Our interface is attached once (`attachInterface` at the controller's start, no detach) and the driver
  has no path that publishes it again. On the PC boot of this branch the error came once to three times in each
  process that has a Wi-Fi client (UserEventAgent 1, rapportd 1, apsd 2, locationd 3), with en0 attached once and
  wifid not restarted: one per client object that starts while wifid has the device (the device list it copies and
  the attach notice name the same en0), not an interface that comes and goes. [I] Preferences' 17 are 17 such client
  objects (one per Wi-Fi view it opens); harmless.
- [I] **Preferences' own load is not shown to come from Wi-Fi events.** 724 TableView signposts over the session are
  about 60 list reloads of about 12 cells, close to the 63 scans (the Wi-Fi list redraws per scan, as on an iPhone);
  `Create send gesture actions` is UIKit's `_UIGestureRecognizerSendActions` signpost (one per gesture action: the
  typing). WiFiKit's own lines (`UI scan results updated: N networks`, `partial scan results`, `scanning did finish`)
  were not among the top messages. The password sheet's Sharing query (`SFClient wifiPasswordSharingAvailability`, 8
  answers "not available") is iOS's and not a loop at 8.

**Auto-join "disabled by user"** (`UserAutoJoinState` 0): set only by a client's XPC request (#34). Settings' Wi-Fi page
says so in words when it is: `New Wi‑Fi network connections have been turned off from Control Center.`
(WiFiKitUI's `kWFLocUserAutoJoinDisabledWarning`). Nothing of ours reports Wi-Fi off: `POWER` answers on unless a client
set it off, `STATE` follows the station, no power event is posted without a change (since this fix), and a link event
is posted only around a join. A Control Center Wi-Fi button that shows off is turned on by a tap, which sets auto-join
on, not off; the 0 comes from a tap on a button that showed on (Wi-Fi on, not joined: "disconnect until tomorrow").
**To turn it on again: open Control Center and tap the Wi-Fi button once** (it turns blue; wifid logs `Set user auto
join enabled requested by "..." enabled 1`). The Settings switch off and on, or a tap on a network, also works.

### Missing, unknown

- Seen in a session (2026-11-12): wifid takes en0, scans, lists the networks, joins an open network through the
  vendor's supplicant; frames cross both ways; the give-back at the return. [U] still: what `ASSOCIATE` carries for
  a secured network on iOS 27 (a passphrase, or the 32-byte key: the driver's log line says which), DHCP by
  configd (the bridge logs it now), the internet, a wrong password, a WPA3 network.
- [U] What the vendor stack does when system_server stops: whether the supplicant stays up with wlan0 (the bridge
  handles both: it takes the interface, or adds it), whether the Wi-Fi HAL or wificond take wlan0 down.
- [U] Whether the supplicant's callbacks reach our process (binder calls from `hal_wifi_supplicant_default` into
  `ksu`; KernelSU allows binder calls into its domain [I]). `registerCallback` fails with a line naming SELinux if
  the supplicant cannot ask our object for its descriptor.
- [U] One UI joining its network again after the return without help.
- wlan0's address is a random one when One UI is not joined, so iOS's en0 can have another address in the next
  session (a new DHCP lease). The factory address is not set by the bridge (the framework sets wlan0's address
  through the Wi-Fi HAL; the bridge does not use that HAL).
- A WPA3-only network when iOS gives the 32-byte key instead of the password: not joinable (logged).
- Enterprise (802.1X) networks, WEP, hotspot (iOS's Personal Hotspot), AWDL/AirDrop: not in the first version.
- The fork's return does not run `wifictl restore` after a kill yet (`oneos/interface.md` section 6).
- Mobile data (rmnet) as the uplink: not with this design (no NAT); not planned.

## 4. Bluetooth

### The phone [F]

- Controller: Qualcomm WCN785x, SoC name `hamilton`; DT node `bt_kiwi`, `compatible = "qcom,kiwi"` with reset and
  sw-ctrl GPIOs and five supplies (`e3q_eur_openx_w00_r09.dts` line 17875). Address: `/mnt/vendor/efs/bluetooth/bt_addr`.
- HAL: **HIDL** `android.hardware.bluetooth@1.1::IBluetoothHci/default`, init service `vendor.bluetooth-1-1-qti`
  (`vendor/bin/hw/android.hardware.bluetooth@1.1-service-qti`,
  [android_device_samsung_sm8650-common@48c66f6](https://github.com/samsung-sm8650/android_device_samsung_sm8650-common/blob/48c66f6543b31784fce4e7d23ea855897f18ce6e/proprietary-files.txt)
  line 126; running on the owner's phone). Not the AIDL HAL. It owns power, the UART and the firmware download.
- Its only client is Android's Bluetooth app, a zygote child: gone in a session, so the HAL is free [I: the HAL
  closes the controller when its client dies].
- Checked on the phone 2026-11-12: `lshal` lists `android.hardware.bluetooth@1.0::IBluetoothHci/default` and
  `@1.1::IBluetoothHci/default` (pid 1746) and `vendor.samsung.hardware.bluetooth@2.0::ISehBluetooth/default`;
  `service list` has no AIDL `android.hardware.bluetooth.IBluetoothHci`. `bt_addr` is 18 bytes of text
  (`system:bluetooth 0660`, `bluetooth_efs_file`), readable by root in init's namespace; in bringup's session of
  2026-11-12 xnuhost found the HAL 1.1 but logged `made up: /mnt/vendor/efs/bluetooth/bt_addr not read`. Cause [F]:
  the file is the 17 characters of the address and a NUL (`od -c`), and `br_bt_address` (C) took the NUL for a
  character that is not part of an address. Fixed 2026-11-12 (`hci.c`, now `hci.rs` `bt_address`: the text ends at the first NUL; a test with
  the phone's layout); not yet run on the phone.
- **Galaxy A56 (SM-A566E, Android 17, Exynos s5e8855) [F: the A56 fork's session of 2026-11-21 and our session of
  issue 73]:** the HAL is the stable **AIDL** `android.hardware.bluetooth.IBluetoothHci/default` (process
  `android.hardware.bluetooth-service.default`; `vendor.samsung.hardware.bluetooth.ISehBluetooth/default` beside it),
  registered with servicemanager on `/dev/binder`. hwservicemanager has no Bluetooth HIDL service: before the AIDL
  client, our session logged `bluetooth: no android.hardware.bluetooth@1.x::IBluetoothHci/default from
  hwservicemanager: Broken pipe` and iOS got no controller. The AIDL client of "The bridge" below is for it.

### iOS side (`drivers/bluetooth`, port of iOSPlus10 `patches/laurel/xnu/2600-laurel-bluetooth-wcn3990.patch`)

- bluetoothd's POSIX transport: kernel control `com.apple.uart.bluetooth` (`oneios_bt_ctl.c`, as is) or
  `/dev/uart.bluetooth`, `/dev/cu.bluetooth`, `/dev/btwake` (`oneios_bt_dev.c`, as is); raw H4 bytes both ways.
- `oneios_bt_h4.c` (as is): H4 framing and the answers given on the controller's behalf. bluetoothd was written for
  Broadcom chips; its vendor commands (OGF 0x3f) mean other things on a Qualcomm chip (0xfc48 baud rate, 0xfc00
  firmware download, 0xfc14 address), so each gets a Command Complete in the kernel and never reaches the WCN785x.
  The same family as the Mi A3's WCN3950, which is why the table applies unchanged [I].
- `oneios_bt_ios.c`: the transport core with its seam changed: packets go to and come from the host
  (`OB_CH_HCI`; the message type is the H4 indicator) instead of a UART. No in-band sleep bytes, no firmware, no
  rails, no speed change: the HAL does those. Power follows the client: the first client sends `OB_HCI_OPEN`, the
  host answers `OB_HCI_STATE` (ready with the address, or failed).
- `OneiOSBluetooth.cpp`: writes the address in use into the device tree's `bluetooth` nub; hides the nub when
  Bluetooth is off, so bluetoothd sees no Bluetooth instead of a transport that fails.

### The bridge (`runtime/bridges/bt`)

- `hci_hidl.rs`: a HIDL client with **no Android library** (xnuhost is static): `/dev/hwbinder` through the kernel's
  binder UAPI, `IServiceManager::get`, `initialize_1_1(callbacks)` (or `initialize` on a 1.0 service),
  `sendHciCommand/AclData/ScoData/IsoData`, and a callback object the HAL calls (events, ACL, SCO, ISO,
  `initializationComplete`, and the base methods `interfaceChain`, `interfaceDescriptor`, `ping`, ...). A death
  notification turns the state to failed. Two threads: the looper only answers the HAL's calls into us; the caller
  makes our calls (get, initialize, the sends, close) in order from a queue of up to 512 packets, so the guest's
  vCPU, which holds the bridge device's lock, never waits on a binder call, and a HAL that sends an event before its
  `sendHciCommand` returns is answered at once. No `initializationComplete` within 15 s
  (`ONEIOS_BT_INIT_TIMEOUT_MS`) is a failure with a line naming SELinux. `hci_hidl::probe()` asks for the
  service once at xnuhost's start. `ONEIOS_HWBINDER` names another binder device (the PC test).
- AIDL mode (same file, 2026-11-24; ported from the A56 fork's `bt/hci_hidl.c`, Musicalista/OneiOS-A56 `26a2276`):
  only when hwservicemanager has no HIDL HAL (the probe of the S24 Ultra, which has one, returns 11 before
  `/dev/binder` is opened, and its open and parcels are the HIDL ones as before), `probe()` asks servicemanager on
  `/dev/binder` (`ONEIOS_BINDER` names another device) with `IServiceManager.getService` (code 1) for
  `android.hardware.bluetooth.IBluetoothHci/default`, checks its descriptor (`INTERFACE_TRANSACTION`) and asks its
  version, and returns 20; `open_hidl` then speaks AIDL on the same two threads. The codes are
  FIRST_CALL_TRANSACTION + the declaration order of AOSP's `IBluetoothHci.aidl` and `IBluetoothHciCallbacks.aidl`
  (frozen version 1, `aidl_api/android.hardware.bluetooth/1`, checked against AOSP main 2026-11-24): `close` 1,
  `initialize` 2, `sendAclData` 3, `sendHciCommand` 4, `sendIsoData` 5, `sendScoData` 6; callbacks
  `aclDataReceived` 1, `hciEventReceived` 2, `initializationComplete` 3 (`Status`, an int: 0 = SUCCESS),
  `isoDataReceived` 4, `scoDataReceived` 5. Our callback object (`aidl::Parcel::put_local`: VINTF stability 0x3f, as
  a VINTF HAL requires) also answers the descriptor, ping, `getInterfaceVersion` (1) and `getInterfaceHash`
  (`f0eb2eb6...`, version 1's). The token's header is `SYST` (a stable-AIDL HAL on libbinder_ndk runs the system
  libbinder, an LLNDK library); a HAL that refuses it (BAD_TYPE or PERMISSION_DENIED) gets `initialize` once more
  with `VNDR`, and every later call uses the header it took. A send parcel may be 64 KiB (`aidl::Parcel::set_limit`;
  a byte[] is in the parcel, not a scatter buffer). Parcels: `bt/aidl_parcel.rs`. The fork's version and hash codes
  (0x00fffffe, 0x00fffffd) were off by one; ours are libbinder's (0x00ffffff, 0x00fffffe). Log lines:
  `bluetooth: the AIDL HAL android.hardware.bluetooth.IBluetoothHci/default (version N) on servicemanager: no HIDL
  HAL on hwservicemanager (WHY)` at the probe, xnuhost's `bluetooth: on: the phone's controller through its HAL
  (AIDL), address ...`, then at bluetoothd's open `bluetooth: HAL AIDL (version N, token header SYST) found,
  initialising ...` and `bluetooth: HAL initialisation complete: the controller is on (status 0)`. With neither HAL:
  one line naming both, `bluetooth: no Bluetooth HAL: no android.hardware.bluetooth@1.x::IBluetoothHci/default from
  hwservicemanager (WHY), no android.hardware.bluetooth.IBluetoothHci/default from servicemanager (WHY)`.
  `bluetooth=hidl` (no probe) always opens the HIDL HAL: on an A56 use the default `bluetooth=auto`. Host-tested
  (`test_hidl_binder`'s AIDL scenarios, below); not run on an A56.
- `devices/bridge_dev.rs`: an `OB_HCI_OPEN` while the controller is failed (the HAL died or never finished) closes it and
  opens it again, so a new bluetoothd client starts over.
- `hidl_parcel.rs` (on `hal/hidl.rs`, `hal/binder.rs`): the parcels (interface token, `hidl_string`, `hidl_vec<uint8_t>` as scatter-gather buffer
  objects, strong binders). Unit-tested byte for byte; the transaction codes are the HIDL order of the methods.
- `hci.rs`: packet length checks both ways, the state, the address file. It logs `bluetooth: the controller hears
  devices: N report(s) so far (LE advertising|inquiry result)` for the first report and for each power of two after
  it: a scan of bluetoothd's that finds devices can be read in xnuhost's log (2026-11-12; not yet seen on the phone).

### First session with the HAL bridge on (prelink's session 2, 2026-11-12) and what 24A446's bluetoothd opens

- xnuhost: `bluetooth: on: the phone's controller through its HAL 1.1, address a4:a4:90:4a:53:2f`: the real address
  (the fix of `81c4e5d` works). Guest: `ONEIOS BT: on: ...`. bluetoothd ran. **Nothing opened the transport**: no
  `ONEIOS BT: ios: client ... connected` line, so the HAL was never opened either.
- **Why [F, 24A446's `/usr/sbin/bluetoothd`, disassembled]:** this bluetoothd does not read and write a serial node.
  Its transports by `transport-encoding`: 0 USB, 1 H4, 2 H4DS, 3 H4BC, 4 H5, 5 BCSP (refused), 6 APPLEBT, 7 PCIE
  Skywalk, 8 PCIE SRS, 9 APPLE PCIE, 10 virtual (the boot argument `bt-transport-override` replaces it). Its UART
  transport opens `/dev/btwake`, then the **kernel control `com.apple.uart.sk.bluetooth`** (`CTLIOCGINFO`,
  `connect`), sets a channel specification and asks for a channel UUID through it (`IOAOSSKYSETCHANNELSPEC`,
  `IOAOSSKYGETCHANNELUUID`), and moves the bytes over a **Skywalk channel** (`os_channel_create`, `os_channel_sync`;
  its lines say `BM3UART`). `/dev/cu.bluetooth` and `/dev/uart.bluetooth` are only looked at in `main` (do they
  exist: "is there Bluetooth on this device"), and that look is skipped in a virtual machine. The driver here offers
  the transports of iOS 16 (the control `com.apple.uart.bluetooth` and the two serial nodes), which this bluetoothd
  never opens. UniXNU's port of the same OS build carries H4 to bluetoothd through that Skywalk pipe ("UART.SK",
  [UniXNU/QcaBluetooth](https://github.com/UniXNU/QcaBluetooth) `qbt_transport`), with `transport-encoding` 1.
- So Bluetooth needs the Skywalk UART pipe in the guest, with the host bridge under it as it is. **Written
  2026-11-12, not yet run**: `drivers/bluetooth/oneios_bt_sk.c`, after UniXNU/QcaBluetooth `986d631`
  `src/qbt_transport.c` (APSL-2.0), which carries H4 to this same bluetoothd: the kernel control
  `com.apple.uart.sk.bluetooth` (options `IOAOSSKYSETCHANNELSPEC` 0x800c5414, the spec 0x400c5413, the channel UUID
  0x40105412, the 64-bit termios 0x80487414 and 0x40487413), a kernel pipe nexus bound to the client's pid, and the
  two sync callbacks: bluetoothd's packets go into an 8 KiB FIFO that the core's pump thread hands to the H4 core
  (framing, the answers on the controller's behalf, the bridge to the host), and the core's queued bytes fill the
  receive ring. Connecting the control is the core's "client connected": the host opens the HAL then.
  `transport-encoding` is 1 now (plain H4, what UniXNU gives this bluetoothd over the pipe; bluetoothd's boot
  argument `bt-transport-override=3` tries H4BC).
- **What is not known [U]:** how far the unchanged bluetoothd goes on a Qualcomm controller. UniXNU does not run
  it unchanged: its adapter (`native-ios27.md`) patches this very binary so that every product gets the plain
  controller type with no vendor operations, the virtual machine check says no, scans use the extended commands
  (0x2041, 0x2042: the WCN7850 refuses the legacy ones once extended advertising was used), and capabilities follow
  the controller's own reports. Here the kernel answers the vendor commands bluetoothd sends (the table of
  `oneios_bt_h4.c`, made for iOS 16) and `/defaults/vmm-present` makes bluetoothd skip its chip initialisation. The
  first boot's `ONEIOS BT: bluetoothd: command 0x....` lines say which command it stops at.
- Expected lines, in order, when Bluetooth is turned on in iOS (since 2026-11-13 after `ONEIOS BT: transport up N ms
  after the start: ...` at the root mount, and bluetoothd's `oneios-iokit:` lookups of 0062): `ONEIOS BT: dev:
  bluetoothd[N] opens /dev/btwake: 0`, `oneios-iokit: bluetoothd[N] kernel control "com.apple.uart.sk.bluetooth" -> id
  N`, `sk: bluetoothd[N] connects to com.apple.uart.sk.bluetooth: 0`, `ios: client Skywalk pipe connected`, xnuhost
  `bluetooth: HAL 1.1 found, initialising`, `bluetooth: HAL initialisation complete`, `ONEIOS BT: host: controller
  ready, address ..`, `sk: channel of 2048-byte buffers, N/N slots for pid N: 0`, `sk: bluetoothd's channel is
  connected`, `bluetoothd: command 0x0c03` (HCI Reset), `controller: Command Complete for 0x0c03, status 0x00`, and
  with a scan xnuhost's `bluetooth: the controller hears devices: N report(s)`.
- The driver now logs who opens which node and how it went (`ONEIOS BT: dev: NAME[PID] opens /dev/...: error`), the
  first 40 commands of a client with the ones it answers itself (`bluetoothd: command 0x....`), and the controller's
  first 40 events (`controller: Command Complete for 0x...., status ..`).

### bluetoothd finds our transport and never powers the stack (fixbt, 2026-11-13)

- **The phone (2026-11-12 23:45, main ea06d46, and the Mac session of 2026-11-13 10:5x, main c1223f8):** only the
  `on` lines of xnuhost and the guest; no `ONEIOS BT: dev:`, `sk:` or `ios:` line, so bluetoothd never opened even
  `/dev/btwake`, the first step of its UART transport's open; Settings' Bluetooth switch does not turn on.
- **On the PC with a stand-in controller (`bluetooth=sim`, hosted 0062, 2026-11-13, proven):** bluetoothd finds
  our node and picks our transport. Kernel (`oneios-iokit:`): `match {IONameMatch=bluetooth ...} -> IOPlatformDevice
  "bluetooth"`, `property "local-mac-address" (IODeviceTree, recursive) ... -> found`, later `property
  "transport-encoding" (IODeviceTree, recursive) of IOPlatformDevice "bluetooth" -> found`, the key store and
  `IOPMrootDomain` opened, `/product` read (`boot-ios-diagnostics`, `product-id`: not found). iOS's log
  (`oneios-tool ioslog show data.img --proc bluetoothd`): `marconi-bt: Unable to match bluetooth service in device
  tree` (it asks for `marconi-bt` first, then `bluetooth`), `Bluetooth is running inside a Virtual Machine, so
  skipping check for Bluetooth available`, `DeviceTree transport = 0x00000001`, `HCI Transport is set to H4`,
  `Warning: BT driver=0x...`. So the device-tree side is right: H4 is the transport whose driver ("POSIX" in its
  strings) opens `/dev/btwake` and `com.apple.uart.sk.bluetooth` (`oneios_bt_sk.c`).
- **What does not happen (PC, proven; phone, the same lines missing):** bluetoothd opens its transport only when it
  starts its stack at power on; its log lines for that are `Beginning asynchronous power init`, `Default power state
  : N`, `Beginning asynchronous power on`, `Bluetooth is running inside a Virtual Machine, so skipping chip
  initialization`, `Starting up StackManager`, `Starting bluetooth core stack`. None of them came in 13 minutes of
  bluetoothd on the PC (nor in the PC boot of 2026-11-12, Bluetooth off), and its own line says `fStackStarted:0`
  (and on a fresh Data store `BuddyHasNotCompleted:1`). [I] bluetoothd's power manager is never started, so nothing
  reaches the transport; what starts it is not known yet. Not the transport, the node or the HAL: those are found or
  never asked for.
- iPhone18,2's own device tree (`devicetree.bin` of the kernel inputs): `/arm-io/bluetooth` with
  `transport-encoding` 9 (APPLE PCIE) and `local-mac-address` from syscfg; ours is `/bluetooth`, encoding 1, which
  bluetoothd reads as above.
- **PC stall, not fixed (corrected 2026-11-13 afternoon):** some qemu boots never get their root device: APFS's start
  waits for the `IOBSD` resource while `IOResources` stays busy (AppleImage4, AMFI and AppleLockdownMode wait in their
  starts for services that come later: `IOAESAccelerator`, `sep-endpoint,scrd`/`AppleCredentialManager`,
  `AppleSEPManager`), with `/defaults` and the APFS media busy in the `oneios-iobusy:` lines; seen with
  `hosted_dump_threads=1`. With the Bluetooth driver on: 3 stalls in 3 boots before the change below, then 1 boot that
  mounted, then the same day 1 stall and, in a retry loop, 1 that mounted. With `oneios_bt=0` or no bridge: 2 of 2
  booted. So the driver's transport now starting after the root mount (`ONEIOS BT: transport up N ms after the start:
  ...`) is kept (nothing needs it earlier), but it did not remove the stall; [I] a timing race in the kernel's
  early matching that the driver start's timing makes likelier. The phone never showed it. For a PC test: boot again
  when `Waiting on <dict ...BBA627F7...>` is not followed by `BSD root` within about 90 s.

### The phone's iOS log (Mac session 2026-11-13 10:5x, main c1223f8) and what is next

- bluetoothd, in this order: `marconi-bt: Not a valid MAC address (all 00s)`, `marconi-bt: Unable to match bluetooth
  service in device tree`, `DeviceTree transport = 0x00000001`, `HCI Transport is set to H4`, `Warning: BT driver=...`,
  **`Beginning asynchronous power init`**, `Bluetooth is running inside a Virtual Machine, so skipping chip
  initialization`, then 32 times `Power state changed. PowerState:3 managerState:2` and 55 times `Returning bluetooth
  power state: FFFFFFFFDEADBEEF`. SpringBoard's and Preferences' sessions: `is turning power on for module Bluetooth`,
  `GetControllerInfo: BlSt PoweredOff, Chip APPLE_VIRTUAL, HCIT UART`, `fStackStarted:0`. No `/dev/btwake` open, no
  StackManager line. On the phone power init begins (it did not in the PC boots, which never got that far: the PC's
  SpringBoard did not ask within the runs) and never finishes.
- **The all-zero MAC is not ours [F]:** bluetoothd asks for a service named `marconi-bt` first (an Apple chip's node we
  do not have), logs zeros for it, then asks for `bluetooth`. On the PC the same two `marconi-bt:` lines come, and
  0062 shows `property "local-mac-address" (IODeviceTree, recursive) of IOPlatformDevice "bluetooth" -> found`, with no
  `bluetooth: Not a valid MAC address` line. The phone's next boot's `oneios-iokit:` lines confirm it.
- **Power init fails on the virtual-machine path [F since 2026-11-15, read in 24A446's bluetoothd: next section]:**
  with `vmm-present` bluetoothd takes chip `APPLE_VIRTUAL`, and its chip initialisation in a VM succeeds only when the
  transport is 10; with H4 (1) it fails and power init sets PowerState 3, the line above.
- **Tried and dropped (2026-11-14): hiding the VM from bluetoothd** with `owner-hide-vmm=bluetoothd,BlueTool` (the
  kernel's per-process `kern.hv_vmm_present` switch of Droid-VM-UniXNU's `zzyd-owner-hide-vmm.patch`). bluetoothd then
  leaves the virtual-controller path and **crashes at its start, in a loop** [F]: on the phone (main 9f6dc38) 18 pids in
  one boot, then launchd's 1200 s throttle; on the PC (`bluetooth=sim`) the same, a new pid every few seconds, each one
  right after its first device-tree reads. Without the hide (c1223f8, and every PC boot before) it runs as one pid.
  The non-VM path expects Apple's own Bluetooth chip and its bring-up (UniXNU runs this bluetoothd only with a patched
  binary and its own library, `native-ios27.md`); the project does not change Apple's binaries, so **the VM path stays**
  and its power init is what to fix. Never put bluetoothd in `owner-hide-vmm`: no default does, and xnuhost now says
  so in its log when the boot arguments do (`boot arguments: owner-hide-vmm names bluetoothd: ...`).
- **Tried on the phone (main 3233fed, 2026-11-15) and a dead end: the VM path's own transport**,
  `bt-transport-override=10`. The virtual transport is a simulated controller inside bluetoothd's own process, not a
  service to publish: next section. Do not use it; xnuhost now says so in its log when the boot arguments carry it.
- **0062 now also says how a process saw the VM:** `oneios-iokit: bluetoothd[N] sysctl kern.hv_vmm_present -> 1 (a
  virtual machine)` or `-> 0 (the virtual machine hidden by owner-hide-vmm)` (kernel.kc bc81badb, built, not yet in a
  boot).
- **What the next phone boot must check:** in xnuhost.log the `oneios-iokit: bluetoothd[...]` and `oneios-iokit:
  BlueTool[...]` lines (what it looks up, whether a `kernel control "com.apple.uart.sk.bluetooth"` lookup ever comes);
  in iOS's log `oneios-tool ioslog show data.img --proc bluetoothd --grep "power|Power|chip|Chip|Virtual|BlueTool|OLYHAL|HWIdentifiers|StackManager|core stack|transport|MAC"`
  and `--proc BlueTool`.

### The virtual transport is bluetoothd's own (fix-bt-virtual, 2026-11-15)

The phone session of main 3233fed (2026-11-15) had `bt-transport-override=10` and no `owner-hide-vmm`: no crash loop,
but Bluetooth never powered on. bluetoothd[73] `exited due to exit(1), ran for 56223ms`, then pids 168, 190, 245; 42
`Returning bluetooth power state: FFFFFFFFFFFFFFFF`, 17 `... FFFFFFFFCAFEBABE`, 51 `manager state not powered on`; 128
`BTVirtual transport send packet type: 0x00000001`, 128 `-[BTVCPacketControl didSendPacket:error:] no completion
function`, 116 `send HCI event to virtual transport size 6`; no `.ips`. Read in 24A446's `/usr/sbin/bluetoothd` on the
PC (`oneios-tool image apfs-copy` from `os.img`, `llvm-objdump -d`, the `adrp`/`add` pairs matched to its strings; the
addresses are the binary's, unslid):

- **Transport 10 never leaves bluetoothd's process [F].** `BTVirtualTransportRegisterCBs` (0x1003c42a0) stores five
  callbacks, and `BTVirtual transport init` (0x1003c4384) creates its HCI, ACL and LMP transports through them. Its
  only caller (0x100899d78, logged `registerCallbacks`) hands it bluetoothd's own `TransportCreate`, `TransportFree`
  and `TransportWrite` (0x100899e00 on); `TransportWrite` (0x10089a3d4) sends the packet, by `objc_msgSend`, to a
  `BTVCHciManager` that bluetoothd allocates itself (0x100899d2c): `-[BTVCHciManager
  transportWriteCmd:length:amountWritten:dtor:]`. That object is a simulated LE controller ("BTVC", sources
  `MobileBluetooth_base/Server/Platforms/Darwin/VirtualController/`): `handleHciCmd: opCode 0x%04X`,
  `handleHciResetCmd`, `handleHciReadLocalVerInfoCmd`, ... `handleHciLeExtendedCreateConnectionCmd`, and its events go
  back with `%s send HCI event to virtual transport size %d`. Its "radio" is Bonjour (`_btvc._tcp`,
  `BTVCBonjourService`) to other virtual devices. So there is no IOKit service, kernel control or XPC service to publish:
  the 128 commands and the 6-byte events (a Command Complete with only a status: Reset, Set Event Mask, LE Set Event
  Mask, the filter accept list, ...) are bluetoothd talking to itself, and nothing reaches `drivers/bluetooth`, the
  bridge or the phone's controller. A boot with it has no `oneios-iokit: bluetoothd[...] kernel control
  "com.apple.uart.sk.bluetooth"` line and no `ONEIOS BT: dev:`, `sk:` or `ios:` line.
- **"no completion function" is not a missing answer [F].** `-[BTVCPacketControl didSendPacket:error:]` (0x1006f08xx)
  logs `%s completion function` and calls the block when the packet was queued with one, and `%s no completion
  function` when it was queued with none: BTVC's own pacing of its own packets. Nothing outside is asked for a
  completion, a credit or a Number of Completed Packets.
- **In a VM only transport 10 powers on [F].** The chip initialisation of power init (0x10062941c) asks whether it runs
  in a VM; if so it logs `Bluetooth is running inside a Virtual Machine, so skipping chip initialization` and returns
  `transport == 10` (0x1006294b4: the transport is the device tree's `transport-encoding`, or the boot argument's,
  kept at 0x100c1ce28 by 0x10030901c). Power init (0x100628730) sets PowerState 3 when that is false. So with
  `/defaults/vmm-present` and H4 (1), power init always fails before any transport is opened: the c1223f8 session's
  `PowerState:3 managerState:2` and no `/dev/btwake` open. The VM answer is `kern.hv_vmm_present`, read once and kept
  (0x10056a164); four places use it: `main`'s check for `/dev/cu.bluetooth` (skipped), the chip factory (chip
  `APPLE_VIRTUAL`), this chip initialisation, and CloudPairing.
- **So an unchanged 24A446 bluetoothd cannot reach the phone's controller while it sees a VM**: transport 10 is its own
  simulation, every other transport fails power init. With the VM hidden (`owner-hide-vmm`) it crashes at its start
  (above). [I, from the code and the crash's address] The chip factory (0x10050a08c) picks the chip by the environment
  (`BT_CHIP=ACI BT`: chip 2000), by transport 9 (APPLE PCIE: chip 3000), by the VM (chip 6000, `APPLE_VIRTUAL`), or by
  the product's type (`MGIsDeviceOneOfType`, a table of products); none matches transport 1 on iPhone18,2 (whose own
  tree says transport 9, so it is not in the table), the factory logs the fault `Unknown platform 0x%08x detected` and
  leaves the chip null, and the chip's field at 0x350 (read right after `getChip()`, 0x1000071f0, in many places) is
  the `SIGSEGV at 0x350` of the 9f6dc38 session. The fault line in that session's iOS log would prove it.
- **The exit(1) [F for the paths, U for which one].** bluetoothd calls `_exit(1)` in two places: its fatal-error path,
  which first logs `Bluetooth error - restarting { build=release, reason=%d, context="%s" }`, and the chip
  initialisation's restart (`timed out booting the chip, restarting`, `hardware is unhealthy, restarting`, `Unknown
  platform/chipset/manufacturer combination, restarting`). Fatal errors with BTVC's code among their callers: `Filter
  Accept List add error`, `... remove error`, `... clear error`, `Policy enforcement timeout`, `BT VSC init failed`.
  Which one ended pid 73 after 56 s is in 3233fed's iOS log: `oneios-tool ioslog show data.img --proc bluetoothd
  --grep "Bluetooth error - restarting|restarting|Filter Accept List|Policy enforcement|VSC init|Unknown platform"`.
  It does not matter for the controller: transport 10 is not a way to it.
- **Nothing of ours stays busy for the next bluetoothd [F, from the code].** In a transport-10 boot nothing of ours is
  opened (no control, no node; the HAL stays closed). With H4 a client's disconnect (`sk_ctl_disconnect`,
  `ctl_disconnect`, the nodes' close) frees the core's client slot, destroys the Skywalk pipe and closes the HAL
  (`obt_ios_release`); a new pid connects to a free slot, and a failed controller is opened again by the bridge.
- **Fixed in our code:** (1) `oneios_bt_ios.c`'s pump thread woke every 20 ms from the root mount to the end of the
  session with no client and nothing queued (50 wakeups a second of a vCPU, in every session, and with the VM
  visible bluetoothd never connects). It now sleeps until it is kicked (a packet for the client, bluetoothd's bytes in
  the Skywalk FIFO, `ctl_rcvd`'s room again, which kicks it now) and looks every 20 ms only while bytes wait for the
  client. (2) xnuhost logs `boot arguments: bt-transport-override=10 gives bluetoothd its own simulated controller, in
  its own process: nothing reaches the phone's controller (docs/hardware.md 4); take it out` (read as bluetoothd reads
  it: any word that contains the name, the value after its `=`, under 13, the last word wins; host-tested).
- **PC boot (2026-11-15, kernel.kc 7261a00b from xnu.macho 948bb412 with both fixes; `--set bluetooth=sim`,
  `bt-transport-override=10`; stopped after 27 minutes):** xnuhost logged the new `boot arguments:
  bt-transport-override=10 ...` error line; the root mounted on the first try; `ONEIOS BT: on: ...` and `ONEIOS BT:
  transport up 2620 ms after the start: ...` as before. bluetoothd[75] (one pid for the whole run): `transport override
  from boot-arg: 10`, `HCI Transport is set to virtual`, `fStackStarted:0`; its `oneios-iokit:` lines are the device
  tree (`marconi-bt`, `bluetooth`, `local-mac-address`, `transport-encoding`), the key store, `IOPMrootDomain`,
  `/product` and `kern.hv_vmm_present -> 1`, with **no kernel control lookup** and no `ONEIOS BT: dev:`, `sk:` or
  `ios:` line. As in every PC boot so far nothing asked bluetoothd to power on (no `Beginning asynchronous power init`),
  so neither a power-on nor the phone's exit(1) can be seen on the PC; a power on would run BTVC, not the stand-in
  controller.
- **What would reach the controller (2026-11-15) [superseded 2026-11-23]:** a chip for transport 1 outside the VM
  (`BT_CHIP=ACI BT` with the VM hidden, or transport 9). Not needed: the next section keeps the VM and transport 10 for
  the power init and moves only the HCI transport, by bluetoothd's own `BT_HCI_TRANSPORT`.
- **The next phone session (2026-11-15):** take `bt-transport-override=10` out of `xnuhost.conf` (done; the
  2026-11-22 sessions have none) and keep `owner-hide-vmm` without bluetoothd.

### Bluetooth powers on: transport 10 for the power init, `BT_HCI_TRANSPORT` for the HCI (fix-ios-bluetooth-power, 2026-11-23)

The owner's request (2026-11-22): Bluetooth must work in iOS sessions; it is also the first thing AirDrop needs
(section 4a). **Fixed in our code; host-tested and built, not yet booted on the phone.**

**What the sessions of 2026-11-22 show [F]** (iOS's logs `out/session-logs/2026-11-22/s8-ioslog/ioslog.txt` and
`s9b-ioslog/ioslog.txt`, xnuhost's `kbd-debug/xnuhost-s9.log`):

- The host side works. xnuhost: `bluetooth: on: the phone's controller through its HAL 1.1, address
  a4:a4:90:4a:53:2f` (the HAL is the HIDL 1.1 one of section 4, so the AIDL client ported from the A56 fork on
  2026-11-24 is never reached on this phone). Guest: `ONEIOS BRIDGE: on: ... channels 0x77 (... 4 Bluetooth)`, `ONEIOS BT: on: ...`, `ONEIOS BT:
  transport up 300 ms after the start: control com.apple.uart.sk.bluetooth ...`.
- bluetoothd never opens it. bluetoothd[75]: `DeviceTree transport = 0x00000001`, `HCI Transport is set to H4`,
  `Beginning asynchronous power init`, `Bluetooth is running inside a Virtual Machine, so skipping chip
  initialization`, then 11 times `Power state changed. PowerState:3 managerState:2` (34 times in s9b) and
  `GetControllerInfo: BlSt PoweredOff, ... Chip APPLE_VIRTUAL, ... HCIT UART`, `fStackStarted:0`. Its 0062 lines end
  at `kern.hv_vmm_present -> 1` and `transport-encoding -> found`: no `kernel control "com.apple.uart.sk.bluetooth"`
  lookup, no `ONEIOS BT: ios:`, `sk:` or `dev:` line. sharingd: `AirDrop not ready: wifi=..., bluetooth=...`.
- That is the failure the section above explains: in a VM the chip initialisation returns `transport == 10`, so with
  H4 (1) power init sets PowerState 3 before any transport is opened.

**The way out, read in 24A446's `/usr/sbin/bluetoothd` [F]** (`oneios-tool image apfs-copy out/image/24A446/os.img
/usr/sbin/bluetoothd`, `llvm-objdump -d`, `ipsw macho dump` and `ipsw class-dump --re`; unslid addresses):

- The transport number (0x100c1ce28, read through 0x1000aa348) is compared with 10 in two places only: the chip
  initialisation of power init (0x1006294b8) and the HCI transport choice (0x10036f9ac, a switch: 1 H4, 2 H4DS, 3
  H4BC, ..., 10 virtual). Its other readers test 3, 6, 7, 8 or 9, or report it (`setHciTransportType:`, the
  controller info's `HCIT`).
- The HCI transport choice (0x10036f8ac) first calls `getenv("BT_HCI_TRANSPORT")`. When it is set, it is split at
  `:` (`strtok`): the first name is looked up in the transport table (0x100ba7c28: `USB`, `H4DS`, `H4BC`, `H4`, `H5`,
  `APPLEBT`, `VIRT`, `PCIE`, `PCIESW`, `PCIESUN`, `APPLEPCIE`), the second in the driver table (0x100ba7c90: `USB`,
  `POSIX`, `APPLEBT`, `VIRT`, `PCIE`, `PCIESW`, `PCIESUN`, `APPLEPCIE`; with no second name the first is looked up
  there), a third is kept as the driver's device path (`strdup`). The number is not read on that path.
  `H4:POSIX` gives exactly the pair the device tree's 1 gives (transport `H4` 0x100c1c460, driver `POSIX`
  0x100c1c360, no device path): the driver that opens `/dev/btwake` and the kernel control
  `com.apple.uart.sk.bluetooth`. A name it does not know logs `FATAL ERROR - HCI Transport %s could not be matched`.
- bluetoothd reads four environment variables: `BT_HCI_TRANSPORT`, `BT_CHIP`, `BT_BAUDRATE`, `BT_DEVICE_ADDRESS`
  (and `HOME`). Its launchd job (in the sealed root's `System/Library/xpc/launchd.plist`) sets only
  `OS_ACTIVITY_ENABLE_DYNAMIC`, so the variable has to come from the kernel.
- So bluetoothd keeps chip `APPLE_VIRTUAL` (the VM) and transport number 10 (power init passes), and talks H4 to our
  transport. What `APPLE_VIRTUAL` sends after that was written against BTVC, bluetoothd's own virtual controller
  (`-[BTVCHciManager handleHciCommand:params:length:]` 0x100467418): BTVC handles the standard commands one by one,
  0xfc06 (Write BD_ADDR) and three Apple vendor commands (0xfeb0, 0xfeff, 0xffc7: filter accept list, LE IRK, its
  test), and answers **every other command, vendor commands included, with Command Complete, status 0, no return
  parameters** (`handleHciCmdDefault:` 0x1004676f4 -> `_sendHciCommandCompleteEvent:status:params:length:` with 0, 0,
  0). Vendor commands never reach the phone's Qualcomm controller (`oneios_bt_h4.c`), so in this mode the kernel answers
  them as BTVC does.

**The fix (kernel only; xnuhost still publishes 1, the loader is unchanged):**

- `drivers/bluetooth/oneios_bt.c` (`hci_env`), when the host serves a controller: the nub's `transport-encoding` 1 ->
  10 (`OneiOSBluetooth.cpp` `oneios_bt_adt_transport`), `oneios_bluetoothd_env` = `BT_HCI_TRANSPORT=H4:POSIX`
  (`oneios_bt_env.h`, which checks a value against bluetoothd's two tables), and every new client's answers in the
  BTVC mode (`obt_shim.vsc_empty`: each vendor command Command Complete, status 0, no parameters). One line:
  `ONEIOS BT: bluetoothd: transport-encoding 1 -> 10 (its power init in a VM) and BT_HCI_TRANSPORT=H4:POSIX at its
  start (...)`.
- `kernel/patches/oneios-hosted/0087-bluetoothd-hci-transport.patch` (`kern_exec.c`): at each exec of
  `/usr/sbin/bluetoothd` the variable is added to its environment, unless its environment names `BT_HCI_TRANSPORT`
  already (then that one is kept and said). One line per exec: `oneios-bt: bluetoothd gets BT_HCI_TRANSPORT=H4:POSIX`.
- Boot argument `oneios-bt-hci`: `0` is the old way (transport-encoding as published, no variable, the iOS 16 answer
  table); `TRANSPORT:DRIVER[:/dev/...]` tries another pair of bluetoothd's (for example `H4BC:POSIX`); a value it would
  not match is replaced by the default and said so.
- xnuhost: `bt-transport-override=N` with N other than 10 now gets the error line (power init fails), 10 an
  information line (it is what the driver gives already).
- Kept as they were: the HAL bridge, the Skywalk pipe, `/defaults/vmm-present`, `owner-hide-vmm` without
  bluetoothd. A kernel with 0087 and an older runtime work together (xnuhost's tree is not changed); a kernel without
  0087 is the state before.

**Tested:** `drivers/tests/host.sh` (new `bluetooth/test_bt_env.c`: the variable for each boot-argument value, the
names bluetoothd would refuse, the longest value; the vendor answers in both modes); the kernel build and prelink
(`kernel/patches/oneios-hosted/README.md`, "0087": `out/kernel/bt-24A446/kernel.kc`, sha256 `68b6317f2bc96f56...`, audit 0
failures, stubs-check 666 good); `runtime/host-tests.sh` (xnuhost's warning). Not booted on the phone or the PC yet.

**What is not known [U]:** how far bluetoothd's stack gets on the real controller after power on. The first boot's
`ONEIOS BT: bluetoothd: command 0x....` lines (the first 40 of a client, `answered here` for the vendor ones) and
iOS's log say where it stops; `Bluetooth error - restarting { ... reason=%d ... }` would be a fatal error of the
stack (for example `Filter Accept List add error`, `BT VSC init failed`).

**The phone check** (stage `kernel.kc` and `kernel.kc.hashes`; nothing else changes):

1. `xnuhost.log`, in this order: `ONEIOS BT: on: ...`, `ONEIOS BT: bluetoothd: transport-encoding 1 -> 10 ... and
   BT_HCI_TRANSPORT=H4:POSIX ...`, `oneios-bt: bluetoothd gets BT_HCI_TRANSPORT=H4:POSIX`, `oneios-iokit:
   bluetoothd[N] kernel control "com.apple.uart.sk.bluetooth" -> id N`, `ONEIOS BT: ios: client Skywalk pipe
   connected`, xnuhost `bluetooth: HAL 1.1 found, initialising` and `bluetooth: HAL initialisation complete`,
   `ONEIOS BT: bluetoothd: command 0x0c03` (Reset), and with a scan `bluetooth: the controller hears devices: N
   report(s)`.
2. `oneios-tool phone pull-log DIR`, then `oneios-tool ioslog show DIR/... --proc bluetoothd --grep
   "Searching for|HCI Transport|power|Power|StackManager|core stack|BlSt|restarting|FATAL"`: `Searching for transport:
   "H4"`, `Searching for driver: "POSIX"`, no `PowerState:3`, `Starting up StackManager`, `Starting bluetooth core
   stack`, `GetControllerInfo: BlSt PoweredOn`, `fStackStarted:1`; a BLE scan (`LeDeviceManager` scan lines, devices
   found).
3. Settings > Bluetooth shows on and lists devices nearby; Control Center's AirDrop toggle switches (sharingd's `AirDrop
   not ready` no longer for Bluetooth; AirDrop itself still needs `awdl0`, section 4a).

### iOS 16.5's bluetoothd on the Galaxy A56: no chip without a product (fix/a56-ios16-faults, 2026-11-24)

**The fault [F]** (OneiOS issue 73, the A56's boot of 2026-11-24 21:47, v0.1.24): 29 bluetoothd crashes, one every
~10 s. Each start: `oneios-bt: bluetoothd gets BT_HCI_TRANSPORT=H4:POSIX`, `match {IONameMatch=marconi-bt ...} -> none`
(an Apple chip's node; bluetoothd then asks for `bluetooth` and finds ours), `local-mac-address -> found`, `property
"product-id" ... of IOPlatformDevice "product" -> not found`, then `trap ... far 0x260 esr 0x92000005 ... pc
0x100...cf0 lr 0x100...cf0` and `Process[N] crashed: bluetoothd`. The host side was up (`bluetooth: on: the phone's
controller through its HAL (AIDL)`).

**The cause [F, read in 20F66's `/usr/sbin/bluetoothd` on the build laptop: `oneios-tool image apfs-copy`,
`llvm-objdump`, `ipsw macho disass`; unslid addresses]:** the crash is `ldr w23, [x0, #0x260]` at 0x100274cf0 (the only
instruction at a page offset 0xcf0 that reads 0x260 off a register, right after a call, so lr = pc), on the result of
the chip singleton's getter (0x10029a9d4). Its `dispatch_once` block (0x10029aa14) is the chip factory: `BT_CHIP`
(only `ACI BT`: chip 2000), then dozens of `MGIsDeviceOneOfType` lists; nothing else (16.5's factory has no VM chip,
unlike 24A446's `APPLE_VIRTUAL`). MobileGestalt knew no product: 16.5 finds it by `/product product-id`, which our tree
did not have (docs/userland.md, "The iOS 16.5 identity"). So the factory logged "Unknown platform", the chip stayed
null and the first reader of `[chip + 0x260]` died. The `marconi-bt` miss is not it: bluetoothd falls back to
`bluetooth`, as 24A446's does.

**The fix:** xnuhost gives a 20F66 image set the iPhone X's `product-id` (the runtime alone). With it the factory makes
the iPhone X's chip (15: the list at 0x10029b110 holds that id).

**What comes next is not known [U]:** chip 15 is a Broadcom chip, and 16.5's power init has no VM path: it asks
BlueTool (`com.apple.BlueTool`, XPC `RunBuiltinScript`) to boot the chip with its Broadcom script, over the transport
bluetoothd chose. Our transport answers Broadcom vendor commands in the kernel (`oneios_bt_h4.c`, and in 0087's
default mode `vsc_empty`), so the script may pass; if it does not, bluetoothd logs `BlueTool failed to run %s script
with result %ld` and restarts (its `HWRepair` / `BlueToolBTResult` setting overrides the result, read at 0x100271eb0,
only behind a check not followed here). 0087's `transport-encoding` 1 -> 10 (written for 24A446's VM path) is what 16.5
reads too; `oneios-bt-hci=0` gives it 1 and the iOS 16 answer table instead (the A/B switch). The owner's iOSPlus10
(iOS 16.7) ran bluetoothd with a BlueTool stand-in that answers `result 0` (its `docs/bluetooth-plan.md` 4.1): the
next step here if the A56's log shows BlueTool failing. **The proof lines** (the A56's next boot): `property
"product-id" ... -> found`, no `trap ... far 0x260`, then `oneios-iokit: bluetoothd[N] kernel control
"com.apple.uart.sk.bluetooth" -> id N` and `ONEIOS BT: ios: client ...` if it opens the transport, and in iOS's log
(`oneios-tool phone pull-log`, `ioslog show ... --proc bluetoothd --grep "BlueTool|Unknown platform|HCI Transport|
PowerState|restarting"`) which of the two it was.

### Missing, unknown

- [U] **SELinux**: hwservicemanager checks `find` for the caller's context, and the HAL's domain must be allowed to
  call back into ours (`binder { call transfer }`). KernelSU's domain is allowed most things; if the callback is
  denied, a `ksud sepolicy` rule is needed. `bridgectl bt` shows which step fails.
- [U] the transaction codes and the callback handshake against the real libhwbinder and the QTI HAL. Tested on the PC
  (the C's `make -C runtime/bridges test-binder`, 2026-11-11; now `cargo test -p onebridges --test test_hidl_binder`): a fake hwservicemanager and IBluetoothHci 1.1 and 1.0 on a
  real binder driver (binderfs in a user namespace) that read every parcel with libhwbinder's checks (tokens,
  `readBuffer`/`readEmbeddedBuffer` parent links), call back two-way from their own thread, `interfaceChain` on our
  callback, Reset and Read BD_ADDR answered, ACL echoed, close; no HAL registered; no `initializationComplete`; a
  HAL that dies. First phone test: `bridgectl bt` must print two Command Complete events (`runtime/bridges/README.md`).
- [U] the AIDL mode (the A56) against a real libbinder_ndk and the A56's HAL: not run. Tested on the PC (2026-11-24,
  `test_hidl_binder`'s AIDL scenarios): no context manager on the hwbinder (the A56's `Broken pipe`), a fake
  servicemanager and IBluetoothHci on a second device of the same binderfs (`ONEIOS_BINDER`, served with
  `hal::aidl`): the probe line, initialize with our callback object, its descriptor and version asked from the HAL's
  own thread, initializationComplete, Reset and Read BD_ADDR answered through hciEventReceived before they return,
  ACL (8 bytes and 5000, past the 4 KiB parcel), ISO and SCO echoed, close; a HAL that refuses the `SYST` header
  (the `VNDR` retry); neither HAL (the line naming both, the HIDL error returned); an AIDL HAL that dies (the state
  turns failed). The parcels' bytes against libbinder's layout: `bt::aidl_parcel`'s unit tests. SELinux on
  `/dev/binder` (the HAL calling back into our domain) is [U] as for HIDL; the A56 fork's session found the HAL, so
  the lookup is allowed there [F]. On an A56, `bridgectl bt` prints `HAL AIDL (android.hardware.bluetooth.IBluetoothHci/default) registered`
  and must print the same two Command Complete events.
- booter: the tree's node. `runtime/loader/src/machine.rs` publishes `/bluetooth` with `transport-encoding` 10 (the
  virtual transport of Apple's VMs) or, with the HAL (`bluetooth=hidl`, or `auto` when the HAL is registered, the
  default since 2026-11-11), `transport-encoding` 1 (H4; it was 3, H4BC, until 2026-11-12), `transport-speed` 3000000 and the phone's
  `local-mac-address`; `/defaults/vmm-present` 1 either way. What 24A446's `/usr/sbin/bluetoothd` contains (strings,
  read from `os.img` on the PC): `HCI Transport is set to H4BC`, `No valid serial port found (tried cu.bluetooth and
  uart.bluetooth)`, `Bluetooth is running inside a Virtual Machine, so skipping chip initialization` and `... skipping
  check for Bluetooth available` (`kern.hv_vmm_present`). So with `vmm-present` it should take the H4BC transport on
  our `/dev/cu.bluetooth` and run no BlueTool firmware step [I: from the strings, not run].
- The session of 2026-11-11 14:33 had none of this: xnuhost's default was `bluetooth=virtual`, so the bridge offered
  no HCI channel (`channels 0x33`), the guest said `ONEIOS BT: off: the host serves no Bluetooth controller` and hid
  the nub; xnuhost logged nothing about it. The key store answers `aks_wrap_key`/`aks_unwrap_key` (10/11; the same log
  has `selector 10 from securityd: 0x0` and `selector 11 ...: 0x0`), the fix Droid-VM needed (PROGRESS 11-02) for
  bluetoothd to store its LE identity root; bluetoothd ran once (pid 74) with no restart loop.
- userland: the bluetoothd job is on (it ran); BlueTool's firmware step should be skipped by `vmm-present` (above);
  `BTAudioHALPlugin.driver` kept in HAL and bluetoothd's iAP PTY made non-blocking (iOSPlus10
  `docs/laurel_sprout/bluetooth.md`, last section) are not done: they matter for headsets.
- SCO audio goes through the HAL as HCI; A2DP is iOS's own stack over ACL. LE Audio (ISO) only with a 1.1 HAL.
- UniXNU QcaBluetooth is a radio driver for a bare-metal WCN7850: not needed here.

## 4a. AirDrop (airdrop, 2026-11-22; corrected by quick-share-airdrop, 2026-11-23)

The owner's request (2026-11-22): AirDrop does not work in iOS, and its Control Center toggle does not switch; "check
how Android works with AirDrop and port it over". **Result: AirDrop cannot work in a session yet. Bluetooth was off in
sessions, so the toggle could not switch (fixed 2026-11-23, not yet booted: section 4), and the guest has no `awdl0`. The radio path exists on this phone: One UI's own
Wi-Fi driver has Google's "Wonder" raw 802.11 path, and its source is open.** What was built: `wifictl p2p`, a lasting
read-only report of every radio's peer-to-peer paths and of Google's Wonder radio, and `wifictl wonder`, a session on
Wonder's raw path beside `wlan0` that counts AWDL's frames and closes everything after it (2026-11-23; both below,
with `oneios-tool phone wifi p2p|wonder`; not run on the phone yet).

**Correction (2026-11-23).** The first version of this section said that Google's code is closed and that nothing
open runs AWDL on a Qualcomm chip. The owner disagreed. The owner is right for the kernel and radio half; only the
AWDL engine in user space is closed. What the first version got wrong:

1. "The code is not public: no AOSP source". Google's kernel driver (Wonder, GPL-2.0), Qualcomm's provider in
   qcacld-3.0 (ISC) and Samsung's copy of both for this phone (GPL release `S928BXXU5DZDP`) are public. Only
   `mosey_server`, its library, the Quick Share Extension app and the firmware are closed.
2. "Nothing open runs AWDL on a Qualcomm chip". Open drivers run AWDL on a Xiaomi 14 (SM8650, the same `kiwi_v2`
   radio as ours) with the S24 Ultra's own firmware, both ways with an iPhone on iOS 27.0.1.
3. "qcacld's monitor is a whole-driver mode, so OWL's way cannot work beside the station". That is true for
   `con_mode=4`, but Wonder's PASSTHRU vdev is a raw 802.11 path in the normal driver mode, beside `wlan0`.
4. "AWDL's 16 TU timing cannot be kept from user space". Google's design keeps AWDL in user space. The firmware
   only holds the channel (Wonder 1.4 and 1.5) or follows a TSF channel schedule (Wonder 1.6 and later).
5. "nl80211 vendor commands (Qualcomm OUI 00:13:74)". The commands are under Google's OUI `00:1a:11`, on Wonder's own
   wiphy, not on `wlan0`'s.
6. `thelok1s/mosey-extended` lists the Snapdragon S24 as "virtual phy only". Samsung's source shows otherwise.

### How Android does it: open and closed parts

The chain on a Pixel and on One UI 8.5 or later [F: the sources below; I: no part of it was read on our phone yet]:

```
GMS Quick Share -> com.google.android.mosey (Quick Share Extension app)
  -> IMoseyService (binder) -> mosey_server -> libmosey_daemon_ffi.so   (Rust: AWDL and AirDrop in user space)
  -> nl80211 vendor commands, OUI 00:1a:11, and radiotap frames on Wonder's interface
  -> wonder.ko (a mac80211 soft-MAC) -> wondertap provider in the Wi-Fi driver -> wondertap0 (PASSTHRU vdev)
  -> Wi-Fi firmware
```

| Part | Open? | Where |
|---|---|---|
| Wonder, the kernel driver ("Google Wonder WiFi Virtual Soft-MAC Driver") | **Yes, GPL-2.0** | [`android.googlesource.com/kernel/common-modules/wonder`](https://android.googlesource.com/kernel/common-modules/wonder): branches `android14-6.1` (head `71026f6`, 2026-06-06), `android15-6.6`, `android16-6.12`, `android17-6.18` (head `f70b3a4`, 2026-11-02). Also in Pixel kernel trees (`google-modules/wlan/wonder`) |
| The provider API | **Yes, GPL-2.0** | `include/wondertap.h` there: `struct wondertap_ops` (`init`, `deinit`, `set_freq`, `set_filter`, `set_fixed_tx_rate`, `set_tx_rate_mask`, `get_capabilities`, `channel_schedule_request`, `get_mac_tsf`, `get_channel_status_report`, `set_station_info`) and capability bits (`sta_coexist`, `channel_hopping`, `nss`, ...) |
| Providers in Wi-Fi drivers | **Yes** | Broadcom `bcmdhd4390` (Pixel 9 and 10); Intel `iwlmld` (commit `2ece331`, 2026-07-20); Qualcomm qcacld-3.0 `core/hdd/src/wlan_hdd_wondertap.c` (ISC; CodeLinaro tag `WLAN.LA.2.0.r3-10800-kaanapali.0`, mirrored in `LineageOS/android_kernel_qcom_sm8850-modules` `lineage-24.0`); MediaTek gen4m `gl_wondertap.h` (seen in `liudaoziwa/mainline-mt6991`) |
| **This phone's copy** | **Yes** | Samsung's GPL release for SM-S928B `S928BXXU5DZDP`, mirrored in [`samsung-sm8650/opensource`](https://github.com/samsung-sm8650/opensource) commit `6be2ff6f6c27`: `vendor/qcom/opensource/wlan/wonder/` (Wonder 1.5), `.../qcacld-3.0/core/hdd/src/wlan_hdd_wondertap.c` (provider 1.4.1), `.../qcacld-3.0/configs/pineapple_gki_kiwi-v2_defconfig` with `CONFIG_DRIVER_PASSTHRU_MODE=y`. The releases before it (`S928BXXS5CZB2` and older) have no `wonder/` directory |
| `mosey_server`, `libmosey_daemon_ffi.so` (the AWDL engine, Rust; embedded path `vendor/google/services/QuickShareExtension/src/`) | **No** | Google's binaries. One UI 9 ships them in `system_ext` [F per `ha0xin/houji-airdrop`, which unpacked `S928BXXU6EZIF`] |
| `com.google.android.mosey`, GMS Quick Share | **No** | Google-signed APKs |
| Wi-Fi firmware with the PASSTHRU service (`amss20.bin`) | **No** | `S928BXXS6DZF2` `vendor/firmware/kiwi/amss20.bin` ([`samsung-sm8650/proprietary_vendor_samsung_e3q`](https://github.com/samsung-sm8650/proprietary_vendor_samsung_e3q) `f749124168e9`) |
| [`google/nearby`](https://github.com/google/nearby) (Apache-2.0) | Yes, but not this | Its AWDL "medium" (`connections/implementation/mediums/awdl`, since `8acf9249344e`, 2025-03-28) is for Apple hosts only: it uses Network.framework's peer-to-peer mode (`initWithPeerToPeer:YES`). It has no AirDrop or AWDL code for Android |

**How the radio path works** [F: read in the Wonder and Samsung sources]:

- **Wonder** registers its own mac80211 wiphy (interface types monitor, ad hoc, station, NAN) and the vendor commands
  under `OUI_GOOGLE 0x001A11`: subcommand 1 `SET_FREQUENCY` (frequency in MHz, bandwidth), 2 `SET_FILTER` (BSSID or
  frame type), 3 `SET_FIXED_TX_RATE`, 4 `SET_REGULATORY` (country code), 5 `GET_IF_MAC_ADDR`. Google's newer
  branches add 6 `SET_CHANNEL_SCHEDULE_REQ`, 7 `GET_MAC_TSF`, 8 `GET_CAP`, 9 `GET_CHANNEL_STATUS_REPORT`,
  0xA `SET_STATION_INFO`, 0xB `SET_FEATURES`, 0xF `SET_TX_RATE_TEST`. **Samsung's `S928BXXU5DZDP` copy has 1 to 5 only:
  one channel, no hopping.**
- **Start.** Wonder's mac80211 `start` (an interface up on its wiphy) calls the provider's `init`. qcacld then opens
  an adapter in `QDF_PASSTHRU_MODE` named `wondertap%d`, a firmware vdev beside `wlan0`. The policy manager counts it
  as a session (STA and PASSTHRU on one channel, or MCC). `init` fails with `-EBUSY` while a connection or a link
  switch is in progress.
- **Frames.** Wonder registers an `rx_handler` on `wondertap0`, which carries radiotap frames. It sends 802.11 frames
  to it with `dev_queue_xmit`. The AWDL engine reads and writes frames on Wonder's monitor interface.
- **Binding.** Samsung's Wonder 1.5 is a platform driver (`google,wonder-drv-v1`, a device-tree `wondertap-provider`
  phandle, module parameter `physical_name`). It binds to cnss2's component (`qcom,cnss-vendor-wlan-wonder`), which
  qcacld fills with `pld_set_vendor_wonder_priv_data()` -> `cnss_set_vendor_wonder_priv_data()`. Google's 3.x branches
  use the auxiliary bus (`bcmdhd4390.wondertap`, `iwlmld.wondertap`).
- **Our phone's build.** The phone runs `S928BXXU6EZIF` (One UI 9, kernel `6.1.162-android14-11-34343818-abS928BXXU6EZIF`,
  `docs/agents.md`). `ha0xin/houji-airdrop` read that build's `qca_cld3_kiwi_v2.ko`: it imports
  `cnss_set_vendor_wonder_priv_data` [F per that project, not read by us]. Which Wonder version that build has, and
  whether `wonder.ko` loads at boot or only for Quick Share, is not known [U: the phone check below].

**Ports to other phones, all with open Wonder and qcacld sources:**

- [`YoWenhow/Xiaomi14-PixelOS-Airdrop`](https://github.com/YoWenhow/Xiaomi14-PixelOS-Airdrop) (GPL-2.0, v0.5.1-preview.1,
  2026-10-26): Xiaomi 14 (`houji`, SM8650, `kiwi_v2`) on PixelOS. It runs Google's `mosey_server` on a qcacld-3.0
  with a 115-file PASSTHRU overlay, a bridge module (the Xiaomi kernel's cnss2 lacks Samsung's Wonder symbols) and
  **Samsung's `kiwi_v2` PASSTHRU firmware** (`docs/BUILD-zh.md`). Its validation: two-way transfers with an iPhone on
  iOS 27.0.1. Normal Wi-Fi stops during a share.
- [`ha0xin/houji-airdrop`](https://github.com/ha0xin/houji-airdrop) (2026-11): the same phone; its
  `docs/airdrop/01-architecture.md` names the firmware: `S928BXXS6DZF2` `amss20.bin`. It runs on channel 6 (2437 MHz),
  HT20, one spatial stream, with the station off (`pt_exclusive=1`). It also runs Xiaomi's AWDL engine `libxos.so`,
  which it reads as derived from OWL (`OWL_mDNS` symbols) [I per that project].
- [`midori01/XiaomiGlobalAirDrop`](https://github.com/midori01/XiaomiGlobalAirDrop): Xiaomi 17 Ultra (`peach_v2`), AWDL
  on 5745 MHz while `wlan0` stays connected. Lessons for us: Mosey drops received frames without radiotap TSFT
  (`Ignoring management frame without required radiotap fields`), and Mosey disconnects Wi-Fi when the station is on
  5 or 6 GHz ("STA and mosey are running on the same band").
- Others with `wondertap` providers: `Weverses/OPAirDrop` (OnePlus), `AdenXie/pandora-airdrop` (Xiaomi 17),
  `Anton7444/MT6991-Airdrop` (MediaTek).
- [`uamind/quickshare_airdrop`](https://github.com/uamind/quickshare_airdrop) (Galaxy S23 Ultra, One UI 9): One UI's
  Quick Share calls this "Protocol X". On the S23 the `Mosey` service is missing, and a plain monitor interface
  cannot change channel (`cfg80211_set_monitor_channel` returns `-EBUSY` while `wlan0` exists). That fits a driver
  without PASSTHRU [I: the S23's driver source was not read].

**The protocols** (unchanged): AWDL ([Stute et al., MobiCom'18](https://ar5iv.labs.arxiv.org/html/1808.03156)) uses
vendor action frames (category 127, Apple's OUI 00:17:f2) for sync and service discovery, 16 TU availability windows,
an elected master, and data frames with Apple's LLC/SNAP header (IPv6 link-local only). Apple's devices meet on
channel 6 and channel 149 (44 too). [OWL](https://github.com/seemoo-lab/owl) (GPL-3.0, archived 2026-10-12) is an open
AWDL engine in user space; it needs a monitor interface with frame injection on one of those channels, and Wonder's
interface gives that. [OpenDrop](https://github.com/seemoo-lab/opendrop) is AirDrop's protocol over `awdl0` (Bonjour,
HTTPS, the BLE advertisement type 0x05); Everyone mode only. Google's interop also works with an iPhone in "Everyone
for 10 minutes" only ([Google](https://blog.google/security/android-quick-share-support-for-airdrop-security/)).
Wi-Fi Aware (NAN) is not AirDrop's link [I].

### What iOS 27 needs (24A446, read in the image's binaries and the 2026-11-22 boot)

Read with `oneios-tool image apfs-copy out/image/24A446/os.img /usr/libexec/sharingd|wifip2pd`, `llvm-objdump -d`,
and `ipsw dyld disass|dump` on `out/dsc-24A446-full`; the boot's logs are `out/session-logs/2026-11-22/s8-ioslog/ioslog.txt`
and `kbd-debug/xnuhost-s8.log`.

1. **sharingd's "ready" test [F, sharingd 0x1001ccac4]**: it logs `AirDrop not ready: wifi=%d, bluetooth=%d,
   carplay=%d, receivingEnabled=%d, isVirtualMachine=%d isMulticastAdvertisementsDisabled=%d` and is ready only when
   `wifi && bluetooth && receivingEnabled && !carplay && !multicastDisabled` (and one more flag, not logged). The
   VM flag is logged only, and `SFDeviceIsVirtualMachine` (Sharing.framework) is true only on an Apple-internal build
   (`IsAppleInternalBuild()`), so `/defaults/vmm-present` does not block AirDrop. Control Center's toggle asks
   sharingd, which logs `Turning on AirDrop, turning on WLAN=%d, Bluetooth=%d` and powers both on first [I: the
   toggle's path is read from the strings, not traced].
2. **Bluetooth is off in every session [F]**: the 2026-11-22 boot has bluetoothd `PowerState:3 managerState:2`,
   `Chip APPLE_VIRTUAL`, `BlSt PoweredOff` (section 4: on the VM path an unchanged bluetoothd powers on only its own
   simulated controller). **This is why the toggle does not switch** [I: the cause from the code above and this
   fact; no sharingd line of the tap was persisted in the log]. BLE is also AirDrop's discovery (the type 0x05
   advertisement), so Bluetooth power is the first thing AirDrop needs, before any Wi-Fi work. **Fixed 2026-11-23, not
   yet booted** (section 4, "Bluetooth powers on": transport-encoding 10 and `BT_HCI_TRANSPORT=H4:POSIX`).
3. **No `awdl0` [F]**: wifip2pd (AWDL's daemon since iOS 15) asks the guest's en0 once, at +163 s:
   `gets 97: -> 102` (`APPLE80211_IOC_VIRTUAL_IF_PARENT`, EOPNOTSUPP) and `sets 389: -> 102`
   (`APPLE80211_IOC_USERSPACE_P2P_OPTIONS`); then `gets 19` POWER, `9` BSSID, `51` COUNTRY_CODE, `sets 566`
   (`HAS_BATTERY`, accepted). It makes `awdl0`, `llw0` and `nan0` itself on a parent interface with
   `VIRTUAL_IF_CREATE` (94) when a client wants AWDL (`No AWDL interface found. Creating AWDL interface`, `Cannot
   create %s interface because the parent interface %s is missing`), then programs it with about 100 `AWDL_*`
   requests (115 to 262: sync, election, channel sequence, service parameters, peer database, keys). The guest's
   driver refuses every one of them (`kRefusedSets` in `kernel/patches/oneios-hosted/net/OneiOSWifi.cpp`), on
   purpose: no AWDL engine is behind it yet (the port plan below). The refusals cost nothing (wifip2pd asked once, 3% of a CPU for 10 s at its
   start).
   The names of the request numbers are 24A446's table (`_convertApple80211IOCTLToString`, 607 pointers at
   0x2d01b32f8 in IO80211.framework); the strings in order are not the numbers (they drift by up to 11).

### The port plan

iOS's AWDL is in Apple's driver and chip firmware: wifip2pd sends parameters (about 100 `AWDL_*` requests), and the
firmware does the frames and the timing. Our radio has no such firmware. So our side must run the AWDL engine in
software, over Wonder's raw interface, the way Google's `mosey_server` does. Google's engine is closed, so we cannot
reuse it in the guest's path. OWL's engine is open.

| Step | What | Feasible? |
|---|---|---|
| 1 | **Bluetooth powered in iOS** (section 4): bluetoothd on the phone's controller. AirDrop's discovery (BLE) and sharingd's "ready" need it. | Fixed 2026-11-23 in the kernel (section 4, "Bluetooth powers on"); not yet booted: the phone check there |
| 2 | **The radio, from the host** (`runtime/bridges/wifi`, root, beside One UI): find the Wonder wiphy (the one with vendor commands under `00:1a:11`); add a monitor interface on it (`NL80211_CMD_NEW_INTERFACE`) and bring it up (Wonder's `start` -> qcacld's `init` -> `wondertap0`); send `SET_REGULATORY` (4), `SET_FREQUENCY` (1: 2437 or 5745 MHz, 20 MHz), `GET_IF_MAC_ADDR` (5), `SET_FIXED_TX_RATE` (3); then send and receive radiotap frames with `AF_PACKET` on the monitor interface. On the way back: interface down, then delete it (Wonder's `stop` -> the provider's `deinit`). | Yes by the source [I: not run on our phone]. Limits: one channel (no hopping in 1.4.1/1.5); a 5 GHz station on the other channel means MCC; `init` refuses while a connection is in progress; One UI's Quick Share must not hold Wonder at the same time |
| 3 | **The AWDL engine**: OWL (C, GPL-3.0) in the host bridge on that interface: sync, election, availability windows, action frames, data frames. Map wifip2pd's `AWDL_*` requests (service parameters, peers, keys) onto it, and convert Bonjour on `awdl0` to AWDL's service TLVs and back [I: houji's `libxos.so` does this conversion]. | Plausible; the largest work. OWL's GPL-3.0 must fit the runtime's licence [U] |
| 4 | **Guest `awdl0`**: `VIRTUAL_IF_CREATE` (94) and the `AWDL_*` requests in `OneiOSWifi.cpp`, a second `IO80211Interface` whose Ethernet frames go to the host's engine (a second virtio-net device; the requests as OW messages on `en0`'s link: "How iOS's `awdl0` maps onto Wonder" below). | After steps 2 and 3 |
| 5 | **The data**: AirDrop itself (Bonjour on `awdl0`, TLS on IPv6 link-local) is iOS's own. | Yes, given steps 1 to 4 |

What cannot be done: Google's `mosey_server` cannot be the engine for iOS, because it is closed and it speaks to
Google's app and not to wifip2pd. A guest `awdl0` with no engine behind it would make wifip2pd and sharingd wait for
peers that never come (no fix, only a probe), so it is not written.

**The next step.** Steps 1 and 3 of the list of 2026-11-22 are built (2026-11-23, below): `wifictl p2p` dumps every
radio and names Wonder, and `wifictl wonder` opens and closes a Wonder session beside `wlan0`. What is left is the
phone check ("The phone checks" below), then the engine's choice (the owner's) and "How iOS's `awdl0` maps onto
Wonder".

### `wifictl p2p` (runtime/bridges/wifi/p2p.rs, built 2026-11-22, every radio since 2026-11-23)

Read only, safe beside One UI and in a session: a split `NL80211_CMD_GET_WIPHY` dump with no interface named (every
wiphy: cfg80211 writes `NL80211_ATTR_WIPHY` first in each message, so each message goes to its own radio, also
when the dump interleaves them) and a `GET_INTERFACE` dump of every interface (those with no network device too),
then the network devices, modules and processes whose names hint at AWDL or NAN (`awdl`, `aware`, `nan`, `wonder`,
`mosey`, `p2p`, `quickshare`, `nearby`, `llw`: [I] hints only). For each radio it prints the interface types, each
interface combination (`combination N: up to M interfaces on C channel(s): 1 of {station}; ...`), which interface
types may send and receive action frames, off-channel sends and remain-on-channel, every vendor command and event
by OUI, and `(wlan0's radio)` or `(Google's Wonder radio)`; then every interface (`(on Wonder's radio)`, `(the Wi-Fi
driver's PASSTHRU vdev for Wonder)` for a `wondertap*`).

**Which radio is Wonder:** the wiphy named `wonder...` (Wonder registers its mac80211 hardware with the name
`wonder`, `ieee80211_alloc_hw_nm(..., DRV_NAME)`), else one with vendor commands under `00:1a:11` that offers
monitor mode and no access point (a Broadcom driver's own wiphy has commands under Google's OUI too, and it offers an
access point). Then the `p2p: Wonder:` lines:

```
p2p: Wonder: wiphy 1 (wonder), interface types ad-hoc, station, monitor, nan
p2p: Wonder: vendor subcommands under 00:1a:11: 1 SET_FREQUENCY 2 SET_FILTER 3 SET_FIXED_TX_RATE 4 SET_REGULATORY 5 GET_IF_MAC_ADDR
p2p: Wonder: version 1.4 or 1.5 (subcommands 1 to 5 only: one channel, no hopping; Samsung's S928BXXU5DZDP source)
p2p: Wonder: idle (no interface on it, no wondertap vdev)
```

(`version 1.6 or later` with subcommand 6 or more; `in use: NAME (TYPE)` for each interface on Wonder's wiphy;
`the PASSTHRU vdev wondertap0 (index N, wiphy M) is up on F MHz`; `no Wonder radio (...)` when there is none.) The
`p2p:` verdicts are for `wlan0`'s radio (`wifictl p2p IFACE` for another): monitor mode (and whether beside the
station), AWDL's action frames on the station, NAN through nl80211, Wi-Fi Direct, vendor commands under Apple's OUI,
and for AirDrop `Wonder's raw 802.11 path is offered (wiphy N)` or, without Wonder, the older verdicts. Last, one
`wonder: PATH: present|none` line for each of `/sys/module/wonder`, its `parameters/physical_name` (with its value),
`/sys/bus/platform/drivers/wonder`, `/vendor_dlkm/lib/modules/wonder.ko`, `/vendor/lib/modules/wonder.ko`,
`/system_ext/bin/mosey_server`, `/vendor/bin/mosey_server` (named paths only).

Host-tested (2026-11-23, in the container: `test_wifi` 301 checks, 0 failed, with ASan and UBSan; `test_radios`:
two wiphys with Wonder's dump between phy0's two messages, Wonder 1.5 and 1.6, Wonder by its commands and not by
its name, a Broadcom-like wiphy that is not Wonder, Wonder in use, a `wondertap0` without a Wonder radio, more
wiphys than kept, none, a cut report); **not run on the phone**.

### `wifictl wonder` (runtime/bridges/wifi/wonder.rs, built 2026-11-23)

A session on Wonder's raw 802.11 path beside `wlan0`, as root with One UI running:
`wifictl wonder [--seconds N] [--freq 2437|5220|5745] [--country CC]` (default 30 s on 2437 MHz). The order is
Samsung's Wonder 1.5 (`vendor/qcom/opensource/wlan/wonder` in `S928BXXU5DZDP`) [F: read in that source]: Wonder
caches the frequency, the TX rate, the BSSID and the country, and its `wondertap_init` fails with `Missing cached
settings` unless all four came before the interface's up; the up (mac80211's `start`) calls the provider's `init`,
which refuses with `-EBUSY` while a connection is in progress, **sets the driver's country to the one given**
(`hdd_reg_set_country`), and opens `wondertap0`; `GET_IF_MAC_ADDR` answers only when Wonder is up (the vdev's
address is random, `eth_random_addr`); the down calls `deinit`, which closes the vdev. All five commands need the
interface (`WIPHY_VENDOR_CMD_NEED_NETDEV`), not its being up. The steps, each one a `wonder:` line with its result:

1. `wlan0`'s link before (nl80211 `GET_STATION`: its access point, signal, rate; `GET_INTERFACE`: its channel).
2. Every radio; **refused** (nothing made) when there is no Wonder radio, when it lacks `SET_FREQUENCY`,
   `SET_REGULATORY` or `GET_IF_MAC_ADDR`, when Wonder is in use (another interface on its wiphy, or a `wondertap*`
   up: One UI's Quick Share holds it), when there is no `wlan0`, when the driver's country (`GET_REG` for `wlan0`'s
   wiphy) is not known (`00`) and no `--country` was given, or when `--country` is another than the driver's (it
   would change the phone's country). A `wondermon0` left on Wonder's wiphy by a killed session is removed first.
3. `wondermon0`, a monitor interface on Wonder's wiphy (`NL80211_CMD_NEW_INTERFACE`).
4. `SET_REGULATORY` (the driver's own country, `"GB\0"`), `SET_FREQUENCY` (the frequency, 20 MHz),
   `SET_FIXED_TX_RATE` (HT, MCS 0, one stream, 20 MHz, 0.8 us), `SET_FILTER` (BSSID, AWDL's `00:25:00:ff:94:73`);
   the last two only when the radio lists them. A line says when `wlan0` is on another channel (the radio then
   shares its time: MCC).
5. `wondermon0` up (`SIOCSIFFLAGS`): Wonder's start, the provider's init, `wondertap0`. Then `GET_IF_MAC_ADDR` and
   the interfaces on Wonder's wiphy and `wondertap*` with their channel.
6. `AF_PACKET` on `wondermon0`: N s of frames counted (a 500 ms `poll`, no busy loop). Each frame's radiotap
   (TSFT, flags with FCS and bad FCS, signal) and 802.11 header are read: AWDL's action frames (category 127,
   Apple's OUI, type 8) by subtype (0 PSF, 3 MIF, others), AWDL's data frames (addr3 AWDL's BSSID, no DS bits), the
   peers (up to 16) with their counts and best signal, and the frames without the radiotap TSFT (Mosey drops
   those, and an engine needs it for the sync).
7. In reverse, also after any failed step and after Ctrl-C, `SIGTERM` or `SIGHUP` (they end the count; `SIGPIPE` is
   ignored, so a dropped adb never stops the teardown): the socket closed, `wondermon0` down (deinit: `wondertap0`
   closes), `wondermon0` removed (`NL80211_CMD_DEL_INTERFACE`).
8. `wlan0`'s link after, against the link before: asked again each second for up to 10 s while it is down; the end
   is `-ENOLINK` and `wlan0 lost its link` when it was associated before and is not again; `a roam` when its access
   point changed.

The last lines: `wonder: done: N frames, M AWDL action frames from P peer(s); wlan0 associated as before`, then
`ok`; or the step's error, then `failed`. Host-tested (2026-11-23, `test_wifi` 301 checks, 0 failed: `test_awdl_frames`
(radiotap with and without TSFT, a second present word, FCS at the end, a bad FCS, AWDL's PSF, MIF and other
subtypes, Apple's vendor action that is not AWDL, a public action, AWDL's data frame and others, an ACK, cut and
broken headers, more peers than kept); `test_wonder_vendor` (each command's attributes byte for byte, the MAC reply);
`test_wonder_session` (a stand-in nl80211: the order of every call, the refusals, a leftover removed, a failed
create, a refused frequency, a refused up (`-EBUSY`), a failed socket, a receive error, a refused removal, a Wonder
without `SET_FILTER` and `SET_FIXED_TX_RATE`, a signal, `wlan0` back after 3 s, lost, and not associated before)).
**Not run on the phone.**

### The phone checks (for `wifictl p2p` and `wifictl wonder`; not run yet)

`oneios-tool phone wifi p2p|wonder` runs the staged `wifictl` as root (`wonder`'s lines as they come, through `adb
exec-out`; its arguments checked before they reach root's shell); tested against a stand-in `wifictl`
(`tools/oneios-tool/tests/phone/wifi.sh`, 24 checks, ALL OK on the Mac 2026-11-23).

Stage a runtime that has the new `wifictl` (`runtime/build.sh`, then `oneios-tool phone stage runtime-asis DIR`). One
UI running, no session, Wi-Fi on and associated, Bluetooth on:

```sh
T=tools/oneios-tool/target/release/oneios-tool
$T phone wifi p2p --save p2p-idle.txt              # (a) One UI idle
$T phone wifi p2p --save p2p-share.txt             # (b) during a Quick Share "Share with Apple devices" send to an iPhone
diff p2p-idle.txt p2p-share.txt
$T phone wifi wonder --seconds 30 --freq 5745 --save wonder-5745.txt   # (c) an iPhone near in AirDrop "Everyone for 10 minutes"
$T phone wifi wonder --seconds 30 --freq 2437 --save wonder-2437.txt   #     and on channel 6
```

What proves each:

- **(a)** `p2p: Wonder: wiphy N (wonder), ...` (Wonder's radio is registered at boot), its `vendor subcommands`
  line and `version 1.4 or 1.5` or `1.6 or later` (which Wonder One UI 9 ships), `p2p: Wonder: idle`, and
  `wonder: /sys/module/wonder: present`, `.../physical_name: present: wondertap0`, `/system_ext/bin/mosey_server:
  present`. `p2p: Wonder: no Wonder radio` with `/sys/module/wonder: none` means that Wonder loads only for Quick
  Share: then (b) shows it.
- **(b)** started while the share sheet looks for the iPhone: `p2p: Wonder: in use: ...` (Mosey's interface on
  Wonder's wiphy and its type), `the PASSTHRU vdev wondertap0 (index N, wiphy M) is up on F MHz` (the channel
  Google's engine chose: 2437 or 5745), `process: ... mosey_server`. The diff with (a) is One UI's AWDL path.
- **(c)** `wonder: wondermon0 up (...): ok`, `the PASSTHRU vdev's address ...`, `up: wondertap0 (...) 5745 MHz`,
  then `AWDL: N action frames (P PSF, M MIF, ...) ... from K peer(s)` with N > 0 and an `AWDL peer:` line (the iPhone's
  AWDL address, random) prove that Wonder receives AWDL on this phone; no `radiotap: ... frames without the TSFT`
  line proves the TSFT an engine needs; `wondermon0 removed (...): ok` and `wlan0: the same link as before` (or
  `a roam`) prove that the way back works. `refused: Wonder is in use` means that Quick Share still holds it (close
  its sheet); `up (...): Device or resource busy` means a connection in progress (try again);
  `AWDL: no frame at all` means Wonder's receive path gave nothing (the firmware or the filter); `no AWDL frame`
  with other frames means no AWDL on that channel (try the other one, the iPhone's share sheet open).

### How iOS's `awdl0` maps onto Wonder (design, 2026-11-23; not built)

The parts and who does what. The AWDL engine is not chosen yet (the owner's decision): OWL (C, GPL-3.0; the runtime
is MIT, so a separate helper process with its own licence is the plain way [I: no legal reading]) or our own.

**1. The guest's driver (`kernel/patches/oneios-hosted/net/OneiOSWifi.cpp`).** wifip2pd asks `en0` for
`VIRTUAL_IF_PARENT` (97) and `USERSPACE_P2P_OPTIONS` (389) and makes `awdl0` with `VIRTUAL_IF_CREATE` (94) on it.
The driver answers 97 with `en0`, makes a second interface (an `IO80211` virtual interface with its own address,
the role AWDL) for 94 with the AWDL role, and goes on refusing `llw0` and `nan0` (no engine for them). Then each
`AWDL_*` request (115 to 262 in 24A446's table) is passed to the host's engine as a control message and the answer
is the engine's. 24A446's shared cache names 82 `APPLE80211_IOC_AWDL_*` requests (read with `grep -ao` on
`out/dsc-24A446-full`: strings only, not yet mapped to their numbers [U]). Grouped by what the engine must do with
them:

| Group | The requests (24A446 names, `APPLE80211_IOC_AWDL_` left out) | The engine |
|---|---|---|
| On, off, the role | `CONFIG`, `CTL`, `OPER_MODE`, `STRATEGY`, `PRESENCE_MODE`, `QUIET`, `BLOCK_SET_COMMANDS`, `MAC_ADDRESS_IN_USE`, `BSSID`, `OUI`, `DEVICE_CAPABILITIES`, `RSDB_CAPS` | starts and stops the Wonder session (`wonder.rs`'s steps, kept open), the address of `awdl0` as the vdev's own, AWDL's BSSID, what we can do (one channel, HT20, one stream) |
| Synchronization | `SYNC_ENABLED`, `SYNC_PARAMS`, `SYNC_STATE`, `SYNC_FRAME_TEMPLATE`, `SYNC_FRAME_AP_BEACON_ALIGNMENT`, `PERIODIC_SYNC_FRAME_PACKET_LIFETIME`, `MASTER_MODE_SYNC_FRAME_PERIOD`, `NON_ELECTION_MASTER_MODE_SYNC_FRAME_PERIOD`, `GUARD_TIME`, `AVAILABILITY_WINDOW_AP_ALIGNMENT`, `EXTENSION_STATE_MACHINE_PARAMETERS`, `EXPLICIT_AVAILABILITY_WINDOW_EXTENSION_OPT_OUT`, `MAX_NO_MASTER_PERIODS`, `MAX_TREE_DEPTH`, `TOP_MASTER` | the availability windows: 16 TU (16.384 ms) each, counted against the master's TSF from its PSF and MIF (the radiotap TSFT of each received frame), our own PSF (about every 110 ms) and MIF (each window we are in), the window extensions, the sync tree |
| Election | `ELECTION_ALGORITHM_ENABLED`, `ELECTION_ID`, `PRIVATE_ELECTION_ID`, `ELECTION_METRIC`, `ELECTION_PARAMS`, `ELECTION_MASTER_COUNTS`, `ELECTION_RSSI_THRESHOLDS`, `GET_AWDL_MASTER_DATABASE` | the master election (metrics, counters, the top master), a slave beside an iPhone, the master alone |
| Channels | `SYNCHRONIZATION_CHANNEL_SEQUENCE`, `MASTER_CHANNEL`, `SECONDARY_MASTER_CHANNEL`, `COMMON_CHANNEL`, `PREFERRED_CHANNELS`, `SOCIAL_TIME_SLOTS`, `SCAN_RESERVED_TIME`, `PIGGYBACK_SCAN_REQ`, `DFSP_CONFIG`, `DFSP_UCSA_CONFIG`, `ENABLE_ROAMING`, `FORCED_ROAM_CONFIG` | the channel sequence it advertises: with Wonder 1.5 one channel only (every slot on 149, or on 6: `SET_FREQUENCY` once; [I] iPhones accept a sequence of one channel, as OWL runs on one); with 1.6 or later the schedule through `SET_CHANNEL_SCHEDULE_REQ` |
| Services and peers | `SERVICE_PARAMS`, `UPDATE_SERVICE_COUNT`, `PEER_SERVICE_REQUEST`, `PEER_DATABASE`, `PEERS_INFO`, `PEER_TRAFFIC_REGISTRATION`, `AWDL_ADVERTISERS`, `OOB_REQUEST`, `OOB_AUTO_REQUEST`, `OOB_SEC_PAYLOAD_REQUEST`, `AF_SECONDARY_PAYLOAD_CAPABILITIES`, `AF_TX_MODE`, `MI_BITMAP`, `UMI_TX_PEER`, `RSSI_MEASUREMENT_REQUEST` | the service parameters TLV of our frames (the Bonjour hashes wifip2pd gives), the peer table (each peer's address, its TLVs, last seen, signal) for wifip2pd's reads, and the driver's events when a peer comes and goes |
| Keys | `AES_KEY`, `ENCRYPTION_KEYS`, `ENCRYPTION_TYPE` | [U] what AWDL encrypts at layer 2 (AirDrop itself is TLS); answered "none" until known |
| Bluetooth and coexistence | `BTLE_ENABLE_SYNC_WITH_PARAMS`, `BTLE_PEER_INDICATION`, `BTLE_STATE_PARAMS`, `BT_COEX_AGREEMENT`, `BT_COEX_AGREEMENT_ENABLED`, `BT_COEX_AW_PROTECTED_PERIOD_LENGTH`, `CCA`, `TXCAL_PERIOD`, `MIN_RATE`, `BATTERY_LEVEL` | accepted with no effect (the host's radio does its own coexistence) |
| Statistics and logs | `STATISTICS`, `DEV_STATS`, `AIRPLAY_STATISTICS`, `LOW_LATENCY_STATISTICS`, `RTG_PEER_STATS_CONFIG`, `SIDECAR_DIAGNOSTICS`, `NEARBY_LOG_TRIGGER` | the engine's counters, else zeros |

**2. The engine on the host.** It owns the Wonder session for the length of a session (opened when wifip2pd turns
AWDL on, closed when it turns it off and always at the return: the way back). It reads radiotap frames from
`wondermon0`'s `AF_PACKET` socket and writes radiotap-headed 802.11 frames to it (mac80211's monitor TX, then
Wonder's `tx`, then `dev_queue_xmit` on `wondertap0`). What it must do:

1. **Synchronization**: keep its availability-window counter in step with the master's (the sync parameters TLV:
   the window's sequence number, the TSF of the PSF or MIF, the TX delay), with the received frames' TSFT.
2. **The window schedule**: which channel each window is on, when it may send (in its own and the peer's windows),
   the extended windows while data flows.
3. **Election**: the master election and the sync tree; a slave beside an iPhone, the master when it is alone.
4. **Its frames**: PSF and MIF with the TLVs (sync parameters, election parameters, the channel sequence, the
   service parameters, the data path state, the host name, the version, HT capabilities), each in its window.
5. **The peer table**: from the peers' frames; it answers wifip2pd's peer requests and sends the driver's events.
6. **The data path**: Ethernet frames of `awdl0` to 802.11 data frames to AWDL's BSSID (Apple's LLC/SNAP header,
   OUI `00:17:f2`, then AWDL's data header and the frame's ethertype) and back, queued to the next window both sides
   are in. IPv6 link-local only; the neighbour addresses come from the peers' MACs.
7. **Bonjour** stays iOS's own: mDNSResponder sends mDNS on `awdl0` as multicast data frames; the service
   parameters TLV carries the hashes wifip2pd gives [I: from OWL and Stute et al., not read in wifip2pd].

The load is small for the host: a PSF every 110 ms and a MIF a window per peer, about 100 frames a second and their
syscalls (70 us each on this phone: under 1% of a core), plus the data. The windows' 16 TU timing is the engine's
own clock from the TSFT; the firmware holds the channel (Wonder 1.5) [I: OWL does the same on one channel].

**3. The frame path between the guest's `awdl0` and Wonder:**

```
send:    sharingd, mDNSResponder (iOS) -> awdl0 (OneiOSWifi's virtual interface)
         -> virtio-net: a second net device of xnuhost for awdl0, or an OW message on en0's link
         -> the AWDL engine (host): Ethernet -> 802.11 data + LLC/SNAP + AWDL header, held to the next window
         -> AF_PACKET on wondermon0 (a radiotap TX header) -> mac80211 -> Wonder tx -> wondertap0 -> firmware
receive: firmware -> wondertap0 -> Wonder rx_handler -> mac80211 monitor -> AF_PACKET (radiotap with TSFT)
         -> the engine: action frames to the sync, election and peers; data frames to Ethernet
         -> virtio-net -> awdl0
```

The control path (the `AWDL_*` requests and their answers, the peer events) goes as new OW message types on
`en0`'s control link (`drivers/net/oneios_wifi_proto.h`), as the station's messages do. For the data, a **second
virtio-net device** for `awdl0` (xnuhost's `runtime/bridges/devices/virtio_net.rs`, a second entry in the device tree,
its own queues and address) is the better choice [I]: no encapsulation, no shared queue with `en0`, and iOS's own
interface statistics; an OW message type on `en0` is the smaller change if a second device costs more than it
seems.

**4. The limits on this phone** [F: Samsung's source; I: not run]: one channel (no hopping in 1.5); MCC with `wlan0`
when it is on another channel of the same radio (Google's Mosey disconnects a 5 or 6 GHz station for that, per
`midori01/XiaomiGlobalAirDrop`); the provider's init refuses during a connection or a roam; One UI's Quick Share and
ours cannot hold Wonder at the same time (`wifictl wonder` refuses then); the init sets the driver's country, so the
engine must give the driver's own.

**The order of the work:** (1) the phone checks above; (2) the owner chooses the engine; (3) one PSF sent and an
iPhone's PSF or MIF that names us as a peer received (the first TX test: `wifictl wonder` with a send step); (4) the
guest's `awdl0` and the control messages; (5) the data path; (6) AirDrop with Bluetooth on (section 4).

## 5. Vibration and haptics

### The phone [F]

- Actuator: an LRA on a **Cirrus CS40L26A**, I2C address 0x40 (`cirrus,cs40l26a`, `e3q_eur_openx_w00_r09.dts` line
  11453; `samsung,f0-tracking`, dynamic boost control on). Modules `input_cs40l26_i2c`, `snd_soc_cs40l26`, `cl_dsp`.
- Exposed by Samsung's wrapper `sec_vib_inputff` (line 682; `CONFIG_SEC_VIBRATOR_INPUTFF=m`, `CONFIG_INPUT_CS40L26_I2C=m`
  in `arch/arm64/configs/oem/e3q_defconfig`) as an **evdev force-feedback device**: `/dev/input/event8`, name
  `sec_vibrator_inputff`, EV_FF bitmap `124020000`: `FF_PERIODIC`, `FF_SINE`, `FF_CUSTOM`, `FF_GAIN`; **no
  `FF_RUMBLE`** [F, owner's phone as root].
- Driver source: `drivers/vibrator/cs/cs40l26/cs40l26.c` and `drivers/vibrator/common/inputff/` in
  [samsung-sm8650/android_kernel_samsung_sm8650@809f7ed](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650/blob/809f7edaad95be2f60ce09f577fe5104d8940214/drivers/vibrator/cs/cs40l26/cs40l26.c):
  - `FF_PERIODIC` only (`cs40l26_upload_effect`); `FF_SINE` programs the chip's buzz generator
    (`cs40l26_sine_upload`, line 3234): period clamped to 4..10 ms (100..250 Hz), magnitude 0..255, length in ms;
  - `FF_CUSTOM` with two values `[bank, index]` plays a stored waveform (bank 0 RAM, 1 ROM, 3 buzz), with more
    values a composite or PWLE waveform uploaded to the chip (`cs40l26_custom_upload`, line 3335);
  - `FF_GAIN`: Samsung's scale 0..10000 (`gain / 100` = percent, `cs40l26_set_gain`, line 2569).
- The AIDL vibrator HAL (`android.hardware.vibrator-service.samsung`) and IVibratorManager run and hold `event8`
  open; a second open is allowed, and an effect belongs to the file that uploaded it.
- Also present and not used: `qcom,hv-haptics` and `qcom,pm8550b-swr-haptics` nodes (PMIC haptics, Qualcomm's
  reference design), `leds_qpnp_vibrator_ldo`.

### iOS side (`drivers/haptics`, port of iOSPlus10 `patches/laurel/xnu/2700-laurel-pmi632-haptics.patch`)

- `OneiOSVibrator.cpp` (LaurelVibrator as is): the IOService named `vibrator` that FigVibrator's legacy path finds
  (`IOServiceNameMatching("vibrator")`, `IORegistryEntrySetCFProperties`): start
  `{hertz_millisecs = ({intensity = 16.16 fixed}, on_ms, false, off_ms, ...), repeat}`, stop = a CFNumber 0. The
  step timer, the repeat cap (a loop without a new request ends after 60 s: iOS loops a pattern without
  TotalDuration for 31 days and relies on its client's stop), the key stop, the one log line per request shape are
  kept. Changed: the `intensity` of a step is kept (0..1000) and sent to the host (the Mi A3's ERM was on/off);
  the service is made by `oneios_vibrator_start()` instead of a KernelConfigTables entry, so no kernel patch.
- `oneios_haptic.c` (laurel_haptic.c): `/dev/oneios-haptic` and `/dev/redwood-haptic` (the node ios-topaz's
  RedwoodAudio HAL plug-in opens for its "Actuator" output): S16LE mono 48 kHz. Kept: 2 ms blocks, a peak above
  256/32768 is "on", the closed-loop rule (publish `IODeviceTree:/product/haptics {closed-loop 1}` only when the root
  has an Actuator plug-in and VirtualAudio.plugin: with closed-loop on and no Actuator device iOS plays no vibration
  at all), the node made before the audio server can ask for it. Replaced: the SPMI lane, the LDO and the motor
  thread (overdrive, minimum on-time, duty limit, watchdog). Those protected an ERM on a bare LDO; here the chip's
  DSP brakes and protects the LRA. New: each write's loud span is sent with its peak as the intensity and its
  zero-crossing rate as the frequency.
- Messages: `OB_HAPTIC_VIB {on_ms, intensity 0..1000, frequency_hz}`, `OB_HAPTIC_STOP`,
  `OB_HAPTIC_IMPACT {kind 1..3}` (an explicit one-shot, never a short VIB segment; only when the host
  advertised `OB_FEAT_HAPTIC_IMPACT` in a length-extended v1 `OB_CTL_HELLO`, else the guest sends none and an
  old host's 8-byte hello reads as features=0).

### UI haptics: the Actuator audio device (media, 2026-11-12; built, not run on the phone)

Why taps did nothing [F, iOS's log of the 14:12 session]: `ONEIOS HAPTIC UI: off: no Actuator HAL plug-in in the
root`, so no closed-loop node; UIKit then says `_UIKBFeedbackGenerator: ... Haptics: unsupported ... Mode: audio
only`, `_UIFeedbackSystemSoundEngine` plays the feedback as a system sound, and backboardd says `Device doesn't support
Haptic Music`. Only FigVibrator's legacy `vibrator` path ran (`ONEIOS VIBRATOR: request kind 1 from audiomxd`).

How iOS 27 plays a tap [F 24A446 unless marked]: VirtualAudio.plugin (`/Library/Audio/Plug-Ins/HAL/VirtualAudio.plugin`
on the OS volume, not in the shared cache) has `Device_Actuator_Aspen.cpp`, `VirtualAudioDevice_Actuator`,
`VAD_Actuator`, `ActuatorIV`, `Creating Actuator Port`, `Could not find actuator port after 8 seconds`; its routing
already looks for the actuator's settings (`Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general` in every
session). [V iOS 16, iOSPlus10 docs/haptics-plan.md 8.1; I for iOS 27] the physical device with UID **"Actuator"**
becomes that VAD; UIKit's level 2 (taps, keyboard) and Core Haptics need MobileGestalt's closed-loop answer, which is
`IODeviceTree:/product/haptics closed-loop`. The V54 device tree has `/product/haptics {closed-loop 1,
supports-3rd-party-haptics 1, power-control-features 2, controller-coeff-cal "syscfg/CLCL"}`; the loader does not
copy it (`/product`'s children are not copied). The kernel drops MobileGestalt's cache at each boot
(`gestalt-cache-drop`), so a node published before the first answer is what it reads.

What is built:
- **The device**: the audio archive (`oneios-tool kernel gen audio`) stages a third
  IOAudio2Device into `DroidVMAudio.cpp`: UID `Actuator`, one output stream, mono S16 at 48 kHz
  (`oneios-actuator-channels=2` for stereo), the speaker's engine (240-frame periods on the guest's clock), output
  latency one period (not the speaker's 6480 frames: the haptic synth keeps haptics in step with a device's
  latency). Each period goes to `oneios_haptic_actuator_play()`, not to virtio-snd. Published only when
  `drivers/haptics` has the host's actuator and UI haptics are on; `oneios-actuator=0` leaves it out.
- **The requests** (`drivers/haptics/oneios_actuator.h`, host-tested by `drivers/haptics/test_actuator.c`, 19 checks):
  a period is loud above 256 of 32768; the first loud period asks 20 ms (a tap), the run renews when less than 12 ms
  is left (50 ms segments after 20 ms) or the level moves by more than 100 of 1000; a run of 30 ms or more is stopped
  after 10 ms of silence, a tap runs out. Intensity = the period's peak; frequency = the run's zero crossings with
  hysteresis, after 10 ms. A tap is one request; a long vibration about 20 a second (each is an effect upload on
  the CS40L26's I2C).
- **Native impacts** (off by default, both sides opt in): with the guest boot argument `oneios_native_impacts=1`
  and the host started with `ONEIOS_HAPTIC_NATIVE_IMPACTS=1` on a HAL whose `getSupportedEffects` advertises
  TICK, CLICK and HEAVY_CLICK, a short isolated run (under 30 ms, confirmed by 10 ms of silence) becomes one
  `OB_HAPTIC_IMPACT` instead of 20 ms motor starts; the intensity maps to LIGHT (<=250), MEDIUM (<=800), HEAVY.
  The host plays the matching `IVibrator.perform()` preset through the same serialized worker; a later STOP never
  cancels an already-finished preset (its HAL-reported duration bounds `off()`), so One UI's own vibration is not
  interrupted. Longer runs keep VIB/STOP semantics, malformed or unsupported impacts are dropped, never mapped to
  VIB. Host-tested (`test_bridges` native-impacts case, `test_hal_binder` preset case); not run on the phone.
  **The session's switch** (2026-11-21, from the S26 Ultra fork's bec7efe, adapted): the OneOS fork starts xnuhost
  with no environment, so the two halves are one xnuhost setting, `haptics-native-impacts=1` in the image set's
  `xnuhost.conf` (or `--set`; [booter.md](booter.md) section 1): it tells the bridge to ask for the presets
  (`haptics::set_native_impacts`, no `setenv`) and appends `oneios_native_impacts=1` to the boot arguments,
  keeping them whole. xnuhost refuses it (exit 2) unless `hw-backend-haptics=treble`, `hw-backend-fallback=0`,
  `haptics=auto` and the bridges are on, and when the boot arguments already say `oneios_native_impacts=` with
  another value; `=0` turns it off whatever the environment says. Unset (the default on every phone), the
  environment decides as before. Host-tested (`test_xnuhost` config cases, `test_bridges` override case).
  On the phone, beside One UI: `ONEIOS_HAPTIC_NATIVE_IMPACTS=1 ONEIOS_HW_BACKEND_HAPTICS=treble ONEIOS_HW_FALLBACK=0
  bridgectl haptic-native-inspect` checks the same preset preflight without playing anything (runtime/bridges
  README, "On the phone"). The fork felt one `perform(HEAVY_CLICK, STRONG)` on the S26 Ultra (2026-11-16, its
  bridges README); no guest-to-motor tap has been felt on any phone.
- **The closed-loop node**: `oneios_haptic.c` waits (5 s at most) for the audio archive's decision
  (`oneios_haptic_actuator_published`), then publishes `/product/haptics {closed-loop 1, supports-3rd-party-haptics 1}`
  when the device exists (the old rule, an Actuator HAL plug-in in the root, stays as the second way in). Not copied
  from the DT: `controller-coeff-cal` (a syscfg key the guest has not) and `power-control-features` (unknown use).
- With closed-loop on, FigVibrator also takes the unified engine [V iOS 16]: ringtones and alerts then play on the
  Actuator too, and the `vibrator` service gets no requests.

backboardd's `BKButtonHapticsController` and its "click states" [F, backboardd strings]: it configures the click
haptics of a button through the ButtonHaptics framework and the HID haptic interface (`BKSButtonHapticsDefinition
homeButtonDefinition`, `clickStatesPending`, `clickStatesConfigured`, `BKHIDHapticFeedbackInterface`), the path of a
haptic Home button or a touch-sensitive button, not of the Actuator audio device. The device above does not satisfy it;
on this identity (`home-button-type 2`, no haptic Home button) its pending state is harmless [I]. Corrected
2026-11-13 (fix-haptics, read in backboardd): not harmless for a caller of `playHapticForClickState:clickSpeed:`,
which then blocks until ButtonResolver is ready ("Haptics do nothing" below).

Next boot's log (kernel lines):
```
DroidVMAudio: published Speaker=1 Mic=1 Actuator=1 as IOAudio2Devices (UIDs Speaker, Mic, Actuator x1; ...)
ONEIOS HAPTIC: Actuator audio device published (UID "Actuator", mono 48 kHz S16)
ONEIOS HAPTIC UI: on: /product/haptics closed-loop = 1 (... -> VirtualAudioDevice_Actuator -> the Actuator IOAudio2Device -> the host's actuator)
ONEIOS HAPTIC: Actuator: I/O started by audiomxd (start 1)          (at the first haptic)
ONEIOS HAPTIC: play 1: 20 ms, intensity N of 1000, 0 Hz (resonance)   (each tap; the first 24 are logged)
ONEIOS HAPTIC: Actuator: I/O stopped: N periods, N loud, N requests to the motor so far
```
In iOS's log: no `Haptics: unsupported` from `_UIKBFeedbackGenerator`. If the device is published and no `I/O
started` line comes: VirtualAudio did not make the VAD (`Could not find actuator port`) or MobileGestalt still says
no. To go back: `oneios-actuator=0` (no device, no node: the `vibrator` path as before).

### The power key since the Actuator (15:35 boot; read in 24A446's shared cache on the Linux PC, mem, 2026-11-12)

In the 15:35 boot the side key's presses reached backboardd (34 HID events, 0 dropped) and no `request_power_change`
followed; the boots before the Actuator device locked and woke. What is read [V, `ipsw dyld disass`, SystemOS cryptex
`043-70037-715.dmg`]; what follows from it is marked [I]:

- iOS 27 resolves hardware button presses (single, double, long) in the private framework **ButtonResolver**
  (`BRButtonResolverController`, `+[BRInterface interfaces]`: a `BRInterfaceLegacy`, a `BRInterfaceKeyboard` and, when
  `+[BRInterfaceAOP interface]` gives one, a `BRInterfaceAOP`). The AOP interface is made when MobileGestalt's
  `JwLB44/jEB8aFDpXQ16Tuw` answers 1, or with `initWithFastHaptics:YES` when `MGIsDeviceOneOfType` matches a list of
  device types (0x2569b3e20-0x2569b3ee4; which models the list holds is not decoded). It talks to the always-on
  processor's button and haptics drivers (strings `AppleSPUButtonDriver`, `AppleSPUHapticsAudioDriver`,
  `IOServiceMatched`), loads the press haptics through Haptics.framework's `HAButtonHapticsLoader`, waits for
  `HAButtonHapticsLoaderServiceIsAvailableNotification` (`-[BRInterfaceAOP _receiveLoaderAvailableNotification:]`),
  and tells its users when it is ready (`scheduleReadyNotificationOnDispatchQueue:withBlock:`). This guest has none of
  the SPU services.
- libMobileGestalt reads `closed-loop` (the `/product/haptics` node that `drivers/haptics` publishes with the
  Actuator device).
- [I, not proven] With `closed-loop = 1` the haptics loader expects the SPU haptics path, which never appears, so the
  resolver's haptic interface is never ready and the presses it should resolve are dropped; before the node, the
  loader answered "no haptic" (`HasHaptic: NO`) and the presses went on. The media agent's earlier reading
  (`BKButtonHapticsController`'s click states, harmless with `home-button-type 2`) is about another class.

**The test (needs the phone):** boot with `oneios-actuator=0` (no Actuator device, so no `/product/haptics` node:
`drivers/haptics` never publishes closed-loop without the Actuator route) and press the side key once (lock) and
once more (wake). If it locks and wakes, the haptics node is the cause; then `oneios_haptic_ui=0` is the same with
the kernel's UI haptics off as well. Read in iOS's log (backboardd, the logarchive procedure of `docs/newtodo.md`):
ButtonResolver's lines `HasHaptic: ...`, `... get asset components: ... haptic: ... success: ...` and any
`HAButtonHapticsLoader` line, with and without the switch.

**Fix candidates, by what the test shows:** (1) the node off by default (`oneios_haptic_ui` default 0) until (2) is
done: UI haptics then go through the vibrator path again; (2) keep closed-loop and tell iOS the side buttons have
no haptics: an answer for ButtonResolver's haptics query that says "none" (a userland hook in backboardd, built with
the Metal plug-in as `BacklightMode.inc` is) so that the AOP interface is ready without the loader; which call to
answer is the first `HasHaptic` / loader line of the log above. Nothing of this is built yet.

### Haptics do nothing (23:45 boot; fix-haptics, 2026-11-13)

On the phone every layer said "on" (`haptics: on: /dev/input/event5`, `ONEIOS HAPTIC: on`, the Actuator device and
`/product/haptics closed-loop = 1` published) and no line showed a request reaching any of them: no `Actuator: I/O
started`, no `play N`, and the `vibrator` service got nothing (with closed-loop on it is not used). Spotlight's log
had `AVHapticClient ... Got error during transaction` (subsystem `com.apple.corehaptics`).

**What iOS 27 reads, in 24A446** [V: llvm-objdump and ipsw on the 24A446 binaries, `oneios-tool image apfs-copy` for
the OS volume's files; the MobileGestalt table read from `libMobileGestalt.dylib`'s `__AUTH_CONST`]:

- MobileGestalt's two haptics answers come from our node only: **`DeviceSupportsHaptics`** (`cBy4BcYs5YWtFHbBpt4C6A`)
  is "`IODeviceTree:/product` has a child `haptics`", **`DeviceSupportsClosedLoopHaptics`** (`gqDnklGQnpv5ilgh5uHckw`)
  is that node's `closed-loop`. `supports-3rd-party-haptics`, `power-control-features` (`HapticsPowerControlFeatures`)
  and `pre-warm-disabled` are other keys of the same node. Before the Actuator change both answers were NO; since
  the 15:35 boot both are YES.
- There is no `hapticsd` in 24A446: no launchd job names one. Core Haptics' client (`AVHapticClient`, CoreHaptics)
  talks to the Mach service `com.apple.audio.hapticd`, which **audiomxd** publishes (`com.apple.audiomxd.plist`);
  VirtualAudio.plugin runs in audiomxd too. So `--proc audiomxd` holds the haptic server, VirtualAudio and the
  Actuator VAD.
- `Got error during transaction: %@` is logged by ten `AVHapticClient` calls (`loadHapticEvent:reply:`,
  `prepareHapticSequence:error:`, `loadAndPrepareHapticSequenceFromVibePattern:reply:`,
  `loadAndPrepareHapticSequenceFromEvents:reply:`, `createCustomAudioEvent:...`, `copyCustomAudioEvent:...`,
  `referenceCustomAudioEvent:`, `releaseCustomAudioEvent:`, `setChannelEventBehavior:`, `setSequenceEventBehavior:`)
  with the method's name before it and the server's NSError after it: the whole line says which transaction and which
  error. [I] A server whose haptic engine has no actuator port answers every transaction with an error.
- VirtualAudio, at its plug-in's creation (`New_VirtualAudio_PlugIn`), asks `MGGetBoolAnswer("DeviceSupportsHaptics")`
  and then waits up to 8 s for the actuator port: `Beginning loop to wait for actuator port`, then `Found actuator
  port!` or `Could not find actuator port after 8 seconds of waiting`. The port is made by `Device_Actuator_Aspen`
  (`Creating Actuator Port`) for the physical device whose UID is `Actuator`; its settings come from
  `ActuatorSettingsFactory_Aspen` by product type (exceptions `Invalid Product Type`, `Invalid ActuatorDatabaseType in
  platform attributes`). Earlier sessions logged `Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general`;
  the OS volume's `/System/Library/Audio/Tunings` has only `AID8022`, `Generic` and `IRs`, so [U] whether a missing
  strips file stops the VAD or only its DSP tuning.
- The V54 ADT's own Actuator is `arm-io/iop-audio-controller/audio-haptic` (`uid "Actuator"`, `model-name "leap"`,
  `identifier "chpa"`, `input-latency 8`, `output-latency 8`, `thermal-budget-range`, `peak-power-range`,
  functions `hpsc`/`pkpw`/`ther` on `arm-io/haptics-support-interface`), with `audio-hpdbg` ("Haptic Debug") and
  `audio-haptic-leap` (`AppleHapticsAudioInterface`). Our device publishes the UID and the stream; [U] whether
  `Device_Actuator_Aspen` reads any of the others (the CPMS lines `CPMS-Actuator: Get ThermalBudgetRange` suggest the
  thermal budget).

**Where it stops is not known yet: the 23:45 log has no line between the "on" lines and the motor.** So every layer
now counts (2026-11-13: host-tested; the hosted kernel built on the Linux PC (`HOSTED_OUT=fixhap`) and prelinked, audit 0 failures, `stubs-check` 666 good; no PC boot, not on the phone):

| Layer | Line | Says |
|---|---|---|
| Actuator's user clients (DroidVMAudio.cpp as staged) | `ONEIOS HAPTIC: Actuator: user client N opened by audiomxd`, `... asks for N channels at R Hz (the device: 1 at 48000 Hz: the same\|differs, refused ...)`, `... selector N refused`, `... maps memory type 0x...` (the first 24) | whether VirtualAudio opened the device at all, and whether it refused our format |
| The device's I/O | `ONEIOS HAPTIC: Actuator: I/O started by audiomxd (start N)` and `I/O stopped: ...` (as before) | whether a VAD ran I/O |
| The samples | `ONEIOS HAPTIC: counters: Actuator N opens (N closed, last by X), N selectors (N StartIO, N StopIO, N refused, N formats asked, last S), N maps; N periods (N ms), N with samples, N loud, peak P of 32768; N requests to the motor \| vibrator service N requests \| /dev/oneios-haptic N frames \| bridge N vib + N stop sent, N failed, N with no host actuator` (every 30 s while a figure moved; `oneios_haptic_report=0` leaves it out) | whether samples reached the ring (`with samples`), were loud enough (`loud`, above 256), became requests and went over the bridge |
| xnuhost's bridge | `haptics: effect N played on /dev/input/eventN: N ms, period N ms, level N (intensity N, N Hz asked)` (the first 8, then 16, 32, 64, ...), `haptics: counters: N requests from the guest (N vibrate, N stop; N replaced before they started), N effects played, N stopped, N failed on ...` (at 16, 32, ... and at the end), `haptics: 60 s: +N requests (+N vibrate, +N stop, +N impacts), +N played, +N failed` (xnuhost, every 60 s when they moved: docs/booter.md 2.9b) | whether the requests arrived and the force-feedback device took them |

**The power key (fault 2) and this fault** [V for what is read; I where marked]: backboardd's
`BKButtonHapticsController` is made in every boot, by backboardd's start-up step `ButtonHaptics` right after `HID
event processing`, independent of our node. Its init enters a dispatch group that only
`-[BRButtonResolverController scheduleReadyNotificationOnDispatchQueue:withBlock:]` leaves (log `_BRController ready:
device haptic asset slot count:%d`); with `home-button-type` 2 it queues `_configureAsyncWithBlock:`, whose block
waits on that group with no time limit on the serial queue `com.apple.backboardd.buttonHaptics`. Every later
`playHapticForClickState:clickSpeed:` goes through `_configureSyncWithBlock:` (`dispatch_sync` on that queue): until
ButtonResolver is ready, its caller blocks for good. Its caller is backboardd's MIG routine `_BKHIDXXPlayHaptic`
(clients with the entitlements `BKFastHapticPlayEntitlement` / `BKBackBoardClientEntitlement`). ButtonResolver
itself asks MobileGestalt only `HomeButtonType` (`JwLB44/jEB8aFDpXQ16Tuw`) and `MGIsDeviceOneOfType`; Haptics.framework's
`HAButtonHapticsLoader` waits for an IOKit service by property match (`IOServiceAddMatchingNotification`). **Neither
reads `/product/haptics`**, so the node does not change whether ButtonResolver gets ready. So the same missing piece
does not explain both by this reading: [I] what the node can change is who asks backboardd for a button haptic, or a
lock path that waits on audiomxd's haptic server (the transactions above). Not proven either way; the first test
decides:

- `oneios-actuator=0` (no Actuator device, so no `/product/haptics`: both MobileGestalt answers NO, as before 15:35).
  If the side key locks and wakes again, the node is the switch: then the `oneios-ubt=SpringBoard,backboardd` lines
  after a press name the wait (`_dispatch_sync_f_slow` under `playHapticForClickState:clickSpeed:` and
  `_BKHIDXXPlayHaptic` in backboardd; `dispatch_group_wait` in the `com.apple.backboardd.buttonHaptics` block; or a
  SpringBoard thread in an `AVHapticClient`/`AudioServicesPlaySystemSound` call). If it still does not lock, the node
  is not the cause of fault 2, and the haptics fault is a separate one.

### The power key: a stand-in for the button haptics service (fix-power-key, 2026-11-14; built, not on the phone)

The owner confirmed in the 9f6dc38 session that the power key neither sleeps nor wakes the display. The presses reach
iOS (`oneios-hid: buttons: ... 24 key events ... 24 HID reports`, `HID events queued ..., 0 dropped`), so the stop is
above the HID service, as in the 15:35 boot.

What ButtonResolver needs to get ready [V: ButtonResolver, Haptics.framework and the kernelcache of 24A446]:

- `+[BRInterfaceAOP interface]` makes the AOP interface when MobileGestalt's `HomeButtonType` is 1, or, with fast
  haptics, when `MGIsDeviceOneOfType` matches its list of device types. [I] iPhone18,2 (a Camera Control phone) is in
  that list: the list is hashes and was not decoded.
- `-[BRInterfaceAOP _findService]`: an `IOServiceNameMatching` notification for **`AppleSPUHapticsAudioDriver`** (fast
  haptics; else `AppleSPUButtonDriver`), then `IOServiceOpen` type 0 (`AOP service added: %u`). `updateReadyState`
  logs `%s isReady: %s loaderReady: %s`: ready needs the service **and** the loader.
- Haptics.framework's `HAButtonHapticsLoader` matches `IOPropertyMatch {"Button Haptics Asset Interface" = 1}` (a
  number), opens type 0, maps memory type 0 (`IOConnectMapMemory`), calls scalar method 1 (`GetNumberOfSupportedAssets`,
  one scalar out) and posts `HAButtonHapticsLoaderServiceIsAvailableNotification` (also if that call fails; not if
  the open or the mapping fails: `Unable to open user client err %d`). Assets go in through struct method calls
  (`UpdateSSHBAsset`).
- `-[BRInterfaceAOP playState:forSpeed:error:]` is scalar method 2 with three scalars in; the configuration goes in
  with `IORegistryEntrySetCFProperty` on the service.
- Apple's `AppleSPUHapticsAudioDriver` is in the kernelcache (the string is in `kernelcache.macho`; in our `kernel.kc`
  until 2026-11-25, when `prelink roots` left com.apple.driver.AppleSPU out: section 7, "Motion"), but it belongs to the
  always-on processor's kexts, which have no hardware to match here: neither
  service exists, so ButtonResolver is never ready and backboardd's `BKButtonHapticsController` queue waits for good
  (section above). Also read: backboardd's `_BKHIDXXApplyButtonDefinitions` does not wait on this home-button type
  (`not configuring SSHB because we don't have a real home button`); its `_BKHIDXXPlayHaptic` does.

**The stand-in** (`drivers/haptics/OneiOSButtonHaptics.cpp`, a driver-archive source, no XNU patch; **off by default**):
boot argument `oneios-button-haptics=1` publishes `IOService:/IOResources/oneios-button-haptics/AppleSPUHapticsAudioDriver`
with `"Button Haptics Asset Interface" = 1`; its user client takes every call (selector 1 answers the slot count,
`oneios-button-haptics-slots=N`, default 8; selector 2 and the others succeed, outputs zeroed), maps 128 KiB of type
0, and its `setProperties` keeps every key. Nothing is played on the actuator. Log:
`ONEIOS BUTTON HAPTICS: off (...)` or `ONEIOS BUTTON HAPTICS: on: AppleSPUHapticsAudioDriver published ...`, then
`user client N (type 0) opened by backboardd`, `backboardd calls selector N (call N; ...)` (the first 4 calls of each
selector), `backboardd sets KEY` (each new key).
Tested: compiled with the drivers (`oneios-tool kernel drivers --check`, 17 sources). **Whether it brings the power key
back is not known**: the power key worked before `/product/haptics`, and nothing above reads the node, so the stand-in
helps only if the blocked path above is the one the press takes. The next boots decide (A/B):

| Boot | Boot arguments | If the power key then works |
|---|---|---|
| A | as now, plus `oneios-ubt=SpringBoard,backboardd oneios-ubt-at=240 oneios-ubt-every=60` | (the control; the `oneios-ubt` lines after a press show where the threads wait) |
| B | A plus `oneios-button-haptics=1` | ButtonResolver's readiness was the stop: keep it (then make it the default) |
| C | A plus `oneios-actuator=0` (no Actuator device, no `/product/haptics`) | the node is the switch, by another path than ButtonResolver |
| D | A plus `oneios-bl-mode=0` (CoreBrightness keeps VMBLControl; the slider then does nothing again) | the backlight control's mode is the switch (below) |
| E | A plus `oneios-hid-buttons-builtin=1` | backboardd did not route the buttons because it took them for an external keyboard (below) |

**The buttons look like an external keyboard to backboardd** [F, the PC boot of 2026-11-14 with `haptics=sim`,
`oneios-button-haptics=1` and the input script; iOS's log read with `oneios-tool ioslog show ... --proc backboardd`]:
`Services discovered: [<BKIOHIDService ...; eventSource: externalKeyboard; primaryUsagePage: 0x1; primaryUsage: 0x6;
name: OneiOS buttons; transport: Virtual>` (the touch screen: `eventSource: builtin; ... authenticated; builtIn`), and
every volume press as `VolumeIncrement page:0xC usage:0xE9 downEvent:1 source:externalKeyboard senderID:100000219
down`, `0xC/0xE9/100000219 began: ... destinations:<none>`. No power-key line came from the button processor in the
8 s between backboardd's start-up and the end of that boot, so the power key's own route on the PC is not seen, and
SpringBoard had only just become the shell, so `destinations:<none>` may be early. [I] An external keyboard's
consumer keys are not the hardware buttons SpringBoard listens to. `oneios-hid-buttons-builtin=1` (the HID archive,
`kernel/patches/oneios-hosted/hid/OneiOSHidInput.cpp`, off by default) publishes `Built-In` and the Consumer Control
pair (0x0C, 1) on the buttons' event service and interface, as the touch screen's are, once the event driver has
attached (log `oneios-hid: buttons: Consumer Control, Built-In published on the event service`). The device's own pair
(1, 6), which the personality matches, stays. Also seen in that boot: ButtonHaptics was `ready in 0.73 s` and
ButtonResolver opened two HID event system connections, but nothing opened the stand-in `AppleSPUHapticsAudioDriver`
before the boot ended (8 s later), so whether ButtonResolver uses the AOP interface on iPhone18,2 is not settled.
The decisive line on the phone: backboardd's `BackBoard:Button` line of a power press (`page:0xC usage:0x30 ...
source:... destinations:...`) in the 9f6dc38 log.

**The 15:35 boot changed two things, not one** [F git]: it was the first with the Actuator (5e426f3, 15:01) and the
first with `drivers/brightness/BacklightMode.inc` in the Metal plug-in (7d6d6a3, 15:23), which turns CoreBrightness's
VMBLControl into BLControl in backboardd. A lock's display-off and a wake's display-on go through backboardd's
brightness control, so [I] the power key may stop there as well: SpringBoard's lock asks backboardd for the backlight
off, which BLControl now sends to our backlight service (as brightness 0 or a display power change), and the press
"does not sleep the display" if that request is taken but the panel is not blanked. The brightness bridge never writes
0 to the panel (`never 0`, section 7a), so a display-off would show as the lowest brightness, not as a dark panel. The
9f6dc38 session's "a brightness of 0 put the touch IC to sleep" may be that display-off. So read, before any boot,
whether the 9f6dc38 xnuhost log has a brightness request of 0 (`ONEIOS BACKLIGHT` / `brightness:` lines) within a
second or two of a power press (`oneios-hid: buttons:` at +5083..+5175). If it does, iOS locked and the stop is ours:
the panel is not turned off and on (the bridge's side, fix-brightness-touch's area).

What a lock looks like in iOS's log on its way [V strings of SpringBoard.framework and backboardd 24A446; the order is
[I]]: backboardd `UISensor mode changing to: ... changeSource: LockButton` (seen in 9f6dc38: the UI mode change comes
with the lock button), then SpringBoard `Lock button down: state=%d`, `Lock button single press should wait for
double-press timeout: ...`, `Lock button single press recognized.`, `Lock button single press: state=%d`,
`lockUIFromSource:%{public}@ options:%{public}@`, then backboardd's backlight factor change and CoreBrightness. A
wake: `Button press up; waking screen.`, `unlockUIFromSource`. The first of these that is missing names the step.

In iOS's log, B should show backboardd's `_BRController ready: device haptic asset slot count:8` (a default-level line), ButtonResolver's (debug level: they may not be stored)
`AOP service added`, `isReady: YES loaderReady: YES`, and Haptics' `UpdateSSHBAsset` without `err`. What a working
press produces [I, from backboardd's and SpringBoard's strings, not traced]: SpringBoard's lock path asks backboardd to
turn the display off (backlight to 0 and the framebuffer's power state off; on the next press, on); the display and
backlight drivers see that as a brightness of 0 and then a brightness above 0, or a framebuffer power-state change
(`kernel/patches/oneios-hosted/display/README.md`, the power word).

### Core Haptics' server gave up at its start: no preset for "oneios,xnuhost" (fix-hapticd, 2026-11-15)

**The fault (phone, main 8644ae9, 2026-11-15):** typing lags in every text field (the Wi-Fi password sheet, Settings'
search) with Preferences at 102-110 % of a CPU, and no haptic is ever felt (keyboard, system, buttons). iOS's log:
Preferences 180 and SpringBoard 42 times `[com.apple.corehaptics:hcln] AVHapticClient.mm:1138 operator(): Initial XPC
call to server timed out. Invalidating connection to prevent hang` with `Default XPC timeout` and `-[AVHapticClient
setupConnectionWithOptions:error:]: [xpc] Got error during transaction: ... Code=4099`, in bursts while typing, each
with a new connection to `com.apple.audio.hapticd`; with keyboard haptics off still SpringBoard 114 and Preferences 18
(8644ae9-b); HangTracer: Preferences 38 hangs (251 s, the longest 61 s), SpringBoard 25 (71 s).

**The cause [F, iOS's log of a PC boot with `haptics=sim`, 2026-11-14, `oneios-tool ioslog show ... --proc
audiomxd`]:** the Mach service `com.apple.audio.hapticd` is audiomxd's haptic server (Core Haptics' server). At its
start it loads its synth preset from `/Library/Audio/Tunings/<hardware model>/Haptics/hapticengineconfig.plist`, and
the guest's hardware model is the device tree's first `compatible`, `oneios,xnuhost` (xnuhost's loader, with no
identity tree: the same name as in `Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general`):
```
audiomxd  HapticServerManager.mm:41  === Initializing Haptic 2.0 Engine ===
audiomxd  AVHapticServer.mm:2014  -[AVHapticServer loadSynthPreset]: ERROR: Unable to load the configuration plist: ...
          NSFilePath=/Library/Audio/Tunings/oneios,xnuhost/Haptics/hapticengineconfig.plist ... No such file or directory
audiomxd  AVHapticServer.mm:2036  -[AVHapticServer incrementInit:]: ERROR: failed to load haptic preset
audiomxd  AVHapticServer.mm:1848  -[AVHapticServer init]: ERROR: Failed to set up SSS client -- server bailing out
audiomxd  HapticServerManager.mm:43  ERROR: Haptic Engine & Server Failure
```
launchd still holds the service's port (`com.apple.audiomxd.plist`'s MachServices), so a client's first call waits
for a reply that never comes, its default XPC timeout, on the calling thread: for the keyboard that is the app's main
thread at each key (the lag and the CPU), whatever the haptics setting (the keyboard and SpringBoard make their haptic
clients either way). Nothing reached the Actuator device: every `ONEIOS HAPTIC: counters:` line had `0 StartIO`. The
OS volume has the iPhone's own folder: `/Library/Audio/Tunings/V54/Haptics` (the preset and the tap, keyboard and
wheel waveforms; the preset names them by absolute `/Library/Audio/Tunings/V54/Haptics/...` paths), and nothing else
under V54. Not the cause: the Actuator IOAudio2Device (audiomxd opened it, mapped its memory and built
`VirtualAudioDevice_Actuator` and its port before the server's start: `Created and published port: [ ... type: pact;
uid: Actuator ...]`), the `AppleSPUHapticsAudioDriver` stand-in (`oneios-button-haptics=1`; backboardd's ButtonHaptics,
another path: `ButtonHaptics ready in 0.73 s`), the haptics bridge (it never got a request), and HAL's `HALS_UCPlugIn
... construction failed, probably because this isn't a server plug-in` (three of the OS's HAL bundles that are not
server plug-ins; the PC's boot has them too).

**The fix (hosted 0078, `kernel/patches/oneios-hosted/README.md` "0078"):** before launchd the kernel makes the Data
folder `/private/var/oneios/addons/AudioTunings` with one link, `oneios,xnuhost -> V54`, and shows it read-only over
`/Library/Audio/Tunings` (0031's union mount): `/Library/Audio/Tunings/oneios,xnuhost/Haptics/...` is then the
iPhone's own sealed folder. Nothing of Apple's is copied; an existing Data store gets it at its next boot.
`oneios-audio-tunings=0` turns it off (the A/B switch). Why not the board name in `compatible` instead: `V54AP` first
would make `hw.model` the iPhone's and every per-board table (CoreMotion, MediaExperience, the speaker's VAD tunings)
and possibly an original platform kext would follow it (`kernel/profiles/iphone18-2-ios27.md`); the link changes only
the haptics folder.

PC_RESULTS_HERE

### The Actuator played silence: no DSP chain for it (fix-audio-volume-haptics, hosted 0086, 2026-11-22; built, not on the phone)

**The fault (phone, 2026-11-22, kernel with 0082 + 0083, 0078 in place):** no haptic is felt. 0078 works: audiomxd's
haptic server starts (`=== Initializing Haptic 2.0 Engine ===` and no failure), clients connect, and SpringBoard's and
Settings' players start with haptics on (`start: client ID: 0x... audio: 0 haptic: 1`, `startSynthRunning: Calling
startRunning(0, 1, 0x1) on synth`). The Actuator device runs I/O (`ONEIOS HAPTIC: counters: Actuator 1 opens ..., 9
selectors (4 StartIO, 3 StopIO, ...), 61920 periods (309600 ms), 0 with samples, 0 loud`), and every period is
silence: audiomxd's own meter says the same at the HAL (`RTAID [ ... node=Actuator-Output ... ] -- [ rms:[-120.000000],
peaks:[-240.000000] ]`, 30 times in the s7 boot). xnuhost's `haptics: 60 s: +1 requests (+0 vibrate, +1 stop, ...),
+0 played` follows from it. Not the cause: the keyboard (its client is in audio-only mode, `Muting haptics on client
ID ... AudioOnly 1`: iOS's keyboard haptic feedback is off by default, Settings > Sounds & Haptics > Keyboard Feedback),
the device's format (no refused selector, no format asked), the Actuator's open (1 open, its memory mapped).

**The cause [F iOS's log; V 24A446's VirtualAudio.plugin and the OS volume's tunings]:** VirtualAudio makes the
Actuator's virtual stream with a DSP chain and finds no strips file for it:
```
audiomxd StreamUtilities.cpp:402  Created virtual stream for [vzzz] (cbkg/imdf) port pact with DSP chain 'd42_actuator_general', ...
audiomxd RoutingSettings_Aspen.cpp:700  Cannot find strips file oneios,xnuhost/VAD/d42_actuator_general
```
(the lookup is `/Library/Audio/Tunings/<hardware model>/VAD/<chain>`, then `Generic/VAD/<chain>`: the two `Cannot find
strips file` formats of the plug-in). The virtual stream has 2 channels (`forced virt chans: 2`, the haptic synth's
2-channel haptic bus: `LegacyHapticSynth ... haptic output bus format to 2-chan`) and the physical Actuator 1; the
chain is what turns one into the other. The iPhone's own chain is on the OS volume:
`/Library/Audio/Tunings/AID8022/VAD/actuator_general.dspg` wires `in` (2 channels) -> `AUHapticsProtection` -> 1
channel -> `AUVolumeForHaptics` -> `out`, its `.austrip` holds the two units' presets, and `AID8022/VAD/
graph_configurations.plist` says the same (`actuator_general`: busChannelCounts 2 -> 1). AID8022 is the V54's
`/product/audio acoustic-id 8022`; the guest has no `/product/audio` (`IORegistryEntryFromPath IODeviceTree:/product/
audio returned MACH_PORT_NULL`), so VirtualAudio uses the hardware model and its legacy chain name. [I] Without the
chain the stream's two channels do not reach the one-channel device; that this is the only cause is not proven until
the phone boot below.

**The fix (hosted 0086, `kernel/patches/oneios-hosted/README.md` "0086"):** 0078's link `oneios,xnuhost -> V54` in the
Data folder `/private/var/oneios/addons/AudioTunings` becomes a folder of links, still shown read-only over
`/Library/Audio/Tunings`: `Haptics -> ../V54/Haptics` (the preset, as before), `VAD/d42_actuator_general.austrip ->
../../AID8022/VAD/actuator_general.austrip` and `.dspg` the same. Nothing of Apple's is copied; an existing store's
0078 link is replaced at the next boot. `oneios-audio-tunings-vad=0`: only the Haptics link. Not chosen:
`/product/audio acoustic-id 8022` in the guest tree, which would hand every route the iPhone's tunings (the speaker's
protection needs its `syscfg` Thiele-Small values, the microphones' beam forming three microphones; we have one).

**Tested:** the patch applies after 0083 with and without the camera helper's 0084 (both orders);
`HOSTED_OUT=audio kernel/build-hosted.sh` (Mac, 2026-11-23, 962 s: `xnu.macho` sha256 `dbf9fc9f...`, no warning in
the new code, its strings in the kernel); `KERNEL=out/kernel/current-hosted-audio/xnu.macho OUT=out/kernel/audio-24A446
kernel/prelink/build.sh`: `kernel.kc` 96,845,824 bytes, sha256
`a50202ea0aa3a71c3529222abf4fc74a9e5bced8e8c80a59f6252369072101c8`, audit 2712 paired, 0 failures; `prelink
stubs-check` 666 good, 0 bad; `kernel/tests/host.sh` 8 of 8. The same `kernel.kc` has the Speaker's volume control
(section 6, "Volume"). Not on the phone.

**The phone check:** in `xnuhost.log` `oneios-audio-tunings: ... oneios,xnuhost/Haptics -> V54/Haptics,
oneios,xnuhost/VAD/d42_actuator_general -> AID8022/VAD/actuator_general (N entries made), the Actuator's strip
visible; stage 5, status 0`; in iOS's log (`oneios-tool phone pull-log`) no `Cannot find strips file
oneios,xnuhost/VAD/d42_actuator_general`, and RTAID's `node=Actuator-Output` with a level above -120 dB when a haptic
plays (a toggle in Settings, a long press on an icon); then `ONEIOS HAPTIC: counters: ... N with samples, N loud,
... N requests to the motor` with N > 0, and xnuhost's `haptics: 60 s: ... +N played` with N > 0. Keyboard taps need
Settings > Sounds & Haptics > Keyboard Feedback > Haptic on (iOS's default is off). If the strip is visible and the
Actuator still gets silence, the next reading is VirtualAudio's DSP set-up lines for `[vzzz]` in the same log.

### The boot of 2026-11-23: no haptic was asked for (fix-haptics-waveform, 2026-11-23)

**What the boot was read as:** main `44d9c49` with 0086 on the phone, `ONEIOS HAPTIC: counters: Actuator 1 opens ...
(1 StartIO, 1 StopIO ...), 2689 periods (13445 ms), 0 with samples, 0 loud`, so "the Actuator still plays silence".

**What iOS's log of that boot says [F, `ioslog haptics` on its `phone pull-log` text]:** 0086 works, and the boot
played no haptic at all.
- The chain is there and well formed: no `Cannot find strips file`, then `DSPChainConfig:d42_actuator_general ...
  [dpio] {2, 1} ... [dprt]: 1751216240` (`happ`, AUHapticsProtection, 2 channels to 1) and `[dpio] {1, 1} ... [dprp]
  {[64002]: 1} ... [dprt]: 1987013737` (`voli`, AUVolumeForHaptics), `Instantiated DSP Processor type 'happ'` and
  `'voli'`, the mapping `0v: 32/48000/2` to `0a: 16/48000/1`.
- The one I/O run is SpringBoard's prewarm: `prewarm engine <_UIFeedbackCoreHapticsHapticsOnlyEngine>` at 21:59:52,
  `Starting prewarm on cold client ID`, `StartIOProcID: 79 Actuator` at 21:59:52.94, `Stopping prewarmed client` and
  `StopIOProcID` at 22:00:06.39: the 13445 ms. A prewarm plays nothing, so its periods are zero by design.
- No haptic was sent: `engine: made 1, prewarm 1, start 0, events 0`, `server: start 0`. The one feedback of the boot
  (`UINotificationFeedbackGenerator`, the power-connected sound, SSID 1106) went to `_UIFeedbackSystemSoundEngine`,
  and MediaExperience gave it `Vibration = 0` (`mShouldVibe 0`). The session had 28 touch contacts and SpringBoard
  logged no feedback after 22:00:06. The keyboard says `Haptics: supported. Haptics: disabled` (iOS's default).

So the waveform's path after the synth (the chain, the device's copy, the bridge) is **not tested yet on the phone**.
[U] whether AUHapticsProtection gives a level without the iPhone's actuator calibration: the graph form of the chain
(`AID8022/VAD/actuator_general.dspg`) wires two properties from the host into the units (`hprt` to
AUHapticsProtection 88000, `acst` to AUVolumeForHaptics 64003) that the strip form does not set, and `/product/haptics
controller-coeff-cal "syscfg/CLCL"` names a calibration the guest does not have. AUVolumeForHaptics' preset has `_swv`
0, `volt` 3, `volv` 3; `failed to fetch software volume command ... port pact` is logged once at the route's set-up
(its effect is not known).

**The lasting tool:** `oneios-tool ioslog haptics` (`tools/oneios-tool/README.md` "haptics"; `phone pull-log` writes
`ioslog-haptics.txt`): one line per step of the path, each step's count shown, so the first 0 is where a haptic stops.

**The phone check (the test this boot did not make):** in the session, press an icon on the Home Screen long (the
context menu's haptic), move a switch in Settings, and with Settings > Sounds & Haptics > Keyboard Feedback > Haptic
on, type. Then `oneios-tool phone pull-log DIR` and read `DIR/ioslog-haptics.txt`:
- `feedback: ... core haptics N` and `engine: ... events N` with N > 0: a haptic was sent (else nothing was asked);
- then `device: ... stopped with requests N`, `ONEIOS HAPTIC: counters: ... N with samples, N loud ... N requests to
  the motor` and xnuhost's `haptics: 60 s: ... +N played` with N > 0: the path works;
- `events` above 0 with `level: silent N` and `device: stopped silent` only: the chain gives silence. The next fix is
  then our own strip for `oneios,xnuhost/VAD/d42_actuator_general` without AUHapticsProtection (0086's folder of
  links can hold a file of ours in place of the link), not a third reading of the same logs.

Not done here: a boot on the PC with a scripted long press (`haptics=sim`, 2026-11-23) was stopped at the owner's
word before SpringBoard had made its engine (a first boot on a new Data store: 38 minutes, audiomxd had opened the
Actuator, `server: up 1`, `chain: made 1`, no feedback asked yet).

### The bridge (`runtime/bridges/haptics/haptic.rs`)

- Two backends since 2026-11-15 (section 7e): samsung, the evdev device below (the default, unchanged); treble, the
  vibrator HAL `android.hardware.vibrator.IVibrator/default` (`on(ms)`, then `setAmplitude(level / 255)` when its
  capabilities have amplitude control, `off()`; `Haptic::open_as`, `haptics/vibrator.rs`).
- Finds the device by its EV_FF bits (`sec_vibrator_inputff` first), sets `FF_GAIN` 10000 once.
- One effect, updated for every request: `FF_PERIODIC` / `FF_SINE`, period = 1000 / frequency clamped to 4..10 ms
  (6 ms when iOS names none), magnitude = 40 + 215 x intensity / 1000 (an LRA does not move below a floor), length
  = the request's (at most 10 s). Then `EV_FF` play; stop = play 0. A worker thread does the ioctls (each is I2C
  traffic), the newest request wins.
- The motor is stopped when the guest resets the device, when the bridge closes, and by the kernel when the file
  closes (xnuhost exits).
- Counters (2026-11-13): requests from the guest (vibrate, stop, replaced before the worker started them), effects
  played, stops, failures; the first 8 effects and every power of two after are logged (`haptics: effect N played on
  ...`), and `haptics: counters: ...` at 16, 32, ... effects and when the bridge closes.
- `haptics=` in `xnuhost.conf` (or `--set`): `auto` (the device above, with the bridges on), `off`, `sim`: **a
  stand-in actuator** for a boot with no vibrator (the PC), with the bridges on or off; it takes every request,
  counts and logs it, and makes the guest's whole path run (the hello names the haptic channel, so `drivers/haptics`
  publishes the Actuator device and `/product/haptics`). Host test: `runtime/bridges/haptics/test_haptic.rs`
  (the C's `test_haptic` in `test_bridges.c`: the counters of the fake motor and of the stand-in, and the bridge channel's).

| iOS | Reaches the phone as |
|---|---|
| Ringtone, alert and system-sound patterns (`hertz_millisecs`) | on steps as sine effects at the step's intensity; off steps as silence |
| Core Haptics continuous events (through the Actuator device) | sine effects following the stream's level and frequency, renewed every 30-40 ms |
| Core Haptics transients, UI and keyboard taps (Actuator device, 3-20 ms bursts) | a 20 ms sine burst at the tap's peak. [U] how sharp it feels; the ROM "click" waveforms (`FF_CUSTOM` `[1, index]`) are the better fit and are not used yet |
| Sharpness | frequency, within 100..250 Hz |

### Missing, unknown

- [U] the LRA's resonance (attribute `f0_measured` of class `sec_vib_inputff`): 6 ms (166 Hz) is a guess.
- [U] whether the buzz generator stops fast enough for taps; if not, map bursts under about 25 ms to ROM clicks.
  The table of ROM/RAM indices is Samsung's firmware (`cs40l26.wmfw`/`.bin` on the phone), not in the kernel source.
- Not run on the phone yet: the Actuator device (above). [U] whether iOS 27's VirtualAudio wants the `ActuatorIV`
  input (current and voltage sense) as well; HackiOS ran iOS 16 without it. [U] whether the HAL asks the Actuator for
  more than one channel (`LegacyHapticSynth: ... output bus format to 2-chan` in iOS's log is the synth's bus, not
  the device's): `oneios-actuator-channels=2` if the log shows a refused format (selector 5).
- [U] iOS 27: `hertz_millisecs` is still in the iOS 26.5 MediaExperience [F iOSPlus10]; that MobileGestalt's
  closed-loop answer still comes from `/product/haptics` is [V] for iOS 16 only.
- The key stop (`oneios_vibrator_key`) is not wired: the HID input driver must call it on power and volume presses.
- The AIDL vibrator HAL is the second route (`IVibrator.compose`, `perform`): it needs a binder client for the AIDL
  binder (`/dev/binder`, another parcel format than `bt/hidl_parcel.c`). Not written; evdev as root is the first.

## 6. Audio

### The phone [F]

- One ALSA card, `pineapple-mtp-snd-card`. Codec WCD939x (`qcom,wcd939x-codec`, DT line 24679; modules
  `wcd939x`/`wcd938x`), speaker amplifiers WSA884x on SoundWire (`qcom,wsa884x`, lines 24550 and 24621; modules
  `wsa884x`/`wsa883x`); the DT also carries two Cirrus `cs35l43` nodes (lines 25216, 25249) named by the sound node's
  `cirrus-codec-name`. LPASS macros `lpass-cdc-{rx,tx,va,wsa,wsa2}-macro`. The DSP graph is AudioReach
  (`android_vendor_qcom_opensource_agm`, `arpal-lx` in the same GitHub organisation).
- `audioserver` runs in a session with `media.audio_flinger` and `media.aaudio`; Samsung's `ISecAudioHal` is the HAL.
- Android 17 (SDK 37). Properties: `aaudio.mmap_policy` 2 (MMAP when it can), `aaudio.mmap_exclusive_policy` 2,
  `audio.deep_buffer.media` true (a media track with no flags goes to the deep-buffer output). libaaudio exports
  `AAudio_setMMapPolicy` and `AAudioStream_isMMapUsed` (AAudioTesting.h). The media stream's volume was 14 of 15
  (2026-11-12).

### iOS side

UniXNU-droidvm f9121cd's `package/DroidVMAudio.cpp` (two `IOAudio2Device`s, UIDs "Speaker" and "Mic", which
VirtualAudio turns into routable ports) over `DroidVMVirtioSnd.c` (virtio-snd transport, node `/arm-io/virtio-snd`,
compatible `droidvm,virtio-snd`, 48 kHz S16, stereo out, mono in, periods of 240 frames, polling).

**Until 2026-11-12 neither was built into the hosted kernel**: iOS had no audio device at all. The five session logs
on the phone of 2026-11-11/12 show `audio: on: helper ...` and then nothing: the guest never started a stream. Now
they are the hosted build's audio archive, `oneios-tool kernel gen audio`
(`liboneiosaudio.a`, force-loaded; `oneios_drivers_start` calls `oneios_audio_start`, weak in `OneiOSBridge.c`):
the transport unchanged (`-DDROIDVM_DEVICE_DYNAMIC=1`), `DroidVMAudio.cpp` staged with boot arguments, and
`OneiOSAudio.c` (the start line and a report every 30 s while the counters move). Kernel log lines:
`ONEIOS AUDIO: on|off: virtio-snd ... IOAudio2 devices: ...`, `DroidVMAudio: published Speaker=1 Mic=1 ...`,
`ONEIOS AUDIO: speaker running|idle, ... engine N periods, N late, ...; virtio-snd N tx periods, N dropped ...`
(the speaker's and the mic's periods, and those of them taken over a period late; until 2026-11-24 `late` also
counted the Actuator's), and `ONEIOS AUDIO: late periods: ...` ("Late periods" below).
Boot arguments: `droidvm-snd=0`, `droidvm-audio=0`, `oneios-audio-out-uid=`, `oneios-audio-in-uid=` (default
Speaker, Mic; `none`: no microphone), `oneios-audio-latency=FRAMES` (the speaker's "output latency", so iOS keeps
video in step: since 2026-11-15 xnuhost passes the helper's measured latency, its probe before the boot; without it
the default 720 = 15 ms, the MMAP path's prediction; 6480 = 135 ms before), `oneios-audio-ring=FRAMES` (the
speaker's ring, default 1920: iOS's I/O buffer is at most 0.375 of it, "Latency end to end" below),
`oneios-audio-rt=0` (the engines at kernel priority and their wait `clock_delay_until`'s, as before 2026-11-24;
default 1: "Late periods" below), `oneios-audio-report=0`, `oneios-audio-selftest=SECONDS`
(1..10: 3 s after the start a 1 kHz sine at -30 dBFS straight into the transport, before iOS's audio server: the
guest kernel to the speaker without iOS; UniXNU-droidvm's `droidvm-snd-selftest` is a full-scale sawtooth, never with
a speaker).

VirtualAudio publishes only its Null device when the guest says it is a VM (`vmm-present` 1 in xnuhost's tree):
the boot argument `owner-hide-vmm=audiomxd,com.apple.WebKit.GPU` (lead's decision of 2026-11-11, Droid-VM-UniXNU's
`patches/xnu-12377/zzyd-owner-hide-vmm.patch`, in the arch series the build uses) hides it from audiomxd. xnuhost's default boot arguments do not carry it
yet: the audio test cycle adds it (below).

### The bridge

- `runtime/bridges/devices/virtio_snd.rs`: the device (control queue: `PCM_INFO`, `SET_PARAMS`, `PREPARE`, `START`,
  `STOP`, `RELEASE`; two streams; output periods go to the back end as they arrive; a capture thread fills the
  receive buffers the guest posted).
- `runtime/bridges/audio/aaudio_helper.rs` (the bin `audio/oneios_audio.rs`) -> `bin/oneios-audio`: **a helper process**, because AAudio is a shared
  library and xnuhost is static. xnuhost starts it (`AudioHelper::spawn(path)`, `posix_spawn`, a `SOCK_SEQPACKET`
  pair); it lives in xnuhost's namespace and cgroup, and the return's SIGKILL of the namespace (or xnuhost's exit,
  which closes the socket) ends it and with it the stream: audioserver closes a dead client's track.
  - **No realtime thread, by construction.** AAudio makes a SCHED_FIFO thread only for a data callback (an MMAP
    stream's or a FAST track's: `AudioStream::createThread_l` registers it, which asks system_server's
    `scheduling_policy` service; in a session that service is gone). So the helper uses no data callback: one
    blocking `AAudioStream_write` from its own SCHED_OTHER thread, with a uclamp.min boost (`ONEIOS_AUDIO_UCLAMP`,
    512 of 1024, `sched_setattr` with the policy and nice kept, as `runtime/bridges/cpu`'s vCPU hint). Measured
    beside One UI (2026-11-12): every thread of the helper SCHED_OTHER (policy 0), the threads in the parent's
    cpuset; AudioTrack sets the starting thread's nice to -16 (`ANDROID_PRIORITY_AUDIO`, not realtime). A guard
    checks every thread after each start and every 10 s and sets one found SCHED_FIFO or SCHED_RR back to
    SCHED_OTHER (logged).
  - **Since 2026-11-15 (fix-audio-latency): LOW_LATENCY and MMAP exclusive by default**, the path in "Latency end
    to end" below: MMAP exclusive (the DSP reads our buffer; nothing asks for a priority), MMAP shared only while
    system_server runs (audioserver's endpoint thread registers itself through `scheduling_policy` and would wait
    for it forever in a session), else a mixer track (MMAP refused, LOW_LATENCY: what the rest of this bullet
    describes). `ONEIOS_AUDIO_MMAP=exclusive|shared|off`, `ONEIOS_AUDIO_PERF=none` (the path before 2026-11-15).
  - **Performance mode before 2026-11-15: NONE by default; LOW_LATENCY opt-in since 2026-11-13** (`ONEIOS_AUDIO_PERF=low`, not run on the phone), asked only when the MMAP
    refusal worked (AAudio tries MMAP only for LOW_LATENCY; a stream that says `AAudioStream_isMMapUsed` anyway is
    closed and opened again with NONE). What it gives a blocking writer, read in AOSP `main` (2026-11-13; not read
    in One UI's own build): AAudio's legacy path (`AudioStreamTrack::open`) sets `AUDIO_OUTPUT_FLAG_FAST|RAW` for
    usage MEDIA with no session, but with `TRANSFER_SYNC` ("Note that TRANSFER_SYNC does not allow FAST track");
    `AudioTrack::createTrack_l` then takes FAST off itself ("AUDIO_OUTPUT_FLAG_FAST denied by client": a synchronous
    writer gets FAST only from a Java thread) and sends no client thread for a priority boost (`clientTid` is set only
    for a FAST callback thread). So there is no FAST mixer track and no SCHED_FIFO request: the track is a normal
    mixer track, AAudio reports performance mode NONE (the open line says `low latency asked, none given`), its burst
    is the normal track's notification period. What is left of LOW_LATENCY is `AUDIO_OUTPUT_FLAG_RAW`: the audio
    policy's `selectOutput` prefers an output whose flags have the most requested performance flags, so if the
    speaker's `fast` mixPort declares RAW the track goes on that output's normal mixer (20-24 ms periods,
    AudioFlinger's `kMinNormalSinkBufferSizeMs`) instead of `primary-out`; if not, nothing changes (the earlier phone
    run of LOW_LATENCY, 2026-11-12, saw "the same normal-mixer track"). Whether One UI's `fast` mixPort has RAW: not
    known ([U]; `grep -B2 -A6 'name="fast"' /vendor/etc/audio_policy_configuration.xml` on the phone says).
  - Output (rewritten 2026-11-13, fix-audio-latency): the guest's periods go into a ring; the writer moves 240-frame
    chunks into AAudio (woken by each period, not on a 2 ms poll). **The level follows the server's pulls**: a
    normal mixer track is read a whole mixer period at a time and only when it holds one (AudioFlinger's
    `prepareTracks_l`: `desiredFrames` = `mNormalFrameCount`), so `AAudioStream_getFramesRead` jumps once per
    period. The writer measures the jumps (steps less than 3 ms apart are one pull; the median of the last 16) and
    the time of the last one, and holds the level (ring + AAudio's unread frames) at the target
    (`ONEIOS_AUDIO_TARGET_MS`, 20 ms) above what the next pull takes of what came in since the last one: right
    after a pull only the target. Before that it was a burst plus the target at every moment, so the level the
    guest's audio started on was up to a whole period (on average half) higher than needed, and then the writer
    filled AAudio to its 2-burst buffer. Until the pulls are seen (a new stream's first ~170 ms) the old rule holds.
    Priming (before the guest's first chunk, while it is stopped, after an underrun) keeps that level with silence.
    An underrun (the level below what the next pull takes) plays what the ring has, primes again and raises the
    target by 5 ms (at most 80); **each `ONEIOS_AUDIO_TARGET_DECAY_S` (20 s) without one lowers it by 5 ms back to
    its floor** (before, it only went up for the stream's life, and the stream lingers). Drift and excess: the level
    averaged over 1 s is held within 5 ms of its aim by playing a chunk of 240 ring frames as 239 or 241 (237 while
    10 ms above: 1.25 %; linear interpolation). **Silence is cut, sound is not**: while the level is 10 ms above its
    aim, a chunk at the ring's head that is digital silence (every sample within ±1) is dropped, so a backlog (a new
    track's stall, a burst of late periods) goes at the first pause instead of over 16 s at 1.25 %. **AAudio's
    buffer** (AudioFlinger's start threshold, and the most a blocking write leaves queued) is a burst plus the
    target plus one chunk, set again when the target moves (`ONEIOS_AUDIO_BURSTS=N` fixes it at N bursts as
    before). Above the target plus 60 ms the ring drops its oldest audio to the target (an overflow). Until the
    server takes the stream's first frames (a newly opened track takes nothing for about 200 ms) nothing is cut:
    the ring holds 500 ms, and after the stall its backlog (plus 40 ms) is the limit. A partial blocking write (a
    stall over 200 ms) is finished, not dropped. The guest's STOP keeps the stream open for `ONEIOS_AUDIO_LINGER_S`
    (60 s): what the ring holds plays, then silence; iOS stops and starts its device around each sound, and a
    reopened track stalls again. The price is One UI's speaker path kept on with silence for that minute.
  - Input: a reader thread, blocking `AAudioStream_read` of 240 frames, input preset VOICE_RECOGNITION, the first
    channel to xnuhost, performance mode NONE. `audio/backend.rs` drops what an earlier recording left in the socket at
    each input START.
  - Log (xnuhost's log; the numbers are the host test's): `oneios-audio: ready: performance mode low latency, MMAP
    exclusive first (shared only with system_server) (a blocking writer at SCHED_OTHER, uclamp.min 512: no data
    callback, no realtime thread), AAudio buffer a burst + the target + 5 ms, ring 500 ms, target 5 ms on MMAP (a new stream at 10), 20 ms
    on a mixer (at most 80, down 5 ms per 20 s without an underrun), linger 60 s`; `oneios-audio: probe: speaker
    latency 720 frames (15 ms) on MMAP exclusive: the DSP reads the buffer: 5 ms below AAudio's read + 10 ms of level
    when a period comes in` and xnuhost's `audio: speaker latency 720 frames (15 ms) measured by the helper:
    oneios-audio-latency=720`, then `oneios-audio: iOS is told an output latency of 720 frames (15 ms)`;
    `oneios-audio: speaker open: 48000 Hz, 2 channel(s), low latency asked, low latency given, sharing exclusive,
    mmap 1, burst 192, buffer 672 of 3072 frames, device 3, session -1; MMAP exclusive: the DSP reads the buffer`
    (on a mixer: `...; a mixer track (MMAP not given)` or the reason MMAP was passed over); on a mixer, once the
    server has pulled five times, `oneios-audio: speaker: the server pulls N frames every N ms: the level follows its
    pulls`; `oneios-audio: speaker: target N ms (20 s without an underrun)`; every 10 s and at each stop `speaker
    running|stopped ...: frames in, played, underruns, overflows, slips +N/-N, N frames of silence cut, ring N ms
    (min..max, target N, peak N), margin N ms, the reader takes frames all the time (MMAP exclusive) | server pulls
    N frames every N ms, AAudio N ms queued, N xruns; latency about N ms; end-to-end N ms (max N; the guest's period
    late by N ms + the host N ms; iOS is told N ms)` (`margin`: the level above what the reader takes before the
    guest's next period, or above the next pull, averaged; it should sit near the target. `end-to-end`: from iOS's
    sample time to the speaker, per guest period since the last report); the same for the microphone.
  - Fixed 2026-11-12: the helper's state reports were blocking sends that xnuhost reads only while the microphone
    runs, so after a few hundred speaker starts the socket would fill and the helper stop for good. They are
    `MSG_DONTWAIT` now.
- Why AAudio and not tinyalsa: on this SoC a PCM device plays only after the AudioReach graph is set up through the
  AGM plug-ins and PAL, which the audio HAL does; opening the card beside it would also fight audioserver.
- Tests: `runtime/bridges/audio/test_audio_helper.rs` (`cargo test -p onebridges audio::`, about 50 s of real time; the C's `tests/test_audio_helper.c`, every section ported; the check counts below are the C's): the helper against a
  fake AAudio whose server thread stands in for AudioFlinger's normal mixer (it starts reading once the track holds
  its buffer size, then takes a whole period of `pull` frames each period and nothing in a period where the track
  holds less; burst and pull set apart): no underrun or overflow at 0 and ±2000 ppm, the drift steered the right way
  over the last 6 s of 9 and the level ending within 10 ms of its aim, latency under 120 ms, the microphone's
  periods in order, the linger, a 200 ms start stall followed by silence (the backlog cut as silence, a marker 1 s
  later on time), a guest 40 ms late every 0.5 s (underruns raise the target; it is back at 20 ms 5 s after), the
  latency per stage at six phases of the pulls (below), MMAP not refused (NONE) and MMAP in use anyway (opened again
  NONE), LOW_LATENCY only with `ONEIOS_AUDIO_PERF=low`, a clean stop when the socket closes: 11,101 checks (most are the guest's sends; timing checks are skipped, each with a line, when a loaded host runs the test late: since 2026-11-25 "late" also counts a canary thread on a 2 ms absolute schedule, because a host that ran only the helper's own writer late failed `audio_helper` with 1 underrun at a load average of 24 while the fake server and the sends were on time).
  Since 2026-11-15 the fake HAL can also give MMAP exclusive or shared, and the test checks the path choice, the MMAP
  level, the end-to-end figure, the probe and the target's recovery on MMAP (20,213 checks; "Latency end to end"
  below). `devices/test_devices.rs` (the C's `test_bridges.c`) checks
  that virtio-snd takes each output period inside the guest's notify (0 in flight), and that the latency probe of a
  back end other than the helper is `-ENOTSUP`.

### Measured on the phone (2026-11-12, `bridgectl audio`, One UI running, no session)

| What | Result |
|---|---|
| uid 0 opens an AAudio output and input beside One UI | yes, no SELinux denial (`dmesg`: 0 `avc: denied`), domain `ksu` |
| Track | a normal mixer track (AudioFlinger: flags 0x000, frame count 3200, `Latency 133 ms`), 48 kHz stereo, `mmap 0`; written down as deep-buffer, but [F `/vendor/etc/audio_policy_configuration.xml`] the Speaker's route has only `primary-out`, `fast` and `mmap_no_irq_out`: [I] the track is on `primary-out` |
| 12 s of a 440 Hz tone at -30 dBFS | 0 underruns, 0 xruns, 0 overflows, 70 slips; a first run had 3 overflows at the start (the track's stall) |
| Latency (written to presented, AAudio's timestamp) | 128-137 ms: AAudio 128-136 ms (our 67 ms buffer + the deep-buffer path), ring 1-5 ms |
| Second tone 0.5 s after a stop | the lingering stream: no new open, no stall, 0 underruns |
| Microphone, 2 s | 92,400 frames (96,000 expected; the stream's start), room noise RMS -52 dBFS, peak -41.5 dBFS |

### In a session (2026-11-12, agent prelink's two sessions: `kernel.kc` `66faa3e8` from main 924a0d9, runtime 0.1.8-dev; read from their `xnuhost.log`)

- Kernel: `ONEIOS AUDIO: on: virtio-snd up: output stream 0, input stream 1 x1 (found 1, stage 3, 2 streams);
  IOAudio2 devices: Speaker and Mic published`, `DroidVMAudio: published Speaker=1 Mic=1 ... output latency 6480
  frames`, 1.2 s after xnuhost placed the kernel.
- audiomxd opens both devices and takes hog mode: `2 clients (hog 106)`, `4 selectors (0 refused, last 6)`, `9 maps`
  (the status pages and both rings), without `owner-hide-vmm`. In the first session nothing started IO.
- The second session played iOS's first sound through the whole path: the helper opened its stream (`none, sharing
  shared, mmap 0, burst 1600, buffer 3200 of 4800`), 12,720 frames (265 ms) came in 0.2 s, and the guest stopped.
  The guest's report after it: `6 selectors (0 refused, last 1)` (StartIO and StopIO), `engine 53 periods, 9 late,
  12720 frames played`, `virtio-snd 53 tx periods, 0 dropped, 0 errors`: audiomxd ran IO for 265 ms, and 9 of 53
  engine periods were late by more than a period (the vCPU ran late; the engine then sends back to back), which is
  why the frames came in a burst.
  The helper of that runtime cut 5,760 frames (2 overflows: the new track's stall against an 80 ms limit) and
  cleared the ring at the STOP: fixed after it (above), not yet run on the phone. `0 underruns`, `0 xruns`; the
  stream closed after the 15 s linger. Which sound it was and whether it was heard: not known (not my session).
- With system_server gone the helper opened and played (the stream above): AAudio from uid 0 works in a session.

### "Very slow" playback: the cause (media, 2026-11-12: iOS's logs of the 2026-11-11/12 sessions and the 14:12 session)

- **[F] iOS's audio I/O thread could not start for large I/O buffers.** In every boot audiomxd logs `Throwing Exception:
  not (kernel_error == 0) AMCP::Utility::Thread_Utilities::configure_thread_for_realtime - thread_policy_set failed.
  Cycle Size: 1536000 Thread Parameters: 1536000 1200000 1536000 1` (2229115 for the Null device), then `An Exception
  was thrown during Init`, `IO Stopped Context 80 after 0 frames`, `AQMEIO.cpp:201 timed out after 3.000s` and
  `System sound finished playing ... total time 2441 ms` (2.4-3.3 s in each boot). The guest's counter runs at
  19.2 MHz (`host: ... counter 19200000 Hz`): the cycle, 1536000 ticks, is 80 ms (`vaemCalculateAndSetDeviceBufferDuration:
  duration = 0.080, ... 3840`), and the computation, 1200000 ticks, is 62.5 ms here but 50 ms at Apple's 24 MHz. XNU
  refuses a computation above `max_rt_quantum` (50 ms = 960000 ticks at 19.2 MHz; osfmk/kern/thread_policy.c). So
  every sound played with a large buffer (system sounds: buffer 3072 frames; long-form media) got no I/O at all and
  waited out the 3 s timeout; a 1024-frame buffer (the Settings ringtone preview of the 14:12 session: `IO Stopped
  Context 96 after 52224 frames`) asks less and played. macOS-DroidVM found the same on its 19.2 MHz board (WebKit's
  players stopped at their first second). [I] This is the "very slow": sounds seconds late or missing, players that
  stall.
- **Fix: hosted 0052** (`kernel/patches/oneios-hosted/0052-rt-computation-held.patch`): a computation above the
  maximum is held to it when the constraint allows. The guest's scheduler only: no host thread changes its policy.
  Needs a new `kernel.kc`. Expected in iOS's log after it: no `configure_thread_for_realtime` exception, `IO Stopped
  Context N after N frames` with N > 0 for system sounds, no `timed out after 3.000s`; in xnuhost's log a
  `speaker open` at the lock click or a keyboard click, `frames in` close to the sound's length.
- **The host path's latency** (the rest of the delay, after the fix): 131-142 ms in the 14:12 session (`AAudio 131 ms
  queued`), as measured beside One UI. It is AAudio's buffer level (one 1600-frame burst plus a 20 ms target, about
  53 ms) plus the normal mixer and the HAL (about 78 ms). **Superseded on 2026-11-13** by "Latency per stage" below
  (the helper's level measured in a harness, the burst shown to be the mixer's period, the level, the target, the
  buffer changed; the reported latency not yet). What was looked at on 2026-11-12:
  - a non-deep-buffer route: the track is on `primary-out` already (above); the `fast` port needs a FAST track,
    which AudioFlinger grants only to a callback or a Java writer and then raises that client's thread to SCHED_FIFO
    through system_server: not allowed (no realtime thread) and not there in a session. MMAP is out the same way.
  - a smaller buffer: `ONEIOS_AUDIO_BURSTS=1` and `ONEIOS_AUDIO_TARGET_MS=10` are the knobs (xnuhost's environment, which the helper inherits; no `xnuhost.conf` key); not
    made the default, because the server's mixer period on this phone is not measured (the burst of 1600 is the
    track's notification period, not the mixer's) and an underrun is worse than 30 ms.
  - the stream kept open in silence: the linger is 60 s now (above). A sound after a longer pause pays the open
    and the new track's first 200 ms once.
  - the latency iOS is told (`oneios-audio-latency`, 6480 frames = 135 ms): it matched the 131-142 ms measured (the new helper: about
    105 ms predicted, below; the default is unchanged until the phone measures it).
  - iOS's own buffer: system sounds use a 3072-frame I/O buffer (64 ms) of iOS's choosing; not changed.

### Latency per stage (fix-audio-latency, 2026-11-13; host-tested, not run on the phone; the mixer-track path, superseded as the default by "Latency end to end" below)

The fault (the phone's 23:45 boot of 2026-11-12): `DroidVMAudio: ... output latency 6480 frames`, `oneios-audio:
ready: performance mode none, MMAP off, AAudio buffer 2 bursts, ring 500 ms, target 20 ms (at most 80), linger 60
s`, playback about 130-140 ms late.

**What the owner hears.** iOS's HAL adds a device's `output latency` (and its safety offset) to the time stamps it
gives its clients; AVFoundation's players use them to show a video frame when its sound comes out. So the number
delays pictures (A/V sync), not the sound: DroidVMAudio's engine plays its ring on its own clock whatever the
property says. A tap or key click that comes late is the real path below; a reported latency larger than the path
shows only as video waiting for its sound (sound ahead of the lips), a smaller one as sound behind the lips.

| Stage (the order a sample takes) | Before | After | How it is known |
|---|---|---|---|
| iOS's HAL: the I/O buffer it writes ahead (1024-3840 frames, 21-80 ms, iOS's choice per client) plus the safety offset (480 frames, 10 ms) | the same | the same | [F] iOS's logs (`vaemCalculateAndSetDeviceBufferDuration: duration = 0.080`), DroidVMAudio.cpp; iOS's own, as on a device |
| The guest's engine (hands a period on once it has passed) | 0-5 ms | the same | DroidVMAudio.cpp `run()` |
| virtio-snd, guest to xnuhost | under 0.01 ms, 0 periods in flight | the same | `devices/test_devices.rs` (C: `test_bridges.c`): each period is with the back end before the guest's notify returns (0.7 us on average, 6 us at most); the guest's 16 slots (`SND_SLOTS`) never fill, so fewer slots would cut nothing |
| Socket and the helper's ring | 10.5 ms (harness; the writer waited for 244 frames and polled every 2 ms; the phone's reports said 1-5 ms) | 0.1-0.7 ms | harness |
| AAudio's queue, to the server's read | 62.9 ms (harness: the writer filled AAudio to its 2-burst buffer, 3200 frames) | 32.7 ms (the target plus half a pull) | harness |
| Android after the read: the normal mixer, the HAL, the DSP | 65-73 ms | the same | [I] the phone's AAudio timestamp latency of 2026-11-12 (128-136 ms written to presented) less the old helper's 63 ms in the harness |
| **Host path** (the helper's `latency about N ms`) | 131-142 ms (phone) | **about 98-106 ms** [I, predicted] | the two rows above |
| A new stream's first sound (the open and the new track's ~200 ms before the server reads) | late by the stall, the backlog then played 1.25 % faster for about 16 s | the backlog's silence is cut at the sound's first pause: a marker 1 s after a 200 ms stall went from 268 ms to 22 ms (harness) | harness |

The harness: `test_audio_helper.c`'s latency cases (now `audio/test_audio_helper.rs`) (`stage:` lines), the helper of `origin/main` 3d0644f and the new
one built against the same fake (pulls of 1600 frames, burst 1600, the guest starting at six phases of the pulls, 60
markers each), averages:

| Fake server | Helper | Socket + ring | AAudio queue | Guest send to server read (max) |
|---|---|---|---|---|
| pulls 1600 (33 ms), burst 1600: the phone's | before | 10.5 ms | 62.9 ms | 73.4 ms (84.3) |
| | after | 0.1-0.7 ms | 32.7-34.1 ms | 33.4-34.2 ms (44.5-45.0) |
| pulls 800 (17 ms), burst 1600 | before | 6.4 ms | 55.9 ms | 62.4 ms (72.0) |
| | after | 0.1 ms | 25.6-27.0 ms | 25.7-27.2 ms (34.4-35.3) |
| a 200 ms start stall, then 1 s of silence, then markers | before | 201.6 ms | 66.4 ms | 268.0 ms |
| | after | 0.1-0.2 ms | 21.7-22.0 ms | 21.8-22.1 ms |

(Before, in the harness, also failed 13 of the new test's checks: the drift and level checks and the stall case.)

**The burst is the mixer's period** [I, from AOSP `main`'s sizing, not read in One UI's build]: the phone's track has
burst 1600 of capacity 4800 (`bridgectl audio`, 2026-11-12). A normal track's notification period (AAudio's burst)
is its frame count over `nBuffering`, 3 when the frame count is three normal-mixer periods, else 2
(`PlaybackThread::createTrack_l`), and the frame count is `calculateMinFrameCount` = a whole number of mixer
periods. 4800 / 1600 = 3 holds only for a mixer period of 1600 frames (33.3 ms), which also means `primary-out` has
no fast mixer in front (a fast mixer's normal sink is 20-24 ms) and reports a latency of 100-133 ms. The helper now
measures it (`the server pulls N frames every N ms`).

**What changed, and what each measured** (all in the harness above; the phone has not run any of it):
1. The level follows the server's pulls (the old one was phase-blind: a burst plus the target at every moment, then
   AAudio filled to 2 bursts): AAudio's queue 62.9 -> 32.7 ms; with pulls of 800, 55.9 -> 25.6 ms (the burst no
   longer counts, the pull does).
2. The writer is woken by each period and plays a chunk as soon as a period is in the ring (slips change the output
   length instead of waiting for 241 frames): socket + ring 10.5 -> 0.1-0.7 ms.
3. AAudio's buffer is a burst plus the target plus a chunk (2800 frames on the phone) instead of 2 bursts (3200): it
   is AudioFlinger's start threshold, so a new stream starts 400 frames (8 ms) lower, and a backlog waits in the
   ring where its silence can be cut.
4. Silence in a backlog is cut: after a 200 ms stall the next sound is on time once a pause has come (268 -> 22 ms);
   continuous sound still loses a backlog only by playing 1.25 % faster (as before).
5. The target comes back down: 5 ms per 20 s without an underrun (a guest 40 ms late every 0.5 s raised it to
   25 ms; 5 s on time brought it back to 20). Before, it only went up, for a stream that lingers 60 s after each
   sound. The floor stays 20 ms: the guest's engine was late by more than a period in 9 of 53 periods on the phone
   (2026-11-12), and an underrun is a gap.
6. LOW_LATENCY, opt-in (`ONEIOS_AUDIO_PERF=low`; above, "Performance mode"): no FAST track and no realtime thread for a blocking
   writer (AOSP source); the RAW flag it carries may move the track to the `fast` output's normal mixer [U]. Nothing
   to measure on the host: the fake has one output.
7. The reported latency: NOT changed (6480 = 135 ms, which also leaves the engine's 5 ms out). The new helper predicts 5040
   frames (105 ms = 5 ms + about 100 ms); a kernel build with that default was stopped at the hand-off of 2026-11-13: set `oneios-audio-latency=5040` (or the measured value) in the boot arguments instead.

**Not done, and why.** A FAST mixer track or MMAP needs a realtime client thread (AAudio's callback path asks
system_server for SCHED_FIFO), which the way back forbids. [I, AOSP source] AudioFlinger also gives a FAST track
without any priority request to a `TRANSFER_OBTAIN` client and to a synchronous Java writer (`clientTid` -1): the
next lever for the 65-73 ms after the read, but only through libaudioclient's private C++ API (no NDK, no stable
ABI across One UI builds) or a Java writer (app_process) beside xnuhost; not tried. iOS's own I/O buffer and safety
offset: unchanged (a smaller safety offset would glitch when the guest's audio thread is late).

**On the phone (what waited for it, for the helper of 2026-11-13; superseded by "Latency end to end" below, whose defaults are LOW_LATENCY, MMAP exclusive and the measured output latency).** A session with that runtime (any `kernel.kc`; the kernel was unchanged):
- xnuhost's log: `oneios-audio: ready: performance mode none ...`; `speaker open: ... none asked, none given, ...
  mmap 0, burst B, buffer B+1200 of C frames`; `oneios-audio: speaker: the server pulls N frames every M ms`
  (expected 1600 every 33.3: then the inference above holds); then a second session (or `bridgectl`, below) with
  `ONEIOS_AUDIO_PERF=low` in xnuhost's environment: `low latency asked, none given` expected (a `mmap 1` or a `given
  low latency` would be new: then leave it off), and a smaller N means RAW moved the track to the `fast` output;
  `speaker running|stopped ...: ... margin N ms` near the target (20); `latency about N ms` (AAudio's timestamp:
  written to presented, plus the ring): expected about 95-105, was 130-137; `0 underruns`, and if there are some,
  `target N ms (20 s without an underrun)` lines bringing it back. No `thread N was SCHED_FIFO` line.
- The reported latency: the kernel still says `output latency 6480 frames`. Set `oneios-audio-latency=` to (N + 5) x
  48 frames, N the helper's `latency about N ms`, in the `boot-args=` line of `xnuhost.conf` (5040 if N is 100), and
  then make it the default (`tools/oneios-tool/crates/oneios-kernel/src/staged/mod.rs` `output_latency!`, the goldens
  `tests/kernelgen/golden/synthetic/audio.out`, `stdout.txt`, `staged.sha256`, and a kernel build + prelink).
- Beside One UI, without a session (the same as 2026-11-12, so the two compare): `bridgectl audio
  /data/local/tmp/oneios-audio 12`, then once more with `ONEIOS_AUDIO_PERF=low`; the `latency about` of each says
  what RAW does. `dumpsys media.audio_flinger` while it plays shows which output thread holds the track (its pid) and
  the thread's frame count (the mixer's period).
- The owner's ear, end to end (touch to sound): Settings > Sounds & Haptics > Keyboard Feedback > Sound on; record
  the phone with another device's microphone while tapping keys with a fingernail (the nail's click on the glass is
  the first click in the recording, the keyboard's sound the second). The gap, over ten taps, before and after; it
  includes iOS's own I/O buffer, so it is longer than the host path. A/V: a lip-sync test video (a flash with a
  beep) in Safari, filmed with sound by another device: the beep before the flash means the reported latency is
  too large, after it too small.

### Latency end to end (fix-audio-latency, 2026-11-15; host-tested and built, not run on the phone)

The fault (the owner, high priority): "audio doesn't play in time, the delay of audio playback is very high". The
8644ae9 session's `xnuhost.log`: `oneios-audio: ready: performance mode none ..., MMAP off, ...`, `speaker open:
48000 Hz, 2 channel(s), none asked, none given, sharing shared, mmap 0, burst 1600, buffer 2800 of 4800 frames,
device 3`, `speaker running: ... ring 0 ms (0..0, target 20, peak 30), margin 20 ms, server pulls 960 frames every
19.9 ms, AAudio 106 ms queued, 2 xruns; latency about 116 ms` (105-123 ms over the session, the host side alone);
the kernel's `DroidVMAudio: published ... output latency 6480 frames` (135 ms told to iOS).

Each part, from iOS's mix to the speaker, its cause and the fix:

| Part | Cause | Fix | Before | After |
|---|---|---|---|---|
| iOS's I/O buffer: the HAL writes one to two I/O buffers plus the safety offset ahead of the device's clock (a frame waits 1.5 buffers + the safety offset on average) | [F] CoreAudio's `HALS_IOContextDescription::CalculateMaximumIOBufferFrameSize` (24A446's dyld cache, read with `ipsw dyld disass` on the Linux PC): the smallest `io buffer frame size` of the context's devices x 0.375, at most 4096. DroidVMAudio's ring was 8192 frames: at most 3072, which system sounds take (iOS asks 0.080 s, 3840: `vaemCalculateAndSetDeviceBufferDuration`, "system sounds: buffer 3072 frames" above). 0.375 is 3/8: two buffers ahead (3/4 of the ring) and a quarter for the device's read | the speaker's ring 1920 frames (`oneios-audio-ring`; a whole number of periods): at most 720 frames (15 ms) | 1.5 x 3072 + 480 = 5088 frames, 106 ms for a system sound [I, from the cap] | 1.5 x 720 + 240 = 1320 frames, 27.5 ms at the cap [I]; the guest's `speaker engine:` line measures it |
| The guest's engine | handed a period on once it had passed (+240 frames) behind a safety offset of 480 | hands the period on as it starts; the safety offset is that period (240): the HAL has written it by then | 5 ms after the safety offset | 0 (the safety offset is in the row above) |
| The ring's wrap (every device) | 8192 is not a whole number of 240-frame periods: once a ring (171 ms) the engine read and zeroed 208 frames past the ring's buffer (the microphone wrote them), and the ring's first 208 frames were never played; the zero time stamp moved only every 245760 frames (5.1 s), when a period happened to end on a wrap | a bounce buffer for a period over the end; the zero time stamp at each wrap | a 4.3 ms piece of the next allocation every 171 ms [I, from the code] | none |
| virtio-snd, the socket, the helper's ring | (unchanged: 0 periods in flight) | | under 1 ms | under 1 ms |
| The host: AAudio and Android after it | a normal mixer track (performance mode NONE; LOW_LATENCY cannot get a FAST track for a blocking writer) on `primary-out`: AAudio's queue (the target above a pull of 960-1600 frames) and the mixer, the HAL and the DSP after the read | LOW_LATENCY, MMAP exclusive (the DSP reads our buffer; a blocking SCHED_OTHER writer with uclamp.min 512); the level 5 ms (at least a burst) above what the DSP takes before the guest's next period (on MMAP shared a burst more: the endpoint takes one at a time); a new stream starts at 10 ms; AAudio's xruns and an underrun raise it 5 ms; it comes down 5 ms after 20 s without one only when the latest guest period of those 20 s was 3 ms inside the lower target (the floor of 5 ms needs periods within 2 ms) | 105-123 ms (8644ae9, phone) | [I] about 10-20 ms: the level 5-10 ms + what the HAL and DSP hold below the read (on the phone: the probe's `N ms below AAudio's read`) |
| What iOS is told (`output latency`) | a fixed 6480 frames | the helper's probe before the boot (`oneios-audio-latency=` from xnuhost) | 135 ms told, about 121 ms true | measured each boot |

So for a system sound (iOS at the cap): about 111 + 116 = 230 ms from iOS's mix to the speaker before [I], about
27 + 15 = 42 ms after [I]; from iOS's sample time to the speaker (what `output latency` means, and what A/V sync
uses) 121 ms before, 10-20 ms after [I]. A smaller ring lowers the first part further: `oneios-audio-ring=1440`
(at most 540 frames) leaves the HAL only 120 frames of slack for a late engine at the cap; 1920 leaves 240 (5 ms).

**One raise per starvation, and the probe's prefill** (2026-11-28, from YMGPwcca/OneiOS `c4d9e318`, `7cf70ce7`,
`4dd7242c`; host-tested, not run on a phone). AAudio's MMAP xrun counter goes up on each write attempt while its read is
ahead of the write: 18 increments in 0.3 ms on their SM-S948B, one starvation. The writer raised the target 5 ms for
each, so one episode put the level at its 80 ms ceiling and kept it there as latency. Now the writer raises the
target at most once per guest period (`CHUNK`, 5 ms) for xruns and underruns together (`Ring::raise_target`); the
counts in the `speaker running` line (`N underruns`, `N xruns`) still count every report. On every phone (a fix of
how the level reacts, not a new default); the S24 Ultra's line shows it as a lower `target_peak` after an xrun burst.
Test: `aaudio_helper::raise_tests`. `ONEIOS_AUDIO_PROBE_PREFILL=1` (off by default; the `galaxy-s26-ultra` row of
the host table sets it, 7h) fills the AU_PROBE speaker stream's buffer with silence by non-blocking writes before
`requestStart` (`oneios-audio: speaker prefill: N/N frames before requestStart`): on their phone a probe stream
started underfilled took 480 of 576 frames and never moved FramesRead or gave a timestamp. The frames already
written count in the stream's own frame counter, so the latency figure is not short by the silence. A guest START's
stream is never prefilled. Test: `aaudio_helper::prefill_tests`.

**Why not a FAST track.** [F, AOSP `main`'s source, not One UI's build] AAudio's legacy path asks
`AUDIO_OUTPUT_FLAG_FAST` for LOW_LATENCY but with `TRANSFER_SYNC` for a blocking writer, and `AudioTrack::
createTrack_l` takes FAST off ("denied by client": a synchronous writer gets FAST only with `threadCanCallJava`,
which AAudio passes false). A FAST track needs a data callback, whose thread AudioFlinger raises to SCHED_FIFO
(the way back forbids it, and the request through `scheduling_policy` would wait for system_server forever in a
session). MMAP exclusive needs neither: the DSP itself reads the buffer, `AudioStreamInternal` starts a thread only
for a callback, and neither `AAudioServiceStreamMMAP` nor AudioFlinger's `MmapThread` asks for a priority. MMAP
shared does (its endpoint thread in audioserver registers through `android::requestPriority`, which loops on
`checkService("scheduling_policy")` with `sleep(1)` forever without system_server): kept only while system_server
runs. What this phone's HAL gives: [U] until the phone runs it. The phone has `aaudio.mmap_policy` 2,
`aaudio.mmap_exclusive_policy` 2 and a `mmap_no_irq_out` port on the speaker's route (above), so MMAP exclusive is
expected; the helper's `speaker open` line says it (`sharing exclusive, mmap 1, burst B`).

**End to end, measured (lasting lines).**
- Host (`oneios-audio: speaker running|stopped ...; end-to-end N ms (max N; the guest's period late by N ms + the
  host N ms; iOS is told N ms)`): per guest period, its lateness against the guest's schedule (the earliest arrival
  of the last one to two seconds less its sample time, so that a guest that paused or lost a period is not late for good: the guest's engine hands period n on at its start, on the same counter as the
  host's `CLOCK_MONOTONIC_RAW`) plus what the ring and AAudio hold ahead of it when it comes in (frames written but
  not yet presented, from `AAudioStream_getTimestamp`). That is iOS's sample time to the speaker. A kernel from
  before this change hands a period on 5 ms later than the model says: its figure is 5 ms short.
- Guest (`ONEIOS AUDIO: speaker engine: N periods, taken late by N us on average (max N); the HAL wrote N..N frames
  ahead (N on average, N ms: iOS's I/O buffer and safety offset), N periods tight (ring N frames)`, every 30 s while
  it plays): the HAL's write end (`EngineStatus` +0x20) against the period being taken is iOS's mix to the handoff;
  `tight` counts periods where the HAL had written within a period of overwriting what was not taken yet (then raise
  `oneios-audio-ring`). If the HAL does not fill the write end, the line says `the HAL's write end not ahead (last N)`.
- iOS's mix to the speaker = the guest's average lead + the host's end-to-end.
- The phone check: `bridgectl audio-loop` (runtime/bridges/README.md, "On the phone"): clicks into the helper, heard
  by the microphone: the round trip bounds the speaker's part from above.

**Tested on the PC** (the C's `runtime/bridges/tests/test_audio_helper.c`, 20219 checks, 0 failed; the box had a load of 12-40
on 4 CPUs from other agents' qemu boots and builds, so every timing check was skipped with its line and the figures
below are under that load): a fake HAL with MMAP exclusive (burst 192 taken at once every 4 ms, a timestamp 240
frames behind the read): one open, `sharing exclusive, mmap 1`, floor 5 ms, a new stream at 10 ms; the probe
answered 720 frames (5 ms below the read + the 10 ms level), and the stream it opened was the one the next START used.
**The CI runner (a lightly loaded 4-CPU VM, PR #51's first run) found one underrun in 9 s on MMAP exclusive at a 5
ms target, with 0 xruns**: the helper's own "about to run dry" with the ring empty, and so a gap of silence, which
the phone would hear too. Two causes, both fixed: priming ended when the ring's chunk and AAudio's silence together
reached the need, so the guest's first chunk went in on whatever the DSP had left of the silence (up to a burst under
the target), and a slip up came only 5 ms under a 5 ms target (never); and 5 ms leaves a guest period only 3 ms to
be late, which the runner's (and a phone's busy vCPUs') periods were at times. Now priming fills AAudio itself to
the need, a level 1 ms under the target slips up, a new MMAP stream starts at 10 ms and comes down only when the
guest's periods were on time for it. The figures below are from before that fix; over six runs the six
latency sounds' end-to-end was 16-42 ms (16.3 at the lowest load, 12-15; the guest's lateness 0.8-2.4 ms), AAudio's
queue 13-34 ms, the target raised by the fake DSP's xruns when the loaded host ran the writer late (each +5 ms),
against the mixer track's queue 22-41 ms and end-to-end 17-77 ms in the same runs (that fake presents a pull's frames
from the pull on, so its end-to-end is under its queue); MMAP shared without system_server closed before
its start and a mixer track opened, with it kept; AAudio_setMMapPolicy refused: a mixer track kept LOW_LATENCY;
`ONEIOS_AUDIO_PERF=none` and `ONEIOS_AUDIO_MMAP=off`; a guest 40 ms late every 0.5 s on MMAP: 3 underruns, the
target up to 10-25 ms and back down as far as the guest's lateness allows. `origin/main`'s helper and test under the same load failed 5
checks (overflows, the stall's silence cut, the markers): this test now skips those, as the other timing checks,
when the host ran it late. `test_bridges.c` (now `devices/test_devices.rs`): the probe of a non-helper back end is `-ENOTSUP`. The kernel's edit
list against the pinned `DroidVMAudio.cpp` and the goldens: `cargo test -p oneios-kernel`; the hosted kernel build
(`HOSTED_OUT=audiolat`, 1229 s; `compare-golden.sh`: the audio files the same) and its prelink (`kernel.kc`
`ee72d94886023f31`, audit 0 failures). A PC boot of it was not run: the box's qemu queue had five boots of up to 100
minutes ahead, and a PC boot plays no iOS sound (the null back end), so it would only show the attach lines.
**The PC cannot show:** the phone's HAL and DSP (whether MMAP exclusive is given, its burst,
what it holds below the read), the real end to end, iOS's I/O buffer and the HAL's write end (no iOS sound plays on
a PC boot with the null back end), the round trip through air.

**On the phone (what waits for it).** A session with this runtime and kernel:
- xnuhost's log: `audio: speaker latency N frames (N ms) measured by the helper: oneios-audio-latency=N`, the
  helper's `probe:` line, `speaker open: ... low latency asked, low latency given, sharing exclusive, mmap 1, burst
  B ...; MMAP exclusive: the DSP reads the buffer` (or the reason it fell back), no `thread N was SCHED_FIFO` line;
  `speaker running: ... 0 underruns ..., N xruns; latency about N ms; end-to-end N ms (...)`: expected 10-20 ms, was
  116-121.
- The kernel's log: `DroidVMAudio: speaker: ring 1920 frames (iOS's I/O buffer at most 720), safety offset 240, each
  period handed on as it starts; output latency N frames` (N the probe's), and while sound plays `ONEIOS AUDIO:
  speaker engine: ... the HAL wrote N..N frames ahead ...`: expected 960-1680 (720 x 1..2 + 240), 0 tight.
- iOS's own log (`oneios-tool ioslog show ... --proc audiomxd`): `vaemCalculateAndSetDeviceBufferDuration` asking
  what it asks, the HAL taking at most 720 frames; no `Cannot set audio buffer frame size`.
- Beside One UI: `bridgectl audio /data/local/tmp/oneios-audio 12` (the probe, the path) and `bridgectl audio-loop
  /data/local/tmp/oneios-audio 10` (the round trip); then the keyboard-click recording above, before and after.

### Volume: the Speaker's hardware volume control, the host at full scale (fix-audio-volume-haptics, 2026-11-22; host-tested and built, not on the phone)

**The fault (owner, 2026-11-22):** the volume keys do nothing, iOS's level cannot be changed (keys or slider), sound
plays at a low volume. The keys reach iOS (`input: volume up down: to the guest`, 10 up and 4 down presses;
`oneios-hid: buttons: Consumer Control, Built-In published`, backboardd holds the event service), sound plays
(`oneios-audio: speaker open: ... MMAP exclusive`, `speaker stopped by the guest`).

**The cause, iOS's side [F iOS's log of the s6, s7 and s8 boots; V 24A446 VirtualAudio.plugin and CoreAudio]:** every
volume set of audiomxd throws (10-17 times a boot, at each `vaemSetDeviceVolume: Setting cmsvolume = 0.500 for device
UID: VirtualAudioDevice_Default`):
```
audiomxd OutputVolumeControl_HAL_Common.cpp:1091  Reconfiguring volume curve with dB range {0, 0} and transfer function 0
audiomxd OutputVolumeControl_HAL_Common.cpp:2090  OutputVolumeControl_HAL_Common::ReconfigureInternal() - unable to lock software volume command.
audiomxd OutputVolumeControl_HAL_Common.cpp:1423  EXCEPTION (std::invalid_argument) [mSoftwareVolumeMode == VolumeControl::SoftwareVolumeMode::kHardwareOnly
         && !HasExternalVolumeHandler() && !mPhysicalDeviceSupportsHardwareVolume is true]: "Volume Mode is HardwareOnly but physical device does not support HW volume"
```
The speaker route's software volume mode is HardwareOnly (VirtualAudio's routing database), and UniXNU-droidvm's
Speaker publishes an empty `controls` array: no volume control, a dB range of {0, 0}. So MediaExperience's volume never
reaches anything, the volume HUD has nothing to move, and no volume key or slider changes the sound (the earlier
reading, "VirtualAudio's software volume [I from iOSPlus10]", was iOS 16's and is wrong for iOS 27).

**The cause, the host's side [F code]:** the helper's stream is usage MEDIA, so AudioFlinger plays it at One UI's
media volume of the speaker, the audio policy's index when system_server stopped (nothing changes it in a session):
the "low volume". iOS's own volume could not lower or raise it (above).

**The fix:**
- **The Speaker's volume control** (the staged `DroidVMAudio.cpp`, `tools/oneios-tool/crates/oneios-kernel/src/staged/
  audio.rs`, the block "fix-audio-volume"; `drivers/audio/README.md`): `controls` holds one control in the HAL's
  IOAudio2 form (read in 24A446's CoreAudio: `HALS_IOA2Control::SynchronizeWithDictionary` reads `control ID`,
  `base class`, `class`, `scope`, `element`, `read only`, `value`, `minimum value`, `maximum value` and a `range map`
  of `start int value` (SInt32), `start db value` (Fixed64), `integer steps` (UInt32), `db per step` (Fixed64)):
  class `vlme`, base class `levl`, scope `outp`, element 0, steps 0..128 over -64..0 dB in 0.5 dB steps; step 0 is
  silence. The HAL sets it with selector 2 (`HALS_IOA2UCDevice::SetControlValue`: two scalars in, control ID and
  value, one out, the value taken), which was refused before. The speaker engine scales each 5 ms period by the
  step's gain (a Q16 table, 480 multiplies, nothing at 0 dB) before the transport. It starts at 0 dB; iOS sets its
  own level at its start. Log: `DroidVMAudio: speaker volume N of 128 (-X.Y dB, gain N of 65536), change N: iOS's
  volume, applied here (the host plays at full scale)` (the first 16 changes, then every 64th).
- **The host at full scale** (`runtime/bridges/audio/aaudio_helper.rs`, `runtime/bridges/README.md`): at the speaker's
  first open the helper sets the audio policy's index of USAGE_MEDIA on the speaker to its maximum through
  libaudioclient's `AudioSystem` (the call AudioService makes; looked up by mangled name, the Android 16 form with
  `muted` first), and sets the index it found back when it ends (xnuhost closes its socket). One UI's saved volume is
  in system_server's settings and is not written. [I] If the return kills the namespace before the helper ends,
  system_server's AudioService sets every saved index again as it starts (AOSP's `checkAllAliasStreamVolumes`; not
  read in One UI's build). `ONEIOS_AUDIO_HOST_VOLUME=keep`, or system_server running (`bridgectl audio` beside One
  UI): not touched. Only the speaker device (`AUDIO_DEVICE_OUT_SPEAKER`): a headset keeps One UI's index.

**The volume keys:** they go backboardd -> SpringBoard -> MediaExperience's volume up/down -> the same VAD volume set
that threw; `Volume button client session: None` is normal (no app claimed the buttons). [I] With the control the
set succeeds and the HUD moves; the key path itself could not be read in these boots: `pull-log`'s iOS log holds only
the first 2-3 s of userland and the last seconds, not the presses minutes later.

**Tested** (the C tree, before the Rust port): `runtime/bridges/tests/host.sh` in the host-test container (2026-11-23): `test_audio_helper` 20310
checks, 0 failed (the fake `AudioSystem` of the test gets the index set to its maximum once a run, set back when the
helper ends, not touched with `ONEIOS_AUDIO_HOST_VOLUME=keep`, at the maximum already, or with system_server up),
bridges 377, wifi 162, brightness 126, battery 83, thermal 99, hal binder 46, hw_backend 120, 0 failed; `make -C
runtime/bridges all` (the NDK build of `oneios-audio`) builds; `cargo test -p oneios-kernel` (the edit list on its
synthetic source against `golden/synthetic/audio.out`) 14 of 14; the kernel build and `kernel.kc` `a50202ea...` of
section 5's 0086 (the staged `DroidVMAudio.cpp`, sha256 `9feea520...` in `tests/kernelgen/golden/staged.sha256`,
compiled without a warning). Not on the phone.

**The phone check:** in `xnuhost.log` `oneios-audio: host volume: One UI's media volume on the speaker was index N of
M; set to the maximum for the session` (or `... is at its maximum`, or the reason it could not), at the end `... put
back to index N`; `DroidVMAudio: speaker volume N of 128 (...)` at iOS's start and at each key press or slider move;
in iOS's log no `Volume Mode is HardwareOnly but physical device does not support HW volume`, `Reconfiguring volume
curve with dB range {-64, 0}`; on the screen the volume HUD moving with the keys; by ear the level following them,
up to the phone's full loudness. Back in One UI: the media volume as it was.

### The volume curve: UniXNU-sheng's -36 dB squared (fix-audio-curve, 2026-11-25; built, not booted)

**The fault (owner):** sound is quiet with iOS's volume at a normal level. **The cause [F our logs,
`out/session-logs/2026-11-24-mac-gpumem/live.log`]:** the host plays at full scale (`oneios-audio: host volume: ...
set to the maximum for the session`) and the guest attenuates by what iOS sets on the control above: `speaker volume 70
of 128 (-29.0 dB, gain 2325 of 65536)`, `79 of 128 (-24.5 dB)`, `100 of 128 (-14.0 dB)`. iOS spreads its volume evenly
over the control's steps: from 79 each volume-up press moved 7-8 steps (79, 86, 94, 101, 108, 115, 128: 3.5-4 dB a
press), and audiomxd says `Reconfiguring volume curve with dB range {min, max} and transfer function 0`. So with a
linear -64..0 dB range half of iOS's volume is about -32 dB.

**The fix [F UniXNU-sheng c8f92b2 `hal/ShengAudioPlugIn.c` 204-210, iPadOS 27.0.1 24A446, BSD-2-Clause-Patent]:** sheng's
audio plug-in had the same quietness on the same build ("a linear -40 dB range still -26 dB (quiet); now it is -16
dB") and maps the volume s (0..1) to -36 x (1 - s)^2 dB. Here, with `oneios-audio-curve=1` (the default; the staged
`DroidVMAudio.cpp`, `tools/oneios-tool/crates/oneios-kernel/src/staged/audio.rs` block "fix-audio-curve"):

- the control keeps its 128 even steps (iOS's volume s is step / 128, as the phone showed) and reports -36..0 dB
  (`start db value` -36, 128 steps of 0.28125 dB: the curve's ends);
- the speaker engine applies the curve to the step: the gain of step v is 10^(-36 x (1 - v/128)^2 / 20) (a Q16 table,
  step 0 silent). Step 43 (a third) is -15.9 dB, 64 (half) -9.0 dB, 70 -7.4 dB, 100 -1.7 dB; before, -42.5, -32.0,
  -29.0 and -14.0 dB.
- `oneios-audio-curve=0`: -64..0 dB in 0.5 dB steps, applied as reported (before 2026-11-25).

Why not a range map of several ranges shaped like the curve (the plan's first idea): the HAL's scalar and audiomxd's
curve are fractions of the dB range [I from the IOAudio2 range-map form and audiomxd's "dB range ... transfer function"
line; not read in 24A446's CoreAudio], so a multi-range map would only move each step's dB, and iOS would still ask for
an even spread in dB. The reported dB of a step (what an app reading the control's dB sees: linear over -36..0) is not
the applied one in between the ends; nothing in iOS's speaker route reads it back [I].

**The lasting lines:** each change (the first 16, then every 64th) `DroidVMAudio: speaker volume N of 128 (-X.Y dB, gain
N of 65536), change N: iOS's volume, applied here (the host plays at full scale; curve sheng: -36 dB x (1 - v/128)^2,
oneios-audio-curve=1)`, the dB now in tenths; and in `OneiOSAudio.c`'s 30 s report, when the volume changed since the
last report, where it settled: `ONEIOS AUDIO: speaker volume N of 128 (-X.Y dB applied, gain N of 65536) after N changes
(N since the last report); curve sheng -36 dB squared (oneios-audio-curve=1)` (a slider's drag is many changes, past
the 16 change lines).

**Tested:** `cargo test -p oneios-kernel` (`staged::audio::tests::the_volume_curve_is_shengs` recomputes both tables of
the staged source from the formula; the edit list on the synthetic source against `golden/synthetic/audio.out`), the
kernel built with it, `runtime/host-tests.sh` (no qemu part) passes (docs/switches.md "Tests": the hashes, the hand-off
`out/handoff/audiovol-1c3e86a/`). **Not booted.** The phone check: the change line at a third of iOS's
volume about `-16.x dB` and at half about `-9.0 dB`; iOS's log (`ioslog --proc audiomxd`) `Reconfiguring volume curve
with dB range {-36, 0}`; by ear the middle of the slider clearly louder than before and the bottom steps still quiet;
`oneios-audio-curve=0` gives the old `-29.0 dB` at step 70 again.

### Late periods: the engines realtime in the guest, their wait without timer slop (fix-audio-late, 2026-11-24; built and prelinked, not booted)

The fault: iOS's audio late. The phone, 2026-11-24 01:17-01:25 (kernel `e79886d9` = main,
`out/session-logs/2026-11-24-mac-gpumem/live.log`):

- the guest: `engine 1474 periods, 123 late`, later `5092 periods, 271 late`, with `virtio-snd ... 0 dropped`;
  `speaker engine: 723 periods, taken late by 1488 us on average (max 55188)`, `751 periods, ... 1364 us ... (max
  28879)`, `3618 periods, ... 1622 us ... (max 98149); the HAL wrote 216..5680 frames ahead ..., 412 periods tight`;
- the host helper: 1 then 5 underruns, its target up to 40 ms and never down again (it comes down only when the worst
  guest period of 20 s was 3 ms inside the lower target), `latency about 40-60 ms; end-to-end 36-49 ms (max 113-141;
  the guest's period late by 0.7-2.3 ms ...; iOS is told 12 ms)`;
- 0051's `kernel_task` line in the windows of the worst lateness: `VM_pageout_scan 14706665` us of CPU in 30 s,
  before it `VM_compressor 3746618`, `VM_compressor 3527132`, `VM_pageout_scan 2081473`, `thread call high #1 815855`;
  `oneios-cpu: ... (all 800%)`: every vCPU busy; xnuhost's `perf: cpuN ... waited for a host CPU 22-31%`.

What the code shows (the cause):

1. **`late` was not the speaker's.** `droidvm_audio_late` counted every engine's period taken over a period (5 ms)
   late, the Actuator's too, while `droidvm_audio_periods` counts the speaker's and the mic's only: `engine 0
   periods, 77 late` at 01:19 were all the Actuator's (1559 periods then), and between 01:21 and 01:23, with the
   speaker stopped, `late` rose 74 on 3310 Actuator periods (2.2 %). So the 8 % was a mix; the speaker's own share was
   not printed.
2. **The engine waited behind the kernel's busy threads.** An engine is a kernel thread (`kernel_thread_start`:
   BASEPRI_KERNEL, 81, fixed). The VM's pageout and compressor threads run at BASEPRI_VM (91), the high thread calls
   and `daemon.deferred-dealloc` above that; on a guest at 800 % with `VM_pageout_scan` at half a vCPU the engine's
   period waited for them (up to 98 ms). [I] Kernel threads also prefer the boot cluster (the Edge clutch's default
   preferred pset, `sched_clutch.c`, with no CLPC to move it): cpu0's E cluster, whose two vCPUs sit on the host's
   busiest cores (`cpu0`/`cpu1` waited 30-31 % for a host CPU, the P vCPUs 22-26 %); which vCPU ran the engine is not
   logged.
3. **Its wait had timer slop.** `clock_delay_until` waits with `TIMEOUT_URGENCY_SYS_NORMAL`; XNU gives a thread at 80
   or above the kernel tier's slop (`timer_call_slop`, `arm_timer.c`: an eighth of the wait, at most 1 ms, up to
   twice that with the user idle level). The engine waits about 5 ms a period: 0.6-1.2 ms late by design, about the
   measured 1.4-1.6 ms average.

The fix (staged `DroidVMAudio.cpp`, `tools/oneios-tool/crates/oneios-kernel/src/staged/audio.rs`, block
"fix-audio-late"; boot-arg `oneios-audio-rt`, default 1, `docs/switches.md`):

- each engine thread (speaker, mic, Actuator) asks at its start `THREAD_TIME_CONSTRAINT_POLICY`: period and
  constraint one period (5 ms), computation 2.5 ms (XNU raises a computation under half the constraint to that). It
  then runs before every non-realtime thread of the guest, its preemption IPIs are immediate, and the realtime spill
  rules (0089) apply. The guest's scheduler only: no host thread is realtime (xnuhost's vCPUs stay SCHED_OTHER; the
  way back's rule holds). XNU's failsafe demotes a realtime thread that runs 1 s without blocking; an engine blocks
  every period;
- its wait is `assert_wait_deadline_with_leeway(..., TIMEOUT_URGENCY_SYS_CRITICAL, deadline, 0)`: no slop;
- `oneios-audio-rt=0`: kernel priority and `clock_delay_until`, exactly as before.

Not done, on purpose: no host thread realtime or at a negative nice (docs/booter.md 2.9); no uclamp for a vCPU that
runs the engine (the engine's work is a copy and a virtio kick, about 0.1 ms; uclamp sets the core's clock, not the
wait for it; a vCPU is pinned to its core). The host's wait for a core (`waited for a host CPU`) is the vCPUs' and is
not changed here.

The lasting lines (logging, no switch):

- `DroidVMAudio: Speaker engine: realtime in the guest (period 5 ms, computation 2.5 ms, constraint 5 ms), its wait
  without timer slop (oneios-audio-rt=1)` once per device at its first start (or `thread_policy_set refused the time
  constraint (0x..): kernel priority, ...` at every refusal);
- the counter line's `late` is the speaker's and the mic's (their `periods`); the Actuator's apart;
- `ONEIOS AUDIO: late periods: speaker N over 1 ms, N over 5 ms, N over 20 ms of N, N written by the HAL only in
  part; Actuator N over 5 ms; engine threads realtime N, refused N (oneios-audio-rt=1)` with the counter line, since
  boot. "Written by the HAL only in part": the HAL's write end was inside the period when the engine took it (iOS's
  I/O thread late, not the engine: then its rest plays as silence). A phone boot with the fix should show the
  speaker's `over 5 ms` near 0, the `speaker engine:` line's average under 0.5 ms and its max far under 28 ms, the
  helper's target back at 10 ms and its `latency about` near 15 ms; if `written by the HAL only in part` grows, the
  HAL's own thread is late and the speaker's safety offset (240 frames) is the next thing to size.

Tested: the archive built against `HOSTED_OUT=wired`'s tree with the change (`oneios-tool kernel gen audio`,
`ONEIOS_AUDIO_OUT` elsewhere: no warning; `thread_policy_set`, `assert_wait_deadline_with_leeway`, `thread_block`
undefined in `DroidVMAudio.late.o`, as the kernel defines them); `cargo test -p oneios-kernel` (the synthetic
source has the three new anchors; the staged source's hash in `golden/staged.sha256`). Then the kernel: built on the laptop with the whole series (`HOSTED_OUT=audiolate ONEIOS_CRYPTO=original`, 686 s, 182 patches, releasable): `xnu.macho` `6d40f8e0`; prelinked to `out/kernel/24A446-audiolate/kernel.kc` `853068df` (`.hashes` `fa7166ba`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 10 of 10.
`runtime/host-tests.sh` (2026-11-24, after the rebase on 904809b): workspace, lint, lint-android, port, links and package pass, boot skipped. Not booted (no qemu boot, owner's rule of 2026-11-24; not on the phone yet).

### No audio at all in v0.1.16: VirtualAudio never started (fix-audio-camera-model, 2026-11-23; built, not on the phone)

The camera's model name of 2026-11-22 (`/product unique-model` "V54AP") made AVFCapture's AVGestalt answer V54's
webcam key in audiomxd; VirtualAudio then built a webcam subport with no DSP list, its precondition failed and it
never started: no Speaker, no volume, no haptics (the Actuator is a VirtualAudio device too). Cause, fix (the name
"OneiOSAP" and hosted 0088's link `CMCapture.framework/OneiOS -> V54`; `camera-model=V54AP` the old name) and the
phone lines: section 7, "The model name stopped all audio". `oneios-tool ioslog health` says in its `audio` line
whether VirtualAudio started (`started 1`) and which precondition failed.

### Missing, unknown

- **Not run yet in a test cycle of mine:** `owner-hide-vmm=audiomxd,com.apple.WebKit.GPU` (VirtualAudio's Null device
  when the guest says it is a VM; audiomxd took both devices without it, so whether it changes the routes is [U]),
  the kernel's self-test tone, a sound iOS is asked for (the lock click with the power key; [U] iOS's ringer state:
  the buttons' Ring Enable bit is 0, which may mean silent, so UI sounds may be muted), the microphone in a session,
  and the helper with the stall fix. The Mac `kernel.kc` I built before (`61d926d8`, `636ec3fa`) is superseded:
  `kernel.kc` from main has the archive and boots.
- Playback from uid 0 with system_server gone works (above). [U] Recording in a session: whether the record app-op
  silences uid 0 without system_server (AppOpsManager is a system_server service) is not measured yet.
- [U] Whether VirtualAudio of iPhone18,2 builds its speaker route on "Speaker" and "Mic" (UniXNU-droidvm's iPad
  UIDs). iOSPlus10's iOS 16 iPhone needed "Speaker" with a silent stereo input and "Hawking"; the UIDs are boot
  arguments now, a third device would be a change of the staged source.
- Volume (corrected 2026-11-22): iOS 27's speaker route wants hardware volume, not software volume; the Speaker's
  volume control and the host stream at full scale: "Volume" above.
- Latency: 131-142 ms on the phone before 2026-11-13, 105-123 ms (host side) in the 8644ae9 session; since
  2026-11-15 MMAP exclusive and the smaller speaker ring ("Latency end to end" above): [U] what the phone's HAL gives
  and the real figures, until the phone runs it. A FAST track needs a callback's realtime thread (not done).
- Routing: One UI's audio policy's default for a media stream (speaker; a wired or Bluetooth headset if connected
  [U]). The earpiece needs a voice-communication stream or AAudio's device id of the earpiece port: not done. Calls
  are out of scope (no baseband in the guest).
- The Actuator device (Core Haptics taps, section 5) is an IOAudio2Device of the same archive, but its periods go to
  the haptics bridge, not to virtio-snd and this helper.

## 7. Camera (fable-camera, 2026-11-11; camera, 2026-11-12)

Built on all three sides. **Where it broke (iOS's own log of the 2026-11-12 sessions): iOS's capture stack never used
our camera.** CMCapture 27 probes for one of six camera driver classes, found none in 5 s ("Camera driver not
started in time"), so its device vendor had no create function and every client got error -12786 and no camera;
the old plug-in was in no image and nothing would have loaded it. **The route now (2026-11-12, built, not run):
Apple's own plug-in loader.** The kernel publishes `AppleH9CamIn`; for that class CMCapture `dlopen()`s
`/System/Library/MediaCapture/H9ISP.mediacapture` and makes every device with its `H9ISPCaptureDeviceCreate`; that
file is our plug-in, on the Data seed and shown there by hosted 0056; it reads frames through AppleH9CamIn's user
client, which `cameracaptured`'s sandbox allows. No method of CMCapture is replaced. Details and the log lines the
next session must show: `userland/camera/README.md`. The host side's first phone run (camctl, NDK back end, 2026-11-11):
the cameras are listed and one opens, no frame arrived; the cause is fixed in the code and the fix has not run on
the phone yet ("The first phone run" below). Also `runtime/bridges/camera/README.md` (the host, the phone commands),
`drivers/camera/README.md`.

**The 2026-11-22 sessions (main 203a9e7, kernel with 0082; iOS's own log `out/session-logs/2026-11-22/s7-ioslog`):
the plug-in route works up to the device; CMCapture then had no model name.** In order: the kernel published
`AppleH9CamIn`, 0056 mounted the plug-in, `fcc_probeCameraDriverService ... Camera driver AppleH9CamIn is active`,
`ONEIOS CAMERA: ios: cameracaptured[51]: loaded`, `the device made: stream 0 Back Camera (17 formats)`, `stream 1
Front Camera (8 formats)`, `Created FigCaptureDeviceRef`, `ONEIOS CAMERA: user client opened by cameracaptured`. Then,
at each of cameracaptured's lookups: `Fig assert: "modelSpecificAVCaptureSessionPlist" at loopBail
(FigCaptureSourceBackingsProvider.m:2316)`, err -12780, `Created 0 in-memory built-in backings`: no source, so no
stream was ever asked for and no frame went to cameracaptured. **Cause** [F, read in the 24A446 shared cache]:
`csu_createSourceInfoDictionariesFromAVCaptureSessionPlistForCaptureDeviceIDs` reads its per-model plist with
`fcsbp_plistCreateAndPreprocess(FigCaptureGetModelSpecificName())`; that name is MobileGestalt's
`HWModelDescriptionForCamera` less a trailing "AP" (or "DEV"), and MobileGestalt answers it from `/product
unique-model` (on the iPhone18,2 "V54AP", so `CMCapture.framework/V54/`). The guest's `/product` had no
`unique-model`, so the name was NULL. **Fix (2026-11-22, built and host-tested, not run on the phone):** the loader
writes `unique-model` "V54AP" and the camera nodes (below, "The camera nodes"), and hosted 0084 removes
MobileGestalt's cache once when the tree changes (until 0084 the kernel removed it at every boot; `docs/userland.md`,
"MobileGestalt's cache follows the tree"). The same answer is MobileGestalt's `HWModelDescriptionForAnalytics`,
`...ForAudio`, `...ForPowerPerf` and `...ForAutomatedTesting` (one table entry each, all reading `unique-model`):
those were NULL before and are "V54AP" from 2026-11-22, "OneiOSAP" from 2026-11-23 (below), with the cameras only. [U] who reads them (a search of the shared cache
for their importers stopped at its 10 minute limit); audio's tunings are found by HWModelStr (`oneios,xnuhost`, 0078).

**The 2026-11-23 session (runtime 0.1.16, hosted to 0087; `out/session-logs/2026-11-23/boot-0.1.16`): the Camera
icon shows and the app opens, the preview is black; CMCapture dropped the back stream because it runs in a VM.**
The nodes and `unique-model` worked: CMCapture read `V54/AVCaptureSession.plist` and made the device with both
streams. Then `-[BWFigCaptureStream initWithFigCaptureStream:...]` for `Back` was followed at once by `-[BWFigCaptureStream
dealloc]` for `Back`, and every source lookup failed: `unable to find a stream with port type PortTypeBack` (and
`PortTypeFrontSuperWide` for the front devices), `Fig assert: "err == 0 " at bail (BWFigCaptureDeviceVendor.m:1362)
- (err=-12780)`, `Failed to open streams for device Back Camera, this device will be disabled` (and the same for each
device of the plist). So no stream was asked for (`virtio: 60 s: ... camera#0 q0 +1`) and no frame came.
**Cause** [F, read in the 24A446 shared cache]: `-[BWFigCaptureDevice initWithFigCaptureDevice:deviceID:]` (0x1b041bc50)
takes the "external camera replaces built-in" path when `FigCaptureExternalCameraReplacesBuiltIn` is true, and that
is `FigCaptureIsRunningInVirtualization`: `sysctlbyname("kern.hv_vmm_present")` not 0. On that path it removes every
stream whose port is Back, BackTelephoto or BackSuperWide and keeps the front one. Our guest is a VM to iOS
(`/defaults vmm-present` = 1; bluetoothd needs it, section 4). Every caller of the two checks is in cameracaptured
(`_createDevice`, the vendor's init, `_copyStreamsForUnderlyingSourceAttributes`, `captureSourceServer_handleCopySourcesMessage`,
`csu_createVideoCaptureSourceInfoForCaptureDeviceFromModelSpecificPlist`, `CMIOExtensionFigCaptureStreamCreate`).
A second fault was behind it: V54's front devices ("Front Ultra Wide Camera", "Front Camera") want a
`PortTypeFrontSuperWide` stream, and ours was `PortTypeFront`.
**Fix (2026-11-23, host-tested, not run on the phone):** xnuhost adds `cameracaptured` to the boot arguments'
`owner-hide-vmm=` (the kernel's per-process `kern.hv_vmm_present` switch, Droid-VM-UniXNU's patch, in our kernel
since 2026-11-11) when the camera helper lists a camera; only cameracaptured is told it is not in a VM, bluetoothd
and the rest keep the VM. `camera-vm=show` in `xnuhost.conf` turns it off (`docs/booter.md`). The plug-in's front
stream is `PortTypeFrontSuperWide` and takes the formats of "Front Ultra Wide Camera" by name (a new plug-in: it reaches
the phone only with a new seed; the back camera needs only the new xnuhost). Lines that prove it
on the phone: `userland/camera/README.md`, "What the next session's logs must show".

**The second 2026-11-23 boot (main 44d9c49; iOS's log on the laptop `rel-logs/ioslog-int2/ioslog.txt`, xnuhost's
`rel-logs/boot-int2.log`): the VM fix worked, the back stream stayed, and the preview was still black.**
`oneios-iokit: cameracaptured[51] sysctl kern.hv_vmm_present -> 0`, `Created 2 sources`, `device: streams asked for (1)`.
Then five faults, each traced to its cause [F, read in the 24A446 shared cache with `ipsw dyld disass|xref`]:
1. `failed to copy ModuleInfo (err=-12787)`, then `Fig assert: "( portType && sensorIDString )"` and `Unexpected sensor ID
   ((null)) found for PortTypeBack`. `BWFigCaptureStream` asks `ModuleInfo` with requireSupported and answers -12787 itself
   when our SupportedPropertiesDictionary does not list it; `+[FigCaptureCameraParameters sensorIDStringFromModuleInfo:]`
   is `[NSString stringWithFormat:@"0x%04x", ModuleInfo[kFigCapturePropertyValue_SensorID ("sensorID")]]`, and
   `csu_createMultiStreamSourceFormatsArrayAndAddAVCompressionSettingsAttribute` looks the camera's tables up by port type
   and that string (V54's `CameraSetup.plist` `DefaultSensorIDs`: PortTypeBack `0x0903`, PortTypeFrontSuperWide `0x0914`).
   **Fix:** the plug-in lists and answers `ModuleInfo` `{sensorID: N}`, N from `DefaultSensorIDs` of the stream's port.
2. `failed to resolve format 'Video-FES-60'` (and every other name), then `Fig assert: "streamFormatDict"` and
   `"pixelFormatIsSupported"` for each AVCaptureDevice format. `csu_resolveFormatIndexForNamedStreamFormat` (CMCapture
   0x1b07b7644) compares a named format's SensorWidth/Height, Binned, HDR, FES with FESWidth/Height, MaxFrameRate,
   SIFREnabled with its binning factor (SIFRBinningFactor, else 2 when binned), QuadraBinningFactor,
   OptimizedForMidFrameSynchronization and SecureSigningFaceIDSupported with the stream format's SensorWidth/Height,
   VideoIsBinned, SensorHDRMode, FrontEndScalerOutputWidth/Height, VideoMaxFrameRate, SIFREnabled, SIFRBinningFactor,
   QuadraBinningFactor, OptimizedForMidFrameSynchronization and SensorSigningEnabled (first match wins, Experimental ones
   skipped). Our formats had no FrontEndScalerOutput size and no SIFR factor, so none matched.
   **Fix:** `userland/camera/oneios_camera_formats.c` makes those keys from each named format and from each companion
   (`Photo[DepthCompanionFormat]`: the parent's keys with the companion's over them), and the plug-in runs CMCapture's rule
   over its own list at each create (a lasting line, below). The outputs also list `x420` and `x422` (V54's 10-bit device
   formats; filled by `oneios_camera_scale.c`); the default `420v`/`420f` were there.
3. `native-metal: heap refused: ... UndefinedValue { ty: "MTLResourceOptions", raw: 6291968 }` (0x600200), then
   `Fig assert: "_heap" ... Failed to allocate metal heap` (FigMetalAllocator.m:878). FigMetalAllocator's heaps set bits 21
   and 22, which metal2vulkan refuses as not Metal's. **Fix** (our Metal plug-in, `docs/gpu.md` "What the camera needs"):
   the frontend takes unknown option bits off, once said per set.
4. `native-metal: missing NBLibrary newFunctionWithName:constantValues:pipelineLibrary:error: (answered nil)`, and
   `Fig assert: "function" ... function is nil (FigMetalContext.m:565)`: CMImaging's `-[FigMetalContext
   computePipelineStateFor:constants:additionalPipelineOptions:reflection:]` makes every compute function with that
   selector. **Fix:** NBLibrary answers it (the pipeline library is not needed: the runtime compiles its own).
5. cameracaptured `exited due to SIGSEGV` about 2 s after (3) and (4), 7 times in a few minutes (pids 51, 259, 271, 281,
   ...), each restart opening the camera again. [I] the crash is the graph using the nil heap or function (no stack was
   logged: the boot's `sheng-exc-name=` named only newfs_apfs); our plug-in's own code was not running then (its lines stop at `stopped (0
   pictures given)`). The plug-in also opened the phone's camera at the vendor's stream reservation for its published
   device (each cameracaptured start: `reading frames through AppleH9CamIn`, `no frame: Resource temporarily unavailable`);
   **fix:** it opens the camera at the graph's Start only, and closes it when the streams are given back.
The plug-in reaches the phone without a new seed since hosted 0090 (the boot addon, `oneios-tool phone addon`).
Host-tested; the next boot's lines: `userland/camera/README.md`, "What the next session's logs must show".

**The boot of 2026-11-23 13:45 (boot-jobs, the plug-in and the Metal plug-in by the boot addon): cameracaptured no
longer crashed and the Camera app still showed nothing** (`docs/HANDOFF-2026-11-23.md` 4c). The streams asked for at
08:17:26 and given back at 08:17:28 were the vendor publishing the device (its 2 s close timer), not a session. The
Camera app opened on the front camera, and "Front Camera" had no format (`Can't add <AVCaptureDeviceInput: ... [Front
Camera]> because the device does not support AVCaptureSessionPresetHigh`), so its session had no input. Causes [F, read
in the 24A446 cache with `ipsw dyld disass` and in V54's plists]:
1. Every named format of the front devices failed, and with them their device formats (`AVCaptureDeviceFormat
   '1080p30-16x9-422-10Bit' is derived from FigCaptureStreamFormat 'Video-16x10-FES-30', but that format couldn't be
   found`): V54's front devices keep their formats in `deviceInfoBase` files (`FrontUltraWideCamera-DeviceInfoBase.plist`,
   ...), which the plug-in did not read, and three devices ("Front Ultra Wide Camera", "Front Camera" = the plist's
   "Front Virtual Camera", "Front Ultra Wide Metadata Camera") resolve against the one front stream. **Fix:** the front
   stream has all three devices' formats with their bases. The `x422` output was already there.
2. `failed to resolve format 'Photo[FESCompanionFormat]'` and the two `[MidFrameSynchronizationOptimizedCompanionFormat]`
   of the back: CMCapture makes a companion from a copy of its parent with fixed steps (an FES companion gets `FES` true,
   a mid-frame one `OptimizedForMidFrameSynchronization` true, ...); the plug-in only laid the companion's keys over the
   parent's. **Fix:** CMCapture's table in `oneios_camera_formats.c` (`userland/camera/README.md`, "How CMCapture builds a
   companion"). On V54's own plists the old way gives exactly the phone's 3 back failures, the new way none.
3. `failed to copy WhiteBalanceGain (err=-12787)`: asked with requireSupported for the source's DefaultWhiteBalanceGains
   (`{RedGain, GreenGain, BlueGain}`, 4.12 fixed point). **Fix:** listed and answered 1.0 each.
4. `Fig assert: "pixelFormatIsSupported"` (11): the ProRes RAW (`btp2`) device formats, which stay off. No change.
5. `Failed to open streams for device Back Telephoto Camera, this device will be disabled` (and the ultra-wide, dual and
   triple): no stream on those ports; CMCapture disables them and goes on, which is V54's device list without them. No
   telephoto stream of the wide camera (it would publish a 4x lens that is not there). No change.
Host-tested and built for iOS (fix-camera-formats); not run on the phone.

### The model name stopped all audio: `OneiOSAP` and a link to V54 (fix-audio-camera-model, hosted 0088, 2026-11-23)

**The fault (the same v0.1.16 session, iOS's log `ioslog2/ioslog.txt`, 20:21:48):** no sound, no volume and no haptics
for the whole session. audiomxd logged `RoutingSettings_N71.cpp:1195 Creating subport config for webcam recording`,
then `PRECONDITION FAILURE (std::logic_error)`, `Precondition failure: webcamRecordingDSPList && "Need valid DSPConfig for
webcam recording"`, `VA Init Status: 1`, no `Ending Deferred Initialization`, no VAD device and no Speaker port, and
`VirtualAudio PlugIn is not initialized yet` 1618 times. The 2026-11-22 boots (s6-s9b, before 9ae1f0c) have no
webcam line, `Ending Deferred Initialization` and `VA Init Status: 0`.

**Cause [F, read in 24A446's VirtualAudio.plugin and shared cache on the laptop]:** RoutingSettings_N71 makes the
webcam subport only when `AVGestaltGetBoolAnswer(AVGQ3J3FEVOOCNOKKTK3XQPUQ47DYY)` is true, and its DSP list exists
only with input processing on (this guest logs `Input processing disabled`). AVFCapture's `AVGestaltGetDeviceFeatures`
looks up `FigCaptureGetModelSpecificName()` in its table of 41 models (each entry a `-` separated list of names:
`D93-D94`, `V53-V54`, `J517-J517x-J518-...`; `componentsSeparatedByString:` and `containsObject:`), and the webcam
key is 1 in every entry. That name is MobileGestalt's `HWModelDescriptionForCamera` less a trailing "AP" (or "DEV";
"v64s" becomes "V64s"): `___FigCaptureGetModelSpecificName_block_invoke`. Before 9ae1f0c the name was NULL (no
entry: false); 9ae1f0c's `unique-model` "V54AP" gave "V54", found in `V53-V54`.

**Fix (built and host-tested on the laptop, not run on the phone):**
- The loader's `/product unique-model` with the cameras is `CAMERA_MODEL` "OneiOSAP" (`runtime/loader/src/machine.rs`,
  `Machine::unique_model` for `camera-model=`; the C's `XL_CAMERA_MODEL` in `machine.c`), in no entry of either table, so AVGestalt answers every key as with no name and
  VirtualAudio builds no webcam subport. xnuhost's **`camera-model=NAME`** (`xnuhost.conf` or `--set`; 3-15 letters and
  digits ending in "AP"; `docs/booter.md`) chooses another: `camera-model=V54AP` is the name of 9ae1f0c, for an A/B
  boot. The name is part of the identity of hosted 0084, so a change of the setting makes MobileGestalt's cache again
  once in an existing Data store. One xnuhost line: `camera: unique-model OneiOSAP (camera-model=): CMCapture's tables
  through the kernel's link to V54, AVGestalt has no entry (VirtualAudio builds no webcam subport)`.
- CMCapture still reads V54's tables: its folder for "OneiOSAP" is `CMCapture.framework/OneiOS`
  (`fcmss_createPropertyListForModelAndBundleIdentifier`: the bundle `com.apple.CMCapture`, the model's folder, then
  the plist; there is no `Default` folder on the image, `image apfs-ls` of `os.img`). Hosted 0088 makes
  `/private/var/oneios/addons/CMCapture/OneiOS -> V54` before launchd and shows that folder over
  `/System/Library/PrivateFrameworks/CMCapture.framework` read-only (0031's union mount, as 0056 and 0078). The name
  comes from the tree's `/product unique-model`, so the kernel follows `camera-model=`; a name whose folder is sealed
  (`V54AP`) or no `unique-model` (no camera): nothing is made or mounted. `oneios-cmcapture-model=0` turns it off,
  `=NAME` links another folder. Kernel line: `oneios-cmcapture-model: /private/var/oneios/addons/CMCapture over
  /System/Library/PrivateFrameworks/CMCapture.framework: OneiOS -> V54 (link made 0), stage 5, status 0`.
- `oneios-tool ioslog health` has an `audio` line: `started` (`Ending Deferred Initialization`), `webcam`, `failed
  at` and `failed` (the precondition's file:line and text, as names), `status` (`VA Init Status` not 0), `not ready`.
- Tested on the build laptop (2026-11-23, the C runtime on main): the loader's tests (196 checks), `runtime/host-tests.sh` (all nine parts;
  xnuhost 14,283 checks with `camera-model=` from `--set` and from the image set's `xnuhost.conf`, the bad names refused),
  `cargo test` of `oneios-ioslog` and `oneios-tool` (the `audio` kind on the hand-written lines of the v0.1.16 boot and a
  good start), `tests/phone/run.sh` and `tests/cycle/run.sh` (ALL OK), `runtime/build.sh` (`bin/xnuhost` sha256
  `dc4f7e17e2ce6b4f...`), the kernel with 0088 (`kernel.kc` sha256 `71d7db1449994d58...`, audit 0 failures, stubs 666
  good; `kernel/patches/oneios-hosted/README.md`, "0088"). Not run on the phone.
- The Rust port (branch rust-runtime, 2026-11-23): `cargo test -p xnuloader --test loader` (`machine_cameras_and_identity`:
  "OneiOSAP" by default, `camera-model=V54AP` with its 6 bytes and another identity, back to the default identity;
  `machine_identity_tree`: an identity's own name kept, `camera-model=` over it). xnuhost's `camera-model=` setting
  feeds `Machine::unique_model` (`runtime/xnuhost/src/session/bridges.rs` `camera_identity`, with the camera-vm=
  part: `config::boot_args_hide_vmm_add`; the C's `config.c` and `main.c`); `cargo test -p xnuhost config::` (`image_set_settings`:
  `camera-model=` and `camera-vm=` from the image set's `xnuhost.conf`, the command line winning;
  `boot_argument_helpers`: every case of the C's owner-hide-vmm add and the camera-model names refused). Not run
  on the phone.

**What the camera loses with a name in no table, as far as the code shows [F, the same reading]:**
- AVFCapture's AVGestalt (the app side: `avgestalt_buildDataBase`) has no entry: every one of its per-model answers
  (bool, integer and float tables, hashed key names) takes its default, as before 9ae1f0c. The block sets a few
  keys itself after the lookup (one to false, the Continuity Camera key by platform); those do not depend on the name.
- CMCapture's own `CMCaptureGestalt` (`cmcapturegestalt_buildDataBase`, in cameracaptured) has the same 41-entry table
  and also finds no entry: its per-model answers take their defaults. [U] which of them (hashed names) the back stream,
  the photo path or the formats need: the v0.1.16 boot (V54AP, both tables answered) did not get as far as a frame.
- Functions that compare the name with other models (`BWDeviceModelIsV6x`, `BWDeviceModelIsV68`, ...) answer false for
  both names. The VM regexp alias (`FigCaptureResolveAliasedModelSpecificName`, `VM\d\d\d\d`) is for other names.
- Not lost: the per-model plists (`AVCaptureSession.plist`, `CameraSetup.plist`, the IQ tunings and module configs of
  `V54/`), all read through the link.
- VirtualAudio's own camera checks (`FigCaptureMicrophoneInterferenceMitigationIsSupported`) go by `MGGetProductType`
  and the platform, not by the name. [U] who reads `HWModelDescriptionForAudio` (the same answer): VirtualAudio calls
  no MobileGestalt model key, and a search of the shared cache for callers of its MobileGestalt function found none
  (the calls go through stubs, so not a proof).

The phone's proof (A/B with `camera-model=V54AP`): in iOS's log no `Creating subport config for webcam recording`
before `Ending Deferred Initialization`, `VA Init Status: 0`, a Speaker port, `Reconfiguring volume curve with dB
range {-64, 0}` (`ioslog health`: `audio ... started 1, webcam 0`); in xnuhost's log `oneios-audio: ... frames in` above
0 when a sound plays, `DroidVMAudio: speaker volume N of 128` at a key press, `ONEIOS HAPTIC: counters: ... StartIO`
above 0 after a haptic; the kernel's `oneios-cmcapture-model: ... stage 5, status 0`; and for the camera the back stream
still opens (`ONEIOS CAMERA: ... streams asked for`, the `camera#0` count on the `virtio: 60 s` line).

### The phone [F]

- Sensors (module files in
  [android_device_samsung_sm8650-common@48c66f6](https://github.com/samsung-sm8650/android_device_samsung_sm8650-common/blob/48c66f6543b31784fce4e7d23ea855897f18ce6e/proprietary-files.txt),
  slots 0, 1, 2, 3, 6, 12 in the DT, `qcom,cam-sensor*`, lines 15571-16040):

  | Slot | Module file | Sensor | Role [I from the slot names] | Bridge role |
  |---|---|---|---|---|
  | 0 | `com.samsung.sensormodule.0_lsi_s5khp2.bin` | Samsung S5KHP2 (200 MP) | rear wide | `wide` |
  | 1, 12 | `1_lsi_s5k3lu.bin`, `12_lsi_s5k3lu_full.bin` | Samsung S5K3LU (12 MP) | front | `front` |
  | 2 | `2_sony_imx564.bin` | Sony IMX564 (12 MP) | rear ultra-wide | `ultrawide` |
  | 3 | `3_sony_imx754_x3.bin` | Sony IMX754 (10 MP) | rear 3x | `tele3` |
  | 6 | `6_sony_imx854.bin` | Sony IMX854 (50 MP) | rear 5x | `tele5` |

- Services running in a session: the provider HAL `vendor.samsung.hardware.camera.provider`
  (`ICameraProvider/internal/0`), `media.camera` (cameraserver), and the vendor-facing
  `android.frameworks.cameraservice.service.ICameraService/default`.
- Cameras by cameraserver [F the phone, SM-S928B, Android 17 `CP2A.260605.016.S928BXXU6EZIF`, `camctl --backend
  ndk list` as root, 2026-11-11]. Every one has YUV sizes up to 1920x1080 and the frame rate ranges 15-15, 24-24,
  15-30, 30-30, 15-60, 60-60:

  | Id | Facing | Focal length (35 mm equivalent) | Sensor array | Turned | Kind | Bridge role |
  |---|---|---|---|---|---|---|
  | 0 | back | 6.30 mm (22) | 4080x3060 | 90 | logical multi-camera | `wide` |
  | 1 | front | 3.30 mm (25) | 4000x3000 | 270 | | `front` |
  | 2 | back | 2.20 mm (14) | 4000x3000 | 90 | | `ultrawide` |
  | 3 | front | 3.30 mm (30) | 3392x2544 | 270 | | `other` (not given to the guest) |

  The 3x and 5x telephotos are not cameras of their own: they are physical cameras behind logical camera 0. The
  bridge lists a logical camera's physical cameras after these and opens one as a physical stream of its logical
  camera. [U] their ids, and whether the HAL gives such a stream alone (else: camera 0 with a zoom ratio).
- [U] which ids the provider HAL lists (expected: every sensor and Samsung's hidden ones). Roles are given by
  facing and 35 mm equivalent focal length (below 18 mm ultra-wide, below 45 wide, below 90 3x, else 5x).

### The first phone run (2026-11-11)

`camctl --backend ndk --camera wide --frames 90 capture`, root through KernelSU, One UI running, runtime 0.1.1:
camera 0 opened, the 1280x720 30 fps session was made, the repeating request was accepted, and `no frame arrived`,
with no error.

- **Cause [F AOSP `android17-release`; I that it is the only one]: no binder threads.** cameraserver delivers
  everything by binder calls into the client: the callbacks, and the pictures (the `AImageReader`'s buffer queue is
  in the client; cameraserver's request thread calls its `dequeueBuffer`). An app has binder threads from zygote; a
  plain executable has none and the NDK camera and media libraries start none. The calls made while the client is
  itself inside a call (open, configure) are served on the calling thread, so those succeed; then cameraserver's
  first `dequeueBuffer` waits for ever, and the error callback could not arrive either.
- **Not the cause [F the same sources]:** uid state (uids below 10000 are always active; an idle uid is
  disconnected with an error, not starved), app-ops (none for a native client below uid 10000), the privacy switch
  and device policy (they refuse the open). The reader's format (YUV_420_888), its CPU-read usage and the
  `TEMPLATE_PREVIEW` request with the reader's window as its target were already right.
- **Fix:** both back ends start the binder thread pool (4 threads, ordinary priority) before anything else; the
  NDK back end prints every session, device and capture callback and every status by name, and at the stop a
  `stopped at:` line that names the stage when no picture came (`camctl -v`, `-vv`). Not yet run on the phone.
- **If it still gives no frame:** the HAL back end (`--backend hal`) becomes the route with One UI running too.

### The path

| Part | Where | State |
|---|---|---|
| Helper process `bin/oneios-camera`: opens a camera, NV12 frames with the sensor's time into a memfd ring | `runtime/bridges/camera` | built for Android; 131 host checks with a test picture; on the phone: the list and the open work, no frame yet (above) |
| xnuhost: the helper's socket and ring, the `oneios,camera` virtio device (`/arm-io/camera`, `DeviceKind::Camera`) | `runtime/bridges/camera` (the crate `onecamera`, linked statically), `runtime/xnuhost/src/session/bridges.rs` | linked into xnuhost; the device tested on xnuhost's transport against a guest of plain memory |
| Kernel: the transport and `/dev/oneios-camera` (the `NCFRAME1` layout of iOSPlus10's and HakiOS's node, NV12); `AppleH9CamIn` and its user client (the same session) | `drivers/camera` | compiles with the kernel's records, in the `HOSTED_OUT=cam` kernel; 37 host checks of the pure parts; on the phone (2026-11-12) the transport came up with 3 cameras; the service not run |
| Kernel: the plug-in's folder over `/System/Library/MediaCapture` | `kernel/patches/oneios-hosted/0056-camera-plugin-mount.patch` | on the phone (2026-11-22): mounted, `stage 5, status 0` |
| Plug-in `H9ISP.mediacapture`: `H9ISPCaptureDeviceCreate`, a FigCaptureDevice with the back and front streams, formats from the device's own `AVCaptureSession.plist`, frames scaled into the graph's pools | `userland/camera` | built with Xcode (arm64e, `macho-check.py` PASS); on the Mac macOS's own `BWFigCaptureDeviceVendor` makes a device with it and a frame reaches an output's handler (35 checks); in iOS (the phone, 2026-11-22): cameracaptured loads it and it makes the device with both streams, then CMCapture made no source (no model name, fixed above); 2026-11-23: the back stream dropped in a VM and the front port wrong (fixed above, not run); placed in the seed by the image hook (a new seed) |

- **Route on the host** [F AOSP android15-release for the reason, I for Samsung's build]: the provider HAL directly
  when system_server is gone, Camera2 NDK through cameraserver while One UI runs (`auto`). cameraserver refuses every
  client outside its own process without system_server: its camera-permission check asks the `permission_checker`
  service, waits 10 s for it and denies (uid 0 too). The uid-state and app-op checks would pass. The HAL needs
  nothing of system_server. Both back ends are in the helper and in `camctl`.
- **No ISP in the guest**: the HAL's pictures are finished. Exposure, focus and gain commands of a raw-sensor reader
  are accepted and ignored.
- **A frame**: NV12, at most 1920x1080, 4:2:0 BT.601 full range (iOS's `420f`), a sequence number that rises by one
  per frame the camera made, the sensor's time, the camera's role and how it is turned. Copies: gralloc buffer ->
  ring (conversion), ring -> guest buffer, guest buffer -> reader (read()), reader -> IOSurface.
- **What iOS needs** [F the 24A446 cache for the loader; F iOSPlus10 `docs/laurel_sprout/camera.md` section 3 for the
  rest]: Apple's ISP firmware protocol cannot be served by another camera, so the IOKit side is only a name:
  `AppleH9CamIn`, which CMCapture's probe matches and for which it loads `H9ISP.mediacapture` from
  `/System/Library/MediaCapture` (on iOS 16 HakiOS had to replace `+[BWFigCaptureDeviceVendor
  sharedCaptureDeviceVendor]` instead; iOS 27 takes the create function from the plug-in it loads). The plug-in's
  FigCaptureDevice hands CVPixelBuffers to the capture graph. The app needs, in order: the capture servers running,
  a FigCaptureDevice with a steady preview stream, a still from the same stream, and only then the Camera icon's
  device tree nodes (`/arm-io/isp` `camera-rear`, `camera-front`; `/product/camera`): with the nodes and no frame
  path the launch hangs [F iOSPlus10, iOS 16]. On 27 the nodes are added now (below): the source list itself needs
  `unique-model`, and without the nodes nothing can open the Camera app to test the stream.

### The camera nodes (2026-11-22)

The loader (`runtime/loader/src/machine.rs`, `Machine::cameras`) adds them only with the `oneios,camera` device and
only for the roles the helper lists; xnuhost waits for the list before the tree (at most 3 s; 170 ms on the phone).
Values are the iPhone18,2's (`out/kernel-inputs/24A446/devicetree.bin`, `xnuloader-inspect adt`):

| Node, property | Value | Given when | MobileGestalt answer (24A446, read in libMobileGestalt) |
|---|---|---|---|
| `/arm-io/isp` `camera-rear` | u32 1 | a back camera is listed (wide, ultra-wide or a telephoto) | `rear-facing-camera`; `still-camera` (Camera.app's `SBAppTags`) |
| `/arm-io/isp` `camera-front` | u32 1 | the front camera is listed | `front-facing-camera`; `still-camera` |
| `/product/camera` | empty node | either | `video-camera` (presence) |
| `/product` `unique-model` | "OneiOSAP" (`camera-model=`; "V54AP" from 2026-11-22 to 2026-11-23) | either; `camera-model=` wins over an identity tree's own | `HWModelDescriptionForCamera`: CMCapture's model folder `OneiOS`, hosted 0088's link to `V54` (the model name stopped all audio, above) |

Left out on purpose: `/arm-io/isp`'s other properties (`compatible "isp,isp-generic-ex|isp,s5l8960x"`, `reg`, the
exclave services: no driver may match the node); `/product/camera`'s feature flags (the iPhone's has about 80: `flash`,
`live-photo-capture`, `rear-cam-superwide-capability`, `rear-cam-telephoto-capability`, slow motion, 4K, portrait,
...), because the plug-in gives two streams of at most 1920x1080 and no torch: a flag the plug-in cannot honour would
make the Camera app offer a mode with no device behind it. The ultra-wide and the telephotos get their flags with
their streams. The S24 Ultra defaults are unchanged (the nodes follow the camera device, which follows the helper).
Host test: `runtime/loader/tests/loader.rs` `machine_cameras_and_identity` (no nodes without the device; both, rear only, front only with an
identity; the identity's own `unique-model` kept unless `camera-model=` names one; `camera-model=V54AP` and its other identity; the identity hash).

### The stream that never started (fix/s24-fps-heat-camera, 2026-11-25; built, not on the phone)

In the gpumem session of 2026-11-24 (`docs/s24-fps-heat-camera.md`) the Camera app opened and the preview stayed
black: `camera#0 q0` moved only at the device's creation, so the graph never called the stream's Start. Its Metal
compute pipelines failed meanwhile, `oneios-gpu-text: cameracaptured[62] problem compute pipeline ...:
Compile(Budget(Time))` (the noise-reduction kernels, `kMotionThreshold`, `kIIRUpdateCoefficient`): metal2vulkan's 20 s
compile deadline on a phone with every core at its top clock. What changed:

- The compile deadline of every Metal client but backboardd is 120 s (`oneios-compile-s`, docs/gpu.md 2.5b).
- The stream posts StreamStarting and StreamStarted around a Start and StreamStopping and StreamStopped around a Stop
  (CoreMedia's notification center, the stream as the object), as H10ISP's does; ported from Jesvi-iOS
  `userland/src/camera_device.c` `post()` (a13e406, MIT), where BWFigVideoCaptureDevice waited for StreamStopped and
  every stop otherwise timed out. Switch `oneios-camera-notify=0`.
- A crash of cameracaptured in the Metal plug-in's reflection (`nmb_compute_pipeline_reflection`, SIGSEGV at 0): a
  compute pipeline rebuilt with its unset function constants zero freed its function before the reflection read it
  (CCPortrait's kernels in cameracaptured's precompilation, every boot on Droid-VM). Ported from Droid-VM/NativeMetal
  3c46b5e (MIT) as a frontend edit (docs/gpu.md "What the camera needs"); no switch (a crash fix). Proof: no
  cameracaptured crash report naming `nmb_compute_pipeline_reflection`, no `oneios-kill` exit of cameracaptured with the
  Camera app open.
- Checked, no change needed: the names the plug-in takes (`VideoOutputConfigurations`, `VideoOutputsEnabled`,
  `VideoOutputHandlers`, `PixelBufferHandler`, `BufferPool`, the outputs `PrimaryScaler`, `SecondaryScaler`,
  `StillImage`) are in 24A446's CMCaptureCore as they were in iOS 16 (`oneios-tool sym cstr`); iOS 27 adds outputs
  (`IntermediateTap`, `FrontEndPyramidScale0`, `StillImageRaw`, ...), which the plug-in names once as `stream: an output
  not here:`.

Proof lines (`phone pull-log`, iOS's log, with the Camera app open): no `Compile(Budget(Time))` for cameracaptured;
`ONEIOS CAMERA: ios: cameracaptured[N]: stream: posted StreamStarting`, `stream Back Camera: the graph starts the
stream (format N)`, `stream: posted StreamStarted`, `stream Back Camera: the first frame: ...`, `Back Camera
PrimaryScaler: the first picture given ...`; xnuhost's `virtio: 60 s` with `camera#0 q0 +N` while the app is open.

### Missing, unknown

- **The first session with the sensor IDs, the resolved formats and the two Metal fixes** (the phone check:
  `userland/camera/README.md`, "What the next session's logs must show"): whether the graph builds and starts the
  stream, whether cameracaptured still crashes (boot with `sheng-exc-name=newfs_apfs,cameracaptured`: the kernel then
  logs its exceptions' pc, lr, far and return addresses for `oneios-tool sym`), and which
  properties it asks for that are not answered (`WhiteBalanceGain` was asked and is not listed).
- [U] on the phone: the capture again with the fix, the telephotos, the diagnostic run if no frame arrives
  (`runtime/bridges/camera/README.md`, "On the phone" and "When no frame arrives"), and "In a
  session" (the HAL back end with system_server stopped; whether Samsung's HAL takes a plain YCbCr_420_888 stream
  from a client that is not cameraserver; what a killed helper leaves in the provider).
- oneos: if a killed helper leaves the camera "in use" in the provider, the provider's init service has to be
  restarted at the return; its name is not known yet (`getprop | grep -i 'init.svc.*camera'` on the phone).
- Stills above 1920x1080, exposure/focus/zoom/flash from iOS, two cameras at once: not done.

## 7a. Panel backlight and ambient light (fable-brightness, 2026-11-11)

Details and the phone test: `runtime/bridges/brightness/README.md`, `drivers/brightness/README.md`.

### The phone

- Panel: `E3_S6E3HAF_AMB679FN01`, command mode, `qcom,mdss-dsi-bl-pmic-control-type = "bl_ctrl_dcs"`,
  `qcom,mdss-dsi-bl-min-level = <1>`, `qcom,mdss-brightness-default-level = <0x9f6>` (2550) [F DT lines 9233-9243].
  The node carries no `bl-max-level` of its own (the `PBA_BOOTING` stand-in panels have 0x639c = 25500 and 0xfff).
  Brightness is a DCS command that Samsung's panel driver (`ss_dsi_panel`, a vendor module, not in the kernel
  repository) builds from its candela tables (`samsung,ss_candela_map_table_revA`).
- The backlight class device: expected `/sys/class/backlight/panel` (Samsung's name; Qualcomm's is
  `panel0-backlight`) [I, not read on the phone]. `max_brightness` [U]: `brightctl info` prints both.
- Light: `als_rear` = Sensortek STK6D2x, I2C address 0x47, `stk,als_scale = <1000>` [F DT line 11736,
  `drivers/optics/stk6d2x_sec.c`, `stk6d2x.h:25`]. Samsung's rear flicker sensor: it faces away from the screen.
  Off until `1` is written to the input device's `enable` attribute; then one report per frame of its FIFO:
  `REL_X` infrared, `REL_RX` UV, `REL_RY` clear (each the gain-normalised average + 1), `ABS_Y/Z/RX` the gains
  (`stk_sec_report`). `/sys/class/sensors/als_rear/` has `als_ir`, `als_clear`, `als_uv`, `als_raw_data`. No lux in
  this mode; the scale to lux is not calibrated [U].
- The front light sensor is on the sensor hub (SSC): read over QRTR by `runtime/bridges/sensors` with `sensors=auto|ssc`
  (section 7i; on by default since 2026-11-25 with `sensors=auto`, not booted); the brightness bridge then takes its readings instead of `als_rear`'s.

### iOS side (`drivers/brightness`, port of iOSPlus10 `2201-laurel-backlight-panel-power.patch` and `9005-laurel-backlight-calibration.patch`)

- `OneiOSBacklight` (service name `AppleARMBacklight`): `backlight-control`, `IODisplayParameters`, `max-nits`,
  `min-nits`, `brightness-nits`, `backlight-calibration-parameters`, `backlight-marketing-table`; takes
  `{brightness, FadePeriod}` and `brightness-nits` and sends `OB_BRIGHT_SET` on the bridge channel. Since
  2026-11-12 also iOS 27's keys (from UniXNU-sheng's working iPadOS 27 service): `IODisplayParameters.BrightnessMilliNits`,
  `LmaxProduct`/`LmidProduct`/`LminProduct`, `NativeSandboxGrantProcess = backboardd`; `brightness-nits` signed.
  Since the 15:35 session also `DisplayBrightness` (the slider's position 0..1, or a dictionary `{Brightness, Commit,
  Period}`, read in 24A446's CoreBrightness): taken as the slider's level when its value is unambiguous (a float's bits,
  data or text; not the integers 0 and 1), and logged with what it held (`drivers/brightness/README.md`,
  "DisplayBrightness"). Host-tested decoder; not run.
- **Every level the dimmest (fix-brightness-path, 2026-11-23).** Phone, 2026-11-23: CoreBrightness used
  frame-synchronous brightness (`UseSyncBrightnessTransactionForDisplay: 1`), which goes to CoreAnimation, and QuartzCore
  has no brightness handler here (`Display 1, no brightness handler registered`); the service got only a truncated
  float and slider positions of 0. Fixed, not booted: backboardd's `useSyncBrightnessTransactionForDisplay:` answers NO
  (`BacklightMode.inc`; `oneios-bl-sync=1` keeps iOS's answer), so CoreBrightness sets `brightness-nits`; `LmidProduct`
  and `LminProduct` are 16.16 nits (CoreBrightness read the whole numbers as 1/65536 of them); `brightness-nits`, once
  it came, wins over the slider position. Read in 24A446's CoreBrightness with the addresses:
  `drivers/brightness/README.md`, "Why every level was the dimmest".
- **The slider, nits 0 and the touch IC (fix-brightness-touch, 2026-11-14).** Phone, 2026-11-13 (main 9f6dc38): iOS
  sent `brightness-nits` 0 with its display on; the panel went to its floor and the bridge slept the touch IC (dead
  touch until woken by hand); Control Center's slider sent only `DisplayBrightnessSliderPosition` and
  `DisplayBrightness`, both dropped. Read in 24A446 [F]: IOKit's serializer turns a CFNumber float into its truncated
  integer (`Brightness number 0x0 (31 bits)`: 31 is Float32's width there), so `DisplayBrightness` cannot carry the
  position; `DisplayBrightnessSliderPosition` and `brightness-nits-cap` are SInt32 16.16. Now: the slider position is
  the slider's level (the sliders work, no iOS-side code), the cap is kept and holds `brightness-nits` under it, nits
  of 0 or less are no level (the panel keeps its own), and the touch IC follows iOS's display power: backboardd's
  `IOMobileFramebufferRequestPowerChange` (selector 12, seen by hosted `0069`, sent as `OB_BRIGHT_POWER`) sleeps it at
  off and wakes it at on, and every power-key press wakes it (xnuhost's evdev reader). The return writes One UI's value
  and wakes the IC as before. Host tests: `test_bl_request` 2038, the bridge's brightness 81, `runtime/bridges/tests`
  (the channel's `OB_BRIGHT_POWER`), xnuhost (the power-key hook); the driver compiles (`oneios-tool kernel drivers
  --check`); the kernel with hosted 0069 and the new driver built (HOSTED_OUT=fixbr, xnu.macho 122efae5) and prelinked (audit 0
  failures, stubs 666 good, kernel.kc 7632e390); the
  boot is not done (not on the PC: no phone panel, touch IC or power key there). Details: `drivers/brightness/README.md` ("The slider, the cap
  and nits 0"), `runtime/bridges/brightness/README.md` ("The touch IC follows the display").
- **Only a change goes to the host (fix-gpu-perf, 2026-11-15).** A Control Center drag sends about 550
  `DisplayBrightnessSliderPosition` requests a second, most of them the level the one before had; each was a bridge
  message (whose fade the host started again) and a re-publish of `IODisplayParameters` and `brightness-nits` (nine
  objects made and freed). Now a request whose level, fade and flags are the last one sent is not sent again within a
  second, and the two properties are set only when they change (`oneios_bl_request.h` `obl_send_wanted`,
  `obl_publish_wanted`; iOS's display power request makes the next level go whatever it is). `setProperty` sends no
  IOKit notification, so this was CPU and bridge traffic, not a cause of iOS redrawing (`docs/newtodo.md`, update of
  2026-11-15, "the storms"). Line: `ONEIOS BACKLIGHT: N requests repeated the level last sent (L of 65535): not sent
  again, nothing re-published` (every 1024). Host test: `test_bl_request` 2044 checks. Not run on the phone.
- **Why the slider did nothing** (phone, 2026-11-12): vmm-present makes MobileGestalt answer `IsVirtualDevice`, and
  24A446's CoreBrightness then runs `VMBLControl` (log: `Set VM Mode 3 BLControl`), which sends brightness to a
  Virtualization.framework host over remote XPC and never to the service. `drivers/brightness/BacklightMode.inc`,
  built into the Metal plug-in that runs in backboardd, gives `initBLControl:` mode 1 (BLControl) in backboardd only.
  Evidence, addresses and the next boot's lines: `drivers/brightness/README.md`. Built, not yet run.
- The original `AppleARMBacklight` class is in the kernel collection (AppleARMPlatform) [F kernelcache 24A446], so
  the class cannot have that name; conformance for CoreBrightness's `IOObjectConformsTo` is made by linking the
  class under the original's metaclass at run time (`oneios_bl_conform=0` turns it off).
- Light: `OB_BRIGHT_ALS` readings are kept in the kernel. The way to CoreBrightness is a HID device for the
  original `AppleUSBALSService` (AppleHIDALSService is in the collection): `OneiOSALSDevice.cpp`, built into the
  hosted kernel's HID archive against the collection's `IOHIDDevice` layout (section 7b) and started after the
  touch screen and the buttons. With `oneios-als-spu=1` (2026-11-25; the kernel's default 0, xnuhost adds 1 when the light comes from the hub, which `sensors=auto` makes the default) it is an iPhone's SPU light
  sensor instead, for CoreBrightness itself (section 7i).

### The bridge (`runtime/bridges/brightness`)

- Route: the backlight sysfs node as root (no system_server, no surfaceflinger, no binder). Slider squared over
  `1..ceiling`; never 0; One UI's value written back at the stop.
- `als_rear` read by evdev without a grab; clear counts x `ONEIOS_ALS_SCALE` millilux.
- Two backends (section 7e): samsung, the above; treble, the standard backlight class by named paths
  (`panel0-backlight`, `panel1-backlight`, `backlight`, `lcd-backlight`, `panel`), the whole range, the touch IC left
  alone, no light sensor yet. `runtime/bridges/brightness/README.md`, "The two backends".

### Missing, unknown

- [U] everything on the phone: the node's name and range, writing it in the session's namespace, the sensor's
  `enable` file, the counts in known light.
- [U] everything in iOS: no session reaches backboardd yet.
- The HID light sensor device is built and starts (`ONEIOS ALS: on` in a PC boot) and `AppleUSBALSService`
  attaches to it; no reading has gone through, and whether CoreBrightness uses its events is [U]. A rear sensor is a poor
  input for auto-brightness in any case (dark when the phone lies face up).
- iOS's `IOMFBBrightnessLevel` on the framebuffer is not handled here (the display driver is UniXNU-droidvm's).
- **EDR and nits (fix-display-colour, 2026-11-13).** QuartzCore takes the display's EDR headroom and nits from the
  brightness capabilities (`IOMFBDisplay::update_brightness_capabilities`: `MaxNits`, `MinNits`, `MaxNitsEDR`,
  `EDRPotentialHeadroom`) [F, 24A446]. The real iPhone18,2 tree has them on `/backlight` and `/exbright` [F,
  `out/kernel-inputs/24A446/devicetree.bin`, 16.16 numbers divided out]: `EDRPotentialHeadroom` 8.0,
  `EDRReferenceHeadroom` 12.0, `edr-max-nits` 1600, `user-accessible-max-nits` 1060, `override-boot-brightness-nits`
  80, `panel-gamma` 2.2, `LmaxProduct` 3000, `LminProduct` 10, `DBVMin` 117, `DBVMax` 2047. This service publishes
  `BrightnessMilliNits` 0.038-500 nits and `LmaxProduct` 500 and no EDR key, and the Metal plug-in holds the headroom
  at 1 in backboardd, so iOS renders no EDR here; with the 8-bit sRGB display path that is right. The S24 Ultra
  panel's own luminance is in the display helper's `panel colour mode ...; HDR types ..., luminance max ...` line (not
  read yet); EDR keys for this service wait for that number and the half-float path
  (`kernel/patches/oneios-hosted/display/README.md`, "Colour, depth and the panel").

## 7b. Touch screen and buttons (fable-hid, 2026-11-11)

Details, the layout header's evidence and the log lines: `kernel/patches/oneios-hosted/hid/README.md`.

### The phone

- Read on the phone on 2026-11-12 (One UI 8, `getevent -lp`, `/sys/class/sec/tsp`): `event0` `gpio-keys` (Volume Up only),
  `event1` `pmic_pwrkey` (Power), `event2` `pmic_resin` (Volume Down only), `event8` `sec_touchscreen` (the screen,
  `INPUT_PROP_DIRECT`), `event9` `sec_touchpad` (a pointer device with the same axes, not taken). The numbers can
  change between boots; xnuhost finds the devices by their bits, not by number. Neither key device has `EV_REP`.
- The touch IC: Synaptics S3916T, firmware SY35030014. Multitouch protocol B: 10 slots, `ABS_MT_POSITION_X/Y` 0..4095
  each, `ABS_MT_TOUCH_MAJOR/MINOR` 0..255, tracking ids 0..65535, no pressure axis. Its `cmd` interface offers, among
  others, `set_game_mode`, `dead_zone_enable`, `set_grip_data`, `blocking_palm` and `get_status` (read: `WET:0,NOISE:32,FREQ:0`).
  Nothing of OneiOS writes those; the report rate and the edge rejection One UI left are what a session gets [U: not
  measured].
- The volume keys are one rocker: holding both ends at once can chatter (session of 2026-11-12: 6 key events in 3 s).
  The fork's return chord tolerates releases under 0.5 s since OneOS `7839e4b`.
- No Home key and no ringer switch.

### The path

1. xnuhost (`runtime/xnuhost/src/input.rs`) opens every event device that has multitouch positions or one of the
   three side keys, **read only and without `EVIOCGRAB`**, and puts the events, unchanged, on one virtio-input
   device (`/arm-io/input-0`) with the panel's own axis ranges in its configuration. The switch's Volume Up +
   Volume Down watcher reads the same key devices beside it. Each touch frame also carries its scan time (`EV_MSC` /
   `MSC_TIMESTAMP`, in counter ticks), and after `SYN_DROPPED` the device's state is read again so that no lift is
   lost (`docs/booter.md` 2.8).
2. In the kernel, UniXNU-droidvm's `DroidVMVirtioInput.c` (staged with `OneiOSSideKeys.inc`) reads the queue: the
   finger set goes to the digitizer, a side key's state to the buttons.
3. Two HID devices of UniXNU's `NativeHidInput`, subclasses of the collection's original `IOHIDDevice`
   (`OriginalHidDeviceABI.h`): "OneiOS touch screen" (Digitizer / Touch Screen, ten fingers, each axis 0..32767,
   Built-In, DisplayIntegrated) and "OneiOS buttons" (Consumer Menu, Power, Volume Increment and Decrement,
   Telephony Ring Enable, a boot keyboard).
4. Apple's IOHIDFamily and IOHIDEventDriver, unchanged in `kernel.kc`, parse the reports and make the HID events
   backboardd reads. The touch reports are handed over with the frame's scan time (`handleReportWithTime`), so the
   events carry the time of the scan, not of their arrival.

Where the time of one touch goes and the log lines that measure each step: `kernel/patches/oneios-hosted/hid/README.md`,
"Latency".

Coordinates: each axis of the panel's range is scaled to the digitizer's full range, which iOS maps to the whole
display, so the panel's range covers the guest's mode (the panel's own 1440 x 3120 by default) when the guest's
frame fills the panel. Home is the swipe up from the bottom edge: a finger that lands in the bottom 4 % starts on the edge, and
edge contacts carry the edge flag SpringBoard's gestures need.

### Keys

| Key | evdev code | iOS usage |
|---|---|---|
| Power | `KEY_POWER` 116 | Consumer / Power |
| Volume up | `KEY_VOLUMEUP` 115 | Consumer / Volume Increment |
| Volume down | `KEY_VOLUMEDOWN` 114 | Consumer / Volume Decrement |
| (none on this phone) | `KEY_HOMEPAGE` 172 | Consumer / Menu (Home) |
| (none on this phone) | `KEY_MUTE` 113, each press flips it | Telephony / Ring Enable (the ringer switch) |

The side keys stay ungrabbed: iOS and the switch's watcher both see every press, including the two of the
Volume Up + Volume Down chord that ends the session.

### Missing, unknown

- [U] everything on the phone: the touchscreen's axis ranges and rate, whether it reports while One UI's framework
  is stopped, palm rejection, the feel of the edge band.
- [U] whether iOS 27 reads Ring Enable = 1 as "ring"; nothing sets it on this phone.
- A guest frame that does not fill the panel (letterboxed) needs an offset in the transport's scaling; not written.
- No trackpad, no motion sensors (the transport's code for them is compiled and finds no such device). The S Pen: section 7f
  (off by default, `pen=auto`).

## 7c. Battery (battery bridge, 2026-11-11)

Details and the phone test: `runtime/bridges/battery/README.md`, `drivers/battery/README.md`.
Two backends since 2026-11-15 (section 7e): samsung, the `power_supply` nodes below (the default, unchanged); treble,
the health HAL (`android.hardware.health.IHealth/default`, `getHealthInfo`).

- **Before this**: iOS showed a near-empty battery with no charging state. [I] nothing published an `IOPMPowerSource`
  under the hosted kernel (none of the classes that do has its hardware here); no log or registry dump confirmed it.
- **Route**: `runtime/bridges/battery` reads `/sys/class/power_supply/` in xnuhost (read only, one normal-priority
  thread, every 3 s only while iOS listens); `OB_CH_BATTERY` carries one `struct ob_battery_state`, at once for a
  change of level or charging state, the rest at most every 30 s; `drivers/battery/OneiOSBattery.cpp` publishes it.
- [F 24A446 `powerd`, disassembled] it matches `IOServiceMatching("IOPMPowerSource")`, and its reader needs
  `BatteryInstalled` = true, `CurrentCapacity` and `MaxCapacity`.
- **The order** (since 2026-11-13, `drivers/battery/oneios_battery_props.h`): the guest asks the host before its service
  exists; the host's first reading is in the service's registry properties before `registerService()`, so powerd's first
  match reads the real ExternalConnected; that first publish sends no `kIOPMMessageBatteryStatusHasChanged`, and a later
  host message sends one only when a published value changed. Host-tested (`drivers/tests/host.sh`: 21 checks;
  `runtime/bridges/battery`: 65, among them the phone on its charger: the real state at the query, no report while it
  stays, an unplug and a plug each reported). The code before 2026-11-13 already put the first reading in before
  `registerService()`; it also messaged (to no client yet) at that publish. PC boot with a fake
  charger (2026-11-13): `ONEIOS BATTERY: on: IOPMPowerSource published at 100%, on power (ExternalConnected true, ...
  before registerService)` 3 s in, minutes before launchd.
- **The charging chime** [F 24A446 powerd and SpringBoard, disassembled; `drivers/battery/README.md` "The charging
  chime"]: powerd publishes `Play Charging Chime` = ExternalConnected; SpringBoard's `SBChargingController` chimes for
  an unmarked power source with it true, and at startup marks the connected ones silently only when its startup
  context has the boot overlay (backboardd's boot UI) or is dark. Phone, 2026-11-12 23:45 and again on a later boot
  (c1223f8, iOS log read): the "plugged in" sound played before the lock screen. **Proven** from that log: the boot
  overlay was still up (`addOverlay(1-init 2)`, `Current bootUI is an Apple Logo`) but its boot-UI activity assertion
  had timed out at 37 s before SpringBoard's startup context tested it, so the startup took the chime branch; no
  `isChimeDisabled:` line (not powerd, not a late power-source change). The battery service is not the cause; what the
  startup context's overlay test reads is the backboardd-saved overlay that a clean iOS shutdown leaves and the next
  boot restores. A session ends by killing xnuhost, so iOS never shuts down cleanly and nothing is saved: every boot
  looks like a boot after a crash. The fix is the return's graceful shutdown (`docs/booter.md` "The return",
  `return-graceful-ms`): ask iOS to power off before the kill so it saves its overlay. Host side built and tested; the
  guest half (iOS actually powering off) is the Mac's to build and is not yet verified.

| Android node | iOS key | Mapping |
|---|---|---|
| `battery/capacity` | `CurrentCapacity` (`MaxCapacity` = 100) | percent, clamped to 0..100 |
| `battery/status` | `IsCharging`, `FullyCharged` | Charging -> IsCharging; Full, or Not charging at 100 % on power -> FullyCharged |
| `usb,ac,wireless,pogo /online` (any > 0), or status Charging/Full | `ExternalConnected`, `ExternalChargeCapable`, `AppleRawExternalConnected` | never `otg` (a device on the port draws power from the phone) |
| `battery/present` | `BatteryInstalled` | missing: taken as present |
| `battery/temp` (tenths C) | `Temperature` (hundredths C [I]) | x 10 |
| `battery/voltage_now` | `Voltage` (mV) | numbers >= 100000 are µV, others mV |
| `battery/current_now` | `Amperage`, `InstantAmperage` (mA, + into the battery) | unit learned from size (>= 20000 is µA, sticky), sign from `status` |
| `battery/charge_full`, `charge_full_design` | `NominalChargeCapacity`, `AppleRawMaxCapacity`, `DesignCapacity` | numbers >= 100000 are µAh, others mAh; 5000 mAh if neither exists |
| computed: `charge_full` x percent | `AppleRawCurrentCapacity` | percent is the source of truth for the level |
| `battery/cycle_count` or Samsung's `battery_cycle` | `CycleCount` | |
| computed: remaining mAh / current | `TimeRemaining` (minutes) | 0xFFFF while unknown |

### Missing, unknown

- [F owner's phone, 2026-11-11, on USB] supplies: `ac battery max77775-charger max77775-fuelgauge max77775-otg mfc-charger
  otg pca9481-charger pogo sec-direct-charger usb wireless`. `usb/online`=1, `ac`, `wireless`, `pogo` 0; `battery/online`=19 is
  a cable type and is not read. `capacity`=100, `status`=Full, `temp`=389, `voltage_now`=4294000 (µV), `current_now`=74,
  `charge_full` and `charge_full_design`=5000000 (µAh), `cycle_count` empty, `battery_cycle`=1103.
- [U] `current_now`'s unit: 74 fits both 74 mA and 74 µA at Full. A reading of 20000 or more under load settles it (the
  learned unit); until then the number is taken as mA. Its sign in charging and discharging is not read either; the
  status gives it. `batteryctl` prints them; `ONEIOS_BATTERY_SUPPLIES`, `ONEIOS_BATTERY_MAH` and
  `ONEIOS_BATTERY_CURRENT_UNIT` override.
- [U] that SpringBoard's status bar and BatteryCenter follow `powerd` here, which is the point of the first session.
- [U] that `/sys/class/power_supply` is visible in xnuhost's session namespace (it is read from there).
- Battery health: `powerd`'s `AppleSmartBatteryPack` paths (health, per-cell data) find nothing and stay off.

## 7d. Thermal (thermal, 2026-11-12)

Details: `runtime/bridges/thermal/README.md` (sensors, levels, the step, `thermalctl`), `drivers/thermal/README.md`.
The sensors have two backends since 2026-11-15 (section 7e): samsung, the files below (the default, unchanged);
treble, the thermal HAL (`android.hardware.thermal.IThermal/default`, `getTemperatures`). The levels and the step are
the same for both.

### "It runs damn hot, why" (9f6dc38 session, 2026-11-14; fix-thermal-log)

Measured from outside at about +6060 s: `/proc/vmstat` pswpin +12190 and pswpout +10291 pages in 5 s (zram
compressing the guest's RAM: the memory fix is fix-ram-fit's), top 800 % with 226 % sys and 167 % user, xnuhost
148-155 %, battery 40.3 C charging (1.6 A at the boot); in the guest backboardd 291-364 %, Spotlight 78-107 %,
SpringBoard about 70 %. Two new lines say where the heat goes at the next boot, and what decides each part:

- `thermal: 30 s: host CPU ...` (the bridge, every 30 s: the host's busy, sys, user, irq; swap in and out KiB/s;
  xnuhost's own CPU; the battery's temperature, status and current; case and SoC): `runtime/bridges/thermal/README.md`.
- `oneios-cpu-sys: 10 s: ...` after each `oneios-cpu:` line (the gpu archive, `OneiOSAdreno.cpp`): the same busiest
  processes' kernel time. **backboardd's 300 %:** its share in kernel time is its system calls, faults and IOKit calls,
  among them the framebuffer swap. A BGRA frame is presented from the surface's own pages (no copy in the guest; the
  copy into the display's buffer is xnuhost's device thread, `display: 10 s: flushes N, copy mean ...`, and the
  helper's into the Android buffer is the helper's); a half-float `RGhA` frame without `oneios-display-wide=1` and
  without the GOP presenter's GPU hand-off is converted to 8-bit sRGB by the CPU in the swap (`NativeLegacyDisplay`
  `convertHalfRows`, a full 1440x3120 frame each time), after waiting for the GPU's work in 100 us sleeps up to
  200 ms (`LegacyDisplayProbe.cpp` `waitForGpuWork`). Which one runs is the boot's `oneios-display: the compositor swaps
  format '...'` and `oneios-display: half-float frames: ...` lines. Mostly user time instead is Core Animation's and
  Metal's own work (metal2vulkan's translation, Turnip's command building): `oneios-ubt=backboardd` samples named
  with `oneios-tool sym` say which. Nothing is changed in the display path until those lines are read [the CPU
  conversion as a cause is a guess].
- **Spotlight:** the guest's `Spotlight` is the app (`/Applications/Spotlight.app`), the indexer jobs are
  `com.apple.searchd` and `com.apple.spotlight.IndexAgent` (24A446's launchd cache); `com.apple.spotlightknowledged` and
  `com.apple.hybridsearchd` are already disabled (`userland/config/daemons.toml` base). iOS runs its indexing as
  maintenance work, and plugged-in time is when its scheduler runs such work [Apple's documented behaviour for
  background tasks, not read in this build]. The least-risk test, no seed: one boot with
  `oneios-disable-jobs=com.apple.accessibility.axassetsd,com.apple.aonsensed,com.apple.prngseedd,com.apple.searchd,com.apple.spotlight.IndexAgent`
  (hosted 0053; keep the default list, axassetsd, aonsensed and prngseedd since 2026-11-22, in it). It writes the Data store's `disabled.plist`, so it stays for later
  boots until `oneios-enable-jobs=com.apple.analyticsd,com.apple.searchd,com.apple.spotlight.IndexAgent` (0044) turns
  them on again. Cost while off: search finds nothing new (apps, settings, mail, notes are not indexed). Not run.

### The phone in a session [F, the phone, 2026-11-12]

- Kept running: the kernel's 81 thermal zones (`step_wise`), LMh, `thermal-engine-v2` (empty
  `/vendor/etc/thermal-engine.conf`), `vendor.samsung.hardware.thermal-service`, the charger's own temperature limits.
  Gone with system_server: Samsung's case-temperature policy (SSRM) and the power HAL's boosts.
- The kernel's trips are junction trips: X4 90 C (`cpufreq-cpu7`), GPU 95 C (KGSL devfreq), DDR 90 C, the cores pause
  at 108-110 C, `xo-therm` 78/80 C, the BCL levels. They cap every Linux thread, xnuhost's too; nothing caps on the
  case temperature any more. xnuhost's thermal step does (inside the process only: thread affinities; no cpufreq,
  devfreq or `/sys/power` node is written, so a SIGKILL leaves nothing to restore).
- Sensors: `cpu-*`, `cpuss-*`, `gpuss-0..7`, `ddr`, `xo-therm` zones; `/sys/class/sec/sec-{ap,cf,lp,pa,wf}-thermistor`;
  `power_supply/battery/temp` and `lrp`. No `skin` zone. `lrp` is the case sensor Samsung's charger limits current by
  (device tree: 37-39 C with the screen on, 46 C `mix_v2_lrp_cond`).
- One UI's limits move: `policy7/scaling_max_freq` read 3187200, 3302400 and 3340800 kHz within a minute (hardware top
  3398400); the power HAL raised `scaling_min_freq` of all four policies to near their top in bursts.

### iOS's side [F 24A446]

thermalmonitord is product-tuned: `/System/Library/ThermalMonitor/V54AP-Info.plist` asks for an SMC
(`usesSMCSensorExchange`, group `zETM`), HID temperature sensors by name (`Tarc`, `NAND CH0 temp`, the camera dies) and
a die-temperature PID (`DieTempHS`, target 90); its battery sensor `TG0B-IOPMPowerSource` reads an `IOPMPowerSource`'s
`Temperature` (ours publishes it, section 7c). None of the SMC and PMU sensors exist here, so it reports nominal. The
pressure level iOS's frameworks read is the notify state `com.apple.system.thermalpressurelevel` (0, 10, 20, 30, 40,
50: `OSThermalNotification.h`); `oneios-thermal` sets it from the host's level. Not proven in a guest.

### Measured (thermalctl watch, every 10 s; the phone on USB, so "battery mA" is the net current, not the draw)

Bringup's sessions of 2026-11-12, read only from outside (runtime 0.1.7-dev without the thermal bridge; kernel.kc
9476a867):

| | One UI, screen on, between sessions | session 3, boot to lock screen (08:43-08:48) | session 4, lock screen after boot (08:52-08:56) |
|---|---|---|---|
| CPU zones (hottest) | 33-46 C | 55-82 C | 60-77 C (boot: 94.6 C) |
| GPU zones | 30-35 C | 46-56 C | 50-60 C |
| board (`sec-ap-thermistor`) | 29-35 C | 44-50 C | 48-55 C |
| case (`lrp`) | 29-33 C | 37-39 C | 40.7-43.2 C, still rising at 4 min |
| battery | 26-30 C | 33 C | 36-37 C |
| GPU clock | 231 MHz | 231-1000 MHz (1000 most of the time) | 231-1000 MHz |
| CPU in use (800 = all cores) | 30-240 | 360-530 | 300-500 |
| xnuhost / the whole session | - | 235-410 / 300-480 | 240-420 / 300-500 |

Where a still lock screen's CPU goes (`thermalctl top`, session 4, 10 s after the first frame): xnuhost 281%, all of it
the eight vCPU threads (`xnu-cpu0` 55% ... `xnu-cpu7` 6%), the iOS processes 80% (two at 26% and 23%). The vCPU
threads run the guest kernel, so the kernel, not iOS's user space, keeps the phone hot. Session 3's stop line: cpu2
emulated 104 million trapped instructions in 400 s (260,000 a second; cpu3 62 M, cpu4 37 M, the E vCPUs about 1 M),
each a signal round trip on its thread.

**The cause, found in a session of mine (09:19, runtime 0.1.7-thermal, kernel.kc 9476a867):** xnuhost's new count
(SIGQUIT, `emulated: insn ...`) had `0xd5087e42` (`dc cisw, x2`, clean and invalidate by set/way) 115 million times in
about 3 minutes, with CLIDR, CSSELR and CCSIDR accesses 112,280 times each. That is XNU's `cpu_idle` ->
`platform_cache_idle_enter()` -> `FlushPoU_Dcache` (`osfmk/arm/caches.c`, `osfmk/arm64/caches_asm.s`
`DCACHE_SET_WAY`: this board is not `APPLE_ARM64_ARCH_FAMILY`): a set/way flush of L1 at every idle entry, 1024 trapped
`dc` each (256 sets x 4 ways of the geometry xnuhost's CCSIDR gives), about 600 idle entries a second. On a host process
the flush does nothing. xnuhost now ends that loop after its first trap (`setway-skip`, on by default: the operand
becomes the level's last set and way; `docs/booter.md` 2.5). The fix in the kernel (`platform_cache_idle_enter` empty
under `XNU_HOSTED`) is asked of hosted-xnu on the board; it was not built because Mac-built `kernel.kc`s panic on the
phone today.

My two sessions, one after the other, lock screen untouched, the same kernel and runtime; B with `setway-skip=1` in the
image's `xnuhost.conf` (logs in `out/phone-baseline/2026-11-12/thermal/`):

| Lock screen, after the first frame | One UI, screen off (09:13-09:18) | A: one trap per line (09:22-09:24) | B: setway-skip (09:31-09:35) |
|---|---|---|---|
| CPU in use, the phone (800 = all cores) | 40 | 480 | 334 |
| xnuhost / the whole session | - | 334 / 409 | 197 / 264 |
| vCPU threads (`thermalctl top`, 30 s) | - | 310% (`xnu-cpu2` 62, `cpu3` 54, `cpu0` 54, `cpu1` 54, `cpu4` 40, `cpu5` 24, `cpu6` 14, `cpu7` 8) | 233% (`cpu0` 56, `cpu1` 56, `cpu2` 38, `cpu3` 32, `cpu4` 22, `cpu5` 14, `cpu6` 9, `cpu7` 5) |
| `dc cisw` traps per flush | - | 1024 | 1 (192,940 flushes in 4 min) |
| CPU zones (mean) | 38.3 C | 50.1 C | 44.4 C |
| GPU zones | 36.2 C | 42.6 C | 38.9 C |
| board thermistor | 35.5 C | 40.5 C | 37.8 C |
| case (`lrp`) | 34.3 C (cooling from bringup's session) | 35.3 C | 34.4 C |
| GPU clock (mean of samples) | 231 MHz | 441 MHz | 417 MHz |
| battery current | about 0 mA: on USB and full, so not the phone's draw | same | same |

The sessions were 5 and 7 minutes (the coordinator's rule: short sessions, each ended with `return.request`), so the
case had not reached its level: compare the CPU lines, not the case. Not measured: 15 minutes, a scroll or animation
load (nothing can touch the panel from the Mac), the drain off USB.

What is left in B: the E vCPUs (`cpu0`, `cpu1`, 56% each) run most of iOS's system calls (1.5 million task traps each
in session 3 against 0.3 million on a P vCPU); interrupts are about 2,900 a second (`ICC_IAR1_EL1` 688,826 in 4 min);
each idle entry still traps 6 times (CLIDR twice, CSSELR, CCSIDR twice, the one `dc`). The kernel fix removes those too.

### One UI's limits stay in a session

One UI (Samsung's SSRM in system_server, and the HALs it drives) caps the clocks when the phone is warm: at 09:18
`policy0..7/scaling_max_freq` read 1689600, 2035200, 2035200, 2035200 kHz (the hardware's 2265600, 3148800, 2956800,
3398400) and `/sys/kernel/gpu/gpu_max_clock` 500 MHz (owner `system`, written at 08:58, after a return). KGSL shows that
cap as `thermal_pwrlevel` 9 (`max_gpuclk` 500000000; its devfreq and `gpu` cooling devices were at state 0, so the
kernel's thermal zones did not set it). The switch stops system_server, so whatever cap One UI last set stays for the
whole session (sessions A and B ran with these; bringup's sessions 3 and 4, after a cool One UI, had 3340800 and 1000
MHz). A session that starts after a warm One UI is slowed for its whole length, and one that starts cool has no case
policy but ours. **The OneOS fork now saves and lifts them** (branch `oneios`, `oneios_switch.sh` `clocks_save` /
`clocks_tops`, `oneios_return.sh`; the fork's `docs/oneios.md`, "Session flow"): at the baseline it saves Samsung's
`/sys/devices/system/cpu/cpufreq_limit/cpufreq_max_limit` and `cpufreq_min_limit`, `scaling_min_freq` and `scaling_max_freq` of each policy, `/sys/kernel/gpu/gpu_max_clock` and KGSL's `max_gpuclk`
(`control/base/clocks`); once system_server is gone it writes the hardware's range (`cpuinfo_min_freq`,
`cpuinfo_max_freq`, the top of the GPU tables) and logs each node `old -> new` in `switch.log`; the return writes One
UI's values back before Gate A and zygote, on every way back but the reboot (a SIGKILL of xnuhost does not skip the
return), and logs them in `return.log`. The kernel's thermal zones, trips and cooling devices and `/sys/power` are not
written: the kernel's junction trips, LMh and xnuhost's thermal step are the session's caps. In the opt-in Android takeover
(`session-mode=takeover`, [oneos/interface.md](../oneos/interface.md) 4a) the fork adds one of its own: its CPU cap lowers
each policy's `scaling_max_freq` from the case at 46 C (80 %, 60 % at 48 C; a return at 50 C), and the same return
puts One UI's values back. Tested against a fake
sysfs (`OneiosScriptTest`). The first phone run (prelink's session of 10:28) lifted the GPU (`gpu_max_clock` 500 -> 1000,
`thermal_pwrlevel` 0) but not the CPUs: `scaling_max_freq` still read 1689600/2035200 after the write. The holder is
Samsung's `cpufreq_limit` driver (`drivers/cpufreq/cpufreq_limit.c` of the SM8650 kernel, read at 809f7ed): SSRM writes
one value to `cpufreq_max_limit` and the driver keeps a freq QoS max request per cluster of its own, which a write of
`scaling_max_freq` cannot lift; `-1` releases it. All cooling devices were at 0, `msm_performance` `cpu_max_freq`
unlimited and WALT's `sched_fmax_cap` only on cluster 0 (1804800). The fork now releases `cpufreq_limit` first and puts
it back last (fork 2nd commit; tested against a fake sysfs, not yet on the phone). xnuhost itself writes none of them.

### Missing, unknown

- Run on the phone: the thermal bridge in xnuhost (`thermal: on: 29 zones; case 32.7 C (lrp), SoC 48.1 C (cpu-1-2-0),
  battery 30.8 C; nominal`); the level stayed nominal (case below 39 C), so no step was taken. Not run: the step, the
  driver (it needs a new `kernel.kc`), the helper (the helpers are in no image yet: `docs/userland.md` 7).
- [U] Whether the "heavy" and "critical" levels change what iOS does (frame rate, the cool-down screen).
- [U] The case thresholds are a guess on Samsung's scale, not Samsung's policy.
- The GPU: drm2kgsl's burst rule (`docs/gpu.md` 2.2) and its constraint are the gpu agent's; at heavy pressure they
  could stop constraining (a note on the board). The KGSL constraint stays under the thermal devfreq limit [inference].

## 7e. Hardware backends: samsung (the default) and treble (hw-backend, 2026-11-15)

The owner's decision of 2026-11-15: the Samsung paths stay the default and keep working exactly as on the S24 Ultra;
a second, vendor-neutral backend, `treble`, sits beside them so that the image can later run on other Android phones
(other Samsung phones first). Device profiles (the fork's) come later: `oneos/interface.md`, "The hardware backend".

**The settings** (`runtime/bridges/hw/hw_backend.rs`; xnuhost's `runtime/xnuhost/src/config.rs`; `docs/booter.md` 1):

| Setting | Values | What |
|---|---|---|
| `hw-backend` | `samsung` (default), `treble` | every bridge |
| `hw-backend-<bridge>` | `samsung`, `treble` | one bridge, over `hw-backend`: `backlight`, `battery`, `thermal`, `haptics`, `touch`, `keys`, `audio`, `camera`, `wifi`, `bluetooth`, `display` (for tests, or a phone whose one part is Samsung's) |
| `hw-backend-fallback` | `0` (default), `1` | a treble bridge whose interface is missing uses its samsung form (1) or stays off (0) |

Under them since 2026-11-21: the host phone's table entry (section 7h) may hold `hw-backend*` defaults; none
does yet, so every known phone is samsung everywhere unless a setting says otherwise.
In xnuhost.conf (the image set's or the overlay's), with `--set KEY=VALUE`, or as `--hw-backend treble`,
`--hw-backend-battery treble`; the command line wins per setting. The ctl tools (`brightctl`, `batteryctl`,
`thermalctl`, `bridgectl`) read `ONEIOS_HW_BACKEND`, `ONEIOS_HW_BACKEND_<BRIDGE>` and `ONEIOS_HW_FALLBACK`.

**The log** names each bridge's backend and why, one line each: `hw: battery: backend treble (hw-backend=treble)`,
`hw: thermal: backend samsung (the default)`, `hw: touch: backend samsung (the default); keys: backend samsung (the
default): the same lookup in both ...`. A treble interface that is missing is one line:
`hw: battery: treble: missing: android.hardware.health.IHealth/default (no service: ...); the bridge stays off
(hw-backend-fallback=0)` or `...; falling back to samsung (hw-backend-fallback=1)`.

**The way back applies to both.** No treble path grabs an input device (the side keys are found by their capabilities
and read only), starts a realtime thread, leaves xnuhost's cgroup, writes under `/sys/power` or restarts anything.
Treble lookups never walk a directory of sysfs or /proc: a HAL is asked for by name from servicemanager
(`IServiceManager.getService`, handle 0 of `/dev/binder`), a sysfs class device by a named path, an evdev device as
`/dev/input/eventN` for N = 0..31 probed by its ioctl capabilities. The binder client is the Wi-Fi supplicant's
(`runtime/bridges/hal/binder.rs`, `aidl.rs`): no Android library.

### The audit table

Every hardware path of xnuhost, the bridges, the phone tools and the fork's session scripts (read only here), its
Samsung form (today's, the default) and its Treble form. Status: **done** (both backends in code, host-tested and
tested on the stand-in phone), **same** (one form, already vendor-neutral: both backends use it), **next** (not yet;
who or what it waits for).

| Path | Where | Samsung form (default) | Treble form | How treble finds it (no walk) | Status |
|---|---|---|---|---|---|
| Backend choice | `bridges/hw/`, `xnuhost/src/config.rs` | - | - | settings above | **done** (this PR) |
| Panel backlight | `bridges/brightness/` | `/sys/class/backlight/panel`, then `panel0-backlight`, then the class folder listed; Samsung's doubled ranges (510, 51000) halved | the standard backlight class: the first of `panel0-backlight`, `panel1-backlight`, `backlight`, `lcd-backlight`, `panel` with a `max_brightness`; the whole range (`ONEIOS_BRIGHT_MAX` caps it) | named paths only | **done** (this PR: test_brightness, hw-backend.sh) |
| Touch IC power | `bridges/brightness/` | `/sys/class/sec/tsp*/enabled` whose `input/name` is `sec_touchscreen` (folder listed), `2,0`/`2,1` and `1,0`/`1,1`; with xnuhost's pen (`ONEIOS_PEN_IC=1`) also the S Pen's `/sys/class/sec/sec_epen/enabled` in the same writes | none: no vendor-neutral node; the panel driver's own notifier handles it [I]; one line, left alone | - | **done** (treble: nothing written) |
| Ambient light | `bridges/brightness/` | evdev `als_rear` by name (`/dev/input` listed), its `/sys/class/input/eventN/device/enable` | the sensors HAL (AIDL `android.hardware.sensors.ISensors/default`): events come through an FMQ, not written yet; missing line, `hw-backend-fallback=1` uses the samsung lookup | - | next |
| Battery | `bridges/battery/` | `/sys/class/power_supply/battery/*`, `usb,ac,wireless,pogo/online`; 5000 mAh default | the health HAL: `android.hardware.health.IHealth/default`, `getHealthInfo()` | servicemanager by name | **done** (`battery::tests`, `tests/hal_binder.rs`, hw-backend.sh) |
| Thermal sensors | `bridges/thermal/` (`Sensors::find`, `Sensors::read`) | `/sys/class/thermal/thermal_zone*` (folder listed; types `cpu-*`, `cpuss-*`, `gpuss-*`, `ddr`, `xo-therm`), `/sys/class/sec/sec-ap-thermistor/temperature`, `power_supply/battery/{temp,lrp}` | the thermal HAL: `android.hardware.thermal.IThermal/default`, `getTemperatures()`: CPU, GPU, BATTERY, SKIN | servicemanager by name | **done** (test_thermal, test_hal_binder, hw-backend.sh) |
| Thermal load line | `bridges/thermal/load.rs` (`HostLoad::read`) | `/proc/stat`, `/proc/vmstat`, `/proc/self/stat`, `power_supply/battery/{current_now,status}` | the same (standard Linux, named files) | named files | same |
| Thermal policy, CPU step | `thermal/` (`thermal::decide`), `xnuhost/src/session/bridges.rs` | Samsung's scale (lrp) | not touched here (fix-ram-locked's) | - | same |
| Vibrator | `bridges/haptics/` | evdev force feedback: `/dev/input` listed for FF_PERIODIC + FF_SINE, `sec_vibrator_inputff` first | the vibrator HAL: `android.hardware.vibrator.IVibrator/default`, `setAmplitude`, `on(ms)`, `off()` | servicemanager by name | **done** (`tests/hal_binder.rs`, hw-backend.sh) |
| Touch screen | `xnuhost/src/input.rs` | `/dev/input/event0..31`: multitouch X/Y with `INPUT_PROP_DIRECT`, the first one | the same | eventN by ioctl capabilities | same (spen owns the lookup function) |
| Side keys | `xnuhost/src/input.rs` | `/dev/input/event0..31` with `KEY_POWER`, `KEY_VOLUMEUP` or `KEY_VOLUMEDOWN`, read only, never grabbed | the same | eventN by ioctl capabilities | same |
| Power key -> touch IC | `xnuhost/src/session/bridges.rs`, `Bright::key` | wakes the `sec_touchscreen` IC | nothing to wake (no touch IC node in treble) | - | **done** (`Bright::key` wakes nothing in treble) |
| CPU topology, placement | `bridges/cpu/topology.rs` | `/sys/devices/system/cpu/...` (standard) | the same | named files | same (fix-host-overhead's) |
| Audio | `bridges/audio/aaudio_helper.rs` | AAudio (NDK, `libaaudio.so`) | the same | - | same |
| Camera | `bridges/camera/backend_hal.rs`, `backend_ndk.rs` | AIDL `android.hardware.camera.provider.ICameraProvider/internal/0`; Camera2 NDK | the same; another vendor may name its provider otherwise [I]: from servicemanager's list (`listServices`) | servicemanager | same; the instance name next |
| Wi-Fi | `bridges/wifi/` | `wlan0` (AF_PACKET, nl80211), AIDL `android.hardware.wifi.supplicant.ISupplicant/default` | the same interfaces; the interface's name from nl80211 instead of `wlan0` | nl80211 | next (after fix-settings-wifi-cpu merges) |
| Bluetooth | `bridges/bt/hci_hidl.rs` | HIDL `android.hardware.bluetooth@1.1::IBluetoothHci/default` on `/dev/hwbinder` | AIDL `android.hardware.bluetooth.IBluetoothHci/default` (Android 13+), HIDL as before | servicemanager, hwservicemanager | next (after fix-bt-virtual merges) |
| Display (drm mode) | `xnuhost/src/display.rs` | `/dev/dri/card0` | the DRM card found at run time: `/dev/dri/card0..7` by `DRM_IOCTL_VERSION` and a connected connector | eventN-style probe of named nodes | next (after fix-boot-black merges) |
| Display (shm mode) | `runtime/display` (the helper) | SurfaceControl (Java, app_process) | the same | - | same |
| GPU 3D | `xnuhost/src/gpu3d_kgsl.rs`, drm2kgsl | KGSL (`/dev/kgsl-3d0`), `/dev/dma_heap/system`, `/dev/udmabuf` | KGSL when present (Qualcomm), else 2D | named nodes | next (Qualcomm only today) |
| RAM fix, KGSL memory | `xnuhost/src/ramfix.rs` | `/sys/class/kgsl/kgsl/proc/PID`, the session cgroup | the same on Qualcomm | named files | same (fix-ram-locked's) |
| Phone tool: backlight, touch | `oneios-tool phone backlight`, `phone touch` | `/sys/class/backlight/panel0-backlight`, `/sys/class/sec/tsp/enabled` | the backlight names above; no touch IC | named paths | next (the tool) |
| Session scripts (the fork's; read only) | `switcher/assets/oneios_switch.sh`, `oneios_return.sh`, `oneios_root.sh` | `/sys/class/backlight/*/brightness` (glob), `/sys/kernel/gpu/gpu_max_clock` and `/sys/devices/system/cpu/cpufreq_limit/*` (Samsung's), `/sys/class/kgsl/kgsl-3d0/max_gpuclk`, `/sys/class/power_supply/usb/online`, `/sys/class/udc/*/state`, watchdogs, `/sys/class/input/event*/device/enable` | cpufreq `policy*/scaling_max_freq` (standard), the KGSL devfreq, the backlight names above, `/sys/class/udc` | the fork's device profiles | next (the fork) |

### What each treble form does

The sections below say, per bridge, what the treble form reads or calls, how it maps to what the bridge gives iOS, and
how it was tested. Until a bridge's step is done its row above says next.

**Backlight** (`runtime/bridges/brightness`, README "The two backends"). Treble: the backlight class device by its
standard names, `/sys/class/backlight/{panel0-backlight,panel1-backlight,backlight,lcd-backlight,panel}/max_brightness`
read in that order (named files), the whole range, One UI's value back at the close; the touch IC's power is left
alone and the light sensor is off (one line each). Tested: `brightness/tests.rs` `treble` (C: `test_brightness.c`; both backends on one fake sysfs:
samsung takes `panel` and halves 51000, treble takes `panel0-backlight` whole and never writes `tsp/enabled`; a
vendor's own name: treble's one missing line and None, samsung with the fallback; 105 checks in the C), the stand-in phone
(`tests/phone/hw-backend.sh`: an S24, another phone, an odd one; 9 checks). On the phone the default proves itself by
`hw: backlight: backend samsung (the default)` followed by the same `brightness: panel on: ...` and `brightness:
touch: 1 IC ...` lines as before; a treble boot (`--hw-backend-backlight treble`) by `brightness: panel on:
/sys/class/backlight/panel0-backlight/brightness, 1..N of N` and the touch line `treble: no vendor-neutral power node`.

**The HAL client** (`runtime/bridges/hal/`): `Hal::open` opens `/dev/binder` (the binder layer the Wi-Fi supplicant's client uses,
`hal/binder.rs`, `hal/aidl.rs`), asks servicemanager for the instance by name (`getService`; no wait, no list walked), checks
the object's descriptor (`_NTF`) and reads its version; then plain calls. Each treble bridge has its own connection (one
binder looper thread each, SCHED_OTHER, idle: no HAL calls back into it); samsung opens none. A HAL that is not registered is the bridge's
one missing line. The method codes are the order of AOSP's frozen AIDL files (stable AIDL only appends), [I] until a
phone answers. The tests' fake servicemanager and HALs: `hal/fake.rs` (the bin `fake-hal DIR`, `cargo run -p onebridges --bin fake-hal`, values read
from files at every call), used by `tests/hal_binder.rs` and the stand-in phone.

**Battery** (`runtime/bridges/battery`, README "The two backends"). Treble: the health HAL,
`android.hardware.health.IHealth/default`, one `getHealthInfo()` per read; `HealthInfo` becomes what the nodes would
have said and the same mapping follows. Read only (`update()` and callbacks never called). Tested: `battery/tests.rs` `health` (C: `test_battery.c`)
(the parcel both ways, an older HAL's shorter one, a bad one, the mapping; 83 checks in the C, the 65 samsung ones unchanged),
`tests/hal_binder.rs` (C: `test_hal_binder.c`; against the fake on a real binder driver: a read, a change, the HAL's death, not registered, the
fallback), the stand-in phone (6 checks: `batteryctl` on the S24's nodes and through the fake health HAL, a phone
with only the HAL). On the phone: `hw: battery: backend samsung (the default)` then the same `battery: on:
/sys/class/power_supply, ...` line; a treble boot by `battery: on: the health HAL
android.hardware.health.IHealth/default (version N), ...` and the same `ONEIOS BATTERY` line in iOS's log.

**Thermal sensors** (`runtime/bridges/thermal`, README "The two backends of the sensors"). Treble: the thermal HAL,
`android.hardware.thermal.IThermal/default`, one `getTemperatures()` per reading: cpu, gpu, soc (CPU, GPU, SOC), the
battery, and the case from the HAL's SKIN sensor (samsung: Samsung's `lrp`). The policy is untouched and the same for
both: a SKIN of 41.2 C gives the level `lrp` 41.2 C gives. Tested: `thermal/tests.rs` (C: `test_thermal.c`, 81 checks, the 72 samsung ones
unchanged), `tests/hal_binder.rs` (a reading, a hotter one to HEAVY, the HAL's death, not registered, no
temperature), the stand-in phone (4 checks: `thermalctl` on the S24's zones and through the fake HAL, another
phone's CPU/SOC/skin). On the phone: `hw: thermal: backend samsung (the default)` then the same `thermal: on: N zones;
case ... (lrp)` line; a treble boot by `thermal: on: the thermal HAL android.hardware.thermal.IThermal/default
(version N), K temperatures; case ... (SKIN)`.

**Vibrator** (`runtime/bridges/haptics/haptic.rs`, `vibrator.rs`). Treble: the vibrator HAL,
`android.hardware.vibrator.IVibrator/default`, behind the same worker, mapping and counters: an effect is `on(length)`
(no callback) then, when `getCapabilities()` has `CAP_AMPLITUDE_CONTROL`, `setAmplitude(level / 255 x gain)` (AOSP's
vibrator service orders them so); a stop and the close are `off()` (the close always sends one: the motor is stopped
whatever the worker believed). The period has no place in `on()`: the HAL plays at its own resonance. Not registered:
one missing line and no actuator for iOS, or the evdev search with `hw-backend-fallback=1`. Tested: `tests/hal_binder.rs`
(on and amplitude for two intensities, a stop, the close, no amplitude control, not registered, the fallback), the
stand-in phone (3 checks: `bridgectl haptic 40` with the fake HAL's call log). The samsung tests (`haptics/test_haptic.rs`, C: `test_bridges.c`)
unchanged. On the phone: `hw: haptics: backend samsung (the default)` then the same `haptics: on: /dev/input/eventN
...` line; a treble boot by `haptics: on: the vibrator HAL android.hardware.vibrator.IVibrator/default (version N,
capabilities 0x...)` and `haptics: effect N played on the vibrator HAL`.

### Tested (2026-11-15, the hw-backend branch, the C tree) and what is next

- `runtime/host-tests.sh`: all pass (loader, xnuhost incl. the config checks, display, bridges incl. test-binder,
  camera, links, package). One run on a box at load 17 failed one timing check of the virtio-gpu lanes
  (`test_xnuhost.c` 951, a 2 s wait; nothing of this branch), which passed in two runs alone and in `make -C
  runtime/xnuhost test` (13883 checks, the test kernel booted). The bridges: `hw_backend` 55, brightness 105, battery 83, thermal 81, `hal binder` 37,
  bridges 295, wifi 162 checks, 0 failures; the samsung checks that were there before are unchanged.
- The stand-in suites (`tests/phone/modes.sh`, both `/proc` modes): phone 222 (22 of them `hw-backend.sh`), app 65,
  cycle 103, all ok. The cycle suite needs a short run folder in a worktree with a long path (`MODES_RUN=/tmp/...`):
  its tmux socket path is longer than a Unix socket allows otherwise (`File name too long`), a fault of the suite
  (main a591c08), not of this branch.
- Not run on a phone (no phone on the build box). With nothing set the phone is samsung everywhere; the only new
  lines are the `hw: <bridge>: backend samsung (the default)` ones.
- Next: the light sensor's treble form (the sensors HAL: events through an FMQ); Wi-Fi (the interface's name from
  nl80211), Bluetooth (the AIDL `IBluetoothHci`) and the display (the DRM card found at run time) after
  fix-settings-wifi-cpu, fix-bt-virtual and fix-boot-black merge; the camera provider's instance from
  servicemanager's list; `oneios-tool phone backlight` with the treble names; the fork's device profiles
  (`oneos/interface.md` 6b). The first phone boot with `--hw-backend treble` checks the HALs' codes ([I] above).

## 7f. S Pen (spen, 2026-11-15; off by default)

The S24 Ultra's S Pen (a Wacom EMR digitizer under the panel; writing needs no battery and no Bluetooth link) as an
Apple Pencil in iOS: hover, contact, pressure, tilt, the side button. **Off by default** until a phone session shows it:
xnuhost `pen=auto` (`--set pen=auto`, or `pen=auto` in `xnuhost.conf`), which also gives the guest `oneios-pen=1`.
The phone's image has had `pen=auto` in `overlay/userland/xnuhost.conf` since 2026-11-16, but xnuhost dropped the image
set's `pen=`, `pen-rotate=` and `pen-mirror=` until 2026-11-22 (then `config.c`, now `config.rs`; test in `config/tests.rs`), so the pen
had never run on the phone before that fix.

### The phone

- [F] The pen's input device is `sec_e-pen` (event11 on the owner's phone on 2026-11-10 and in the session of
  2026-11-22; numbers move between boots, so xnuhost goes by bits and name). Read in the first session with `pen=auto`
  (2026-11-22, runtime 0.1.15 and the parent's config merge fix): `x 0..7229, y 0..15662, pressure 0..4095, distance
  0..255, tilt x -63..63, tilt y -63..63; keys: tool pen, rubber, touch, stylus`, no resolution on any axis.
- [F] Samsung's driver (read in its source, 2026-11-22: the S24 Ultra kernel tree `Elchanz3/android_kernel_samsung_pineapple_e3q`
  at 34d7e2d, `drivers/input/sec_input/wacom/wacom_core.c`; the same functions in `samsung-sm8650/android_kernel_samsung_sm8650`
  lineage-22.1; the device tree `samsung-sm8650/android_kernel_samsung_sm8650-devicetrees` 2f6a973,
  `samsung/e3q_eur_openx_w00_r09.dts`, node `wacom@56`, `wacom,wez02`):
  - one input device, `sec_e-pen`: `BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`, `BTN_STYLUS` (no `BTN_STYLUS2`),
    `SW_PEN_INSERT` (1 with the pen **out** of its garage), and for its screen-off (AOP) mode `KEY_WAKEUP_UNLOCK` and
    `KEY_HOMEPAGE`; **no `INPUT_PROP_DIRECT`**; `ABS_X/Y` 0..the IC's maximum (fuzz 4), `ABS_PRESSURE` 0..4095,
    `ABS_DISTANCE` 0..255, `ABS_TILT_X/Y` -63..63, no `input_abs_set_res`. The maximums come from the IC's query at probe
    (the device tree's `sec,max_coords` 7315 x 15687, `wacom,max_tilt` 63, `wacom,max_height` 255 otherwise).
  - **no coordinate transform**: `wacom_coord_handler` reports the IC's own x and y (no `xy_switch`, no invert in the
    driver or in the device tree). The IC's frame has the short side on x, the panel's shape (7229 / 15662 = 0.462, the
    panel 1440 / 3120 = 0.462), so `pen-rotate=auto` gives 0 [F]; that the IC's origin is the panel's top left is [I]
    (the first Notes stroke from the top left corner checks it).
  - a frame: X, Y, `BTN_STYLUS`, `BTN_TOUCH`, pressure, distance (in every frame, also while touching), tilt, the tool
    key, `SYN`. The first frame of a proximity is dropped by the driver. The leave is two frames: pressure 0, `BTN_STYLUS`
    0, `BTN_TOUCH` 0, `SYN`; then distance 0, the tool keys 0, `SYN`. Whether a larger distance is farther is not in the
    source [U]: the `pen: in range` line's `height a..b/255 hovering` shows it.
  - **power**: probe ends in `wacom_enable`; after that only a userspace write changes it. `sec,enable_sysinput_enabled`
    is set, so `/sys/class/sec/sec_epen/enabled` takes `<display state>,<event>` as the touch IC's does: `2,1` runs
    `wacom_enable`, `1,0` `wacom_disable`, which leaves the IC in its garage / AOP survey mode: it sends no positions,
    only `KEY_WAKEUP_UNLOCK`, `KEY_HOMEPAGE` or a tap. One UI's input HAL writes it at panel power changes [I, as for the
    touch IC], so after the switch's blank it is asleep for iOS [I]. **Fix (2026-11-22):** with the host's pen xnuhost
    sets `ONEIOS_PEN_IC=1` and the brightness bridge writes the pen's node with the touch IC's: `2,0`, `2,1` at its open,
    at iOS's display on, at a power-key press and at the close; `1,0`, `1,1` at iOS's display off
    (`runtime/bridges/brightness/README.md`). By hand: `oneios-tool phone pen status`, `phone pen wake`.
- [I] No `EVIOCGRAB` is needed: in a session zygote and system_server are gone, so nothing else reads the device (as for
  the touch screen, `oneos/interface.md` 1); evdev gives every reader every event, and an ungrabbed device is One UI's
  again at the return as it was. xnuhost never grabs it. `input.rs` never takes it either (no side key, no multitouch), so
  the pen IC's `KEY_HOMEPAGE` never presses Home; `pen.rs` drops those keys and logs the first (`pen: key N from the pen's
  IC, not a pen key: its screen-off (AOP) mode ...`).
- [U] Palm rejection: the touch IC reports a resting hand as fingers; iOS's own handling of fingers while a pencil is in
  range is all there is (the two are separate digitizer services in iOS; nothing of ours drops fingers).

### What iOS needs (read in 24A446's shared cache on the Linux PC with `out/bin/ipsw`, 2026-11-15)

| Need | Where | Code read [F] or guess |
|---|---|---|
| A service that is Built-In with primary usage page / usage 0x0d / 0x20 (Digitizer / Stylus) is backboardd's "pencil digitizer" (`*** acquired pencil digitizer`); 0x0d/0x04 is the main display's, 0x0d/0x01 the AOP's; a second pencil service is refused (`*** already have a pencil digitizer`) | BackBoardHIDTouchEventProcessor, `__59-[BKHIDDirectTouchEventProcessor matcher:servicesDidMatch:]_block_invoke` (`isBuiltIn`, then `primaryUsagePage << 16 \| primaryUsage` against 0xd0001 / 0xd0004 / 0xd0020) | [F] |
| A touch is `UITouchTypePencil` (type 2) when its digitizer event's transducer type (field 0xb0004) is 0 (`kIOHIDDigitizerTransducerTypeStylus`); otherwise 1 (indirect) or 0 (direct, by DisplayIntegrated). No idiom check, no MobileGestalt answer, no pairing state on that path | UIKitCore `_UITouchSetBasicTouchPropertiesFromEvent` (`-[UITouch _setType:]`, ivar +0x160 = `-[UITouch type]`); `_UIEventHIDGetChildStylusEvent` | [F] |
| Hover: the feature flag PencilAndPaper/Hummingbird on, a stylus child whose Z (0xb0002) is not 0, and a maximum Z in the event's digitizer attributes | UIKitCore `_UIEventHIDShouldAllowHoverEventGeneration`, `_UIEventHIDPencilMaximumPositionZForEvent` (`BKSHIDEventGetDigitizerAttributes`); `/System/Library/FeatureFlags/Domain/PencilAndPaper.plist` in the image: Hummingbird Enabled true | [F] |
| The maximum Z and the maximum force backboardd puts in those attributes come from the service's `MaxHoverHeight` and `AccurateMaxDigitizerPressureValue` | both strings are in 24A446's BackBoardHIDTouchEventProcessor [F]; how they are read: UniXNU-droidvm's reading of iPadOS 27's backboardd (`ipados27/scripts/build-droidvm-hid-input.py`) | [I] for iPhone |
| A pencil's force is the event's pressure / 120 (an Apple Pencil's 0..4.17 for 0..500 under a maximum of 500); PencilKit widens its ink by that force | droidvm's reading (`-[UITouch _unclampedForce]`, PencilKit `InputPoint`) | [I] |
| Altitude and azimuth: UIKit reads the stylus event's polar orientation (`kIOHIDEventFieldDigitizerAltitude` 0xb000f, `Azimuth` 0xb0010) | droidvm's reading of `-[UITouch _updateWithChildEvent:]` | [I] |
| Apple's IOHIDEventDriver makes a stylus transducer of a Stylus collection; takes In Range, Tip Switch, Barrel Switch (button 1), Eraser (button 2), Tip Pressure, Z; **drops X and Y Tilt** and parses no azimuth or altitude | IOHIDFamily-2238.120.5 (the profile's pin) `IOHIDEventDriver.cpp` | [F] source at the pin |
| `IOHIDEvent::setFixedValue` is slot 26 (after getFixedValue's 25), discriminator 0x6e23 | `IOHIDEvent.h` at the pin; `__builtin_ptrauth_string_discriminator("_ZN10IOHIDEvent13setFixedValueEjij")` = 0x6e23, as the same builtin gives NativeHidInput's known 0xf637 for getFixedValue; and 24A446's kernelcache itself (`ipsw kernel cpp out/kernel-inputs/24A446/kernelcache.macho -c IOHIDEvent --methods`): slot 25 pac 0xf637, slot 26 pac 0x6e23, 37 slots | [F] |
| The event's IOHIDDigitizerEventData is 0xa0 bytes with orientationType at +0x58 (1 = polar) | droidvm (HID.framework `+polarDigitizerEvent:`); the wrapper checks size and type before it writes and counts a mismatch | [I], checked at run time |
| MobileGestalt `DeviceSupportsApplePencil`: not read by UIKitCore (no such string there); PencilKit, NotesSupport, Preferences, AnnotationKit read it (UI and settings). libMobileGestalt has a `supports-apple-pencil` string beside the `/product` `supports-*` names, and 24A446's iPhone device tree has no `supports-apple-pencil` | strings | [F] strings; [I] that the answer is that `/product` property and so false here. Not set by OneiOS: strokes do not need it; what Notes or PencilKit show differently with it is [U] |
| The side button: UIKit's pencil barrel events (double tap, squeeze) are generic-gesture HID events (type 0x27) of the pencil's own service, not the digitizer's button mask | UIKitCore `_UIEventHIDIsPencilBarrelEvent` | [F]; so the S Pen's button reaches iOS as the digitizer's Barrel Switch, which no iOS code is known to act on [U] |
| Pairing (PencilPairingUI, `com.apple.pencil.pairing.app`) is a Bluetooth Apple Pencil's; not needed for the digitizer's touches | | [I] |

### The path

1. **Host** (`runtime/xnuhost/src/pen.rs`; `docs/booter.md` 2.8 "The pen"). `pen::lookup` (one function, replaceable by
   another hardware backend) finds the device by its bits (`BTN_TOOL_PEN`, `ABS_X/Y`, `ABS_PRESSURE`; ranked up for
   `ABS_TILT_X`, `BTN_STYLUS`, `ABS_DISTANCE` and a pen's name; never a device with a side key or multitouch) and opens
   it read only. The filter turns each frame into one sample in fixed units (x, y 0..32767 turned to the panel; pressure
   0..4095 while the tip is down; height 1..255 while hovering; tilt in degrees; tip, side button, eraser end), sends
   only what changed, skips a frame like the last, and reads the state again after `SYN_DROPPED`. It sends at most one frame per
   8.33 ms (the panel's 120 Hz) but the edges, which go at once (see "Lag" below).
2. **The channel**: a second virtio-input device, `/arm-io/input-1` ("OneiOS pen"), with those fixed ranges, read on
   `input.rs`'s thread. The touch screen's device is unchanged.
3. **Guest transport**: the staged `DroidVMVirtioInput.c` (UniXNU-droidvm's code, unchanged: `KIND_PEN`) takes a device
   with `INPUT_PROP_DIRECT`, `BTN_TOOL_PEN` and `ABS_X/Y` as a pen and sends each frame to the pen sink: flags, x, y,
   pressure, distance, tilt, and the altitude and azimuth it computes from the tilt.
4. **Guest HID device** (`kernel/patches/oneios-hosted/hid`, `staged/hid_pen.rs`; with `oneios-pen=1`): "OneiOS pen", a
   third NativeHidInput, Transport SPI, report 4 in a Digitizer / Stylus collection (Tip Switch, Barrel Switch, In Range,
   Eraser, X, Y 0..32767, Z 0..255, Tip Pressure 0..4095, X/Y Tilt -90..90), Built-In, DisplayIntegrated,
   DigitizerCollectionDispatch, MaxHoverHeight 1, AccurateMaxDigitizerPressureValue 500. The SPI / Digitizer
   personality that takes the touch screen takes it too (IOHIDEventDriver at once).
5. **The event driver's dispatch** (NativeHidInput's edge wrapper, now on the pen's driver too) completes each stylus
   event: polar orientation, altitude, azimuth, pressure 0..500, Z 0..1. OneiOSHidInput.cpp publishes Built-In,
   DisplayIntegrated, PrimaryUsagePage 0x0d, PrimaryUsage 0x20, MaxHoverHeight and AccurateMaxDigitizerPressureValue on
   the event service and its interface once it attaches.
6. backboardd acquires it as the pencil digitizer; UIKit makes `UITouchTypePencil` touches with force, altitude and
   azimuth, and hover where the flag and Z allow.

### Tested

- Host: `pen/tests.rs` (`cd runtime && cargo test -p xnuhost pen::`; the C's `test_pen`): the lookup's test (a pen's bits, the rank, never side keys or
  multitouch), the sample (pressure scaling, hover height never 0 and half way without the axis, tilt by resolution, as
  degrees and by range, the four turns and the mirror, the eraser end, a tip without `BTN_TOOL_PEN`, out of range all
  zero), the frames (only what changed, nothing for a still pen in the air, the tip and the button, `SYN_DROPPED` read
  again), the guest device's configuration, `pen-rotate=auto`, the script's pen lines and the demo script.
- Generator: `cargo test -p oneios-kernel --test golden`, `the_pen_edits_fit_the_pinned_source` (the edits against
  NativeHidInput 24f1f3f after the phone's).
- PC boot (2026-11-15, Linux PC, qemu-aarch64 under `flock /tmp/oneios-qemu.lock`, 8 vCPUs, `--ram 4G`, a fresh Data
  store from the PC image set, `--no-input --input-script runtime/xnuhost/tools/input-demo.script --set pen=auto`;
  kernel.kc c99c179c of this branch: audit 0 failures, `stubs-check` 666 good, 0 bad; stopped by hand after 9.5 min, no
  panic). xnuhost: `pen: the guest's pen device is /arm-io/input-1 (the input script's pen); oneios-pen=1`, then 87 times
  `pen: in range 1988 ms: 70 frames (8 hovering, 62 touching), 2 strokes, pressure up to 3800/4095, tilt x 0..40 y -30..0
  degrees, the side button pressed 1 time; 0 unchanged frames not sent`. Guest: `droidvm input /arm-io/input-1: OneiOS
  pen, pen`, `virtio-input: 2 device(s) in the device tree, 2 started`, `pen: HID device started under IOHIDResource`,
  `pen: event driver IOHIDEventDriver attached`, `pen: Digitizer/Stylus, Built-In, ... published on the event service;
  edge wrapper 0` (printed before the first report: the wrapper is put on the pen's driver at its first report),
  `pen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid 54, backboardd)`; at the end
  `pen: 6141 pen frames, 6141 HID reports (last result 0x0), 6141 sent (6055 stylus events given the pencil's angles,
  pressure and height, 0 of a layout not known)` (the 86 others are the out-of-range reports, for which the driver
  makes no event: its "multiple untouch" rule) and `pen: 3780 HID events queued for the service's client(s), 0 dropped`.
  So the event data's layout is the one the wrapper knows, in 24A446. **iOS's own log** (`oneios-tool ioslog show
  out/pcboot/data/data.img --proc backboardd --grep digitizer`): `digitizer attached (sender 100000216)
  service:<BKIOHIDService ...; eventSource: builtin; primaryUsagePage: 0xD; primaryUsage: 0x20; name: OneiOS pen;
  transport: SPI; ...>` and `*** acquired pencil digitizer` (beside `*** acquired main display digitizer` for the touch
  screen); and (`--grep encil`) 41 `touchstats 100000216-<main> ...: [ down: 2, up: 2, rangeIn: 1, rangeOut: 1, pencil: 2,
  pathsSeen: 4, destinations: { ... touchIDs: ... } ]`: backboardd counts the strokes as pencil touches and routes them
  to a destination. Not shown on the PC: an app drawing them (the boot shows no UI to the script), UIKit's hover, and
  the stroke's width.
- Host, 2026-11-22 (the Mac's arm64 Linux container of `runtime/host-tests.sh`, the xnuhost unit tests and the brightness
  bridge's only: xnuhost 14229 checks 0 failed; the whole `runtime/host-tests.sh` passed on f30188d, brightness 126 checks 0 failures; the Android xnuhost builds with
  `-Werror` on the Mac's NDK): `test_pen` with the S24 Ultra's
  `sec_e-pen` as the driver states and sends it (x 0..7314, y 0..15686, tilt -63..63, `KEY_HOMEPAGE` and
  `KEY_WAKEUP_UNLOCK` beside the pen's keys: rank 8; `pen-rotate=auto` 0 for the 1440 x 3120 panel; a hover frame in the
  driver's order, a contact whose distance the guest gets as 0, the two-frame leave, the hover height range, the
  screen-off keys and the garage switch never passed on); the coalescing (an edge at once, hover moves held and the newest sent at its
  time by the filter's `tick`, a hover within a pixel not sent, touching frames folded with the most pressure, the tip up and
  the leave at once 1 ms apart, a batch read late spaced by its own times: 5 of 40 frames sent and the 41st held, no
  period: every frame, frame times on another clock: the read's time); `test_brightness`: the pen's node woken, slept and woken with
  the touch IC only with `pen_ic`, refused when its `input/name` is not `sec_e-pen`, taken by its folder without an
  `input/` link, never written without `pen_ic`. `oneios-tool`: `cargo test -p oneios-log --test synthetic`
  (`pen_report`), `cargo test -p oneios-phone` (`--pen`, `phone pen`'s scripts). The full `runtime/host-tests.sh` was
  not run.
- Phone, 2026-11-22 (the parent; runtime 0.1.15, without this branch's fixes): the device found (`pen: /dev/input/event11
  "sec_e-pen": ... x 0..7229, y 0..15662 ...; turned 0 degrees`), `/arm-io/input-1` given, the guest's transport took it,
  the pen service published and opened by backboardd (pid 53). No `pen: the first event` while that log was read: the
  bridge of 0.1.15 never wakes the pen's IC (see "The phone"). Strokes in Notes not shown yet.

### Lag (spen-lag, 2026-11-22)

The owner's first drawing in Notes (2026-11-22, `out/session-logs/2026-11-22/kbd-debug/xnuhost-s3.log`): "S Pen works,
but the drawing is quite laggy at best and pressure does not work". The log: the IC sends about 480 frames a second while
in range (`pen: in range 188 ms: 91 frames (21 hovering, 70 touching)`), and before this fix each changed frame was a
virtio-input frame, a HID report and a stylus event (`oneios-hid: pen: 10116` at 7456.3 s to `12572` at 7464.2 s, all
sent); backboardd's CPU went to 99 %, 164 % and 126 % (`oneios-cpu`) while drawing, against about 30 to 45 % in sessions
without the pen.

**Fix (`pen.rs`'s filter, then `pen.c`'s):** at most one frame per 8.33 ms (`pen::PERIOD_NS`, the panel's 120 Hz), timed by the frames'
own kernel times. An edge (in range or out, the tip down or up, the eraser end, the side button) is sent at once with
the newest position and never held. Between sends the newest frame is held, a newer one replaces it, and `input.rs`'s
poll wakes when the held one is due (the filter's `due` and `tick`, `pen.rs`), so the last position of a pen that stops is sent too.
The pressure sent is the most of the frames folded together while the tip was down. A hovering frame within one guest
pixel of the last one sent is not sent. The `pen: in range` line counts all of it: `U unchanged frames not sent, C
coalesced (at most one frame per 8.3 ms but the edges), K hovering frames moved under a pixel; N sent`. Expected on the
phone: about 120 sent a second while drawing (a quarter of before) and backboardd's CPU back near the sessions without
the pen [I: not yet measured].

The wrapper was not the pressure's fault: that session's guest line says `12542 stylus events given the pencil's angles,
pressure and height, 0 of a layout not known` (of 12572), so every stylus event carried pressure 0..500, the polar
angles and Z. `edge wrapper 0` on the publish line is a global install counter printed before the pen's first report.

### On the phone: the lines that prove it

With `pen=auto` in the session's `xnuhost.conf` (or `--set pen=auto`):

| Step | Line |
|---|---|
| The device found, read only | xnuhost.log `pen: /dev/input/eventN "sec_e-pen": read only, not grabbed; x ..., y ..., pressure ..., distance ..., tilt x ...; keys: tool pen, ..., stylus (the side button); turned D degrees (...)` |
| The guest has the device | `pen: the guest's pen device is /arm-io/input-1 (the host's pen); oneios-pen=1`; guest `droidvm input /arm-io/input-1: OneiOS pen, pen` |
| The pen's IC awake | `brightness: pen: the S Pen's IC (/sys/class/sec/sec_epen/enabled) woken (2,0 then 2,1); it sleeps and wakes with the touch IC` (this branch's bridge); by hand `oneios-tool phone pen status` (`1`: on) and `phone pen wake`. Asleep: no `pen: the first event`, and at most `pen: key N from the pen's IC, not a pen key: its screen-off (AOP) mode ...` |
| The pen service published | `oneios-hid: pen: HID device started under IOHIDResource`; `oneios-hid: pen: event driver IOHIDEventDriver attached (...)`; `oneios-hid: pen: Digitizer/Stylus, Built-In, DisplayIntegrated, MaxHoverHeight, AccurateMaxDigitizerPressureValue published on the event service; edge wrapper 0` (a global install counter printed before the first report: it says nothing of the pen's wrapper, which goes on at the pen's first report; its proof is the frames line's `stylus events given the pencil's angles` above 0) |
| backboardd holds it | `oneios-hid: pen: 1 user client(s) on the event service: IOHIDEventServiceUserClient (pid N, backboardd)` |
| Events arrive | `pen: the first event from sec_e-pen`; per time in range `pen: in range T ms: F frames (H hovering, C touching), S strokes, pressure up to P/4095, height a..b/255 hovering, tilt x ... degrees, the side button pressed B times; U unchanged frames not sent, C coalesced (...), K hovering frames moved under a pixel; N sent` (N about 120 a second while drawing); `pen: SW_PEN_INSERT 1: the pen taken out of its garage`; guest `oneios-hid: pen: N pen frames, N HID reports (last result 0x0), N sent (N stylus events given the pencil's angles, pressure and height, 0 of a layout not known); the last: ...` and `oneios-hid: pen: N HID events queued for the service's client(s), 0 dropped` |
| iOS takes it as a pencil | `oneios-tool ioslog show DATA.img --proc backboardd --grep 'pencil digitizer'`: `*** acquired pencil digitizer` (not `already have`); `--grep 'digitizer attached'` lists the service |
| Pencil touches | Notes (or any PencilKit canvas): strokes whose width follows the pressure. `oneios-tool ioslog show DATA.img --proc backboardd --grep stylus`, `--grep 'ignoring digitizer event'` (must be absent for the pen's service), `--proc Notes --grep pencil` |

All of these at once: `oneios-tool log pen xnuhost.log` (each stage's first line or why it is missing, the stop lines, one row per time in range, the guest's counts, and whether the wrapper is on the pen's driver); live: `oneios-tool phone log --pen -f`. The tilt: this IC's -63..63 is taken as degrees (a range within +-90 without a resolution), sent as degrees, and the guest computes the altitude and azimuth from them; an S Pen leaned past 63 degrees reads 63.

`N of a layout not known` above 0 with `0 stylus events given ...` means the event data is not the layout orient()
knows: tilt and pressure then stay as the driver made them (pressure 0..1, no angles).

## 7g. Video encoder (hevc-encoder, hevc-guest, 2026-11-15; host and guest built, booted on the PC; on main and built on the Mac 2026-11-22; on the phone 2026-11-22: no frame encoded, iOS 27's tile session added; booted 2026-11-22: widgets render, encode-fail 0, no tile encoded yet; 2026-11-23: 10-bit tiles (P010, HEVC Main10) and the helper's binder threads, not yet booted)

The phone's hardware video encoder (Qualcomm's Codec2 `c2.qti.hevc.encoder` / `c2.qti.avc.encoder`) for iOS, so
VideoToolbox encodes HEVC and H.264 and **CMPhoto writes HEIC**: chronod's widget snapshots, screenshots, the photo
library and Live Photos (then the camera's recording, screen recording and FaceTime). Host side:
`runtime/bridges/encoder`; guest side: `kernel/patches/oneios-hosted/codec`.

**The fault (session 8644ae9, 2026-11-15).** Home-screen widgets rendered white: chronod logged 58 times `Error
[com.apple.imageio:chronod] writeImageAtIndex: *** CMPhotoCompressionSessionAddImage: err =
kCMBaseObjectError_UnsupportedOperation [-N] (codec: 'hvc1')`, then `finalizeDestination: session write failed`, and
WidgetRenderer then `Content load failed: unable to find or unarchive file for key: ...systemSmall...` for Weather,
Calendar, News and Maps, sending 50 `WidgetRendererErrorDomain/1` replies to SpringBoard. The owner chose real
HEVC/HEIC through the phone's encoder, not a JPEG fallback.

### The route [F the 24A446 cache, read on the Linux PC 2026-11-15]

VideoToolbox 3350.77.1.6 carries a whole **paravirtualized** encoder and decoder
(`ParavirtualizedVideoEncoder_CreateInstanceWithSpecification`, `..._EncodeFrame/EncodeTile`,
`paravirtualizedVideoEncoder_HandleMessageFromHost`; symbols in the cache) and uses them for every eligible codec
once `VTParavirtualizationIsRunningInGuest` — i.e. once the service **`AppleVideoToolboxParavirtualizationDriver`**
(string in `VideoToolboxParavirtualizationSupport` 71.0.14) is published. CMPhoto 488.22.4's HEIC path takes it:
`_CMPhotoSupportsHEVCHWEncode`'s block calls `VTSelectAndCreateVideoEncoderInstance` with
`kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder` = true and `kCFBooleanTrue`, and sets
`hwSupported` from whether an encoder is returned (disassembled at `0x1a676e924`). The paravirtualized encoder
answers `UsingHardwareAcceleratedVideoEncoder` true, so CMPhoto sees hardware HEVC and `AddImage(codec: 'hvc1')`
stops failing. [F] these cache reads; [I] that iOS 27's encode graph takes the device unchanged (the next session's
log confirms, `codec/README.md`).

**Route chosen: publish Apple's own paravirtualization service** (the guest kernel archive is UniXNU-droidvm's
`DroidVMVTParavirt.cpp` + `DroidVMVTHost.cpp` + `DroidVMVirtioMedia.c`, proven to the iPad home screen on iPadOS 27),
with the real encoding in the host over Android's MediaCodec. **Not** an AVE2 user-client emulation (the `ave`
device-tree node is `compatible = "ave2"`, `soc-id "t8150"`, and `AppleAVE2` 913.43.1's firmware command ABI is
Apple's, not serveable by another encoder) and **not** a `.videoencoder` bundle of ours (CMPhoto requires a
*hardware* encoder; a plain bundle registers as software). This is the route the code already supports: the Metal and
camera plug-ins are likewise iOS-side code built on Apple's own loaders.

### The host side (`runtime/bridges/encoder`)

**Switches (2026-11-23, docs/switches.md):** `encoder-10bit=8bit` (P010 taken, never Main10), `encoder-10bit=nv12`
(the device takes no P010: the guest sends NV12) and `encoder-warmup=0` (the helper starts at the first encode, no
warm-up open). xnuhost passes them to the helper as `oneios-encoder 3 [no-main10] [no-warmup]`.

A stateful V4L2 encoder device, `droidvm,virtio-media` (`/arm-io/virtio-media-0`), on xnuhost's transport: OUTPUT is
tightly packed NV12 (the IOSurfaces VideoToolbox gives, copied by `DroidVMVTHost` into the pool), CAPTURE is the
HEVC/H.264 bitstream (Annex-B NAL units, parameter sets). The real encoding runs in the **oneios-encoder helper**
(xnuhost is static, MediaCodec is a shared library, as with the camera and audio helpers); the helper runs the NDK's
`AMediaCodec` (`video/hevc` → `c2.qti.hevc.encoder`), one keyframe and constant quality for a still. The MMAP'd V4L2
buffers live in a 48 MiB **media pool** carved after the GPU pool, inside the 64 MiB the hosted kernel already lets a
task map (`0024-gpu-pool.patch`, so no new pmap patch). Off by default: `encoder=auto` turns it on, `encoder=sim` gives
the stand-in encoder (below).

- **Code [F]:** `runtime/bridges/encoder/`: `media_device.rs` (the device), `backend_ndk.rs` (MediaCodec), `backend_helper.rs` + `helper.rs` (the
  helper split), `backend_stub.rs` (the stand-in), `encctl.rs` (the phone test); C until 2026-11-23. The message layout and the V4L2 ioctl
  set are what `DroidVMVirtioMedia.c` and `DroidVMVTHost.cpp` issue.
- **Four faults of the device against the real guest, found by reading `DroidVMVirtioMedia.c` and fixed (hevc-guest,
  2026-11-15)** — each would have stopped the first encode on the phone, and the first host test's guest (written
  beside the device) had the same assumptions: MMAP answered the buffer's guest address where the guest expects its
  place in the pool (every buffer refused); CLOSE, which the guest sends with no buffer for an answer, was dropped (no
  session ever closed: the two slots were gone after the guest's start-up check and one encode); ENUM_FMT answered
  nothing; a coded unit larger than the guest's buffer came back as an error at every poll for the session's life.
  The device's worker also polled its backend 50 times a second for ever with nobody using it; it now sleeps until a
  command with no session open. Until 2026-11-25 it still asked an encoder session's backend every millisecond for
  2 s after each command; now the helper backend's receive thread wakes it with each coded unit (`Session::wakes`;
  50 ms guard), and `media: 30 s: worker woken N: by data ..., by the timeout N; 1 ms looks N` says its wake-ups
  (fix/codec-irq, not booted). `runtime/bridges/encoder/README.md` has each.
- **The stand-in encoder [F]:** for a host with no MediaCodec (the PC). It writes a real stream that decodes to the
  frame it was given: HEVC with every coding unit I_PCM (the frame's samples, raw, at the PCM bit depth that fits the
  guest's buffer, dithered), H.264 as I_PCM or a grey picture. xnuhost runs it in its own process with
  `encoder=auto` when the bridges are off (a PC boot, `--mode bench`), and with `encoder=sim` anywhere; never in a
  session on the phone otherwise.
- **Tested [F, Linux PC, the C build]:** `make test`: `test_standin` 173 checks and **ffmpeg decodes all 16 of its streams
  (64x64 to 1440x3120) to exactly the picture given**; `test_encoder` 214 checks, the device driven over xnuhost's
  real transport (`virtio.c`, now `xh_core::virtio`) by a guest of plain memory that does MMAP, CLOSE and ENUM_FMT as
  `DroidVMVirtioMedia.c` does, replaying `DroidVMVTHost`'s encoder sequence (OPEN, S_FMT HEVC+NV12,
  REQBUFS/QUERYBUF/MMAP, STREAMON, QBUF a NV12 test pattern, the coded DQBUF, ENCODER_CMD STOP → EOS, CLOSE, five more
  sessions), through the **real helper process** built for the host and with the stand-in in the device's process;
  the coded picture is read back and compared. The Android build (`oneios-encoder`, `encctl`, `liboneencoder.a`: the C's)
  builds with NDK r28c on the Linux PC; xnuhost links it; `runtime/build.sh` stages both and its ELF check passes.
- **Guess [I]:** NV12 as `COLOR_FormatYUV420SemiPlanar` ByteBuffer input to `c2.qti.hevc.encoder` (not yet run on the
  phone); the still-image CQ settings; that a frame's unit fits the guest's buffer. `encctl` checks the first two on
  the phone.

### The guest side (`kernel/patches/oneios-hosted/codec`)

`oneios-tool kernel gen codec` builds `liboneioscodec.a` between the two kernel passes (`kernel/build-hosted.sh`,
after `audio`): UniXNU-droidvm's `DroidVMVTParavirt.cpp` unchanged, `DroidVMVirtioMedia.c` (since 2026-11-25 its event queue on the
device's interrupt instead of a 1 ms poll, `oneios-codec-irq=0` the poll; `codec/README.md` "The transport on the
interrupt"), `DroidVMVTHost.cpp`
with four edits (the hvcC's level, below; iOS 27's tile session, a small HEVC session coded larger and cropped, `jcap`:
"On the phone, 2026-11-22" below), and `OneiOSCodec.c` (`oneios_codec_start`, which `oneios_drivers_start`
calls; a weak default in `drivers/bridge/OneiOSBridge.c` stands for a kernel without the archive). The hvcC edit: its
profile, tier and level bytes were copied from the SPS with its emulation prevention bytes in them, which every
HEVC SPS has there, so the hvcC carried `03` bytes as flags and level 0; they are taken without them.
`OneiOSCodec.c` prints the start line, the device's own line, and every 30 s while a figure moves the counters of
the three files (with the last message type the host has no case for) and the session lines `DroidVMVTHost.cpp`
keeps (`codec/README.md` has the lines and what each failed step means).

Limits [F from the code]: a session's buffers come from the 48 MiB pool, first fit (each of two sessions had a fixed half
until 2026-11-24), about 9 bytes a pixel for an encoder; four sessions at a time, encoders and decoders. The decoder:
"The video decoder" below.

### Built and booted on the Linux PC (hevc-guest, 2026-11-15)

- **Built [F]:** `HOSTED_OUT=hevc XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh` on `origin/main` `fb4ec88` plus this
  work (716 s): `xnu.macho` `972b9132...`; `kernel.kc` `e4ffcabd1d95f25e...` (96,141,312 bytes), `audit: 2711 paired,
  93 functions on the stub, 9 data unpaired; 0 failures`, `prelink stubs-check` 666 good, 0 bad.
- **Booted [F], 40 s:** a PC boot with `--set encoder=auto` printed xnuhost's `encoder: on: stand-in backend, media
  pool 48 MiB at 0x20fbe04000 (2 session slots)`, and the guest's `ONEIOS CODEC: 1 virtio-media device(s), 1 up;
  VideoToolbox paravirtualization registered (stage 2)` and `ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000
  out: NV12 cap: HEVC H264` (the guest transport's own OPEN, ENUM_FMT and CLOSE against the device). The boot was
  then ended: the owner stopped all work.
- **Not done:** that boot to the home screen and iOS's log from it (who opens the service, whether CMPhoto encodes;
  chronod's `hvc1` errors and WidgetRenderer's `Content load failed`, counted against a boot without `encoder=auto`).
  Three finished PC boots of other work have neither line at all, so the PC may not reach the widget snapshot
  without a GPU [I]. `codec/README.md` has the state and the next step.

### On main, built on the Mac (2026-11-22)

- PR #59 merged onto main with 0081 and 0082; `codec` is the last hosted force-load (`oneios-tool kernel
  hosted-archives`). `HOSTED_OUT=codec kernel/build-hosted.sh`: `xnu.macho` `baad8462...`; `kernel/prelink/build.sh`:
  `out/kernel/codec-24A446/kernel.kc` `5add18fab1cb64ce...` (96,845,824 bytes), audit 2712 paired, 92 on the stub,
  0 failures; `prelink stubs-check` 666 good, 0 bad; `_oneios_codec_start` and 0082's `_hosted_dyld_flags_init` in
  it. `runtime/host-tests.sh` passes; ffmpeg on the Mac decodes the 16 stand-in streams to the picture given.
- **Fixed [F]:** xnuhost read the image set's `encoder=` and then dropped it, and the OneOS app passes no `--set`:
  no phone session could have turned the encoder on. It applies now (`encoder_given`, then `config.c`, now `runtime/xnuhost/src/config.rs`; host-tested).
- **Not done:** a boot of this kernel. The phone is next (below).

### On the phone, 2026-11-22: iOS 27's tile session

- **The fault [F]:** main 203a9e7's kernel (`out/kernel/codec-24A446`) with `encoder=auto`: the host encoder worked
  (`encctl hevc`, `encctl h264 640 480` passed; `encoder: on: mediacodec-helper backend`; the device's line), and no
  frame was encoded: `ONEIOS CODEC: counters: ... messages 469 (unknown 117) ...; sessions 116, frames in 0 out 0`,
  3 sessions a second, each videocodecd `'+enc' 'ests' 'einv' '-enc'`. iOS's log: `ParavirtualizedVideoEncoder_
  StartTileSession: ... failed with error -12900 for 84 x 84`, and chronod's 116 `CMPhotoCompressionSessionAddImage:
  err = unknown error [-12900] (codec: 'hvc1')`: white widgets still.
- **The cause [F, 24A446's VideoToolbox]:** iOS 27's CMPhoto writes HEIC through VideoToolbox's **tile encoder**
  (`ests` StartTileSession, `epti`, `etil` EncodeTile, `ecti` CompleteTiles; the host's `TEsa`, `TEcd`, `TEem`).
  UniXNU-droidvm's host was recorded against macOS's and has none of them: `ests` got the unknown type's -12900, and
  VideoToolbox gave each session up. 24A446's own host and guest (both in its VideoToolbox) give each message's
  parameters (`codec/README.md`, "The tile session", has the table).
- **The fix [F host-tested]:** the staged `DroidVMVTHost.cpp` has the tile session (each tile copied from its part
  of the surface and coded as a key frame, handed over by `TEcd`/`TEem`, `ecti` answered as `ecfr`); an HEVC session
  below 96x96 (the 84x84 tiles: c2.qti codes nothing smaller) is coded at 96 and cropped back by its SPS's
  conformance window (`codec/OneiOSCodecPicture.h`: x265's and VideoToolbox's 96x96 streams cropped to 84x84 decode
  to the exact top-left of the picture in ffmpeg); `jcap` (the JPEG session's query) is answered -12908; the
  counters line names the last unknown type. `oneios-tool ioslog health`'s `encode-fail` now names `unknown error
  [-12900]` whole (it said `unknown`).
- **Built [F]:** `HOSTED_OUT=codecfix kernel/build-hosted.sh`, `xnu.macho` `1325991f...`; `kernel/prelink/build.sh`:
  `out/kernel/codecfix-24A446/kernel.kc` `852adb8be21b525d...` (96,845,824 bytes), audit 0 failures, `prelink
  stubs-check` 666 good, 0 bad. Host tests: `cargo test` (golden, ioslog), `kernel/tests/host.sh` 8 of 8.
- **Not done:** a phone boot of the new kernel (`codec/README.md`, "What a session on the phone must show").

### On the phone (first run 2026-11-22, above)

The runtime package carries `bin/oneios-encoder` and `bin/encctl` (built on the Linux PC or the Mac by
`runtime/build.sh`). As root, One UI running, then a session:

```sh
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl hevc'          # c2.qti.hevc.encoder, 256x256
adb shell su -c 'cd /data/local/tmp/oneios/runtime/bin && ./encctl h264 640 480'
# then a session with encoder=auto: the line encoder=auto in the staged image/overlay/userland/xnuhost.conf (its other
# lines kept), staged with kernel.kc and kernel.kc.hashes by `oneios-tool phone manifest DIR --stage`
```

(`runtime/bridges/encoder/README.md`, "On the phone", has the commands from a build tree and what `encctl` prints.)

- `xnuhost.log`: `encoder: on: mediacodec-helper backend, media pool 48 MiB at ...`; `ONEIOS CODEC: 1 virtio-media
  device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)`; `ONEIOS CODEC: device 0: oneios-encoder
  ...`; `encoder: session N: HEVC WxH on mediacodec-helper` (96x96 for chronod's 84x84 tiles); `ONEIOS CODEC:
  counters:` with `sessions` growing slowly (not 3 a second), `frames in N out N` above 0, `errors 0`, `unknown` not
  growing; the message lines `'ests'`, `'etil'`, `'ecti'`.
- `oneios-tool phone pull-log DIR` (iOS's log): **no** `StartTileSession failed`; **no**
  `CMPhotoCompressionSessionAddImage ... (codec: 'hvc1')` (`encode-fail 0` in `ioslog-health.txt`); WidgetRenderer **without**
  `Content load failed`;
  `oneios-gpu-pixels: WidgetRenderer_ ... drawn`; a screenshot saved as HEIC (`....ftypheic`) [I: that iOS 27 saves
  one as HEIC]. `oneios-tool ioslog checks` counts these failure lines in its `heic` row: 0 is the proof.

**The freeze of 2026-11-23 (v0.1.16, two boots).** The helper backend's open waited for the helper's reply with no
limit, on the vCPU that answered the guest's `VIDIOC_STREAMON`, with the virtio-media device held. The helper never
answered (MediaCodec's open did not come back in it; where it waits is not known yet), so that vCPU took no interrupt
for 80-90 s, a stackshot's cross-call panicked the guest (`Debugger synch pending on cpu 1`), and xnuhost's main loop
waited for the same device and never ended the session ([booter.md](booter.md) 2.9c). Fixed in
`backend_helper.c`: the HELLO and each write 1 s, an open's reply 2.5 s; a helper that misses one is killed with a
line that says where it waited (`encoder: the helper (pid N) did not answer an open in 2500 ms (state S, waiting in
WCHAN): killed (1 of 2); the next open starts a new one`), the next open starts a new helper, and after two hangs the
encoder is off for the session (`encoder: the helper hung 2 times: no more helpers this session (VideoToolbox keeps
its software encoder)`). Host-tested (`test_hung_helper`); not yet on the phone. **Left:** the open still runs on the
vCPU (up to 2.5 s of a deaf vCPU, under the 5 s `debug-ack`); moving it to the device's worker would make STREAMON
return before the session is open, which the guest's V4L2 path does not expect today. Why MediaCodec's open hangs in
the helper is the next thing to find from the new line.

**The session of 2026-11-23 18:24 (runtime 0.1.18) and its fix (2026-11-23; built and host-tested, not yet booted on
the phone).** No tile was coded, for two reasons ([codec README](../kernel/patches/oneios-hosted/codec/README.md),
"The tile session on the phone" and "10-bit pictures and the helper's open"):
- six `DroidVMVTHost: stage 101 failed: 2019963440` (`'xf20'`): CMPhoto gave the wallpaper posters' tiles in 10-bit
  surfaces. The guest now takes `x420` and `xf20`, opens a tile session's device at its first tile with P010 for a
  10-bit tile, and xnuhost codes P010 as HEVC Main10 (`COLOR_FormatYUVP010`, `HEVCProfileMain10`) when the encoder
  offers it at that size, else as 8-bit from each sample's high byte. Each session's line says which
  (`encoder: session N: HEVC 96x96 on mediacodec-helper: 10-bit pictures as Main10 (P010)`), and `encoder: 60 s:
  coded 8-bit N, 10-bit as Main10 N, 10-bit as 8-bit N; opens refused N; sessions N` counts them every 60 s while any
  are coded; the guest's `ONEIOS CODEC: pictures: 8-bit N, 10-bit as P010 N, 10-bit as 8-bit N` counts the copies.
- six `encoder: backend mediacodec-helper refused 96x96`: not a refused size (`encctl hevc 96 96` passed on
  2026-11-22) but the helper's open that never came back (`did not answer an open in 2500 ms ... futex_wait_queue`,
  killed, twice, then no more helpers). The helper and `encctl` now start their binder threads (the camera helper's
  fault of 2026-11-17: a plain executable has none, and Codec2 calls its client back over binder) [I: that this was
  the hang], the helper starts at the boot's first virtio-media session and opens one HEVC encoder at once (`encoder:
  helper: warm-up: ...`, with the encoder's sizes from Android 16's codec store), and a slow open says which MediaCodec
  call it waits in (`encoder: helper: an open of HEVC 96x96 (NV12) has waited 1000 ms in AMediaCodec_configure`). A
  picture below the encoder's smallest size is coded at that size (the guest's SPS crop takes it back).
  `encctl caps` prints the encoders' sizes and Main10 support; `encctl hevc10 96 96` codes a P010 picture.
- **2026-11-28 (takeover session hangs in `AMediaCodec_configure` / `AMediaCodec_start`):** on the `v0.1.44-test` S24
  Ultra `session-mode=takeover` boot, `oneios-encoder` still timed out in `AMediaCodec_configure` / `AMediaCodec_start`
  (falling back to software video decode at 733% CPU and overheating the phone). Three root causes fixed:
  1. `runtime/bridges/camera/spawn.rs` `spawn_with_fd3` now sets `POSIX_SPAWN_SETSIGMASK` with an empty signal mask so
     `oneios-encoder` and `oneios-camera` do not inherit xnuhost's blocked `SIGRTMIN..=SIGRTMIN+3` signals.
  2. `app/switcher/assets/oneios_switch.sh` removed `mediametrics` from `TAKEOVER_DEFAULT` (kept in
     `TAKEOVER_STOPPABLE`), because `AMediaCodec` blocks in `libmediametrics` when `mediametrics` is held stopped.
  3. `runtime/bridges/encoder/backend_ndk.rs` + `helper.rs` mark the background `warm_up()` thread with
     `ignore_open_step_on_thread()` so warm-up calls cannot overwrite the main thread's global `OPEN_STEP` watchdog
     state. Not booted on the phone yet.

### The video decoder (fix/hw-decode, 2026-11-24; host-tested, built; not yet booted on the phone)

**The fault [F, `out/session-logs/2026-11-24-mac-gpumem/live.log`].** YouTube in Safari lagged and the phone got hot.
The device was an encoder only (card `oneios-encoder`), so `decs` (videocodecd asked it at 108622 s) listed no
decoder, and iOS decoded the video on the CPU: `oneios-cpu` had com.apple.WebKit.Web at 91-142 %, videocodecd at
15-26 % and mediaplaybackd at 58 % during the video.

**What 24A446 does [F, its VideoToolbox, WebCore and VideoToolboxParavirtualizationSupport, `ipsw dyld extract` of
`out/ios27-cache`].** The paravirtualized decoder takes any codec type the host lists: `vtLoadParavirtualizedVideoDecoders`
registers each `decs` entry, and `VTIsHardwareDecodeSupported` creates a decoder with hardware required (VP9 only
behind `VTIsVP9DecodeEnabledForCurrentProcess`). The guest's messages are `decs +dec dsta dfrm dfin dinv -dec dset
dmul dcop dsup`, plus two that upstream's host did not answer:
- `dcap` (`vtCreateDecoderCapabilitiesDictionary`, in a guest: `,ver ,cod`, answer `,cda`). `VTCopyAV1DecoderCapabilitiesDictionary`
  and `VTCopyHEVCDecoderCapabilitiesDictionary` (WebCore's) send it. The dictionary: `VTSupportedProfiles` (profile
  numbers), `VTPerProfileSupport` (by the profile's number as a string: `VTIsHardwareAccelerated`,
  `VTMaxDecodeLevel`, `VTMaxPlaybackLevel`).
- `dcan` (`CanAcceptFormatDescription`, `,fds`, answer `,can`).
- The tile decoder (`dsts dtil dtfi`) exists too and is not answered (HEIC decoding stays in software).

**The route.** The guest's half is upstream's decoder in `DroidVMVTHost.cpp` (H.264 and HEVC), with staged edit 6
(`tools/oneios-tool/crates/oneios-kernel/src/staged/codec_decode.rs`): VP9 and AV1 (8-bit 4:2:0 only, by their `vpcC`
and `av1C`), `dcap` and `dcan` answered, the surface's kernel mapping kept for the session (upstream mapped and
unmapped about 3 MB of IOSurface per 1080p frame), a SOURCE_CHANGE while streaming taken, and decode counters. The
host's half is xnuhost's same virtio-media device, which now decodes too (`runtime/bridges/encoder/README.md`, "The
decoder"):
- The compressed frames go to the oneios-encoder helper over its socket.
- MediaCodec runs there in asynchronous mode (`decode_ndk.rs`: `video/avc`, `video/hevc`, `video/x-vnd.on2.vp9`,
  `video/av01`; ByteBuffer output asked as NV12).
- The helper writes each picture **straight into the guest's CAPTURE buffer**, in the media pool, which it maps from
  xnuhost's RAM memfd (sent by SCM_RIGHTS after the HELLO).
- The guest copies it once into the IOSurface.
- No thread polls: MediaCodec's callbacks drive the copy, and the device's worker is woken by each result.

**The lines.**
- xnuhost: `decoder: on: mediacodec-helper backend, H.264, HEVC, VP9, AV1 (decoder-codecs=)`,
  `decoder: session N: VP9 1920x1080 on mediacodec-helper`, and
  `decoder: session N: pictures 1920x1080 at a stride of S`.
- The helper: `encoder: helper: decoder: VP9 1920x1080: c2.qti.vp9.decoder, opened in N ms` and
  `encoder: helper: decoder: VP9 1920x1080: output ..., taken`.
- Every 60 s while frames are decoded: `decoder: 60 s: decoded H.264 N, HEVC N, VP9 N, AV1 N of N fed; decode time
  avg A ms, max M ms; dropped N; sessions N, opens refused N (mediacodec-helper)`.
- The guest: `ONEIOS CODEC: decoded: avc1 N, hvc1 N, vp09 N, av01 N; dropped ...; dfrm to Demt avg N us, max N us
  (N frames)` at the codec report's 30 s cadence.

**Switches:** `decoder=auto|off`, `decoder-codecs=` (docs/switches.md).

**Tested [F, the build laptop].**
- `cargo test -p oneencoder`: the decoder's unit tests, and `tests/decoder.rs`. That test runs the guest's V4L2
  sequence over the real transport, through the helper process with a memfd pool (VP9, H.264) and in-process (AV1,
  HEVC), and checks `decoder=off`. It also runs `encctl decode` on an IVF and an Annex-B file.
- `cargo test -p oneios-kernel --test golden`: the edit on the pinned source.
- The kernel build and prelink (`out/kernel/hwdec-24A446/kernel.kc` `dfd8835e...`, audit 0 failures, stubs 666 good;
  `codec/README.md` has the hashes), and `runtime/build.sh` (elf-check 0 failures). Not booted (no qemu boots: the
  phone is the test).

**Guesses [I].**
- That WebCore offers VP9 and AV1 to YouTube once `dcap` answers.
- That c2.qti's decoders take the in-band parameter sets and give NV12 in ByteBuffer mode.
- That 1080p decodes in time.

`encctl decode FILE` checks the second and third on the phone, and a session's lines the first.

## 7h. The host device (host-device, 2026-11-21)

One tree for every known host phone: xnuhost finds which phone it runs on at its start and takes that phone's
defaults from one compiled-in table, `runtime/bridges/hw/host_device.rs` (its API: `onebridges::hw::host_*`).
The table is the source of truth: the host profile files (`profiles/host-galaxy-s24-ultra-sm8650.toml`, one for every S24 Ultra model number,
the SM-S928W too; `profiles/host-galaxy-s25-ultra-sm8750.toml`, `profiles/host-galaxy-s25-sm8750.toml`, `profiles/host-galaxy-s26-ultra-sm8850.toml`, `profiles/host-galaxy-s26-sm8850.toml`, `profiles/host-galaxy-s26-exynos-s5e9965.toml`) are records for
the port docs and are read by no code (the runtime package carries them in `lib/profiles/`, beside the IPSW profiles). The bridges' ctl tools (`brightctl`, `batteryctl`, `thermalctl`,
`bridgectl`) and `oneios-tool phone status` use the same table.

**What is read** (once, read only): the properties `ro.product.model`, `ro.product.device`, `ro.board.platform`,
`ro.soc.model`, `ro.soc.manufacturer` (bionic's property area, as xnuhost's `usb.rs` reads it: `__system_property_find` and
`__system_property_read_callback`), and as fallbacks `/sys/devices/soc0/machine`, `/sys/devices/soc0/soc_id` (logged
only) and `/proc/device-tree/compatible`. Matched in this order, the first entry that matches wins: a model prefix, a
codename, the SoC (`ro.soc.model`, then soc0's machine), the platform, the device tree's compatible list (an entry
`vendor,NAME` whose NAME is the SoC or the platform [I]: a board's own `qcom,pineapple-mtp` does not match). A SoC or
platform match alone means "that SoC's entry" (an S24 or S24+ on SM8650 gets the S24 Ultra's: `pen=auto` finds no pen there).

**Precedence:** an explicit setting (the command line; the image set's `xnuhost.conf` or the overlay's; an
environment variable that is set) > the table's default > the built-in default.

| Setting (xnuhost; the ctl tools' variable) | Values | What |
|---|---|---|
| `host-device` (`ONEIOS_HOST_DEVICE`) | `auto` (default), `none`, a name below | `auto`: detect; `none`: no entry, today's defaults (nothing is read); a name: that entry whatever the phone says (the properties are still read for the log). A name that is not in the table is refused at start |
| `host-props=FILE` (`ONEIOS_HOST_PROPS`) | a getprop dump | read instead of the property area: `adb shell getprop` lines (`[key]: [value]`) or `key=value` (build.prop). Host tests; a PC replaying a phone. A file that cannot be read is refused |
| `host-root=DIR` (`ONEIOS_HOST_ROOT`) | a folder | stands for `/` for the sysfs and proc fallbacks (tests) |

**The table** (each entry holds only settings that already exist as options; marks as in `docs/s25.md`); `takeover: true` marks the rows that allow `session-mode=takeover`, the S24 Ultra's and the S26 Ultra's only, and `xnuhost --host-info` prints it (`takeover=yes|no`):

| Name | Matched by | Defaults it changes | Why, and what is not set |
|---|---|---|---|
| `galaxy-s24-ultra` | `SM-S928*` (every S24 Ultra model number: below), `e3q`, SoC `SM8650`, platform `pineapple` | `pen=auto`, `encoder=auto` (2026-11-28) | the home device: today's behaviour with its S Pen and its hardware video encoder and decoder on (host-tested: its finished config is `host-device=none`'s with those two). Both had been only in the image set's `xnuhost.conf`, and the rebuilt 27.2.1 `24B156` image had neither: the sessions of 2026-11-27 had no `pen:` line and `ONEIOS CODEC: 0 virtio-media device(s)`, so Safari's video was decoded on the CPU. The command line and the image set still win (`pen=off`, `encoder=off`). `takeover: true`: `session-mode=takeover` is allowed (the opt-in Android takeover, [booter.md](booter.md) 2.9d; not a default, not run on the phone yet) |
| `galaxy-s25-ultra` | `SM-S938*`, `pa3q`, `SM8750`, platform `sun` [F] (YumikoHayamato/OneiOS: the fork's SM-S938B) | `ONEIOS_THERMAL_SOC_TRIPS=1`, `ONEIOS_THERMAL_SOC_SAMPLES=3` | [F] the fork's sessions 1-7 (2026-11-19, One UI 9.0): the kernel's first passive trip on the SoC zones is 105 C and the hot one 125 C, so the S24's SoC rule (HEAVY 95 C, TRAPPING 105 C, one reading) put iOS in TRAPPING (its "too hot" screen) at each throttle: 20 level changes in 8 minutes, 5 into TRAPPING, on single junction spikes (68 -> 104 -> 68 C between two polls). With the trips (115/120 C) and the lowest of 3 readings (15 s) the SoC rule stayed under them; the levels came from the case, as on the S24. The SoC rule takes no step: this changes what iOS is told, not what xnuhost runs (`runtime/bridges/thermal/README.md`, "The SoC rule on the phone's own trips"). Not set: the backlight (`panel0-backlight` max 12750 [F], One UI's manual top not measured on this phone; the S26's 2300 is not assumed). The panel is 1440x3120 [F] (`nt37801`, DSI command mode), from the display helper's report. Otherwise: the Adreno 830 fence path is the default everywhere (drm2kgsl `0007`), the Oryon CPU rule is automatic (`topology.c`), the panel's mode comes from the display helper's report (`width=`/`height=` would override it, so 1080x2340 is not forced); every bridge's node names are [U] (`tools/probe/s25-probe.sh` answers them), so every backend stays samsung |
| `galaxy-s25` | `SM-S931*`, `pa1q`, `SM8750`, `sun` [F] (the `S931BXXUCDZIF` vendor_boot DT compatible list) | `panel-fallback=1080x2340@120`, `corner-radius=34`, `boot-args-add=oneios-hid-buttons-builtin=1 oneios-button-haptics=1`, `hw-backend-thermal=treble`, `hw-backend-fallback=1`, `ONEIOS_THERMAL_SOC_TRIPS=1`, `ONEIOS_THERMAL_SOC_SAMPLES=3` | [F] a 1080x2340 panel (`PA1_S6E3FAE_AMB616FL03`) with 34 dp / 102 px rounded corners (`corner-radius=34`, NoNameBuilder/OneiOS `120df342`); built-in side and power keys (`23b9b85f`); case temperature from the stock `android.hardware.thermal` HAL `SKIN` sensor with file fallback (`9bed1282`, because Samsung's `lrp` reads ~10 C below the back glass). The SoC rule as the S25 Ultra's [I] (same SM8750) |
| `galaxy-s26-ultra` | `SM-S948*`, `m3q`, SoC `SM8850`, platform `canoe` [F] (YMGPwcca/OneiOS: the fork's SM-S948B, `S948BXXS4BZIG`; codename and platform since 2026-11-23, `docs/s26.md`) | `ONEIOS_BRIGHT_MAX=2300`, `ONEIOS_AUDIO_PROBE_PREFILL=1` | [F] `ONEIOS_AUDIO_PROBE_PREFILL=1` since 2026-11-28: on the fork's phone the audio probe's stream started underfilled never moved (section 6, "One raise per starvation, and the probe's prefill"). [F] the fork's phone with One UI in control: One UI's manual top (screen_brightness 255) wrote 2300 to `panel0-backlight`, whose `max_brightness` is 12750 (`brightness/tests.rs`, `sm8850_manual_brightness_cap`; the C's `test_brightness.c`). Without it iOS's slider top writes 12750, 5.5 times One UI's manual top (heat, current). Conservative; not a claim about the panel's maximum. `takeover: true` (2026-11-23): `session-mode=takeover` is allowed, where their root-only Framework-takeover experiment ran with the touchscreen grabbed ([booter.md](booter.md) 2.9d); `touch-exclusive` itself is not a default (that mode turns it on). The native-touchscreen priority and the `cpu-hw-trip-*` exclusion already apply on every phone |
| `galaxy-s26` | `SM-S942*`, `SM-S941*`, `SC-51G*`, `SCG36*`, `m1q`, `SM8850`, `canoe` [F] (burugou05-wq/OneiOS `e85405f1`: live docomo `SC-51G` / `SM-S942D`) | `panel-fallback=1080x2340@120`, `ONEIOS_BRIGHT_MAX=2300`, `ONEIOS_THERMAL_SOC_TRIPS=1`, `ONEIOS_THERMAL_SOC_SAMPLES=3` | [F] FHD+ 1080x2340@120 panel (`mMaxLuminance 1275.0`), 8 Oryon cores (`cpu0-5` cap 741, `cpu6-7` cap 1024), Adreno 840 KGSL + `/dev/udmabuf`, thermal passive 105–114 C / hot 125 C (`docs/s26.md`). Shared `SM-S942*`/`SM-S941*` prefixes are disambiguated against `galaxy-s26-exynos` by checking agreeing device/SoC/platform/compatible first |
| `galaxy-s26-exynos` | `SM-S942*`, `SM-S941*`, `m1s`, `s5e9965`, `erd9965` [I] (burugou05-wq/OneiOS `e85405f1`) | `gpu=2d`, `panel-fallback=1080x2340@120` | No physical `m1s` unit captured yet; no KGSL on Exynos 2600 (Xclipse 960 on amdgpu), so 2D fallback and FHD+ panel fallback until verified |
| `galaxy-a56` | `SM-A566*`, `a56x`, SoC `s5e8855` (the fork's gate: `ro.soc.model`), platform `s5e8855` [I] | `gpu=auto`, `panel-fallback=1080x2340@120`, `width=720`, `height=1560`, `scale=2`, `hz=60`, `return-graceful-ms=4000`, `hw-backend-haptics=treble`, `hw-backend-fallback=1` | [F] no KGSL (Xclipse 540 on amdgpu, `/dev/dri/renderD128`). `gpu=auto` since 2026-11-24 (`gpu=2d` before, when xnuhost's 3D half was drm2kgsl only): xnuhost's amdgpu renderer only when the kernel's gpuaddon carries the A56's Metal plug-in (`OneiOSRadeonMetal`) and the renderer's self-test passes at the start, else 2D as before with a `gpu: 2D only: ...` line ([a56-gpu.md](a56-gpu.md) 2; its 8 MiB GPU pool then unused); `gpu=2d` in the image's `xnuhost.conf` keeps it 2D. Not run on an A56. The panel: 1080x2340 [F] (the fork's `docs/a56-guide.md`), as for the S25. Since 2026-11-24 (the other A56 fork, Musicalista/OneiOS-A56 to `c83b9ca`, [a56.md](a56.md) "The second port"): the guest frame 720x1560 @2x at 60 Hz [F] (the same 360x780 points as 1080x2340 @3x, 2.25x fewer pixels for its CPU compositor; the display helper scales the frame to the panel; the frame is taken as one setting, and its scale wins over the image set's `scale=3` but not over the command line's, and not where the command line or the image set gives a width or a height); `return-graceful-ms=4000` [F] (hosted 0106 sends logd `SIGTERM` in that window, so iOS's log reaches the Data store); haptics on the AIDL vibrator HAL [I] (issue 73's log: `haptics: off: no force-feedback device with FF_PERIODIC + FF_SINE under /dev/input`; the fork found no vibrator either), with the evdev search behind it if the HAL is not registered. Bluetooth: the AIDL HAL, no HIDL one [F]: the probe finds it with no setting (section 4, "AIDL mode", 2026-11-24; not run on an A56). Touch, battery and thermal names [U]: samsung stays |

**The S24 Ultra's models.** One entry for every model number, matched by `ro.product.model`'s prefix (record: `profiles/host-galaxy-s24-ultra-sm8650.toml`; host-tested for
each one below: `runtime/bridges/hw/test_hw_backend.rs`, `host.rs`, `scripts/port/test-verify-host.sh`, `tools/oneios-tool/tests/phone/run.sh`):

| Model | Market | What is known |
|---|---|---|
| SM-S928B | Europe, Asia (global) | [F] the owner's phone: e3q, SM8650, platform pineapple; every session in this tree |
| SM-S928U, SM-S928U1 | US carrier, US unlocked | e3q, SM8650 [I] (one hardware family); not booted with iOS |
| SM-S928W | Canada (CSC XAC) | The same path as the B in the runtime and in the OneOS fork (its profile `s-ultra-8650`, the iOS 27 card, the switch's own check, the custom-ROM rule since fork 0.4.89: host-tested there too). the US carrier model's bootloader and vendor partition under its own firmware: `ro.bootloader` `S928USQU6EZIF`, `ro.product.vendor.model` `SM-S928U`, firmware `S928WVLU6EZIF` [F per the OneOS fork's public issue 128, a stock phone, 2026-11-10]. Nothing in the runtime reads either, so its own model matches. e3q, SM8650, pineapple [I]; its side keys, panel, touch and pen nodes, backlight path, thermal zones and HAL instance names [U] (the B's are kept: same hardware [I]). Root: KernelSU as on the B; whether its bootloader can be unlocked is [U] (the user's own matter). Not booted with iOS |
| SM-S928N, SM-S9280 | Korea, China | e3q, SM8650 [I]; not booted with iOS |
| SC-52E, SCG26 | Japan (docomo, au) | not `SM-S928`, codename [U]: they get this entry by their SoC (`SM8650`, the SoC's entry, no defaults either); the OneOS fork's profile (`SM-S928*` and `e3q` and `SM8650`, all three) does not match them, so its iOS 27 card warns them as another phone (not refused) |

Which keys an entry may hold: xnuhost settings that `config.rs` merges as device defaults (`gpu`, `panel-fallback`, `corner-radius`, `artwork-subtype`, `boot-args-add`, `thermal-cap`, `hz`, `width`, `height` and `scale` (one frame), `return-graceful-ms`, `astc`, `hw-backend*`; an
entry with another key makes xnuhost refuse to start, and the test `config::tests::host_device` forces every entry and checks each default
lands), and environment variables a bridge reads (`ONEIOS_BRIGHT_MAX`, `ONEIOS_THERMAL_SOC_TRIPS`, `ONEIOS_THERMAL_SOC_SAMPLES`; xnuhost sets them while it has one thread,
without overwriting a variable the session set). `gpu`'s default moved with this: unset until the config's finish (C: `xh_config_finish`), so an
image set's `gpu=` is now taken (it was read and dropped before; no image this tree writes has one).

**The log line** (xnuhost's start, right after `xnuhost: session ABI ...`):

```
host: galaxy-s24-ultra (Galaxy S24 Ultra (SM-S928, SM8650)), matched by ro.product.model=SM-S928B; read: ro.product.model=SM-S928B ro.product.device=e3q ro.board.platform=pineapple ro.soc.model=SM8650 ro.soc.manufacturer=QTI; defaults: none (today's)
host: galaxy-a56 (Galaxy A56 (SM-A566, Exynos 1580)), matched by ro.product.model=SM-A566E; read: ...; defaults: gpu=auto, panel-fallback=1080x2340@120, width=720, height=1560, scale=2, hz=60, return-graceful-ms=4000, astc=0, hw-backend-haptics=treble, hw-backend-fallback=1
host: galaxy-a56 (...), ...; defaults: gpu=auto not taken (the command line or the image set gives it)
host: galaxy-s26-ultra (...), matched by ro.product.model=SM-S948B; read: ...; defaults: ONEIOS_BRIGHT_MAX=2300, ONEIOS_AUDIO_PROBE_PREFILL=1
host: unknown phone (read: ro.product.model=Pixel 11 Pro ro.product.device=... ...): today's defaults (host-device=NAME forces one of galaxy-s24-ultra, ...)
host: detection off (host-device=none): today's defaults
```

The ctl tools print the same line first (`bridges: host: ...`, or `  host: ...` in `bridgectl`); `bridgectl host` prints
what was read, the match and the whole table (read only, safe beside One UI and beside a session).

**Tested** (2026-11-21, on the PC, the C build; no phone; the same cases now in Rust): `runtime/bridges/hw/test_hw_backend.c` (now `test_hw_backend.rs` `host_device`) (each phone, a codename, a SoC and
a platform alone, soc0's machine, the compatible list, an unknown phone, getprop and build.prop dumps, forced, none, a
bad name, a missing file, the environment's defaults taken and kept, the ctl tools' line); `runtime/xnuhost/tests/test_xnuhost.c` (now `runtime/xnuhost/src/config/tests.rs`)
`test_host_device` (the S24 Ultra, an unknown phone, no property area, the S25s and the S26 finish to exactly the config of
`host-device=none`; the A56's `gpu=auto` (`gpu=2d` before 2026-11-24) under the command line and the image set, and `gpu-node=`; the image set's `host-device=none`;
every entry forced); the stand-in phone (`tools/oneios-tool/tests/phone/hw-backend.sh`: `brightctl` on an S26 Ultra's
panel, 1..2300 of 12750 detected, 1..12750 with `none` or the S24's properties, a set `ONEIOS_BRIGHT_MAX` kept; `run.sh`:
`phone status`'s host line for each phone). 2026-11-23, on the build laptop: every S24 Ultra model number above (SM-S928B, U, U1, W, N, 0; the W with its US vendor model and bootloader) in `test_hw_backend.c` (the C, on main), `host.rs`, `test-verify-host.sh` and `run.sh`; the same day in the Rust (`cargo test -p onebridges host_device`, `test_hw_backend.rs`): those models, the stock SM-S928W, and the S26 Ultra by its codename `m3q` and by its platform `canoe` alone; `cpu::test_cpu::cpu` has the S26 Ultra's SM8850 topology (the C's `test_bridges.c` `test_cpu`). **Not run on a phone**: the first phone start shows the line above; on the
S24 Ultra it must say `galaxy-s24-ultra ... defaults: none (today's)`.

## 7i. Sensors: motion and the front light, from the sensor hub (fix/sensors-als, 2026-11-25; on by default since the merge into main, `sensors=off` turns it off; not booted)

Details: `runtime/bridges/sensors/README.md` (the host, the route, the HAL fallback), `kernel/patches/oneios-hosted/hid/README.md`
("Motion sensors", "The SPU light sensor"), `drivers/brightness/README.md` (the light sensor). The research behind it:
`docs/ports-2026-11-25-hw.md` sections 1 and 2.

### The phone

- The accelerometer, gyroscope, magnetometer, pressure, proximity and the **front** light sensor are on Qualcomm's sensor
  hub (SSC, the ADSP's sensors PD, its SEE), not on `/dev/input`; Android's sensors HAL reaches the hub over QRTR (QMI
  service 400) [F `docs/ports-2026-11-25-hw.md` 1, ios-redfin's and iOSPlus10's readings of the SEE protocol]. The rear
  light sensor `als_rear` (section 7a) gives 0 lux on the S24 [F `docs/TODO.md`].
- [U] whether a root process beside One UI may open `AF_QIPCRTR` and sees service 400 (SELinux: KernelSU's domain
  allows most things [I]), and the S24's data type names and light sensors. **`oneios-tool phone qrtr --sensors`** answers
  all of it, read only (`tools/oneios-tool/README.md`).

### The host (`runtime/bridges/sensors`, xnuhost `motion.rs`; `sensors=auto|ssc`, default auto since 2026-11-25)

- A SEE client ported from ios-redfin (MIT) on Linux's own QRTR: the hub's service 400 found at xnuhost's boot, each
  sensor's SUID and attributes; the light used is the first whose name does not say "rear" (`sensors-light=N` picks one).
- The accelerometer and gyroscope at `sensors-hz` (50), the magnetometer at 25 Hz, the light on change; one thread in
  `poll()`; streams stop while iOS's display is off and at the stop (closing the sockets ends them on the hub).
- Motion frames go to a third virtio-input device, `/arm-io/input-N` "OneiOS motion sensors" (INPUT_PROP_ACCELEROMETER;
  the gravity vector 256 per g in iOS's sign, 16 per degree per second, 16 per microtesla); xnuhost adds `oneios-motion=7`
  (15 with a magnetometer). Only when the kernel collection has no `com.apple.driver.AppleSPU` member (`motion_allowed`,
  `session/board.rs`; v0.1.30 gave none at all): else `sensors: motion: not given to the guest: KERNEL has
  com.apple.driver.AppleSPU, ...; prelink it again`, and with one `sensors: motion: the kernel's AppleSPUHIDDevice ...`. Light readings go to the brightness bridge (`Bright::als_feed`, then `OB_BRIGHT_ALS`) in place
  of `als_rear`; xnuhost adds `oneios-als-spu=1`.
- No hub (auto): `sensors: off: source old ...`, no motion device, the rear light sensor: the boot of before.
- [I] about 125 wake-ups a second of the bridge's thread and 50 frames a second to the guest while iOS's display is on,
  one a minute while it is off.

### iOS (`kernel/patches/oneios-hosted/hid`, `drivers/brightness`)

- Motion (`oneios-motion=MASK`): UniXNU-droidvm's `AppleSPUHIDDevice`s (proven on iPadOS 27 24A446 there: locationd and
  CoreMotion, backboardd's autorotation) for the accelerometer `{0xff00, 3}`, gyroscope `{0xff00, 9}`, device orientation
  `{0x20, 0x8a}` and, ours, the compass `{0xff00, 0x0a}`; their SPU command channel `AppleSPUHIDDeviceUserClient`.
  Apple's `com.apple.driver.AppleSPU` has classes of the same names, and it was in both collections until 2026-11-25:
  v0.1.29 panicked at kernel start (S24 Ultra, 24B156: "Kext com.apple.driver.AppleSPU class AppleSPUHIDDevice is a
  duplicate"). The archive's IOHIDEventDriver personality for `{0xff0c, 3}` on Transport SPU (score 100000) wins the
  match, but does not keep Apple's `AppleSPUGestureDriver` (the same pair and transport) from being tried:
  `IOService::probeCandidates` loads the kext of every personality that passes `matchPassive`
  (`IOCatalogue::isModuleLoaded`), whatever its score, before it starts the best one [F xnu-12377 IOService.cpp,
  IOCatalogue.cpp]. So `prelink roots` leaves AppleSPU out of both collections, with the kexts that link it and
  would bring it back as a dependency (AppleALSColorSensor, whose `AppleSPUCT7xx` match the SPU light sensor's
  `{0xff00, 4}`, AppleEmbeddedLightSensor, AOPTouchKext, AppleHIDTransportSCMCommon; `kernel/prelink/README.md`), as
  UniXNU-droidvm's iPad kernelcache has no AppleSPU [F kernelcache 24A446 and 24B156]. Two guards for a collection
  prelinked before: xnuhost gives no motion device when the collection has a `com.apple.driver.AppleSPU` member, and
  the kernel registers no motion class when that kext has an OSKext (`oneios_motion_register`). Its line names whose
  classes are in use: `oneios-hid: motion: AppleSPUHIDDevice registered below IOHIDDevice, ...; the kernel's classes:
  com.apple.driver.AppleSPU is not in the collection`.
- Light (`oneios-als-spu=1`): the light sensor as an iPhone's SPU ALS `{0xff00, 4}` (Transport SPU, Built-In, VendorID
  0x5ac, `ALSCurveInfo`: Jesvi-iOS's reading of CoreBrightness), its event driver's events turned into ambient light
  events, and the last reading re-posted within 15 ms of a wake (ios-redfin 1298: CoreBrightness's 250 ms
  `FirstALSSampleTimeout`). 0: the HID Sensor device for `AppleUSBALSService` of section 7a, as before.

### Proof lines (none seen yet)

- `oneios-tool phone qrtr`: `qrtr: SSC: service 400 version V at node N port P answers: ...`.
- xnuhost: `sensors: on: ssc ...`, `sensors: streaming: ...`, `sensors: 60 s: accelerometer N, ... light N readings, last N lux`.
- The kernel: no `class AppleSPUHIDDevice is a duplicate` panic; `oneios-hid: motion: AppleSPUHIDDevice registered below
  IOHIDDevice, ...; the kernel's classes: com.apple.driver.AppleSPU is not in the collection`,
  `oneios-hid: motion: AppleSPUHIDDevice accelerometer, gyroscope, orientation, compass published`, its 60 s
  line, `ONEIOS ALS: SPU ambient light service (0xff00/4) published`, `ONEIOS ALS: IOHIDEventDriver's dispatch wrapped`,
  `ONEIOS ALS: 60 s: ...`.
- iOS (`oneios-tool phone pull-log DIR`, `ioslog --proc locationd`, `--proc backboardd`): locationd's `CLIoHidInterface`
  matching the services, CoreMotion's orientation, CoreBrightness's ALS lines; on the screen an app that rotates, and
  auto-brightness following a hand over the front sensor.

## 8. Sources

| What | Repository and commit |
|---|---|
| Stock device trees (decompiled): CPUs, haptics, audio, Wi-Fi, Bluetooth, camera slots | [samsung-sm8650/android_kernel_samsung_sm8650-devicetrees@2f6a973](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650-devicetrees/tree/2f6a9734e556f2f7680b0094bec4ad4671157f8b) (`samsung/e3q_eur_openx_w00_r09.dts`, `qcom/pineapple-v2.dts`) |
| Samsung's kernel source: CS40L26 and sec_vib_inputff drivers, `e3q_defconfig`, `drivers/optics/stk6d2x*.c` (the light sensor) | [samsung-sm8650/android_kernel_samsung_sm8650@809f7ed](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650/tree/809f7edaad95be2f60ce09f577fe5104d8940214) |
| Common device tree: HAL binaries (Bluetooth HIDL 1.1, vibrator service, camera provider, sensor modules) | [samsung-sm8650/android_device_samsung_sm8650-common@48c66f6](https://github.com/samsung-sm8650/android_device_samsung_sm8650-common/tree/48c66f6543b31784fce4e7d23ea855897f18ce6e) |
| e3q device tree: Wi-Fi and Bluetooth firmware names | [samsung-sm8650/android_device_samsung_e3q@fb2ba30](https://github.com/samsung-sm8650/android_device_samsung_e3q/tree/fb2ba3010d59c0f38ddf81aef7fd9c78e2172860) |
| Other trees seen, not used: Samsung OSS builds with KernelSU, the Gunyah kernel | `YuzakiKokuban/android_kernel_samsung_sm8650` (7c7f8dd), `Andy312432/android_kernel_samsung_sm8650_gunyah` (bacbd93) |
| iOS-side ports | iOSPlus10 (local, read-only): `patches/laurel/xnu/2700-laurel-pmi632-haptics.patch`, `2600-laurel-bluetooth-wcn3990.patch`, `docs/laurel_sprout/{haptics,bluetooth,camera,audio}.md` |
| Guest drivers taken unchanged | UniXNU-droidvm f9121cd (`external/UniXNU-droidvm/package/`), `docs/references.md` |
| What other ports would fix in the open areas (sensors, auto-brightness, camera, audio, Bluetooth), ranked, with licences, effort and proof lines (2026-11-25, research only: nothing ported then) | [ports-2026-11-25-hw.md](ports-2026-11-25-hw.md): ios-redfin 23019ab4, ios-guacamole 53e6c5e, UniXNU-sheng c8f92b2, UniXNU-droidvm f9121cd, Jesvi-iOS a13e406 |
| The owner's phone | board notes of 2026-11-10 in `docs/agents.md` (shell and root reads) |

Only single files were fetched from the kernel and device repositories (raw downloads at the pinned commit) and one
shallow clone of the 748 KiB devicetrees repository; nothing of them is in this repository.

## Galaxy S25 (SM-S931B, SM8750)

The second target; the facts, what changed for it and what is unknown are in [s25.md](s25.md). Ported here so far:
the CPU topology rule (`runtime/bridges/cpu/topology.rs`: a Qualcomm core, MIDR implementer 0x51, is never E, and a
host whose cores are all P but of different speeds lends its slowest cores to the guest as E vCPUs), the
read-only phone probe (`tools/probe/s25-probe.sh`), and — for the S25 Ultra (SM-S938, pa3q) — the Adreno 830 fence
default (`0007-a830`), `virgl_fence_table_init`, the host profile, `scripts/port/verify-host.sh`,
`scripts/phone/ssh-forward.sh` and the guest shell builders ([s25.md](s25.md), "Galaxy S25 Ultra"). The S24 Ultra sections above stay as they were.
