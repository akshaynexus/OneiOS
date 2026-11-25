# The runtime in Rust

The host side of OneiOS (xnuhost, its loader, the bridges, the camera and encoder helpers, the display helper, the
package's small programs) is Rust: the Cargo workspace `runtime/Cargo.toml`, plus the task stub and the test kernel,
two bare-metal programs with workspaces of their own. It was ported from C on branch `rust-runtime` (2026-11-23), one C
file to one Rust module, keeping the behaviour, the settings, the protocols and the log lines. Each port commit
(`git log --oneline main..rust-runtime`) names the C files it replaced, the C tests it carried over and the C bugs it
fixed on the way.

**What has run:** every workspace test passes (`cargo test --workspace`), `cargo fmt --check` and `cargo clippy
--workspace --all-targets -- -D warnings` are clean, `runtime/xnuhost/check-arm64.sh check`'s three boots of the test
kernel pass with the Rust xnuhost under qemu-aarch64, and the trap path was benchmarked against the C build under qemu
(docs/booter.md 2.6a, docs/building.md section 4). **On the phone** (2026-11-23 13:03, xnuhost `58a378e2...` from
`00c174d`): iOS booted to the Home Screen, first frame with content 50.1 s after xnuhost's start (the C: 51.9 s on the
same kernel and image); touch, the GPU, Wi-Fi, audio and haptics ran; the trap path is level with the C
(docs/booter.md 2.6a, "On the phone, Rust against C"). The return to One UI was clean (`exit 0`, gates A-C, One UI in place after 67 s). Not checked yet: the camera's
picture, the pen. Every other phone result in the docs dated before 2026-11-23 is of the C runtime.

## The crates

| Crate | Folder | What it is | Programs (`[[bin]]`) |
|---|---|---|---|
| `xh-core` | `core/` | The base every crate builds on: the one log of xnuhost and the bridges (`loge!`/`logi!`/`logd!`, the console, the signal-safe line), the interrupt and guest-RAM hooks devices call, a trapped context's registers, AArch64 load/store and `mrs`/`msr` decoding, the MMIO bus, the virtio-mmio transport and its test driver (`virtio::testing`), the pixel formats (`pixels.h`), `evdev` (the event codes, `input_event`, the EVIOC* ioctls), and the small helpers: `bytes`, `fs` (sysfs, /proc, meminfo), `parse` (numbers and sizes as `strtoull`/`xh_parse_size`), `thread` (named threads, the lock), `fd` (memfds, socket pairs, SCM_RIGHTS), `sys` (errno texts), `ioctl`, `text`, `time`, `inflate` (zlib streams: the gpuaddon's files xnuhost reads) | none |
| `xnuhost-abi` | `abi/` | `no_std`: what xnuhost shares with the hosted kernel and the task stub: `hostcall` (`hostcall.h`), `stub` (the stub's region, trap pages, the service page), `free_report` (`free_report.h`, `free_report_runs.h`) | none |
| `xnuloader` | `loader/` | The XNU loader: Mach-O, the Apple device tree, the boot arguments, the machine layout; `fixture` builds test kernels | `xnuloader-inspect` (`kernel FILE`, `adt FILE`, `board NCPU`: what the loader sees in a kernel collection or a device tree), `mkkernel` (wraps the test kernel's ELF as a `kernel.kc` and writes its test disk) |
| `xnuhost` | `xnuhost/` | XNU as a process on Android's Linux kernel, one module per part of the machine (`src/lib.rs` lists them): `arch` (context switch and trap entry, `global_asm!`), `cpu`, `task`, `kctx`, `hostcalls`, `gic`, `sysreg`, `mem`, `discard`, `ramfix`, `uart`, `perf`; `session` (`main`, the main loop, the stop; `session/board.rs`, `bridges.rs`, `host.rs`, `memory.rs`), `config`, `imageset`, `blk`, `virtio_blk`, `usb`, `ready`, `ret`; the devices `display`, `virtio_gpu`, `gpu3d`, `gpu3d_kgsl` (+ `policy`), `gpu3d_amdgpu` (+ `uapi`, `fences`, `addon`, `selftest`; host builds also compile `gpu3d_amdgpu/layout.c` against Mesa's and libdrm's headers for its layout test), `input`, `input_script`, `pen`, `virtio_input`. `build.rs` builds the task stub and links its blob in | `xnuhost`; `gpuctl` (`tools/gpuctl.rs`, the GPU check on the phone); example `machine-smoke` (the vCPU machinery alone) |
| `drm2kgsl-sys` | `xnuhost/drm2kgsl/` | Droid-VM's drm2kgsl renderer from `external/virglrenderer` (pinned), patched with `drm2kgsl/patches` and compiled from its C by `build.rs` (the NDK's clang for Android, `$CC` for the host), with a small C glue and a safe Rust front. Without the checkout it builds empty (`AVAILABLE` is false, a cargo warning says so) and xnuhost is 2D-only | none |
| task stub | `xnuhost/stub/` (own workspace) | `no_std`, `aarch64-unknown-none`: the only Linux code inside an iOS task process, a flat position-independent blob. `stub/build.sh OUT` writes `stub.bin` and refuses one that needs relocating or data, could panic, does not start at offset 0 or is over `XS_TEXT_MAX` (64 KiB) | (the blob) |
| test kernel | `xnuhost/testkernel/` (own workspace) | `no_std`, `aarch64-unknown-none`: xnuhost's host-call ABI exercised as XNU's machine layer uses it (the boots of `check-arm64.sh`). `testkernel/build.sh OUT` writes `testkernel.elf`; `testkernel/check-arm64.sh XNUHOST IMAGE WORK` makes the three boots | (the ELF) |
| `oneios-display` | `display/` | The display helper: SurfaceControl on Android, the `xhd` protocol shared with xnuhost (`src/xhd/`), `frame_perf`, the JNI library (feature `jni`) and `java/OneiOSDisplay.java`. `build-android.sh OUT` makes its four phone files | `oneios-display` (`src/bin/launcher.rs`), `displayctl` |
| `onebridges` | `bridges/` | The host bridges, one folder each: `cpu`, `hw` (the backend layer, the host-device table), `haptics`, `net`, `audio`, `bt`, `wifi`, `hal` (the treble HAL client, one binder layer for AIDL and HIDL), `brightness`, `battery`, `thermal`, `sensors` (the sensor hub over QRTR), `devices` (the bridge device, virtio-net, virtio-snd), `proto` (the guest drivers' protocols), `log` | `bridgectl`, `wifictl`, `brightctl`, `batteryctl`, `thermalctl`, `sensorctl`, `oneios-audio` (the AAudio helper; dynamic); for tests: `fake-hal`, `hal_binder`, `test_hidl_binder`, `test_aidl_binder`, `brightness_inspect` |
| `onecamera` | `bridges/camera/` | The camera bridge: the `oneios,camera` device, the helper's socket and ring, the Camera2 NDK and provider-HAL back ends | `oneios-camera`, `camctl` (both dynamic) |
| `oneencoder` | `bridges/encoder/` | The video encoder bridge: the `droidvm,virtio-media` device and its back ends (MediaCodec through the helper, the stand-in) | `oneios-encoder`, `encctl` (both dynamic) |
| `oneios-switches` | `switches/` | The switch catalog (docs/switches.md) and every key of `xnuhost.conf` with the rule of its values, no dependency and no I/O: xnuhost's `switches:` line is made from it, and `tools/oneios-tool` builds it too (a path dependency) for `oneios-tool switches` and `conf`, the OneOS app's Switches screen. xnuhost's `config::tests::the_catalog` checks it against the parser | none |
| `oneios-package-tools` | `package/` | The package's small standalone programs | `test_a56_gpu` (the A56's GPU check). The placeholder xnuhost, `package/placeholder-xnuhost.rs`, is built by `runtime/package.sh` with rustc alone |

Which programs go to the phone, static or dynamic, is `runtime/build.sh`'s header; each folder's README has its
commands and what a good run prints.

## Building and testing

Prerequisites (docs/building.md section 0): rustup's stable toolchain with `rustfmt` and `clippy`, the targets
`aarch64-linux-android` and `aarch64-unknown-none`, and the NDK 28.2. Cargo links `aarch64-linux-android` with
`runtime/ndk-cc` (`runtime/.cargo/config.toml`), the NDK's `aarch64-linux-android30-clang`, found from `$NDK`, else
`$ANDROID_NDK_ROOT`, else `$ANDROID_NDK_HOME`, else the SDK's `ndk/28.2.13676358`.

```sh
runtime/build.sh STAGE                 # the phone's programs, stripped, into STAGE (static and dynamic), then elf-check
runtime/host-tests.sh                  # the whole host suite: workspace, lint, boot, port, links, package
runtime/tests/host.sh                  # what CI runs: cargo test, fmt, clippy, check-arm64.sh smoke check
cd runtime
cargo test --workspace                 # every crate's tests
cargo test -p onebridges thermal       # one crate, tests whose names contain "thermal"
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --target aarch64-linux-android -p xnuhost \
  --config 'target.aarch64-linux-android.rustflags=["-C","target-feature=+crt-static"]'  # a static phone program
xnuhost/check-arm64.sh smoke check     # the static xnuhost under qemu-aarch64: the machinery, the test kernel's boots
xnuhost/check-arm64.sh bench | rpc | parallel | ab   # the trap path's costs (docs/building.md section 4)
```

- Static programs (xnuhost, gpuctl, the phone checks) build with `crt-static` in a target dir of their own
  (`runtime/target/android-static` in `build.sh`); the dynamic ones (`oneios-audio`, `oneios-camera`, `camctl`,
  `oneios-encoder`, `encctl`, `liboneios-display.so`) without (`runtime/target/android`). Static arm64 programs run on
  an x86_64 PC under `qemu-aarch64`.
- Several builds at once: give each its own `CARGO_TARGET_DIR` (e.g. `runtime/target/NAME`), so cargo's lock never
  makes one wait.
- Tests that need a phone, binderfs, `/dev/udmabuf`, root, the NDK or `ffmpeg` skip with a printed reason (the binder
  tests re-run themselves in a user namespace with a binderfs, `bridges/hal/binderfs.rs`). Test binaries that cannot
  use cargo's harness (the binder ones) are `[[bin]]`s or `harness = false` tests.
- A shared object is linked by a `cargo rustc ... --crate-type cdylib -- -C link-arg=-Wl,-z,defs` line in a runtime
  script (today only `display/build-android.sh`, for `liboneios-display.so`); no Cargo.toml declares a `cdylib`.
  `runtime/package/elf-check-test.sh` fails when a cdylib line lacks the flag or a Cargo.toml declares one, and
  `runtime/package/elf-check.sh` checks every stage (docs/building.md section 4).

## Conventions

- **Log lines are an interface.** `oneios-tool log`, `phone log --grep`, the phone tests and the docs grep xnuhost's
  and the helpers' lines, so a line's text stays byte for byte when code moves; change one only with its readers
  (tools/oneios-tool, the docs) in the same commit. The same holds for every setting and its default, file path,
  protocol layout, timing constant and exit code.
- **`unsafe` only at the system boundary**: system calls, mmap'd guest RAM, the vCPU's context switch, FFI to the NDK
  and to drm2kgsl's C. Each block has a `// SAFETY:` comment (clippy's `undocumented_unsafe_blocks` warns, and warnings
  fail the lint; `unsafe_op_in_unsafe_fn` is denied) and sits in a safe function the rest of the code calls.
- **Rust, not C in Rust's clothes**: enums for states and codes, `Result` for failures, `OwnedFd`/`Drop` for resources
  (and an explicit idempotent `shutdown(&self)` where the process ends without dropping, as the brightness and haptics
  bridges have: the way back to One UI must not depend on a `Drop`), slices for buffers, `std::sync` and atomics for
  shared state; a global only where the design needs one (`OnceLock`, `Mutex`).
- **Shared code once**: what two crates need goes into `xh-core` (a new module and its `pub mod` line with a doc line
  in `core/src/lib.rs`), what two modules of one crate need into that crate's own helper module. No local copies of
  `fs`, `parse`, `thread`, `fd` or the log.
- **Tests beside the code** (`#[cfg(test)] mod tests`, or `tests/*.rs` named in Cargo.toml): every case the C tests
  had was carried over; a fix comes with a test of the fault.
- **Comments say why**: the phone's numbers, the dates, the faults that made a rule, as the C comments did.
- **Dependencies**: `libc` and the workspace's crates. Another crate only when it replaces a lot of hand-written unsafe
  code, with the reason in its Cargo.toml.
- `cargo fmt` (`runtime/rustfmt.toml`: width 120) and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- The owner's rules (CLAUDE.md) hold here as everywhere: no Python; no `sleep` and no wait loops; no Apple files in
  git; the way back to One UI must always work (never grab the side keys, run realtime threads, leave the namespace or
  cgroup, lower the OOM score, write under `/sys/power` or reboot); every change is a fix or a lasting tool.

## What stays C, and why

| C | Why | Its Rust side, and the check that they agree |
|---|---|---|
| `runtime/xnuhost/include/xnuhost/hostcall.h`, `free_report.h`, `free_report_runs.h` | The hosted kernel's C includes them (kernel patches, `osfmk/arm64/hosted`) | `xnuhost-abi`'s `hostcall.rs`, `free_report.rs`; `abi/tests/headers.rs` reads the headers and checks every constant, struct size and field offset, that no define is left unmirrored, and that the free-report headers equal the kernel patches' copies (0058, 0066) |
| `runtime/xnuhost/include/xnuhost/pixels.h` | The guest's display driver includes it | `xh-core`'s `pixels.rs`; its test reads the header and checks every constant |
| `drivers/*/` protocol headers (`drivers/bridge/oneios_bridge_proto.h`, `drivers/haptics/oneios_actuator.h`, `drivers/net/oneios_wifi_proto.h`, `drivers/brightness/oneios_bl_request.h`, `drivers/camera/oneios_camera_proto.h`, `oneios_camera_abi.h`) | The guest drivers are kernel C, built with XNU's own compile commands | `onebridges::proto` (`proto/cheader.rs`), `bridges/wifi/proto.rs`, the brightness tests, `onecamera`'s `tests/headers.rs`: each reads the header and checks what it mirrors |
| drm2kgsl (`external/virglrenderer`, pinned) | Droid-VM's renderer, another project's code; rewriting it would fork it | `drm2kgsl-sys`: `build.rs` compiles it with `drm2kgsl/patches`; `glue/oneios_glue.c` (only the struct and `va_list` parts Rust cannot reach; one lock over the renderer's tables), `glue/oneios_sys.{c,h}` (host builds route the renderer's `open`/`ioctl` through a table the tests set), `glue/oneios_kgsl_layout.c` (the tests' mirror of `msm_kgsl.h` checked against the header), `oneios_drm2kgsl.h`, `config.h`, `host_c11.h` |
| `runtime/xnuhost/src/gpu3d_amdgpu/layout.c` (host builds only) | The sizes, offsets and numbers of Mesa's amdgpu_virtio_proto.h, drm_hw.h and amdgpu_drm.h and libdrm's amdgpu.h that `gpu3d_amdgpu/uapi.rs` mirrors | `the_layout_is_the_headers` compares every value (skipped without external/mesa-droidvm or libdrm's amdgpu.h) |
| `runtime/xnuhost/drm2kgsl/oneios_gpu_policy.h` | The patched renderer's C includes it for its class names and KGSL priorities | `xnuhost/src/gpu3d_kgsl/policy.rs` has the policy; a test checks the two agree |
| `xnuhost/stub/stub.ld`, `xnuhost/testkernel/testkernel.ld` | Linker scripts of the two bare-metal programs | — |
| `kernel/`, `drivers/`, `userland/` | The hosted kernel's patches, the guest drivers, the guest helpers built for iOS: not the runtime | — |

The NDK's own numbers and layouts the camera and encoder use (Camera2, MediaCodec) are checked against the NDK's
headers by a probe built with the NDK's clang and run under qemu-aarch64 (`bridges/camera/tests/cprobe`, both crates'
`tests/headers.rs`; skipped with a line without them). The encoder's and the audio helper's wire layouts have no C
side any more: both ends are Rust, checked by round-trip tests.

## Adding to the runtime

- A new module: one file, its `pub mod` line with a doc line in the crate's `lib.rs` (sorted as rustfmt keeps them),
  its tests beside it.
- A new phone program: a `[[bin]]` in its crate's Cargo.toml, then its `cargo_static`/`cargo_dynamic` and `stage` lines
  in `runtime/build.sh` (and the README of its folder); a dynamic one must pass `runtime/package/elf-check.sh`.
- A new guest-facing header: it lives where the kernel's C includes it; the Rust mirror gets a test that reads it.
- Docs: CLAUDE.md's table says which doc covers which part; docs/booter.md (xnuhost, the loader), docs/hardware.md (the
  bridges), docs/gpu.md (the GPU path), runtime/display/README.md, docs/building.md (commands).
