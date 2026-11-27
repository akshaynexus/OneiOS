# runtime/bridges: the host side of OneiOS's hardware

The crate `onebridges` (Rust, in the runtime's workspace `runtime/Cargo.toml`; `runtime/RUST.md`). What each device
is, on the phone and for iOS: `docs/hardware.md`. The kernel side (C, built into the hosted kernel): `drivers/`. The
camera (`camera/`, crate `onecamera`) and the video encoder (`encoder/`, crate `oneencoder`) are crates of their own
in this folder, with their own READMEs. The bridges were C until 2026-11-23 (branch `rust-runtime`): every C file
became a Rust module of the same name, every C test a Rust test with the same cases, every log line kept byte for
byte. **No phone run of the Rust bridges yet**: every phone result quoted below is of the C build.

```sh
cd runtime
cargo test -p onebridges                 # host tests; no phone, no Apple files (the binder ones need a binderfs, below)
cargo test -p onebridges --test hal_binder --test test_hidl_binder --test test_aidl_binder   # the binder tests alone
cargo run -p onebridges --bin bridgectl -- host                # a host tool on the PC (ONEIOS_HOST_PROPS=FILE: another phone)
tests/host.sh                            # the whole workspace, fmt, clippy, the qemu boots: CI runs it
./build.sh STAGE                         # the phone's binaries in STAGE/bin (NDK r28c; docs/building.md 4)
CARGO_TARGET_DIR=target/android-static cargo build --release --target aarch64-linux-android -p onebridges --bin brightctl \
  --config 'target.aarch64-linux-android.rustflags=["-C","target-feature=+crt-static"]'   # one tool alone, static, as build.sh
                                         # builds it: target/android-static/aarch64-linux-android/release/brightctl
```

Binaries (`Cargo.toml`): `bridgectl`, `wifictl`, `brightctl`, `batteryctl`, `thermalctl`, `sensorctl` (static, as `runtime/build.sh`
stages them), `oneios-audio` (dynamic: the NDK's libaaudio), `fake-hal` (the stand-in HALs of the tests and of
`tools/oneios-tool/tests/phone/hw-backend.sh`; host only).

| Module | What |
|---|---|
| `lib.rs` | the crate: one `pub mod` per folder; no dependency on xnuhost (xnuhost depends on it) |
| `log.rs` | the bridges' log (`br_log!`, `br_error!`/`br_info!`/`br_debug!`, 511-byte lines) on `xh_core::log`: one log, one sink xnuhost sets (`set_sink`), `Capture` for tests |
| `proto/` | the Rust mirrors of `drivers/bridge/oneios_bridge_proto.h` (`proto/bridge.rs`) and `drivers/haptics/oneios_actuator.h` (`proto/actuator.rs`); `proto/cheader.rs` reads the C headers and the tests check every constant, size and offset |
| `devices/` | the virtio devices on `xh_core::virtio`: `bridge_dev.rs` (`oneios,bridge`), `virtio_net.rs` (`droidvm,net`; a frame that finds the receive ring full waits for the guest's kick of it, up to 50 ms for the Wi-Fi station's own type and 20 ms for any other, then is dropped; never on the vCPU's own send; the receive function says whether the guest took the frame; since 2026-11-23), `virtio_snd.rs` (`droidvm,virtio-snd`); `test_devices.rs` |
| `hw/` | the backend layer: `hw-backend=samsung\|treble`, `hw-backend-<bridge>`, `hw-backend-fallback` (`hw_backend.rs`, pure: the settings, the choice; `hw_log.rs`: the `hw: <bridge>: backend ...` and `... treble: missing: ...` lines); `test_hw_backend.rs`. `docs/hardware.md` 7e: the audit table |
| `hw/host_device.rs` | the host phone and its defaults (`docs/hardware.md` 7h): the one table of known phones (S24 Ultra, S25 Ultra, S25, S26 Ultra, S26 Snapdragon, S26 Exynos, A56; an entry holds only existing options), detection from `ro.product.model`, `ro.product.device`, `ro.board.platform`, `ro.soc.model`, `ro.soc.manufacturer` (soc0 and the device tree as fallbacks; a getprop dump and a root folder for tests), the `host: ...` line; xnuhost's `host-device=auto\|none\|NAME` and the ctl tools' `ONEIOS_HOST_DEVICE`, `ONEIOS_HOST_PROPS`, `ONEIOS_HOST_ROOT`. Every ctl tool says the host first and takes the entry's environment defaults (`ONEIOS_BRIGHT_MAX` on the S26/S26 Ultra; a set variable wins); tested in `test_hw_backend.rs` |
| `hal/` | the treble backend's HAL client (`mod.rs`: a HAL by name from servicemanager on `/dev/binder`, its descriptor checked, the calls) on one binder layer for AIDL and HIDL (`binder.rs`, `aidl.rs`, `hidl.rs`; no Android library), the HALs' parcels both ways (`HealthInfo`, `Temperature[]`), `fake.rs` (a fake servicemanager with fake health, thermal and vibrator HALs whose values are files of a folder; the bin `fake-hal DIR`), `binderfs.rs` (a private binderfs in a user namespace for the binder tests) |
| `cpu/` | `topology.rs`: host topology from sysfs, the vCPU plan (E first, then P), thread placement, the cpufreq register window (`Cpufreq`, an `MmioDevice`) |
| `haptics/` | evdev force feedback (`sec_vibrator_inputff`) or, with `hw-backend-haptics=treble`, the vibrator HAL (`vibrator.rs`, `Haptic::open_as`), a worker thread, the iOS-to-effect mapping, the counters (`haptics: effect N played`, `haptics: counters:`) and the stand-in actuator of xnuhost's `haptics=sim` (`Haptic::open_sim`: a boot with no vibrator). Native impacts (off by default): with `ONEIOS_HAPTIC_NATIVE_IMPACTS=1` and TICK/CLICK/HEAVY_CLICK presets advertised, an explicit `OB_HAPTIC_IMPACT` plays the matching `IVibrator.perform()` preset (`docs/hardware.md` 5); the guest opts in with `oneios_native_impacts=1`; xnuhost's `haptics-native-impacts=1` turns on both halves for a session (`haptics::set_native_impacts`, docs/booter.md section 1) |
| `switches.rs` | the switches xnuhost sets once from its config (docs/switches.md): `wifi-ring-wait`, `wifi-send-wait`, `wifi-buffers`, `wifi-joined-scan`; one relaxed load where each acts |
| `net/` | the tap route, used by `wifi=tap` and `wifi=cell`: `net.rs` (the tap back end, the built-in DHCP answer, the `NetBackend` trait the virtio-net device and the Wi-Fi bridge share), `oneios-net.sh` (routing and NAT, root; a shell script; selector-safe rule deletion tested by `test-oneios-net.sh`) |
| `wifi/` | the phone's Wi-Fi for iOS (`docs/hardware.md` section 3): wlan0 is the guest's en0. `frames.rs` (which frames cross, coalesced frames cut), `station.rs` (the guest driver's station messages; `proto.rs` mirrors `drivers/net/oneios_wifi_proto.h`), `sim.rs` (a made-up radio, xnuhost's `wifi=sim`), `cell.rs` (made-up open network over the tap uplink, `wifi=cell`), `wlan.rs` (AF_PACKET, nl80211 scans and signal, wlan0's settings kept and given back; three threads), `chip.rs` (the Wi-Fi chip's HAL: starting it when One UI's Wi-Fi was off at the switch, and stopping it again at the return), `uplink.rs` (the guest's frames that find the send buffer full wait for `POLLOUT`), `traffic.rs` (the `wifi: 60 s:` line), `supplicant.rs` (joins through the vendor's wpa_supplicant over `hal::aidl`, reading `getInterfaceVersion`), `p2p.rs` and `wonder.rs` (`wifictl p2p`, `wifictl wonder`), `nl.rs` (the netlink attribute loops) |
| `bt/` | the Bluetooth HAL (HIDL 1.0/1.1) over `/dev/hwbinder` without Android libraries (`hci_hidl.rs`, `hidl_parcel.rs`), or, only when hwservicemanager has no HIDL HAL (the Galaxy A56), the stable AIDL `android.hardware.bluetooth.IBluetoothHci/default` over `/dev/binder` (`aidl_parcel.rs`, ported from the A56 fork's `26a2276`; `hci_hidl::probe` returns 20 then) (both: two threads: one answers the HAL's callbacks, one makes the calls from a queue, so a vCPU never waits on the HAL); packet checks (`hci.rs`); `hci_hidl::probe` for xnuhost's start; `hci_sim.rs`: the stand-in controller of `bluetooth=sim` (a boot with no phone): Command Complete with a 5.4 controller's return parameters for every command (Command Status for the ones answered that way), Unknown HCI Command for vendor commands, one made-up device ("OneiOS sim") heard by an LE scan, data dropped; its tests check its answers; `le_scan.rs` |
| `audio/` | the back end xnuhost uses (`backend.rs`: the `AudioBackend` trait, `AudioHelper::spawn`, `NullAudio`), the helper's protocol (`proto.rs`) and the AAudio helper process (`aaudio_helper.rs` behind AAudio traits, the bin `oneios_audio.rs` -> `oneios-audio`); `test_audio_helper.rs` against a fake AAudio |
| `brightness/` | the panel's backlight (sysfs, the curve, fades, restore), the light sensor `als_rear`, `brightctl` (the phone test); its own `README.md` |
| `sensors/` | the sensor hub (SSC) over Linux's QRTR, no Android framework: the accelerometer, gyroscope, magnetometer and the front light sensor (xnuhost's `sensors=auto\|ssc`, auto by default since 2026-11-25, `sensors=off` turns it off); a SEE client ported from ios-redfin (MIT); `sensorctl` (`qrtr`: read only, `stream`); its own `README.md` (the route, the HAL fallback, the CPU) |
| `battery/` | the phone's battery for iOS: Android's `power_supply` nodes read every 3 s, normalized, changes sent to the guest; `batteryctl` (the phone test); its own `README.md` |
| `thermal/` | temperatures and thermal steps, `thermalctl`; its own `README.md` |
| `camera/` | the camera (fable-camera), crate `onecamera`: the helper process `oneios-camera`, the frame ring, xnuhost's end and `camctl`; its own `README.md` |
| `encoder/` | the video encoder, crate `oneencoder`: `oneios-encoder`, `encctl`; its own `README.md` |
| `tools/bridgectl.rs` | tries each bridge on the phone without xnuhost (root) |
| `tools/wifictl.rs` | the Wi-Fi bridge on the phone without xnuhost (root): `status`, `scan`, `join`, `restore`, `p2p` (every radio's peer-to-peer paths and Google's Wonder radio: `wifi/p2p.rs`), `wonder` (a session on Wonder's raw 802.11 path beside wlan0, AWDL's frames counted: `wifi/wonder.rs`) |
| `tools/sensorctl.rs` | the sensor hub on the phone without xnuhost (root): `qrtr [--sensors]` (read only: QRTR's services, service 400, each sensor's SUIDs), `stream` (`sensors/README.md`) |
| `tests/` | the binder tests (`harness = false`), each on a binderfs mounted in a user namespace (`hal/binderfs.rs`; the PC kernel's `CONFIG_ANDROID_BINDERFS`; no root; skipped with a printed reason without it): `test_hidl_binder.rs` (the Bluetooth HAL client against a fake hwservicemanager and IBluetoothHci, and in the A56's layout against a fake servicemanager and AIDL IBluetoothHci on a second device of the same binderfs, `ONEIOS_BINDER`), `hal_binder.rs` (battery, thermal and haptics' treble backends against `fake-hal`), `test_aidl_binder.rs` (the supplicant client against a fake service manager and supplicant). A kernel whose `CONFIG_ANDROID_BINDER_DEVICES` is empty (Ubuntu's; the Mac's container kernel) mounts a binderfs with only `binder-control`: the tests ask it for the device by name (`BINDER_CTL_ADD`). The fake HAL retries `BINDER_SET_CONTEXT_MGR` while the last scenario's killed HAL still holds it (`EBUSY`, up to 2 s). On a Mac: `runtime/host-tests.sh`, in Apple's `container` with a kernel built with binderfs ([building.md](../../docs/building.md) section 4). The other tests sit beside their modules (`#[cfg(test)]`): the Wi-Fi bridge's frames, the nl80211 channel list reader, the DHCP reader (the REQUEST forms of RFC 2131 4.3.2, iOS's and another station's, and their log lines), station, parcels, join plan and callbacks, the Wonder session (`wifi/test_wonder.rs`) |

## Into xnuhost (bridges -> booter)

xnuhost depends on the crate (`runtime/xnuhost/Cargo.toml`) and makes the bridges in `runtime/xnuhost/src/session/
bridges.rs`; `runtime/build.sh` ships `oneios-audio`, `net/oneios-net.sh`, `wifictl` and the other ctl tools in the
runtime package's `bin/`. The calls, in the order the session makes them:

```rust
onebridges::log::set_sink(...);                           // optional: the bridges' lines into xnuhost's log

// CPU, before the device tree is made and the vCPUs start
let host = cpu::Topology::read("/sys")?;
let plan = cpu::CpuPlan::make(&host, ncpu, false)?;      // machine.cpu_cluster[i] from plan.v[i].kind
let cpufreq = Arc::new(cpu::Cpufreq::with_apply(&plan, Box::new(apply)));  // or Cpufreq::new(&plan, tid_of): it clamps itself
xh_core::mmio::bus().add(BOARD_CPUFREQ, ncpu * cpu::CPUFREQ_WINDOW, "cpufreq", cpufreq.clone())?;
// each vCPU thread and the iOS thread it runs: cpu::set_affinity(tid, mask); the clamp from plan.util_for(vcpu, cpufreq.requested(vcpu))

// One window and SPI each, as for the block device
let bridge = BridgeConfig { haptic: Haptic::open(None).map(Arc::new), hci_factory: Some(Box::new(hci_hidl::open_hidl)),
                            bt_address_path: None, brightness, battery, thermal };   // each an Option
devices::bridge_dev::create(base0, spi0, bridge);        // device tree: /arm-io/bridge, "oneios,bridge"

let (open, mac) = wifi::wlan::prepare("wlan0", Some(session_dir))?;   // or wifi::sim::prepare, or a tap (net::Net)
devices::virtio_net::create(base1, spi1, &net, Some(open));          // /arm-io/net

let audio: Arc<dyn AudioBackend> = match AudioHelper::spawn("/data/runtime/bin/oneios-audio") { Some(h) => Arc::new(h), None => Arc::new(NullAudio) };
devices::virtio_snd::create(base2, spi2, audio);         // /arm-io/virtio-snd
```

- Every create and open returns `None` (with a log line saying why) when its Linux side is missing; a missing device
  only turns that feature off. A `None` haptic or HCI factory is fine to pass on.
- Threads: each bridge has its own (haptic worker, tap receiver, binder looper, sound capture). They take the
  device's lock through `xh_core::virtio`, as xnuhost's input device does. None of them uses signals.
- The network: `wifi::wlan::prepare("wlan0", session_dir)` returns the back end's open and the guest's address;
  `wifi::wlan::close()` at the stop (the network left, the supplicant as it was found, wlan0's two settings back from
  `session_dir/wifi.state`).
- Stop: on `SIGTERM`, `Haptic::shutdown()` (the counters' line, the motor stopped), `Bright::shutdown()` (One UI's
  backlight level again, the touch and pen ICs awake, the light sensor as it was), `wifi::wlan::close()` (and
  `oneios-net.sh down` with `wifi=tap`). These are explicit calls, idempotent, because xnuhost's stop ends the process
  without dropping the bridge device that holds them (24b642e); closing the descriptors does the rest (the kernel
  erases a closed file's force-feedback effects; the audio helper exits when its socket closes).
- Everything is under `/dev`, `/sys`, `/proc/sys/net` and the session's own files: nothing of One UI's `/data` is
  touched. System state changed: wlan0's GRO and `disable_ipv6` and its IPv4 addresses, and the supplicant's
  networks (`docs/hardware.md` section 3, "Return": what is left after a kill, and why it does not break One UI's
  Wi-Fi). With `wifi=tap` instead: the tap, the policy rules and the two iptables chains, which `down` removes.

## On the phone

The passes quoted in this section were runs of the C tools (the same commands and the same lines); the Rust tools
have not been run on a phone yet.

```sh
runtime/build.sh STAGE                                       # from the checkout's root: STAGE/bin/bridgectl, oneios-audio, ...
adb push STAGE/bin/bridgectl STAGE/bin/oneios-audio /data/local/tmp/
adb shell su -c '/data/local/tmp/bridgectl host'           # read only: the properties, the table entry xnuhost would take, the table
adb shell su -c '/data/local/tmp/bridgectl cpu'
adb shell su -c '/data/local/tmp/bridgectl haptic-inspect'          # read only: the FF devices' name, bits, slots
adb shell su -c '/data/local/tmp/bridgectl haptic-hal-inspect'      # read only: the vibrator HAL's version, capabilities
adb shell su -c '/data/local/tmp/bridgectl haptic-hal-primitives'   # read only: composition limits, primitives, presets
adb shell su -c 'ONEIOS_HAPTIC_NATIVE_IMPACTS=1 ONEIOS_HW_BACKEND_HAPTICS=treble ONEIOS_HW_FALLBACK=0 /data/local/tmp/bridgectl haptic-native-inspect'
adb shell su -c 'ONEIOS_HAPTIC_ACTIVE_TEST=1 /data/local/tmp/bridgectl haptic-hal-click'        # ONE perform(CLICK, MEDIUM)
adb shell su -c 'ONEIOS_HAPTIC_ACTIVE_TEST=1 /data/local/tmp/bridgectl haptic-hal-heavy-click'  # ONE perform(HEAVY_CLICK, STRONG)
adb shell su -c '/data/local/tmp/bridgectl haptic 200 1000'
adb shell su -c '/data/local/tmp/bridgectl haptic-pattern'
adb shell su -c '/data/local/tmp/bridgectl bt'            # with Bluetooth off in One UI's settings (see below)
adb shell su -c '/data/local/tmp/bridgectl audio /data/local/tmp/oneios-audio'          # 2 s, then 0.5 s
adb shell su -c '/data/local/tmp/bridgectl audio /data/local/tmp/oneios-audio 12 record' # 12 s, then the microphone
adb shell su -c '/data/local/tmp/bridgectl audio-loop /data/local/tmp/oneios-audio 10'   # 10 clicks: the round trip
```

`host` changes nothing (safe beside One UI and a session): `read: ro.product.model=... ...`, `host: galaxy-s24-ultra
(matched by ro.product.model=SM-S928B)` on the S24 Ultra, then one line per table entry (`*` marks the one taken) with
its match strings and defaults; `ONEIOS_HOST_PROPS=FILE` replays another phone's `getprop` dump. Not run on a phone yet.
Every command (and `brightctl`, `batteryctl`, `thermalctl`) first prints `host: ...`.
`bt` takes the controller from One UI's Bluetooth app if that app has it open: turn Bluetooth off first. It prints
`HAL 1.1 registered`, `bluetooth: HAL 1.1 found, initialising ...`, `bluetooth: HAL initialisation complete: the
controller is on (status 0)`, `state 1`, two events `<- type 4, 6 bytes: 0e 04 01 03 0c 00` (Reset) and `<- type 4,
12 bytes: 0e 0a 01 09 10 00 <address, low byte first>` (Read BD_ADDR), `bt: ok: 2 events ...`, then `ok`. The
bridge's own lines (`error: bluetooth: ...`) say which step failed: not registered / find denied, `initialize`
refused, or no `initializationComplete` in 15 s (then look for `avc: denied` lines with `hal_bluetooth` in dmesg). On a
phone with only the AIDL HAL (the Galaxy A56) it prints `HAL AIDL (android.hardware.bluetooth.IBluetoothHci/default)
registered` and `bluetooth: HAL AIDL (version 1, token header SYST) found, initialising ...` instead, then the same events
(not run on an A56). `haptic`
and `audio` are harmless beside One UI.

The haptic checks (2026-11-21, from the S26 Ultra fork's a281001, 400de8d, 0930a44, a3c1c34, dc7e4a1 and 6d9decc):
`haptic` and `haptic-pattern` now wait for the worker and exit 1 with `bridgectl: haptic failed (effects played=N,
failed=M)` when the device rejected an effect or none played: before, they said `ok` while the S26 Ultra's
`sec_vibrator_inputff` refused every `FF_SINE` upload (`EINVAL`). `haptic-inspect` opens each `/dev/input/event*`
`O_RDONLY` for its name, `FF_PERIODIC`/`FF_SINE`/`FF_CUSTOM`/`FF_GAIN` bits and effect slots (no `FF_GAIN`, no upload, no
play, no grab). `haptic-hal-inspect` and `haptic-hal-primitives` make only the vibrator HAL's getters (version,
`getCapabilities`, the composition limits, `getSupportedPrimitives`, `getPrimitiveDuration`, `getSupportedEffects`).
`haptic-native-inspect` opens the real haptics bridge on the treble backend with native impacts asked for and says
whether TICK, CLICK and HEAVY_CLICK are there; it refuses without `ONEIOS_HAPTIC_NATIVE_IMPACTS=1` or with
`ONEIOS_HW_FALLBACK=1`, and closes without `off()`. `haptic-hal-click` and `haptic-hal-heavy-click` play exactly one
preset, only with `ONEIOS_HAPTIC_ACTIVE_TEST=1` and only when the HAL lists it (no `off()` after: One UI's own vibration
is not cut). Stand-in phone: `tools/oneios-tool/tests/phone/hw-backend.sh` (the fake vibrator HAL logs every motor call;
the read-only commands must leave the log empty). The fork's results on the S26 Ultra (its README, not repeated here):
`FF_SINE` upload `EINVAL`, HAL v3 capabilities `0x3c`, one `HEAVY_CLICK STRONG` felt as a clear tick. Not run on the S24.

**Wi-Fi** (`STAGE/bin/wifictl` of `runtime/build.sh`, `bin/wifictl` in the runtime package):

```sh
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl status'    # read only
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl scan'      # read only (one scan, 6 s)
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl p2p'       # read only: every radio, Google's Wonder radio, the AirDrop verdicts (docs/hardware.md 4a)
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl wonder --seconds 30 --freq 5745'   # a Wonder session beside wlan0 (below)
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl join NAME PASSPHRASE'   # takes the Wi-Fi from One UI while it runs
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl restore /data/local/tmp/oneios/session/wifi.state'
```

`status` and `scan` change nothing and are safe beside One UI and beside a session. A pass, as on the phone on
2026-11-12 beside One UI: `wlan0: 8e:f6:ae:01:7e:c9`, `supplicant: service
android.hardware.wifi.supplicant.ISupplicant/default, station interface wlan0, address 8e:f6:ae:01:7e:c9, 0
network(s):`, `signal poll: none (not joined)`, `ok`; and for `scan` one line a network (access point, frequency,
signal, security, name) and `scan: 53 networks`, `ok`. An `error: wifi: supplicant: no ... from the service manager`
line is a lookup refused or a service not registered; `no station interface wlan0` is One UI's Wi-Fi off.
`join` is the whole host path with the tool as the guest: it takes wlan0 as a session does (the supplicant's
networks removed, wlan0's IPv4 addresses removed, GRO and IPv6 off), scans, joins, sends a DHCP discover and prints
the offer (`DHCP: 192.168.x.y offered by ...`), stays 5 s, leaves and gives wlan0 back. Not during a session
(xnuhost's bridge has the supplicant). The passphrase is on the command line: use it on your own phone only.
`restore` puts wlan0's GRO and IPv6 settings back from a killed session's `wifi.state`.

`p2p` (read only, beside One UI and in a session) dumps every wiphy (a split `GET_WIPHY` dump with no interface
named) and every interface, names Google's Wonder radio (the wiphy named `wonder...`, else one with vendor commands
under `00:1a:11` and monitor mode but no access point), and prints its `p2p: Wonder:` lines: its interface types,
its subcommands by name, the version they imply (1 to 5 only: Wonder 1.4 or 1.5; 6 or more: 1.6 or later), and who
holds it now (an interface on its wiphy, a `wondertap*` vdev). Then the verdicts for wlan0's radio, and Wonder's
module, platform driver, `.ko` files and `mosey_server` (named paths, `present` or `none`).
`wonder [--seconds N] [--freq 2437|5220|5745] [--country CC]` (root, One UI running; default 30 s on 2437 MHz) is a
session on Wonder's raw path beside wlan0, in Samsung's Wonder 1.5 order (`wifi/wonder.rs`): wlan0's link read; the
refusals (no Wonder; Wonder in use: another interface on its wiphy or a `wondertap*` vdev up; no wlan0; the
driver's country not known or another than `--country`: Wonder's init sets the driver's country to the one given);
a monitor interface `wondermon0` on Wonder's wiphy; `SET_REGULATORY` (the driver's own country), `SET_FREQUENCY`
(20 MHz), `SET_FIXED_TX_RATE` (HT MCS 0, one stream), `SET_FILTER` (AWDL's BSSID `00:25:00:ff:94:73`); up (the
provider's init opens `wondertap0`); `GET_IF_MAC_ADDR`; `AF_PACKET` on it and N s of frames counted (AWDL's action
frames by subtype, AWDL's data frames, the peers' addresses and best signal, frames without the radiotap TSFT); then
in reverse, also after any failed step or Ctrl-C: the socket closed, down (the provider's deinit), the interface
removed; last wlan0's link against its link before (waited for up to 10 s; `-ENOLINK` when it was associated and is
not). Each step is a `wonder:` line with its result. A killed `wonder` leaves `wondermon0`: the next one removes it
first. Host-tested on a stand-in nl80211 (`wifi/test_wonder.rs` `wonder_session`, the port of `tests/test_wifi.c`'s `test_wonder_session`); **not run on the phone**.
`oneios-tool phone wifi p2p|wonder` runs both from the Mac (docs/hardware.md 4a has the phone checks).

`audio` plays a 440 Hz tone at -30 dBFS (quiet at any media volume) for S seconds, then 0.5 s later a 660 Hz one
for 0.5 s (the helper's stream lingers: no new open), and with `record` takes 2 s of the microphone. A pass, as on
the phone on 2026-11-12 beside One UI (the helper of then): `oneios-audio: speaker open: 48000 Hz, 2 channel(s), none,
sharing shared, mmap 0, burst 1600, buffer 3200 of 4800 frames`, the `speaker stopped by the guest` lines with `0
underruns` and `latency about 130 ms`, `audio: microphone: 92400 frames in 2 s (96000 expected), peak ... RMS -52.0
dBFS` (room noise), `ok`. The helper of 2026-11-13 (not run on the phone yet) says `none asked, none given`,
`buffer 2800 of 4800`, then `the server pulls N frames every N ms`, and its reports a `margin` near 20 ms and, [I]
expected, `latency about 95-105 ms`; `ONEIOS_AUDIO_PERF=low bridgectl audio ...` runs it with performance mode
LOW_LATENCY for a comparison (`docs/hardware.md` section 6, "Latency per stage"). `0 frames` or `all zero` is a silenced microphone; an `open speaker: ...` line is AAudio's refusal
(then `dmesg | grep avc`). The other tools have not been run yet.

Since 2026-11-15 (fix-audio-latency; not run on the phone yet) `audio` first prints the helper's probe, `audio:
probe: speaker latency N frames (N ms): oneios-audio-latency=N in a session`, and the helper opens the speaker
LOW_LATENCY with MMAP exclusive first: its open line says what the phone's HAL gives (`low latency given, sharing
exclusive, mmap 1, burst B, ...; MMAP exclusive: the DSP reads the buffer`, or the reason it fell back: `MMAP not
given`, `MMAP shared ...`). Beside One UI system_server runs, so an MMAP shared answer is kept there (in a session
it is not): `ONEIOS_AUDIO_SYSTEM_SERVER=0 bridgectl audio ...` behaves as a session does. Its reports end
`latency about N ms; end-to-end N ms (...)`; on MMAP exclusive about 10-20 ms is expected [I], was 128-137.
`audio-loop` plays N clicks (20 ms of 1 kHz at -30 dBFS, 0.5 s apart) while the microphone records, and prints
when each was heard after it was handed to the helper (`audio-loop: click K: heard N ms after ...`), the noise and
the threshold (`noise -52 dBFS`: 18 dB above it, at least -56 dBFS), and `audio-loop: round trip (the speaker + the
air + the microphone) median N ms, MIN..MAX, K of N clicks`. The round trip includes the microphone's own path, so it
is an upper bound of the speaker's; the helper's `end-to-end` (stderr) is the speaker's part from AAudio's
timestamps, and the difference is the microphone's and the air's (about 0.5 ms for the phone's speaker to its
microphone). `not heard` for every click: the media volume at 0, or the microphone silenced.

Since 2026-11-22 (fix-audio-volume-haptics; host-tested, not run on the phone) the helper plays at full scale and
iOS's volume is the one that counts (applied in the guest, `drivers/audio/README.md`): at the speaker's first open it
sets the audio policy's index of USAGE_MEDIA on the speaker to its maximum (libaudioclient's `AudioSystem`, looked
up by mangled name: `setVolumeIndexForAttributes` with `muted` (Android 16 and later) or without, `get...` and
`getMaxVolumeIndexForAttributes`) and sets the index it found back when it ends. Only the audio policy's live index
changes: One UI's saved volume (system_server's settings) is not written, and [I] system_server sets every saved
index again as it starts, should the return kill the helper first. Lines: `oneios-audio: host volume: One UI's media
volume on the speaker was index N of M; set to the maximum for the session: ...`, `... is at its maximum (index N)`,
`... libaudioclient's AudioSystem volume calls not found (...)`, `... the audio policy did not say ...`, and at the
end `... put back to index N of M (status S)`. `ONEIOS_AUDIO_HOST_VOLUME=keep`: not touched (One UI's media volume,
the path before). Beside One UI (system_server runs: `bridgectl audio`) it is not touched either (`... kept:
system_server runs`): there the volume is AudioService's.
