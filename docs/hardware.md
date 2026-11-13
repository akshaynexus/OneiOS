# Hardware: what the Galaxy S24 Ultra has and how iOS reaches it

Owner of this file: the bridges agent (`drivers/`, `runtime/bridges/`). Date: 2026-11-10.

iOS 27 runs on a source-built XNU inside xnuhost, a process on One UI's kernel. One UI's kernel, init services,
vendor HALs, audioserver, cameraserver and surfaceflinger keep running in a session; zygote, system_server and every
app are gone. So each device has two halves:

- a **driver in the kernel** (`drivers/`, or one taken unchanged from UniXNU-droidvm) that iOS's daemons talk to;
- a **host bridge** in xnuhost (`runtime/bridges/`) that uses the Linux or Android interface.

Marks: **[F]** read in a source at a pinned commit, or reported from the owner's phone (SM-S928B, kernel
`6.1.162-android14-11`, board notes of 2026-11-10 in `docs/agents.md`). **[I]** inference. **[U]** unknown until tried
on the phone. **Nothing in this file has run on the phone**: the host bridges pass their unit tests on the PC, build
for Android, and the kernel-side drivers compile and link-check against the kernel build.

## 0. State at a glance

| Device | Kernel side | Host side | State |
|---|---|---|---|
| CPU | `droidvm-amp.patch` + `DroidVMCpufreq.c` (UniXNU-droidvm, unchanged) | `runtime/bridges/cpu`: topology, vCPU plan, thread placement, the cpufreq device | host built and tested; kernel side waits for release-kernel (series, archive) and booter (device tree, placement calls) |
| GPU | UniXNU-droidvm's virtio-gpu 3D + NativeAdreno user client (unchanged) | booter's `virtio_gpu.c` + drm2kgsl | booter's; not owned here (section 2) |
| Wi-Fi / network | `kernel/patches/oneios-hosted/net`: `IO80211Controller`/`IO80211Interface` under the collection's `IOEthernetController`/`IOEthernetInterface`, a station that asks the host, UniXNU-droidvm's virtio-net transport (section 3) | `runtime/bridges/wifi`: wlan0 is iOS's for the session: frames by AF_PACKET, scans by nl80211, joins by the vendor's supplicant (AIDL V2 over `/dev/binder`); `wifictl`; the old tap/NAT route only with `wifi=tap` | both built; host tested (132 + 35 checks, the binder client on a real binder driver); on the phone beside One UI: the supplicant's service and an nl80211 scan (53 networks) answer our client; **not yet run in a session** (wifid, a join, DHCP) |
| Bluetooth | `drivers/bluetooth` (port of iOSPlus10's transport) on `drivers/bridge` | `runtime/bridges/bt`: the HAL over `/dev/hwbinder` | both built; on by default in a session since 2026-11-11 (before: off, `bluetooth=virtual`); HAL client tested on the PC against a fake HAL on a real binder driver, not yet on the phone |
| Haptics | `drivers/haptics` (port of iOSPlus10's LaurelVibrator and haptic node) and, since 2026-11-12, the Actuator IOAudio2Device of the audio archive (UI and keyboard taps) | `runtime/bridges/haptics`: evdev force feedback | both built; host tested with a fake actuator; the `vibrator` path ran in sessions (alerts), the Actuator path not yet run (section 5) |
| Audio | `DroidVMVirtioSnd.c` + `DroidVMAudio.cpp`: the hosted kernel's audio archive since 2026-11-12 (before: not built, no device in iOS) | `devices/virtio_snd.c`, `audio/aaudio_helper.c` (`oneios-audio`) | helper run on the phone beside One UI (plays and records, 0 underruns, about 130 ms); host-tested against a fake AAudio; in sessions iOS's I/O thread failed for large buffers (system sounds 3 s late or lost): hosted 0052, not yet run (section 6) |
| Camera | `drivers/camera`: the `oneios,camera` transport, `/dev/oneios-camera`; `userland/camera`: the capture plug-in (its device object not written) | `runtime/bridges/camera`: helper `oneios-camera` (provider HAL in a session, Camera2 NDK with One UI), ring, device, `camctl` | host and kernel sides built and host-tested; iOS shows no camera yet; one phone run (2026-11-11): cameraserver lists 4 cameras and opens one for a root client, no frame arrived (no binder threads in the process: fixed, the fix not yet run on the phone); the HAL back end never run (section 7) |
| Backlight and light sensor | `drivers/brightness`: the backlight service (port of iOSPlus10's `AppleARMBacklight`), the light readings; the HID light sensor device, built into the hosted kernel's HID archive | `runtime/bridges/brightness`: the backlight sysfs node, `als_rear` by evdev, `brightctl` | both built; host tested; in a PC boot the HID device starts and Apple's `AppleUSBALSService` attaches to it; on the phone backboardd finds the service; the slider reached no request because CoreBrightness ran VMBLControl (fixed 2026-11-12 in backboardd through the Metal plug-in, not yet run; section 7a) |
| Battery | `drivers/battery`: an `IOPMPowerSource` powerd reads | `runtime/bridges/battery`: Android's `power_supply` nodes, polled, changes sent over the bridge channel; `batteryctl` | both built; host tested with a fake sysfs; one phone session (2026-11-12 23:45, on the charger): published, and a "plugged in" sound before the lock screen (the startup chime: section 7c) |
| Thermal | `drivers/thermal`: `OneiOSThermal` (the readings and the level in the registry); `userland/helpers/oneios-thermal` sets iOS's `com.apple.system.thermalpressurelevel` from it | `runtime/bridges/thermal`: the thermal zones, Samsung's thermistors, the battery's sensors; a level on iOS's scale; xnuhost's CPU step when hot; `thermalctl` | host built and tested; the bridge and `thermalctl` run on the phone (2026-11-12); the step, the driver and the helper not yet run; an idle session's heat traced to the guest's idle cache flush (xnuhost's `setway-skip`: -40% CPU) (section 7d) |
| Touch screen and buttons | `kernel/patches/oneios-hosted/hid`: UniXNU's NativeHidInput (digitizer, buttons) under the collection's `IOHIDDevice`, UniXNU-droidvm's virtio-input transport with the side keys | xnuhost's `input.c` (evdev, no grab) and `virtio_input.c`; `input_script.c` for a host with no panel | built; in a PC boot backboardd opens both services and scripted touches and keys are queued for it as HID events; never run on the phone (section 7b) |

Common pieces:

- **The bridge channel** (`drivers/bridge/oneios_bridge_proto.h`): one virtio-mmio device, `/arm-io/bridge`, compatible
  `oneios,bridge`, carrying small messages (haptic requests, HCI packets). Guest end `drivers/bridge/OneiOSBridge.c`,
  host end `runtime/bridges/devices/bridge_dev.c`. Devices that already have a virtio driver in UniXNU-droidvm
  (network, sound) use standard virtio devices instead.
- **Tests**: `runtime/bridges/tests/host.sh` (CI runs it): 198 checks (and 48 in `brightness/`, 65 in `battery/`) with ASan and UBSan, the virtio devices driven
  through xnuhost's real transport (`runtime/xnuhost/src/virtio.c`) by a guest made of plain memory; then the Android
  build. `oneios-tool kernel drivers --check` compiles the kernel sources with the kernel build's own compile records
  (xnu-12377, arm64e, `-Wall -Wextra -Werror`); the archive's 292 undefined symbols all resolve in `xnu.macho`.
- **On the phone**: `runtime/bridges/build/android/bridgectl` (static) tries each bridge without xnuhost:
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
- `l2-cache-size`, `l3-cache-size` of a cpu node are read by XNU when present.

### The bridge (`runtime/bridges/cpu/topology.c`)

- `br_cpu_read("/sys", &t)`: per CPU `cpu_capacity`, `cpufreq/cpuinfo_{min,max}_freq`,
  `scaling_available_frequencies`, `related_cpus`, `regs/identification/midr_el1`, `cache/index*`. CPUs of the lowest
  capacity are E when capacities differ: on this phone cpu0-1 are E, cpu2-7 P.
- `br_cpu_plan_make(&t, nvcpu, pin, &plan)`: the guest's vCPUs, **E first** (an iPhone's boot CPU is an E core), then P.
  8 vCPUs: 2 E + 6 P. Fewer: one E per four vCPUs, at least one. Each vCPU carries `type`, `cluster_id` (0 E, 1 P),
  the host CPUs it may use (`affinity`: all cores of its kind; with `pin` one core each, the X4 first), the frequency
  table and cache sizes of the fastest core in the mask.
- `br_cpu_place(tid, &plan, vcpu)`: `sched_setaffinity`. For the vCPU thread, and for the Linux thread of the iOS
  thread a vCPU runs (iOS threads are threads of other processes; booter's `xh_thread_run`).
- `br_cpufreq_*`: the register window. A write of a frequency sets the utilisation clamp
  (`sched_setattr`, `SCHED_FLAG_UTIL_CLAMP_MIN`, capacity x kHz / max kHz) of the vCPU's thread; the lowest table
  entry drops the request. Linux's governor keeps clock control. `br_cpufreq_requested(d, vcpu)` lets booter apply
  the same clamp to the iOS thread it runs (`br_cpu_request`).

### Missing

- release-kernel: `droidvm-amp.patch` in the series, `DroidVMCpufreq.c` in the board archive.
- booter: `cpu_cluster[]` and the cache properties of the cpu nodes from the plan; the `/arm-io/cpufreq` node
  (`reg` = nvcpu x 0x1000) and its MMIO region forwarded to `br_cpufreq_read/write`; `br_cpu_place` on its threads.
- [U] whether `uclamp` is writable from KernelSU's domain on Samsung's kernel (`CONFIG_UCLAMP_TASK`); `bridgectl cpu`
  says.
- [I] `hw.perflevel*` cache sizes come from the kernel reading `CCSIDR_EL1` on the running CPU, which under xnuhost
  is the host core the vCPU thread happens to be on: correct per kind as long as placement holds.

## 2. GPU

Not owned here. The references agent found the working path on this SoC (`docs/references.md` section 1): the guest's
NativeMetal plug-in, metal2vulkan and Turnip over virtio-gpu's DRM native context, served on the host by
virglrenderer's `drm2kgsl` on `/dev/kgsl-3d0`. That is the 3D half of booter's `runtime/xnuhost/src/virtio_gpu.c`;
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
     WPA elements in the last scan (`br_wifi_plan_join`):

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
   - scan: nl80211 `TRIGGER_SCAN` on wlan0 with one wildcard SSID (an active scan, as wificond's) and `GET_SCAN`
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
3. **Return.** xnuhost's stop (`br_wlan_close`): `disconnect` and `removeNetwork` for its network, `removeInterface`
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
  framework also reads (an address at 0x8d0, a flag at 0x8d8, two 32-bit values at 0x8dc and 0x8e0, three bytes at
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
  daemon's `_dispatchAsyncAssocEvent` reads them, so an empty event is not safe), 10 SCAN_DONE; and the link state on
  the IOKit side (`setLinkStatus`, which configd's DHCP waits for). A join that worked sends, in this order: the
  IOKit link, ASSOC_DONE, SSID_CHANGED, BSSID_CHANGED, LINK_CHANGED.
- The regulatory country (51 COUNTRY_CODE) is the Country element of the strongest network of the last scan that
  has one (the host's station finds it and sends it in `OW_STATE`); "XX" until a scan.

### The host bridge (`runtime/bridges/wifi`)

| File | What |
|---|---|
| `frames.c` | which frames cross (above), and the cut of a coalesced TCP frame |
| `station.c` | the station the guest's driver talks to: its messages become calls on a radio back end, the radio's reports become messages |
| `wlan.c` | the back end on the phone: the AF_PACKET socket, nl80211, wlan0's settings kept and given back. Three threads: frames, nl80211 events, control. The guest's vCPU only sends a frame or queues a message; netlink, binder and the join's timeouts are the control thread's |
| `sim.c` | a made-up radio (`wifi=sim`) |
| `supplicant.c` | the supplicant's client: the lookup, the interface, the callback object, the join's plan and its outcome |
| `aidl_parcel.c`, `aidl_binder.c` | stable AIDL over `/dev/binder` with no Android library (xnuhost is static): the interface token (strict mode word, work source, `SYST`), `String` as UTF-16, `byte[]`, binder objects with the stability word (VINTF 0x3f), the status header of a reply; the kernel's binder protocol, one looper thread for the calls into our callback, death notifications |
| `tools/wifictl.c` | the bridge without xnuhost: `status`, `scan`, `join`, `restore` (`runtime/bridges/README.md`) |

xnuhost: `wifi=auto` (the default; `xnuhost.conf` or `--set wifi=...`) takes wlan0 when the bridges are on, `wifi=off`
makes no network device, `wifi=tap` is the old route, `wifi=sim` is `sim.c`: a made-up radio with two networks
("OneiOS open", and "OneiOS WPA2" with the passphrase `password`) for a boot on a PC, where the guest's side can be
run and read in the log; no frame goes anywhere (unit-tested; not yet run in a boot). Log lines: `wifi: wlan0 is iOS's for the session: ...`, `wifi:
on: wlan0 (..) is the guest's en0`, `wifi: the vendor's supplicant is the bridge's`, `wifi: the guest's driver is up`,
`wifi: join: network N in the supplicant, WPA2 (passphrase): selected`, `wifi: supplicant: <state>`, `wifi: join done`
or `join failed (status S, code C)`, `wifi: link down (reason R)`, `wifi: closed (...)`, `wifi: wlan0 given back`.

Old (only with `wifi=tap`): `runtime/bridges/net/net.c` (tap `oneios0`, DHCP answered in the bridge: 192.168.77.2/24)
and `runtime/bridges/net/oneios-net.sh` (NAT). The script's `ip rule del priority 16000` would delete netd's own rule
of that priority (netd's `fwmark 0x10063/0x1ffff iif lo lookup local_network` is at 16000 on this phone) when ours is
absent: a reason not to run it on the phone as it is.

### Tested

- Host (arm64 Linux container, `runtime/host-tests.sh`): `test_wifi` 153 checks (frame rules; the netlink group reader; the DHCP reader, with the REQUEST forms
  SELECTING, INIT-REBOOT, RENEWING and REBINDING, iOS's and another station's, and the log line of each; a 4000-byte coalesced
  IPv4 frame and a 3000-byte IPv6 one cut into three with right lengths, sequence numbers and checksums; the station
  against a fake radio; the AIDL parcels byte for byte; the join's plan for WPA2, WPA3, transition, WPA with TKIP,
  open, OWE and a network not seen; the supplicant's callbacks decoded); `test_aidl_binder` 35 checks: the client
  against a fake service manager and supplicant in another process on a real binder driver (a lazy service found on
  the third lookup, the interface added and removed, the callback's descriptor and version asked for inside
  `registerCallback`, a join, a wrong key, a network not found, a rejection, a link that drops, a look without a
  callback, a supplicant that dies). Both ends of that test use `aidl.h`.
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
    (`br_wifi_nl_group`).
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
  the file is the 17 characters of the address and a NUL (`od -c`), and `br_bt_address` took the NUL for a
  character that is not part of an address. Fixed 2026-11-12 (`hci.c`: the text ends at the first NUL; a test with
  the phone's layout); not yet run on the phone.

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

- `hci_hidl.c`: a HIDL client with **no Android library** (xnuhost is static): `/dev/hwbinder` through the kernel's
  binder UAPI, `IServiceManager::get`, `initialize_1_1(callbacks)` (or `initialize` on a 1.0 service),
  `sendHciCommand/AclData/ScoData/IsoData`, and a callback object the HAL calls (events, ACL, SCO, ISO,
  `initializationComplete`, and the base methods `interfaceChain`, `interfaceDescriptor`, `ping`, ...). A death
  notification turns the state to failed. Two threads: the looper only answers the HAL's calls into us; the caller
  makes our calls (get, initialize, the sends, close) in order from a queue of up to 512 packets, so the guest's
  vCPU, which holds the bridge device's lock, never waits on a binder call, and a HAL that sends an event before its
  `sendHciCommand` returns is answered at once. No `initializationComplete` within 15 s
  (`ONEIOS_BT_INIT_TIMEOUT_MS`) is a failure with a line naming SELinux. `br_hci_hidl_probe()` asks for the
  service once at xnuhost's start. `ONEIOS_HWBINDER` names another binder device (the PC test).
- `bridge_dev.c`: an `OB_HCI_OPEN` while the controller is failed (the HAL died or never finished) closes it and
  opens it again, so a new bluetoothd client starts over.
- `hidl_parcel.c`: the parcels (interface token, `hidl_string`, `hidl_vec<uint8_t>` as scatter-gather buffer
  objects, strong binders). Unit-tested byte for byte; the transaction codes are the HIDL order of the methods.
- `hci.c`: packet length checks both ways, the state, the address file. It logs `bluetooth: the controller hears
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
- **PC stall, fixed in the driver:** with the transport core, the kernel controls and the device nodes started at the
  board's probe (before the root mount), three qemu boots of 2026-11-13 out of three never got their root device
  (APFS's start waited for `IOBSD` while `IOResources` stayed busy; `hosted_dump_threads=1`); with `oneios_bt=0` or no
  bridge two out of two booted. They now start after the root mount (`ONEIOS BT: transport up N ms after the start:
  ...`); the next qemu boot found its root at once and bluetoothd ran. The phone never showed the stall.

### Missing, unknown

- [U] **SELinux**: hwservicemanager checks `find` for the caller's context, and the HAL's domain must be allowed to
  call back into ours (`binder { call transfer }`). KernelSU's domain is allowed most things; if the callback is
  denied, a `ksud sepolicy` rule is needed. `bridgectl bt` shows which step fails.
- [U] the transaction codes and the callback handshake against the real libhwbinder and the QTI HAL. Tested on the PC
  (`make -C runtime/bridges test-binder`, 2026-11-11): a fake hwservicemanager and IBluetoothHci 1.1 and 1.0 on a
  real binder driver (binderfs in a user namespace) that read every parcel with libhwbinder's checks (tokens,
  `readBuffer`/`readEmbeddedBuffer` parent links), call back two-way from their own thread, `interfaceChain` on our
  callback, Reset and Read BD_ADDR answered, ACL echoed, close; no HAL registered; no `initializationComplete`; a
  HAL that dies. First phone test: `bridgectl bt` must print two Command Complete events (`runtime/bridges/README.md`).
- booter: the tree's node. `runtime/loader/src/machine.c` publishes `/bluetooth` with `transport-encoding` 10 (the
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
- Messages: `OB_HAPTIC_VIB {on_ms, intensity 0..1000, frequency_hz}`, `OB_HAPTIC_STOP`.

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
- **The requests** (`drivers/haptics/oneios_actuator.h`, host-tested by `drivers/haptics/test_actuator.c`, 16 checks):
  a period is loud above 256 of 32768; the first loud period asks 20 ms (a tap), the run renews when less than 12 ms
  is left (50 ms segments after 20 ms) or the level moves by more than 100 of 1000; a run of 30 ms or more is stopped
  after 10 ms of silence, a tap runs out. Intensity = the period's peak; frequency = the run's zero crossings with
  hysteresis, after 10 ms. A tap is one request; a long vibration about 20 a second (each is an effect upload on
  the CS40L26's I2C).
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
on this identity (`home-button-type 2`, no haptic Home button) its pending state is harmless [I].

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

### The bridge (`runtime/bridges/haptics/haptic.c`)

- Finds the device by its EV_FF bits (`sec_vibrator_inputff` first), sets `FF_GAIN` 10000 once.
- One effect, updated for every request: `FF_PERIODIC` / `FF_SINE`, period = 1000 / frequency clamped to 4..10 ms
  (6 ms when iOS names none), magnitude = 40 + 215 x intensity / 1000 (an LRA does not move below a floor), length
  = the request's (at most 10 s). Then `EV_FF` play; stop = play 0. A worker thread does the ioctls (each is I2C
  traffic), the newest request wins.
- The motor is stopped when the guest resets the device, when the bridge closes, and by the kernel when the file
  closes (xnuhost exits).

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
`ONEIOS AUDIO: speaker running|idle, ... engine N periods, N late, ...; virtio-snd N tx periods, N dropped ...`.
Boot arguments: `droidvm-snd=0`, `droidvm-audio=0`, `oneios-audio-out-uid=`, `oneios-audio-in-uid=` (default
Speaker, Mic; `none`: no microphone), `oneios-audio-latency=FRAMES` (the speaker's "output latency", default 6480 =
135 ms: the host path below, so iOS keeps video in step), `oneios-audio-report=0`, `oneios-audio-selftest=SECONDS`
(1..10: 3 s after the start a 1 kHz sine at -30 dBFS straight into the transport, before iOS's audio server: the
guest kernel to the speaker without iOS; UniXNU-droidvm's `droidvm-snd-selftest` is a full-scale sawtooth, never with
a speaker).

VirtualAudio publishes only its Null device when the guest says it is a VM (`vmm-present` 1 in xnuhost's tree):
the boot argument `owner-hide-vmm=audiomxd,com.apple.WebKit.GPU` (lead's decision of 2026-11-11, Droid-VM-UniXNU's
`patches/xnu-12377/zzyd-owner-hide-vmm.patch`, in the arch series the build uses) hides it from audiomxd. xnuhost's default boot arguments do not carry it
yet: the audio test cycle adds it (below).

### The bridge

- `runtime/bridges/devices/virtio_snd.c`: the device (control queue: `PCM_INFO`, `SET_PARAMS`, `PREPARE`, `START`,
  `STOP`, `RELEASE`; two streams; output periods go to the back end as they arrive; a capture thread fills the
  receive buffers the guest posted).
- `runtime/bridges/audio/aaudio_helper.c` -> `bin/oneios-audio`: **a helper process**, because AAudio is a shared
  library and xnuhost is static. xnuhost starts it (`br_audio_helper(path, &ctx)`, `posix_spawn`, a `SOCK_SEQPACKET`
  pair); it lives in xnuhost's namespace and cgroup, and the return's SIGKILL of the namespace (or xnuhost's exit,
  which closes the socket) ends it and with it the stream: audioserver closes a dead client's track.
  - **No realtime thread, by construction.** AAudio makes a SCHED_FIFO thread for an MMAP stream and for a FAST
    track with a data callback, and asks system_server's `scheduling_policy` service for it; in a session that
    service is gone [I: AOSP's `requestPriority` waits for the service; not read in source here]. So the helper
    uses no data callback (a blocking `AAudioStream_write` from its own thread), turns MMAP off for its process
    (`AAudio_setMMapPolicy(NEVER)`) and asks for performance mode NONE. Measured beside One UI: every thread of the
    helper SCHED_OTHER (policy 0), `mmap 0`, the threads in the parent's cpuset; AudioTrack sets the starting
    thread's nice to -16 (`ANDROID_PRIORITY_AUDIO`, not realtime). LOW_LATENCY without MMAP and callback gave the
    same normal-mixer track on the phone, so it is not offered. A guard checks every thread after each start and
    every 10 s and sets one found SCHED_FIFO or SCHED_RR back to SCHED_OTHER (logged).
  - Output: the guest's periods go into a ring; the writer moves 240-frame chunks into AAudio, whose buffer is cut
    to 2 bursts (`ONEIOS_AUDIO_BURSTS`). Before the guest's first chunk, silence fills the level (the ring plus
    what AAudio holds) to one burst plus a target (`ONEIOS_AUDIO_TARGET_MS`, 20 ms): the server takes a whole burst
    per cycle. An underrun (AAudio about to run dry) plays silence, primes again and raises the target by 5 ms (at
    most 80). Clock drift: the level averaged over 1 s is held within 5 ms of its value 1 s after the start by
    playing a chunk from 239 or 241 ring frames (linear interpolation, at most 4166 ppm); a ring above the target
    plus 10 ms is played faster the same way; above the target plus 60 ms it drops its oldest audio to the target
    (an overflow). Until the server takes the stream's first frames (a newly opened deep-buffer track takes
    nothing for about 200 ms) nothing is cut: the ring holds 500 ms, and after the stall its backlog (plus 40 ms)
    is the limit, shrinking as it plays at up to 1.25 % faster. The guest's STOP keeps the stream open for
    `ONEIOS_AUDIO_LINGER_S` (60 s since 2026-11-12, 15 s before): what the ring holds plays, then silence; iOS stops
    and starts its device around each sound, and a reopened track stalls again (the open and about 200 ms before the
    server takes frames). The price is One UI's speaker path kept on with silence for that minute.
  - Input: a reader thread, blocking `AAudioStream_read` of 240 frames, input preset VOICE_RECOGNITION, the first
    channel to xnuhost. `backend.c` drops what an earlier recording left in the socket at each input START.
  - Log (xnuhost's log): `oneios-audio: speaker open: ... none, sharing shared, mmap 0, burst 1600, buffer 3200 of
    4800 frames, device 3`; every 10 s and at each stop `speaker running|stopped ...: frames in, played, underruns,
    overflows, slips +N/-N, ring, AAudio N ms queued, N xruns; latency about N ms`; the same for the microphone.
  - Fixed 2026-11-12: the helper's state reports were blocking sends that xnuhost reads only while the microphone
    runs, so after a few hundred speaker starts the socket would fill and the helper stop for good. They are
    `MSG_DONTWAIT` now.
- Why AAudio and not tinyalsa: on this SoC a PCM device plays only after the AudioReach graph is set up through the
  AGM plug-ins and PAL, which the audio HAL does; opening the card beside it would also fight audioserver.
- Tests: `runtime/bridges/tests/test_audio_helper.c` (in `make test`): the helper against a fake AAudio whose DAC
  takes bursts of 1600 from a buffer of 2 bursts: no underrun or overflow at 0 and ±2000 ppm, the drift steered the
  right way, latency under 120 ms, the microphone's periods in order, the linger (a quick restart takes the open
  stream; the stream closes after the silence), a clean stop when the socket closes: 4049 checks.

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
  53 ms) plus the normal mixer and the HAL (about 78 ms). What was looked at:
  - a non-deep-buffer route: the track is on `primary-out` already (above); the `fast` port needs a FAST track,
    which AudioFlinger grants only to a callback or a Java writer and then raises that client's thread to SCHED_FIFO
    through system_server: not allowed (no realtime thread) and not there in a session. MMAP is out the same way.
  - a smaller buffer: `ONEIOS_AUDIO_BURSTS=1` and `ONEIOS_AUDIO_TARGET_MS=10` are the knobs (xnuhost's environment, which the helper inherits; no `xnuhost.conf` key); not
    made the default, because the server's mixer period on this phone is not measured (the burst of 1600 is the
    track's notification period, not the mixer's) and an underrun is worse than 30 ms.
  - the stream kept open in silence: the linger is 60 s now (above). A sound after a longer pause pays the open
    and the new track's first 200 ms once.
  - the latency iOS is told (`oneios-audio-latency`, 6480 frames = 135 ms): it matches the 131-142 ms measured; kept.
  - iOS's own buffer: system sounds use a 3072-frame I/O buffer (64 ms) of iOS's choosing; not changed.

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
- Volume: iOS's volume keys scale the samples in iOS (VirtualAudio's software volume [I from iOSPlus10]); the
  Android media stream stays at One UI's last volume (14 of 15 on 2026-11-12). Nothing turns it down for a session.
- Latency 130 ms: AAudio's level plus the normal mixer (the section above). A FAST track would be lower but needs the
  realtime path above.
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
| xnuhost: the helper's socket and ring, the `oneios,camera` virtio device (`/arm-io/camera`, `XL_DEV_CAMERA`) | `runtime/bridges/camera` (`libonecamera.a`), `runtime/xnuhost/src/main.c` | linked into xnuhost; the device tested on xnuhost's transport against a guest of plain memory |
| Kernel: the transport and `/dev/oneios-camera` (the `NCFRAME1` layout of iOSPlus10's and HakiOS's node, NV12); `AppleH9CamIn` and its user client (the same session) | `drivers/camera` | compiles with the kernel's records, in the `HOSTED_OUT=cam` kernel; 37 host checks of the pure parts; on the phone (2026-11-12) the transport came up with 3 cameras; the service not run |
| Kernel: the plug-in's folder over `/System/Library/MediaCapture` | `kernel/patches/oneios-hosted/0056-camera-plugin-mount.patch` | built; not run |
| Plug-in `H9ISP.mediacapture`: `H9ISPCaptureDeviceCreate`, a FigCaptureDevice with the back and front streams, formats from the device's own `AVCaptureSession.plist`, frames scaled into the graph's pools | `userland/camera` | built with Xcode (arm64e, `macho-check.py` PASS); on the Mac macOS's own `BWFigCaptureDeviceVendor` makes a device with it and a frame reaches an output's handler (35 checks); not run in iOS; placed in the seed by the image hook (a new seed) |

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
  path the launch hangs.

### Missing, unknown

- **The first session with the plug-in** (`userland/camera/README.md`, "What the next session's logs must show"):
  whether the 27 capture graph takes the device as it is, and which properties it asks for that are not answered.
  The device tree nodes for MobileGestalt and the camera permission are after that.
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
- The front light sensor is on the sensor hub (SSC), reachable only through the sensors HAL: not used.

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
  touch screen and the buttons.

### The bridge (`runtime/bridges/brightness`)

- Route: the backlight sysfs node as root (no system_server, no surfaceflinger, no binder). Slider squared over
  `1..ceiling`; never 0; One UI's value written back at the stop.
- `als_rear` read by evdev without a grab; clear counts x `ONEIOS_ALS_SCALE` millilux.

### Missing, unknown

- [U] everything on the phone: the node's name and range, writing it in the session's namespace, the sensor's
  `enable` file, the counts in known light.
- [U] everything in iOS: no session reaches backboardd yet.
- The HID light sensor device is built and starts (`ONEIOS ALS: on` in a PC boot) and `AppleUSBALSService`
  attaches to it; no reading has gone through, and whether CoreBrightness uses its events is [U]. A rear sensor is a poor
  input for auto-brightness in any case (dark when the phone lies face up).
- iOS's `IOMFBBrightnessLevel` on the framebuffer is not handled here (the display driver is UniXNU-droidvm's).

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

1. xnuhost (`runtime/xnuhost/src/input.c`) opens every event device that has multitouch positions or one of the
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
- No pen, no trackpad, no motion sensors (the transport's code for them is compiled and finds no such device).

## 7c. Battery (battery bridge, 2026-11-11)

Details and the phone test: `runtime/bridges/battery/README.md`, `drivers/battery/README.md`.

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
  context has the boot overlay (backboardd's boot UI) or is dark. Phone, 2026-11-12 23:45 (on the charger, Full): the
  "plugged in" sound played before the lock screen. [I] SpringBoard found no boot overlay (it started at +38 s, the
  first frame came at +48.8 s); not proven: the iOS log lines that decide it are in that README.

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
written: the kernel's junction trips, LMh and xnuhost's thermal step are the session's caps. Tested against a fake
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
| The owner's phone | board notes of 2026-11-10 in `docs/agents.md` (shell and root reads) |

Only single files were fetched from the kernel and device repositories (raw downloads at the pinned commit) and one
shallow clone of the 748 KiB devicetrees repository; nothing of them is in this repository.
