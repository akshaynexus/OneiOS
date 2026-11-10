# runtime/bridges: the host side of OneiOS's hardware

Owner: the bridges agent. What each device is, on the phone and for iOS: `docs/hardware.md`. The kernel side:
`drivers/`.

```sh
make test          # host unit tests (ASan, UBSan); no phone, no Apple files
make               # build/android/libonebridges.a, oneios-audio, bridgectl   (NDK r28c)
tests/host.sh      # both; CI runs it
```

| Folder | What |
|---|---|
| `include/bridges/bridges.h` | every bridge's C API, with no dependency on xnuhost |
| `include/bridges/devices.h` | the virtio devices on xnuhost's transport (`runtime/xnuhost/src/xnuhost.h`) |
| `cpu/` | host topology from sysfs, the vCPU plan (E first, then P), thread placement, the cpufreq register window |
| `haptics/` | evdev force feedback (`sec_vibrator_inputff`), a worker thread, the iOS-to-effect mapping |
| `net/` | tap back end, the built-in DHCP answer, `oneios-net.sh` (routing and NAT, root) |
| `bt/` | the Bluetooth HAL (HIDL 1.0/1.1) over `/dev/hwbinder` without Android libraries; packet checks |
| `audio/` | the back end xnuhost links (`backend.c`) and the AAudio helper process (`aaudio_helper.c` -> `oneios-audio`) |
| `brightness/` | the panel's backlight (sysfs, the curve, fades, restore), the light sensor `als_rear`, `brightctl` (the phone test); its own `README.md` and `Makefile` |
| `camera/` | the camera (fable-camera): the helper process `oneios-camera`, the frame ring, xnuhost's end (`libonecamera.a`) and `camctl`; its own `Makefile` and `README.md` |
| `devices/` | `bridge_dev.c` (`oneios,bridge`), `virtio_net.c` (`droidvm,net`), `virtio_snd.c` (`droidvm,virtio-snd`) |
| `tools/bridgectl.c` | tries each bridge on the phone without xnuhost (root) |
| `tests/` | `test_bridges.c`, `host.sh` |

## Into xnuhost (bridges -> booter)

Link `build/android/libonebridges.a` (static, no library beyond libc) and add `-I runtime/bridges/include
-I drivers/bridge`. Ship `build/android/oneios-audio` as `bin/oneios-audio` (0755) and `net/oneios-net.sh` as
`bin/oneios-net.sh` in the runtime package.

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
- Stop: on `SIGTERM`, `br_haptic_close()` (stops the motor) and `oneios-net.sh down`; closing the descriptors does
  the rest (the kernel erases a closed file's force-feedback effects; the audio helper exits when its socket closes).
- Everything is under `/dev`, `/sys` and the session's own files: nothing of One UI's `/data` is touched. The tap,
  the policy rules and the two iptables chains are the only system state added, and `down` removes them.

## On the phone

```sh
adb push build/android/bridgectl build/android/oneios-audio /data/local/tmp/
adb shell su -c '/data/local/tmp/bridgectl cpu'
adb shell su -c '/data/local/tmp/bridgectl haptic 200 1000'
adb shell su -c '/data/local/tmp/bridgectl haptic-pattern'
adb shell su -c '/data/local/tmp/bridgectl bt'            # with Bluetooth off in One UI's settings
adb shell su -c '/data/local/tmp/bridgectl audio /data/local/tmp/oneios-audio'
```

`bt` takes the controller from One UI's Bluetooth app if that app has it open: turn Bluetooth off first. `haptic`
and `audio` are harmless beside One UI. None of them has been run yet.
