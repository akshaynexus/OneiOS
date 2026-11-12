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
| Wi-Fi / network | **none in the hosted kernel** (UniXNU-droidvm's virtio-net and DroidVMWifi.inc sources are not built into it; section 3) | design of 2026-11-12: iOS drives the vendor's supplicant (AIDL V2) and nl80211, frames by AF_PACKET on wlan0 (`runtime/bridges/wifi`, being written); old tap/NAT route (`runtime/bridges/net`) not used | iOS has had no en0 in any boot; the phone had no network to join on 2026-11-12 |
| Bluetooth | `drivers/bluetooth` (port of iOSPlus10's transport) on `drivers/bridge` | `runtime/bridges/bt`: the HAL over `/dev/hwbinder` | both built; on by default in a session since 2026-11-11 (before: off, `bluetooth=virtual`); HAL client tested on the PC against a fake HAL on a real binder driver, not yet on the phone |
| Haptics | `drivers/haptics` (port of iOSPlus10's LaurelVibrator and haptic node) | `runtime/bridges/haptics`: evdev force feedback | both built; host tested with a fake actuator |
| Audio | `DroidVMVirtioSnd.c` + `DroidVMAudio.cpp`: the hosted kernel's audio archive since 2026-11-12 (before: not built, no device in iOS) | `devices/virtio_snd.c`, `audio/aaudio_helper.c` (`oneios-audio`) | helper run on the phone beside One UI (plays and records, 0 underruns, about 130 ms); host-tested against a fake AAudio; in a session and the guest side not yet run (section 6) |
| Camera | `drivers/camera`: the `oneios,camera` transport, `/dev/oneios-camera`; `userland/camera`: the capture plug-in (its device object not written) | `runtime/bridges/camera`: helper `oneios-camera` (provider HAL in a session, Camera2 NDK with One UI), ring, device, `camctl` | host and kernel sides built and host-tested; iOS shows no camera yet; one phone run (2026-11-11): cameraserver lists 4 cameras and opens one for a root client, no frame arrived (no binder threads in the process: fixed, the fix not yet run on the phone); the HAL back end never run (section 7) |
| Backlight and light sensor | `drivers/brightness`: the backlight service (port of iOSPlus10's `AppleARMBacklight`), the light readings; the HID light sensor device, built into the hosted kernel's HID archive | `runtime/bridges/brightness`: the backlight sysfs node, `als_rear` by evdev, `brightctl` | both built; host tested; in a PC boot the HID device starts and Apple's `AppleUSBALSService` attaches to it; never run on the phone or in a session (section 7a) |
| Battery | `drivers/battery`: an `IOPMPowerSource` powerd reads | `runtime/bridges/battery`: Android's `power_supply` nodes, polled, changes sent over the bridge channel; `batteryctl` | both built; host tested with a fake sysfs; never run on the phone or in a session (section 7c) |
| Thermal | `drivers/thermal`: `OneiOSThermal` (the readings and the level in the registry); `userland/helpers/oneios-thermal` sets iOS's `com.apple.system.thermalpressurelevel` from it | `runtime/bridges/thermal`: the thermal zones, Samsung's thermistors, the battery's sensors; a level on iOS's scale; xnuhost's CPU step when hot; `thermalctl` | host built and tested; `thermalctl` run on the phone (read only); the step, the driver and the helper not yet run (section 7d) |
| Touch screen and buttons | `kernel/patches/oneios-hosted/hid`: UniXNU's NativeHidInput (digitizer, buttons) under the collection's `IOHIDDevice`, UniXNU-droidvm's virtio-input transport with the side keys | xnuhost's `input.c` (evdev, no grab) and `virtio_input.c`; `input_script.c` for a host with no panel | built; in a PC boot backboardd opens both services and scripted touches and keys are queued for it as HID events; never run on the phone (section 7b) |

Common pieces:

- **The bridge channel** (`drivers/bridge/oneios_bridge_proto.h`): one virtio-mmio device, `/arm-io/bridge`, compatible
  `oneios,bridge`, carrying small messages (haptic requests, HCI packets). Guest end `drivers/bridge/OneiOSBridge.c`,
  host end `runtime/bridges/devices/bridge_dev.c`. Devices that already have a virtio driver in UniXNU-droidvm
  (network, sound) use standard virtio devices instead.
- **Tests**: `runtime/bridges/tests/host.sh` (CI runs it): 198 checks (and 48 in `brightness/`, 57 in `battery/`) with ASan and UBSan, the virtio devices driven
  through xnuhost's real transport (`runtime/xnuhost/src/virtio.c`) by a guest made of plain memory; then the Android
  build. `drivers/build.py --check` compiles the kernel sources with the kernel build's own compile records
  (xnu-12377, arm64e, `-Wall -Wextra -Werror`); the archive's 292 undefined symbols all resolve in `xnu.macho`.
- **On the phone**: `runtime/bridges/build/android/bridgectl` (static) tries each bridge without xnuhost:
  `bridgectl cpu|haptic|haptic-pattern|bt|net|audio`. It is the first thing to run when the phone is attached.

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

Design changed on 2026-11-12 (owner): **no NAT**. Wi-Fi works the way a Treble GSI gets it: in a session iOS is the
only OS that uses the radio, and it drives the vendor's own Wi-Fi stack. The tap, the private subnet and
`oneios-net.sh` are the old route; they stay in the tree until the new one runs, and are not used by it.

### The phone [F, read on the owner's phone 2026-11-12 unless marked]

- Chip: Qualcomm WCN785x / FastConnect 7800 ("kiwi"): DT node `compatible = "qcom,cnss-kiwi"`, `supported-ids =
  <0x1107>` (`samsung/e3q_eur_openx_w00_r09.dts` line 17994); modules `qca_cld3_kiwi_v2` + `cnss2` (`ethtool -i
  wlan0`: driver `cnss_pci`); firmware `vendor/firmware/kiwi/` ([android_device_samsung_e3q@fb2ba30](https://github.com/samsung-sm8650/android_device_samsung_e3q/blob/fb2ba3010d59c0f38ddf81aef7fd9c78e2172860/proprietary-files.txt)).
- `wlan0`: ifindex 44, MTU 1500. Offloads (`ethtool -k`): GRO on (can be changed), TSO and TX scatter-gather on and
  fixed, RX checksum on and fixed, LRO off and fixed. While One UI is not joined to a network, `disable_ipv6` is 1
  on wlan0 and the interface has no address.
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
- The supplicant's **AIDL service answers root**: `service call android.hardware.wifi.supplicant.ISupplicant/default 3`
  (`getDebugLevel`) returned 3, and `getInterfaceHash` the hash above, both from the `ksu` domain with One UI running.
- No SIM in the phone on 2026-11-12 (`gsm.sim.state` ABSENT), mobile data off. One UI's Wi-Fi was on and joined to no
  network (its last network, a phone hotspot, left on 2026-11-11 15:36). So that day the phone had no internet to
  give iOS whichever route is taken.

### The design: iOS drives the vendor stack

```
 iOS: wifid, Settings, configd (DHCP, ARP, IPv6, DNS)
   |  Apple80211 ioctls (SIOCGA80211/SIOCSA80211) on en0, IEEE 802.11 kernel events
 guest kernel: IO80211Controller/IO80211Interface (DroidVMWifi.inc's classes) with a host-backed station,
               frames on the virtio-net transport
   |  station requests and events: a channel of the bridge device; frames: virtio-net
 xnuhost: runtime/bridges/wifi
   control: ISupplicant V2 (AIDL over /dev/binder): getStaInterface(wlan0), addNetwork, set*, select,
            disconnect, removeNetwork, getSignalPollResults; callbacks for state changes and disconnects
   scans and signal: nl80211 on wlan0 (TRIGGER_SCAN, GET_SCAN, GET_STATION), as wificond does
   frames: AF_PACKET on wlan0 (ETH_P_ALL, PACKET_IGNORE_OUTGOING), the guest's MAC = wlan0's
 One UI's kernel: cfg80211 + qca_cld3, the vendor's wpa_supplicant (keys, 4-way handshake, rekeys)
```

1. **Control.** The supplicant keeps doing what only it can do on this chip (WPA2/WPA3 handshakes, PMF, rekeys,
   roaming inside an ESS). The bridge is its only client in a session, as the framework is in One UI:
   - join: `addNetwork`, then on the network `setSsid`, `setKeyMgmt` (NONE, WPA_PSK, SAE), `setProto`,
     `setPairwiseCipher`, `setGroupCipher`, `setPskPassphrase` or `setSaePassword`, `setRequirePmf` for SAE, `select`;
     the outcome from the interface callback (`onStateChanged` to COMPLETED, `onDisconnected`,
     `onAuthenticationTimeout`, `onAssociationRejected` with its status code) and nl80211's station.
   - leave: `disconnect`, `removeNetwork`.
   - scan: nl80211 `TRIGGER_SCAN` on wlan0 and `GET_SCAN` after `NEW_SCAN_RESULTS`: BSSID, frequency, signal (mBm),
     capability, beacon interval, the information elements (SSID, rates, RSN, ...) that iOS's scan record carries
     as they are. The supplicant sees the same results (it listens to the same nl80211 events).
   - signal: `getSignalPollResults` (RSSI, link speeds, frequency) or nl80211 `GET_STATION`, every 2 s while joined;
     iOS gets RSSI_CHANGED events and the values for its 16 RSSI / 8 RATE / 4 CHANNEL requests.
   - iOS keeps its own known networks (wifid's store in the Data volume); the supplicant holds only the one network
     iOS is joining or joined to.
   - At the start of a session the bridge disconnects the supplicant and removes the framework's networks from it, so
     the supplicant does not rejoin One UI's network on its own; One UI's framework adds its networks again when it
     starts (it does that after every framework restart).
2. **Data.** One IP stack on the link: iOS's. The bridge opens `AF_PACKET`/`SOCK_RAW`/`ETH_P_ALL` bound to wlan0,
   with `PACKET_IGNORE_OUTGOING` and `PACKET_VNET_HDR`. The guest's en0 has wlan0's MAC (a station can send only
   with its own address). Frames from the guest go out as they are; frames to wlan0's MAC, broadcast and multicast
   come in. EAPOL (0x888e) is not passed either way: it is the supplicant's. GRO is turned off on wlan0 for the
   session (`ETHTOOL_SGRO`); a coalesced TCP frame that still arrives (its `virtio_net_hdr` says GSO) is cut back
   into MTU-sized segments with fresh IPv4/TCP checksums. The Linux side keeps quiet on wlan0: its IPv4 addresses
   are removed (`ip addr flush dev wlan0`), `disable_ipv6` 1, `arp_ignore` 8, `ip_forward` stays as it is (0 on
   the phone: a frame for iOS's address is dropped by Linux, not forwarded or answered). netd, which keeps running,
   is not asked for anything.
3. **Return.** The bridge's stop: `disconnect` and `removeNetwork` for its network, its supplicant callback
   unregistered by closing the binder (the supplicant drops a dead client's callbacks), GRO and the three sysctls
   back to the values it read at the start, wlan0 left up with no address. One UI's framework then sets wlan0 up
   itself when it starts: IpClient enables IPv6 and runs DHCP, the framework re-adds its saved networks and joins
   (to be checked on the phone: the step "One UI's Wi-Fi joins again by itself").
   **After a SIGKILL** of xnuhost nothing of xnuhost runs: the bridge writes what it changed and the old values to
   `/data/session/wifi.state` before it changes anything, and `bin/oneios-wifi.sh restore` (runtime package, no
   xnuhost needed) puts them back and disconnects the supplicant by the AIDL tool `wifictl disconnect`. The fork's
   return must run it after its SIGKILL step and before it starts zygote (oneos: `oneios/interface.md`); the
   return's netd restart then rebuilds netd's routes and rules anyway.
4. **Wi-Fi off in One UI at the switch** [U, not designed further yet]: then the chip is stopped and wlan0 and the
   supplicant's interface may not exist. The bridge would have to do the framework's start sequence: IWifi
   `start`, the chip's mode, `createStaIface`, the supplicant's `addStaInterface("wlan0")`; and the reverse at the
   return. First version: the bridge works only when One UI's Wi-Fi was on at the switch, and says so in one line.

### What wifid needs from the interface (Apple80211)

From DroidVMWifi.inc (UniXNU-droidvm f9121cd, written for iPadOS 27 24A446 and shown in Settings by Droid-VM on
2026-11-02 with `droidvm-wifi-ssid=`) and iOSPlus10's 20H392 trace (`patches/xnu/README-wifi.md`, "How wifid uses
Apple80211"; iOS 16, so the iOS 27 details are [I] until seen):

- Found as Wi-Fi only if: the controller's class is `IO80211Controller` (configd), the interface is `IFT_ETHER` with
  `IFNET_SUBFAMILY_WIFI` (IO80211.framework), it has the property `IOInterfaceName` (wifid matches on it), and the
  device tree has `/arm-io/wlan` (wifid's launchd plist has `_LimitLoadToDeviceTree=wlan`; MobileGestalt's `wifi`
  reads it too). A refused `APPLE80211_IOC_FILTER_MONITOR_EVENTS` makes wifid give up the device: accept it.
- Requests (type numbers): 1 SSID, 2 AUTH_TYPE, 4 CHANNEL, 5 POWERSAVE, 8 RATE, 9 BSSID, 10 SCAN_REQ, 11 SCAN_RESULT
  (1164-byte records, EIO after the last), 12 CARD_CAPABILITIES, 13 STATE, 14 PHY_MODE, 15 OP_MODE, 16 RSSI, 17 NOISE,
  19 POWER, 20 ASSOCIATE (592 bytes: SSID, BSSID, cipher, key or passphrase), 21 ASSOCIATE_RESULT (0 while the join
  runs, 1 success, polled every 20 ms for 8 s), 22 DISASSOCIATE, 27/254 SUPPORTED_CHANNELS, 51 COUNTRY_CODE, 86
  SCAN_REQ_MULTIPLE, 103 CURRENT_NETWORK, 106 RSSI_BOUNDS, 207 CHANNELS_INFO; many sets accepted with no effect
  (383 SET_PROPERTY, 365 DISPLAY_STATE, 25 HOST_AP_MODE refused).
- Events (kernel event class 6, subclass 1, the interface name first): 1 POWER_CHANGED, 2 SSID_CHANGED, 3
  BSSID_CHANGED, 4 LINK_CHANGED (32 bytes: down, RSSI, SNR, noise, reason), 9 ASSOC_DONE, 10 SCAN_DONE; and the
  link state on the IOKit side (`setLinkStatus`, which configd's DHCP waits for).
- So the guest driver is DroidVMWifi.inc with its station replaced by the host: scan and join requests go to the
  bridge, scan records and state come back, and the controller sends and receives frames (DroidVMWifi.inc's
  controller has no transport and drops them).

### iOS side today: no network driver in the hosted kernel [F, 2026-11-12]

The hosted kernel (`out/kernel/current-hosted/xnu.macho` of 2026-11-12) has **no network driver at all**: no
`DroidVMVirtioEthernet`, `DroidVMVirtualWifi`, `IO80211Interface` or `droidvm_net_tx_frame`. UniXNU-droidvm's network
sources are built only by its own `ipados27/scripts/build-droidvm-devices.py`, which this kernel's build does not run,
so xnuhost's `/arm-io/net` device has had no driver in every boot so far and iOS had no en0 (the earlier text here,
"iOS side unchanged from UniXNU-droidvm", described the sources, not the kernel). What it takes:

- IONetworkingFamily. The collection keeps Apple's `com.apple.iokit.IONetworkingFamily` as a root (with
  `IOSkywalkFamily`, `IONetworkFamily`, `IOUserEthernet`, `usb.networking`), so `IOEthernetController` is Apple's
  original class in a kext. Two ways: (a) subclass the original, with an ABI header of its vtables checked against
  the kernelcache (as `kernel/patches/oneios-hosted/hid` does for `IOHIDDevice`; IONetworkController and
  IONetworkInterface have many virtuals); or (b) as Droid-VM does on the same 24A446 kernelcache: compile
  IONetworkingFamily-188 into the kernel and keep Apple's copy and its dependents out of the collection
  (`kernel/prelink/roots.py` `FORCE_OUT`). (b) is what ran in Droid-VM's iPadOS 27 guest. Neither is done; (b) needs
  the prelink's owner.
- A hosted archive (like `hid/`): DroidVMVirtioNet.c (transport), the Wi-Fi controller with the host station and the
  transport, and DroidVMVirtioNetIf.cpp (the late start of IONetworkStack); no Ethernet controller (the iPhone's en0 is
  its Wi-Fi). Plus `/arm-io/wlan` in booter's device tree.

### The host bridge

- New (being written, 2026-11-12, `runtime/bridges/wifi`): the data path and the control path above, and
  `bin/oneios-wifi.sh restore` / `wifictl`. Not yet in the tree when this design was written.
- Old (not used by the new route; kept until it runs): `runtime/bridges/net/net.c` (tap `oneios0`, DHCP answered in
  the bridge: 192.168.77.2/24), `runtime/bridges/devices/virtio_net.c` (the virtio-net device; kept, it carries the
  frames of the new route too), `runtime/bridges/net/oneios-net.sh` (NAT). On the phone on 2026-11-12 xnuhost made
  the tap and `oneios-net.sh up` stopped at "no uplink: the phone has no default network" (bringup's session 1).
  The script's `ip rule del priority 16000` would have deleted netd's own rule of that priority (netd's
  `fwmark 0x10063/0x1ffff iif lo lookup local_network` is at 16000 on this phone) when ours was absent: a reason
  not to run it again as it is.

### Missing, unknown

- [U] What the vendor stack does when system_server stops: whether the supplicant stays up with wlan0, whether the
  Wi-Fi HAL or wificond tear the interface down when their client dies. Read in the first session with this route.
- [U] Whether the supplicant's callbacks reach our process (binder calls from `hal_wifi_supplicant_default` into
  `ksu`; KernelSU allows binder calls into its domain [I]).
- [U] One UI joining its network again after the return without help.
- Enterprise (802.1X) networks, hotspot (iOS's Personal Hotspot), AWDL/AirDrop: not in the first version.
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
- `hci.c`: packet length checks both ways, the state, the address file.

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
  default since 2026-11-11), `transport-encoding` 3 (H4BC), `transport-speed` 3000000 and the phone's
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
| Core Haptics continuous events (through the Actuator stream) | sine effects following the stream's level and frequency, 2 ms blocks |
| Core Haptics transients, UI and keyboard taps (Actuator stream, 3-20 ms bursts) | a short sine burst. [U] how sharp it feels; the ROM "click" waveforms (`FF_CUSTOM` `[1, index]`) are the better fit and are not used yet |
| Sharpness | frequency, within 100..250 Hz |

### Missing, unknown

- [U] the LRA's resonance (attribute `f0_measured` of class `sec_vib_inputff`): 6 ms (166 Hz) is a guess.
- [U] whether the buzz generator stops fast enough for taps; if not, map bursts under about 25 ms to ROM clicks.
  The table of ROM/RAM indices is Samsung's firmware (`cs40l26.wmfw`/`.bin` on the phone), not in the kernel source.
- The Actuator stream needs an audio HAL plug-in on the iOS side that writes the node (ios-topaz's RedwoodAudio, or
  an Actuator device added to `DroidVMAudio.cpp`): userland / release-kernel. Until then only the `vibrator` path
  works: ringtones and alerts yes, UI and keyboard taps no (same state as the Mi A3 on iOS 16).
- [U] iOS 27: `hertz_millisecs` is still in the iOS 26.5 MediaExperience [F iOSPlus10]; whether iPhone18,2's
  MobileGestalt answers closed-loop true from its own cache regardless of the device tree is not known.
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
they are the hosted build's audio archive, `kernel/patches/oneios-hosted/audio/build-oneios-audio.py`
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
    (an overflow: after a stall). The guest's STOP keeps the stream open, playing silence, for
    `ONEIOS_AUDIO_LINGER_S` (15 s): iOS stops and starts its device around each sound, and a newly opened
    deep-buffer track takes nothing for about 200 ms.
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
| Track | deep-buffer mixer track (AudioFlinger: flags 0x000, frame count 3200, `Latency 133 ms`), 48 kHz stereo, `mmap 0` |
| 12 s of a 440 Hz tone at -30 dBFS | 0 underruns, 0 xruns, 0 overflows, 70 slips; a first run had 3 overflows at the start (the track's stall) |
| Latency (written to presented, AAudio's timestamp) | 128-137 ms: AAudio 128-136 ms (our 67 ms buffer + the deep-buffer path), ring 1-5 ms |
| Second tone 0.5 s after a stop | the lingering stream: no new open, no stall, 0 underruns |
| Microphone, 2 s | 92,400 frames (96,000 expected; the stream's start), room noise RMS -52 dBFS, peak -41.5 dBFS |

### Missing, unknown

- **The guest side has not booted yet.** Built on the Mac (`HOSTED_OUT=audio`, series through 0039, `xnu.macho`
  `1279b87a...`; the archive compiles with `-Wall -Wextra -Werror` and `oneios_drivers_start` ends in a call of
  `oneios_audio_start`) and prelinked (XnuPrelink 9b950f2, `kernel.kc` `61d926d8...`, 97 originals on the stub, 10
  data unpaired: the same pairing as the Mac collection `eae11352` that panics 1.5 s in at Sandbox's
  `mac_posixsem_check_create`, `kernel/prelink/README.md` "On the phone"). It waits for the agent prelink's audit
  of the Mac pairing; the phone's baseline is the Linux-built `kernel.kc`, which has no audio archive.
- [U] **AAudio from uid 0 with system_server gone** (a session): not run yet. Playback: audioserver treats uids
  below 10000 as services and asks no app-op [I]; recording: audioserver's own checks pass root without
  system_server [I], whether the record app-op silences it is the open question.
- [U] Whether VirtualAudio of iPhone18,2 builds its speaker route on "Speaker" and "Mic" (UniXNU-droidvm's iPad
  UIDs). iOSPlus10's iOS 16 iPhone needed "Speaker" with a silent stereo input and "Hawking"; the UIDs are boot
  arguments now, a third device would be a change of the staged source.
- Volume: iOS's volume keys scale the samples in iOS (VirtualAudio's software volume [I from iOSPlus10]); the
  Android media stream stays at One UI's last volume (14 of 15 on 2026-11-12). Nothing turns it down for a session.
- Latency 130 ms: the deep-buffer output. A FAST track would be lower but needs the realtime path above.
- Routing: One UI's audio policy's default for a media stream (speaker; a wired or Bluetooth headset if connected
  [U]). The earpiece needs a voice-communication stream or AAudio's device id of the earpiece port: not done. Calls
  are out of scope (no baseband in the guest).
- The Actuator stream (Core Haptics taps) shares nothing with this path yet (section 5).

## 7. Camera (fable-camera, 2026-11-11)

Built on all three sides; iOS shows no camera yet: the capture device object in the plug-in is not written. **One
phone run (2026-11-11, `camctl` with the NDK back end): the cameras are listed and one opens, no frame arrived; the
cause is fixed in the code and the fix has not run on the phone yet** ("The first phone run" below). Details:
`runtime/bridges/camera/README.md` (the host, the route, the phone commands), `drivers/camera/README.md`,
`userland/camera/README.md`.

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
| Kernel: the transport and `/dev/oneios-camera` (the `NCFRAME1` layout of iOSPlus10's and HakiOS's node, NV12) | `drivers/camera` | compiles with the kernel's records, links against `xnu.macho`; 37 host checks of the pure parts; not run |
| Plug-in: the node's reader, IOSurface-backed 420f pixel buffers, the report of what CMCapture 27 has, the vendor hook | `userland/camera` | reader host-tested (26 checks); the plug-in syntax-checked only (no SDK); **the FigCaptureDevice for CMCapture 27 is not written** |

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
- **What iOS needs** [F iOSPlus10 `docs/laurel_sprout/camera.md` section 3, `docs/camera-plan.md`]: Apple's path
  (`AppleH1xCamIn`, its ISP firmware protocol) cannot be served by another camera, so no IOKit stand-in: a plug-in in
  the capture server (`cameracaptured` on iOS 27 [I]) replaces `+[BWFigCaptureDeviceVendor
  sharedCaptureDeviceVendor]` and makes a FigCaptureDevice whose stream hands CVPixelBuffers to the capture graph.
  The app needs, in order: the capture servers running, a FigCaptureDevice with a steady preview stream, a still
  from the same stream, and only then the Camera icon's device tree nodes (`/arm-io/isp` `camera-rear`,
  `camera-front`; `/product/camera`): with the nodes and no frame path the launch hangs.

### Missing, unknown

- **The capture device object** (`oneios_camera_device_create`, `userland/camera/README.md`, "Not written"): needs
  the 24A446 shared cache on the build machine and a guest that reaches SpringBoard. Also there: how the plug-in is
  loaded into `cameracaptured`, the device tree nodes, the camera permission.
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
  `{brightness, FadePeriod}` and `brightness-nits` and sends `OB_BRIGHT_SET` on the bridge channel.
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

- `/dev/input/event5` the touchscreen (multitouch protocol B: slots, tracking ids, `ABS_MT_POSITION_X/Y`),
  `event0` gpio-keys, `event2` the power key, `event4` volume down (from the owner's phone; the axis ranges were
  not read [U]).
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
each a signal round trip on its thread. xnuhost now logs which instructions (`emulated: insn ...`, `docs/booter.md`
2.5) to find it.

### Missing, unknown

- Not run: the thermal step, the driver, the helper (the helpers are in no image yet: `docs/userland.md` 7).
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
