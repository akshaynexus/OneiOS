# The booter: xnuhost and the loader

Owner: booter. Code: `runtime/loader/` (the loader, crate `xnuloader`), `runtime/xnuhost/` (the program, crate
`xnuhost`), `runtime/build.sh` (what `runtime/package.sh` calls). Date: 2026-11-10.

**The runtime is Rust (2026-11-23, branch `rust-runtime`).** xnuhost and the loader were C until then; every C file
became a Rust module of the same name (`src/<name>.c` -> `src/<name>.rs`; `main.c` -> `src/session.rs` and
`src/session/{board,bridges,host,memory}.rs`, `return.c` -> `src/ret.rs`, `arch_aarch64.S` -> `src/arch.rs`'s
`global_asm!` thunks; xnuhost's `log.c`, `virtio.c`, `mmio.c` and `insn.c` -> the shared crate `xh-core`,
`runtime/core/src/`), with the same settings, log lines and exit codes ([runtime/RUST.md](../runtime/RUST.md) has the
crate map and the rules). The task stub is `runtime/xnuhost/stub` (no_std, `aarch64-unknown-none`, `stub/build.sh`),
the test kernel `runtime/xnuhost/testkernel` (`testkernel/build.sh`), the loader's tools the bins `xnuloader-inspect`
and `mkkernel`. What stays C: the guest-facing headers the kernel's C includes,
`runtime/xnuhost/include/xnuhost/{hostcall,pixels,free_report,free_report_runs}.h`, each mirrored in Rust
(`runtime/abi/src/`, `runtime/core/src/pixels.rs`) with a test that reads the header and checks every constant, size
and offset; and the GPU's renderer (virglrenderer's drm2kgsl, pinned, built by `runtime/xnuhost/drm2kgsl/build.rs`
with a small C glue). Function names below are the Rust ones (`module::function`) where the text names code; a record
of a test or a measurement keeps the names of the tree it was made on. **The Rust xnuhost first booted iOS on the
phone on 2026-11-23** (section 2.6a, "On the phone, Rust against C"); it also passes the host tests and boots the test
kernel under qemu-aarch64 (`runtime/xnuhost/check-arm64.sh check`, the five boots: the fourth, xnuhost's last words on a SIGUSR1 from outside; the fifth, the task trim). Every other phone measurement
below was made with the C xnuhost.

xnuhost boots a source-built XNU **as a Linux process** on One UI's kernel: it loads the kernel collection
into its own address space, gives it CPUs (host threads), interrupts and a timer (signals), devices
(emulated in-process) and iOS tasks (other Linux processes whose system calls and faults it traps).

**State, plainly, as of 2026-11-10** (today's: [README.md](../README.md) "Status"). Everything below is built and tested on the PC, including the whole program booting a
test kernel under `qemu-aarch64`. Nothing has run on the phone, and iOS does not boot: the kernel that
uses this host (the hosted machine layer inside XNU) does not exist yet. Section 8 lists what is missing.

## 1. Use

```sh
runtime/build.sh STAGE        # cargo, aarch64-linux-android: STAGE/bin/xnuhost (static, crt-static; the task stub is
                              # inside), gpuctl, the bridges' helpers and phone checks (the script's header lists them)
runtime/build.sh --test       # = runtime/host-tests.sh
runtime/host-tests.sh         # every host test of the runtime (workspace, lint, boot, port, links, package); on a Mac
                              # in an arm64 Linux container, where the binary boots the test kernel as it is (no qemu)
cd runtime && cargo test -p xnuloader -p xnuhost   # the loader's and xnuhost's unit tests alone
runtime/xnuhost/check-arm64.sh smoke check         # the vCPU machinery alone, then the test kernel's five boots
                              # (the static Android xnuhost under qemu-aarch64 on a PC; natively on arm64 Linux)
runtime/xnuhost/check-arm64.sh bench|parallel|rpc|ab   # the trap path's costs, the parallel test, the Mach RPC
                              # round trip, two binaries compared (sections 2.6a, 2.6b, 2.6d; the script's header)
```

Both build hosts work: a Linux x86_64 PC and a Mac (`runtime/ndk-cc`, cargo's linker, finds the NDK's `linux-x86_64`
or `darwin-x86_64` tools). The host unit tests are Linux tests. On a target that can run the kernel (AArch64 Linux or
Android) `build.rs` sets `cfg(xh_machine)` and the real vCPUs, thunks and task layer are built (and the stub, by
`stub/build.sh`, embedded by `arch.rs`); elsewhere, and in the unit tests, `cpu.rs` and `task.rs` build their
stand-ins ([building.md](building.md) section 4).

Command line (the first line is what the OneOS fork runs, `oneos/interface.md` section 4):

```sh
xnuhost --session-abi 1 --mode takeover --runtime DIR --image DIR --data DIR --session DIR
        [--kernel FILE] [--cpus N] [--ram SIZE|auto] [--boot-args TEXT] [--display shm|drm|none]
        [--width W --height H] [--hz N] [--scale 2|3] [--no-input] [--input-script FILE] [--no-tasks]
        [--hw-backend samsung|treble] [--hw-backend-BRIDGE samsung|treble] [--hw-backend-fallback 0|1]
        [--host-device auto|none|NAME] [--host-props FILE] [--host-root DIR]
        [--config FILE] [--set KEY=VALUE] [-v]
xnuhost --selftest            # on the phone: page size, address bits, PAC keys across fork, memfd, seccomp
```

| Input | Default | What |
| --- | --- | --- |
| `--image DIR` | required | the image set (`docs/image-format.md`) |
| `--kernel FILE` | `DIR/kernel.kc`, else `DIR/kernel.macho` | the kernel collection: `MH_FILESET`, or an `MH_EXECUTE` kernel |
| `FILE.trustcache`, `.vroot`, `.gpuaddon`, `.ramdisk`, `.adt` | optional | DarwinBoot's sidecars beside the kernel, same formats and checks. `.gpuaddon` is the boot addon the kernel writes into the Data store: the Metal plug-in (`addons/`) and, since hosted 0090, the capture plug-in (`camera/`); `oneios-tool ios addon` makes it, `phone addon` stages it (the manifest need not list it) |
| `DIR/identity.adt` | optional | the device's own decoded device tree: model, target-type, board name and `/product` are taken from it. Without it the root has model iPhone18,2 and target-type V54 (since fix-ios-compressor: iOS names its jetsam properties by it, section 2.2). No script writes one (2026-11-13: `scripts/image/build.sh` does not, and the PC's set `out/image-pc/24A446` has none), so `/product` is only `product-name`, `artwork-scale-factor` and `model`: none of the iPhone's own `/product` (`artwork-display-gamut P3`, `oled-display`, `graphics-featureset-class APPLE10`, `ui-background-quality 100`, ...), which MobileGestalt answers from. Since 2026-11-22 (fix-artwork-traits) the no-identity `/product` also has the artwork and GPU feature-set values CoreUI needs (`artwork-device-idiom` phone, `artwork-device-subtype` 2868, `artwork-display-gamut` sRGB, `artwork-dynamic-displaymode` 0, `graphics-featureset-class` APPLE10 and its fallbacks without APPLE2 and MTL2,2 when xnuhost's `astc=0`, with them at its default `astc=1` since 2026-11-25 (`Machine::astc`, `set_featuresets`; MobileGestalt then answers DeviceSupportsASTC); [userland.md](userland.md) "The artwork answers"). `--set identity=out/kernel-inputs/24A446/devicetree.bin` (the decoded tree as it is) gives them for one run (`kernel/patches/oneios-hosted/display/README.md`, "What iOS reads about the panel") |
| `--display-format bgrx\|rgba16f` (`display-format=`) | `bgrx` | the frame's pixels (section 2.8): 8-bit sRGB, or half floats that keep wide colour and EDR |
| `DIR/xnuhost.conf` | optional | `key=value` lines: `boot-args`, `boot-args-add`, `disk`, `disk-rw`, `data-store`, `scale`, `scale-fixup`, `ios16-fixup`, `text-fixup`, `ram`, `ram-percent`, `ram-host-floor`, `host-mem-log`, `ram-giveback`, `ram-ondemand`, `ram-prefault`, `ram-lock`, `ram-session-reserve`, `ram-unlock-below`, `ram-huge`, `ram-gpu-reserve`, `ram-pagetable-percent`, `ram-hold-percent`, `ram-tasks`, `ram-task-cost`, `ram-tasks-awake`, `ram-task-idle-cost`, `task-trim`, `task-trim-idle`, `ram-fit-terms`, the switches of docs/switches.md (`uclamp-rule`, `gpu-down`, `gpu-blob-entries`, `wifi-ring-wait`, `wifi-send-wait`, `wifi-buffers`, `wifi-joined-scan`, `encoder-10bit`, `encoder-warmup`, `decoder`, `decoder-codecs`, `display-post`, `stop-loop`, `takeover-reclaim`, `takeover-swappiness`), `ready-content-wait`, `bridges`, `wifi`, `bluetooth`, `haptics`, `pen`, `encoder` (and the others of this table that say so); the command line wins. Until 2026-11-21 `bridges=`, `wifi=`, `bluetooth=` and `haptics=` here were read and then ignored (only the command line's applied); no image this tree makes sets them |
| `boot-args-add=WORDS` (`--set`, or the image set's `xnuhost.conf`; 2026-11-22) | none | boot arguments added to the guest's, whichever gave those (`--boot-args`, the image set's `boot-args`, or the built-in default), each unless they name it already (`NAME` or `NAME=...`): what `boot-args` says wins, and a profile adds one argument (`oneios-disable-jobs=...`, a debugging switch such as `oneios-ubt=`) without repeating the defaults. A `boot-args=` line that replaced the defaults was the cause of logo-only stalls in the S25 Ultra forks ([s25.md](s25.md)). The command line's `boot-args-add` wins over the image set's; a result over 1023 bytes is refused (exit 2). Ported from `YumikoHayamato/OneiOS` (440abb2). A runtime older than this key refuses it as a bad setting and does not start: stage the runtime before an image set that has it |
| `DIR/overlay/userland/xnuhost.conf` | optional | the same, written by the userland hook: `scale=3` or `scale=2`, the UI scale the image's Data seed was made for (`docs/userland.md` "The screen") |
| (checked and changed by `oneios-tool conf`, 2026-11-25) | | `oneios-tool conf show\|check\|set\|unset\|write\|restore FILE` reads an `xnuhost.conf` as `Config::file` does (C blanks trimmed, `#` comments, `KEY=VALUE`; a line without `=`, a key xnuhost does not read or a value it refuses makes it refuse the whole file and the session does not start) and changes one switch at a time with every other line kept, a kernel switch as a word of the one `boot-args-add=` line; every value is checked against the switch catalog `runtime/switches` (crate `oneios-switches`), which also makes the `switches:` line and which `config::tests::the_catalog` checks against this parser. The OneOS app's Switches screen uses it ([tools/oneios-tool/README.md](../tools/oneios-tool/README.md) "switches and conf") |
| `--width W --height H`, `--hz N` (or `width=`, `height=`, `hz=` in the image set's `xnuhost.conf`) | the display helper's report of the panel | the guest's display mode (section 2.8); for a PC run or another panel |
| `--set panel-fallback=WxH[@HZ]` (or `panel-fallback=` in `xnuhost.conf`) | 1440x3120@120 | the mode assumed only where the panel cannot be read (no helper report in time, no DRM panel; a bench run): section 2.8 step 4. For a phone whose panel is not the S24 Ultra's: the Galaxy S25 and A56 are 1080x2340@120 (`docs/s25.md`, `docs/a56.md`). A read panel still wins |
| `--set display-direct=0` (or `display-direct=0` in `xnuhost.conf`) | 1 | 0: xnuhost copies every frame into the memfd, even for a helper that can copy from guest RAM (section 2.8, direct frames) |
| `--set display-post=apart\|serial` (or in `xnuhost.conf`) | apart | serial: the display helper reads no frame until it has posted the last, the one loop before 2026-11-24 (section 2.8, docs/switches.md) |
| `--set display-present=ahb\|window` (or in `xnuhost.conf`) | ahb | ahb: the display helper posts into write-only AHardwareBuffers on a child of its layer (no CPU cache maintenance of the whole buffer at each lock and post; it falls back to the window by itself when they cannot be had); window: it locks and posts the layer's window as before 2026-11-25 (section 2.8, docs/switches.md) |
| `--scale 2\|3` (`--set scale=2`) | the image set's, else 3 | the UI scale: pixels per point |
| `DIR/os.img`, `systemos.img`, `app.img` | as present | read-only disks, in this order |
| `--data DIR` | none | `data.img`: the Data store, sparse, created on the first start; `unique-chip-id` |
| `DIR/data.seed` | as present | a tar seed is served as the last read-only disk (the guest makes the Data volume and unpacks it); an APFS seed image is copied into a new store |
| `--session DIR` | none | `xnuhost.pid`, `ready`, `return.request`, `display.sock` |
| `--cpus` | the host's cores, at most 8 | 1 to 8 |
| `--ram SIZE` or `--ram auto` (`ram=`) | `auto` | the guest's RAM: a multiple of 2 MiB, 256 MiB to 64 GiB. `auto`: the least of `ram-percent` of the host's `MemTotal`, `MemTotal` less `ram-host-floor`, and what fits beside the session's needs (section 2.2, "What fits"), rounded down to 256 MiB (below). A size given by hand that is more than fits is used, with an error line saying so |
| `--set ram-fit=0\|1` | 1 | 1: `ram=auto` takes no more than what fits (2026-11-14); 0: as before (share and floor only) |
| `--set ram-gpu-reserve=SIZE` | 256M | what fits leaves this for KGSL's own memory and the helpers (the GPU's blobs are in the guest's RAM: section 2.2, "The terms of 2026-11-23") |
| `--set ram-tasks=N` | 400 | what fits is sized for this many iOS tasks at once, 0 to 4096 (2026-11-23) |
| `--set ram-task-cost=SIZE` | 4M | the host's memory for one iOS task outside the guest: its page tables and kernel memory (2026-11-23) |
| `--set ram-tasks-awake=N` | 150 | with `task-trim=1`, what fits counts this many of `ram-tasks` at `ram-task-cost` and the rest at `ram-task-idle-cost`, 0 to 4096 (section 2.2, "The trim"; an estimate until a session's `tasks: trim:` lines measure it) |
| `--set ram-task-idle-cost=SIZE` | 1M | the host's memory for one trimmed iOS task (its threads, its stub region, its mm; an estimate) |
| `--set task-trim=0\|1` | 1 | 1: an iOS task with no thread released for `task-trim-idle` seconds has its host mappings dropped, its page tables and VMAs given back (section 2.6, "The trim"); 0: every task keeps them, and what fits counts every task at `ram-task-cost` |
| `--set task-trim-idle=SECONDS` | 30 | the idle time before a task is trimmed, 1 to 86400; 2 s while host MemAvailable is under the headroom and 512 MiB |
| `--set ram-fit-terms=tasks\|old` | tasks | old: ram=auto with the terms before 3b8742a (headroom 768 MiB locked, 1 GiB unlocked; page tables 8 %; no task cost; no hold share); a key of those given still wins (docs/switches.md) |
| `--set uclamp-rule=work\|all` \| `--set gpu-down=governor\|bottom` \| `--set gpu-blob-entries=N` | work, governor, 65532 | the switches of docs/switches.md for the vCPU clamps (section 2.6e), the GPU level's way down (docs/gpu.md), the 3D blob size limit; each value other than the default is the behaviour before that change |
| `--set wifi-ring-wait=0\|1` \| `wifi-send-wait=0\|1` \| `wifi-buffers=0\|1` \| `wifi-joined-scan=cache\|low\|plain` | 1, 1, 1, cache | the Wi-Fi bridge's switches (docs/switches.md, docs/hardware.md 3) |
| `--set encoder-10bit=main10\|8bit\|nv12` \| `encoder-warmup=0\|1` | main10, 1 | the video encoder's switches (docs/switches.md, docs/hardware.md 7g) |
| `--set decoder=auto\|off` \| `decoder-codecs=h264,hevc,vp9,av1` | auto, all four | the video decoder (2026-11-24): the same device decodes for iOS's paravirtualized VideoToolbox decoder (MediaCodec in the helper; the stand-in on a PC boot); needs `encoder=auto` or `sim` (docs/switches.md, docs/hardware.md 7g "The video decoder") |
| `--set stop-loop=wait\|poll` | wait | read by the fork's stop loop (poll: no `--prop-wait`); xnuhost checks the word only |
| `--set ram-hold-percent=N` | 7 | what holding the guest's RAM costs the host beyond its bytes, % of it, 0 to 50 (2026-11-23) |
| `--set gpu-level=deadline\|hold\|governor` (`gpu-level=`) | `deadline` | the GPU's level (`docs/gpu.md` 2.2b): the top only for a frame that would miss its deadline; `hold`: every render-server frame at the top to 150 ms after it (the rule before 2026-11-15); `governor`: KGSL's governor alone. Passed to the renderer as `ONEIOS_GPU_LEVEL` |
| `--set gpu-priority=0\|1` (`gpu-priority=`) | 1 | 1: each GPU context's KGSL priority from its owner's class (render server and front 4, default and background 8), background contexts at KGSL's lowest level at a still screen while no other context has GPU work queued (`docs/gpu.md` 2.2b); 0: the guest's queue priority, as upstream, and no lowest level (`ONEIOS_GPU_PRIORITY=off`) |
| `--set boot-args-add=oneios-pool-cut=0` (guest boot argument, 2026-11-23) | on | 0: the Metal plug-in's runtime keeps every command buffer a spare command pool made, as before; on: a pool holding more than 8 command buffers past its last 10 s of use is released at its give-back (`docs/gpu.md`, "The command pools held the render server's small blobs"). Read by the plug-in from `kern.bootargs` at its load (`METAL2VULKAN_POOL_USE_WINDOW_S=0`); needs the add-on of 2026-11-23 or later |
| `--set boot-args-add=oneios-idle-spares=0` \| `oneios-bo-cache-idle=0` (guest boot arguments, 2026-11-23) | on, on | idle-spares=0: the Metal plug-in's runtime ages its parked images and buffers and its spare dedicated image blocks only as another is parked, taken or returned, as before (`METAL2VULKAN_IDLE_SPARES=0`); on: its 2 s idle trim frees those past their 1 s and 10 s. bo-cache-idle=0: no idle trim of Turnip's BO cache; on: every 60 s the BOs cached 30 s unasked go back (`docs/gpu.md`, "The idle memory"). Both read by the plug-in from `kern.bootargs` at its load; need the add-on of 2026-11-23 23:00 or later |
| `--set ram-pagetable-percent=N` | 0 (8 before 2026-11-23) | page tables as a share of the guest's RAM on top of `ram-task-cost`, 0 to 50 |
| `--set ram-huge=0\|1` | 1 | 1: 2 MiB host pages for xnuhost's window of the RAM where the host allows them (shmem THP by `MADV_HUGEPAGE`; only with the RAM held fixed); section 2.2, "2 MiB pages" |
| `--set ram-percent=N` | 90 | `ram=auto`'s share of `MemTotal`, 10 to 100 |
| `--set ram-host-floor=SIZE` | 1.5G | what `ram=auto` always leaves the host |
| `--set host-mem-log=SECONDS` | 30 | a `memory: host MemAvailable` line every so often; 0 = only at the start and the stop |
| `--hw-backend samsung\|treble` (`hw-backend=`, also in `xnuhost.conf`) | `samsung` (or the host phone's entry's, [hardware.md](hardware.md) 7h; none sets one yet) | which form of the phone's hardware every bridge uses: today's Samsung paths, or the vendor-neutral Treble interfaces (AIDL HALs through servicemanager, evdev by capabilities, standard sysfs classes by named paths). One line per bridge says which and why (`hw: battery: backend treble (hw-backend=treble)`). [hardware.md](hardware.md) 7e has the audit table |
| `--hw-backend-BRIDGE samsung\|treble` (`hw-backend-backlight=`, `-battery`, `-thermal`, `-haptics`, `-touch`, `-keys`, `-audio`, `-camera`, `-wifi`, `-bluetooth`, `-display`) | unset (`hw-backend`'s) | one bridge's backend, over `hw-backend`; an unknown bridge name is refused |
| `--hw-backend-fallback 0\|1` (`hw-backend-fallback=`) | 0 | a treble bridge whose interface is missing: 0 the bridge stays off, 1 it uses its samsung form; either way one line says what was missing |
| `--set camera-vm=hide\|show` (`camera-vm=`, also in `xnuhost.conf`; 2026-11-23) | `hide` | `hide`: when the camera helper lists a camera, xnuhost adds `cameracaptured` to the first `owner-hide-vmm=` word of the boot arguments (or makes the word), so the kernel answers `kern.hv_vmm_present` 0 to cameracaptured alone: 24A446's CMCapture drops every back stream in a VM ([hardware.md](hardware.md) 7). One line: `camera: cameracaptured is not told of the VM (owner-hide-vmm gets it\|has it): ...`; an error line when the boot arguments or the kernel's 127 characters for the list have no room. `show`: cameracaptured sees the VM (the back cameras dropped), as before 2026-11-23 |
| `--set camera-model=NAME` (`camera-model=`, also in `xnuhost.conf`; 2026-11-23) | `OneiOSAP` | `/product unique-model` when the camera helper lists a camera (MobileGestalt's `HWModelDescriptionForCamera`; CMCapture's per-model folder is NAME less "AP"). 3-15 letters and digits ending in "AP", else refused. `OneiOSAP` is in no table of AVFCapture's AVGestalt, so VirtualAudio starts, and hosted 0088 shows `CMCapture.framework/OneiOS` as a link to the sealed `V54`; `V54AP` (the iPhone's, the name from 2026-11-22) makes VirtualAudio's webcam subport stop all audio, for an A/B boot only ([hardware.md](hardware.md) 7, "The model name stopped all audio"). A change makes MobileGestalt's cache again once (the identity, hosted 0084). One line: `camera: unique-model NAME (camera-model=): ...` |
| `--set haptics-native-impacts=0\|1` (`haptics-native-impacts=`, also in `xnuhost.conf`) | unset | iOS's short taps as the vibrator HAL's own presets (TICK, CLICK, HEAVY_CLICK) instead of 20 ms motor starts, both halves with one setting: 1 asks the haptics bridge for them and adds `oneios_native_impacts=1` after the boot arguments (kept whole); refused (exit 2) without `hw-backend-haptics=treble`, with `hw-backend-fallback=1`, with `haptics=off\|sim` or the bridges off, or when the boot arguments say `oneios_native_impacts=` otherwise. 0: off, whatever the environment says. Unset: `ONEIOS_HAPTIC_NATIVE_IMPACTS=1` in xnuhost's environment decides, as before (the OneOS fork passes none). [hardware.md](hardware.md) 5, "Native impacts". Default off on every phone; host-tested only |
| `--host-device auto\|none\|NAME` (`host-device=`, also in `xnuhost.conf`) | `auto` | the host phone ([hardware.md](hardware.md) 7h): `auto` reads `ro.product.model`, `ro.product.device`, `ro.board.platform`, `ro.soc.model`, `ro.soc.manufacturer` (fallbacks: `/sys/devices/soc0/machine`, `/proc/device-tree/compatible`) and takes the matching entry of `runtime/bridges/hw/host_device.rs` (`galaxy-s24-ultra`, `galaxy-s25-ultra`, `galaxy-s25`, `galaxy-s26-ultra`, `galaxy-s26`, `galaxy-s26-exynos`, `galaxy-a56`); `none`: no entry; a name forces that entry; another name is refused (exit 2). The entry's defaults go under the command line and the image set. One line at the start: `host: NAME (...), matched by ro.product.model=...; read: ...; defaults: ...` (or `host: unknown phone (read: ...): today's defaults`) |
| `--host-props FILE`, `--host-root DIR` (`host-props=`, `host-root=`) | the property area, `/` | a getprop dump read instead of the property area (`[key]: [value]` or `key=value` lines; an unreadable file is refused), and a folder for `/` of the sysfs and proc fallbacks: host tests, or a PC replaying a phone's `adb shell getprop` |
| `--set session-mode=hosted\|takeover` (`session-mode=`, also in `xnuhost.conf`; 2026-11-23) | `hosted` | `takeover`: the opt-in Android takeover (section 2.9d): only with `mode=takeover`, `input=1` and a host-table row that allows it (the S24 Ultra, the S26 Ultra), else exit 2; it grabs the native touchscreen alone (`touch-exclusive` on unless given), the side keys read and never grabbed. `hosted`: the session as before, byte for byte |
| `--host-info` | - | prints `host-info: device=NAME takeover=yes\|no (why)` for this phone and exits (section 2.9d); no image needed. The OneOS switch's preflight reads it |
| `--set takeover-stops=default\|none\|NAME,...` (`takeover-stops=`, also in `xnuhost.conf`; 2026-11-23) | `default` | the Android services the OneOS switch stops in `session-mode=takeover` (section 2.9d): each NAME one of `cameraserver`, `mediaextractor`, `mediametrics`, `media.tuner`, `perfmond`, `wificond`, `netd`, `media`, once; anything else is a bad setting (exit 2). xnuhost only checks it and names it in the takeover line; the switch reads it from the image's `xnuhost.conf` and does the stops. Accepted and unused in `hosted` |
| `--set takeover-reclaim=0\|1`, `takeover-swappiness=N\|keep` (also in `xnuhost.conf`; takeover-ram, 2026-11-23) | `1`, `keep` | the OneOS switch's memory steps in `session-mode=takeover`, after its stops and before xnuhost (section 2.9d, "More RAM in the takeover"): `takeover-reclaim=1` drops the caches (`sync`, `drop_caches` 3) and compacts each zram device; `takeover-swappiness=N` (0-200) sets `vm.swappiness` for the session, One UI's value saved first and written back by every return. xnuhost only checks the words (anything else is a bad setting, exit 2) and names a value that is not the default in the takeover line. Accepted and unused in `hosted` |
| `--prop-wait SECONDS NAME=VALUE...` (first argument only) | - | waits until one of Android's properties NAME has VALUE (prints each such NAME, exit 0), or SECONDS (0 to 3600) pass (exit 1); 2 for a bad command line or a build without Android's property area (section 2.9d). The OneOS stop loop's wait; nothing of a session runs |
| `--ro-bind PATH...` (first argument only) | - | remounts existing bind mounts read-only via `mount(2)` (`MS_BIND \| MS_REMOUNT \| MS_RDONLY`) directly (KernelSU BusyBox 1.36 on S25/S26 cannot match single-file bind mounts in `/proc/mounts`; ported from `burugou05-wq/OneiOS` `e85405f1`) |
| `--set corner-radius=N`, `artwork-subtype=N` (also in `xnuhost.conf`) | `0`, `0` | `/product display-corner-radius` in points (0..200; 0 writes nothing; 34 on `galaxy-s25`) and `/product artwork-device-subtype` (0..10000; 0 picks 2340 on 1080x2340 panels, else 2868; ported from `NoNameBuilder/OneiOS` `120df342`, `2a021be7`) |
| `--set thermal-cap=0\|1` (also in `xnuhost.conf`) | `1` | 1: narrow vCPUs at critical cell temperature (default on `main`, S24 Ultra unchanged); 0: keep every core while still reporting thermal levels to iOS (`NoNameBuilder/OneiOS` `23cb7469`) |
| `--set wifi=auto\|off\|tap\|cell\|sim` | `auto` | `cell` gives iOS a made-up joined open network (`OneiOS uplink`) routed over the tap + `oneios-net.sh` NAT so iOS uses any phone uplink (mobile data included; `NoNameBuilder/OneiOS` `2c7ca0ef`) |
| `--gpu auto\|3d\|2d` (`gpu=`, also in `xnuhost.conf` since 2026-11-21) | `auto` (also the A56's entry since 2026-11-24; `2d` before) | the GPU ([gpu.md](gpu.md) 2): `auto` 3D when KGSL opens; on a host without `/dev/kgsl-3d0` but with an amdgpu render node (the Galaxy A56), the amdgpu renderer when the kernel's gpuaddon carries the `OneiOSRadeonMetal` plug-in for the image set's build and its self-test passes at the start, `oneios-gpu-amd=1` then added to the boot arguments ([a56-gpu.md](a56-gpu.md) 2; `gpu: 3D on amdgpu (NODE): ...`, else `gpu: 2D only: WHY`); when the same gpuaddon also carries `OneiOSRadeonGL.bundle` (the A56's OpenGL ES, [a56-gpu.md](a56-gpu.md) 4a; since 2026-11-25), `oneios-gpu-amd-gl=1` too (`gpu: OpenGL ES: the gpuaddon's OneiOSRadeonGL (...)`, else `gpu: OpenGL ES: WHY: Metal alone, CPU composition`; a conf's `oneios-gpu-amd-gl=0` is kept); `3d` that renderer (or KGSL's) or do not start (`gpu: no 3D: WHY`), `2d` the scanout only and no GPU pool. Before 2026-11-21 an image set's `gpu=` was read and dropped |
| `--set gpu-node=PATH` (`gpu-node=`, also in `xnuhost.conf`; 2026-11-24) | `/dev/dri/renderD128` | the amdgpu render node the amdgpu renderer and its self-test open ([a56-gpu.md](a56-gpu.md) 2); taken only when its DRM driver says `amdgpu` (or `sgpu`, a guess). The command line's wins over the image set's |
| `--set usb-log=SECONDS` | 3 | how often the phone's USB gadget is read for its `usb:` line (section 2.9a), which is logged at the start, on every change and at the stop; 0 = off |
| `--set ready-content-wait=SECONDS` | 120 | `session/ready` comes at the guest's first frame of its own that is not all black, or this long after its first frame of its own when all were black; 0 = at its first frame (the rule before 2026-11-15); 0 to 480 (section 2.8, "the ready rule") |
| `--set ram-meminfo=FILE` | `/proc/meminfo` | where `ram=auto` reads `MemTotal` and `MemAvailable` (tests) |
| `--set ram-giveback=0\|1` | **0** (since fix-ram-locked, 2026-11-15) | 0: the RAM is **held fixed and locked for the session** (section 2.2, "Fixed RAM": prefaulted, `mlock`ed, no free pages given back; the owner's rule: iOS's RAM is never swapped). 1: 0058's way again (faulted in as used, free pages given back, the host may swap it); one setting flips it back |
| `--set ram-ondemand=0\|1` (also in `xnuhost.conf`; takeover-ram, 2026-11-23) | **0** | **Option C, for the owner to choose; against the rule that iOS's RAM is fixed (2026-11-15).** 1: the RAM is not prefaulted; each page is locked as iOS first touches it (`mlock2` `MLOCK_ONFAULT`: the host never swaps a page it holds), iOS's free pages are given back (`ram-giveback=1`'s way, kernel 0058), the valve and the locked headroom stay. With `ram=SIZE` above what fits the top is backed only while iOS uses it; when iOS fills it the valve opens (the host may swap it from there) or the kernel's OOM killer ends xnuhost (section 2.2, "On demand") |
| `--set ram-prefault=0\|1`, `ram-lock=0\|1` | 1, 1 (with `ram-giveback=0`) | prefault the whole memfd on a nice-19 thread, `mlock`ing it as it goes; at the first `mlock` that fails the prefault stops and what is locked stays locked (where it fails at the first step, EPERM or an RLIMIT_MEMLOCK under 16 MiB, nothing is prefaulted); nothing is prefaulted unlocked unless `ram-lock=0`; neither if the RAM is more than fits |
| `--set ram-session-reserve=SIZE` | 1536M where the lock can work (CAP_IPC_LOCK, or no RLIMIT_MEMLOCK), else 1792M (768M and 1G before 2026-11-23) | the headroom: MemAvailable what fits leaves the host at the session's peak, and the level of the pressure line (section 2.2, "What fits"); with the lock never less than `ram-unlock-below` + 256 MiB; the lock is tried only if the RAM is within what fits |
| `--set ram-view=SIZE\|auto\|off` (also in `xnuhost.conf`) | **off** (until a phone boot proves it) | the memory view (section 2.2, "The memory view"): iOS is told SIZE of DRAM (`auto` = 12 GiB, an iPhone18,2's) over the same locked RAM: `hw.memsize_physical`, an iPhone18,2's per-process limits scaled to SIZE, and room in iOS's compressor for SIZE (`kern.compressor_pool_multiplier`). The RAM itself stays `ram=` (with `ram=auto`, the most that fits locked) |
| `--set ram-unlock-below=SIZE` | 256M | with the RAM locked, the valve: the host's `MemAvailable` under this (read once a second by the main loop, and before each prefault step) unlocks it once for the rest of the session; with the lock at least 128M (0 or less is raised to 128M, with a line) |

**The guest's RAM (`ram=auto`, the owner's decision of 2026-11-12: iOS gets at least 90% of the phone's RAM).**
In a session One UI's apps and system_server are gone, but the host still needs memory beside the guest: the guest gets
`min(ram-percent% of MemTotal, MemTotal - ram-host-floor, what fits)`, rounded down to 256 MiB, where what fits
(2026-11-14, section 2.2) is what MemAvailable at the start holds beside the session's needs outside the guest. On the
phone (MemTotal 11,084 MiB) the first two gave 9,472 MiB, and Linux swapped 2,126 MiB of it out (section 2.2, "What
fits"). A host where that leaves the guest less than 1 GiB is refused (exit 2, "give --ram SIZE"); a host
with no `/proc/meminfo` gets the old 4 GiB. By default (since fix-ram-locked, 2026-11-15) the RAM is held fixed and
locked for the session (section 2.2, "Fixed RAM"); with `ram-giveback=1` it is a memfd that is never populated or
locked: the host gives it pages only as the guest touches them (`ram_discard` gives them back). The log says what was
chosen, and then what the host has left:

```text
memory: ram=auto: host MemTotal M MiB, MemAvailable A MiB; the guest gets G MiB: the least of 90% (.. MiB), MemTotal less the host's floor of 1536 MiB (.. MiB) and what fits beside the session's needs: F MiB (MemAvailable A MiB less 1024 MiB headroom, 256 MiB for the GPU and 8% of the guest for page tables), in 256 MiB steps (what fits decides)
memory: host MemAvailable A MiB of M MiB; the guest's RAM holds H of G MiB; given back N MiB of free pages in C calls (F failed, T ms)
memory: the guest's RAM: R MiB in the host's RAM, W MiB swapped out by the host; host swap S of Z MiB used, page tables P MiB
                                                                                     every host-mem-log seconds, and at the stop
```

The second line (2026-11-13): `R` is the memfd's pages in the host's RAM (`mincore` over xnuhost's mapping,
`discard::ram_resident` in `src/discard.rs`, 64 MiB per call; since fix-ram-locked only over the part that is not locked: the
locked part is resident while the process's `VmLck` covers it, so with the whole RAM locked there is no `mincore` at
all), `W` = `H` - `R`: pages the guest touched that Linux swapped
out (zram on the phone; `st_blocks` counts them as held). A guest page that is swapped out costs a fault and a
decompression in whichever vCPU or task touches it next. `S`, `Z` and `P` are `/proc/meminfo`'s SwapTotal - SwapFree,
SwapTotal and PageTables (every iOS task is a Linux process with its own page tables over the memfd). First seen on the
PC (2026-11-13, 4 GiB guest, the box under other agents' load): `2989 MiB in the host's RAM, 1058 MiB swapped out by the
host; host swap 3670 of 15871 MiB used, page tables 332 MiB`.

`H` is the memfd's resident pages (what the guest has touched and not given back); `N` what the kernel gave back
with `ram_discard` since the start (section 2.2, "Free pages back to the host"), in `C` host calls that took `T` ms. xnuhost never changes its own
OOM score (the way back to One UI, section 2.9): if the host runs out and its low-memory killer ends xnuhost, the
watcher returns to One UI, the safe failure. The GPU's host allocations (KGSL, the renderer) come out of the floor
too: if `host MemAvailable` runs low in a session, lower `ram-percent` or raise `ram-host-floor` in the image set's
`xnuhost.conf` (no new runtime needed).

Log: standard error (the switch appends it to `session/xnuhost.log`). Lines `xnuhost I/E/D` are the host's,
`guest` lines are the kernel's console, `xnuhost !!` lines come from signal context.

**How xnuhost ended, always in the log (2026-11-23).** On the phone at 19:06 that day xnuhost ended with no line at
all: no `stopping:`, no panic, no `xnuhost !!` (`out/session-logs/2026-11-23-mac-s1904/`). Two lasting lines now
say how it ended, whatever it was:
- **xnuhost's last words (the runtime after v0.1.19, `cpu.rs` `last_words`, `xh_core::log::fatal_line`).** A signal whose
  default action ends the process and that xnuhost takes nowhere else (SIGABRT: bionic's abort, Rust's
  `panic=abort`, the allocator; SIGSYS; SIGUSR1, SIGUSR2, SIGALRM, SIGVTALRM, SIGPROF, SIGXCPU, SIGXFSZ, SIGSTKFLT,
  SIGIO, SIGPWR; each only where it still has its default action) goes through the vCPU signal entry (the host's
  TPIDR_EL0 and x18 first) to a handler that writes, async-signal-safe (fixed buffers, `write(2)`, no lock):
  `xnuhost !! fatal signal 6 (SIGABRT) on thread 'NAME' (tid T): code C, sent by pid P uid U` (si_code <= 0: a
  process sent it; for abort() that is xnuhost's own pid) or `..., address 0x...` (one the kernel raised), then
  `xnuhost !!   pc, sp`, `  x30, esr`, and xnuhost's own frame records (`  host frame: fp, lr`, 16 at most; on a vCPU
  in kernel code `  in the kernel: cpu, pc` instead). The faults on a host thread or in a host call (`fault on a host
  thread: signal, address`, `fault inside a host call: ...`) write the same `fatal signal` line and frames after
  their own three. Then xnuhost dies of the signal itself (its default action, the signal unblocked, raised on that
  thread): the inner shell below sees the signal, not an exit code (the faults ended with `_exit(5)` before, because
  the raise stayed blocked in the handler). SIGKILL cannot be caught: for it there is only the line below.
- **The inner shell's record (the OneOS app after 0.5.80; `oneos/interface.md` 4).** The switch's inner shell waits for
  xnuhost and writes `oneios inner: DATE TIME xnuhost PID ended: exit N` or `... ended: killed by signal 9 (SIGKILL)`
  as the log's last line, and `session/xnuhost.exit`. The watcher copies the record into `return.log` with what the
  host saw right after (the OOM killer's and lmkd's lines that name xnuhost, xnuhost's own logcat lines, the memory
  pressure, the session cgroup's `memory.events`), lines `watcher: after xnuhost's end: ...`. `oneios-tool log
  summary` prints the record as its row `how xnuhost ended`, and its `the end` row is the head line of the last
  `xnuhost !!` group (the `fatal signal` line), not its last indented line.

Stop: `SIGTERM`, `SIGINT` or `SIGHUP`: tasks are killed, the Data store is synced, exit 0.

The return (graceful shutdown): a stop signal is how the fork returns to One UI (its `oneios_return.sh` sends
`SIGTERM`, then `SIGKILL` after 20 s). With `return-graceful-ms` > 0 (**default 0 = the immediate kill as before**, because the guest
half is not yet tested end to end; set it to e.g. 10000 to turn the wait on) and iOS up (a first frame has been
shown), the first stop signal **asks iOS to shut itself down** first, so
it saves what a clean power-off saves (SpringBoard's teardown persists its boot overlay for the next boot; without
this, every session ends as a crash from iOS's view, and a boot on a charger plays the "plugged in" chime — see
[hardware.md](hardware.md) 7c). xnuhost sends the ask on the bridge (`OB_CTL_SHUTDOWN`, the guest's bridge driver
hands it to `oneios_power_request()`), keeps the vCPUs running, and waits up to `return-graceful-ms` (an absolute
deadline). The wait ends at once on any of: iOS halting itself (its PSCI SYSTEM_OFF / halt — a restart in the window
is taken as the return, not a re-exec), the deadline, a **second** return signal (the chord pressed again, which the
fork turns into another `SIGTERM`), or a panic (it sets the stop directly). Then the stop runs exactly as before.
The way back never depends on the guest acting: the deadline is ours and the fork's `SIGKILL` is the backstop under
everything; a guest with no shutdown helper (or any image before this) simply reaches the deadline and is killed.
Log: `return: graceful shutdown asked; iOS has up to N ms`, then `return: iOS shut down in N ms` or
`return: graceful shutdown timed out after N ms; stopping`. State machine: `runtime/xnuhost/src/ret.rs`
(unit-tested). Since 2026-11-24 (hosted 0106, from the other A56 fork) the kernel's `oneios_power_request()` sends logd
`SIGTERM` (line `oneios-power: return asked: SIGTERM to logd (N process(es))`), so iOS's unified log reaches the Data
store before the kill; iOS does not halt, so the wait runs to its deadline. Only the galaxy-a56 host row turns the
window on (`return-graceful-ms=4000`, [hardware.md](hardware.md) 7h); the S24 Ultra's default stays 0. **The rest of the guest half is the Mac's to build:** a guest helper that `oneios_power_request()` reaches which
runs iOS's clean power-off (a halt, so xnuhost exits and the return completes, not a reboot). The host half above
works and is tested without it; the end-to-end (iOS saving its overlay) is not yet verified. The kernel's own
shutdown path does nothing to the host: a guest halt or PSCI SYSTEM_OFF/RESET only sets `xh_stop` (no `reboot(2)`,
nothing under `/sys/power`; section 2.9), which the return completes.

| Exit code | Meaning |
| --- | --- |
| 0 | the guest shut down (halt, PSCI SYSTEM_OFF), a stop signal, or a return asked from inside (then `session/return.request` exists) |
| 2 | bad arguments or a failed setup (no kernel, bad image, no memory) |
| 3 | the guest asked for a restart (PSCI SYSTEM_RESET, `XH_HALT_RESTART`) and xnuhost could not start it again: it restarts in place (`exec` of itself, same pid; `XNUHOST_RUNNER` names an emulator in front on a PC) and gives up after 8 restarts in a row with no `ready` |
| 4 | kernel panic, or a kernel fault the kernel did not take |
| 5 | a fault in xnuhost's own code (it re-raises the signal) |

## 2. Architecture

```text
One UI's Linux kernel (unchanged; 39-bit user addresses, 4 KiB pages, PAC, seccomp)
├─ xnuhost                         one process = "the machine"
│   ├─ RAM: one memfd, mapped once; its address is also its physical address
│   ├─ vCPU threads               run the kernel's code natively; SIGRT+1/+2 are the interrupt line and timer
│   ├─ SIGSEGV on the I/O aperture -> device emulation (GICv3, PL011, virtio-mmio block/input/GPU)
│   ├─ SIGILL                      -> EL1 system registers, PSCI, WFI, TLBI emulated
│   └─ host threads               main (stop signals), factory reader, display socket, evdev reader
├─ xnuhost-tasks (factory)         forked before any thread or RAM mapping; never execs
└─ one process per iOS task        fork of the factory; only the stub (3 KB) inside
    ├─ service thread             carries out map / unmap / protect / new-thread orders
    └─ one Linux thread per iOS thread; every svc (seccomp -> SIGSYS) and fault (SIGSEGV...) stops it
       in the stub's handler, which hands its registers to xnuhost through a shared page
```

### 2.1 Loader (`runtime/loader`)

A port of DarwinBoot with no firmware under it (C until 2026-11-23, now the crate `xnuloader`, `runtime/loader/src/`):

- `macho.rs`: arm64/arm64e `MH_EXECUTE` and `MH_FILESET` (`LC_FILESET_ENTRY`, `com.apple.kernel` checked against
  the collection's segments), every offset bounds-checked; a symbol lookup. Fixups are **not** applied: XNU
  rebases and signs its own chains after entry, as under DarwinBoot.
- `adt.rs`: Apple device tree in memory: build, parse, edit, flatten.
- `machine.rs`: the tree of xnuhost's board, a port of UniXNU-droidvm's `make-droidvm-adt.py`: `/chosen` (256
  seed bytes, CHRP NVRAM proxy, dram, security properties, root matching, MAC addresses), `/cpus`, `/psci`,
  `/defaults` (with the iPhone18,2's I/O throttle since 2026-11-22: `kern.io_throttle_period_tier1` 5,
  `_tier2` 15, `_tier3` 20 and `kern.io_throttle_window_tier3` 100 ms; the hosted disks answer DKIOCISSOLIDSTATE 0, so
  XNU had used its rotating-disk table, 40/85/200 ms periods in 25/100/500 ms windows, `docs/userland.md` 11),
  `/options`, `/product` (copied from the identity tree, minus exclaves; APPLE2 and MTL2,2 of `graphics-featureset-fallbacks`
  only with `astc=0`, `Machine::astc`, 2026-11-25; `artwork-scale-factor` is the UI scale), `/buttons`, `/bluetooth`, `/arm-io` with the GICv3, `wlan`, `uart0` and one node per virtio window
  under the names and `compatible` strings UniXNU's drivers match; `/chosen/xnuhost` for a hosted kernel.
  **The cameras (2026-11-22, `Machine::cameras`, only with an `oneios,camera` device):** `/arm-io/isp` with
  `camera-rear` = 1 when the camera helper lists a back camera (wide, ultra-wide or a telephoto) and `camera-front` = 1
  when it lists the front one (u32, the iPhone18,2's values; no `compatible`, no `device_type`: no driver matches it),
  an empty `/product/camera`, and `/product unique-model` = `Machine::unique_model` (xnuhost's `camera-model=`;
  None: an identity tree's own value, else `machine::CAMERA_MODEL` "OneiOSAP"; the C's `xl_machine.unique_model` and
  `XL_CAMERA_MODEL`; "V54AP" from 2026-11-22 to
  2026-11-23, which stopped all audio: [hardware.md](hardware.md) 7). What 24A446's libMobileGestalt reads them for: `rear-facing-camera` = isp `camera-rear`,
  `front-facing-camera` = isp `camera-front`, `still-camera` (Camera.app's `SBAppTags`; SpringBoard shows the app
  only with it) = either, `video-camera` = `/product/camera` is there, and `HWModelDescriptionForCamera` (also
  `...ForAnalytics`, `...ForAudio`, `...ForPowerPerf`, `...ForAutomatedTesting`: the same answer) = `unique-model`,
  which CMCapture takes less "AP" as the folder of its per-model tables (`CMCapture.framework/OneiOS/`: hosted
  0088's link to the sealed `V54/`). xnuhost waits
  for the helper's list before it builds the tree, asleep on the bridge's condition variable, at most 3 s (the phone:
  170 ms after the helper's start); `camera: iOS is told of a rear camera and a front camera (3 cameras from the
  helper)`. Without the camera device, or with an empty list, the tree is as before.
  With a camera listed xnuhost also hides the VM from cameracaptured (`camera-vm=`, the options table).
  **`/chosen oneios-identity` (`machine::identity`, 2026-11-22):** 8 bytes, FNV-1a 64 of what MobileGestalt can
  read that stays the same from boot to boot: the root's own properties, the `/product`, `/arm-io`, `/buttons` and
  `/bluetooth` subtrees, `/chosen`'s own properties and `/chosen/xnuhost`'s `display-*` ones; not `random-seed`,
  `dram-base`, `dram-size`, the GPU and media pools, the memory map, xnuhost's vector (its ASLR) or `/defaults` (its
  limits follow the RAM). xnuhost writes it on the finished tree (after a shipped tree's nodes). Hosted 0084 keeps
  MobileGestalt's cache while it stays the same and removes it once when it changes ([userland.md](userland.md),
  "MobileGestalt's cache follows the tree"). xnuhost logs it: `identity: <16 hex digits> (/chosen oneios-identity: ...)`.
- `boot.rs`: `boot_args` (1024-byte command line of Darwin 23+, or the 608-byte one), the sidecar checks
  (trust caches, packed RAM disk `DBRDISK1`, GPU addon, root policies) and the placement in DarwinBoot's
  order: kernel, `boot_args`, tree, RAM disk, trust caches, addon, policies; memory-map entries filled.
  `RAMDisk1` mirrors `RAMDisk` (same address and size) only when the tree names it: iOS 16 userlands look for the
  ramdisk under that name too (port-a56, docs/a56.md). xnuhost names it exactly when the image set's build is
  20F66; a 27 kernel's tree is byte-identical to before.
- iOS 16.5's kernel switches (2026-11-24, docs/a56.md "The second port"): for an image set of build 20F66, xnuhost
  adds to the guest's boot arguments each word of `IOS16_BOOT_ARGS` (`runtime/xnuhost/src/config.rs`) they do not
  name already: `oneios-hid16=1` (hosted 0103, touch), `oneios-grant=configd:com.apple.networking.ionetworkstack.user-client`
  (0104, Wi-Fi's en0), `oneios-ls-keep=3` (0105) and `oneios-disable-jobs=...` (0053: its three defaults and eight idle
  jobs of the other A56 fork). An image made before them needs no remake (roottweak writes the same words for a new
  one; a test keeps the two lists equal). One line: `ios16: image set 20F66: added to the boot arguments: oneios-hid16
  oneios-grant oneios-ls-keep oneios-disable-jobs` (or `... name every iOS 16.5 switch already`). Any other build: nothing.
- iOS 16.5's product, serial number and CoreImage fixup (2026-11-24, fix/a56-ios16-faults, docs/a56.md "The faults of
  the v0.1.24 boot"): for an image set of build 20F66 xnuhost also sets the loader's `Machine::product_id` to
  `IOS16_PRODUCT_ID` (the iPhone X's `/product product-id`, MobileGestalt 16.5's product key) and
  `Machine::serial_number` to `IOS16_SERIAL` (`ONEIOS16A56`, made up). The loader writes `/product product-id` and
  `product-description` (= `product-name`, "OneiOS guest") only without an identity tree, and the root's
  `serial-number` as 32 bytes in the layout XNU's IODTPlatformExpert turns into `IOPlatformSerialNumber` (bytes
  13..21, then 0..3: `machine::serial_number_property`, which refuses anything but 11 characters of A-Z and 0-9).
  Line: `ios16: /product product-id fd079b29... (the iPhone X's, MobileGestalt's product), serial-number ONEIOS16A56`.
  And it appends `config::ios16_fixups` to `/chosen text-fixups` (after the image's own): `+[CIContext
  internalGLContextWithOptions:]` answers no context (docs/gpu.md, "iOS 16.5 without a Metal device"); line `ios16: 1
  text fixup(s) added: ...`. `--set ios16-fixup=0` (or `ios16-fixup=0` in the image's `xnuhost.conf`) leaves it out.
  With the A56's OpenGL ES (since 2026-11-25: xnuhost found `OneiOSRadeonGL.bundle` in the gpuaddon of the amdgpu
  renderer it picked, and the boot arguments' first `oneios-gpu-amd-gl=` word is `=1`: `config::ios16_gl_driver`) the
  fixup is left out, because the bundle answers CoreImage's EAGLContext. Line: `ios16: CoreImage's GL fallback fixup
  left out: oneios-gpu-amd-gl=1, OneiOSRadeonGL answers its EAGLContext`.
  Any other build: nothing, and the tree is byte for byte as before. No image remake: the runtime alone.

The difference from a firmware loader is one parameter: `virt_base`. DarwinBoot passes the kernel's link
base and the kernel maps itself there. xnuhost passes the address where the window **already is**, a
multiple of 2 MiB away from the link base, which is the slide the kernel finds when it rebases itself.

### 2.2 Memory

- One memfd is the guest's RAM (its size: section 1, `ram=auto`). xnuhost maps it once at `0x2000000000 + (link base mod 2 MiB)` (retrying
  higher), read-write-execute. **Physical address = that address** (`boot_args.physBase == virtBase`):
  one number names a byte for the kernel's pointers, its physical addresses (DMA addresses in virtqueues,
  page tables it may still write) and, minus the base, the offset a task maps.
- The top of the memfd, outside the kernel's window, is the boot framebuffer (`boot_args.Video`); the main
  loop shows it until the guest presents a frame of its own (not later: from then on the frames are the guest's). The guest's display driver maps it too (`kernel_map` takes any page of
  the memfd; kernel `0020`) and presents it itself through the virtio GPU.
- The board's device windows are a `PROT_NONE` reservation at their physical addresses
  (`0x08000000..0x0b000000`), so a device register's physical address is also its pointer and every access
  traps.
- **What follows the RAM's size** (checked for `ram=auto` by the test kernel, `check-arm64.sh check`'s second boot): the
  window is the RAM less the boot framebuffer and the GPU's pool (8 MiB, the renderer's response buffers; fixed: the
  GPU's blobs are guest pages, udmabufs of the window, and grow with it); `boot_args.memSize`, `/chosen dram-size` and
  `xh_info.ram_size` all name the window; `kern.maxvnodes` in `/defaults` is the window / 64 KiB + 1024. The kernel's
  own range below starts at the next GiB after the RAM, so a larger guest leaves it less: 102 GiB with 9.5 GiB of RAM
  (107 GiB with 4 GiB). XNU sizes its zone map and its kernel ranges from `memSize` inside that range. Run on the PC (2026-11-12, the
  Linux-built kernel under qemu): `ram=auto` gave 14,080 MiB and the kernel booted through launchd to SpringBoard with
  no panic (`zones` 24G there and at 2 GiB; `kmem_ptr_range_0` and the two data ranges 18G each); 9.5 GiB, which is less,
  was not run on its own.
- **Free pages back to the host (kernel 0058, 2026-11-12).** The goal: iOS keeps all of its ram=auto memory; the host
  gets back only pages iOS has freed. The kernel gives back pages on XNU's own free queue (never cached, compressed or
  wired ones) with `ram_discard`: `discard::ram_discard_fd` (`src/discard.rs`) punches a hole in the memfd
  (`FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`, allowed under the GPU's shrink seal), only inside the kernel's window
  (never the boot framebuffer or the GPU's pool above it), host-page aligned, else -EINVAL; the range then reads as
  zero in every mapping and the host holds nothing for it until it is touched again (a read through a shared mapping
  fills it too). xnuhost counts calls, bytes, failures and time for the `memory:` line. The kernel's side (what is
  given back, when, in what order) is in `kernel/patches/oneios-hosted/README.md`, 0058; its decisions are
  `include/xnuhost/free_report.h`, which the kernel carries as a copy (`runtime/abi/tests/headers.rs` checks they are one file and that `runtime/abi/src/free_report.rs` mirrors it). Run on
  the PC at `ram=auto` (14,080 MiB): the host held 4,973 MiB after 16.5 min instead of 12,266 MiB, with XNU's own
  view of its memory unchanged. A task that still has the page mapped (an unmap xnuhost has queued, 2.6) loses it in
  the punch and cannot run before the queue is carried out (test kernel). Not run on the phone. With `ram-giveback=0`
  (below) every `ram_discard` is refused with -EPERM.
- **What fits (`ram=auto`, fix-ram-fit, 2026-11-14; on by default, `ram-fit=0` turns it off).** The phone's session of
  2026-11-14 (main `9f6dc38`, MemTotal 11,084 MiB, `ram=auto` = 9,472 MiB, `ram-giveback=1`): Linux swapped iOS's RAM
  out to zram and the UI froze (Settings showed no Wi-Fi list updates; Preferences' threads in the same places 10 s
  apart; iOS's own log, `oneios-tool ioslog checks`: `[app<com.apple.Preferences>:327] Failed to terminate gracefully
  after 5.0s!`, then `Watchdog Violation (0x8BADF00D)`: Preferences hung and was killed by iOS's watchdog while the host
  had 2.1 GiB of iOS's RAM swapped out). At +5491: `host MemAvailable 860 MiB`, `holds 7263 of 9472 MiB`, `2126 MiB swapped out by the host`, `host
  swap 2882 of 12287 MiB`, `page tables 575 MiB`, while iOS saw `memorystatus available 5461 MiB`. Every swapped page
  iOS touches is a fault and a decompression; at +6060 the host swapped 12,190 pages in and 10,291 out in 5 s (about
  9.5 MB/s in, 8 MB/s out) at 800 % CPU (226 % sys): **the swap was also the main heat source, so a RAM that fits and is
  held fixed is the thermal fix too.** Outside the guest the host used about 11,084 - 860 - (7,263 - 2,126) = 5,087 MiB,
  zram's compressed pool and the 575 MiB of page tables included. So `ram=auto` now also takes no more than what fits:
  - Since 2026-11-23 (the terms below, "The terms of 2026-11-23"): `fit = (MemAvailable - ram-session-reserve -
    ram-gpu-reserve - tasks) / (1 + (ram-hold-percent + ram-pagetable-percent) / 100)`, where `tasks` is
    `ram-tasks-awake * ram-task-cost + (ram-tasks - ram-tasks-awake) * ram-task-idle-cost` with the trim (the default
    since 2026-11-23, "The trim" below) and `ram-tasks * ram-task-cost` without it. The rest of this point and the next
    ones are the history of the terms. As of 2026-11-14:
    `fit = (MemAvailable - ram-session-reserve - ram-gpu-reserve) / (1 + ram-pagetable-percent / 100)` (`config::ram_fit`,
    `config.rs`), MemAvailable read at xnuhost's start, which is after the fork stopped One UI's zygote and
    system_server (`oneos/interface.md` 4) and before the guest takes anything.
  - The page tables: every iOS task is a Linux process with its own page tables over the memfd, and the same pages are
    mapped by many tasks (the shared cache, the shared region): 575 MiB for 7,263 MiB held, 7.9 %, so 8 % of the guest.
    Almost all of it is the tasks' (fix-ram-hugepages, below: xnuhost's own window costs size / 512, 14 MiB at 7 GiB),
    so 2 MiB host pages do not change this term (`config.ram_lock_fit` is the locked size that fits, in 256 MiB steps).
  - The GPU: KGSL's own memory for xnuhost (contexts, ringbuffers, the import tables and IOMMU tables of the blobs, about
    1 % of the blobs' bytes [estimate]): 256 MiB. The `memory: fixed RAM` line prints KGSL's per-process files, so the
    next phone boot gives the steady figure.
  - The headroom (`ram-session-reserve`): 768 MiB where the lock can work (the default since fix-ram-locked, below), else
    1 GiB: surfaceflinger and the composer, the helpers, the iOS tasks' kernel memory (each iOS thread is a Linux thread),
    a working page cache for the host, the way back; unlocked also zram's pool and working set for the guest's pages.
  - **For this phone** (as written on 2026-11-14; the 2026-11-15 session measured 8,561 MiB at the start, and the lock's
    figure is below): the session's start MemAvailable is not in the notes. For 8.5 GiB at the start [assumed] what
    fits is (8,704 - 1,024 - 256) / 1.08 = 6,874 -> **6,656 MiB**; for 8 GiB, 6,400; for 9 GiB, 7,168. The check from the
    other side, the session itself: without swap the host's use outside the guest is about 5,087 - 1,000 (zram's pool
    [guess]) = 4,087 MiB at a 9.2 GiB guest, so a guest that is never swapped is at most about 11,084 - 4,087 - 512
    (the kernel's watermarks and a working page cache) = 6,485 MiB, which agrees within one 256 MiB step. The next phone
    boot's `ram=auto` line prints the real figure (`what fits beside the session's needs: F MiB (MemAvailable A MiB ...)`).
  - The session of 2026-11-15 (main `3233fed`, `ram=auto`, `ram-giveback=1`) gave the start figure: `ram=auto ...
    MemAvailable 8561 MiB; the guest gets 6656 MiB` ((8,561 - 1,024 - 256) / 1.08 = 6,741 -> 6,656). Linux still swapped
    iOS's RAM: `the host swaps iOS's RAM: 138 MiB (host MemAvailable 2452 MiB) ... swap-outs 37.9 MB/s`, then 370 MiB
    (1,485) and 604 MiB (873). The host's `/proc/meminfo` at 370 MiB: MemAvailable 2,298,532 kB, Cached 7,278,352,
    Shmem 5,379,532, AnonPages 100,192, Mlocked 4,596, SwapFree 11,711,604 of 12,582,908. So Linux swapped the shmem
    memfd while about 1.9 GB of other page cache (Cached - Shmem) was there, by Samsung's swappiness
    (`/proc/sys/vm/swappiness` is not readable even as root): a size that fits is not enough, only the lock stops it.
    The heat came with it: SoC 59.4 C at the start, 70.6 C at the first swap, 83.8 C later (host sys 200 %, swap in 179
    out 2,364 KiB/s, xnuhost 171 %). Hence the next point.
  - **With the lock (fix-ram-locked, 2026-11-15; the default).** A locked RAM is neither swapped nor reclaimed, so the
    bound is MemAvailable at the start less what the session needs outside the guest; the page cache in MemAvailable is
    taken (the host reclaims cache, never the locked RAM). The terms: page tables 8 % of the guest (575 MiB for 7,263 MiB
    held on 2026-11-14, 7.9 %); KGSL 256 MiB [estimate]; the headroom 768 MiB = the valve's 256 MiB and 512 MiB for what
    else grows (the iOS threads' kernel stacks and task structures, the tasks' mappings, surfaceflinger's and the display
    helper's buffers, a working page cache for the host's own code and the image files, the fork's watcher and the stop).
    No zram term: the guest's pages are never in zram. What the 2026-11-15 meminfo says about the rest [estimate]: at
    the start the host used 11,084 - 8,561 = 2,523 MiB that MemAvailable does not count; at 370 MiB swapped it was
    11,084 - 2,245 - 5,253 (Shmem, the guest's resident pages and a little other shmem) = 3,586 MiB, of which zram's pool
    for 851 MiB of swap is about 300 MiB [guess at about 3:1] and the page tables of a 5.6 GiB mapped guest about 445 MiB
    [7.9 %], which leaves about 320 MiB for KGSL, the kernel and surfaceflinger: inside the 256 + 512 MiB the fit leaves.
    **On this phone:** (8,561 - 768 - 256) / 1.08 = 6,978 -> **6,912 MiB** (one 256 MiB step more than 6,656). With it
    locked, at a like point of the session the host keeps about 8,561 - 6,912 - 553 (page tables) - 320 = 776 MiB of
    MemAvailable, 520 MiB above the valve. Where the lock cannot work (no CAP_IPC_LOCK and a finite RLIMIT_MEMLOCK: the
    PC) the headroom stays 1 GiB and `ram=auto` gives what it did. The `ram=auto` line says which: `less 768 MiB headroom
    with the lock` or `(the lock is not expected to work here)`.
  - **The next phone boot** (the owner's: `ram=auto ram-giveback=0 ram-prefault=1 ram-lock=1`, which is now also the
    default). Whether the phone can lock [inference]: xnuhost runs as root in KernelSU's domain with its full capability
    set, so CAP_IPC_LOCK lifts RLIMIT_MEMLOCK, and KernelSU's policy allows the `ipc_lock` capability. The lines to read:
    the facts line's `CAP_IPC_LOCK yes`; `ram=auto: ... the guest gets 6912 MiB ... less 768 MiB headroom with the lock`
    (for a start MemAvailable of about 8.5 GiB); `memory: the guest's RAM is held: N MiB prefaulted in T ms, locked
    (mlock: the host cannot swap it)`; `memory: fixed RAM: held and locked; ... locked N MiB`; no `memory: the host swaps
    iOS's RAM` line; the kernel's `oneios-mem: free pages given back to the host: no`; host MemAvailable staying above
    256 MiB (no `unlocked for the rest of the session`); the `thermal: 30 s:` line with `swap in 0 out 0 KiB/s` or near
    it; and `stopping:` right after the return's SIGTERM.
  - **The terms of 2026-11-23 (fix/ram-auto-sizing; the default).** Two phone sessions ran out of host memory with the
    terms above and the image's `ram-session-reserve=2G` (written on 2026-11-16 after a kill): 18:24 and 19:04 (runtime
    0.1.19; `out/session-logs/2026-11-23-mac-s1824/xnuhost-final.log`, `.../2026-11-23-mac-s1904/xnuhost.log`).
    `ram=auto` gave 5632 MiB both times (what fits 5686 and 5793 MiB from MemAvailable 8445 and 8561). The host's
    MemAvailable fell to 1077-1265 MiB, it swapped in and out at 31-42 MB/s, and at 19:06 xnuhost ended without a line
    (the kernel's OOM killer fits [guess]; section 2.9 records how it ends since df485a0). What the logs measure:
    - **Holding the RAM**: MemAvailable at the start less the guest less MemAvailable once it is held and locked: 8561 -
      5632 - 2540 = 389 MiB and 8445 - 5632 - 2480 = 333 MiB, 5.9-6.9 % of the guest. `ram-hold-percent=7`. Measured
      at one size; that it scales with the size is a guess.
    - **The tasks**: after the lock MemAvailable fell 1463 and 1321 MiB to the end while the host swapped out 277 and
      369 MiB more (zram: about 2/3 of that is RAM it freed, at 3:1 [guess]), so the session took 1648 and 1567 MiB
      beside the guest with 353 and 342 live tasks (`tasks: 60 s: ... live N`). The host's page tables were 728 and
      683 MiB of it (60 -> 788 and 743 MiB), 2.0-2.1 MiB a task: every iOS task is a Linux process mapping the memfd
      (2.6), so they follow the tasks, not the guest's size (the old 8 % of the guest counted 450 MiB). The rest, about
      900 MiB, is what no line named [guess: the tasks' kernel memory: the VMAs of their many small maps, 2.6, and
      their threads' stacks; the pressure line below names it on the next boot]. Less the 256 MiB of
      `ram-gpu-reserve`: 3.8-3.9 MiB a task. `ram-task-cost=4M` for `ram-tasks=400` (the most at once was 354 and 346),
      1600 MiB. `ram-pagetable-percent` is 0 now (kept for configs that set it: a share of the guest on top).
    - **The GPU**: no term of its size. Every blob is a udmabuf over the guest's memfd ([gpu.md](gpu.md) 2, "No
      separate GPU pool"): the 1633 MiB of `gpu: 10 s: host blobs` at 19:06 (3133 MiB at 18:28) were KGSL's
      `imported_mem` 1640 MiB, the guest's own pages, and KGSL's own kernel memory was 1.7 MiB. So the guest's size
      bounds the GPU's memory and nothing of ours caps it (the owner's rule); `ram-gpu-reserve=256M` stays for KGSL's
      tables over the blobs (about 1 % [estimate]) and the helpers.
    - **The headroom** (`ram-session-reserve`, what the host keeps at the peak): the host swapped in 31-42 MB/s at
      MemAvailable 1077-1265 MiB and under 0.2 MB/s from 1503 MiB up (the `memory:` and `thermal: 30 s:` lines). With
      the lock 1536 MiB (was 768 MiB); unlocked 1792 MiB (was 1 GiB).
    - **On this phone**: (8561 - 1536 - 256 - 1600) / 1.07 = 4830 -> **4608 MiB** (and 4722 -> 4608 from 8445), 1024
      MiB less than these sessions. With the measured needs (holding scaled to 4608: 318 MiB; the tasks 1648 MiB) the
      host keeps about 8561 - 4608 - 318 - 1648 = 1988 MiB at the peak, 452 MiB above the headroom; the old 5632 MiB
      left 893 MiB without the swap. The `ram=auto` line now ends `... 400 tasks at 4096 KiB (1600 MiB: their page
      tables and kernel memory) and 7% of the guest for holding it); the host keeps about 1774 MiB at the peak` (the
      model's own estimate). **The image's `ram-session-reserve=2G` must go** (`overlay/userland/xnuhost.conf`): kept,
      it gives 4352 MiB, because the 2 GiB counted what the tasks' term counts now. Host tests:
      `config::tests::ram_fit_sessions_2026_11_23` (both logs' numbers), `ram_config`, `ram_arithmetic`. On the phone
      (2026-11-23 20:23, the session the Mac copies to `out/session-logs/2026-11-23-mac-s2023/`): a window of 4534 MiB,
      host swap near 0 (`thermal: 30 s: ... swap in 16-27 out 0 KiB/s`), but iOS starved: `oneios-mem: free 1 MiB ...
      wired 2443-2570, compressor 162-244 MiB holding 571-832 MiB`, `jetsam: available 846-880 MiB`. The owner: far too
      slow. Hence the trim (next point), which cuts the tasks' term at its cause.
  - **The trim (2026-11-23, fix/task-trim; the default, `task-trim=1`).** What each task costs the host, from the
    2026-11-23 logs: every page the kernel fills for a task is an `mmap` of one 16 KiB page of the memfd in that task's
    Linux process (`task_map`, 2.6). The kernel's `oneios-perf:   pmap:` line counts 1,556,799-1,697,416 user maps per
    30 s at the start of the 19:40 session (`out/session-logs/2026-11-23-mac-wifi/`). Two pages merge into one VMA only
    when they are next to each other both in the address and in the memfd, which the guest's free pages seldom are, so
    each map is about one VMA and one share of 4 KiB page tables, held for the task's whole life, busy or idle. The
    `memory:` lines: page tables 558 MiB for 204 live tasks (2.7 MiB a task) and 818 MiB at 19:43; 728 and 683 MiB for
    353 and 342 tasks at 19:06 and 18:30 (2.0-2.1 MiB). The other 900 MiB no line named is, by this arithmetic, their
    VMAs [inference: 2 MiB of page tables is 512 tables of 2 MiB of address each; at about 16 mapped pages a table that
    is 8,000 VMAs of about 250 bytes, 2 MiB; the 30 s line now prints the unreclaimable slab, where VMAs live, to show
    it]. These mappings are the pmap's TLB (2.6), so a task none of whose threads has run for `task-trim-idle` (30) s
    gets one unmap of its whole iOS range from its service thread, and Linux frees its VMAs and page tables there at
    once; what the task touches again it faults in (one fill each, as after any flush). An iOS session has a few busy
    tasks and hundreds of daemons that sleep for minutes, so most of the tasks' term goes back to the host. The trim
    has a pressure mode: with host MemAvailable under the headroom and 512 MiB, tasks idle 2 s are trimmed (and 30 s
    again 128 MiB above that). Section 2.6, "The trim", has the code's side.
    - **What fits with it**: of `ram-tasks` (400), `ram-tasks-awake` (150) at `ram-task-cost` (4 MiB) and the rest at
      `ram-task-idle-cost` (1 MiB): 850 MiB instead of 1600. Both numbers are estimates: no log yet counts the tasks
      that ran in 30 s (the trim's own line does from now on), and 1 MiB for a trimmed task (its threads' kernel stacks,
      its stub region, its mm: about 0.3 MiB [estimate]) keeps room in case part of the 900 MiB is not the VMAs.
      `task-trim=0` counts all 400 at 4 MiB again.
    - **On this phone**: (8561 - 1536 - 256 - 850) / 1.07 = 5531 -> **5376 MiB** (and 5423 -> 5376 from 8445), 768 MiB
      more than the 4608 MiB above; the model leaves 8561 - 5376 - 376 - 256 - 850 = 1702 MiB at the peak, 166 above
      the headroom. The `ram=auto` line: `... 400 tasks, 150 awake at 4096 KiB and 250 trimmed at 1024 KiB (850 MiB:
      their page tables and kernel memory) and 7% of the guest for holding it); the host keeps about 1702 MiB at the
      peak`.
    - **The lines** (all lasting and cheap): at the start `tasks: trim: a task with no thread released for 30 s has its
      host mappings dropped (task-trim=1): ...`; once `tasks: trim: the first: pid N after T s with no release: page
      tables K KiB -> k KiB (VmPTE), the unmap U us`; every 60 s `tasks: trim: 60 s: +N trimmed (+P under pressure, F
      failed), page tables M MiB given back (K KiB a task, k KiB left), the unmaps T ms (U us a task, most V ms); now L
      live: A awake (a release in the last 30 s; most B since the last line), R trimmed` (B is the figure
      `ram-tasks-awake` stands for); on each change of the pressure mode `tasks: trim: host MemAvailable N MiB: under
      the headroom and 512 MiB: tasks idle 2 s are trimmed` or `... tasks idle 30 s are trimmed again`. The 30 s
      `memory: the guest's RAM:` line now ends `page tables P MiB (xnuhost's own X MiB; K KiB a live task (L live)); 2
      MiB pages: ...; unreclaimable slab S MiB, kernel stacks K MiB`.
    - **What it costs**: a trimmed task's next run faults its working set in again: one fill per page (about 4 us on the
      test kernel's bench, 2.6a; a fault on the phone costs about 30 us, CLAUDE.md), so a daemon that wakes after a
      minute and touches 300 pages spends about 10 ms more. The unmap itself is one `munmap` in the task, on a nice-10
      thread of xnuhost (`xnuhost-trim`), never on a vCPU. Fewer VMAs on the memfd also make every task's `mmap` and
      `munmap` cheaper: each takes the memfd's `i_mmap_rwsem` and inserts into or removes from its interval tree of all
      the tasks' VMAs [inference from Linux 6.1's mm; not measured].
    - **Holding the RAM** (`ram-hold-percent=7`): 389, 333 and 409 MiB at 5632 MiB on 2026-11-23 (8535 - 5632 - 2494
      at 19:40). Not the 4 KiB pages: xnuhost's own page tables over the window are 11 MiB (the 2 MiB pages point
      below). A new line, once when the prefault is done, names it by user: `memory: holding the guest's RAM: host
      MemAvailable A -> B MiB for N MiB held, +C MiB beyond it (P % of it; ram-hold-percent 7); by user since the start
      (MiB): free -..., the guest's RAM +..., page cache ..., the rest ...`. The term stays 7 % until that line says
      what it is.
    - **The guest's wired 2.5 GiB** (`oneios-mem: ... wired 2443-2570`): mostly the GPU's buffers, which are guest
      pages wired by IOSurface and IOGPU (`oneios-gpu-memory: ... live 1931-2044: own 1001-1284 MiB, wired 164-172
      MiB` at 19:42-19:45, up to 2305 + 918 MiB at 18:30). Not changed here (the GPU path, [gpu.md](gpu.md)); the
      kernel's zones were not measured apart.
    - Tested: host tests `task::tests::the_trim_takes_only_idle_tasks_once`, `task::tests::the_trim_line`,
      `config::tests::ram_fit_with_the_trim` (5376 MiB from both starts), `config::tests::ram_config` (the defaults,
      `task-trim=0`, the keys by hand), `config::tests::host_change_by_user`; the test kernel's fifth boot under
      qemu-aarch64 (`check-arm64.sh check`: `task-trim-idle=1`, boot argument `testkernel-trim`: a task's code and data
      pages mapped and loaded, 4 s idle, then its run faults at its pc, filled faults at the data page, filled reads
      what the kernel wrote since; `tasks: trim: the first: ...`). **Not run on the phone.**
  - A size given by hand (`ram=`) is used as given; if it is more than fits, an error line says so: `memory: ram=N MiB is
    more than this host can hold without swapping it (F MiB fit): the host will swap iOS's RAM; give ram=auto or ram=..M`.
  - When the host does swap iOS's RAM (held less resident, every 5 s), a plain line, once past 64 MiB and again at each
    further 256 MiB: `memory: the host swaps iOS's RAM: N MiB (host MemAvailable M MiB); host swap-ins I pages/s (X MB/s),
    swap-outs O pages/s (Y MB/s)` (the rates are the host's, all processes, `/proc/vmstat` pswpin and pswpout over the
    last 5 s). Heat has its own line (fix-thermal-log); this one is only about iOS's RAM.
  - **The host's pressure (2026-11-23, `MemoryWatch::pressure_watch`, `config::Pressure`).** Every 5 s the main loop
    reads `/proc/meminfo` and `/proc/vmstat` once. When MemAvailable falls under `ram-session-reserve`, or the host
    swaps in 4 MiB/s or more over the 5 s, one error line, once per event:
    `memory: the host is short of memory: WHY; its RAM by user (MiB): the guest's RAM 5632, page tables 788, ...; page
    tables K KiB for each of N live tasks; GPU blobs B MiB, inside the guest's RAM; host swap S MiB used; xnuhost's
    oom_score O (oom_score_adj 500: the kernel's OOM killer ends the highest first)`. The users, largest first: the
    guest's RAM (the memfd's held bytes), page tables, the unreclaimable slab (SUnreclaim: the tasks' VMAs are there),
    kernel stacks, anonymous memory, the page cache (Cached less Shmem), other shmem, the reclaimable slab, zram's pool
    (`/sys/block/zram*/mm_stat`), KGSL's own (`/sys/class/kgsl/kgsl/proc/PID/kernel`) and the rest that meminfo does not
    name (drivers' pages, dma-bufs, vmalloc). When it has been calm for 30 s (MemAvailable at least the headroom and
    128 MiB, swap-ins under 1 MiB/s): `memory: the host's shortage is over after T s: lowest MemAvailable M MiB,
    swap-ins X MiB in all; now MemAvailable A MiB`. Nothing in between: the `memory:` lines every `host-mem-log`
    seconds go on. `oneios-tool log summary` shows a pressure line with no "over" after it as `host short of memory at
    the end`. The zram, KGSL and OOM score reads happen only when a line is written. Host tests:
    `config::tests::host_pressure` (the 19:04 session's MemAvailable and swap figures), `oneios-log`'s
    `summary_says_how_xnuhost_ended`.
  - **The OOM score (task 3 of 2026-11-23, read, not changed).** xnuhost's `oom_score_adj` is 500 (as read on the phone,
    [hardware.md](hardware.md); xnuhost does not set it and never lowers it, CLAUDE.md) and its RSS holds the whole locked RAM, so the kernel's OOM killer ends xnuhost
    before any One UI service that is left: that is the session's end and the way back, by design ("The way back",
    below). The pressure line prints `oom_score` and `oom_score_adj` as read, so a log shows it. The fix is the size:
    enough room that the OOM killer has no reason to run.
  - **The host's breakdown (takeover-ram, 2026-11-23, `src/hostmem.rs`; lasting, read only).** The takeover session of
    2026-11-23 21:11 had MemTotal 11,084 MiB and MemAvailable 8,458 MiB at xnuhost's start, and no line said what held
    the other 2,626 MiB. Two lines now do. At the start, before the guest's RAM is held: `memory: the host at the start
    (before the guest's RAM is held; MiB): MemTotal T, MemFree F, MemAvailable A; anonymous .., page cache .. (dirty ..),
    shmem .., mlocked .., unevictable ..; the kernel's: unreclaimable slab .., reclaimable slab .., kernel stacks ..,
    page tables .., vmalloc .., per-CPU ..; swap U of S used (swap cache ..); other keys: KEY N, ...; zramN: P MiB of
    pages in R MiB of RAM (compressed .., most .. MiB; same pages ..), backing_dev ..; dma-buf heap pools .. MiB; KGSL
    (MiB): page_alloc .., ...; min_free_kbytes .., watermark_scale_factor .., swappiness .., the zones' high watermarks
    .. MiB; processes by anonymous memory (MiB): NAME[ xCOUNT] ANON[ (swap .., shmem ..)], ... (14 names); all N
    processes: anonymous .. MiB, swap .. MiB`. `other keys` is every /proc/meminfo key the line does not name otherwise
    with 1 MiB or more, as the kernel names it (CMA, Samsung's `Rbin*`, `ION_*`, a vendor's GPU keys). The processes are
    grouped by name (the iOS tasks share one), largest anonymous memory and swap first: the kept services of a takeover
    and their swapped pages, surfaceflinger, the composer, the display helper. At the stop, for the session's lowest
    MemAvailable (the pressure watch's 5 s look, a new look at each 64 MiB further down, the processes walked at most
    once a minute): `memory: the host at its lowest MemAvailable (M MiB, T s after the start; MiB): ...` with the same
    parts. Cost: a few small files per look, a handful of looks in a session, one /proc walk at the start and at most
    one a minute while MemAvailable falls. Host tests: `hostmem::tests` (the meminfo part with CMA and RBIN keys, zram's
    mm_stat, the zones' high marks, processes by name).
  - **On demand (`ram-ondemand=1`, option C, takeover-ram 2026-11-23; off by default, for the owner to choose, against
    the rule of 2026-11-15 that iOS's RAM is fixed).** The owner's goal of 2026-11-23 ("at least 10 GB of RAM for iOS")
    is more than fits locked on this phone (section 2.9d, "More RAM in the takeover"). This option lets a guest be
    larger than what fits: `ram-giveback=1`'s way (not prefaulted, no `XH_FEATURE_RAM_FIXED`, iOS's free pages given back
    by 0058's reporting) with the whole window locked on fault (`ramfix::lock_on_fault`: `mlock2(MLOCK_ONFAULT)`, the raw
    system call; bionic has `mlock2` only from API 30). A page iOS touches comes in locked, so the host never swaps a
    page it holds; a page never touched costs nothing; a page given back (a hole punched) is freed, and comes back
    locked when touched again. What stops a shmem page of the memfd from going to zram is the lock alone: the memfd's
    pages are shmem, which Linux swaps like anonymous memory; `oneios_session` has no `memory.swap.max` (the facts line
    prints `-`), and xnuhost writes no cgroup file. What happens when iOS fills it: iOS keeps few free pages (its file
    cache takes the rest: the 21:11 session's 4.3 GiB guest had free 677 MiB 69 s after the start and 31 MiB at 132 s,
    with 1.7-2.7 GiB of file pages), so a guest larger than what fits is filled within minutes of use. Then, in order: the host's MemAvailable
    falls, the trim's pressure mode trims every task idle 2 s, the pressure line names the users, the host reclaims
    page cache and swaps the kept services; under `ram-unlock-below` the valve unlocks the RAM once (from there the host
    may swap iOS's pages: against the rule, but the way back fits); past that the kernel's OOM killer ends xnuhost (the
    largest RSS, `oom_score_adj` 500) and the watcher returns to One UI, as for any end of xnuhost. `ram=auto` with it is
    still what fits; the size above that is `ram=SIZE` by hand, and the start says so: `memory: ram=N MiB with
    ram-ondemand=1 is X MiB more than fits locked beside the session's needs (F MiB): ...`. Lines: `memory: ram-ondemand=1
    (option C, against the fixed-RAM rule): the guest's RAM, N MiB (X MiB of it more than fits locked beside the
    session), is faulted in as iOS uses it and each page is locked as it comes in ...`, and the `memory: fixed RAM:` line
    says `on demand, each page locked as it comes in, free pages given back (ram-ondemand=1)` (`on demand, unlocked by
    the valve` after it opened); its `in the host's RAM` is a mincore of the whole RAM. Host tests:
    `ramfix::tests::lock_on_fault_takes_pages_as_touched` (a real `MLOCK_ONFAULT` memfd on the build laptop: nothing in
    before a touch, two touches bring two pages, a punched hole frees one, the valve's munlock), `config::tests::ram_config`
    (the setting, the image set's, the locked headroom, a size above what fits taken). **Not run on a phone.** The
    measured alternative inside the rule is `ram-view=` (above): iOS is told 10-12 GiB over the locked RAM and its own
    compressor holds the rest.
- **Fixed RAM (`ram-giveback=0`, `src/ramfix.rs`, fix-fixed-memory, 2026-11-13; the default since fix-ram-locked,
  2026-11-15, the owner's rule that iOS's RAM is never swapped; `ram-giveback=1` turns it off).** The owner's requirement:
  iOS's RAM must not grow and shrink during a session. The phone's c2d281a session showed why: the memfd was faulted in
  bit by bit (4,904 -> 6,010 -> 6,254 MiB), 527 MiB of free pages given back in 22,688 calls, then 669 MiB of it swapped
  out by the host (zram) with the host's MemAvailable at 842 MiB: every step a fault in a vCPU or a task. With
  `ram-giveback=0`:
  - xnuhost sets `XH_FEATURE_RAM_FIXED` in `xh_info.features` and refuses `ram_discard` (-EPERM, counted). A kernel
    without 0067 stops giving back after the first refusal (0058's "free pages are no longer given back"); 0067 (in the series
    since 2026-11-14) never asks and keeps 0058's free-queue order (freed pages first: it costs nothing, and where the
    lock fails it keeps iOS on pages the host has kept resident).
  - A thread `xnuhost-ram` (SCHED_OTHER, nice 19, never realtime) prefaults the whole memfd in 16 MiB steps from the
    moment it is mapped, while the boot goes on: `mlock` of each step (which faults it in and locks it). On the first
    failure (EPERM: no CAP_IPC_LOCK; ENOMEM: RLIMIT_MEMLOCK or no memory) the errno is logged, **the prefault stops**
    and the rest is faulted in as iOS uses it; the boot goes on. What was locked before the failure stays locked
    (fix-ram-locked, re-checked: until then it was unlocked again, "all or nothing", which only made those pages
    prefaulted and unlocked, the very pages the host swaps first; the valve covers a partial lock as a whole one). Where
    the lock fails at the first step (EPERM, or an RLIMIT_MEMLOCK under one 16 MiB step: the PC's 8 MiB) nothing is
    locked and nothing is prefaulted, the clean fallback: `not locked (mlock: ...), so not prefaulted: the guest's RAM is
    faulted in as iOS uses it` (the RAM stays fixed: free pages are not given back; boots C and D below found that giving
    them back instead where the lock fails changes nothing on the PC). Before each step the prefault reads MemAvailable: under `ram-unlock-below` it stops there
    (`stopped at N MiB: host MemAvailable M MiB is under ram-unlock-below`). The
    lock is tried only if the RAM is within what fits (above); if it is not, nothing is prefaulted. Why not prefault
    without the lock (fix-ram-fit, 2026-11-14): a prefaulted page iOS has not used is the coldest page there is and the
    first the host swaps; the PC boot B below swapped 841 MiB of a prefaulted, unlocked 4 GiB guest in 15 minutes, boot A
    (faulted in as used) 6 MiB. Only `ram-lock=0` with `ram-prefault=1` still prefaults unlocked (one read per host page:
    a read of a hole through the shared mapping allocates a zeroed page; not `fallocate`, whose pages stay "not up to
    date" and are not counted by `mincore`).
  - The valve: while locked, the main loop reads MemAvailable once a second; under `ram-unlock-below` (256 MiB) it
    unlocks the RAM once (the host may swap it again, 0058's way; nothing is given back) and logs it. On the main thread
    (nice 0) since fix-ram-locked: before, the prefault's thread ran it at nice 19, which eight busy vCPUs at nice 0 can
    starve for seconds, exactly when the valve is wanted; the thread now ends with the prefault (and no longer wakes ten
    times a second). With the lock the valve is always on: `ram-unlock-below` under 128 MiB (0 included) is raised to
    128 MiB with a line, and what fits leaves at least the valve's level plus 256 MiB (`ram-session-reserve` is raised
    to that), so the valve does not open in a normal session and the way back still fits when it does.
  - **The way back (safety).** xnuhost runs as root in cgroup `oneios_session` with `oom_score_adj` 500 (unchanged). If
    the host runs short with the RAM locked, the kernel first reclaims the other processes (file cache, then their
    anonymous pages to zram: surfaceflinger, the HALs, the fork's watcher may slow down; daemons at -1000 are never
    killed); lmkd has no registered process to kill once system_server is gone; the kernel's OOM killer then picks the
    largest badness, xnuhost (the whole RAM is its RSS, plus adj 500), and the watcher returns to One UI, the existing
    safe failure. The valve is there so that this thrash phase is not reached in the first place. The return frees
    everything at once: the memfd's last mapping and descriptor go with xnuhost, the task factory and the tasks (the
    exit or the cgroup kill; mlock ends with the mapping), before the watcher's wait returns (the host test kills a
    child holding 256 MiB and checks Shmem falls back). Nothing is written outside the process: no rlimit, no cgroup
    file (`memory.swap.max` of `oneios_session` would be a fork-side backup), no sysctl.
  - Log: `memory: the guest's RAM is held for the session: N MiB, prefaulted on a thread at nice 19 ..., locked as it goes
    (mlock)|why not; RLIMIT_MEMLOCK .., CAP_IPC_LOCK .., cgroup .. (memory.max .., memory.high .., memory.swap.max ..)`,
    then `memory: the guest's RAM is held: N MiB prefaulted in T ms, locked (mlock: the host cannot swap it)|L MiB of it
    locked (mlock: ERRNO past that; ...)|not locked (mlock: ERRNO), so not prefaulted: ...`, and every host-mem-log
    seconds a third `memory:` line: `memory: fixed RAM:
    held and locked|held, locked in part|...; prefaulted N of N MiB, locked L MiB (VmLck; host Mlocked M MiB); give-back refused C calls; since the
    last line: host swap-ins +I, swap-outs +O pages, xnuhost major faults +F[; KGSL: name value, ...]` (KGSL's files under
    `/sys/class/kgsl/kgsl/proc/PID`, as KGSL prints them: the host's own GPU memory for xnuhost). At the stop:
    `memory: the guest's RAM: N MiB held to the end, L MiB of it locked; the exit gives all of it back to the host`.
  - **Cheap checks (fix-ram-locked).** The 5 s swap check and the host-mem-log lines read nothing they do not need:
    while the whole RAM is locked the 5 s check reads nothing at all (before: `/proc/self/status`, `/proc/meminfo`,
    `/proc/vmstat` twice, `/proc/self/stat`, an `fstat` and a `mincore` over the whole RAM, every 5 s); the mincore
    covers only the part that is not locked, in both places (none while all of it is locked and `VmLck` covers it);
    the host-mem-log lines read `/proc/meminfo` once (before five times) and `/proc/vmstat` once (before twice).
    Measured on the PC: a `mincore` over 2 GiB of resident memfd 4.8 ms, so about 16 ms for 6.9 GiB (more on the
    phone's cores [inference]), every 5 s and again every 30 s; a read of `/proc/meminfo` 9.5 us.
  - Tested: the host tests (`test_ramfix`: prefault keeps every byte; a lock failing part way with ENOMEM keeps what is
    locked and stops, and the valve then unlocks it; at once with EPERM a clean fallback with nothing prefaulted; the
    valve as the main loop runs it (not before the prefault is done, once, from a meminfo file); the prefault stopping
    short when MemAvailable is under the valve's level; a real lock within the PC's 8 MiB RLIMIT_MEMLOCK shows in VmLck
    and the valve undoes it; past the limit a clean fallback; the thread, the refusal, the return. `test_config`: the
    new defaults, the lock's 768 MiB headroom where the lock can work and 1 GiB where it cannot, the fit with the
    2026-11-15 numbers (8,561 MiB -> 6,912 MiB locked, 6,656 MiB unlocked), the valve's floor and margin, one setting
    back to 0058's way; `xh_proc_values` on meminfo and vmstat files; C tests of 2026-11-15, ported 1:1 to the tests in `src/ramfix.rs` and `src/config/tests.rs`, with `xh_core::fs::proc_values`), and the test kernel under qemu-aarch64
    (`check-arm64.sh check`'s second boot; on 2026-11-15 the C Makefile's `make check-arm64`; it runs with the defaults: the refusal, the prefault, the sizing line, and on the PC the
    clean fallback, `not locked (mlock: Out of memory), so not prefaulted`: RLIMIT_MEMLOCK 8 MiB, no root). **Not
    run:** the phone (where xnuhost is root: the lock itself).
  - **PC boots, before and after** (fix-ram-fit, 2026-11-14; qemu-aarch64, 8 vCPUs, `--ram 4G`, `out/image-pc/24A446`, a
    fresh Data store each, the kernel of main `f7bb1c3` plus 0067 (`kernel.kc` `e128c27b`), boot arguments of
    `docs/building.md` 5, 960 s each through `/tmp/oneios-qemu.lock`, the PC shared with other agents). A is
    `ram-giveback=1` (the default), B `ram-giveback=0`. The PC cannot lock (RLIMIT_MEMLOCK 8 MiB, no CAP_IPC_LOCK), and
    this xnuhost (before the change above) then prefaulted the rest unlocked:

    | | A (ram-giveback=1) | B (ram-giveback=0, prefaulted, not locked) |
    |---|---|---|
    | kernel 0067's start line | `free pages given back to the host: yes (the host does not hold the RAM fixed)` | `no (the host holds the RAM fixed)`; no `ram_discard` call (`give-back refused 0 calls`) |
    | prefault | none | 4,096 MiB in 2,120 ms; `not locked (mlock: Out of memory)` |
    | launchd, SpringBoard (s after the start) | 689, 744 | 302, 353 |
    | held by the host at 300 / 600 / 900 s (MiB) | 311 / 1,174 / 2,667 | 4,096 / 4,096 / 4,066 |
    | swapped out by the host at 300 / 600 / 900 s (MiB) | 0 / 0 / 6 | 0 / 0 / 790 |
    | `the host swaps iOS's RAM` lines | none | at 87, 347, 597, 841 MiB (+625 to +945 s), swap-outs 2,662-4,412 pages/s (11-18 MB/s) |
    | page tables at 900 s (MiB) | 115 | 297 |
    | host MemAvailable at the end (MiB) | 9,345 | 4,020 (other agents' jobs took 3 GiB of shared memory meanwhile) |
    | given back | 0 MiB (nothing above keep in 16 min) | none (refused: 0 calls) |

    Neither panicked. The boot times differ by load (A ran right after another agent's boot), not by the setting
    [inference]. A stopped on SIGTERM in 0.65 s (`held to the end ... the exit gives all of it back`); B's log ends 2 s
    before the TERM with no `stopping:` line, and `timeout`'s KILL ended it 10 s later: cause not known (the PC was
    swapping 18 MB/s then); the fork's way back kills after 20 s either way, and a phone boot should check that
    `stopping:` follows the return's SIGTERM. What the PC shows: with no lock, holding the RAM fixed does not keep it
    in RAM; hence the change above (no prefault without the lock).

    **Boots C and D (fix-ram-locked, 2026-11-15)**: the same PC, `--ram 4G`, 8 vCPUs, a fresh Data store each, a kernel
    of main's series (`kernel.kc` `4fb7be43`, built from `1b5fb9f`; `e128c27b` with today's runtime panicked at +62 s in
    `oneios_native_legacy_display_late_probe` -> `~NativeLegacyDisplay` -> `OSMetaClass::~OSMetaClass`, a kernel and
    runtime of different days, not this change), 960 s each. C ran the new defaults: the lock failed at the first step
    and the RAM was held fixed, faulted in as used (`oneios-mem: free pages given back to the host: no`). D ran a trial
    fallback to give-back where the lock fails (`free pages given back to the host: yes`; not kept, below):

    | | C (held fixed, not locked) | D (give-back) |
    |---|---|---|
    | launchd, SpringBoard (s after the start) | 270, 313 | 273, 316 |
    | held by the host at 300 / 600 / 900 s (MiB) | 2,431 / 4,066 / 4,053 | 2,431 / 4,067 / 4,054 |
    | swapped out by the host at 300 / 600 / 900 s (MiB) | 0 / 0 / 910 | 0 / 20 / 961 |
    | `the host swaps iOS's RAM` lines | 82, 332, 579, 859 MiB from +710 s, swap-outs 8-19 MB/s | 67, 329, 606, 842, 1,098 MiB from +725 s, swap-outs 15-20 MB/s |
    | given back | refused, 0 calls | 0 MiB in 0 calls (nothing above the kernel's keep) |
    | page tables at 900 s (MiB) | 323 | 321 |

    Neither panicked. The host's MemAvailable was 3.6-4.6 GiB during the swapping (other agents' jobs), and the host
    swapped iOS's RAM the same way in both: give-back gave nothing back (iOS kept its pages in use, mostly file cache:
    `oneios-mem: ... file-backed 3215` MiB in C, 3211 in D, near the end), so where the lock fails it does not matter whether the RAM is held
    fixed, and the default stays as asked (`ram-giveback=0`; the lock's line says it failed). On the PC only the lock
    would help, which needs root. The logs end abruptly at `timeout`'s TERM (C: no `stopping:` line; D: `stopping:
    signal 15`, then nothing): `timeout` sends the TERM to its whole process group, so the task factory and every iOS
    task (each its own process) get it at the same moment as xnuhost [inference: the cause]; a SIGTERM to xnuhost
    alone (the test kernel under the same qemu, `ram-giveback` 0 or 1) gives `stopping: signal 15` and a clean exit.
    The phone's check stays: `stopping:` right after the return's SIGTERM.

    `ram-giveback=0` became the default with
    fix-ram-locked (2026-11-15, the owner's rule) before the phone has shown `locked (mlock: the host cannot swap it)`:
    the owner's next boot sets the same settings explicitly, and the phone's 3233fed session showed that without the
    lock Linux swaps iOS's RAM even at a size that fits.
- **2 MiB pages (`ram-huge`, fix-ram-hugepages, 2026-11-15; on by default).** What changed:
  - **The memfd's layout.** The window starts at `0x2000000000 + (link base mod 2 MiB)`, `0x...4000`, and until now the
    memfd's offset 0 was that byte, so address and offset never agreed modulo 2 MiB and Linux could not map any 2 MiB
    page of it with one entry. Now the memfd is 2 MiB longer, its offset 0 is at the 2 MiB boundary below the window
    (`fd_phys` in `src/mem.rs`, 16 KiB below; that head is mapped, never guest RAM), and every offset is `address - fd_phys`
    (the tasks' maps, `kernel_map`, `ram_discard`, the GPU's udmabufs). Measured natively on the PC (a 64 MiB memfd
    mapped `MAP_SHARED` with `MADV_HUGEPAGE`, `shmem_enabled` advise): at offset 0 `ShmemPmdMapped 65536 kB`, VmPTE
    56 kB; at offset `0x4000`, today's layout, `ShmemPmdMapped 0 kB`, VmPTE 176 kB.
  - **The choice** (`ramfix::huge_choose`, `ramfix.rs`; read only, named files, once): `shmem_enabled` (a memfd is on the
    kernel's internal shmem mount, which that file governs), `hpage_pmd_size`, and `/proc/meminfo`'s `HugePages_Free`
    and `Hugepagesize`. With `advise`, `always`, `within_size` or `force` and 2 MiB PMD pages, xnuhost's window gets
    `madvise(MADV_HUGEPAGE)` before the first touch; the prefault and `mlock` then fault it 2 MiB at a time (16 MiB
    steps, eight pages each). Not with `ram-giveback=1` (a give-back's pieces would split every page) or `ram-huge=0`.
    `never` (Linux's default) or `deny`: 4 KiB pages, and the line says the OneOS fork's switch would have to write
    `advise` and its return the old value (`oneos/interface.md` 4); xnuhost writes no global setting. hugetlb
    (`MFD_HUGETLB`) is not used: each of its mmaps must be 2 MiB-aligned, which a task's 16 KiB page is not, and GKI
    6.1's `gki_defconfig` (android14-6.1) has no `CONFIG_HUGETLBFS`; it has `CONFIG_TRANSPARENT_HUGEPAGE=y` and
    `CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y`. The phone's `shmem_enabled` has not been read (no session log here):
    Linux 6.1's default is `never` [so 4 KiB pages until the fork sets `advise`: inference].
  - **What it gains, and what it does not.** xnuhost's window (every access of the kernel, every vCPU) is mapped with
    2 MiB entries: 512 times the TLB reach, and the window's own page tables (size / 512: 14 MiB at 6,912 MiB, 20 MiB at
    10 GiB) gone. The iOS tasks map the same folios 16 KiB at a time, with 4 KiB entries (Linux maps a 2 MiB shmem
    folio with one entry only where a mapping covers the whole aligned 2 MiB), so their page tables and their TLB misses
    stay as they are. They are nearly all of the page tables: on the PC (boot E below) xnuhost's own were 6 MiB of
    the host's 93 MiB at 900 s and the boot's task processes' 49 MiB; the phone's 575 MiB at 7,263 MiB held are
    therefore the tasks' (each task is a Linux process with its own tables for the shared region it maps). So the
    page-table term of what fits (8 %) stays: 2 MiB pages give iOS about 14 MiB of it, under one 256 MiB step.
    A guess about the rest: page tables shared between processes exist in Linux only for hugetlb (PMD sharing),
    which the tasks' 16 KiB maps exclude.
  - **Log.** At the start `memory: 2 MiB host pages: yes, the window is mapped with MADV_HUGEPAGE (shmem THP)|no
    (...); shmem_enabled S, hpage_pmd_size N, hugetlb pool F free of K KiB pages; the memfd's offset 0 is at A, 16 KiB
    below the window`; when the prefault is done and at the stop `memory: 2 MiB pages: xnuhost maps N of M MiB of the
    guest's RAM with 2 MiB entries (ShmemPmdMapped; FilePmdMapped .., AnonHugePages .. MiB); its page tables P MiB
    (VmPTE; 4 KiB entries for the whole window would be W MiB)` (`/proc/self/smaps_rollup` walks every mapping of the
    process, so not periodically); every host-mem-log seconds the second `memory:` line adds `(xnuhost's own V MiB);
    2 MiB pages: host ShmemHugePages H MiB, ShmemPmdMapped P MiB` and the fixed-RAM line `minor +m` faults.
  - **PC boot E** (2026-11-15, 960 s, `--ram 4G`, 8 vCPUs, `kernel.kc` `4fb7be43`, the new layout): launchd at +526 s,
    SpringBoard at +680 s (the PC shared with other agents' boots, and the host swapping 1.7 GiB of iOS's RAM with
    9.5 GiB available), no panic: the moved offsets work for the tasks' maps and `kernel_map`. qemu-user refuses
    `MADV_HUGEPAGE` (`madvise(MADV_HUGEPAGE) on the RAM: Invalid argument`, also for a static test program), so no PC
    boot can show the 2 MiB pages themselves, and under qemu `/proc/self/stat`'s minor faults read 0.
  - **On the phone (2026-11-23, three sessions):** `2 MiB host pages: yes ... shmem_enabled advise, hpage_pmd_size
    2097152`, then `xnuhost maps 0 of 5632 MiB of the guest's RAM with 2 MiB entries`, `ShmemHugePages 0 MiB` all
    session, and the prefault's `minor +1448977` faults: one per 4 KiB of 5632 MiB. So shmem gave no 2 MiB folio at all.
    Linux 6.1's `shmem_is_huge` says no for a process with `MMF_DISABLE_THP` (PR_SET_THP_DISABLE, inherited across fork
    and exec) while `madvise(MADV_HUGEPAGE)` still succeeds; a failed allocation would have given some. A guess at the
    cause, so two things (fix/task-trim): `mem::map` reads `THP_enabled` in `/proc/self/status` and, when it is 0, turns
    it on again for xnuhost alone (`prctl(PR_SET_THP_DISABLE, 0)`, with a line: `memory: this process had transparent
    huge pages turned off (THP_enabled 0, PR_SET_THP_DISABLE from its parent): turned on again for xnuhost (prctl)`);
    and when the prefault is done with none, a line says why: `memory: 2 MiB pages: none: THP_enabled 0|1; since the
    start the host's thp_file_alloc +A, thp_file_fallback +F (tried, no free 2 MiB block|never tried|some given);
    defrag D`. What 2 MiB pages would save is small for the RAM (xnuhost's own page tables, 11 MiB at 5632 MiB) and
    larger for the vCPUs' TLB; the tasks' page tables do not change (they map 16 KiB at a time). Not run on the phone.
  - With the RAM more than fits (`ram=` by hand) the `memory: fixed RAM` line said `held, prefault running` for the
    whole session (no prefault was started); now `held, not locked`.
- **iOS's own compressor and the memory view (`ram-view=`, fix-ios-compressor, 2026-11-15; off by default).** The phone's
  session of main 3233fed logged `oneios-mem: free 277 MiB ..., compressor 0 MiB holding 0 MiB` with many jetsam lines.
  What the code says (xnu-12377 as built here, every file under `osfmk/` or `bsd/`):
  - **The compressor is on, in RAM, as on an iPhone18,2 (proven).** `vm_compressor_mode` is a `TUNABLE_DT` of
    `/defaults kern.vm_compressor` with the default `VM_PAGER_FREEZER_WITH_SWAP` (32) for `CONFIG_FREEZE`
    (`vm_pageout.c`); `vm_config_init` makes 32 compressor active, general swap off, freezer swap on. The PC's log says
    `vm: "vm_compressor_mode" is 32`; 24A446's own DeviceTree has `kern.vm_compressor` = 32 too. No boot argument of
    xnuhost's (`config.rs`) or of the PC's sets `vm_compressor`, and no hosted patch touches the compressor or the pageout
    daemon (only 0057, the swap folder, and 0058/0066/0067, which move free pages and change no count the daemon reads).
    Swap is only the freezer's, a file on iOS's own Data volume (`/private/var/vm`, 0057), as on the phone: never the host's.
  - **What feeds it (proven, `vm_pageout.c`).** Only the pageout daemon (when `vm_page_free_count` falls under
    `vm_page_free_target`, about 31 MiB with the hosted 4 KiB pages) and the freezer. The daemon takes file-backed pages
    while there are more than 2.5 x jetsam's critical threshold of them (`vps_choose_victim_page`,
    `vm_pageout_memorystatus_fb_factor_nr/dr` 5/2), and anonymous pages (which it compresses) below that, or when the
    inactive file pages are under half the file pages. So a guest with a large file cache compresses nothing: on the PC
    (2 GiB and 4 GiB, idle) free sits at the target, the file cache at 1.6-3.2 GiB (the shared cache's files) and
    anonymous memory at 50-400 MiB, `compressor 0 MiB`. Not a hosted fault, the stock policy.
  - **Jetsam (proven, `kern_memorystatus.c`).** It compares `memorystatus_available_pages` (free + file-backed:
    `VM_CHECK_MEMORYSTATUS`; compressed memory is not in it) with thresholds that are shares of `max_mem`: critical 4 %
    above 6 GiB, idle 2x, soft 3x, reaper 19/4x (24A446's DeviceTree gives the reaper 2343 MB of its 12 GiB, the same
    share). The free 277 MiB of the phone's line means the daemon had nothing to do at that moment.
  - **The "jetsam lines" were most likely not kills (proven on the PC, a guess for the phone).** `ioslog checks` counts
    every line with `jetsam` in it (`checks.rs`: `memorystatus: killing|JetsamEvent|jetsam`). On the PC boot A below it
    counted 383 while the kernel's own counters said `kills since boot 0`: 370 were 0076's new lines, the other 13 all
    runningboardd's (`... is not RunningBoard jetsam managed`, `Ignoring jetsam update ...`, and one error, next item).
    The phone's 486 (before 0076) were [guess] the same kind; its next log's `oneios-mem: jetsam: kills since boot` says.
  - **iOS had no jetsam properties (proven on the PC; the phone's image is built the same way).** runningboardd logged
    `Can't find jetsamproperties ('/System/Library/LaunchDaemons/com.apple.jetsamproperties' | '' | 'plist')`: it names the
    file by the device tree root's `target-type`, which the generated tree had only from an identity tree (no image set
    has one, section 1). 24A446's DeviceTree has `target-type` V54 and the OS volume has
    `com.apple.jetsamproperties.V54.plist` (129,739 bytes): its bands and memory limits for apps and daemons were not
    applied. **Fixed:** the loader gives the root `target-type` V54 without an identity tree (`runtime/loader/src/machine.rs`, `src/session/board.rs`).
  - **What the guest lacked (proven).** The guest's `/defaults` had none of the iPhone18,2's per-process limits
    (`kern.max_task_pmem` 3376, `kern.entitled_max_task_pmem` 6144, `kern.entitled_dev_max_task_pmem` 6656 MB in 24A446's
    DeviceTree): a task without a limit of its own (launchd's or RunningBoard's) had none (`task_init`). Since this change
    the loader always gives `kern.max_task_pmem` 3376 (`runtime/loader/src/machine.rs`); a footprint counts the task's
    compressed pages too (`phys_footprint`), so the limit is on what the compressor holds of it as well.

  **Can `hw.memsize` be more than the RAM? No (proven).** On iOS `hw.memsize` is `max_mem` (`kern_mib.c`; macOS reports
  `max_mem_actual` there), and `max_mem` is `mem_size` from `boot_args.memSize` (`arm_vm_init.c`: `gPhysSize = mem_size`,
  `max_mem = mem_size`), every page of which `vm_page_bootstrap` (`vm_resident.c`) puts on the free lists: a bigger
  `memSize` hands XNU pages above the locked RAM, which the host would have to back and could swap (refused, the owner's
  rule). What the code does support: `boot_args.memSizeActual` larger than `memSize` (a real iPhone's DRAM less its
  carve-outs): `mem_actual` and `max_mem_actual` (`hw.memsize_physical`; `scale_setup`'s task and thread limits in
  `startup.c`; `ml_get_booter_memory_size`'s boot-stolen count) follow it, and nothing allocates from it. The jetsam
  thresholds stay shares of `max_mem`, the real RAM, which is right: they measure free and file pages of that RAM. The
  per-process limits are absolute megabytes from `/defaults` (and the jetsam properties' bands from launchd), so they can
  be a 12 GiB device's. The room for the excess is the compressor's: `vm_compressor_set_size` caps the pages it holds at
  its pool (`max_mem`) times `kern.compressor_pool_multiplier` (1 on an iOS above 2 GiB), so at most the RAM's own size
  of original pages; past 98 % of that `vm_compressor_low_on_space` makes jetsam kill (`vm-compressor-space-shortage`).

  **`ram-view=SIZE` (`auto` = 12 GiB), the owner's mode:** the RAM is `ram=` as before (with `ram=auto` and the lock, the
  default, the most that fits locked: `config.ram_lock_fit`, 6.9 GiB on the phone); xnuhost then sets
  `boot_args.memSizeActual` = SIZE (`hw.memsize_physical`), `/defaults` the three per-process limits of an iPhone18,2
  scaled by SIZE / 12 GiB (12 GiB: its own), and `kern.compressor_pool_multiplier` = SIZE / RAM rounded up (2 for 12 GiB
  over 6.9 GiB), so the compressor may hold that much. Lines: `memory: ram-view: iOS is told N MiB of DRAM ... over M MiB
  of RAM ...; its compressor may hold up to K x the RAM's pages`, and an error line when the RAM under the view is not all
  locked (the host could swap it). How much app memory the 6.9 GiB then holds rests wholly on the compression ratio
  (`oneios-mem: compressor: ... ratio R`): about 6.9 - wired - the file floor - the compressor's own pages, plus R times
  those pages. Measured on the PC: below.
  **PC boots (2026-11-15, this Linux PC under qemu-aarch64; load average 33-40 from other agents' builds the whole time,
  so a boot took 50 min to SpringBoard and the app script's taps opened [as far as the logs show] no app).** Same image
  set (`out/image-pc/24A446`), fresh Data store, `--ram 1536M`, `runtime/xnuhost/tools/input-apps.script` with its first
  wait 40 min, 6000 s each:
  - **A (no view; the kernel with 0076, xnuhost with `kern.max_task_pmem` and not yet `target-type`):** `Limiting task physical memory footprint to
    3376 MB`; SpringBoard at 51 min; for the last 50 min `oneios-mem: free 10-14 MiB ... speculative 1068-1131 ...
    compressor 0 MiB holding 0 MiB`, file-backed 1,040-1,218 MiB, anonymous 36-166 MiB, `oneios-mem: compressor: holds 0
    MiB of pages in 0 MiB of RAM ...; room for 2973 MiB of pages, low on space no`, `oneios-mem: jetsam: available
    1050-1356 MiB; at critical 75, idle 151, soft 226, reaper 302 MiB; task limit 3376 MB, entitled 0 MB; hw.memsize 1510
    MiB, memsize_physical 1510 MiB`, `kills since boot 0 (idle-exit 0)`. `ioslog checks`: `jetsam 383` (above). The free
    pages sat at the daemon's target with over 1 GiB of file pages to take first, so nothing was compressed: no ratio.
  - **B (`--set ram-view=2688M`, 1.75 x the RAM as 12 GiB is of 6.9 GiB; stopped at 24 min by hand to rerun it with a
    longer timeout, then not rerun: the queue for the PC):** `memory: ram-view: iOS is told 2688 MiB of DRAM
    (hw.memsize_physical, and an iPhone18,2's per-process limits for it) over 1510 MiB of RAM (hw.memsize); its
    compressor may hold up to 2 x the RAM's pages`, the error line `ram-view over a RAM that is not all locked (1536 MiB
    asked, 0 MiB fits locked)` (no lock on this PC: RLIMIT_MEMLOCK 8 MiB), and in iOS `oneios-mem: jetsam: ... task limit
    738 MB, entitled 1344 MB; hw.memsize 1510 MiB, memsize_physical 2688 MiB` (3376 and 6144 x 2688 / 12288). At this size
    XNU's own multiplier is 2 already (an iOS of 2 GiB or less), so `room for 2973 MiB` is the same as A's: on the phone
    (6.9 GiB, multiplier 1) the view doubles it.
  - Not measured here: a compression ratio (no compressed page in any PC boot), and jetsam kills or apps kept under an app
    load (no kill in any PC boot). The phone's next session gives both from the new lines.
  - A first try of A panicked 30 s in, in the display's late probe (`NativeLegacyDisplay::~NativeLegacyDisplay` from
    `oneios_native_legacy_display_late_probe`, `LegacyDisplayProbe.cpp:644`, a data abort in `OSMetaClass::~OSMetaClass`
    after `virtio-gpu: scanout refresh 0 Hz (EDID 0 Hz)`): not this change's code; the rerun passed it.

- **The kernel's own address space** outside the window (zones, kalloc, stacks, IOKit buffers) is a
  no-access reservation from the end of the RAM, rounded up to 1 GiB, to `0x3c00000000`
  (`xh_info.kva_base/kva_size`). `kernel_map(va, size, prot, phys)` maps RAM pages into it and
  `kernel_unmap` makes a range fault again. The kernel's page tables stay its source of truth; the host
  mapping plays the TLB (map on a fault the table allows, unmap on an invalidation). Tasks work the same
  way through `task_map/unmap`. This makes many small mappings, so xnuhost raises `vm.max_map_count` to
  1048576 at start when it may (it logs the value either way, also in `--selftest`).
- An iOS task starts with an empty address space (the stub unmaps everything but its own 1 GiB region at
  `0x7e00000000`). XNU's pmap fills it with `task_map(va, size, prot, phys)`: an `mmap(MAP_SHARED|MAP_FIXED)`
  of the RAM memfd in the task. Shared memory between tasks, the shared region at `0x180000000` and the
  commpage at `0xFFFFFC000` are all the same call. `task_unmap`, `task_protect`, `ram_discard` (hole punch)
  are the rest of a pmap.
- **Address space**: the S24 Ultra's kernel gives user space 39 bits. The kernel must run in the low half
  (it does: it runs where the window is mapped) and iOS's task map must end below `xh_info.user_va_limit`
  (`0x7e00000000`, 504 GiB).
- **16 KiB pages on a 4 KiB host**: nothing to do in the host. Linux maps at 4 KiB granularity, so XNU may
  use 4 KiB or 16 KiB pages for a task; `host_page_size` is in `xh_info` and a host with pages larger than
  16 KiB is refused by `--selftest`.

### 2.3 Two contexts, host calls

Kernel code keeps its own `TPIDR_EL0` and `x18`; xnuhost's own code needs bionic's. Every host call goes through
an assembly thunk (`src/arch.rs`, `global_asm!`; `arch_aarch64.S` in the C) that saves the kernel's two registers, loads the host's, switches to
a per-CPU host stack, calls the Rust function of the same name in `src/hostcalls.rs` (`extern "C"`), and switches back. Signals enter through a thunk that finds
the vCPU from the signal stack's address (stacks are 1 MiB aligned blocks; the first word is the vCPU)
and does the same swap. So the kernel may keep its current-thread pointer in `TPIDR_EL0`, which lets
Apple's kexts' inlined `mrs xN, TPIDR_EL1` be rewritten to `mrs xN, TPIDR_EL0` in place at prelink.

The contract is one header, `runtime/xnuhost/include/xnuhost/hostcall.h` (freestanding C, ABI 1): entry
registers, `struct xh_cpu`, `struct xh_vector` (32 calls; additions are appended and `vector->size` says which exist; the 32nd, `irq_take`, since fix-mach-rpc 2026-11-15), `struct xh_upcalls`, `struct xh_info`, frames
and traps. The header stays C (the kernel's machine layer includes it unchanged); xnuhost uses its Rust mirror,
`runtime/abi/src/hostcall.rs` (crate `xnuhost-abi`, no_std, `XhCpu`, `XhVector`, `XhInfo`, ...), which
`runtime/abi/tests/headers.rs` checks against the header: every constant, every struct's size and every field's offset.
The task stub's ABI (`stub_abi.h` in the C, the trap page, the deferred queue, the fault rules) has no C header any more:
it is `runtime/abi/src/stub.rs`, shared by the stub and xnuhost.

### 2.4 Interrupts and the timer

- The mask is a flag: the kernel stores 1 or 0 to `cpu->irq_masked` (or executes `msr daifset/daifclr`,
  which is emulated to the same flag).
- The line is `cpu->irq_pending`. A device, another CPU's SGI or the CPU's timer raises it and wakes the
  thread: a futex wake if it idles, `SIGRT+1` if it runs.
- If the vCPU runs kernel code unmasked, the signal handler copies the interrupted registers (integer and
  vector) into a `struct xh_frame` **on the interrupted kernel stack**, masks, and makes the signal return
  into `upcalls->irq(cpu, frame)`. The handler's own stack is free again at once, so the kernel may block or
  switch threads inside the upcall. When the upcall returns, `frame_resume` builds a Linux signal frame
  from the `xh_frame` and calls `rt_sigreturn`, which restores every register and the signal mask in one step.
- If the vCPU is inside a host call, the interrupt is only noted and delivered when the call returns.
- A kernel that moves the interrupted thread to another vCPU inside the upcall does not return from it:
  it calls `frame_resume(the vCPU the thread is on now, frame)` itself, and makes its host calls with
  interrupts masked (the return path of a host call belongs to the vCPU the call started on).
- The timer is CNTVCT_EL0 (the kernel and iOS tasks read it directly; `/cpus` timebase = CNTFRQ_EL0) with a
  per-CPU POSIX timer (`SIGEV_THREAD_ID`) for the deadline; its line is INTID 27, a level, as ArmCpu takes it.
- **The host timer is armed only when the deadline comes earlier** (`timer-coalesce=1`, the default since
  fix/unmap-fanout, 2026-11-24; `cpu.rs` `timer_set`, `timer_signal`). XNU sets a new deadline at nearly every dispatch
  (the next thread's quantum), each one later than the last: on the phone (2026-11-24 01:17) 497,376 sets per 30 s,
  495,092 of them a `timer_settime`. A deadline later than the one the host timer is armed for (`timer_host_at`), or a
  cancel, leaves the host timer as it is. When it fires before the deadline (or after a cancel), the signal arms it for
  the deadline and raises nothing: one `timer_settime` per early signal, no interrupt, and the line still rises exactly
  at the deadline (`timer_due`). An earlier deadline arms the host timer at once, as before. `perf: ... timer_set N (M to
  the host timer, K kept it; R early signals armed it again)`. `timer-coalesce=0`: every new deadline and cancel is a
  `timer_settime` and every signal raises the line (before).
- The controller is a GICv3 model (`src/gic.rs`): distributor and redistributor frames by trapped MMIO, the CPU
  interface by trapped `ICC_*` registers, or the `irq_ack/irq_take/irq_eoi/irq_enable/ipi` host calls (the hosted
  kernel uses `ipi`, `irq_take` and `irq_eoi` for IPIs and the timer since hosted 0081; ArmCpu's trapped registers for
  devices' interrupts: 2.6d).

### 2.5 What is emulated from traps

So that unchanged code keeps working while the kernel's hot paths move to host calls:

| Instruction | Emulation (`src/sysreg.rs`) |
| --- | --- |
| `mrs/msr TPIDR_EL1` | `cpu->tpidr_el1` |
| `msr daifset/daifclr`, `mrs/msr DAIF`, `mrs CurrentEL` | the mask flag; EL1 |
| `CNTV_CTL_EL0`, `CNTV_CVAL_EL0`, `CNTV_TVAL_EL0` | the vCPU's timer |
| `ICC_IAR1/EOIR1/PMR/IGRPEN1/SGI1R/SRE/...` | the GIC model |
| `hvc #0`, `smc #0` | PSCI 1.0: `CPU_ON` (a new vCPU thread at a physical entry), `CPU_OFF`, `SYSTEM_OFF`, `SYSTEM_RESET` |
| `wfi`, `wfe` | idle until the line rises; nothing |
| `tlbi`, `at`, EL1 `dc`/`ic` | nothing (`dc` by set/way: the loop is ended, below) |
| any other EL1/EL2 register (and `TPIDRRO_EL0` writes) | a per-CPU shadow: reads return what was written |

Each emulated instruction is a signal round trip on the vCPU's thread. xnuhost counts them by instruction word and
logs the twelve most frequent at the stop and on SIGQUIT (`emulated: insn 0x... N times (first at pc 0x...)`, `sysreg.rs`
`stats_log`): the first thing to read when an idle guest keeps its vCPUs busy (`docs/hardware.md` 7d).
`dc isw/csw/cisw` (set/way) with `setway-skip` (on by default; `setway-skip=0` in `--set` or `xnuhost.conf` turns it
off) sets its register to the last set and way of its level, so XNU's set/way loop ends after one trap: the guest's
`cpu_idle` flushes L1 by set/way at every idle entry, 1024 traps each, and that alone kept an idle lock screen at 310%
CPU in the vCPU threads (233% with the skip; phone, 2026-11-12).

The shadow does **not** give a write its hardware meaning: page tables, vectors and PAC key registers
have no effect. Loads and stores on the I/O aperture are decoded (xh-core's `runtime/core/src/insn.rs`: register, immediate, pre/post
index, pairs, sign extension) and run against the device models.

### 2.5a The thermal step

`runtime/bridges/thermal` reads the phone's temperatures every 5 s from the start. Since fix-ram-locked (2026-11-15; the
owner's rule: all CPU cores) it gives one step, at a critical temperature only: the case (lrp) at 50 C or the cell at
50 C (before, steps 1-3 from heavy pressure up: the case at 44 C, the SoC at 95 C). Then `src/session/bridges.rs`' `thermal_step`
narrows the vCPU threads' affinity (`CpuPlan::cap` of `runtime/bridges/cpu/topology.rs`, step 2: only the slowest P domain and the E cores, cpu0-1 and
5-6 here) and bumps `task::PLACE_EPOCH`, so
`thread_run` places each iOS thread again at its next run (`task.rs`). Only xnuhost's own threads and its tasks' are
changed, nothing outside the process: a SIGKILL leaves nothing to restore. `ONEIOS_THERMAL_CAP=0` turns the step off.
Since 2026-11-13 a step keeps the CPUs it adds of the other kind (step 2: the P vCPUs on cpu5-6 and the E cores, `0x63`;
before, `0x60`), and cpu-pin pins a vCPU to one core only while each vCPU of its kind has a core of the step's mask to
itself, else the vCPUs share the whole mask (before, six P vCPUs went three to a core at step 2: the phone's hang of
2026-11-13 ran there; `runtime/bridges/thermal/README.md`, "Fixed 2026-11-13").
The same reading goes to the guest on the bridge's channel 6 (`drivers/thermal`). Detail:
`runtime/bridges/thermal/README.md`.

### 2.6 Tasks: the trap and service layer

- **Factory.** Forked from xnuhost before it has threads or the RAM mapped; it never execs, and neither do
  the tasks it forks. So xnuhost and every task hold the **same PAC keys**, which is what arm64e's shared
  cache needs (a pointer the kernel signs authenticates in any task). `--selftest` checks it on the host.
- **Stub** (`runtime/xnuhost/stub/src/main.rs`, a no_std crate for `aarch64-unknown-none`; `stub/build.sh` links it at 0 with `stub.ld` into a 3.4 KB position-independent blob with no data, no relocations, no panic path and no libc, which `src/arch.rs` embeds; `stub/stub.c` until 2026-11-23). In a new task it
  unmaps the whole address space but its region, installs the handlers and the seccomp filter, optionally
  drops to another uid, and becomes the service thread.
- **System calls.** The filter returns `SECCOMP_RET_TRAP` for every system call whose instruction pointer is
  outside the stub's text, and inside it allows only the twelve calls the stub makes (`mmap` only on the two
  memfds or anonymous). iOS's `svc #0x80` therefore never reaches Linux: the thread gets SIGSYS, the stub's
  handler copies its registers (x0-x30 with x16 intact, sp, pc, NZCV, v0-v31, FPSR, FPCR, the thread
  register) to the thread's trap page, wakes xnuhost and waits. Seccomp user notification is not used: it
  carries the Linux number and six arguments, not x16.
- **One Linux process per kernel address space, and nothing shared between two.** `task_create` runs one at
  a time (the wait for the factory's answer gives the table lock up: two vCPUs once got the same table
  entry and the same answer, and two iOS processes shared one Linux process). A destroyed task's slots
  (pages of the trap memfd its process has mapped) go back to the pool only when the factory has reported
  the process dead, and a destroyed thread's slot only when Linux has cleared its tid: a thread of the old
  process that still waits on the page would take the next owner's orders or register state. Every such
  wait is by the clock (`CLOCK_MONOTONIC`); the vCPU threads get signals all the time, and a wait counted
  in interrupted `futex`/`usleep` calls ends in milliseconds. An order that is not answered in 10 s, or a
  thread that does not exit, is logged with each Linux thread's state and system call from `/proc`.
- **Faults, undefined instructions, breakpoints, FP exceptions** arrive the same way as SIGSEGV/SIGBUS,
  SIGILL, SIGTRAP, SIGFPE, with the fault address and Linux's ESR when it gives one. **A SIGTRAP never has an ESR
  of its own:** Linux sends every SIGTRAP (brk, single step, watchpoint) through `send_user_sigtrap()`, which leaves
  `thread.fault_code` alone, and `fault_code` is written only when a fault signal is delivered; the signal frame's
  `esr_context` is then the thread's last fault's. xnuhost passes no ESR with a SIGTRAP, in the kernel's upcall
  (`cpu.rs`: the kernel reads the `brk` at pc, `hosted_kernel_esr`) and in a task's trap (the stub: `esr` 0 and the
  instruction in `insn`, for 0002's `hosted_trap_esr`). Before that (phone, 2026-11-12 23:15), a UBSan soft trap in
  `tcp_input` (`compute_iaj_meat`'s `cur_iaj_dev * cur_iaj_dev`, a signed multiply of a wrapped `uint32_t`
  difference) came with a software-TLB fill's data abort and panicked iOS: "Unexpected fault in kernel physical
  aperture"; an app's `brk` (Swift traps, `__builtin_trap`) reached iOS as that fault too, not as `EXC_BREAKPOINT`.
- **A cache maintenance fault is a read** (fix-webkit-icache, 2026-11-14). A `dc cvau`, `dc cvac`, `dc civac`,
  `dc cvap` or `ic ivau` (the cache maintenance by address EL0 may run; Linux sets SCTLR_EL1.UCI and traps them only
  for Cortex-A53 r0p0-r0p2 errata, not on the SM8650's cores) runs natively in the task process. On a page the
  task's Linux process has not mapped (the hosted pmap maps a page into the task only when it faults) it takes a
  data abort whose ESR has **ISS.WnR = 1 and ISS.CM = 1**; Linux treats it as a read (`is_write_abort` is WnR without
  CM), finds no VMA and sends SIGSEGV with that ESR whole in `esr_context` (`set_thread_esr` sanitizes kernel
  addresses only). The stub (`xs_fault_access`, then in `stub/stub_abi.h`, now `fault_access` in `runtime/abi/src/stub.rs`) used to read WnR alone and said `XH_PROT_WRITE`;
  the kernel's software TLB then refused to fill the read-execute page, and its ESR (a permission fault with WnR, no
  CM) made `inspect_data_abort` ask `vm_fault` for write on a read-execute entry: KERN_PROTECTION_FAILURE. That was
  WebKit's crash loop (phone, 2026-11-14 06:03): every WebContent process died in `sys_icache_invalidate` of its first
  JIT thunk (`JSC::LLInt::createJSGateThunk`), on the read-execute alias of its JIT region, "byte write Permission
  fault". Now the stub says `XH_PROT_READ` for CM, and hosted 0071 does the same on the kernel's side (also with an
  older stub, and for the kernel's own faults) and hands XNU the fault with CM and WnR kept as the hardware set them
  and an access flag fault (XNU's rule: CM with WnR is a write only for a permission fault). The fill maps the page
  read-execute (`hosted_pte_prot`: AP read-only and UXN clear = `XH_PROT_READ | XH_PROT_EXEC`, the stub's
  `PROT_READ | PROT_EXEC` of the RAM memfd), so the retried `dc cvau` reads a mapped page and goes on. qemu-user
  runs these instructions as no-ops that never fault, so a PC boot cannot show the fault (section 6).
- **Running a thread.** `thread_run(thread, state, trap)` releases the thread with the given registers (the
  handler returns through `rt_sigreturn`) and blocks the vCPU until the thread traps again: the vCPU "is in
  user mode". If the vCPU's interrupt line rises meanwhile, xnuhost signals the thread and it stops with
  `XH_TRAP_INTERRUPT`. The kernel's side is a loop: run, handle the trap as `sleh_synchronous` would, run.
- **copyin/copyout**: `task_read`/`task_write` (`process_vm_readv/writev`), or the kernel's own walk of its
  pmap to the RAM.
- **The thread register.** iOS reads its TSD base from `TPIDRRO_EL0`, which Linux keeps 0 and user space
  cannot write. The host keeps the base in `TPIDR_EL0` (`xh_user_state.tsd`) and `text_fixup()` rewrites
  `mrs xN, TPIDRRO_EL0` to `mrs xN, TPIDR_EL0` in a page before it is mapped executable (after the kernel
  validated its signature). The call matches 32-bit patterns and must see a page once, so the kernel
  (`osfmk/arm64/hosted/hosted_text.c`, patch 0025) calls it only for a file's page that is being entered
  with execute permission (never for data or link-edit pages, which are validated too), records every word
  it changed, and hashes the page with the original words when XNU validates it again. Risks: a data word
  inside a code page with one of the encodings; code that writes `TPIDR_EL0`.
- **Plain arm64 binaries**: `thread_set_pac(thread, 0)` turns the keys off for that thread
  (`PR_PAC_SET_ENABLED_KEYS`).
- **A task's end.** `task_destroy` sends the factory `M_KILL`, sends the process `SIGKILL` and waits (asleep, on the
  vCPU, up to 20 s) until the factory reports its exit, so that no page the kernel frees is still mapped in a process
  that runs. A death that takes 250 ms or more is a line: `tasks: pid N took T ms to die (the kill to the factory's
  report; its vCPU waited)` (fix-widget-kill-loop, 2026-11-15). iOS's own exit watchdogs count that time: WidgetKit
  gives a widget extension a few seconds after chronod asks RunningBoard to end it (5 s, read in 24A446's WidgetKit, not
  proven to be that timer) before it logs `Extension process termination watchdogged
  pid (N) after Ns` in chronod's log.
- **The trim** (`task-trim`, default 1; `task.rs`, fix/task-trim, 2026-11-23). A task's host mappings are the pmap's
  TLB: dropping them is always allowed, and a page the task touches again faults and is filled from the tables, as
  after the kernel's own flush of a whole pmap (`hosted_tlb_flush_pmap`, which unmaps `[0, user_va_limit)` too). Each
  release stamps the task (`last_release`, one store); a thread `xnuhost-trim` (nice 10, never realtime) looks every 5 s
  (every second when the idle time is under 5 s) and orders a task none of whose threads is released (`running` 0)
  and whose last release is `task-trim-idle` s old (2 s in the pressure mode, 2.2) one `XS_OP_UNMAP` of `[0, the stub
  region)` through its service thread, like any order: under the task's order lock, after the maps and unmaps queued
  before it (which it undoes). Linux frees the task's VMAs and page tables there; the stub region (threads' slots,
  signal stacks, the service page) stays. `trimmed` is set before the order and cleared by the next release, so a task
  is trimmed once per idle spell, and a release that comes while the unmap runs only faults, as after a flush. The
  kernel needs no change: it never assumes a host mapping is there (fills on every fault the tables allow; `task_read`
  and `task_write` fail and are filled on a missing page; `task_protect` is not called), and its own bookkeeping only
  gets more conservative (0045's nested-region bits stay set, so a flush still goes to the task; 0050's fault-around
  windows, off by default, stay marked done). The orders' time is the trim's own line, not the vCPUs' perf steps.
  Lines and numbers: 2.2, "The trim".

### 2.6a The cost of the trap path (perf, 2026-11-12)

One trip of an iOS thread through the kernel is: the trap in the task process (seccomp's SIGSYS, or SIGSEGV for a
fault), the stub's handler copies the registers to the trap page and hands them to the vCPU thread (**up**), the kernel
works (**kernel**), then `thread_run` hands the registers back and the handler returns into user code (**down**). A
fault adds the map (`task_map`); an interrupt of a running thread adds the kick (a signal to it). Each hand-off is a
store of a word in the shared trap page and, when the other side sleeps, a futex wake.

**What changed.**

- **`cpu-pin` (default 1).** With the CPU plan (the bridges on: the phone), each vCPU thread is on one host CPU of its
  plan's mask (the rank-th of its kind: on the S24 Ultra vCPU *i* on CPU *i*), and the iOS thread it runs and the task's
  service thread, when that vCPU gives it an order, go on the same CPU. A hand-off is then a switch on one core. Across
  cores it is a wake-up of another CPU (an IPI, the way out of idle) each way. The thermal step's masks still apply: a
  narrowed mask gives the vCPUs of a kind its CPUs in turn. `cpu-pin=0` is the old placement (any core of the kind).
  `cpu-pin=2` also pins with no plan, vCPU *i* on CPU *i* (for `check-arm64.sh bench`).
- **Wakes only for a sleeper.** Each side says when it sleeps (`host_sleeping`, `task_sleeping` in the trap page) and
  the other side issues `FUTEX_WAKE` only then (`runtime/abi/src/stub.rs` has the rule and its barriers).
- **`map-defer` (default 1).** `task_map` puts the map in the task's queue (up to 256) and returns. The queue goes with
  the next release of one of the task's threads: that thread carries out the maps in the stub's handler before it runs
  (`XS_OPS_OFF` in its slot) while xnuhost holds the task's order lock, or it goes at the head of the task's next order
  (unmap, protect, clone), whichever is first. So a map is in place before any thread of the task runs again and before
  any later unmap or protect of the task, which is what the kernel's TLB rule needs (`hosted_pmap.h`). `task_read` and
  `task_write` send the queue first. The queue has its own lock: a `task_map` never waits for a release whose thread
  is carrying out earlier maps (on the phone that made the kernel's fills wait about 100 us each, with the task's TLB
  lock held). A failed deferred map is logged (`tasks: pid N: a deferred map ... failed`); the
  toucher faults again and the kernel fills again. `map-defer=0` orders each map (and each unmap) as before.
- **Deferred unmaps (with `map-defer`, 2026-11-12).** `task_unmap` is the kernel's TLB flush: when it returns, no thread
  of the task may use the old mapping. A task none of whose threads is released (`task.running`: released by
  `thread_run` and not yet back, counted under the queue's lock in the same hold that takes the queue) cannot use it
  either, so its unmap goes into the same queue, after the maps queued before it, and returns (`XS_OP_UNMAP`: the stub
  carries out `munmap` in the queue's order). A task with a running thread, or a full queue, gets an order at once (the
  order takes the queue first). The count takes no lock on a release (that cost every trip two lock pairs): a release
  adds 1 and then reads the queue, an unmap adds itself to the queue and then reads the count (and takes itself back to
  be ordered if a thread is counted), all sequentially consistent, so one of the two always sees the other. Whoever takes queued unmaps (a release that carries the queue, or an order) holds the
  task's order lock until they are done and marks them out (`task.unmaps_out`); until then no other thread of the task
  is released (it waits for that lock) and `task_read`/`task_write` wait too (a read through a flushed mapping would see
  a page that may be someone else's by now; it must fail, so the kernel fills it). A batch of maps only marks nothing:
  the other releases still go at once. The test kernel checks it on two vCPUs: thread B released on vCPU 1 while thread A
  (or an order) on vCPU 0 carries 128 maps and the unmap, 200 rounds; both threads' loads fault every time. Without the
  `unmaps_out` wait (the first draft, `a97687f`) 40-60 of 200 rounds saw the old page.
- **Spins (`spin-host`, `spin-task`, microseconds, default 0).** Each side may spin before it sleeps. With each thread
  on its own core this halves nothing that pinning does not already remove, and on a shared core it costs the spin
  every time (below), so both are off.
- **The kick (direct since 2026-11-23, fix-fps-scheduler-migration).** A vCPU that waits in `thread_run` for the iOS
  thread it released sleeps in the hand-off's futex on that thread's core (`cpu-pin`). When another thread raised its
  interrupt line (an IPI, a device), `cpu::line` (the C's `xh_cpu_line`) signalled the vCPU's thread (`SIG_KICK`): that thread had to be woken
  and scheduled on the busy core, ahead of the iOS thread it shares the core with, only to send the iOS thread the kick
  (`tgkill`, `KICK_SIGNAL`); the iOS thread's handler then handed the trap back. On the phone (2026-11-23 03:28, every
  vCPU busy after hosted 0089) that was 24,419 kicks in 30 s, mean 136 us, p99 3.7 ms (the `kick` line). Now the raiser
  kicks the iOS thread itself (`src/task.rs` `kick`, the C's `task.c` `xh_tasks_kick`): `thread_run` publishes the
  released thread (pid, tid: `Vcpu::run_pid`, `run_tid`) and a word `Vcpu::run_word` = release number << 1 before the release, and whoever sets the word's bit 0 first (a raiser, by
  compare-and-swap, or the vCPU's own loop for its timer) sends the one kick of that release; the vCPU's thread is not
  woken until the thread's trap comes back. The release numbers are never reused, so a raiser whose word changed under
  it (the thread came back, another was released) sends nothing and signals the vCPU as before. A kick that lands
  after its thread came back waits in the stub's handler (it blocks every signal) and is a spurious interrupt trap at
  that thread's next release, which the kernel takes as an interrupt with nothing pending. A refused `tgkill` puts the
  bit back and falls back to the vCPU's signal. The `perf:   queue:` line says how many raises kicked directly (`N
  signalled or woken (M of them by a kick of the iOS thread they wait for, sent by the raiser)`), and the `kick` line
  now runs from the raiser's kick.
  Tested (build laptop, 2026-11-23): `runtime/host-tests.sh` (xnuhost tests 14296 checks, 0 failed; check-arm64's test
  kernel `all passed`, its `test_xcall` 20 of 20 SGIs taken by a vCPU whose thread spins in user code). An A/B of
  `test_xcall` under qemu-aarch64, old kick (main `44d9c49`) against the direct one, alternated, 3 runs each on a
  laptop with a load of 14-22 from other jobs: mean 115/104, 591/539, 1617/275 us, slowest 444/337, 2262/3251,
  4954/1489 us. qemu's times and that load make it a check that nothing got slower, not a measurement of the phone;
  the phone's is the `kick` line (mean 136 us, p99 3.7 ms before).
  The Rust port (branch rust-runtime, 2026-11-23: `task::kick`, `cpu::line_with`, the `KicksDirect` and `KicksAgain`
  counts of `src/perf.rs`, the same line texts): `runtime/xnuhost/check-arm64.sh smoke check` with the static Rust
  xnuhost under qemu-aarch64 on the build laptop, all three boots passed; `test_xcall` (`testkernel/src/tasks.rs`)
  `taken 0x14 of 20, slowest (us) 0x156, mean (us) 0x6f` (342 us, 111 us), and the boot's `perf:   queue:` line
  `23 signalled or woken (20 of them by a kick of the iOS thread they wait for, sent by the raiser)`. Not run on the
  phone.

**Measured** with the C tree's `make bench-arm64` (today `runtime/xnuhost/check-arm64.sh bench`; the test kernel's loops on vCPU 0, one iOS thread; native arm64 Linux 6.18 in the
Mac's container, an M4 with 8 CPUs; the counter is 24 MHz, so a tick is 42 ns). Means, p50 in brackets, 40,000 null
calls, 10,000 faults, 4,000 interrupts; two runs each, the same within 5%:

| Trip | Before (`cpu-pin=0 map-defer=0`) | After (`cpu-pin=2 map-defer=1`) |
|---|---|---|
| null system call, kernel to kernel | 17.0 us (15.7-16.9) | 1.9-2.0 us (1.9) |
| of it: svc to the kernel / the release to user code | 8.5 / 8.5 us | 1.0 / 1.0 us |
| page fault filled by `task_map`, as user code sees it | 36.0-36.6 us (36.1) | 4.3-4.5 us (3.8-3.9) |
| `task_map` host call | 17.0 us | 0.01 us (the map rides on the release: 1.9 us there) |
| `task_unmap` (an order to the service thread) | 17.8-18.1 us | 3.0-3.1 us |
| timer interrupt of a running thread, deadline to the kernel | 31.4-39.9 us (31.2) | 5.8-7.0 us (2.6) |

On one shared core (`taskset -c 0`, old code) the null call was 3.2 us at p50 without a spin and 42 us with a 20 us
spin on each side; with each thread on its own core a 20 us spin gave 1.8 us. So the cost was the wake of another core,
and on the phone, where two vCPUs and their iOS threads share the two E cores, a spin would cost what it saves. Not
measured on the phone; the hand-off across cores there may be cheaper or dearer than in this VM.

**Deferred unmaps, measured on the x86_64 PC** (2026-11-12, the C xnuhost, `make bench-arm64` of its Makefile, today `check-arm64.sh bench`, `BENCH_SET="--set
cpu-pin=2"`, the Android binary under qemu-aarch64 11.1.3 on 4 host CPUs; qemu's counter is 1 GHz and its times are
qemu's, about ten times the container's: compare these with each other only). Three runs each, before (`main`
`43df9dd`) and after, alternated; mean (p50); 20,000 null calls, 5,000 faults, 2,000 interrupts:

| Trip | Before | After |
|---|---|---|
| null system call, kernel to kernel | 18.8-19.2 us (18.0-19.0) | 19.2-19.6 us (19.0) |
| page fault filled by `task_map`, as user code sees it | 35.1-35.3 us (34.0) | 34.1-34.7 us (32.0-36.0) |
| `task_map` host call | 0.94-1.10 us (0.98) | 0.94-1.11 us (0.98) |
| `task_unmap` host call | 17.0-17.9 us (14.0): an order | 1.0-1.2 us (0.98): queued; 5,505 queued, 1 ordered |
| timer interrupt of a running thread | 66.4-71.3 us (26.0-27.0) | 68.4-70.1 us (26.0-27.0) |

So a fault, its fill and the later unmap of the page cost about 53 us of the vCPU's time before and 36 us after: the
unmap's round trip to the service thread is gone and the `munmap` rides on the next release with the maps (the fault
row, which includes that release, did not grow). The other rows did not move beyond the runs' spread. A first version
that counted `task.running` under the queue's lock (two more lock pairs per release) cost every null call about 1 us
here (p50 21.0 us); the lock-free count does not. On the phone (15:35 boot) the pageout's unmaps were 1.47 million
round trips per 30 s (about 1.5 vCPUs); how many of them this queues depends on how many of those tasks were running
then: not measured yet (the `perf:` line's `task_unmap N queued, M ordered`).

**The stub's register copy, and the Rust xnuhost against the C** (2026-11-23, branch `rust-runtime`, the x86_64 laptop
(i9-12900H), qemu-aarch64, the C tree's test image, `check-arm64.sh ab`: the two binaries in turn
on the same six P cores, means over the rounds). The Rust port was reported 7% slower on the null call (10.96 against
11.75 us); pinned and alternated it was not (6 rounds each, load 1-2.6): null call C 9.86, Rust 9.77 us; svc to the
kernel 4.84 / 4.80; release to user code 4.95 / 4.90; page fault 14.5 / 14.8; every other line within the 3% that
rounds of one binary move. Unpinned single runs move 10-30% with the cores qemu lands on (P or E) and the load, which
is where the 7% came from. What both spent most of a half on was the stub: its handler copied x0-x30, V0-V31, fpsr,
fpcr and the esr between the signal frame and the trap page **one volatile byte at a time** (the C `stub.c`'s `copy`, kept
by the port so that no memcpy call appears in the blob), about 1.5 KiB per trap and some 9000 instructions. The
Rust stub now copies by 8-byte words when both ends are aligned (all of these are): pinned, 6 rounds (load 6-12),
null call C 9.95 us, Rust before 10.63, Rust after **6.48 us**; svc to the kernel 4.88 / 5.22 / **3.14**; release to
user code 5.00 / 5.34 / **3.28**; xnuhost's `up` step 3.27 / 3.39 / **1.93** us; page fault 14.7 / 15.6 / **11.1**; timer
interrupt 12.9 / 15.8 / **11.0**; rpc mode 2 (one vCPU) 19.5 / 18.3 / **12.2**. On the phone the same loop was about
1500 bytes a trap at roughly a byte a cycle each way. The C stub kept the byte copy until it was deleted with the rest
of the C runtime (2026-11-23).

**On the phone, Rust against C** (2026-11-23, S24 Ultra; the same kernel.kc `988c2a7b...` and image set; C: runtime
0.1.16, xnuhost `e2c013ab...`, session of 03:28; Rust: xnuhost `58a378e2...` from `rust-runtime` `00c174d`, staged with
`phone stage runtime-asis` over 0.1.16, session of 13:03). One boot each, so the numbers move with the boot's own
order of work. First frame with content after xnuhost's start: C 51.9 s, Rust 50.1 s. The `perf:` windows 2-5 (the
busy part of the boot; mean / p50 in us, the p50 is the histogram's bucket):

| Window | task traps/s C / Rust | `up` C / Rust | `down` C / Rust | kernel: syscall C / Rust | kernel: fault C / Rust |
| --- | --- | --- | --- | --- | --- |
| 2 | 56,644 / 59,802 | 7.39/6.14 / 7.37/5.12 | 8.53/7.17 / 8.43/6.14 | 66.4/12.3 / 60.9/12.3 | 46.5/3.6 / 32.4/3.6 |
| 3 | 87,085 / 81,814 | 7.43/6.14 / 7.70/6.14 | 8.50/7.17 / 8.95/7.17 | 51.3/14.3 / 52.5/14.3 | 23.0/4.1 / 23.6/5.1 |
| 4 | 69,787 / 71,963 | 8.59/7.17 / 8.87/6.14 | 9.99/8.19 / 10.28/8.19 | 51.0/14.3 / 53.6/14.3 | 29.4/5.1 / 27.4/5.1 |
| 5 | 48,847 / 65,049 | 9.31/7.17 / 8.42/7.17 | 10.29/8.19 / 10.06/8.19 | 58.5/16.4 / 51.5/16.4 | 24.7/6.1 / 21.1/5.1 |

So on the phone the Rust xnuhost is level with the C: the hand-off steps are within 5% either way, the kernel's own
work the same. The word copy's gain under qemu (`up` 3.39 to 1.93 us) does not show here: a phone hand-off is about
7-9 us, most of it the futex sleep and wake-up of the other side (the `futex sleeps` counts of the same lines), not the
copy.

**On the phone** (combined boot of 2026-11-12 12:30, kernel.kc 1e297354, runtime 0.1.10-dev; `cpu-pin=1`, vCPU *i*
on CPU *i*; 30 s windows during the boot): 17,000-22,600 task traps a second, two to three faults for each system
call. Each hand-off took 10-12 us at p50 (up and down alike), ten times the container's: the cores were slow then (the
cap and the governor) and most hand-offs slept in a futex. The kernel's work after a trap: 41 us at p50 for a system
call (mean 115-130 us), 12-20 us for a fault (mean 60 us); a deferred map carried by a release took 50-70 us from the
release to done (the task's `mmap` of one page), an unmap order 29-32 us. So the kernel's own work and the number of
faults are now the larger costs, not the hand-off (the kernel side: `kernel/patches/oneios-hosted/README.md`,
"0040-0044").

**The report.** Each vCPU counts its traps by kind and keeps histograms (4 buckets a power of two) of each step
(`src/perf.rs`). The main thread prints them every `perf-log` seconds (default 30, `--set perf-log=0` only at the stop
and on SIGQUIT), for that period; at the stop and on SIGQUIT since the start:

```text
perf: 30.0 s: N task traps (R/s): syscall .., fault .., interrupt .., other ..; futex sleeps: vCPU .., iOS thread .. (of .. hand-offs each way); futex wakes .. + ..; task_map .. carried by a release, .. ordered; task_unmap .. queued, .. ordered; timer_set .. (.. to the host timer)
perf:   up (trap->vCPU)    n ... mean ... p50 ... p90 ... p99 ... max ... us      the handler's copy and the hand-off
perf:   down (vCPU->task)  ...                                                     the release to the handler's wake
perf:   kernel: syscall    ...      the kernel's work after a trap of that kind, to this vCPU's next thread_run, idle out
perf:   kernel: fault / interrupt / other, order: map / unmap / protect / clone, maps by release, kick
perf: cpuN: C% of a core; of its wall time K% kernel, U% an iOS thread's turn, I% idle; T traps/s; last on host CPU H; waited for a host CPU W% (runnable, not running)
```

"An iOS thread's turn" is the time `thread_run` waited (the thread's own code plus both hand-offs); "kernel" is the rest
of the vCPU's wall time outside `cpu_idle`; "C% of a core" is the vCPU thread's CPU time (Linux's clock of that thread). "Waited for a host CPU" (since 2026-11-23) is its
run-queue wait from `/proc/self/task/TID/schedstat` (one file read per vCPU per report): the part of "kernel" that is
the host's CPUs being busy with other work, not the guest kernel's own. In the session of 2026-11-23 20:23 the vCPUs
showed 94% kernel at 44% of a core with nothing to say where the other half went; `oneios-tool log perf` has it as
`host_wait%`.
The kernel's side of the same trips (which system calls, the fills, the TLB lock) is the hosted kernel's
`oneios-perf:` report (patch 0041, `kernel/patches/oneios-hosted/README.md`).

**Host overhead, 2026-11-15 (fix-host-overhead).** From the phone session of main 3233fed (30 s windows): 1,021,984 user
stays, 684,514 system calls, 330,746 faults, 331,008 user maps and 414,758 user unmaps (each an mmap or munmap of one
page in the task's process), and the host at 482 % of 800 with 212 % in Linux's own code. What changed in xnuhost:

- **The deferred queue joins entries** (`queue_join` of `runtime/abi/src/stub.rs`, `stub_abi.h`'s `xs_queue_join` in the C; unit-tested there, `queue_joins`): a map of the
  next page of the same file range with the same protection extends the last map; adjacent or overlapping unmaps become
  one; an unmap followed by a map of the same range is the map alone (`MAP_FIXED` replaces the old page in one step: the
  pattern of a fault that changes an entry, the flush then the refill); a map followed by an unmap of the same range is
  the unmap alone; the same range mapped twice keeps the later map. Only the last entry is looked at (one compare per
  call). An unmap joined into the last entry that has to be taken back (a thread of the task was counted running) puts the
  entry back as it was and is ordered, as before. The `perf:   queue:` line counts the joins (one host call less each).
- **Maps are populated** (`MAP_POPULATE`, `XS_MAP_POPULATE`): the stub's maps of the RAM memfd, in the release and in an
  order, and the kernel's `kernel_map` fill the page tables in the same call, so the faulting instruction's retry does
  not take a second, Linux, fault on the page it was just given.
- **The GIC tells only the CPU a change concerns** (`gic.rs` `update_cpu`): an ack, an EOI of an SGI or PPI, an SGI, a
  PPI or SPI level change recompute that CPU's line (an SPI's: its target's), not all eight under the one lock. Changes of
  the distributor, an enable, the priority mask still recompute every line.
- **A line that is up is not signalled again** (`cpu.rs` `line`): every GIC change used to send each other vCPU
  whose line was up a `tgkill` (or a futex wake), even when the line had been up for a while (a vCPU running with
  interrupts masked got a signal per interrupt step anywhere). The CPU takes a pending line when it unmasks, when a host
  call returns, and in `thread_run` and `cpu_idle` before they wait, and only it lowers its line, by recomputing under the
  GIC's lock after the raiser's change, so a raise skipped is never lost. `perf:   queue:` counts the raises signalled and
  those skipped.
- **The uclamp reaches iOS's own code** (`task.rs` `thread_run`, `cpu.rs`): the guest's frequency request
  (DroidVMCpufreq's `SET_PERF`, `cpu::Cpufreq` of `runtime/bridges/cpu`) and drm2kgsl's animation boost used to set `uclamp.min` on the vCPU thread
  only, which waits while the iOS thread runs on its task's own Linux thread: the host's governor never saw a request for
  the core where the work ran. Now each release gives the iOS thread its vCPU's clamp (one `sched_setattr` per change,
  none otherwise; SCHED_OTHER stays SCHED_OTHER, nice is kept). The kernel's hint (`hostcall.h` `perf_hint`, hosted 0075:
  the thread's performance-control class) gives a UI, user-initiated or realtime thread its floor and a background or
  utility one no floor and no ceiling of ours (a ceiling, `uclamp.max` 512, stood here until 2026-11-16; it also slowed
  chronod's widget extensions, newtodo's decision list). drm2kgsl's own clamp looked for threads named
  `crosvm_vcpu*` and logged `vCPU uclamp.min 1024: 0 of 0 threads`; its patch 0008 calls xnuhost's
  `drm2kgsl_vcpu_uclamp` instead. Since 2026-11-23 (section 2.6e) that only stores the value, which reaches the frame's
  threads at their release (`cpu::release_floor`), and a vCPU thread's own clamp is xnuhost's alone: the released
  thread's floor at each release, 0 at the guest's idle, the guest's request at its `SET_PERF`.
- **The same timer deadline again is not a host call** (`cpu.rs` `timer_set`): a future deadline equal to the
  armed one leaves the host timer as it is (`perf: ... timer_set N (M to the host timer)`: N - M saved; the PC's baseline
  boot made 4.8 million timer sets in 2,400 s, every one a `timer_settime`).
- **Each vCPU is described as the core it runs on** (`runtime/bridges/cpu`, section 2.6b).
- **The `perf: cpuN:` line ends with `last on host CPU N`** (`/proc/self/task/TID/stat`, a named file per vCPU).

Before and after are in section 2.6b and `kernel/patches/oneios-hosted/README.md` ("0075"). What the phone shows: the
`perf:   queue:` line (joins, uclamps given, raises skipped), `perf: task_map ... task_unmap ...` against the kernel's
`oneios-perf:   pmap: user maps N, unmaps M` (fewer host calls than pmap operations), and `thermal: 30 s: host CPU ...
sys ...` (the host's share in Linux).

### 2.6b CPU placement, the topology iOS sees, and the parallel test (2026-11-15)

**The topology.** `runtime/bridges/cpu` reads, per CPU by name (`/sys/devices/system/cpu/cpuN/...`, N = 0..15, never a
directory walk): `cpu_capacity`, `cpufreq/cpuinfo_max_freq`, `cpuinfo_min_freq`, `related_cpus` (the frequency domain),
`scaling_available_frequencies`, `scaling_boost_frequencies` (new), `scaling_max_freq` (new: a cap someone wrote, logged
and never written), `regs/identification/midr_el1` (the core) and the caches. xnuhost logs at start one line per host
CPU (`cpu: host CPU 7: Cortex-X4 (part 0xd82), P, frequency domain of CPU 7, capacity 1024, 480000-3398400 kHz (top as a
boost frequency)`), one per vCPU (`cpu: vCPU 7 (P) is described to iOS as host CPU 7: ...`), and the cpuset it runs in
with its CPUs and the `cpufreq/boost` switch (`cpu: xnuhost's cpuset /..., CPUs allowed 0-7; cpufreq boost 1`).

- **What iOS 27's kernel reads** (xnu-12377, `osfmk/arm64/machine_routines.c` `ml_parse_cpu_topology`): each `/cpus/cpuN`
  node's `cluster-type` (`E`, `M` or `P`), `reg`, `l2-cache-size`, `l3-cache-size`. The cluster of a CPU comes from
  `MPIDR_CLUSTER_ID(reg)` only when the SoC config has `HAS_CLUSTER`; the hosted kernel is built on UniXNU's BCM2837
  config (no `HAS_CLUSTER`, no `HAS_MCORE`), where the cluster is `cluster-type == P`: exactly two clusters, E and P,
  whatever the tree says. `hw.nperflevels` is 2 (droidvm-amp.patch: `perflevel0` P, `perflevel1` E). The Edge scheduler
  and DroidVMCpufreq's thread-group preferences (UI, above-UI and realtime classes on the P cluster) place the threads.
  So three levels (X4, A720, A520) cannot be told to this kernel by the tree alone: that needs a kernel built with
  per-cluster ids and `M` cores (not done; a guess that the Edge scheduler would take three cluster types on this config).
- **The SM8650's four kinds** (cpu0-1 A520 2.27 GHz, cpu2-4 A720 3.15 GHz, cpu5-6 A720 2.96 GHz, cpu7 X4 3.39 GHz on the
  S24 Ultra; read at run time, not coded): E = the A520s (the lowest capacity), P = the A720s and the X4. The 2.96 GHz
  A720s are P on purpose: each is about twice an A520's work at the same clock, and as E they would take iOS's background
  work while UI work queued on four P cores. They are the P cluster's slowest cores: cpu-pin puts vCPU 5-6 there, and
  with the hint below a UI thread there still asks the host for its top clock.
- **Each vCPU is described as its own core** (`CpuPlan::make`, `runtime/bridges/cpu/topology.rs`): cpu-pin puts the rank-th vCPU of a kind on the
  rank-th CPU of the kind's mask, in CPU order (vCPU *i* on CPU *i* on the S24 Ultra), and the vCPU's clock table,
  capacity and caches are now that core's. Before, every P vCPU was described as the fastest core of its kind (six X4s),
  and a frequency request of a vCPU on an A720 was sized against the X4's table.
- **The X4's top**: the guest's `DroidVMCpufreq: ... 480000-3052800 kHz (cpu7)` on the phone was the table of
  `scaling_available_frequencies`, which leaves out boost frequencies (Linux lists `CPUFREQ_BOOST_FREQ` entries in
  `scaling_boost_frequencies`; Qualcomm's cpufreq-hw marks the X4's 3.39 GHz that way [inference: not seen on this
  phone's files yet]). The boost frequencies are now in the vCPU's table and its top. Whether Linux lets the core reach
  them depends on `cpufreq/boost`, which xnuhost logs and never writes; `oneios-tool phone cpu` shows it, each domain's
  `scaling_max_freq` and governor (a cap left by One UI's side would show there as `CAPPED`).

**Priorities.** iOS's QoS reaches the host in two ways, both without realtime policies: the guest scheduler puts a
thread on an E or P vCPU (the Edge scheduler with DroidVMCpufreq's preferences), and cpu-pin keeps the vCPU, and the
iOS thread it runs, on its core; and the clamp of section 2.6a: the kernel's `perf_hint` (hosted 0075) says the class of
the thread a vCPU is about to run, and the release gives that Linux thread `uclamp.min` 1024 for realtime, UI and
above-UI, 768 for user-initiated, the vCPU's own request for default (drm2kgsl's animation clamp on top of any of
these that is not 0: section 2.6e), and 0 with no ceiling for utility, background and darwin-background threads (RunningBoard's background and suspended roles clamp an app's threads to
background QoS; the `uclamp.max` 512 that stood here until 2026-11-16 also slowed chronod's widget extensions, #57, and
is gone: newtodo's decision list). The same word is readable by host code on the vCPU
(the GPU path: the current vCPU's `perf_hint`, `Vcpu::current()` in `src/cpu.rs`, for fix-gpu-perf's per-context priority).
The E cluster is DroidVMCpufreq's preferred cluster of every class but UI, above-UI and realtime, and XNU's Edge edges
let nothing move from E to P: until hosted `0085` (2026-11-22) the `perf: cpuN:` lines showed vCPU 0-1 0% idle and
vCPU 2-7 85-99% idle in every period, the X4 (vCPU 7) the most idle. `0085` opens the E -> P edges (migration and steal;
weight 0 to default, 64 utility, 1000 background, us of scheduling latency): on the phone the same lines should show
vCPU 2-7 busy while vCPU 0-1 are, and the kernel's `oneios-edge:` line. It needs hosted `0089` (2026-11-23): on this board
nothing called `ml_cpu_init_completed`, so the scheduler's completion step (and 0085 at its end) never ran; 0089 calls it
once the last vCPU is booted (`oneios-smp:` lines). With the completion step XNU's own rules ran too: an E vCPU
going idle sent a rebalance IPI to the P vCPU running a spilled thread, and a P vCPU preempted it when the E cluster's
load was 0, so threads went back and forth on most dispatches (2026-11-23 03:28: `release:` 70,200 moves and `kick`
24,419 in 30 s, against 14,824 and 931 before 0089). Hosted `0091` keeps a thread that prefers E on the P vCPU it
spilled to until it blocks or its quantum ends (`oneios-edge-pullback=1` is XNU's way; the kernel's `oneios-perf:
0091:` line counts what it kept and the scheduler's IPIs by reason, to idle and to busy vCPUs).

**The cpuset.** A thread in Android's `background` or `system-background` cpuset may use only the A520s, whatever
`sched_setaffinity` asks: cpu-pin cannot work there. xnuhost logs its cpuset and allowed CPUs at start. From the fork's
`switcher/assets/oneios_switch.sh` (branch `oneios`, read 2026-11-15): `--inner` moves the shell that execs xnuhost into
the v2 cgroup `oneios_session` only; the v1 cpuset (`/dev/cpuset`) is changed only for the watcher and the key readers
(`out_of_the_way`: the cpuset's root, every core). So xnuhost keeps the v1 cpuset of the root shell the app started
[inference: top-app's or foreground's when the switch is tapped from the front app; never read on the phone yet]. If the
`cpu: xnuhost's cpuset` line or `phone cpu` shows a cpuset of fewer CPUs, the fix belongs in the fork's `--inner` (a
cpuset with `/dev/cpuset/cpus`, as its cgroup move), not in xnuhost, which never leaves its cgroup: not done in this
change.

**The parallel test** (`runtime/xnuhost/check-arm64.sh parallel`, the C Makefile's `parallel-arm64` until 2026-11-23; the test kernel's `testkernel-parallel[=SCALE]`, 8
vCPUs): N threads of one task, one per vCPU, each the same user-mode work (100 rounds of a 100,000-iteration loop and a
system call), all at once, for N = 1, 2, 4 and 8; the wall time and the speed-up against one thread
(`PARALLEL 8 threads: 555 ms, speed-up 3.67 (ideal 8)`). With nothing in xnuhost's path serialising the vCPUs it stays
near N while the host has a core per thread. `PAR_SET="--set cpu-pin=2"` pins vCPU *i* and its thread on host CPU *i*.
On the 4-CPU PC under qemu-aarch64 only up to 4 can show (the results are in section 2.6c). What it does not show: XNU's
own scheduler and locks (the test kernel has none); a PC boot of iOS can show those only with qemu's 4 cores and its
times.

### 2.6c Before and after (fix-host-overhead, 2026-11-15), and the phone test

**The PC** (x86_64, 4 CPUs, qemu-aarch64 user mode, so times are qemu's, about ten times the phone's, and the box ran other
agents' kernel builds and boots throughout: load 5-20). Two boots of iOS with 8 vCPUs, `--ram 4G`, a fresh Data store
each, 2,400 s each (`timeout`, exit 124 both): before = main 1b5fb9f (xnuhost and kernel.kc 4fb7be43), after = this branch
(kernel.kc 0b1dad07, hosted 0075 without the two GPU-path functions added after the boot; the final kernel.kc bffa13ad
was built and prelinked, not booted). Totals of all 30 s windows (`oneios-perf:` and `perf:`):

| | before | after |
|---|---|---|
| SpringBoard started (s after xnuhost's start) | 872 | 349 |
| user stays / system calls / faults | 1,679,239 / 612,881 / 985,102 | 4,348,990 / 1,948,793 / 2,224,827 |
| vCPU threads' CPU (`perf: cpuN`, core-seconds) | 4,021 | 3,687 |
| the same per user stay | 2.39 ms | 0.85 ms |
| pmap user maps + unmaps | 1,763,898 | 4,419,042 |
| of them joined in the queue (one host call less each) | - | 1,421,798 (32 %) |
| raises of another vCPU's line / signalled / skipped (already up) | not counted (all signalled) | 1,426,233 / 806,462 / 619,771 (43 % saved) |
| timer sets / to `timer_settime` | 4,822,253 / 4,822,253 | 6,090,227 / 5,898,001 (3 % saved) |
| fill time per attempt (`filled from the tables` us / faults) | 398 us | 54 us (14,628 fills switched out, not timed) |
| kernel entries made again unchanged (no host map) | - | 0 |

What this shows and what it does not: the counts (joins, skipped raises and timer calls, the fills' honest time) are
properties of the code and carry over to the phone. The times and the boot's progress do not compare cleanly: the before
boot ran while the box was loaded about twice as much, so the 2.6 times as many stays in the same 2,400 s, SpringBoard 523
s earlier and a third of the vCPU time per stay are partly the box. The unchanged-entry skip never fired here: the PC's
kernel maps are new entries (a different cause than the guess in hosted 0075's README; the skip stays, cheap, for the
phone to say). `oneios-cpu` lines are not printed on the PC (the GPU archive's report runs with 3D only); the window's
bound is the host test's (`test_cpu_window`, now `cpu_window` in `src/cpu.rs`). uclamp: 0 on the PC (no CPU plan without the bridges).

**`make bench-arm64 BENCH_SET="--set cpu-pin=2"`** (the C xnuhost; today `check-arm64.sh bench`) on the same PC, alternated, two rounds each (p50, us): null system call
19.0/19.0 before, 19.0/20.0 after; a fault filled by `task_map` 38.0/37.0 before, 38.0/37.0 after; timer interrupt of a
running thread 29.0/28.0 before, 28.0/27.0 after (the means fell, 21.6-20.1 against 20.4-65.0 for the null call, but the
box's load moved them more than the code did). The single-thread trip did not change, as expected: the fixes are in
what runs between trips (queue, GIC, timers) and in what several vCPUs share.

**The parallel test** (`parallel-arm64`, the C xnuhost, 8 vCPUs, scale 3, after the warm-up round; same box and load): 8 threads took
357-573 ms after and 418-474 ms before against 92-270 ms for one thread: speed-ups 2.2-4.4 (after) and 1.6-5.2 (before)
of an ideal 8, where the PC has 4 cores and other agents' jobs on them. The 2-thread and 4-thread runs are within the
same spread. So on this box nothing shows a serialisation in xnuhost's path (8 threads never took 8 times one thread),
and the PC cannot show more than its 4 cores; the phone's run (below) is the measurement that counts.

**The phone test (for the Mac; one session before, on main, and one after, on this branch, the same image set).**

1. Before the switch and in the session: `oneios-tool phone cpu > cpu-before.txt` (then `cpu-after.txt`). Read: the X4's
   top and boost (`cpu7 Cortex-X4 ... top ... boost ...`), any `CAPPED` domain, `cpufreq boost`, xnuhost's cpuset and
   `allowed` CPUs (all 8 is right; `0-1` means the vCPUs cannot reach the big cores), one `xnu-cpuN` thread on each CPU.
2. The parallel test on the phone's own cores, no session open: push the static xnuhost and the test image that `runtime/xnuhost/check-arm64.sh parallel` builds (`runtime/target/machine-static/aarch64-linux-android/release/xnuhost`
   and the folder `runtime/target/machine-check/testimage`) to a scratch folder (`/data/local/tmp/par`), and run there as root the command line
   of the script's `parallel` boot with `--set cpu-pin=2` (and once without): `PARALLEL N threads: ... speed-up` for 1,
   2, 4, 8. Right: near N up to 8 (the A520s are slower, so 8 lands
   between 6 and 8 by their share).
3. Idle lock screen 2 minutes, then Settings scrolled for 60 s, then a text field typed into for 60 s (the keyboard), each
   with `oneios-tool phone cpu --trace 30 --out scroll.pftrace` during the scroll. From `xnuhost.log` per 30 s window:
   - `perf: ... task_map N carried by a release, ... task_unmap N queued, M ordered` and `perf:   queue: J ... joined`:
     J is the host calls saved; the kernel's `oneios-perf:   pmap: user maps, unmaps` against xnuhost's counts;
   - `perf:   queue: ... raises ... K already up (no signal)`: tgkills saved;
   - `perf:   queue: uclamp: U given to iOS threads` (not 0 in a session), and `phone cpu`'s iOS threads by uclamp.min
     (1024 on the P cores while scrolling, 0 for the background);
   - the kernel's `oneios-perf:   0075: fills switched out N ...; kernel entries made again unchanged M`, and the fill time
     per fill in the first `oneios-perf:` line (it should now be a few us, not 230);
   - `thermal: 30 s: host CPU ...% of 800 (sys ...%, user ...%) ... xnuhost ...%`: the same work with less `sys`;
   - `oneios-cpu: 10 s: ... (all N%)`: N never above 800;
   - `display: ... flushes` and the frame times of `oneios-gpu-report` while scrolling and typing.
4. The trace (`ui.perfetto.dev`): the CPU each `xnu-cpuN` and iOS thread ran on, the clocks of cpu7 (up to 3.39 GHz with
   boost on) and of the A720s while scrolling.

### 2.6d A Mach RPC round trip, and the interrupt path (fix-mach-rpc, 2026-11-15)

**What a round trip is under xnuhost.** A synchronous XPC message (`_xpc_connection_send_message_with_reply_sync`) is one
`mach_msg2` trap that sends and then receives the reply. XNU's `ipc_mqueue_post` makes the server's thread, blocked in its
own receive, runnable with `thread_go`; `thread_setrun` puts it on an idle processor and sends that CPU an IPI
(`sched_ipi_perform` -> `cpu_signal_internal`); the client then blocks in its receive and its vCPU idles. XNU never hands
the CPU straight to the receiver for Mach messages (only semaphores and eventlinks use `thread_handoff`). So one round trip
is: the client's user stay (svc, up hand-off), the kernel's send, an IPI, the server's vCPU woken from `cpu_idle` (a futex
wake of its thread), the interrupt taken there, the switch to the server thread and its release (down hand-off); the
server's work; then the same back. Two user stays (four hand-offs), two IPIs, two wakes of an idle vCPU, two interrupts.

**Where it spent its time that was ours.** The hosted kernel drove the interrupt controller through ArmCpu
(`external/Droid-VM-UniXNU/drivers/ArmCpu/ArmCpu.cpp`): `signalCPU` -> `spiIpiSend` is a store to GICD_ISPENDR (with
`armcpu-spi-ipi=1`), and `ArmCpu::interrupt` takes each interrupt with `mrs ICC_IAR1_EL1`, a store to GICD_ICPENDR (for an
IPI SPI), `msr ICC_EOIR1_EL1`, and `mrs ICC_IAR1_EL1` again to see 1023. At EL0 each is a trapped instruction: a SIGSEGV or
SIGILL on the vCPU's thread, xh-core's MMIO emulation or `sysreg::emulate` under the GIC's lock (a distributor store
recomputes all eight CPUs' lines, `gic.rs`: the distributor's store -> `update()`), and the signal return. Five signals per IPI, ten per
round trip, and three per timer interrupt. A PC boot (fix-host-overhead's `run-fpr/off`, 2,400 s) emulated `ICC_IAR1_EL1`
2,293,015 times and `ICC_EOIR1_EL1` 1,171,059 times: more than every other trapped instruction together.

**The fix (hosted 0081 and xnuhost's `irq_take`).** The kernel sends IPIs with the `ipi` host call (SGI 0, which ArmCpu also
takes as an IPI) and takes the timer and IPIs with a new host call, `irq_take` (`hostcall.h`, appended: it acknowledges only
the timer, an SGI or an IPI SPI; a device's interrupt comes back with `XH_INTID_UNTAKEN` and stays pending, for ArmCpu's own
path), then `irq_eoi`: function calls instead of signals. `oneios-fast-irq=0` is the old path, `=2` the two in turn by `oneios-perf` period. The lasting lines it adds to
each `oneios-perf:` period (kernel/patches/oneios-hosted/README.md, "0081"):

```text
oneios-perf:   rpc: N Mach RPCs (mach_msg send + receive), round trip mean X us, max Y us; under 100 us A, 100 us-1 ms B, 1-10 ms C, over 10 ms D (R per s)
oneios-perf:   wake: N threads woken by an IPI: made runnable -> IPI sent S us, -> interrupt taken T us, -> running U us (means), W us in all (max M us); other dispatches O, mean wait Q us
oneios-perf:   0081: interrupts by host call N (timer T, IPI I; X us a dispatch), by ArmCpu's trapped registers D (Y us a dispatch); IPIs sent by host call H, by ArmCpu A (oneios-fast-irq=1)
oneios-perf:   syscalls: N back to their own thread, X us each (the call's own kernel work); M blocked, Y us each to the vCPU's next user entry or idle (the switch, the next thread's way back)
```

The `rpc:` round trip includes the server's own work (a slow wifid shows as a long tail, not as a slow trap path); `wake:`
splits the wait of a thread an IPI woke at the IPI; `syscalls:` splits 0041's per-call mean (which runs to the vCPU's next user
entry) into calls that came back to their own thread and calls that blocked. xnuhost's `perf:   release:` line (new) counts
the `sched_setaffinity` calls that put an iOS thread with another vCPU's core and the host's work in `thread_run` before
each release (placement, uclamp, the deferred queue): time the `down` line does not include.

**The bench** (`runtime/xnuhost/check-arm64.sh rpc`, the C Makefile's `rpc-arm64` when these numbers were taken; the test kernel's `testkernel-rpc`, docs/building.md section 4): a client
task's thread on vCPU 0 and a server task's on vCPU 1 trade a request and a reply as above, 4,000 round trips per mode:
mode 0 = ArmCpu's trapped registers and GICD stores, mode 1 = the host calls of 0081, mode 2 = both threads on one vCPU, no
IPI (the floor: what a direct hand-off of the CPU would cost). On the x86_64 PC under qemu-aarch64 (times are qemu's, about
ten times the phone's; the box had a load of 30-42 from other agents' builds and boots throughout), two rounds, us:

| | round trip p50 | mean | IPI sent -> the idle vCPU out of `cpu_idle`, p50 | 4,000 round trips |
|---|---|---|---|---|
| mode 0, ArmCpu's way (before) | 485 / 672 | 2,167 / 1,742 | 21.0 / 21.0 | 8.7 / 7.0 s |
| mode 1, host calls (0081) | 184 / 144 | 1,323 / 1,210 | 7.9 / 7.9 | 5.3 / 4.9 s |
| mode 2, one vCPU, no IPI | 52 / 52 | 485 / 569 | - | 2.0 / 2.3 s |

xnuhost's stop report for mode 0: `emulated: insn 0xd538cc08 16000 times` (ICC_IAR1_EL1) and `0xd518cc28 8000 times`
(ICC_EOIR1_EL1) for 8,000 interrupts, plus the 8,000 GICD_ISPENDR and 8,000 GICD_ICPENDR stores; mode 1: none. Both modes:
one futex sleep per hand-off on each side (`futex sleeps: vCPU ~7,900, iOS thread ~8,000 of 8,001 hand-offs`), the host's
work before each release 1.2-2.0 us. So on this box the host calls take the round trip's p50 to about a third, and what is
left over the floor is the two wakes of an idle vCPU (a futex wake of a sleeping thread on another core, and its way out).

**What one system call costs, and where** (the C xnuhost's `make bench-arm64`, today `check-arm64.sh bench`; the same PC and load, p50): the null system call 24.0 us
kernel to kernel, of it svc -> the kernel has the trap 11.0 us (seccomp's or, under qemu, the marked instruction's signal,
the stub's register copy, the store and the vCPU's futex wake) and the release -> user code 12.0 us (the store, the futex
wake of the iOS thread, its way out of the futex and the signal return); each side sleeps in a futex every time (`futex
sleeps` = hand-offs). The phone's 36-37 us per call in `oneios-perf:` is not this: it is the kernel's own time after the
trap up to the vCPU's next user entry (0041), which for a call that blocks includes the switch and the next thread's way
back; the new `syscalls:` line says how much of it is which. On the phone (2026-11-12) each hand-off was 10-12 us at p50
(2.6a). Not measured on the phone yet: the same bench there (the command lines of 2.6c step 2 with `testkernel-rpc
testkernel-rpc-mode=0|1|2`).

### 2.6e Clocks and clamps: who asks the host for its top clock (cpu-clocks, 2026-11-23)

**What the session of 2026-11-23 20:23 showed** (`out/session-logs/2026-11-23-mac-s2023/xnuhost.log`, runtime 0.1.19):
every vCPU 34-58% of a core with 0-17% idle and 2,200-19,700 traps/s in the busy periods; `thermal: 30 s: host CPU`
637-760% of 800 (xnuhost about 400% of it, the iOS tasks' own processes the rest); the SoC at 92-98 C in 14 of 21
periods. drm2kgsl logged `vCPU uclamp.min 1024: given to the VMM (8 vCPU threads, ...)` 41 s into the session and held
it for 470 of the session's 651 s (the GPU's level never came down: `docs/gpu.md` 2.2). That clamp went:
- on all eight vCPU threads directly (`drm2kgsl_vcpu_uclamp`), the E vCPUs' and the idle ones' too. A vCPU thread's
  clamp counts whenever it runs, and an idle vCPU runs at each timer and IPI: every core was asked for its top clock
  through every animation, for the guest kernel's own work as much as for the frame's;
- through the release to every iOS thread that was not background or utility: the busiest processes of `oneios-cpu`
  (lsd, PerfPowerServices, chronod, CommCenter) are default-class daemons, and they ran with 1024;
- and it fought the guest's own request on the same thread: the cpufreq device set the vCPU thread's clamp at each
  `SET_PERF`, drm2kgsl at each animation's start and end; the last one won. DroidVMCpufreq also keeps its request while
  a vCPU idles, so a vCPU that last ran a UI thread kept 1024 for every wake of its idle loop.

**The fix** (`src/cpu.rs`, `src/task.rs`, `src/session/bridges.rs`; `runtime/bridges/cpu` `Cpufreq::with_apply`):
xnuhost alone sets a vCPU thread's clamp, on that thread, to the floor of the work it does now (`cpu::own_uclamp`, one
`sched_setattr` per change):
- at each release (`thread_run`): the released thread's floor, `cpu::release_floor(hint, request, boost)`, given to the
  iOS thread and to the vCPU thread with it (the kernel's work after that thread's trap is done for it);
- at the guest's idle (`cpu_idle`): 0. What wakes an idle vCPU is the kernel's own short work;
- at the guest's `SET_PERF`: its value (the device records the request and hands it to xnuhost; it no longer sets a
  clamp itself).

`release_floor`: no hint (a kernel before 0075): the request or the boost, the higher (as before); utility, background,
darwin-background: 0 and no ceiling; otherwise the class's floor (realtime, UI, above-UI 1024, user-initiated 768) or the
vCPU's request, the higher, raised to drm2kgsl's boost only when that is not 0. So the boost reaches the frame's threads
(their class, or an interactive group's default-class thread, for which DroidVMCpufreq asks the vCPU's top) and nothing
else; a daemon's default-class thread on a vCPU nobody asked for gets 0. `drm2kgsl_vcpu_uclamp` now only stores the value
(it returns the started vCPUs: its log line still says `8 vCPU threads`). Host-tested: `cpu::tests::release_floor` (each
class, with and without the boost and a request) and the bridges' `cpu` test (the device hands each change of a request
to the VMM once). `thread_run` and `cpu_idle` are aarch64 code: `runtime/host-tests.sh` builds and lints them and its
boot part runs them (the test kernel under qemu-aarch64, without uclamp).

What it does not change: a UI or realtime thread keeps 1024 while it runs, an interactive group's default thread the
guest's request; no ceiling anywhere (the owner's rule; #57). Not covered: a kernel thread of the guest (no release) runs
at the clamp of the last thread its vCPU released until the vCPU idles, releases another thread or gets a request (the
guest does not tell the host when it switches to a kernel thread).

**The topology** (which work runs where): two clusters as section 2.6b says, E = the A520s (vCPU 0-1), P = the A720s
and the X4 (vCPU 2-7; the X4 is vCPU 7). Inside the P cluster XNU's Edge scheduler picks by idle state and the thread's
last CPU (`choose_processor`, `osfmk/kern/sched_prim.c`), the lowest idle CPU first when the thread has no last CPU
there; a CLPC can keep work off a core with `sched_perfcontrol_edge_cpu_rotation_bitmasks_set` (its migration mask),
which DroidVMCpufreq does not call. In the 20:23 session the X4's vCPU had the most traps (15,000-19,700/s against
2,900-10,400 on the A720s) at the same share of a core: the busiest UI thread lived there, where it should run. With
the E vCPUs 0-7% idle in every busy period, background work spills to the P vCPUs through 0085's edges; after this fix it
runs there with no floor. Whether light work should also be kept off the X4 (the migration mask) waits for the `perf:
qos:` line below: not done here.

**The governor**: One UI's power HAL is gone in a session, so the policy's governor (WALT on the S24 Ultra,
`docs/hardware.md`) alone picks the clocks, from the load and the clamps above. xnuhost writes none of its tunables; it
now logs them at the start, read only from named files: `cpu: policyN: governor walt; up_rate_limit_us ...,
down_rate_limit_us ..., ...` (those that exist of `rate_limit_us`, `up_rate_limit_us`, `down_rate_limit_us`,
`hispeed_load`, `hispeed_freq`, `target_loads`, `pl`, `boost`, `rtg_boost_freq`, `adaptive_high_freq`) and `cpu: WALT
sched_boost N`. No tunable is changed: the measured cause of the top clocks is the clamps, and a tunable written blind
would also need the fork's return to put it back. If the next session's clocks line still shows a P policy at its top
while its busy share is low, the tunables line says what to change, and the fork's `clocks_save` / `clocks_tops` would
carry it (as they carry `scaling_max_freq`).

**Lasting lines** (cheap, in every session):
- `perf: qos: the iOS threads' turns, % of each vCPU's wall time (realtime/UI/default/background): cpu0 E 0/2/10/40,
  ...`: the time `thread_run` waited, by the kernel's class of the released thread (`perf_hint`; one add per release);
- `perf:   queue: ... uclamp: U given to iOS threads, V to vCPU threads (their own: 0 at each guest idle)`;
- `perf:   release: N iOS threads put with another vCPU's core (sched_setaffinity), M of them across E and P` (a
  hand-off that crossed clusters);
- `thermal: 30 s clocks: policy0 (cpu0-1) busy 40%, mean 1478 of 2265 MHz, at the top 33%, under half 33%; ...`: each
  cpufreq policy's busy share (its CPUs' ticks in `/proc/stat`) and frequency residency over the period
  (`policyN/stats/time_in_state`, `related_cpus`, `cpuinfo_max_freq`; named files, policy0..15;
  `runtime/bridges/thermal/clocks.rs`). `oneios-tool log cpu` makes tables of the qos and clocks lines.

**What to compare on the phone** (the 20:23 session against the next one, the same use): `thermal: 30 s` (host CPU, the
SoC), the clocks line's `at the top` and `mean` of policy2, policy5 and policy7 against their `busy`, `perf: cpuN` (% of
a core), the `uclamp:` counts (V a few thousand a period at most: each is one `sched_setattr` of a few microseconds), and
`perf: qos:` (background on cpu0-1, UI and realtime on cpu2-7). The UI must not get slower: the `gpu:` frame times and
the `display:` lines as before. Not measured on the phone yet.

### 2.7 Devices

All devices are virtio-mmio version 2 windows (xh-core's `runtime/core/src/virtio.rs`: split queues, indirect descriptors, the status
and feature handshake) at `0x0a000000 + n * 0x1000`, SPI `32 + n`, in this order: the disks, input, GPU.

| Device | File | Backed by | Guest driver it is written for |
| --- | --- | --- | --- |
| Block, read-only (`F_RO`) | `src/virtio_blk.rs`, `src/blk.rs` | `os.img`, `systemos.img`, `app.img`, a tar `data.seed` | UniXNU `NativeVirtioBlock` (node `virtio-mmio-N`, `compatible` `virtio,mmio`) |
| Block, writable | same | `<data>/data.img`, sparse, 32 GiB logical, flush = `fdatasync`, on the disk's flush thread ("Disks" below), timed (`disk NAME: 30 s: reads .., writes .., flushes N in T ms (max X ms)` every 30 s per busy disk, `since the start` at the stop, `disk NAME: a flush took N ms` for the first 16 of 500 ms or more: a flush holds every other request of the guest to that disk) | the Data store window (node `data-store`) |
| Input (and, with `sensors=auto\|ssc`, the motion device: "Sensors" in 2.8) | `src/virtio_input.rs`, `src/input.rs`, `src/motion.rs` | `/dev/input/event*`: the multitouch events of the one device that is the screen (`INPUT_PROP_DIRECT`; a multitouch pointer device such as the S24 Ultra's `sec_touchpad` is not taken; the log line gives the screen's x and y ranges) and the three side keys, unchanged evdev codes, the panel's own axis ranges | UniXNU-droidvm `DroidVMVirtioInput.c` |
| GPU, 2D scanout | `src/virtio_gpu.rs`, `src/display.rs` | the display back end | UniXNU-droidvm `DroidVMVirtioGpuDisplay.cpp` under IOMobileFramebufferLegacy: `kernel/patches/oneios-hosted/display/` (a 2D resource per surface, up to 16384 backing entries each) |
| Console | `src/uart.rs` | the log | XNU's PL011 console (`uart0`) |
| Bridge channel, network, sound | `runtime/bridges` (the bridges agent's; `docs/hardware.md`) | haptics and Bluetooth HCI; a tap with built-in DHCP; an AAudio helper process | `drivers/bridge`, `DroidVMVirtioNet.c`, `DroidVMVirtioSnd.c` |
| CPU frequency | `runtime/bridges` | a plain MMIO window per vCPU at `0x09000000` (`/arm-io/cpufreq`) | `DroidVMCpufreq.c` |

Interrupts: a chain returned on the used ring sets the interrupt status (0x60) and raises the device's SPI, whatever the
driver's `VIRTQ_AVAIL_F_NO_INTERRUPT`; the driver's acknowledgement (0x64) lowers it. A device model may name queues
whose flag the transport honours (`Device::quiet_queues`, 2026-11-25; none by default): a return there with the flag
set raises nothing. Only the media device names any (both its queues): the codec's transport polls the command queue,
which xnuhost answers inside the guest's notify, and waits on the event queue's interrupt
(`kernel/patches/oneios-hosted/codec/README.md`, "The transport on the interrupt"). The media device's own worker thread
(the event queue's producer, `runtime/bridges/encoder/media_device.rs`) sleeps on a condition variable until a command,
a kick of the event queue, a decoder's result or an encoder's coded unit wakes it (the helper backend's socket receive
thread wakes it with each unit, `Session::wakes`, since 2026-11-25; before, it looked every 1 ms for 2 s after each
command), 50 ms at most while an encoder session is open or an event waits for a guest buffer; it says its wake-ups in
`media: 30 s: worker woken N: by data ..., by the timeout N; 1 ms looks N` (`runtime/bridges/encoder/README.md`, "The
worker waits for the encoder").

A task trap that no page-in can resolve is logged by xnuhost itself with the registers (`tasks: pid N (tid T):
trap kind K signal S far ... esr ... insn ...; pc ... lr ... sp ... fp ... x0 ...`): a fault address outside what an
iOS task may use (below 4 GiB or at and above the stub region), an undefined instruction, a breakpoint; the first 64
of a run. It shows a process's fatal fault when the kernel does not log that process (`sheng-exc-name`).

Read-only images are checked on read (`src/imageset.rs`, `Verify::range`): each 1 MiB chunk is read whole and its
SHA-256 compared with `<image>.hashes` the first time a read touches it. The chunk is read and hashed outside the
image's lock, in a buffer of the reading thread, so several vCPUs check different chunks at once (the guest reads
the 5.8 GiB shared cache on all of them before its first process runs); the lock covers only the checked bits. The
hash uses the CPU's SHA-256 instructions when `AT_HWCAP` has `HWCAP_SHA2` (`is_aarch64_feature_detected!("sha2")`), plain code otherwise. Tested: the C `make test` (2026-11)
(the host build in plain C, the arm64 build's test image under `qemu-aarch64 -cpu cortex-a710`, which has SHA2), and the Rust `src/imageset/tests.rs` (every check of the C test, plus the static test binary under `qemu-aarch64 -cpu max`: the SHA2 path); not
measured on the phone.

The bridges are linked in (the crates `onebridges`, `onecamera`, `oneencoder`; `src/session/bridges.rs` sets them up) and come after the GPU in the window order: `bridge`, `net`,
`virtio-snd`. They are on in a takeover session and off in `--mode bench` (`--set bridges=0|1`). With them,
`/cpus` gets the host's cluster types (E vCPUs first), each vCPU thread is put on one host CPU of its cluster
(`cpu-pin=1`, section 2.6a; `cpu-pin=0`: any CPU of its cluster), and so is the iOS thread a vCPU runs (when the vCPU
changes). The speaker's output latency (since 2026-11-15, fix-audio-latency): once virtio-snd has the AAudio helper,
xnuhost asks it for its measured latency before the guest boots (`AudioBackend::probe_latency` of `runtime/bridges/audio/backend.rs`, at most 2 s: the helper
opens the speaker in silence, which then lingers for iOS's first sound) and adds `oneios-audio-latency=FRAMES` to the
boot arguments, unless they have one (`audio: speaker latency N frames (N ms) measured by the helper:
oneios-audio-latency=N`, or `audio: output latency N frames (N ms): the boot arguments' oneios-audio-latency, not
measured`, or `audio: speaker latency not measured (...): the kernel's default output latency`); the guest publishes
it as the speaker's output latency ([hardware.md](hardware.md) section 6). `ONEIOS_AUDIO_PROBE=0` in the environment
leaves the probe out. Bluetooth (`--bluetooth`,
`bluetooth=` in `xnuhost.conf`): `hidl` bridges the phone's controller (`/bluetooth` transport-encoding 1 =
bluetoothd's H4 UART transport, which on iOS 27 is a Skywalk pipe: [hardware.md](hardware.md) section 4; transport-speed 3000000, the address of `/mnt/vendor/efs/bluetooth/bt_addr`);
`virtual` gives iOS none (encoding 10, the guest driver hides the nub); the default `auto` is `hidl` when the bridges
are on and hwservicemanager returns `android.hardware.bluetooth@1.x::IBluetoothHci/default` at the start (asked once
and let go; the HAL is opened only when bluetoothd connects), else `virtual`. `sim` (a boot with no phone, the PC):
a stand-in controller (`runtime/bridges/bt/hci_sim.rs`) behind the same bridge channel and the same `/bluetooth` node
(transport-encoding 1, the made-up address of `/chosen mac-address-bluetooth0`); with the bridges off xnuhost makes a
bridge device that serves only it. One log line says which:
`bluetooth: on: the phone's controller through its HAL 1.1, address ...`, `bluetooth: on: a stand-in controller
(bluetooth=sim), address ...` or `bluetooth: off (...)` with the reason. Boot arguments whose `owner-hide-vmm=` list
names bluetoothd get an error line (`boot arguments: owner-hide-vmm names bluetoothd: ...`) and are left as they are:
24A446's bluetoothd crashes in a loop with the virtual machine hidden ([hardware.md](hardware.md) 4). The guest's
Bluetooth driver changes the nub's transport-encoding from 1 to 10 when the host serves a controller, and the kernel
adds `BT_HCI_TRANSPORT=H4:POSIX` to bluetoothd's environment (hosted 0087; since 2026-11-23): in a VM bluetoothd powers
on only with 10, and the variable keeps its HCI on the H4 transport ([hardware.md](hardware.md) 4, "Bluetooth powers
on"). So boot arguments that give bluetoothd `bt-transport-override=N` with N other than 10, read as bluetoothd reads
it, get an error line (`boot arguments: bt-transport-override=N: bluetoothd's power init passes only with 10 in a VM,
...`); `=10` gets an information line (it is what the driver gives already; with `oneios-bt-hci=0` it is bluetoothd's
own simulated controller: "The virtual transport is bluetoothd's own").
Haptics (`haptics=` in `xnuhost.conf`, `--set haptics=...`; 2026-11-13): `auto`, the default, opens the phone's
force-feedback device (`haptics: on: /dev/input/eventN ...`) when the bridges are on; `off` gives iOS no actuator
(`haptics: off (haptics=off)`: no Actuator audio device, no `/product/haptics`); `sim` (a boot with no vibrator,
the PC) is a stand-in actuator (`runtime/bridges/haptics/haptic.rs`) that takes, counts and logs every request
(`haptics: on: a stand-in actuator (haptics=sim)`), behind the same bridge channel; with the bridges off xnuhost
makes a bridge device that serves only the stand-ins (with `bluetooth=sim` one device serves both). Each played
effect is logged for the first 8, then at 16, 32, ...; `haptics: counters: ...` at those points and at the end, and
`haptics: 60 s: +N requests ...` every 60 s when they moved (section 2.9b)
([hardware.md](hardware.md) section 5). With `haptics-native-impacts=1` (the table above; ported from the S26 Ultra fork's session opt-in, 2026-11-21) the
start says `haptics: native impacts asked for (...)` and the bridge `haptics: native impacts enabled ...` once the HAL
has the three presets; without them the bridge stays on VIB/STOP.
The video encoder (`encoder=` in the image set's `xnuhost.conf` or its `overlay/userland/xnuhost.conf`, `--set
encoder=...`, the command line wins; hevc-encoder and hevc-guest, 2026-11-15; until 2026-11-22 the image set's
`encoder=` was read and then dropped, so only `--set` turned it on):
`off`, the built-in default, gives the guest no device (the S24 Ultra's host-table row gives `auto` since 2026-11-28, under the command line and the image set: docs/hardware.md 7h); `auto` carves the 48 MiB media pool
between the GPU pool and the boot framebuffer (both inside the 64 MiB the hosted kernel lets a task map), makes the
`droidvm,virtio-media` device `/arm-io/virtio-media-0` with its `media-host-pool`, and adds `droidvm-vt-paravirt=1`
to the guest's boot arguments, so the kernel's codec archive publishes iOS's paravirtualized VideoToolbox driver.
With the bridges on (a session) the encoder is the phone's MediaCodec through `bin/oneios-encoder` (`encoder: on:
mediacodec-helper backend, media pool 48 MiB at 0x... (4 sessions)`, then `decoder: on: mediacodec-helper backend, H.264, HEVC, VP9, AV1 (decoder-codecs=)` or `decoder: off ...`; `encoder: off: no bin/oneios-encoder in
the runtime (the MediaCodec helper)` when the runtime has none). With the bridges off (a PC boot) it is the stand-in
encoder inside xnuhost (`encoder: on: stand-in backend, ...`, then `encoder: the stand-in encoder (encoder=auto with
the bridges off: no phone): no MediaCodec; every frame is coded as it is (I_PCM), no compression`); `sim` is the
stand-in always, also in a session. `encoder: session N: HEVC WxH on <backend>` at each encoder session, and every
30 s while the device's worker wakes `media: 30 s: worker woken N: ...; 1 ms looks N` (2026-11-25; `log summary`'s
encoder part) ([hardware.md](hardware.md) section 7g, `runtime/bridges/encoder/README.md`).
The network (`wifi=` in `xnuhost.conf`, `--set wifi=...`): `auto`, the default, makes the phone's wlan0 the guest's
en0 (`wlan::prepare` of `runtime/bridges/wifi/wlan.rs`, [hardware.md](hardware.md) section 3): the `net` window's virtio-net device carries wlan0's
address in its configuration, its frames go to and from wlan0 by AF_PACKET, and the guest driver's station messages
(frames of type 0x88b5) go to nl80211 and the vendor's supplicant. It keeps what it changes on wlan0 (GRO,
`disable_ipv6`) in `/data/session/wifi.state`. No wlan0: one line (`wifi: off: no wlan0 ...`) and no `net` window.
`off` makes no network device; `tap` is the old route (a tap and `bin/oneios-net.sh`'s NAT), for a bench; `sim` is a
made-up radio for a boot with no phone (it also works with the bridges off): two networks, "OneiOS open" and "OneiOS
WPA2" (passphrase `password`), that the guest can scan and join, and no frames (`runtime/bridges/wifi/sim.rs`).
A bridge whose Linux side is missing logs one line and is left out. At a stop the motor is stopped, the Wi-Fi
bridge leaves its network and gives wlan0's settings back (`wlan::close`), and with `wifi=tap`
`bin/oneios-net.sh down` removes the tap's rules. Every virtio node carries its SPI as `interrupts` and as
`droidvm,intid` (the name UniXNU-droidvm's transports read).

Where UniXNU's RamBlock keeps the Data seed in RAM and changed pages in a store window, xnuhost has
Linux's page cache and a sparse file: the store is a raw image.

**Disks: where a request is served** (`src/virtio_blk.rs`; fix-widget-kill-loop, 2026-11-15). The guest's driver
(`kernel/patches/oneios-hosted/storage/OneiOSHostedDisk.cpp`) sends one request at a time per disk and polls for the
answer, and xnuhost serves reads and writes inside the notify, on the vCPU that wrote the register (a `pread` or
`pwrite` of the image: Linux's page cache). A **flush** was served the same way: `fdatasync(2)` of the 32 GiB
`data.img` ran on the vCPU until the phone's file system had written every dirty page of the store out, and every
APFS transaction, `F_FULLFSYNC` and `F_BARRIERFSYNC` of iOS (SQLite's commits, the atomic plist writes of cfprefsd,
chronod's timeline and snapshot archives) sends one; meanwhile the vCPU ran nothing else and took no interrupt. Now a
writable disk offers `VIRTIO_BLK_F_LATE_FLUSH` (bit 23, OneiOS's own: the top device-specific bit, which virtio-blk
leaves unused); a driver that accepts it gets its flushes served by the disk's flush thread (`xh-blk-flush`, a normal
thread), completed from there into the used ring, and waits for them asleep (`IOSleep(1)` between looks, 60 s before
it resets the disk; reads and writes keep the 10 us spin of a notify that answers at once). A reset of the device drops
a flush in progress. `blk-late-flush=0` (`--set`, `xnuhost.conf`) does not offer the bit: the flush on the vCPU, as
before; a kernel from before this change does not accept it, so it gets that too. The start line says which:
`disk: a flush of a writable disk runs on the disk's flush thread when the guest's driver asks for that (blk-late-flush=1)`.
What the flush itself costs is the same either way (the same `fdatasync`); what changes is that the guest keeps the
vCPU while it runs.

A lasting line per disk, once a minute while it is used: `disk N: 60 s: reads R (M MiB, T ms, slowest S ms), writes W
(M MiB, T ms, slowest S ms), flushes F (on its thread|on the vCPU: T ms, mean A ms, slowest S ms)` (N as in the start
line `disk N: PATH, ...`). Reads and writes are the time the vCPUs spent in the host for them (a read-only image's
includes its chunk checks), the flushes the `fdatasync`'s own time. On the phone the Data store's line says how often
iOS flushes and how long the phone's storage takes for it. A second, coarser line comes from `src/blk.rs` itself: every 30 s
per disk with requests, `disk NAME: 30 s: reads R (M MiB), writes W (M MiB), flushes F in T ms (max S ms)`, at the stop
the same `since the start`, and the first 16 flushes of 500 ms or more as they happen (`disk NAME: a flush took N ms`).
The `blk.rs` flush counts and times count every `fdatasync`, whichever side (vCPU or flush thread) ran it.

### 2.8 Display and input: the choice

- **Default `shm`.** In a OneOS takeover session One UI's surfaceflinger and composer keep running and
  hold the panel (`oneos/interface.md` section 4), so KMS is not available and the only way to the glass is
  a root SurfaceControl layer, which needs the framework's classes (OneOS draws its boot status that way,
  with a dex run by `app_process`). xnuhost therefore keeps the frame in a memfd and serves it on
  `<session>/display.sock` (`SOCK_SEQPACKET`): `hello {"XHD1", width, height, stride, format}`
  with the memfd as `SCM_RIGHTS`, then `flush {"XHDF", x, y, w, h}` per changed rectangle. The helper sends one
  message the other way, straight after it connects: `panel {"XHDP", width, height, hz, flags}`. The helper that
  owns the layer and copies from the memfd is `runtime/display` (`bin/oneios-display`, started by the fork's
  switch on One UI's side; its README has the phone test).
- **Direct frames: one copy a frame on the host, not two.** The guest's display driver scans out the compositor's
  own surface (`SET_SCANOUT_BLOB` of a guest blob, then a whole-frame `RESOURCE_FLUSH`; its swap statistics count
  these as `copy`, but the guest copies nothing). Before, xnuhost copied that surface into the memfd (18 MB a frame
  at 1440x3120) and the helper copied the memfd into its layer's buffer. A helper that sets `flags` bit 0 (the
  current one does, `--no-direct` turns it off) is sent `memory {"XHDM", RAM bytes low, high, slots, slot bytes}`
  with the RAM memfd and a table memfd once the RAM is mapped, and maps both read-only. Each flush of a blob
  scanout in B,G,R,X/A is then `frame {"XHDS", slot, seq low, seq high, 0}`: that slot of the table (4 of them) has
  the frame's offset, stride, size, changed rectangle and its segments as RAM memfd offsets, and the helper copies
  the rows from the guest's pages straight into the layer's buffer. xnuhost copies nothing. The guest's flush is
  answered when the helper sends `done {"XHDA", seq low, seq high, flags, 0}`: once the frame is in the helper's own
  snapshot, before it posts the buffer (since 2026-11-23; after the post before, `docs/gpu.md` 2.5), so the guest
  does not draw into a surface that is still being read (`virtio_gpu.rs` holds the reply on the fence list, as the
  display's: ctx 0). A helper that does not answer within 100 ms is answered for. One that leaves, or a newer
  helper, has its frames answered at once. `done` flags bit 0 (a frame it could not copy) turns direct frames off
  for that helper. 2D resources, a scanout outside RAM or of another format, and a helper without the flag use the
  memfd copy as before. `display-direct=0` (command line or `xnuhost.conf`) turns direct frames off for xnuhost.
  Every 10 s with frames the log has `display: 10 s: N frames copied by the helper from guest RAM (its
  answer: mean X ms, max Y ms; ...), T not answered within 100 ms, C flushes of xnuhost's own copy` (until 2026-11-23
  "posted to in the layer: mean X ms", the same measure: the frame sent to the helper's `done`). The segments'
  offsets are offsets in the RAM memfd, counted from the file's offset 0: the session (`src/session/board.rs`) gives `display::set_ram` the
  address of offset 0 (`fd_phys`, `src/mem.rs`) and the file's length up to the window's end (`head + size`).
  The window starts `head` bytes into the file (16 KiB on the phone, fix-ram-hugepages), so it is not
  `phys`. Until 2026-11-22 the C `main.c` passed `g_mem.phys`: the helper read every page of a frame 16 KiB too
  low, from another guest page, and the screen showed narrow stripes of other memory that flickered (runtime 0.1.13
  to 0.1.15; the direct path was written on 11-11, before the head existed, and merged on 11-20 without a phone run).
  Fixed and checked on the phone on 2026-11-22: the picture is correct, and the helper's 10 s line shows only the
  changed rows copied (24-43% of the rows while iOS moves, 15 of 99 frames unchanged on a still screen; before the
  fix 100% of the rows of every frame, because the wrong pages kept changing). Since 2026-11-22 the helper reads a
  frame's pages once, as soon as `XHDS` comes: each tile into the cache, hashed there, and copied on into its own
  snapshot of the picture only when it changed (`Snapshot`, `runtime/display/src/xhd/snapshot.rs`; `struct xhd_snapshot` of `xhd.h` in the C). Then it waits
  for surfaceflinger's buffer and fills it from the snapshot. Since 2026-11-24 (fix/frame-path) the two are threads of their own
  (`runtime/display/src/xhd/run.rs`): a reader takes the messages, reads each frame into the snapshot and answers at
  once, and the poster waits for the buffer, fills it from the snapshot and posts the newest picture; before, one loop
  read the next frame only after the post (its start 5.5 ms late on the phone, 2026-11-23). `display-post=serial`
  (docs/switches.md) gives the old timing back: xnuhost sends `options {"XHDO", flags, 0, 0, 0}` after the hello with
  bit 0 set, and the reader reads nothing more until the last change is posted; a helper older than the message
  ignores it. Bit 1 (`OPTION_PRESENT_AHB`, `display-present=ahb`, the default since 2026-11-25, so the message now goes at
  every start unless `display-present=window` and `display-post=apart`): the poster posts into three write-only
  AHardwareBuffers on a child of its layer instead of locking the layer's window (`runtime/display/src/xhd/present.rs`,
  `runtime/display/README.md` "Write-only buffers"); a helper older than that bit keeps the window. `XHDS`'s fifth word is xnuhost's send time (the low 32
  bits of `CLOCK_MONOTONIC` in us), and the helper's 10 s line says how long after it the read began. The guest does
  not draw into a surface while the helper reads it: the guest's display driver completes the swap for the
  compositor once the flush is posted (`DroidVMVirtioGpuDisplay.cpp` `droidvm_virtio_complete_on_post`), but it
  lands that frame (waits for the flush's answer, held here until `done`) before it posts the next one, and the
  compositor needs that next swap before it draws into the surface again. The glitch seen on UI changes on
  2026-11-22 ("it glitches and updates") was Android's copy-back, not the read: the helper locked only the changed
  rectangle, and `Surface::lock` copies back from the last buffer a region that loses changes once the queue has
  three buffers or more (`runtime/display/README.md`, "The layer's buffers"). xnuhost's own copy never had it,
  because the guest flushes its whole surface and every post was whole. Now the helper locks the whole buffer and
  fills what that buffer lacks itself. Why not zero
  copies: surfaceflinger takes `AHardwareBuffer`s, and the NDK cannot wrap a udmabuf of guest pages as one (a
  gralloc handle would have to be forged), and the guest's surfaces cannot be made gralloc buffers either, since the
  guest allocates them in its own RAM. Droid-VM's host does one GPU blit instead, with a Vulkan dma-buf import that
  needs the Turnip driver its app ships (`plans/DISPLAY_PIPELINE_DECOUPLING.md`); this phone's own Vulkan driver is
  not known to import a udmabuf. Tested on the PC (the C `tests/test_display_mode.c` with the helper's `xhd.c` on the
  other end, `tests/test_display.c`, the held flush in `test_xnuhost.c`; in Rust `src/display/tests.rs` with `oneios-display`'s xhd on the other end, `runtime/display/src/xhd/tests.rs`, `src/virtio_gpu/tests.rs`); not yet run on the phone.
  switch on One UI's side; its README has the phone test). Not run on a phone yet.
- **The frame's pixels** (`display-format=`, `--display-format`; `include/xnuhost/pixels.h`, kept for the kernel's C; xnuhost and the helper use its mirror `xh_core::pixels`, `runtime/core/src/pixels.rs`, which a test checks against the header; the C helper
  included it too):

  | `display-format` | hello `format` | Bytes a pixel | What the numbers are |
  |---|---|---|---|
  | `bgrx` (default) | 1 | 4 | B,G,R,X, 8 bits, sRGB-encoded, sRGB primaries: iOS's `BGRA` framebuffer (QuartzCore composites this display in sRGB, so Display P3's colours are clipped in the guest) |
  | `rgba16f` | 2 | 8 | R,G,B,A IEEE half floats, linear light, sRGB primaries, extended range (below 0 and above 1: colours outside sRGB, EDR): iOS's `RGhA` framebuffer, Android's `RGBA_FP16` with the dataspace `SCRGB_LINEAR` |

  With `rgba16f` xnuhost adds `oneios-display-wide=1` to the kernel's boot arguments (unless they have it), and the
  guest's display hands half-float surfaces to the virtio scanout as they are, in xnuhost's own scanout format
  `XH_VIRTIO_GPU_FORMAT_RGBA16F` (`0x52476841`, IOSurface's `RGhA`: virtio-gpu has no half-float format). The device
  takes that format in `RESOURCE_CREATE_2D` and `SET_SCANOUT_BLOB` (8 bytes a pixel, the stride a whole number of
  them) beside the eight 8-bit ones, and writes every flush in the frame's own format: half floats copied, 8-bit
  pixels (the boot console) decoded to linear light, and on a `bgrx` frame half floats encoded to sRGB and clamped.
  The log says `display: the frame's pixels: ...`. `rgba16f` needs `--display shm` (KMS takes 32-bit pixels), and
  `oneios-tool kernel display-capture` (PC) reads both (a half-float frame as sRGB, clamped). Host-tested (the C `make test`, now `src/virtio_gpu/tests.rs`: the 2D and blob paths, a pixel split
  between two blob segments); not run on a phone. Whether iOS then renders half floats is the Metal plug-in's
  (`userland/metal/DisplayWide.inc`, built on the Mac).
- **The mode is the one surfaceflinger runs the panel in.** The guest's frame (the boot framebuffer, the virtio-gpu
  scanout, `display.sock`'s hello, the touch range) is decided in this order, and each configured value wins on its own:
  1. `--width`, `--height`, `--hz`, else `width=`, `height=`, `hz=` in the image set's `xnuhost.conf` or its
     `overlay/userland/xnuhost.conf`. With all three given, the panel is not asked at all.
  2. In a session on the `shm` back end, the helper's `panel` report: xnuhost listens first, waits up to 10 s for the
     helper to connect and 8 s for the report, and only then makes the frame. The helper's `--takeover` asks for the
     panel's native mode first, so this is that mode, or the active one if surfaceflinger refused. The S24 Ultra in
     QHD+: 1440x3120 at 120 Hz.
  3. The largest mode of the connected built-in panel's connector in `/dev/dri/card0` (LVDS, eDP, DSI or DPI only).
     The Virtual (writeback) connectors report "connected" with modes up to 5120x2560, and DSI-1's list need not hold
     the mode One UI runs, so this is a fallback.
  4. `panel-fallback=WxH[@HZ]` where given, else 1440x3120 at 120 Hz (the S24 Ultra's), also what a bench run (whose host
     display is a monitor) assumes, without waiting. On a phone with another panel (the S25's and A56's 1080x2340) the
     S24 default made a frame of 1.78 times the panel's pixels, scaled down at every flush for the whole session; the
     device's own `panel-fallback=` makes it the panel's size. A rate-less report takes the fallback's rate.

  The log says which: `display: the mode is the configured one ...`, `display: the display helper reports the panel at
  1440x3120, 120 Hz`, `display: the built-in panel's largest DRM mode is ...`, or `... were not read (...): 1440x3120 at
  120 Hz assumed` (with `(panel-fallback=)` or `(the S24 Ultra's; panel-fallback= sets another phone's)` after it), then `display: shm, guest frame 1440x3120 at 120 Hz, @3x (480x1040 points)`. The stride of the frame
  is the width times 4 (5760 bytes; 8 with `display-format=rgba16f`); the `stride` in the helper's `first frame posted` line is the layer buffer's, in
  pixels, and is whatever gralloc aligned the width to. Tested on the PC against a stand-in helper
  (the C `tests/test_display_mode.c`, now `src/display/tests.rs`); not yet run on the phone.
- **The UI scale** is `scale`: 3 (default, 480x1040 points) or 2 (720x1560 points), from `--scale`/`--set
  scale=`, else `xnuhost.conf` of the image set, else its `overlay/userland/xnuhost.conf` (the userland hook's),
  else 3. The pixels are the same either way. One value goes to every place that states it:

  | Place | What reads it |
  |---|---|
  | `/chosen display-scale` (and `display-rotation` 0) | MobileGestalt's `main-screen-scale`; the display driver checks it |
  | `/product artwork-scale-factor` | the artwork scale (the identity tree's 3 is replaced) |
  | `/chosen/xnuhost display-width`, `display-height`, `display-scale`, `display-hz` | the mode as xnuhost has it, for the hosted kernel |
  | boot arguments `offline-legacy-unit-scale=N`, `sheng-vsync=HZ` | the display driver: its scale policy; the vsync tick and the refresh properties. Added unless the command line has them |
  | `/chosen text-fixups` | for 3 only, and only for builds 24A446 and 24B156 (`config::SCALE_FIXUPS_3X`, the same function at each build's offset): two fixups of QuartzCore's `-[CADisplayMode preferredScale]`, which answers 2 on its own (`kernel/patches/oneios-hosted/display/README.md` "Scale"). `--set scale-fixup=0` leaves them out; `--text-fixup FILE,OFFSET,ORIGINAL,REPLACEMENT` (repeatable) adds others |

  `/product artwork-device-subtype` stays the identity's (2868). The Data seed's `identity.expect` holds the
  scale the image was built with; a `--set scale=` that differs from the image's makes `oneios-mg` report it.
- **`drm`.** Direct KMS: DRM master on `/dev/dri/card0`, the connected panel's largest mode (then the fastest, then
  the preferred one; up to 256 modes read, since the kernel copies none of a list that does not fit), two dumb
  buffers, a legacy `SETCRTC`, then each flush copies its damage (and the frame before's) into the back buffer and
  `PAGE_FLIP`s it with `DRM_MODE_PAGE_FLIP_EVENT`. The next flush first waits for that flip's event (at most 34 ms;
  a later one is counted as late). An EBUSY is waited for and tried again. Only a flip the driver refuses for good
  steps down, once and logged: to a `SETCRTC` a frame (a whole commit, which a command-mode panel needs [I]), then to
  one buffer with `DIRTYFB`. `display: 10 s: KMS N frames by page flip|mode set|DIRTYFB, M flips later than 34 ms`
  every 10 s while frames come. For a session in which surfaceflinger and the composer are stopped
  (`session-mode=takeover`, `display=drm`). Until 2026-11-27 the first EBUSY (two flushes inside one vblank) turned
  flipping off for good and left only `DIRTYFB` into the scanned buffer; and `config::Config::finish_with` dropped
  `display=` (and `display-present=`, and `touch-exclusive=0` in takeover) read from the image's `xnuhost.conf` /
  `overlay/userland/xnuhost.conf`, so a takeover boot with `display=drm` in `xnuhost.conf` stayed on `shm` while the
  switch skipped `oneios-display` (black panel; `display: no helper connected within 10 s`). `display::init` also now
  passes the KMS mode's refresh rate (`vrefresh`, else `clock / (htotal * vtotal)`) instead of 0. Both fixes are
  host-tested (`cargo test --lib config::`, `cargo test --lib display::`), not yet booted.
  What is still open (atomic commits, panel power, a boot picture, the real vsync):
  [plan-ios-first-session.md](plan-ios-first-session.md) section 2.1.
- **`none`.** Frame in memory only.
- **Input** is read from evdev directly and not grabbed, so the switch's Volume Up + Volume Down watcher
  keeps working beside it. The guest's side is `kernel/patches/oneios-hosted/hid` (`docs/hardware.md` 7b).
  - **Scan time.** The touch device's events are read on `CLOCK_MONOTONIC` (`EVIOCSCLOCKID`, which is a property of
    xnuhost's open file only). Before each touch frame's `SYN_REPORT` xnuhost sends `EV_MSC` / `MSC_TIMESTAMP`: the low
    32 bits of the frame's kernel time, as ticks of `CNTVCT_EL0` (the guest's `mach_absolute_time`). The guest stamps
    the frame's HID report with it. `--input-script` sends the time each frame is sent.
  - **`SYN_DROPPED`** (the kernel's buffer for xnuhost overflowed): the events up to the next `SYN_REPORT` are dropped,
    the device's state is read again (`EVIOCGMTSLOTS`, `EVIOCGKEY`, the current slot), and the difference from what the
    guest was sent goes to it: first a frame of the lifts, then a frame with the contacts that are down. Before this a
    lift lost in the drop left a finger down in iOS. Log line: `input: touch: events dropped by the kernel ...`.
  - **Timing log**, one line when the last finger lifts: `input: touch: N frames in T ms, C contacts, a frame every
    a..b ms; read x/y/z us after the scan (min/avg/max)`, then `; K frames waited for the guest's buffers` when the guest
    had posted no buffers at a frame's end, and, for a touch-down after 100 ms without a guest frame, `; the guest's next
    frame F ms after the touch-down` (the end is `input::frame_shown`, called by `display::flush`).
  - The filter (`feed`, `src/input.rs`) is portable and has its own host test (`test_input_filter` in the C, `filter` in `src/input/tests.rs`): the events that pass, the
    scan time, the resync of slots and keys after `SYN_DROPPED`, the still-screen timing.
- **The touch IC's power state** is the brightness bridge's (`runtime/bridges/brightness/README.md`, "The touch IC
  follows the display"): Samsung's IC is off while the panel is blank and One UI's input HAL, which wakes it, is
  not there in a session. The bridge writes `/sys/class/sec/tsp*/enabled` (`sec_touchscreen` only): `2,0`, `2,1`
  at its start, at every iOS display on (after the backlight), at every press of the power key (`input.rs` calls
  the hook `input::set_power_key` gives it, which `src/session/bridges.rs` points at the bridge's `Bright::key`), and at the stop; `1,0`, `1,1` before the
  backlight when iOS turns its display off (its framebuffer's power request, `OB_BRIGHT_POWER`, hosted `0069`). A
  brightness of 0 no longer sleeps it (since 2026-11-14). `touch-sleep=0` (`--set touch-sleep=0`, or the
  image's `xnuhost.conf`) keeps the IC awake at a display off (it sets `ONEIOS_TSP_OFF=0` for the bridge). After a SIGKILL of xnuhost
  the IC is woken by One UI's own input HAL when the return brings the framework back.
- **`--input-script FILE`** (`input-script=FILE`; `src/input_script.rs`, whose head has the format): touches and
  key presses from a text file, on the same virtio-input device, for a host with no touchscreen (the PC under
  qemu) and for repeatable checks. `abs` lines give the device its axes before the guest starts (a device
  without `ABS_MT_POSITION_X/Y` is not a touch screen to the guest), `ready` waits for the guest's driver
  (the virtio transport's `ready`: DRIVER_OK), then `tap`, `swipe`, `key`, `ev`, `wait`, `repeat`.
  `tools/input-demo.script` is a 1440 x 3120 panel with a tap, a swipe from the bottom edge and the three side
  keys, repeated. It runs beside the evdev reader, or alone with `--no-input`. Its events go through the panel's filter
  (`input.rs` `feed`): each frame's end gets `EV_MSC` / `MSC_TIMESTAMP` with the time it is sent, and each scripted
  gesture gets the `input: touch:` line with its touch-to-frame time, so the latency can be measured without a finger.
  The filter passes the three side keys, and Home (`KEY_HOMEPAGE`) and the ringer (`KEY_MUTE`) for a host that has them.
- **`session/ready` (the ready rule, since 2026-11-15)** is created at the guest's first frame of its own **that
  shows something**, or `ready-content-wait` seconds (default 120, `--set ready-content-wait=N`, 0 to 480, or the
  image's `xnuhost.conf`) after its first frame of its own when every frame was black, or by the `ready` host call.
  The guest's display driver also presents the boot framebuffer (the console, and later a blank or CPU-copied frame
  drawn there): a flush of a resource whose backing starts at `boot_args.Video`'s address is shown but never counts,
  lit or not (`virtio_gpu.rs` `frame_shown`). "Shows something" is `xh_core::pixels::lit` (`runtime/core/src/pixels.rs`; `xh_pixels_lit` of `include/xnuhost/pixels.h` in the C): a grid
  of every 16th pixel of every 16th row (90 x 195 = 17,550 pixels at 1440x3120, not the 18 MB frame) with one of R, G,
  B above 16 of 255 (half floats through the clamp to 8-bit sRGB); a lit 16x16 area is always seen. A frame the display
  helper copies itself (the direct path) is sampled on the same grid where it lies, in the guest's pages (`blob_lit`,
  2026-11-22): before that the check read the display's own buffer, which a direct frame never reaches, and every
  session with direct frames waited the whole `ready-content-wait` (section 2.9b). The check runs on
  each frame of the guest's own until ready, on the device's thread, before the frame goes to the display helper (so
  `session/ready` is there when the helper sees the frame); the timeout is checked by the main loop's 100 ms tick, so
  an iOS that draws one black frame and nothing more is still ready. `ready-content-wait=0` is the rule before
  2026-11-15 (ready at the first frame of its own).
  **Why:** the first frames of the guest's own are backboardd's and black. On the phone (main 3233fed, 2026-11-15)
  `guest ready: first frame` came at +133 s with such a frame, the OneOS boot screen ended, and the panel stayed
  black, as if off, until iOS drew its Apple logo. Together with the display helper's hold (its first post waits for
  `session/ready`: `runtime/display/README.md`, "The OneOS boot screen and this layer") the boot screen now stays
  until iOS shows its logo.
  Log lines: `display: the guest's first frame of its own: all black; session/ready waits for one with content (up
  to N s)` (the time the old rule was ready), then one of `guest ready: first frame (it has content)`, `guest ready:
  first frame with content, T s and N frames after the guest's first (black) frame`, `guest ready: first frame, all
  black: no frame with content in N s after the guest's first (F frames); ready all the same (ready-content-wait)`,
  `guest ready: first frame (ready-content-wait=0: content not waited for)`, `guest ready: first frame (the kernel's
  ready call)`. All begin with `guest ready: first frame`, the marker the fork's boot stages, its Session logs
  screen and `oneios-tool` look for. Host tests (C names; in Rust `lit_frames` in `runtime/core/src/pixels.rs`, `ready_rule` in `src/ready.rs`, `two_d` in `src/virtio_gpu/tests.rs`): `test_pixels_lit` (an all-black frame with and without the X byte,
  a console's rows of text, one lit pixel on the grid, one off it, the level at and above 16, a 16x16 block at
  every place, half floats), `test_ready_rule` (lit at once, black then lit, the timeout by a frame and by the tick,
  0, the host call, once only, the key's range) and `test_virtio_gpu` (the console lit and not counted, the guest's
  black frame asked before its flush and asked again, a lit pixel, no question once ready).
  The main loop's copy of the boot framebuffer ends at the guest's first frame of its own, as before (it used to end
  at ready, which was that frame).
  **PC boot (2026-11-15, `--display shm`, qemu-aarch64, kernel.kc bf31eb0d, the PC image set, a seeded Data store,
  `display-capture.py` (now `oneios-tool kernel display-capture`) beside it):** its first picture, at +0.5 s, is the empty memfd, black: what
  the display helper posted before this fix, a second after it connected, over the boot screen. Then 2742 flushes
  of the black boot framebuffer (not counted). backboardd opened the framebuffer at +534.7 s, swap 1 at +576.2 s,
  `guest ready: first frame (it has content)` at +576.3 s, and the capture's next picture (577.1 s after its hello,
  26 colours) is the Apple logo on black. Under qemu the guest's first frame is already the logo, so the old
  rule and the new one were ready at the same frame there; the black frames before the logo are the phone's (its
  frames come through the GPU's blobs). The timeout path has run in the host tests only.

- **The pen** (`src/pen.rs`, spen 2026-11-15; `docs/hardware.md` 7f). Settings: **`pen=off`** (the built-in default; the S24 Ultra's host-table row gives
  `pen=auto` since 2026-11-28, under the command line and the image set: `docs/hardware.md` 7h) or `pen=auto`; `pen-rotate=auto|0|90|180|270` (the digitizer's frame turned clockwise onto the
  panel's; auto: 0 when the digitizer and the panel are both portrait, else 90, a guess for such a device; the S24 Ultra's IC is portrait, so 0: Samsung's driver reports the IC's frame unturned, `docs/hardware.md` 7f); `pen-mirror=1` (then mirrored
  left to right). These three settings come from the command line, else from the image set's `xnuhost.conf` or its
  `overlay/userland/xnuhost.conf`; until 2026-11-22 the image set's were read and then dropped (the C `xh_config_finish` did
  not copy them), so the phone's `pen=auto` never turned the pen on and no session had a `pen:` line. With `pen=auto`,
  xnuhost (with `input` on) calls `pen::lookup`: the default, `pen::lookup_evdev`,
  opens each `/dev/input/event0..63` read only and takes the best device with `BTN_TOOL_PEN`, `ABS_X/Y` and
  `ABS_PRESSURE` (more for `ABS_TILT_X`, `BTN_STYLUS`, `ABS_DISTANCE` and a name with "pen" in it, such as Samsung's
  `sec_e-pen`); never one with a side key or multitouch positions; never grabbed. Another hardware backend replaces the
  lookup with `pen::set_lookup` and its own function (a `LookupFn`; nothing else names a Samsung device). When a pen
  is found, or the input script has `pen-` lines, the board gets a **second virtio-input device, `/arm-io/input-1`**,
  named "OneiOS pen" (`BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`, `BTN_STYLUS`; `ABS_X/Y` 0..32767, `ABS_PRESSURE`
  0..4095, `ABS_DISTANCE` 0..255, `ABS_TILT_X/Y` -90..90 degrees; `INPUT_PROP_DIRECT`), and `oneios-pen=1` is added to
  the guest's command line unless it has an `oneios-pen=` (so `--boot-args "... oneios-pen=0"` keeps the guest's side
  off). The pen's descriptor is read on `input.rs`' thread beside the others; its filter (`Source::feed`) turns each evdev
  frame into one sample in those units (in range, tip, side button, eraser end; pressure only while the tip is down; a
  hover height never 0, half way for a device without `ABS_DISTANCE`; tilt by its resolution in units per radian, else
  as degrees within +-90, else its range's end as 90; at most +-89), sends only the fields that changed and nothing for
  a frame like the last, and reads the state again after `SYN_DROPPED`. **Coalesced to the panel's rate** (spen-lag,
  2026-11-22: the S Pen sent about 480 frames a second, each a HID report and a stylus event, and backboardd took 100 to
  160 % of a CPU while drawing): an edge (in range or out, the tip down or up, the eraser end, the side button) goes at
  once with the newest position; any other frame goes when 8.33 ms (`XH_PEN_PERIOD_NS`, 120 Hz) have passed since the
  last send, by the frames' own kernel times (the read's time when a frame's is not within the last second); until then the newest is held and `input.rs`' poll wakes for it when its
  time comes (`Source::due`, `Source::tick`); the pressure sent is the most of the frames folded together while the tip
  was down; a hovering frame within one guest pixel of the last one sent (`Source::set_panel`, the guest's frame) is not
  sent. No `MSC_TIMESTAMP` on the pen's device (the
  guest's scan-time code is the touch screen's). Log lines: `pen: /dev/input/eventN "NAME": read only, not grabbed; x
  A..B, y ..., pressure ..., distance ..., tilt x ... (res R); keys: ...; turned D degrees (why)`, or `pen: none found:
  why`; `pen: the guest's pen device is /arm-io/input-1 (the host's pen); oneios-pen=1`; `pen: the first event from
  NAME`; per time in range `pen: in range T ms: F frames (H hovering, C touching), S strokes, pressure up to P/4095,
  height a..b/255 hovering, tilt x a..b y c..d degrees, the side button pressed B times; U unchanged frames not sent, C coalesced (at most one
  frame per 8.3 ms but the edges), K hovering frames moved under a pixel; N sent`;
  `; SW_PEN_INSERT V (Samsung: ...)` at the end of the device line and `pen: SW_PEN_INSERT V: the pen taken out of / put
  back in its garage` at each change (Samsung's driver: 1 with the pen out); once, `pen: key N from the pen's IC, not a
  pen key: its screen-off (AOP) mode ...` (the IC asleep; such keys are never passed on). With the host's pen xnuhost
  sets `ONEIOS_PEN_IC=1` before the brightness bridge opens: the bridge then wakes and sleeps the pen's IC
  (`/sys/class/sec/sec_epen/enabled`) with the touch IC (`runtime/bridges/brightness/README.md`). The script's lines
  (`pen-abs`, `pen-ev`, `pen-syn`, `pen-hover`, `pen-stroke`, `pen-button`, `pen-out`: the head of `input_script.rs`) go
  through the same filter; `tools/input-demo.script` has a hover, two strokes (a pressure ramp; a tilted even one) and
  the side button. Host tests: `src/pen/tests.rs` (`test_pen` of `tests/test_xnuhost.c` in the C).
- **Sensors** (`src/motion.rs`, `runtime/bridges/sensors`, fix/sensors-als 2026-11-25; `docs/hardware.md` 7i). Settings:
  `off` (no socket opened, as before), **`sensors=auto`** (the default since 2026-11-25, the owner: every new fix on; with no hub it is the old path) or `ssc`; `sensors-hz=5..200` (50: the
  accelerometer's and gyroscope's rate; the magnetometer's at most 25); `sensors-light=auto|0..7` (which of the hub's
  light sensors). From the command line, else the image set's `xnuhost.conf` (`overlay/userland/xnuhost.conf` on the
  phone). With the bridges on and sensors not off, after the pen and before the GPU, the board opens the phone's sensor
  hub (`sensors::Hub::open`: QRTR's name server for QMI service 400, then each sensor's SUID and attributes, at most
  1.5 s per answer; nothing streams yet). With an accelerometer, and a kernel collection that has no
  `com.apple.driver.AppleSPU` member (`motion_allowed`, `session/board.rs`: with it in the collection v0.1.29 panicked
  at kernel start, "class AppleSPUHIDDevice is a duplicate"; `prelink roots` leaves it out since 2026-11-25, and v0.1.30
  gave no motion device at all; the line `sensors: motion: the kernel's AppleSPUHIDDevice (...)` or `sensors: motion:
  not given to the guest: KERNEL has com.apple.driver.AppleSPU, ...` says which), the board gets a **virtio-input device after the touch
  screen and the pen, `/arm-io/input-N`**, "OneiOS motion sensors" (`INPUT_PROP_ACCELEROMETER`, no keys; `ABS_X/Y/Z` the
  gravity vector, 256 per g, +-16 g; `ABS_RX/RY/RZ` 16 per degree per second, +-2000; `ABS_HAT0X/HAT0Y/HAT1X` 16 per
  microtesla with a magnetometer), and `oneios-motion=7` (15 with a magnetometer) is added to the guest's command line
  unless it has an `oneios-motion=`; with a light sensor `oneios-als-spu=1` likewise. The bridges then start the hub's
  thread: one frame per accelerometer sample to that device (none while more than 60 events wait for the guest's
  buffers: counted as dropped), each light reading to the brightness bridge (`Bright::als_feed`; the evdev `als_rear` is
  then not opened); the streams stop while iOS's display is off (the bridge device's `OB_BRIGHT_POWER`) and at the stop
  (`Bridges::close`, first). No hub (no QRTR, no service 400, no answer): `sensors=auto` boots as with `off` (one line);
  `sensors=ssc` too, but with no light sensor at all (the rear one gives 0 lux on the S24). Log lines: `sensors: on: ssc
  (sensors=auto): service 400 version V at node N port P; accelerometer SUID "name" (vendor), streaming ...; motion device
  /arm-io/input-N at 50 Hz, the light for the brightness bridge, on change; oneios-motion=15 oneios-als-spu=1`, or
  `sensors: off: source old (sensors=auto): WHY: no motion device, the rear light sensor as before`; `sensors: light N:
  ...: used`; `sensors: streaming: ...`; `sensors: paused: ...` / `sensors: resumed: ...`; every 60 s `sensors: 60 s:
  accelerometer N, gyroscope N, magnetometer N samples; N motion frames to the guest, N dropped (the guest had no
  buffers); light N readings, last N.NNN lux; paused N s`. The guest's side: `kernel/patches/oneios-hosted/hid/README.md`.
  Host tests: `src/motion.rs`, `config/tests.rs` (the settings, the image set's, the switches line), `session::board::tests::motion_only_without_the_spu_kext`,
  `cargo test -p onebridges sensors::`. **Not booted.**
- `session/ready` is created at the guest's first presented frame (or by the `ready` host call). The guest's
  display driver also presents the boot framebuffer (the console, and later a blank or CPU-copied frame drawn
  there): a flush of a resource whose backing starts at `boot_args.Video`'s address is shown but is not that
  first frame (`virtio_gpu.rs` `frame_shown`).

### 2.9 What xnuhost must never do in a session

The OneOS fork's way back to One UI depends on these (`docs/agents.md`, "booter -> oneos"); they hold today
and any change that would break one goes on the board first:

- no `EVIOCGRAB` on any input device, and never on the side keys; the Volume Up + Volume Down chord is the fork's.
  One exception, opt-in and off by default (the owner, 2026-11-23): `session-mode=takeover` grabs the native
  touchscreen `sec_touchscreen` alone (2.9d); a key device is never grabbed in any mode;
- no thread in a realtime class or above nice -10 (affinity and uclamp only);
- nothing leaves the mount namespace or the session's cgroup; no change to `oom_score_adj`; no signal to a
  process that is not xnuhost's own (the factory kills only its own process group);
- nothing under `/sys/power`, no `reboot(2)`, no `sys.powerctl`: a guest's "back to One UI" is `XH_HALT_RETURN`;
- nothing that needs a clean exit: every helper is a child inside the cgroup and dies with it.

Two things outlive a killed session and are not xnuhost's files: `vm.max_map_count` stays raised until
the phone reboots, and wlan0 keeps GRO off and `disable_ipv6` 1 if xnuhost is killed before `wlan::close`
(neither breaks One UI's Wi-Fi: [hardware.md](hardware.md) section 3, "Return"; `bin/wifictl restore
SESSION/wifi.state` puts them back). With `wifi=tap`, the tap's routing rules and iptables chains stay until
`oneios-net.sh down` (the tap itself goes with the process).

### 2.9d The Android takeover: `session-mode=takeover` (port-opt-in-takeover-session, 2026-11-23)

The owner (2026-11-23): YMGPwcca/OneOS's Framework-takeover experiment, ported as an option "part of the iOS hosted
session / Android takeover option we have", for the speed that the iOS-first plan
([plan-ios-first-session.md](plan-ios-first-session.md), stages 2-4) gets later. Off by default. **Not run on a phone
yet**; host-tested only (below).

| Setting | Values | Default | Where |
|---|---|---|---|
| `session-mode` | `hosted` (the session as it always was), `takeover` (the opt-in Android takeover) | `hosted` | the command line (`--set session-mode=takeover`, which the OneOS switch adds in this mode only), or the image set's `xnuhost.conf` / `overlay/userland/xnuhost.conf` (the command line wins) |
| `takeover-stops` (2026-11-23, takeover-stages) | `default` (the switch's six: `cameraserver`, `mediaextractor`, `mediametrics`, `media.tuner`, `perfmond`, `wificond`), `none`, or names of `TAKEOVER_STOPPABLE` (those six, `netd`, `media`) separated by commas, each once | `default` | the image set's `xnuhost.conf` (the switch reads the last line there, then in `overlay/userland/xnuhost.conf`); xnuhost checks the word (`config.rs` `takeover_stops_ok`: anything else is a bad setting, so `--host-info --config` refuses it too) and names it in the takeover line; the stops are the switch's ([oneos/interface.md](../oneos/interface.md) 4a) |

What xnuhost does in `takeover` (the Rust runtime, rust-runtime 2026-11-23: `runtime/xnuhost/src/config.rs`
`Config::finish_with`, `input.rs`, `session.rs` and `session/board.rs`; on `main`'s C runtime `config.c`
`xh_config_finish`, `input.c`, `main.c`, the same rules and lines):

- **Refused at the start (exit 2) unless** `mode=takeover` (the OneOS session), `input=1`, and the host phone's row of
  the host table has `takeover: true` (`runtime/bridges/hw/host_device.rs`, the C's `host_device.c` `.takeover = true`:
  the S24 Ultra and the S26 Ultra; no other row, no unknown phone, not `host-device=none`): `xnuhost: session-mode=takeover
  is not supported on this phone (NAME): the host table (runtime/bridges/hw/host_device.rs) does not allow it; ...` (the
  C's line names `host_device.c`, the file it had). A forced `host-device=NAME` is that row.
- **The touchscreen grab:** `touch-exclusive` turns on (unless `touch-exclusive=` was given): `EVIOCGRAB` of the native
  `sec_touchscreen` only (`input::exclusive_touch_allowed`; C `xh_input_exclusive_touch_allowed`), so nothing else on
  the phone reads it while iOS has it. The side keys are read as in every session and never grabbed (`input::start`'s
  `read_keys`, from `Config::reads_side_keys`; C `xh_input_start`'s; only the bench preview, `mode=bench` with
  `bridges=0`, leaves them out). The grab ends with `input::stop` (C `xh_input_stop`), any exit, a SIGKILL, and the
  `exec` of a restart in place (the fd is `O_CLOEXEC`; the kernel ends a grab with its file; the new image grabs
  again). In the Rust, `input::stop` releases the grab before it waits for the reader thread (a reader blocked in a
  guest device's lock, as in 2.9c, cannot keep the touchscreen from One UI until the stop path's 60 s limit), exactly
  once (whichever of the stop and the reader's end comes first; an atomic holds the descriptor); the stop path
  (`session.rs` `end_session`, run once, from the main thread or the stop watch) calls it, and so do the two failed
  starts after the input began (the boot's, the first vCPU's), which the C left to the exit. A grab that the kernel
  refuses ends the start (`input: touch-exclusive: EVIOCGRAB ... refused`), which is a return like any exit.
- Nothing else of xnuhost changes: the bridges, the CPU placement, the RAM, the display are the session's. The mode's
  other parts are the fork's (its stops of Android's services, the CPU cap from the cell at 50 C, the chord and the
  in-place return, the restart only where that cannot work: [oneos/interface.md](../oneos/interface.md) 4a); xnuhost
  has nothing of them to put back. The RAM the stops free is in `MemAvailable` when xnuhost starts (the switch waits
  for each to end first), so `ram=auto` (2.2) gives the guest its share of it with no setting of its own; the
  headroom (`ram-session-reserve`) stays the measured one (below, "The RAM").

`xnuhost --host-info [--host-device ...] [--host-props FILE] [--config FILE]` prints one line and exits 0 (2 for a
`host-device=` that names no row), without an image: `host-info: device=galaxy-s24-ultra takeover=yes (matched by
ro.product.model=SM-S928B)`, or `host-info: device=none takeover=no (unknown phone; read: ...)` (`Config::host_info_line`;
C `xh_config_host_info`). The fork's switch asks it before it stops anything; a runtime from before this has no such
option (exit 2, `unknown or incomplete argument`), and the switch then refuses the takeover.

Log lines (only in this mode; a hosted session's log has none of them): after the `host:` line `session: Android
takeover (session-mode=takeover; galaxy-s24-ultra allows it): the native touchscreen is grabbed for iOS; the side keys
are read, never grabbed; Android's services stopped by the switch: takeover-stops=default; the switch's CPU cap, its
stops and the way back are the OneOS fork's` (`takeover-stops=` as given, else `default`); `hw: touch: ... exclusive touch requested (session-mode=takeover: sec_touchscreen grab,
the side keys read and never grabbed)`; `input: touch-exclusive: sec_touchscreen grabbed (session-mode=takeover; the side
keys are read, never grabbed); Android input hidden until release`; at the stop `input: touch-exclusive: sec_touchscreen
released to Android`. (The bench preview's grab line says `grabbed for the bench preview`.)

**`xnuhost --prop-wait SECONDS NAME=VALUE...`** (takeover-stages, 2026-11-23; `runtime/xnuhost/src/propwait.rs`, run
by `session::main` before anything of a session, in both modes). The OneOS stop loop's wait (stage 2a of the plan): it
keeps zygote, zygote_secondary, the boot animation and the takeover's stops stopped, and until now ran three `getprop`
every 0.2 s for the whole session, 15 processes a second. Now it runs this with `init.svc.NAME=running` for each of its
names and 60 s: bionic's property area, read as `getprop` reads it (`__system_property_find`,
`__system_property_read_callback`), and a futex wait on the area's serial (`__system_property_area_serial`,
`__system_property_wait` with no prop_info), which init raises with every property it sets. So there is no process
and no wakeup while no property changes; a change of any property wakes it for one look at its names, and a property
set between the look and the wait ends the wait at once (the serial is read before the look). Exit 0 with each name
that holds on stdout, 1 when the time passed, 2 for a bad command line (SECONDS 0-3600, 1-32 pairs, a name of
letters, digits and `._-@:`, a value of at most 91 bytes) or a build without the property area (the host's). It sets
nothing and signals nothing, and ends with the shell that started it (`PR_SET_PDEATHSIG` SIGKILL; it exits 1 at once if
that shell is already gone). An xnuhost from before it exits 2 (`bad value for --prop-wait`): the loop then looks
once a second.

**The RAM in `takeover`** (stage 4 of the plan, judged 2026-11-23). The switch's stops come before xnuhost and it waits
for each to end, so what they held is in `MemAvailable` when `ram=auto` reads it (2.2): each MiB adds about 0.93 MiB
to the guest, through the same formula as `hosted` (no fork of it; the ram helpers keep changing its task terms on
`main`). The headroom `ram-session-reserve` (1536 MiB with the lock) is not lowered for the mode: it is the host's
swap-in floor measured on 2026-11-23 (31-42 MB/s of swap-in at 1077-1265 MiB available, under 0.2 MB/s from 1503 MiB
up), and that working set is surfaceflinger, the composer, the display helper, xnuhost's code and the image's page
cache, none of which the takeover's stops end. The stops are idle daemons (no working set to make room for). The
headroom can go down with stage 3 (surfaceflinger, the composer and the helper gone), from a takeover session's own
`memory:` lines. The proof lines: the switch's `Android takeover: stopped ...; host MemAvailable A -> B MiB`, then
xnuhost's `memory: ram=auto: host MemTotal ... MemAvailable B' MiB; the guest gets N MiB`.

**More RAM in the takeover (takeover-ram, 2026-11-23).** The owner (2026-11-23 21:30): "We need at least 10 GB of RAM
for iOS. Find ways to do it", in `session-mode=takeover` only. What the logs say (`out/session-logs/2026-11-23-mac-takeover/`,
the session of 21:11, 4608 MiB locked, before the trim):

- RAM chips 12 GiB; Linux MemTotal 11,084 MiB (the rest is the firmware's carve-outs). The switch: `Android takeover:
  stopped cameraserver mediaextractor mediametrics wificond ...; host MemAvailable 6933 -> 8441 MiB` (system_server's
  late end may be in that too [guess]). xnuhost's start: MemAvailable 8,458 MiB, so 2,626 MiB of MemTotal is the kernel, the kept
  services, zram's pool (547 MiB of Android's pages swapped) and caches MemAvailable does not count: nothing named them
  (the start line of 2.2, "The host's breakdown", names them from the next boot).
- 6 s after the start, the guest held and locked: MemAvailable 3,412 MiB, 438 MiB beyond the guest's 4,608 (9.5 %: the
  hold, the display helper's start, the boot's first host buffers). The lowest: 2,172 MiB at 126 s, page tables 662
  MiB (57 at the start); then 2.3-2.5 GiB with page tables 337-478 MiB. KGSL's own memory 2.1 MiB (`kernel 2166784`,
  `kernel_max 2363392`); `imported_mem` 2.2-2.4 GB is the GPU's blobs, inside the guest's RAM.
- With the trim (07b10a8) the same start gives `ram=auto` (8,458 - 1,536 - 256 - 850) / 1.07 = 5,435 -> **5,376 MiB**.

**10 GB locked does not fit on this phone.** 10 GB (9,537 MiB) locked needs, with today's terms, 9,537 x 1.07 + 850
(tasks) + 256 (GPU) + 1,536 (headroom) = 12,847 MiB of MemAvailable at the start, and even with every term cut hard
(headroom 512, tasks 400, GPU 64, hold 3 %) 10,799 MiB: MemTotal is 11,084, and the kernel's own share (its page
array for 12 GiB alone is about 190 MiB, the slab, stacks, zram's pool) never leaves it. 10 GiB (10,240 MiB) is more
than MemTotal less any kernel at all. What is possible:

| Way | Switch (default) | What it gives | State |
|---|---|---|---|
| A1. The caches, the page pools, zram's free space: `sync`, `drop_caches` 3 (it runs every shrinker: the dma-buf heaps' and KGSL's pools, the slab, zsmalloc's compaction) and each zram device's `compact`, after the stops, before xnuhost | `takeover-reclaim=1` (on; `0` off) | what MemAvailable did not count before; its line measures it: `Android takeover: caches dropped, zram compacted on N device(s) ...; host MemAvailable A -> B MiB, MemFree .., zram's RAM .., dma-buf heap pools ..`. [U] until a boot, 50-300 MiB [guess] | built; nothing to put back |
| A2. More stops: the kept services that no bridge uses | `takeover-stops=` (names to add to `TAKEOVER_STOPPABLE` after a boot) | the start line's process list names them with their anonymous memory and swap | the list is the next step: the start line (or `phone procs --sort mem`) first |
| A3. Stage 3: surfaceflinger, the composer and the display helper stopped (`display=drm`) | (not built) | their anonymous memory (the start line names it; 150-300 MiB [guess]) and a lower headroom (their working set is in it) | plan only: [plan-ios-first-session.md](plan-ios-first-session.md) 5b, 3b-3d |
| A4. zram's pool | none | the pool shrinks only as the kept services' swapped pages are freed (the stops) and by A1's compaction; a pool cannot be made smaller with pages in it (a `disksize` change needs a reset, which drops them). Writing its pages out needs a `backing_dev` (the start line prints it) | nothing more to build |
| A5. logd's buffers, CMA, Samsung's RBIN | none | logd: in the process list; CMA pages take the memfd's movable pages, so no loss; RBIN: the start line's `Rbin*` keys say how much there is; no switch of this tree gives it back | measured only |
| B1. The headroom (1,536 MiB, the host's swap-in floor of 2026-11-23) | `takeover-swappiness=N` (`keep`) | with a low swappiness the host drops page cache before it swaps the kept services, which is what the floor was; then the headroom can go down (Integrate's `ram-session-reserve` for the takeover, from a takeover boot's pressure and swap-in lines): 256-512 MiB [guess] | built, off |
| B2. The tasks (850 MiB with the trim) | `task-trim` | the trim's `tasks: trim:` line measures awake and trimmed tasks | on main's way (fix/task-trim) |
| B3. Holding the RAM (7 %, 376 MiB at 5,376) | - | the hold line (`memory: holding the guest's RAM: ... by user`) names it | measured from the next boot |
| B4. The GPU's 256 MiB | `ram-gpu-reserve=` | KGSL's own is 2.1 MiB; the blobs' udmabuf tables about 1 % of 2.2 GB [estimate]: 128 MiB would do [estimate] | not changed (the task cost was derived with the 256 in it) |
| C. A guest larger than what fits, the top backed on demand | `ram-ondemand=1` (**off**) with `ram=SIZE` | iOS is told SIZE; the host holds only what iOS uses, each page locked as it comes in; iOS fills it within minutes (its file cache), then the valve (the host swaps iOS) or the OOM kill of xnuhost (the return) | built, off: the owner decides (2.2, "On demand") |
| Inside the rule: iOS told more than its RAM | `ram-view=10G` or `auto` (**off**) | iOS's own compressor holds up to twice the RAM's pages, never the host's swap (2.2, "The memory view") | built 2026-11-15, not yet proven on a phone |

**The largest guest (D), with the fixed, locked RAM kept** (each line `(MemAvailable - headroom - GPU - tasks) / 1.07`,
in 256 MiB steps; [estimate] until the next boot's lines measure the gains): today with the trim 5,376 MiB; with A1
and A2 at +300 MiB, B4 at 128 and B1 at 1,280: (8,758 - 1,280 - 128 - 850) / 1.07 = 6,074 -> 5,888 MiB; with stage 3
too (+250 MiB, headroom 1,024): (9,008 - 1,024 - 128 - 850) / 1.07 = 6,547 -> **6,400 MiB**, about 6.3 GiB. The bound
with nothing of Android left but the kernel (1.0-1.5 GiB [guess]): about 8 GiB. With C the guest can be 9.5 or 10 GiB,
but only what fits (the figures above) is safe: past it iOS is swapped or the session ends. With `ram-view=10G` over a
locked 5.4-6.4 GiB, iOS's apps get a 10 GiB device's limits and its compressor's room (at 2-3:1 [guess] 9-12 GiB of
pages) without the host swapping anything: the way to "10 GB for iOS" that keeps the rule.

The three keys (`takeover-reclaim`, `takeover-swappiness`, `ram-ondemand`) are rows of [switches.md](switches.md) and words of
the `switches:` line. The proof lines for the next takeover boot (the Mac): the switch's `Android takeover: caches dropped ...` line (A1's
MiB); xnuhost's `memory: the host at the start ...` (A2, A5: the processes and the kernel's parts),
`memory: holding the guest's RAM: ...` (B3), `tasks: trim: ...` (B2), and at the stop `memory: the host at its lowest
MemAvailable ...` (the peak by user). A boot with `takeover-swappiness=20` in `overlay/userland/xnuhost.conf` then
gives B1: `Android takeover: vm.swappiness 100 -> 20 ...`, the pressure and `thermal:` swap-in figures at the lowest
MemAvailable, and the return's `vm.swappiness back: 100`.
Host tests: `config::tests::session_mode` (the two words, the image set's and the command line's, the takeover line naming
them, words refused), the fork's `OneiosScriptTest.theTakeoversMemoryStepsAndTheReturnPutsSwappinessBack` (the default
steps on a fake phone's nodes and their line, `takeover-reclaim=0`, `takeover-swappiness=20` saved, written and put back
by the return, words that change nothing) and its order check (the steps after the stops, before the stop loop; the
return's put-back on both of its ways). **Not run on a phone.**

Tests. The C runtime (`runtime/xnuhost/tests/test_xnuhost.c` `test_session_mode`, on `main`, on the build laptop,
2026-11-23: 14325 checks, 0 failed): the table (only the two rows), hosted equal to no setting, takeover on both phones
with the grab on, `touch-exclusive=0` kept, refused on the S25 Ultra, an unknown phone, `host-device=none`, the bench
and `--no-input`, the image set's setting and the command line over it, every `--host-info` line. The Rust runtime
(rust-runtime, the build laptop, 2026-11-23): `cargo test -p xnuhost config::tests::session_mode` carries every one of
those cases over, plus the refusals' texts, `touch-exclusive=0` after `session-mode=`, the bench preview's keys left
out, the mode's lines and a property file that cannot be read for `--host-info`; `input::tests::the_grab_lines_and_one_release`
checks both grab lines and that the release runs once. `oneios-tool`'s `host.rs` mirror checks each row's `takeover`
against `host_device.rs` (`table_matches_the_bridges`). Not run on a phone. takeover-stages (the build laptop,
2026-11-23): `config::tests::session_mode` also checks `takeover-stops=` (the words it takes and refuses, the image set's
line and the command line's, the takeover line naming it, `hosted` with it, a bad word in the image set refused), and
`propwait::tests` (6) the command line, a property that holds at once, a wait that looks again only after a change, the
time passing, a property that appears later, and the host build's exit 2. `--prop-wait` on bionic's area is **not run
on a phone yet**.

### 2.9c A host call that does not come back, and a stop the main loop does not see (fix-debugger-crosscall-panic, 2026-11-23)

**The fault.** Two phone boots of v0.1.16 (kernels `776f6c56613a` and `81b85dd4`, 2026-11-23) froze about 6 minutes
in, the same way. The guest panicked: `panic(cpu 0 caller ...): DebuggerXCallEnter>Debugger synch pending on cpu 1`
(the second boot: `cpu 1 ... on cpu 2`). A user process took a stackshot (`stack_snapshot_with_config`: spindump,
ReportCrash), the debugger's cross-call went to every CPU, one vCPU did not answer within `debug-ack` (about 5 s,
hosted 0017), and a stackshot does not go on without every CPU, so the kernel panicked. The panicking vCPU called
`halt` and waited in `pause()` (its wchan `sigsuspend`), six vCPUs spun in the debugger's wait loops (690% of 800),
and xnuhost never exited: the session stayed frozen until the user's chord.

**The cause** (from the logs, `out/session-logs/2026-11-23/boot-0.1.16/` and `boot-edge/`). The vCPU that did not
answer (wchan `futex_wait_queue`) was not in the hand-off to an iOS thread: it was inside a host call, in the
virtio-media device's handler. The guest's `VIDIOC_STREAMON` opened an encoder session; `maybe_open_encoder` called
the helper backend's open on the vCPU, with the device's lock held; `helper_open` started `oneios-encoder` (its
receive thread is the thread named after the vCPU that waits in `read()` / `unix_stream_read_generic`: the encoder's
socket is xnuhost's only `SOCK_STREAM` one), sent the OPEN and waited in `pthread_cond_wait` with no limit. The helper
never answered (MediaCodec's open did not come back in it: why is not known yet; the next boot's `encoder: the
helper ... (state ..., waiting in ...)` line says where it waits). A vCPU in a host call takes no interrupt, so that
vCPU stayed deaf for 80-90 s, until the stackshot. Meanwhile xnuhost's main loop reached its 60 s virtio report,
which took every device's lock in turn and waited for ever in the media device's: its 30 s and 60 s lines (`perf:`,
`memory:`, `tasks:`, `virtio:`) end at 24391 s in the first boot and 25871 s in the second, 78 s and 92 s before the
panic. So the main loop never saw the halt's stop either.

**The fixes.**

- The encoder's helper backend never waits for the helper without a limit (`runtime/bridges/encoder/README.md`): the
  HELLO and each write 1 s, an open's reply 2.5 s (under `debug-ack`). A helper that misses one is killed and the next
  open starts a new one (twice a session at most; then VideoToolbox keeps its software encoder). The stop kills the
  helper.
- The virtio health report (`xh_core::virtio::Registry::health`, `runtime/core/src/virtio.rs`; the C's
  `xh_virtio_health`) never waits for a device's lock (`pthread_mutex_trylock`): a device held for 2 s or longer by
  one request is said once per hold, with the vCPU in its handler, and again when it is free: `virtio media#0: held
  by a request on queue 0 for 78 s (cpu1 in its handler): the guest's next access to the device waits for it` /
  `virtio media#0: free again: the request that held it ended after N s` (a device thread's `Virtio::lock`, the C's `xh_virtio_lock`:
  `held by a device thread for N s`, or `held by cpuK` when a vCPU holds it). One clock read (the vDSO's) per queue
  notify. A held device's bad chains count as last seen (the C left them out of the sum, so that report's `bad chains
  +N` wrapped and the next said them again).
- **The stop watch** (`src/session.rs`, `watch`; the C's `main.c`): a thread (`xnuhost-stop`), started before the vCPUs,
  waits in a futex for the first `session::stop` (the C's `xh_stop`), then in a condvar. When the main loop has not
  left its loop 3 s after the stop, it says `stopping: the main loop did not take the stop (WHY) in 3000 ms: it waits
  in STEP; the stop path runs from the stop watch` (STEP: the loop's step, e.g. `the virtio report`), logs where each
  vCPU is (the SIGQUIT lines), and runs the stop path from another thread (`xnuhost-end`); only one thread runs it (`end_session`). In the Rust
  the session's state the stop path needs is behind locks the main loop holds only around its own ticks (`SESSION`,
  and `BRIDGES` apart, so the bridges are closed even while the main thread is blocked in a memory or USB report);
  when the main thread holds the session the stop path says `stopping: the main thread holds the session (STEP): its
  memory and USB lines at the stop are left out` and goes on.
  When the stop path has not ended 60 s after the stop: `stopping: the stop path did not end in 60 s (it is at:
  STEP): exit N now` and `_exit(N)`. So a guest panic always ends the session with exit 4, its log written, and the
  fork's way back runs.
- The halt host call (`src/hostcalls.rs` `halt`) says a panic: `guest panic: the kernel halted on cpu0 (code 0,
  panic()): ending the session` (codes: 0 `panic()`, 1 a start-up check, 2 a fatal kernel fault, 3 a panic while
  panicking).
- The hand-off's kick (section 2.6) was sent once and its result not looked at. A vCPU whose iOS thread is not back
  200 ms after the kick now sends it again (`src/task.rs` `thread_run`) at each futex step while its line is up; a
  refused `tgkill` (a full queue of real-time signals) is said once (`tasks: the kick of pid N tid T (vCPU K's interrupt) was refused: ...`), and the
  perf line counts the second kicks (`kicks of iOS threads sent again N (not back 200 ms after the first)`). Not seen
  in these boots; it keeps a lost kick from leaving a vCPU deaf the same way.

**Tests** (build laptop): `make -C runtime/xnuhost test` (`test_health_held`: a handler that blocks on another
thread; the report comes back, says `held by a request on queue 0 ... (cpu1 in its handler)` once, and `free again`
after; and check-arm64's test kernel, `test_xcall`: vCPU 1 hands off to an iOS thread that spins in user code and
never traps, so it waits in the hand-off's futex as the phone's vCPUs did; vCPU 0 sends it SGI 7 as the debugger's
cross-call does, 20 times, and each must be taken within 500 ms: `cross-call to a vCPU in the hand-off: taken 0x14 of
20, slowest (us) 0x...`), `make -C runtime/bridges/encoder test` (`test_hung_helper`: the stand-in's open hangs in the helper process
(`ONEIOS_ENCODER_STUB_HANG_OPEN`); the open comes back in its limit, the helper is killed, a new one works, after two
hangs no helper starts). Results on the build laptop, 2026-11-23: `xnuhost tests: 14269 checks, 0 failed`, `TESTKERNEL: all
passed` under qemu-aarch64 with `cross-call to a vCPU in the hand-off: taken 0x14 of 20, slowest (us) 0x146` (326 us),
`test_encoder: 218/218 checks passed`, `runtime/host-tests.sh` exit 0, the oneios-tool crates' tests 0 failed. The stop
watch and the second kick have no host test of their own (a host test that stops
the process would need a fork of xnuhost's main; not done). Those were the C's. **The Rust port** (branch
rust-runtime, 2026-11-23, build laptop): `cargo test -p xh-core virtio::` (`health_held`, the C's `test_health_held`
case for case, and `held by cpu1 for 0 s` for a device thread's `Virtio::lock`; `health_held_keeps_bad_chains`, the
C's wrapped `bad chains +N`), `cargo test -p xnuhost` (`session::tests::the_main_steps_texts`: the steps' texts and
the 3 s / 60 s limits), `cargo test --workspace --locked` exit 0, `cargo fmt --all --check` and `cargo clippy
--workspace --all-targets -- -D warnings` clean, and `runtime/xnuhost/check-arm64.sh smoke check` with the static Rust
xnuhost under qemu-aarch64: all three boots passed, `cross-call to a vCPU in the hand-off: taken 0x14 of 20, slowest
(us) 0x137, mean (us) 0x7a` (311 us, 122 us). The Rust stop watch and second kick have no host test of their own
either (the same reason). **Phone:** the lines to look for in the next boot that
opens an encoder session are `encoder: helper ... (pid N)`, then either `encoder: session N: HEVC ...` or `encoder:
the helper (pid N) did not answer an open in 2500 ms (...): killed (1 of 2)`, and never a `virtio media#0: held by`
line longer than 2-3 s.

### 2.9a The USB gadget in the log (`src/usb.rs`, 2026-11-13)

xnuhost never opens, writes or binds anything of USB, the USB gadget, configfs, Type-C or power supplies but
the battery and thermal bridges' reads of named files of `/sys/class/power_supply/battery` and of `online` of
`usb`, `ac`, `wireless` and `pogo` there (read only; checked in the sources, 2026-11-13), and it sets no property. Keeping
adb on the gadget through a session is the fork's (`oneos/interface.md`, "USB and adb during a session"). What
xnuhost adds is a line that says when the gadget changed, so that a session's log shows when adb's USB function,
or the controller under it, went away (phone, 2026-11-13: a cable plugged in during an iOS session, and the Mac saw
no USB device at all):

```text
usb: state <sys.usb.state>, config <sys.usb.config>, UDC <sys.usb.controller> <its state>
                                     at the start, whenever one of the four changed, and " (at the stop)"
usb: state mtp,adb, config mtp,adb, UDC a600000.dwc3 configured            (a cable to a computer, adb up)
usb: state mtp,adb, config mtp,adb, UDC a600000.dwc3 not attached          (no cable, or one that never enumerated)
```

The properties are read from Android's property area as bionic does: `__system_property_find` until the
property exists (the `prop_info` then stays where it is), its `__system_property_serial` on each poll, and
`__system_property_read_callback` for the value only when the serial changed. The UDC's state is the one named
file `/sys/class/udc/<sys.usb.controller>/state` (the kernel's `usb_state_string`: `not attached`, `powered`,
`default`, `addressed`, `configured`, `suspended`); the controller is found by that property's name, used only
when it is one path component, and `/sys/class/udc` is never listed. The main thread polls every `usb-log`
seconds (3; not a realtime thread; three serials and one small file a poll). A value that is not there reads
`(none)`, a state file that cannot be read `(unreadable)`, a controller that is not a plain name `(not a name)`.
On the PC (no property area: the host build, or the Android binary under qemu-aarch64) the line is
`usb: state (none), config (none), UDC (none) (unreadable)`, once.
`usb_line` in `src/usb.rs` (`test_xnuhost`'s `test_usb` in the C, ported case for case) checks it with a stand-in property source and state file: the first line, nothing
logged or read again while the serials and the file stay, a change of the file alone and of one property, a value
read only through its serial, a property that goes away, a controller name that is a path (`../../../proc/self`:
its file is never opened), and `usb-log`'s default and range. **Not run on the phone yet.**

### 2.9b Health lines: one session's log, part by part (2026-11-22)

**The switches line (2026-11-23).** Right after `versions:` xnuhost writes `switches: KEY=VALUE ... (docs/switches.md);
all at their defaults` (or `not at the default: KEY ...`): every switch of [switches.md](switches.md) as this session
runs it, the kernel ones as the boot-args name them. Test: `config::tests::the_switches`.

The goal: any boot problem can be traced from one session's `xnuhost.log`. Each part of the boot and the session
writes a cheap lasting line that says its state: once per event, or a count every 10-60 s that is printed only when
it moved. `oneios-tool log summary xnuhost.log` (tools/oneios-tool/README.md, "log") reads them back on one screen:
the boot's stages and their times, then each part's count of lines, its last line and its first problem;
`--since T` / `--last S` narrow it to a window. `oneios-tool log timeline` lists the same stages among the other
milestones.

**The versions line** (2026-11-23, v0.1.19; `imageset::versions_line`, logged by `session.rs`). Right after
`xnuhost: session ABI`, and again right after `stopping:` (so the last lines of a log, all a user's report often has,
carry it too), xnuhost writes which runtime, kernel, image and app made the log:

```text
versions: runtime 0.1.19 (git 973e8f7a1b2c), kernel xnu-12377.121.6-ios-27.0.1-24A446-c889b14b24f3, kernel.kc a28fedfdf524, image iOS 27.0.1 24A446 format 1, app versionCode 209
```

`runtime` and `git` (12 hex, ` dirty` when the package was) are `version` and `git.commit` of the runtime's
`runtime.json` (beside `bin/`, `--runtime`); `kernel` is its `kernel.build_id`. `kernel.kc` is the first 12 hex of
the SHA-256 that the image's `manifest.json` lists for its `kernel` file (the staged one: `phone manifest --stage`
writes it; xnuhost does not hash the 100 MB file at the start); `image` is the manifest's `os`, `version`, `build`
and `format`. `app` is `ONEIOS_APP_VERSION_CODE`, which the OneOS fork's root switch verb sets to the app's
versionCode (the fork's docs/oneios.md "Session flow", from app 0.5.80 (209)); `app unknown` without it (a bench run,
the scripts of app 0.5.78 and before). Each value that cannot be read (no file, not JSON, a value with a character
other than `[A-Za-z0-9._+,-]`) is `unknown`, `none` a runtime or image that says it has no kernel; the line never
stops the session. The two JSON files are read once, at the start (at most 4 MiB each). `oneios-tool log summary`
prints the log's last versions line as its second line, or `versions: no line`. Tests:
`imageset::tests::the_versions_line` (the line from files as `runtime/package.sh` and `image manifest` write them,
missing and bad files), `oneios-log`'s `summary_names_the_versions` (a whole log, a tail, two different lines).

**Boot stages** (summary's `stages:`; the first line that shows each): xnuhost starts (`xnuhost: session ABI`), the
RAM is held and locked (`memory: the guest's RAM is held:`), the kernel starts (`cpu0: entering the kernel`, the
guest's `Darwin Kernel Version`), BSD root (`BSD root:`), launchd is loaded (`load_init_program: attempting to load
/sbin/launchd`) and writes its first line (`com.apple.xpc.launchd|`), the first virtio driver is up (`virtio NAME:
the guest's driver is up`), SpringBoard and backboardd are first seen (the first guest line that names them: no line
of their own, see "Still missing"), backboardd opens the framebuffer (`oneios-display: framebuffer opened by
backboardd`), the first swap (`oneios-display: swap 1`), the guest's first frame (`display: the guest's first frame
of its own`), guest ready (`guest ready:`), the first gesture (`input: touch:`), and the end (`exit N`,
`stopping:`, a panic, or none: the session still ran or xnuhost was killed).

**The inventory** (from `out/session-logs/2026-11-22/kbd-debug/xnuhost.log` and `gpu-debug/xnuhost-*.log`, iOS's own
log `kbd-debug/ioslog-last.txt`): the part, the line that shows its state, how often, and whether it was there
before this change.

| Part | The line | How often | Before | Now |
| --- | --- | --- | --- | --- |
| xnuhost start, the host phone | `xnuhost: session ABI`, `host:`, `hw: NAME: backend`, `placed:` | once | present | present |
| which runtime, kernel, image and app (2026-11-23) | `versions: runtime ... app ...` | at the start and after `stopping:` | **missing** | present |
| RAM held and locked, host memory | `memory: the guest's RAM is held: ... locked`, `memory: host MemAvailable ...`, `memory: fixed RAM: ...` | once; 30 s | present | present |
| kernel start | `cpu0: entering the kernel`, guest `Darwin Kernel Version` | once | present | present |
| BSD root, launchd | guest `BSD root:`, `load_init_program:`, `com.apple.xpc.launchd|... Doing boot task` | once each | present (launchd's console lines end with its boot tasks) | present |
| launchd jobs that crash-loop or are disabled | none on the console: launchd writes them to logd | - | **missing** | still missing in xnuhost.log (`oneios-tool ioslog` reads them from the Data store after the session); the host side now counts them: `tasks: 60 s` and `tasks: pid N died on its own` |
| iOS processes as Linux processes | `tasks: pid N (tid T): trap kind ...` (the first 64 fatal traps) | per event | partial: no counts, nothing for a task process that died by itself | `tasks: 60 s: +N made, +N ended by the kernel, +N died on their own, +N fatal traps, +N refused; live N (most N)` (60 s, when moved); `tasks: pid N died on its own (wait status 0x..: signal N), not asked by the kernel` (per event, the first 32) |
| backboardd, SpringBoard | the first guest line that names them | - | indirect | indirect ("first seen") |
| the first frame, ready | `display: the guest's first frame of its own: ...`, `guest ready: ...` | once | present, **wrong with direct frames** (below) | fixed |
| display | `display: 10 s: N frames copied by the helper ... N not answered within 100 ms` | 10 s | present | present |
| one frame end to end (2026-11-24) | `frame: 10 s: N guest frames ...; ms p50/p95/max: interval ...; flush -> the next frame's first GPU submission ...; first submission -> flush ..., of it GPU ... and retired -> the guest's flush ...; the helper's answer ...; touch -> next flush ...` (`src/frame_path.rs`, docs/gpu.md 2.5a) | 10 s while the guest draws | **missing** | present |
| GPU | `gpu: 10 s ...`, guest `oneios-gpu-report: 10 s` and the other `oneios-gpu-*` lines | 10 s | present; drm2kgsl's facts were `xnuhost E` lines (below) | `xnuhost I` |
| CPU, the trap path | `perf:`, `cpuN:`, guest `oneios-cpu: 10 s`, `oneios-top:` | periodic | present | present |
| thermal | `thermal: 30 s: host CPU ...; battery ...; case ..., SoC ...` | 30 s | present | present |
| disks, the Data store | `disk N: PATH, SIZE, role ROLE` (once), `disk NAME: 30 s: ...`, `disk N: 61 s: ...`, `disk NAME: a flush took N ms` | once; 30/60 s; per event | present | present |
| the Data store's file system in iOS | Apple's own `nx_mount`, `apfs_*` console lines; `oneios-tool ioslog health` afterwards | per event | indirect | indirect |
| virtio queues (stuck or full) | none | - | **missing** | `virtio: 60 s: gpu#0 q0 +N q1 +N, input#0 q0 +N, blk#3 q0 +N (in flight N)` (60 s, when moved: chains returned in the span); `virtio NAME: queue Q: N chain(s) in flight and none returned in 60 s (...)`, `virtio NAME: queue Q: N chain(s) posted by the guest and not taken in 60 s`, `virtio NAME: queue Q returns chains again (after N s)` (once per episode); `virtio NAME: the guest's driver is up (features 0x.., N queue(s) ready)`, `... set FAILED`, `... reset by the guest's driver (it was up)` (per event) |
| input: the host's evdev | `input: /dev/input/eventN "NAME" ...` (once), `input: touch: N frames ... the guest's next frame N ms after the touch-down` (per gesture), `input: NAME: events dropped by the kernel` | per gesture | present, **wrong with direct frames** (below) | fixed |
| input: the side keys | none | - | **missing** | `input: power key down: to the guest (N event(s) waiting for its buffers)` (each press and release, no autorepeat) |
| input: the virtio-input backlog | none: the oldest event was dropped without a word | - | **missing** | `input: NAME: the guest posted no event buffers for 1024 events: the oldest are dropped until it does`, `input: NAME: the guest takes events again (N lost since the boot)` (once per episode) |
| input: the guest's HID to backboardd | guest `oneios-hid: touch screen: N virtio-input events, N frames, ... N HID reports`, `... queued for the service's client(s), N dropped`, `oneios-hid: buttons: N user client(s) ... (pid 53, backboardd)` | periodic, per event | present | present |
| Wi-Fi | `wifi: ...` (each step), guest `ONEIOS WIFI: 60 s: ...` | per event; 60 s | present | present |
| audio | `audio: on: helper ...`, the helper's `oneios-audio: speaker open/running/closed ...` | per event | present | present |
| haptics | `haptics: on: ...`; `haptics: counters: ...` at 16, 32, ... effects and at the stop | stop | partial: nothing during a session that ended by a kill | `haptics: 60 s: +N requests (+N vibrate, +N stop, +N impacts), +N played, +N failed` (60 s, when moved) |
| camera | `camera: helper ...`, `camera: the helper has N cameras`, `camera: iOS is told of ...` (the tree's camera nodes), `camera: cameracaptured is not told of the VM ...`; the helper's counters at its stop | once | start only | unchanged |
| video encoder | `encoder:` and the helper's lines when iOS uses it | per event | none in these logs (not used) | unchanged |
| Bluetooth | `bluetooth: on: ...` | once | start only | unchanged |
| battery | `battery: on: ... read every 3000 ms while iOS listens`; the host's battery in `thermal: 30 s` | once; 30 s | start only | unchanged |
| brightness | `brightness: iOS's display on/off ...`, `brightness: panel on: ...` | per event | present | present |
| the guest's memory pressure | guest `oneios-mem: free N MiB ...`, `memorystatus: sending ... SIGKILL` (jetsam) | periodic; per event | present | present (summary counts the kills) |
| USB | `usb: state ...` | on change | present | present |
| the return | `return: ...`, `stopping: ...`, `wifi: wlan0 given back`, `memory: the guest's RAM: ... gives all of it back`, `exit N`; after the exit the fork's `switch.log` | once | present | present |

**Three wrong readings, fixed with the lines (2026-11-22).** All three were in the kbd-debug session:

1. *Touch to frame with direct frames.* Since the display helper copies frames itself from guest RAM (the direct
   path, `xh_display_post`, now `display.rs`), xnuhost's own `xh_display_flush` no longer ran for iOS's frames, and only it ended
   `input.c`'s (now `input.rs`') touch-to-frame wait. So every gesture said `no guest frame after the touch-down` (66 of 66, while the
   helper showed 153 frames in 10 s). `xh_display_post` now ends the wait when a frame is posted
   (`test_display_mode`, now `a_posted_frame_ends_the_touch_wait` in `src/display/tests.rs`: a posted frame, and only it, counts as shown).
2. *Ready with direct frames.* The ready rule asked whether the frame is lit (`xh_pixels_lit`) of the display's own
   buffer, which a direct frame never reaches: it stayed black, and session/ready waited the whole
   `ready-content-wait` (first frame at +51.4 s, ready at +171.5 s, "no frame with content in 120 s"). `virtio_gpu.c`'s (now `virtio_gpu.rs`')
   `blob_lit` now samples the posted blob where it lies in the guest's pages, on the same grid and level
   (`test_xnuhost`, now `three_d` in `src/virtio_gpu/tests.rs`: a direct frame black at the sampled pixel reads black, a lit one reads lit, with the display's
   buffer black both times).
3. *drm2kgsl's facts as errors.* Our drm2kgsl patches print their once-per-event facts at virglrenderer's WARNING
   level (the library's threshold passes them), and xnuhost mapped WARNING to `xnuhost E`: all 28 `E` lines of the
   kbd-debug log were such facts, so a real error had no `E` of its own. Only ERROR (`drm_err`) is an `E` line now.

**Cost.** The virtio, tasks and haptics lines are checked once per main-loop pass and run every 60 s: one walk of
about 12 devices' queue counters (under each device's lock, a few loads each) and one copy of the task counts. The
per-event lines come at boot (one per device), at a key press, or at a fault. The counters are plain increments in
code that already holds the device's or the tasks' lock.

**Tested:** `test_xnuhost`'s `test_health`, now `tasks_health_line` in `src/task.rs` and `health`, `health_summary_and_bad_chains` in `runtime/core/src/virtio/tests.rs` (the tasks' line and its silence when nothing moved; a virtio device
that holds the chains it is given: the summary, the in-flight count, the episode told once, the line when it moves
again, a queue whose posted chains nobody took), the direct-frame cases above, the Rust tests of `log summary`
(`tests/synthetic.rs` `summary_of_a_boot`, the unit test `problems_and_zero_counts`), and `log summary` on the
2026-11-22 logs. **Not run on the phone**: the new lines have not been seen in a session yet.

**Still missing** (each needs the hosted kernel, a numbered patch under `kernel/patches/oneios-hosted`): a line per
launchd job that exits, crashes or is disabled (launchd writes those to logd, not the console), and a line when
backboardd and SpringBoard are spawned and when they exit; today the summary finds them by the first guest line
that names them, and `oneios-tool ioslog` reads the job states from the Data store after the session.

## 3. What each XNU subsystem gets from the host

| XNU needs | From the host | By |
| --- | --- | --- |
| Entry, `boot_args`, device tree, trust caches, root policies | the loader | `x0` = `boot_args`, `x1` = vector, `x2` = `xh_cpu` |
| Physical memory | the window; `ram_discard` to give pages back | `boot_args`, `xh_info` |
| Kernel address space | already mapped; kernel VA = PA + 0 inside the window | no page tables |
| pmap for tasks | `task_map/unmap/protect` | host calls |
| Exception vectors: system calls, aborts from user | `thread_run` returns an `xh_trap` + `xh_user_state` | host call |
| Aborts in kernel mode (copyio faults) | `upcalls->exception(cpu, frame)`; may change `frame->pc` | upcall |
| Interrupts, IPIs, timer | `irq_masked`/`irq_pending`, `upcalls->irq`, `irq_ack/eoi/enable`, `ipi`, `timer_set`, or the GIC and `CNTV_*` registers | flag, upcall, host calls, traps |
| Idle | `cpu_idle` (or `wfi`) | host call, trap |
| Secondary CPUs | `cpu_start` (`upcalls->cpu_entry`), or PSCI `CPU_ON` | host call, trap |
| Context switch | the kernel's own (callee-saved registers and stack); `frame_resume` for a full frame | kernel code, host call |
| current thread | `TPIDR_EL0` in kernel context (or emulated `TPIDR_EL1`) | register |
| Time | CNTVCT_EL0, `timebase_hz`, `wall_time` | register, host call |
| PAC | the process's keys: the same in the kernel and every task; key-register writes are ignored | fork |
| Console, panic, shutdown | `console_write` / PL011; `halt(reason, code)` or PSCI | host call, trap |
| Disks, input, display | virtio-mmio windows; `io_map`, `mmio_read/write` as the fast path | traps, host calls |
| Signed-text changes (`TPIDRRO_EL0`) | `text_fixup` | host call |

## 4. What was ported from where

| Source (commit) | Licence | What | Where |
| --- | --- | --- | --- |
| [DarwinBoot](https://github.com/UniXNU/DarwinBoot) `8544957`: `DarwinBoot.c`, `BootArgs.h`, `PackedRamdisk.h`, `Test/make-fixtures.py` | BSD-2-Clause-Patent | Mach-O and fileset parser, `boot_args`, trust-cache and packed-RAM-disk checks, placement order, the fixture shape | `runtime/loader/src/macho.rs`, `boot.rs`, `fixture.rs` (their heads keep the notice) |
| [UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) `f9121cd`: `ipados27/scripts/make-droidvm-adt.py` | BSD-2-Clause-Patent | the device tree that boots iPadOS 27.0.1: nodes, properties and the reasons recorded for them | `runtime/loader/src/machine.rs` |
| [ArmDTPlatform](https://github.com/UniXNU/ArmDTPlatform) `e8bd8d5`, [ArmCpu](https://github.com/UniXNU/ArmCpu) `1604083` | BSD-2-Clause-Patent | not copied: their requirements define the machine. The tree has what `configure()` demands; the GIC registers ArmCpu reads and writes, timer INTID 27, SGIs and PSCI `CPU_ON` are what `gic.rs`, `sysreg.rs` and `cpu.rs` implement | `runtime/xnuhost/src/gic.rs`, `sysreg.rs`, `cpu.rs` |
| [VirtioMmioBlock](https://github.com/UniXNU/VirtioMmioBlock) `4e1013e` | BSD-2-Clause-Patent | not copied: the register sequence of `NativeVirtioBlock::configureQueue` is the device model's test (in the test kernel and the unit tests) | `runtime/core/src/virtio.rs`, `src/virtio_blk.rs` |
| [RamBlock](https://github.com/UniXNU/RamBlock) `60a54ce` | BSD-2-Clause-Patent | the role (seed + persistent store), done with a sparse file; `DBRDISK1` packed RAM disks are accepted by the loader | `src/blk.rs`, `runtime/loader/src/boot.rs` |
| iOSPlus10 `b2ffcac`: `external/slop/shim/loader.c`, `external/slop/tools/adt.py`, `docs/ios26-port-status.md` section 7 | no licence file in slop: nothing copied | the 608-byte `boot_args` layout; the ADT dump tool's behaviour (`xnuloader-inspect adt`); the one-global-key-set PAC model | `runtime/loader/src/boot.rs`, `runtime/loader/src/bin/xnuloader-inspect.rs`, section 2.6 |
| gVisor systrap, User-Mode Linux skas | design only | the stub, trap pages, service thread | `runtime/xnuhost/stub/`, `src/task.rs` |

Not used, and why: iOSPlus10's `BOARD=` shim and boot-image flow, its `mkadt.py`/`adt-extra.py` and its
platform patches target a bootloader hand-off and SoC hardware that do not exist under a process.
Darling's darlingserver was cloned and not ported: its libsystem_kernel calls the server, iOS's issues `svc`.
XnuPrelink and VerifiedRoot are the kernel's and the image set's. vphone-android's `vphone-native`,
`reims-vgpu` and `metal2vulkan`, and Droid-VM's macOS-DroidVM were not read for this work.

## 5. Changes to the plan's design (`docs/plan.md` sections 4-6)

1. **xnuhost presents the DroidVM board, not a new set of host drivers.** The plan listed HostBlock,
   HostDisplay, HostHid as new kexts. Instead xnuhost emulates the virtio-mmio devices, GICv3, PL011 and PSCI
   that UniXNU's and UniXNU-droidvm's drivers already drive, in-process. Fewer new guest drivers; the fast
   path (`mmio_read/write` host calls) can come later.
2. **Physical = host virtual.** The plan had a memfd offset behind a pmap. With the window's address as its
   physical address, MMU-off start code and DMA addresses work unchanged.
3. **Unchanged privileged instructions are emulated** (SIGILL), so the port of XNU can be gradual and
   Apple's kexts need rewriting only where it is hot. The plan had a census and a rewrite as a gate.
4. **A service thread per task** carries out map orders. The plan had the faulting thread do it; the pmap
   also changes maps of threads that are not faulting.
5. **A factory process** forks tasks, not xnuhost itself (which has threads and gigabytes mapped).
6. **Interrupted kernel contexts live on the kernel stack and return through `rt_sigreturn`**: needed so the
   kernel can switch threads inside an interrupt.
7. **Its own program in `runtime/`, not in `android/vphone-native`.** Written in C first; rewritten in Rust on
   2026-11-23 (branch `rust-runtime`, one C file to one Rust module, behaviour and log lines kept;
   [runtime/RUST.md](../runtime/RUST.md)).
8. **GPU.** The plan's "forward KGSL calls" is dropped: Droid-VM measured the KGSL proxy at half the speed
   and retired it. The proven path on this SoC is virtio-gpu's DRM native context served by drm2kgsl
   (`docs/references.md`); see section 8.

## 6. Tests and what they prove

The Rust runtime (2026-11-23, branch `rust-runtime`; every C test was ported with its cases and expectations):

| Test | Where it runs | Result |
| --- | --- | --- |
| `cd runtime && cargo test -p xnuloader` (`runtime/loader/tests/loader.rs`) | PC | every case of the C `test_loader.c` (193 checks) in 11 tests, plus the Makefile's `xnuloader-inspect` checks and a `mkkernel` run, pass; against the C loader on two refused kernelcaches the output and exit codes were byte-identical |
| `cd runtime && cargo test -p xnuhost` (unit tests beside each module: `src/*/tests.rs`, `#[cfg(test)]`) | PC (x86_64; `cpu.rs` and `task.rs` with their stand-ins) | 73 pass (commit `8c56fa5`); the 3D renderer's stand-in-KGSL scenarios need `external/virglrenderer` and the memory ones `/dev/udmabuf` |
| `cd runtime && cargo test -p xnuhost-abi` (`runtime/abi/tests/headers.rs`) | PC | the Rust mirrors of `hostcall.h`, `free_report.h`, `free_report_runs.h` agree with the C headers (every constant, size and offset), and the kernel's copies are the same files |
| `runtime/xnuhost/check-arm64.sh smoke check` | the static Android xnuhost under `qemu-aarch64` (natively on arm64 Linux) | `smoke`: the vCPU machinery alone passes; `check`: the Rust test kernel's five boots pass (`TESTKERNEL: all passed` with 3 vCPUs and 256 MiB, `ram=auto` 1024 MiB held, the restart loop's exit 3 after 9 starts, a SIGUSR1 from outside in `testkernel-hold`: the `xnuhost !! fatal signal 10 (SIGUSR1)` line and status 138, not SIGABRT, which qemu-aarch64 11 takes itself; and the task trim with `task-trim-idle=1` and `testkernel-trim`: after 4 s idle the task faults at its pc and then at its data page, filled it reads the page as the kernel left it, and xnuhost says `tasks: trim: the first`) |
| `runtime/xnuhost/check-arm64.sh ab` | the same, two binaries pinned and alternated | the Rust xnuhost level with the C on every BENCH line before the stub's word copy, faster after (section 2.6a) |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` | PC; clippy also for `aarch64-linux-android` | clean |
| the phone | - | **not run yet** with the Rust runtime |

`runtime/host-tests.sh` runs all of these with the rest of the runtime ([building.md](building.md) section 4). The C
runtime's record, kept for its numbers:

| Test | Where it runs | Result (2026-11-10, the C runtime) |
| --- | --- | --- |
| `runtime/loader`: `make test` | PC, ASan/UBSan | 150 checks pass: Mach-O and fileset parsing with DarwinBoot's rejected variants, the tree round trip and malformed trees, the board's tree, sidecars, placement in both `boot_args` layouts, hosted and firmware |
| `runtime/xnuhost`: `test_xnuhost` | PC, ASan/UBSan | 149 checks pass: instruction decoding, MMIO emulation, the GIC by registers and by calls, system registers and PSCI, virtio block/GPU/input driven as a guest driver does, the seccomp filter run in a BPF interpreter, configuration, the Data store |
| `make check-arm64` | the Android binary under `qemu-aarch64` | the test kernel's checks pass (98 on 2026-11-14, with `test_jit_icache`) |
| cross-build | NDK 28.2, `aarch64-linux-android30`, `-Wall -Wextra -Werror` | clean |

The test kernel (`runtime/xnuhost/testkernel`, a no_std Rust program for `aarch64-unknown-none` built by `testkernel/build.sh`; `tests/testkernel.c` until 2026-11-23, every check ported in its order and with its text) is our own freestanding program wrapped as a Mach-O (by the loader's `mkkernel`) linked at
`0xfffffff007004000`. xnuhost loads it by the real path (parse, place, device tree, `boot_args`, enter on
vCPU 0) and it checks: `boot_args` and the tree; the emulated registers; the timer waking `cpu_idle`; five
timer interrupts taken inside a running computation with integer and floating-point state intact; the timer
by `CNTV_*`; a second CPU by `cpu_start` with an SGI back; a third by PSCI `CPU_ON` (entered with the vector and its `xh_cpu`); a RAM page mapped into the kernel's address space; the GIC and a virtio
disk by trapped loads and stores, with a sector read through a virtqueue; `text_fixup`, `ram_discard` (a page, a run of
pages and its neighbours, the refusals, a page a task maps, a page whose unmap is queued: 0058); and a
task: a mapped code page, a fault on an unmapped page, the kernel mapping it, the store retried and visible
in the kernel's view, a breakpoint, a looping thread preempted by the timer; the deferred maps and unmaps (in the
queue's order, more unmaps than the queue holds, and two threads of one task released on vCPUs 0 and 1 while one of
them, or an order, carries a queued unmap: 200 rounds, 2026-11-12; under qemu on the PC, 5 runs in a row passed); and
WebKit's JIT (`test_jit`, 2026-11-12): one physical page mapped `task_map` as read-write-execute (MAP_JIT's W+X
reservation) at one address and read-execute at a second (the W^X alias WebKit uses when the host has no APRR/SPRR
fast permissions, which standard Arm cores do not), a thread writing an instruction sequence through the read-write
alias, the i-cache maintenance an Arm JIT must do, and a branch to the read-execute alias: the page runs as it was
written (the two aliases share the physical page, so a store through one is fetched through the other), and the
store is also in the kernel's own view of the page. This proves the host layer carries WebKit's separated-W^X-heaps
JIT; the `msr sprr_uperm_el0` per-thread path is never taken, since the kernel leaves the commpage's fast-permissions
byte zero. (Under qemu-user the host has no SELinux, so whether the phone's kernel permits a W+X mapping of the RAM
memfd for a task is **not** covered here; see section 7.) And JSC's i-cache flush (`test_jit_icache`, 2026-11-14):
code written through the RW alias of a MAP_JIT page, then `dc cvau` + `dsb ish` + `ic ivau` + `dsb ish` + `isb` on
the **RX** alias and a branch to it, once with the RX alias mapped first (no fault) and once with it never touched,
so that its first access is the `dc cvau`: a fault there must come as a read (`XH_PROT_READ`, ISS.CM in the ESR),
after which the test kernel maps the page read-execute, as the kernel does, and the code runs. **What qemu cannot
show:** qemu-user (11.1.3) runs `dc cvau`, `dc civac` and `ic ivau` as no-ops that never fault, even on an unmapped
or `PROT_NONE` page (checked with a probe on 2026-11-14), so under `check-arm64.sh check` the never-touched alias's
first fault is the branch (the test prints a `NOTE`) and the cache maintenance fault path runs only on arm64
hardware (the phone, or `check-arm64` on an arm64 Linux host, which runs the binary without qemu). The decision
itself (an ESR with CM is a read, whatever WnR says) is checked on the host by `fault_access_of_syndromes` in
`runtime/abi/src/stub.rs` (the C `test_xnuhost`'s `test_fault_access`) with the hardware's syndromes, and on the kernel's side by `hosted_selftest=1` (0071).

**Not covered anywhere**: qemu-user has no seccomp and no `process_vm_writev`, so the `svc #0x80` trap and
`task_read/write` have run nowhere (the test prints SKIP). The filter's logic is tested in the interpreter
only. The KMS back end, evdev reader and uid drop have never run. Nothing has run on the S24 Ultra.

One experiment with the kernel agent's build (`out/kernel/current/xnu.macho`, the unchanged EL1 kernel):
xnuhost loads it and enters `_start`; under qemu it runs `_start_first_cpu`, builds its bootstrap page
tables and passes its MMU switch (the register writes are shadowed), then faults on its first access to a
link-time high address (`0xfffffff00719c028`). That is where the hosted layer has to begin.

## 7. Known weaknesses

- A task that jumps into the stub's `svc` with chosen registers can `mmap` any offset of the RAM memfd: a
  guest kernel compromise by guest code (not a host one beyond what the uid allows). `--set task-uid=N`
  drops tasks to another uid; default is xnuhost's own, because the drop is untested.
- **W+X of the RAM memfd on the phone is unverified.** The JIT page a `used_for_jit` entry describes is mapped
  `task_map` with `XH_PROT_READ | XH_PROT_WRITE | XH_PROT_EXEC` (the stub `mmap`s `PROT_READ|WRITE|EXEC` of the RAM
  memfd). xnuhost's own RAM mapping is W+X and works, and `test_jit` passes under qemu-user, but qemu-user has no
  SELinux; One UI's kernel under KernelSU may refuse a write+execute mapping of a shared file for a task process. If
  it does, the fill fails and `task.rs` logs `tasks: pid N: a deferred map (va 0x..., prot 7) failed`; that is the
  first line to read when WebKit's content process cannot run its JIT on the phone.
- Trap cost is a signal plus two hand-offs per system call or fault: measured in section 2.6a on the Mac's container,
  not yet on the phone.
- `task_map` rides on the next release (section 2.6a); `task_unmap` and `task_protect` are one round trip to the
  service thread each.
- The GIC model ignores priorities; the shadow registers ignore meaning.
- A stop does not ask the guest to shut down.

## 8. What is missing before launchd, backboardd and SpringBoard

In order. 1-3 are the kernel agent's with this ABI; the rest are split as noted.

1. **The hosted machine layer in XNU** (`kernel/`): a low link address or working slide below 2^39; start
   code that takes `x1`/`x2` and skips the MMU; a pmap on `task_map/unmap/protect`; the return to user as a
   `thread_run` loop feeding `sleh_synchronous`; interrupt masking on `cpu->irq_masked`; `current_thread()`
   from `TPIDR_EL0`; copyio; context switch; CPU start. Nothing of it exists. **This is the bulk of the work.**
2. **A kernel collection**: the prelink with the iPhone18,2 kexts, with `TPIDR_EL1` reads rewritten.
3. **Guest drivers for this board** linked in: virtio block (for the three images and the Data store),
   input, the 2D display under IOMobileFramebufferLegacy; UniXNU-droidvm's are for the iPad kernelcache.
   (Block and the display are in: `kernel/patches/oneios-hosted/storage`, `display`.)
4. **First run on the phone** of everything in section 6 that qemu cannot run: `--selftest`, the test kernel
   with seccomp, trap cost. Then fix what breaks.
5. **launchd**: verified root over the block devices, the Data volume made by the guest on first start from
   the tar seed, trust caches (`trust/*.trustcache` are IM4P; the loader's sidecar takes DarwinBoot's
   envelope: a conversion at image build or in xnuhost), the shared region, `text_fixup` wired into the
   kernel's page-in.
6. **backboardd**: the display helper (root SurfaceControl layer reading `display.sock`), HID digitizer
   from virtio-input, AppleKeyStoreState and SoftwareAES in the kernel.
7. **SpringBoard**: Metal. virtio-gpu 3D: the DRM native context (capset 6) with drm2kgsl on
   `/dev/kgsl-3d0` in-process, blob resources over the RAM memfd through `/dev/udmabuf` (several udmabufs for a
   blob past 64 MiB or 1024 runs of pages, the phone's limits; `docs/gpu.md` section 2), per-ring fences; each KGSL
   import's IO coherence read back and logged (`gpu: KGSL imports ...`, drm2kgsl patch 0007, `docs/gpu.md` 2.4);
   guest side UniXNU-droidvm's `DroidVMVirtioGpu3D.cpp` and the Metal plugin. (In since: `src/virtio_gpu.rs` serves 2D and 3D, the renderer is `src/gpu3d_kgsl.rs` on drm2kgsl, [gpu.md](gpu.md).)
8. Then: the bridges' first run on the phone (network, sound, haptics, Bluetooth are wired and untested),
   battery, a guest-initiated shutdown on `SIGTERM`, the uid drop and trap batching.
