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
| Wi-Fi / network | `DroidVMVirtioNet.c` + `DroidVMVirtioEthernet.cpp` (unchanged) | `runtime/bridges/net` (tap, built-in DHCP), `devices/virtio_net.c`, `net/oneios-net.sh` | host built and tested; routing script not run on the phone |
| Bluetooth | `drivers/bluetooth` (port of iOSPlus10's transport) on `drivers/bridge` | `runtime/bridges/bt`: the HAL over `/dev/hwbinder` | both built; HAL client untested on the phone |
| Haptics | `drivers/haptics` (port of iOSPlus10's LaurelVibrator and haptic node) | `runtime/bridges/haptics`: evdev force feedback | both built; host tested with a fake actuator |
| Audio | `DroidVMVirtioSnd.c` + `DroidVMAudio.cpp` (unchanged) | `devices/virtio_snd.c`, `audio/aaudio_helper.c` (`oneios-audio`) | host built and tested with a fake back end; AAudio as root without system_server is [U] |
| Camera | design only (section 7) | design only | not built |

Common pieces:

- **The bridge channel** (`drivers/bridge/oneios_bridge_proto.h`): one virtio-mmio device, `/arm-io/bridge`, compatible
  `oneios,bridge`, carrying small messages (haptic requests, HCI packets). Guest end `drivers/bridge/OneiOSBridge.c`,
  host end `runtime/bridges/devices/bridge_dev.c`. Devices that already have a virtio driver in UniXNU-droidvm
  (network, sound) use standard virtio devices instead.
- **Tests**: `runtime/bridges/tests/host.sh` (CI runs it): 170 checks with ASan and UBSan, the virtio devices driven
  through xnuhost's real transport (`runtime/xnuhost/src/virtio.c`) by a guest made of plain memory; then the Android
  build. `drivers/build.py --check` compiles the nine kernel sources with the kernel build's own compile records
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

## 3. Wi-Fi and network

### The phone [F]

- Chip: Qualcomm WCN785x / FastConnect 7800 ("kiwi", "hamilton" on the Bluetooth side): DT node
  `compatible = "qcom,cnss-kiwi"`, `supported-ids = <0x1107>` (`samsung/e3q_eur_openx_w00_r09.dts` line 17994, same
  devicetrees commit); modules `qca_cld3_kiwi_v2` + `cnss2`; firmware `vendor/firmware/kiwi/{amss20.bin,bdwlan.elf*,
  phy_ucode20.elf,regdb.bin}`
  ([android_device_samsung_e3q@fb2ba30](https://github.com/samsung-sm8650/android_device_samsung_e3q/blob/fb2ba3010d59c0f38ddf81aef7fd9c78e2172860/proprietary-files.txt)).
  HALs running: `wifi_hal_legacy`, `wifi_hal_ext`; wpa_supplicant is an init service.
- In a session the radio stays associated: OneOS's switch stops only zygote and system_server.

### iOS side (unchanged from UniXNU-droidvm f9121cd)

`package/DroidVMVirtioNet.c` (virtio-net transport: VERSION_1 + MAC, 12-byte header, receive interrupt),
`DroidVMVirtioEthernet.cpp` + `DroidVMVirtioNetIf.cpp` (an `IOEthernetController` on IONetworkingFamily: `en0`, DHCP by
iOS itself), node `/arm-io/net`, compatible `droidvm,net`. `DroidVMWifi.inc` gives wifid and Settings a Wi-Fi
interface with no radio (`droidvm-wifi-ssid=NAME` shows one joined network).

### The bridge

- `runtime/bridges/devices/virtio_net.c`: the device xnuhost puts at `/arm-io/net`.
- `runtime/bridges/net/net.c`: `br_net_open_tap` opens `/dev/tun` (`IFF_TAP | IFF_NO_PI`, name `oneios0`) and moves
  frames both ways. **DHCP is answered in the bridge** (`br_dhcp_reply`): no DHCP server runs in a session (Android's
  is in the network stack app), so the guest gets 192.168.77.2/24, router 192.168.77.1 and the configured DNS
  servers straight from the device. Everything else goes to the tap.
- `runtime/bridges/net/oneios-net.sh up|down|status`: address on the tap, `ip_forward`, two policy rules
  (`iif oneios0 lookup <uplink table>`, priority 16010; `to 192.168.77.0/24 lookup main`, priority 16000: netd's rules
  never consult `main`), an own `oneios_fwd` chain first in `FORWARD` (netd's `tetherctrl_FORWARD` drops what it does
  not know) and a `MASQUERADE` rule in an own nat chain. `down` removes exactly these. One UI's own rules are not
  edited.

### Missing, unknown

- [U] every line of `oneios-net.sh` on One UI 9 (netd's table numbering `1000 + ifindex`, the chain order).
- [U] **the lease of One UI's own Wi-Fi**: its DHCP client is in the network stack app, which is a zygote child and
  gone. The address stays on `wlan0`, but nobody renews the lease; a session longer than the lease may lose the
  uplink. A small renew helper (or keeping that one process) is open; ask oneos.
- DNS: the bridge hands out 8.8.8.8 and 1.1.1.1 unless booter sets `br_net_config.dns` (the phone's resolvers are
  not readable without system_server [I]).
- IPv4 only. No IPv6, no inbound connections, no mDNS/AirDrop/AirPlay to the LAN (the guest is behind NAT).
- Wi-Fi state in iOS (SSID, signal bars, scan list): `DroidVMWifi.inc` shows a fixed name. A bridge from
  wpa_supplicant's control socket to that station is not written.
- Overlay mode (no root): user-mode NAT is not written; the tap needs root.

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
  notification turns the state to failed.
- `hidl_parcel.c`: the parcels (interface token, `hidl_string`, `hidl_vec<uint8_t>` as scatter-gather buffer
  objects, strong binders). Unit-tested byte for byte; the transaction codes are the HIDL order of the methods.
- `hci.c`: packet length checks both ways, the state, the address file.

### Missing, unknown

- [U] **SELinux**: hwservicemanager checks `find` for the caller's context, and the HAL's domain must be allowed to
  call back into ours (`binder { call transfer }`). KernelSU's domain is allowed most things; if the callback is
  denied, a `ksud sepolicy` rule is needed. `bridgectl bt` shows which step fails.
- [U] the transaction codes and the callback handshake against the real HAL (written from the interface
  definitions, never run). First test: `bridgectl bt` must print two Command Complete events.
- booter: the tree's node. Today `runtime/loader/src/machine.c` publishes `/bluetooth` with `transport-encoding` 10
  (the virtual transport of Apple's VMs) and `/defaults/vmm-present` 1. For a real controller iOSPlus10 used
  `transport-encoding` 3 (H4BC), `transport-speed` 3000000 and the real `local-mac-address` (`br_bt_address`). Which
  of the two bluetoothd 27 takes with `vmm-present` set is [U]; make it a switch.
- userland: the bluetoothd job on, BlueTool's firmware step skipped (iOSPlus10 `userland/bluetooth/negroni-bluetool.c`,
  `scripts/bt/bt-launchd.py`), `BTAudioHALPlugin.driver` kept in HAL, bluetoothd's iAP PTY made non-blocking
  (iOSPlus10 `docs/laurel_sprout/bluetooth.md`, last section).
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

### iOS side (unchanged from UniXNU-droidvm f9121cd)

`package/DroidVMAudio.cpp`: two `IOAudio2Device`s, UIDs "Speaker" and "Mic", which VirtualAudio turns into routable
ports; `DroidVMVirtioSnd.c`: virtio-snd transport, node `/arm-io/virtio-snd`, compatible `droidvm,virtio-snd`,
48 kHz S16, stereo out, mono in, periods of 240 frames, polling. Both transports compile with our records.

### The bridge

- `runtime/bridges/devices/virtio_snd.c`: the device (control queue: `PCM_INFO`, `SET_PARAMS`, `PREPARE`, `START`,
  `STOP`, `RELEASE`; two streams; output periods go to the back end as they arrive; a capture thread fills the
  receive buffers the guest posted).
- `runtime/bridges/audio/aaudio_helper.c` -> `bin/oneios-audio`: **a helper process**, because AAudio is a shared
  library and xnuhost is static. xnuhost starts it (`br_audio_helper(path, &ctx)`, `posix_spawn`, a `SOCK_SEQPACKET`
  pair); it plays through an AAudio low-latency stream fed from a 200 ms ring (the guest's clock and the DAC's
  differ: an empty ring plays silence, a full one drops the oldest audio) and sends the microphone's bursts back.
- Why AAudio and not tinyalsa: on this SoC a PCM device plays only after the AudioReach graph is set up through the
  AGM plug-ins and PAL, which the audio HAL does; opening the card beside it would also fight audioserver.

### Missing, unknown

- [U] **AAudio from uid 0 with system_server gone.** Playback: audioserver treats uids below 10000 as services and
  asks no app-op [I from AOSP's audioserver]; audio policy lives in audioserver. Recording: the permission check for
  `RECORD_AUDIO` normally goes to system_server; whether uid 0 passes is unknown. `bridgectl audio` plays a tone.
- [U] volume: One UI's stream volume was set by AudioService (gone). The stream plays at the device's last media
  volume; iOS's volume keys change the guest's digital level only.
- booter: `runtime/build.sh` must put `oneios-audio` into the runtime package (`bin/oneios-audio`, mode 0755) and
  start it before the vCPUs run.
- Routing (speaker, receiver, wired or Bluetooth headset) is One UI's audio policy's default for a media stream.
  Calls are out of scope (no baseband in the guest).

## 7. Camera (design only)

### The phone [F]

- Sensors (module files in
  [android_device_samsung_sm8650-common@48c66f6](https://github.com/samsung-sm8650/android_device_samsung_sm8650-common/blob/48c66f6543b31784fce4e7d23ea855897f18ce6e/proprietary-files.txt),
  slots 0, 1, 2, 3, 6, 12 in the DT, `qcom,cam-sensor*`, lines 15571-16040):

  | Slot | Module file | Sensor | Role [I from the slot names] |
  |---|---|---|---|
  | 0 | `com.samsung.sensormodule.0_lsi_s5khp2.bin` | Samsung S5KHP2 (200 MP) | rear wide |
  | 1, 12 | `1_lsi_s5k3lu.bin`, `12_lsi_s5k3lu_full.bin` | Samsung S5K3LU (12 MP) | front |
  | 2 | `2_sony_imx564.bin` | Sony IMX564 (12 MP) | rear ultra-wide |
  | 3 | `3_sony_imx754_x3.bin` | Sony IMX754 (10 MP) | rear 3x |
  | 6 | `6_sony_imx854.bin` | Sony IMX854 (50 MP) | rear 5x |

- Services running in a session: the provider HAL `vendor.samsung.hardware.camera.provider`
  (`ICameraProvider/internal/0`), `media.camera` (cameraserver), and the vendor-facing
  `android.frameworks.cameraservice.service.ICameraService/default`.

### What iOS needs [F iOSPlus10 `docs/laurel_sprout/camera.md` section 3, `docs/camera-plan.md`]

Apple's path (`AppleH1xCamIn`, its ISP firmware protocol) cannot be served by another camera. The path that has
worked on other ports (HakiOS, negroni, the Mi A3): a CFPlugIn in the media server replaces
`+[BWFigCaptureDeviceVendor sharedCaptureDeviceVendor]` and a few more CMCapture methods and makes a
FigCaptureDevice whose stream reads frames from a kernel device (`NCFRAME1`: a 120-byte header and the frame) and
hands CVPixelBuffers to the capture graph. The app needs, in order: the Camera icon (`/arm-io/isp` `camera-rear`,
`/product/camera`; only when a frame path exists, else the launch hangs), the capture servers running, a
FigCaptureDevice with a steady preview stream, a still from the same stream.

### A minimal bridge

- Host: a second helper process (`oneios-camera`, dynamic: `libcamera2ndk.so`, `libmediandk.so`): `ACameraManager`,
  one `ACameraDevice`, a repeating request into an `AImageReader` (YUV_420_888, 1920x1080 or 1280x720), a second
  reader for stills (JPEG or full-size YUV). The provider HAL does the ISP work, so the frames are finished NV12:
  **no CPU ISP in the guest**, unlike the raw-sensor ports.
- Transport: a frame is megabytes, so not the 4 KiB bridge channel. A virtio device whose receive buffers are whole
  frames (the guest posts page lists, the host copies one NV12 frame in), or a window of guest RAM both sides map.
  `OB_CH_CAMERA` is reserved for the control messages (open, mode, exposure, a frame-ready note).
- Guest kernel: a char device with the `NCFRAME1` ABI on top (so the existing plug-in's reader fits), or an
  IOSurface filled in the kernel.
- Guest user space (userland agent): the plug-in with a "frames are NV12" mode, its hook table re-found for the
  iPhone18,2 27.0.1 shared cache (the tables in iOSPlus10 are for 20H392); on iOS 17 and later the capture servers
  live in `cameracaptured`, not mediaserverd [I].

### Unknown

- [U] `ACameraManager` from a root process with system_server gone: cameraserver checks the camera permission and
  the app-op for the calling uid. The vendor-facing `ICameraService` (AIDL, made for native vendor clients) skips
  the app layer and is the fallback; it needs an AIDL binder client.
- [U] whether Samsung's provider serves the 200 MP sensor's binned modes to a plain Camera2 client as on One UI.

## 8. Sources

| What | Repository and commit |
|---|---|
| Stock device trees (decompiled): CPUs, haptics, audio, Wi-Fi, Bluetooth, camera slots | [samsung-sm8650/android_kernel_samsung_sm8650-devicetrees@2f6a973](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650-devicetrees/tree/2f6a9734e556f2f7680b0094bec4ad4671157f8b) (`samsung/e3q_eur_openx_w00_r09.dts`, `qcom/pineapple-v2.dts`) |
| Samsung's kernel source: CS40L26 and sec_vib_inputff drivers, `e3q_defconfig` | [samsung-sm8650/android_kernel_samsung_sm8650@809f7ed](https://github.com/samsung-sm8650/android_kernel_samsung_sm8650/tree/809f7edaad95be2f60ce09f577fe5104d8940214) |
| Common device tree: HAL binaries (Bluetooth HIDL 1.1, vibrator service, camera provider, sensor modules) | [samsung-sm8650/android_device_samsung_sm8650-common@48c66f6](https://github.com/samsung-sm8650/android_device_samsung_sm8650-common/tree/48c66f6543b31784fce4e7d23ea855897f18ce6e) |
| e3q device tree: Wi-Fi and Bluetooth firmware names | [samsung-sm8650/android_device_samsung_e3q@fb2ba30](https://github.com/samsung-sm8650/android_device_samsung_e3q/tree/fb2ba3010d59c0f38ddf81aef7fd9c78e2172860) |
| Other trees seen, not used: Samsung OSS builds with KernelSU, the Gunyah kernel | `YuzakiKokuban/android_kernel_samsung_sm8650` (7c7f8dd), `Andy312432/android_kernel_samsung_sm8650_gunyah` (bacbd93) |
| iOS-side ports | iOSPlus10 (local, read-only): `patches/laurel/xnu/2700-laurel-pmi632-haptics.patch`, `2600-laurel-bluetooth-wcn3990.patch`, `docs/laurel_sprout/{haptics,bluetooth,camera,audio}.md` |
| Guest drivers taken unchanged | UniXNU-droidvm f9121cd (`external/UniXNU-droidvm/package/`), `docs/references.md` |
| The owner's phone | board notes of 2026-11-10 in `docs/agents.md` (shell and root reads) |

Only single files were fetched from the kernel and device repositories (raw downloads at the pinned commit) and one
shallow clone of the 748 KiB devicetrees repository; nothing of them is in this repository.
