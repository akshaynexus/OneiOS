# runtime/bridges: the host side of OneiOS's hardware

Owner: the bridges agent. What each device is, on the phone and for iOS: `docs/hardware.md`. The kernel side:
`drivers/`.

```sh
make test          # host unit tests (ASan, UBSan); no phone, no Apple files
make               # build/android/libonebridges.a, oneios-audio, bridgectl, wifictl   (NDK r28c)
tests/host.sh      # both; CI runs it (Linux only: on a Mac, runtime/host-tests.sh runs it in Apple's container)
```

| Folder | What |
|---|---|
| `include/bridges/bridges.h` | every bridge's C API, with no dependency on xnuhost |
| `include/bridges/devices.h` | the virtio devices on xnuhost's transport (`runtime/xnuhost/src/xnuhost.h`) |
| `cpu/` | host topology from sysfs, the vCPU plan (E first, then P), thread placement, the cpufreq register window |
| `haptics/` | evdev force feedback (`sec_vibrator_inputff`), a worker thread, the iOS-to-effect mapping |
| `net/` | the old route, only with xnuhost's `wifi=tap`: tap back end, the built-in DHCP answer, `oneios-net.sh` (routing and NAT, root) |
| `wifi/` | the phone's Wi-Fi for iOS (`docs/hardware.md` section 3): wlan0 is the guest's en0. `frames.c` (which frames cross, coalesced frames cut), `station.c` (the guest driver's station messages, `drivers/net/oneios_wifi_proto.h`), `sim.c` (a made-up radio, xnuhost's `wifi=sim`), `wlan.c` (AF_PACKET, nl80211 scans and signal, wlan0's settings kept and given back; three threads), `supplicant.c` (joins through the vendor's wpa_supplicant), `aidl_parcel.c` + `aidl_binder.c` (stable AIDL over `/dev/binder` without Android libraries) |
| `bt/` | the Bluetooth HAL (HIDL 1.0/1.1) over `/dev/hwbinder` without Android libraries (two threads: one answers the HAL's callbacks, one makes the calls from a queue, so a vCPU never waits on the HAL); packet checks; `br_hci_hidl_probe` for xnuhost's start; `hci_sim.c`: the stand-in controller of `bluetooth=sim` (a boot with no phone): Command Complete with a 5.4 controller's return parameters for every command (Command Status for the ones answered that way), Unknown HCI Command for vendor commands, one made-up device ("OneiOS sim") heard by an LE scan, data dropped; `test_bridges.c` checks its answers |
| `audio/` | the back end xnuhost links (`backend.c`) and the AAudio helper process (`aaudio_helper.c` -> `oneios-audio`) |
| `brightness/` | the panel's backlight (sysfs, the curve, fades, restore), the light sensor `als_rear`, `brightctl` (the phone test); its own `README.md` and `Makefile` |
| `battery/` | the phone's battery for iOS: Android's `power_supply` nodes read every 3 s, normalized, changes sent to the guest; `batteryctl` (the phone test); its own `README.md` and `Makefile` |
| `camera/` | the camera (fable-camera): the helper process `oneios-camera`, the frame ring, xnuhost's end (`libonecamera.a`) and `camctl`; its own `Makefile` and `README.md` |
| `devices/` | `bridge_dev.c` (`oneios,bridge`), `virtio_net.c` (`droidvm,net`; a frame of the Wi-Fi station's own type waits up to 50 ms for a guest buffer, any other is dropped when there is none), `virtio_snd.c` (`droidvm,virtio-snd`) |
| `tools/bridgectl.c` | tries each bridge on the phone without xnuhost (root) |
| `tools/wifictl.c` | the Wi-Fi bridge on the phone without xnuhost (root): `status`, `scan`, `join`, `restore` |
| `tests/` | `test_bridges.c`, `host.sh`; `test_hidl_binder.c` (`make test-binder`, part of `make test`): the HAL client against a fake hwservicemanager and IBluetoothHci on a real binder driver, a binderfs mounted in a user namespace (the PC kernel's `CONFIG_ANDROID_BINDERFS`; no root; skipped without it). A kernel whose `CONFIG_ANDROID_BINDER_DEVICES` is empty (Ubuntu's; the Mac's container kernel) mounts a binderfs with only `binder-control`: the test asks it for `hwbinder` (`BINDER_CTL_ADD`). The fake HAL retries `BINDER_SET_CONTEXT_MGR` while the last scenario's killed HAL still holds it (`EBUSY`, up to 2 s). On a Mac: `runtime/host-tests.sh`, in Apple's `container` with a kernel built with binderfs ([building.md](../../docs/building.md) section 4); `test_wifi.c` (in `make test`): the Wi-Fi bridge's frames, the DHCP reader (the REQUEST forms of RFC 2131 4.3.2, iOS's and another station's, and their log lines), station, parcels, join plan and callbacks; `test_aidl_binder.c` (`make test-binder`): the supplicant client against a fake service manager and supplicant on the same binderfs |

## Into xnuhost (bridges -> booter)

Link `build/android/libonebridges.a` (static, no library beyond libc) and add `-I runtime/bridges/include
-I drivers/bridge`. Ship `build/android/oneios-audio` as `bin/oneios-audio` (0755), `net/oneios-net.sh` as
`bin/oneios-net.sh` and `build/android/wifictl` as `bin/wifictl` in the runtime package.

```c
#include "bridges/devices.h"

br_log_sink = xnuhost_log;                              // optional: (int level, const char *line)

// CPU, before the device tree is made and the vCPUs start
br_cpu_topology host; br_cpu_plan plan;
br_cpu_read("/sys", &host);
br_cpu_plan_make(&host, config.ncpu, false, &plan);     // machine.cpu_cluster[i] = plan.v[i].type
br_cpufreq *cpufreq = br_cpufreq_create(&plan, tid_of_vcpu);
xh_mmio_add(&(xh_mmio_region){ .base = CPUFREQ_BASE, .size = plan.nvcpu * BR_CPUFREQ_WINDOW, .name = "cpufreq",
                               .opaque = cpufreq, .read = cpufreq_read, .write = cpufreq_write });   // thin wrappers
// in each vCPU thread:            br_cpu_place(0, &plan, index);
// when a vCPU runs an iOS thread: br_cpu_place(tid, &plan, index); br_cpu_request(tid, &plan, index, br_cpufreq_requested(cpufreq, index));

// One window and SPI each, as for xh_virtio_blk_create
br_bridge_config bridge = { .haptic = br_haptic_open(NULL), .hci_factory = br_hci_open_hidl };
br_bridge_create(base0, spi0, &bridge);                 // device tree: /arm-io/bridge, "oneios,bridge"

br_net_config net; br_net_config_defaults(&net);        // set net.guest_mac to en0's address
br_virtio_net_create(base1, spi1, &net, br_net_open_tap);   // /arm-io/net; then run bin/oneios-net.sh up

void *audio_ctx;
const br_audio_backend *audio = br_audio_helper("/data/runtime/bin/oneios-audio", &audio_ctx);
if (!audio) audio = br_audio_null(&audio_ctx);
br_virtio_snd_create(base2, spi2, audio, audio_ctx);    // /arm-io/virtio-snd
```

- Every create returns NULL (with a log line saying why) when its Linux side is missing; a missing device only
  turns that feature off. `br_haptic_open` and `br_hci_open_hidl` returning NULL are fine to pass on.
- Threads: each bridge has its own (haptic worker, tap receiver, binder looper, sound capture). They take the
  device's lock with `xh_virtio_lock`, as `input.c` does. None of them uses signals.
- The network: `br_wlan_prepare("wlan0", session_dir, net.guest_mac)` then `br_virtio_net_create_backend(base, spi,
  &net, the returned open, br_wlan_send)`; `br_wlan_close()` at the stop (the network left, the supplicant as it
  was found, wlan0's two settings back from `session_dir/wifi.state`).
- Stop: on `SIGTERM`, `br_haptic_close()` (stops the motor), `br_wlan_close()` (and `oneios-net.sh down` with
  `wifi=tap`); closing the descriptors does the rest (the kernel erases a closed file's force-feedback effects; the audio helper exits when its socket closes).
- Everything is under `/dev`, `/sys`, `/proc/sys/net` and the session's own files: nothing of One UI's `/data` is
  touched. System state changed: wlan0's GRO and `disable_ipv6` and its IPv4 addresses, and the supplicant's
  networks (`docs/hardware.md` section 3, "Return": what is left after a kill, and why it does not break One UI's
  Wi-Fi). With `wifi=tap` instead: the tap, the policy rules and the two iptables chains, which `down` removes.

## On the phone

```sh
adb push build/android/bridgectl build/android/oneios-audio /data/local/tmp/
adb shell su -c '/data/local/tmp/bridgectl cpu'
adb shell su -c '/data/local/tmp/bridgectl haptic 200 1000'
adb shell su -c '/data/local/tmp/bridgectl haptic-pattern'
adb shell su -c '/data/local/tmp/bridgectl bt'            # with Bluetooth off in One UI's settings (see below)
adb shell su -c '/data/local/tmp/bridgectl audio /data/local/tmp/oneios-audio'          # 2 s, then 0.5 s
adb shell su -c '/data/local/tmp/bridgectl audio /data/local/tmp/oneios-audio 12 record' # 12 s, then the microphone
```

`bt` takes the controller from One UI's Bluetooth app if that app has it open: turn Bluetooth off first. It prints
`HAL 1.1 registered`, `bluetooth: HAL 1.1 found, initialising ...`, `bluetooth: HAL initialisation complete: the
controller is on (status 0)`, `state 1`, two events `<- type 4, 6 bytes: 0e 04 01 03 0c 00` (Reset) and `<- type 4,
12 bytes: 0e 0a 01 09 10 00 <address, low byte first>` (Read BD_ADDR), `bt: ok: 2 events ...`, then `ok`. The
bridge's own lines (`error: bluetooth: ...`) say which step failed: not registered / find denied, `initialize`
refused, or no `initializationComplete` in 15 s (then look for `avc: denied` lines with `hal_bluetooth` in dmesg). `haptic`
and `audio` are harmless beside One UI.

**Wi-Fi** (`build/android/wifictl`, `bin/wifictl` in the runtime package):

```sh
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl status'    # read only
adb shell su -c '/data/local/tmp/oneios/runtime/bin/wifictl scan'      # read only (one scan, 6 s)
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

`audio` plays a 440 Hz tone at -30 dBFS (quiet at any media volume) for S seconds, then 0.5 s later a 660 Hz one
for 0.5 s (the helper's stream lingers: no new open), and with `record` takes 2 s of the microphone. A pass, as on
the phone on 2026-11-12 beside One UI: `oneios-audio: speaker open: 48000 Hz, 2 channel(s), none, sharing shared,
mmap 0, burst 1600, buffer 3200 of 4800 frames`, the `speaker stopped by the guest` lines with `0 underruns` and
`latency about 130 ms`, `audio: microphone: 92400 frames in 2 s (96000 expected), peak ... RMS -52.0 dBFS` (room
noise), `ok`. `0 frames` or `all zero` is a silenced microphone; an `open speaker: ...` line is AAudio's refusal
(then `dmesg | grep avc`). The other tools have not been run yet.
