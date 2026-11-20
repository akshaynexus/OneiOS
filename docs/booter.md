# The booter: xnuhost and the loader

Owner: booter. Code: `runtime/loader/` (the loader library), `runtime/xnuhost/` (the program),
`runtime/build.sh` (what `runtime/package.sh` calls). Date: 2026-11-10.

xnuhost boots a source-built XNU **as a Linux process** on One UI's kernel: it loads the kernel collection
into its own address space, gives it CPUs (host threads), interrupts and a timer (signals), devices
(emulated in-process) and iOS tasks (other Linux processes whose system calls and faults it traps).

**State, plainly.** Everything below is built and tested on the PC, including the whole program booting a
test kernel under `qemu-aarch64`. Nothing has run on the phone, and iOS does not boot: the kernel that
uses this host (the hosted machine layer inside XNU) does not exist yet. Section 8 lists what is missing.

## 1. Use

```sh
runtime/build.sh STAGE        # Android binary -> STAGE/bin/xnuhost (static, 1.2 MB; the task stub is inside),
                              # and the bridges' bin/oneios-audio, bin/oneios-net.sh
runtime/build.sh --test       # loader tests, xnuhost tests, and the binary booting the test kernel under qemu-aarch64
runtime/host-tests.sh         # every host test of the runtime; on a Mac in an arm64 Linux container, where the
                              # binary boots the test kernel as it is (no qemu)
```

Both build hosts work: a Linux x86_64 PC and a Mac (`runtime/host.mk` picks the NDK's `linux-x86_64` or
`darwin-x86_64` tools). The host unit tests are Linux tests; `tests/test_xnuhost.c` is built with `-DXH_HOST_TEST`,
so `cpu.c` and `task.c` take their stand-ins on an arm64 Linux host too ([building.md](building.md) section 4).

Command line (the first line is what the OneOS fork runs, `oneos/interface.md` section 4):

```sh
xnuhost --session-abi 1 --mode takeover --runtime DIR --image DIR --data DIR --session DIR
        [--kernel FILE] [--cpus N] [--ram SIZE|auto] [--boot-args TEXT] [--display shm|drm|none]
        [--width W --height H] [--hz N] [--scale 2|3] [--no-input] [--input-script FILE] [--no-tasks]
        [--hw-backend samsung|treble] [--hw-backend-BRIDGE samsung|treble] [--hw-backend-fallback 0|1]
        [--config FILE] [--set KEY=VALUE] [-v]
xnuhost --selftest            # on the phone: page size, address bits, PAC keys across fork, memfd, seccomp
```

| Input | Default | What |
| --- | --- | --- |
| `--image DIR` | required | the image set (`docs/image-format.md`) |
| `--kernel FILE` | `DIR/kernel.kc`, else `DIR/kernel.macho` | the kernel collection: `MH_FILESET`, or an `MH_EXECUTE` kernel |
| `FILE.trustcache`, `.vroot`, `.gpuaddon`, `.ramdisk`, `.adt` | optional | DarwinBoot's sidecars beside the kernel, same formats and checks |
| `DIR/identity.adt` | optional | the device's own decoded device tree: model, target-type, board name and `/product` are taken from it. Without it the root has model iPhone18,2 and target-type V54 (since fix-ios-compressor: iOS names its jetsam properties by it, section 2.2). No script writes one (2026-11-13: `scripts/image/build.sh` does not, and the PC's set `out/image-pc/24A446` has none), so `/product` is only `product-name`, `artwork-scale-factor` and `model`: none of the iPhone's own `/product` (`artwork-display-gamut P3`, `oled-display`, `graphics-featureset-class APPLE10`, `ui-background-quality 100`, ...), which MobileGestalt answers from. `--set identity=out/kernel-inputs/24B156/devicetree.bin` (the decoded tree as it is) gives them for one run (`kernel/patches/oneios-hosted/display/README.md`, "What iOS reads about the panel") |
| `--display-format bgrx\|rgba16f` (`display-format=`) | `bgrx` | the frame's pixels (section 2.8): 8-bit sRGB, or half floats that keep wide colour and EDR |
| `DIR/xnuhost.conf` | optional | `key=value` lines: `boot-args`, `disk`, `disk-rw`, `data-store`, `scale`, `scale-fixup`, `text-fixup`, `ram`, `ram-percent`, `ram-host-floor`, `host-mem-log`, `ram-giveback`, `ram-prefault`, `ram-lock`, `ram-session-reserve`, `ram-unlock-below`, `ram-huge`, `ready-content-wait`; the command line wins |
| `DIR/overlay/userland/xnuhost.conf` | optional | the same, written by the userland hook: `scale=3` or `scale=2`, the UI scale the image's Data seed was made for (`docs/userland.md` "The screen") |
| `--width W --height H`, `--hz N` (or `width=`, `height=`, `hz=` in the image set's `xnuhost.conf`) | the display helper's report of the panel | the guest's display mode (section 2.8); for a PC run or another panel |
| `--set display-direct=0` (or `display-direct=0` in `xnuhost.conf`) | 1 | 0: xnuhost copies every frame into the memfd, even for a helper that can copy from guest RAM (section 2.8, direct frames) |
| `--scale 2\|3` (`--set scale=2`) | the image set's, else 3 | the UI scale: pixels per point |
| `DIR/os.img`, `systemos.img`, `app.img` | as present | read-only disks, in this order |
| `--data DIR` | none | `data.img`: the Data store, sparse, created on the first start; `unique-chip-id` |
| `DIR/data.seed` | as present | a tar seed is served as the last read-only disk (the guest makes the Data volume and unpacks it); an APFS seed image is copied into a new store |
| `--session DIR` | none | `xnuhost.pid`, `ready`, `return.request`, `display.sock` |
| `--cpus` | the host's cores, at most 8 | 1 to 8 |
| `--ram SIZE` or `--ram auto` (`ram=`) | `auto` | the guest's RAM: a multiple of 2 MiB, 256 MiB to 64 GiB. `auto`: the least of `ram-percent` of the host's `MemTotal`, `MemTotal` less `ram-host-floor`, and what fits beside the session's needs (section 2.2, "What fits"), rounded down to 256 MiB (below). A size given by hand that is more than fits is used, with an error line saying so |
| `--set ram-fit=0\|1` | 1 | 1: `ram=auto` takes no more than what fits (2026-11-14); 0: as before (share and floor only) |
| `--set ram-gpu-reserve=SIZE` | 256M | what fits leaves this for KGSL's own memory |
| `--set gpu-level=deadline\|hold\|governor` (`gpu-level=`) | `deadline` | the GPU's level (`docs/gpu.md` 2.2b): the top only for a frame that would miss its deadline; `hold`: every render-server frame at the top to 150 ms after it (the rule before 2026-11-15); `governor`: KGSL's governor alone. Passed to the renderer as `ONEIOS_GPU_LEVEL` |
| `--set gpu-priority=0\|1` (`gpu-priority=`) | 1 | 1: each GPU context's KGSL priority from its owner's class (render server and front 4, default and background 8), background contexts at KGSL's lowest level at a still screen while no other context has GPU work queued (`docs/gpu.md` 2.2b); 0: the guest's queue priority, as upstream, and no lowest level (`ONEIOS_GPU_PRIORITY=off`) |
| `--set ram-pagetable-percent=N` | 8 | what fits counts the iOS tasks' page tables as this share of the guest's RAM, 0 to 50 |
| `--set ram-huge=0\|1` | 1 | 1: 2 MiB host pages for xnuhost's window of the RAM where the host allows them (shmem THP by `MADV_HUGEPAGE`; only with the RAM held fixed); section 2.2, "2 MiB pages" |
| `--set ram-percent=N` | 90 | `ram=auto`'s share of `MemTotal`, 10 to 100 |
| `--set ram-host-floor=SIZE` | 1.5G | what `ram=auto` always leaves the host |
| `--set host-mem-log=SECONDS` | 30 | a `memory: host MemAvailable` line every so often; 0 = only at the start and the stop |
| `--hw-backend samsung\|treble` (`hw-backend=`, also in `xnuhost.conf`) | `samsung` | which form of the phone's hardware every bridge uses: today's Samsung paths, or the vendor-neutral Treble interfaces (AIDL HALs through servicemanager, evdev by capabilities, standard sysfs classes by named paths). One line per bridge says which and why (`hw: battery: backend treble (hw-backend=treble)`). [hardware.md](hardware.md) 7e has the audit table |
| `--hw-backend-BRIDGE samsung\|treble` (`hw-backend-backlight=`, `-battery`, `-thermal`, `-haptics`, `-touch`, `-keys`, `-audio`, `-camera`, `-wifi`, `-bluetooth`, `-display`) | unset (`hw-backend`'s) | one bridge's backend, over `hw-backend`; an unknown bridge name is refused |
| `--hw-backend-fallback 0\|1` (`hw-backend-fallback=`) | 0 | a treble bridge whose interface is missing: 0 the bridge stays off, 1 it uses its samsung form; either way one line says what was missing |
| `--set usb-log=SECONDS` | 3 | how often the phone's USB gadget is read for its `usb:` line (section 2.9a), which is logged at the start, on every change and at the stop; 0 = off |
| `--set ready-content-wait=SECONDS` | 120 | `session/ready` comes at the guest's first frame of its own that is not all black, or this long after its first frame of its own when all were black; 0 = at its first frame (the rule before 2026-11-15); 0 to 480 (section 2.8, "the ready rule") |
| `--set ram-meminfo=FILE` | `/proc/meminfo` | where `ram=auto` reads `MemTotal` and `MemAvailable` (tests) |
| `--set ram-giveback=0\|1` | **0** (since fix-ram-locked, 2026-11-15) | 0: the RAM is **held fixed and locked for the session** (section 2.2, "Fixed RAM": prefaulted, `mlock`ed, no free pages given back; the owner's rule: iOS's RAM is never swapped). 1: 0058's way again (faulted in as used, free pages given back, the host may swap it); one setting flips it back |
| `--set ram-prefault=0\|1`, `ram-lock=0\|1` | 1, 1 (with `ram-giveback=0`) | prefault the whole memfd on a nice-19 thread, `mlock`ing it as it goes; at the first `mlock` that fails the prefault stops and what is locked stays locked (where it fails at the first step, EPERM or an RLIMIT_MEMLOCK under 16 MiB, nothing is prefaulted); nothing is prefaulted unlocked unless `ram-lock=0`; neither if the RAM is more than fits |
| `--set ram-session-reserve=SIZE` | 768M where the lock can work (CAP_IPC_LOCK, or no RLIMIT_MEMLOCK), else 1G | the headroom what fits leaves the host (section 2.2, "What fits"); with the lock never less than `ram-unlock-below` + 256 MiB; the lock is tried only if the RAM is within what fits |
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
`xh_ram_resident` in `src/discard.c`, 64 MiB per call; since fix-ram-locked only over the part that is not locked: the
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
`return: graceful shutdown timed out after N ms; stopping`. State machine: `runtime/xnuhost/src/return.c`
(unit-tested). **The guest half is the Mac's to build:** a guest helper that `oneios_power_request()` reaches which
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

A port of DarwinBoot to plain C with no firmware under it:

- `macho.c`: arm64/arm64e `MH_EXECUTE` and `MH_FILESET` (`LC_FILESET_ENTRY`, `com.apple.kernel` checked against
  the collection's segments), every offset bounds-checked; a symbol lookup. Fixups are **not** applied: XNU
  rebases and signs its own chains after entry, as under DarwinBoot.
- `adt.c`: Apple device tree in memory: build, parse, edit, flatten.
- `machine.c`: the tree of xnuhost's board, a port of UniXNU-droidvm's `make-droidvm-adt.py`: `/chosen` (256
  seed bytes, CHRP NVRAM proxy, dram, security properties, root matching, MAC addresses), `/cpus`, `/psci`,
  `/defaults`, `/options`, `/product` (copied from the identity tree, minus exclaves and the AGX-only feature
  sets; `artwork-scale-factor` is the UI scale), `/buttons`, `/bluetooth`, `/arm-io` with the GICv3, `wlan`, `uart0` and one node per virtio window
  under the names and `compatible` strings UniXNU's drivers match; `/chosen/xnuhost` for a hosted kernel.
- `boot.c`: `boot_args` (1024-byte command line of Darwin 23+, or the 608-byte one), the sidecar checks
  (trust caches, packed RAM disk `DBRDISK1`, GPU addon, root policies) and the placement in DarwinBoot's
  order: kernel, `boot_args`, tree, RAM disk, trust caches, addon, policies; memory-map entries filled.

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
- **What follows the RAM's size** (checked for `ram=auto` by the test kernel, `make check-arm64`'s third boot): the
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
  wired ones) with `ram_discard`: `xh_ram_discard_fd` (`src/discard.c`) punches a hole in the memfd
  (`FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`, allowed under the GPU's shrink seal), only inside the kernel's window
  (never the boot framebuffer or the GPU's pool above it), host-page aligned, else -EINVAL; the range then reads as
  zero in every mapping and the host holds nothing for it until it is touched again (a read through a shared mapping
  fills it too). xnuhost counts calls, bytes, failures and time for the `memory:` line. The kernel's side (what is
  given back, when, in what order) is in `kernel/patches/oneios-hosted/README.md`, 0058; its decisions are
  `include/xnuhost/free_report.h`, which the kernel carries as a copy (the host tests check they are one file). Run on
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
  - `fit = (MemAvailable - ram-session-reserve - ram-gpu-reserve) / (1 + ram-pagetable-percent / 100)` (`xh_ram_fit`,
    `config.c`), MemAvailable read at xnuhost's start, which is after the fork stopped One UI's zygote and
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
  - A size given by hand (`ram=`) is used as given; if it is more than fits, an error line says so: `memory: ram=N MiB is
    more than this host can hold without swapping it (F MiB fit): the host will swap iOS's RAM; give ram=auto or ram=..M`.
  - When the host does swap iOS's RAM (held less resident, every 5 s), a plain line, once past 64 MiB and again at each
    further 256 MiB: `memory: the host swaps iOS's RAM: N MiB (host MemAvailable M MiB); host swap-ins I pages/s (X MB/s),
    swap-outs O pages/s (Y MB/s)` (the rates are the host's, all processes, `/proc/vmstat` pswpin and pswpout over the
    last 5 s). Heat has its own line (fix-thermal-log); this one is only about iOS's RAM.
- **Fixed RAM (`ram-giveback=0`, `src/ramfix.c`, fix-fixed-memory, 2026-11-13; the default since fix-ram-locked,
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
    back to 0058's way; `xh_proc_values` on meminfo and vmstat files), and the test kernel under qemu-aarch64 (`make
    check-arm64`'s second boot runs with the defaults: the refusal, the prefault, the sizing line, and on the PC the
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
    (`g_mem.fd_phys`, 16 KiB below; that head is mapped, never guest RAM), and every offset is `address - fd_phys`
    (the tasks' maps, `kernel_map`, `ram_discard`, the GPU's udmabufs). Measured natively on the PC (a 64 MiB memfd
    mapped `MAP_SHARED` with `MADV_HUGEPAGE`, `shmem_enabled` advise): at offset 0 `ShmemPmdMapped 65536 kB`, VmPTE
    56 kB; at offset `0x4000`, today's layout, `ShmemPmdMapped 0 kB`, VmPTE 176 kB.
  - **The choice** (`xh_huge_choose`, `ramfix.c`; read only, named files, once): `shmem_enabled` (a memfd is on the
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
    boot can show the 2 MiB pages themselves, and under qemu `/proc/self/stat`'s minor faults read 0. **Not run:** the
    phone.
  - With the RAM more than fits (`ram=` by hand) the `memory: fixed RAM` line said `held, prefault running` for the
    whole session (no prefault was started); now `held, not locked`.
- **iOS's own compressor and the memory view (`ram-view=`, fix-ios-compressor, 2026-11-15; off by default).** The phone's
  session of main 3233fed logged `oneios-mem: free 277 MiB ..., compressor 0 MiB holding 0 MiB` with many jetsam lines.
  What the code says (xnu-12377 as built here, every file under `osfmk/` or `bsd/`):
  - **The compressor is on, in RAM, as on an iPhone18,2 (proven).** `vm_compressor_mode` is a `TUNABLE_DT` of
    `/defaults kern.vm_compressor` with the default `VM_PAGER_FREEZER_WITH_SWAP` (32) for `CONFIG_FREEZE`
    (`vm_pageout.c`); `vm_config_init` makes 32 compressor active, general swap off, freezer swap on. The PC's log says
    `vm: "vm_compressor_mode" is 32`; 24A446's own DeviceTree has `kern.vm_compressor` = 32 too. No boot argument of
    xnuhost's (`config.c`) or of the PC's sets `vm_compressor`, and no hosted patch touches the compressor or the pageout
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
    applied. **Fixed:** the loader gives the root `target-type` V54 without an identity tree (`machine.c`, `main.c`).
  - **What the guest lacked (proven).** The guest's `/defaults` had none of the iPhone18,2's per-process limits
    (`kern.max_task_pmem` 3376, `kern.entitled_max_task_pmem` 6144, `kern.entitled_dev_max_task_pmem` 6656 MB in 24A446's
    DeviceTree): a task without a limit of its own (launchd's or RunningBoard's) had none (`task_init`). Since this change
    the loader always gives `kern.max_task_pmem` 3376 (`runtime/loader/src/machine.c`); a footprint counts the task's
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

Kernel code keeps its own `TPIDR_EL0` and `x18`; xnuhost's C needs bionic's. Every host call goes through
an assembly thunk (`arch_aarch64.S`) that saves the kernel's two registers, loads the host's, switches to
a per-CPU host stack, calls the C function, and switches back. Signals enter through a thunk that finds
the vCPU from the signal stack's address (stacks are 1 MiB aligned blocks; the first word is the vCPU)
and does the same swap. So the kernel may keep its current-thread pointer in `TPIDR_EL0`, which lets
Apple's kexts' inlined `mrs xN, TPIDR_EL1` be rewritten to `mrs xN, TPIDR_EL0` in place at prelink.

The contract is one header, `runtime/xnuhost/include/xnuhost/hostcall.h` (freestanding C, ABI 1): entry
registers, `struct xh_cpu`, `struct xh_vector` (32 calls; additions are appended and `vector->size` says which exist; the 32nd, `irq_take`, since fix-mach-rpc 2026-11-15), `struct xh_upcalls`, `struct xh_info`, frames
and traps.

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
- The controller is a GICv3 model (`gic.c`): distributor and redistributor frames by trapped MMIO, the CPU
  interface by trapped `ICC_*` registers, or the `irq_ack/irq_take/irq_eoi/irq_enable/ipi` host calls (the hosted
  kernel uses `ipi`, `irq_take` and `irq_eoi` for IPIs and the timer since hosted 0081; ArmCpu's trapped registers for
  devices' interrupts: 2.6d).

### 2.5 What is emulated from traps

So that unchanged code keeps working while the kernel's hot paths move to host calls:

| Instruction | Emulation (`sysreg.c`) |
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
logs the twelve most frequent at the stop and on SIGQUIT (`emulated: insn 0x... N times (first at pc 0x...)`, `sysreg.c`
`xh_sysreg_stats_log`): the first thing to read when an idle guest keeps its vCPUs busy (`docs/hardware.md` 7d).
`dc isw/csw/cisw` (set/way) with `setway-skip` (on by default; `setway-skip=0` in `--set` or `xnuhost.conf` turns it
off) sets its register to the last set and way of its level, so XNU's set/way loop ends after one trap: the guest's
`cpu_idle` flushes L1 by set/way at every idle entry, 1024 traps each, and that alone kept an idle lock screen at 310%
CPU in the vCPU threads (233% with the skip; phone, 2026-11-12).

The shadow does **not** give a write its hardware meaning: page tables, vectors and PAC key registers
have no effect. Loads and stores on the I/O aperture are decoded (`insn.c`: register, immediate, pre/post
index, pairs, sign extension) and run against the device models.

### 2.5a The thermal step

`runtime/bridges/thermal` reads the phone's temperatures every 5 s from the start. Since fix-ram-locked (2026-11-15; the
owner's rule: all CPU cores) it gives one step, at a critical temperature only: the case (lrp) at 50 C or the cell at
50 C (before, steps 1-3 from heavy pressure up: the case at 44 C, the SoC at 95 C). Then `main.c`'s `thermal_step`
narrows the vCPU threads' affinity (`br_cpu_plan_cap`, step 2: only the slowest P domain and the E cores, cpu0-1 and
5-6 here) and bumps `xh_cpu_place_epoch`, so
`xh_thread_run` places each iOS thread again at its next run (`task.c`). Only xnuhost's own threads and its tasks' are
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
- **Stub** (`stub/stub.c`, a 3 KB position-independent blob with no data and no libc). In a new task it
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
  (`cpu.c`: the kernel reads the `brk` at pc, `hosted_kernel_esr`) and in a task's trap (`stub.c`: `esr` 0 and the
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
  addresses only). The stub (`xs_fault_access`, `stub/stub_abi.h`) used to read WnR alone and said `XH_PROT_WRITE`;
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
  `cpu-pin=2` also pins with no plan, vCPU *i* on CPU *i* (for `make bench-arm64`).
- **Wakes only for a sleeper.** Each side says when it sleeps (`host_sleeping`, `task_sleeping` in the trap page) and
  the other side issues `FUTEX_WAKE` only then (`stub/stub_abi.h` has the rule and its barriers).
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

**Measured** with `make bench-arm64` (the test kernel's loops on vCPU 0, one iOS thread; native arm64 Linux 6.18 in the
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

**Deferred unmaps, measured on the x86_64 PC** (2026-11-12, `make -C runtime/xnuhost bench-arm64 BENCH_SET="--set
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

**On the phone** (combined boot of 2026-11-12 12:30, kernel.kc 1e297354, runtime 0.1.10-dev; `cpu-pin=1`, vCPU *i*
on CPU *i*; 30 s windows during the boot): 17,000-22,600 task traps a second, two to three faults for each system
call. Each hand-off took 10-12 us at p50 (up and down alike), ten times the container's: the cores were slow then (the
cap and the governor) and most hand-offs slept in a futex. The kernel's work after a trap: 41 us at p50 for a system
call (mean 115-130 us), 12-20 us for a fault (mean 60 us); a deferred map carried by a release took 50-70 us from the
release to done (the task's `mmap` of one page), an unmap order 29-32 us. So the kernel's own work and the number of
faults are now the larger costs, not the hand-off (the kernel side: `kernel/patches/oneios-hosted/README.md`,
"0040-0044").

**The report.** Each vCPU counts its traps by kind and keeps histograms (4 buckets a power of two) of each step
(`src/perf.c`). The main thread prints them every `perf-log` seconds (default 30, `--set perf-log=0` only at the stop
and on SIGQUIT), for that period; at the stop and on SIGQUIT since the start:

```text
perf: 30.0 s: N task traps (R/s): syscall .., fault .., interrupt .., other ..; futex sleeps: vCPU .., iOS thread .. (of .. hand-offs each way); futex wakes .. + ..; task_map .. carried by a release, .. ordered; task_unmap .. queued, .. ordered; timer_set .. (.. to the host timer)
perf:   up (trap->vCPU)    n ... mean ... p50 ... p90 ... p99 ... max ... us      the handler's copy and the hand-off
perf:   down (vCPU->task)  ...                                                     the release to the handler's wake
perf:   kernel: syscall    ...      the kernel's work after a trap of that kind, to this vCPU's next thread_run, idle out
perf:   kernel: fault / interrupt / other, order: map / unmap / protect / clone, maps by release, kick
perf: cpuN: C% of a core; of its wall time K% kernel, U% an iOS thread's turn, I% idle; T traps/s
```

"An iOS thread's turn" is the time `thread_run` waited (the thread's own code plus both hand-offs); "kernel" is the rest
of the vCPU's wall time outside `cpu_idle`; "C% of a core" is the vCPU thread's CPU time (Linux's clock of that thread).
The kernel's side of the same trips (which system calls, the fills, the TLB lock) is the hosted kernel's
`oneios-perf:` report (patch 0041, `kernel/patches/oneios-hosted/README.md`).

**Host overhead, 2026-11-15 (fix-host-overhead).** From the phone session of main 3233fed (30 s windows): 1,021,984 user
stays, 684,514 system calls, 330,746 faults, 331,008 user maps and 414,758 user unmaps (each an mmap or munmap of one
page in the task's process), and the host at 482 % of 800 with 212 % in Linux's own code. What changed in xnuhost:

- **The deferred queue joins entries** (`stub/stub_abi.h` `xs_queue_join`, unit-tested in `test_xnuhost`): a map of the
  next page of the same file range with the same protection extends the last map; adjacent or overlapping unmaps become
  one; an unmap followed by a map of the same range is the map alone (`MAP_FIXED` replaces the old page in one step: the
  pattern of a fault that changes an entry, the flush then the refill); a map followed by an unmap of the same range is
  the unmap alone; the same range mapped twice keeps the later map. Only the last entry is looked at (one compare per
  call). An unmap joined into the last entry that has to be taken back (a thread of the task was counted running) puts the
  entry back as it was and is ordered, as before. The `perf:   queue:` line counts the joins (one host call less each).
- **Maps are populated** (`MAP_POPULATE`, `XS_MAP_POPULATE`): the stub's maps of the RAM memfd, in the release and in an
  order, and the kernel's `kernel_map` fill the page tables in the same call, so the faulting instruction's retry does
  not take a second, Linux, fault on the page it was just given.
- **The GIC tells only the CPU a change concerns** (`gic.c` `update_cpu`): an ack, an EOI of an SGI or PPI, an SGI, a
  PPI or SPI level change recompute that CPU's line (an SPI's: its target's), not all eight under the one lock. Changes of
  the distributor, an enable, the priority mask still recompute every line.
- **A line that is up is not signalled again** (`cpu.c` `xh_cpu_line`): every GIC change used to send each other vCPU
  whose line was up a `tgkill` (or a futex wake), even when the line had been up for a while (a vCPU running with
  interrupts masked got a signal per interrupt step anywhere). The CPU takes a pending line when it unmasks, when a host
  call returns, and in `thread_run` and `cpu_idle` before they wait, and only it lowers its line, by recomputing under the
  GIC's lock after the raiser's change, so a raise skipped is never lost. `perf:   queue:` counts the raises signalled and
  those skipped.
- **The uclamp reaches iOS's own code** (`task.c` `xh_thread_run`, `xh_cpu_uclamp`): the guest's frequency request
  (DroidVMCpufreq's `SET_PERF`, `br_cpufreq`) and drm2kgsl's animation boost used to set `uclamp.min` on the vCPU thread
  only, which waits while the iOS thread runs on its task's own Linux thread: the host's governor never saw a request for
  the core where the work ran. Now each release gives the iOS thread its vCPU's clamp (one `sched_setattr` per change,
  none otherwise; SCHED_OTHER stays SCHED_OTHER, nice is kept). The kernel's hint (`hostcall.h` `perf_hint`, hosted 0075:
  the thread's performance-control class) gives a UI, user-initiated or realtime thread its floor and a background or
  utility one no floor and no ceiling of ours (a ceiling, `uclamp.max` 512, stood here until 2026-11-16; it also slowed
  chronod's widget extensions, newtodo's decision list). drm2kgsl's own clamp looked for threads named
  `crosvm_vcpu*` and logged `vCPU uclamp.min 1024: 0 of 0 threads`; its patch 0008 calls xnuhost's
  `drm2kgsl_vcpu_uclamp` instead (the vCPU threads and, through `xh_gpu_vcpu_boost`, the iOS threads).
- **The same timer deadline again is not a host call** (`cpu.c` `xh_cpu_timer_set`): a future deadline equal to the
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
- **Each vCPU is described as its own core** (`br_cpu_plan_make`): cpu-pin puts the rank-th vCPU of a kind on the
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
above-UI, 768 for user-initiated, the vCPU's own request for default, and 0 with no ceiling for utility,
background and darwin-background threads (RunningBoard's background and suspended roles clamp an app's threads to
background QoS; the `uclamp.max` 512 that stood here until 2026-11-16 also slowed chronod's widget extensions, #57, and
is gone: newtodo's decision list). The same word is readable by host code on the vCPU
(the GPU path: `xh_cpu_self()->cpu.perf_hint`, for fix-gpu-perf's per-context priority).

**The cpuset.** A thread in Android's `background` or `system-background` cpuset may use only the A520s, whatever
`sched_setaffinity` asks: cpu-pin cannot work there. xnuhost logs its cpuset and allowed CPUs at start. From the fork's
`switcher/assets/oneios_switch.sh` (branch `oneios`, read 2026-11-15): `--inner` moves the shell that execs xnuhost into
the v2 cgroup `oneios_session` only; the v1 cpuset (`/dev/cpuset`) is changed only for the watcher and the key readers
(`out_of_the_way`: the cpuset's root, every core). So xnuhost keeps the v1 cpuset of the root shell the app started
[inference: top-app's or foreground's when the switch is tapped from the front app; never read on the phone yet]. If the
`cpu: xnuhost's cpuset` line or `phone cpu` shows a cpuset of fewer CPUs, the fix belongs in the fork's `--inner` (a
cpuset with `/dev/cpuset/cpus`, as its cgroup move), not in xnuhost, which never leaves its cgroup: not done in this
change.

**The parallel test** (`make -C runtime/xnuhost parallel-arm64`, the test kernel's `testkernel-parallel[=SCALE]`, 8
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
bound is the host test's (`test_cpu_window`). uclamp: 0 on the PC (no CPU plan without the bridges).

**`make bench-arm64 BENCH_SET="--set cpu-pin=2"`** on the same PC, alternated, two rounds each (p50, us): null system call
19.0/19.0 before, 19.0/20.0 after; a fault filled by `task_map` 38.0/37.0 before, 38.0/37.0 after; timer interrupt of a
running thread 29.0/28.0 before, 28.0/27.0 after (the means fell, 21.6-20.1 against 20.4-65.0 for the null call, but the
box's load moved them more than the code did). The single-thread trip did not change, as expected: the fixes are in
what runs between trips (queue, GIC, timers) and in what several vCPUs share.

**The parallel test** (`parallel-arm64`, 8 vCPUs, scale 3, after the warm-up round; same box and load): 8 threads took
357-573 ms after and 418-474 ms before against 92-270 ms for one thread: speed-ups 2.2-4.4 (after) and 1.6-5.2 (before)
of an ideal 8, where the PC has 4 cores and other agents' jobs on them. The 2-thread and 4-thread runs are within the
same spread. So on this box nothing shows a serialisation in xnuhost's path (8 threads never took 8 times one thread),
and the PC cannot show more than its 4 cores; the phone's run (below) is the measurement that counts.

**The phone test (for the Mac; one session before, on main, and one after, on this branch, the same image set).**

1. Before the switch and in the session: `oneios-tool phone cpu > cpu-before.txt` (then `cpu-after.txt`). Read: the X4's
   top and boost (`cpu7 Cortex-X4 ... top ... boost ...`), any `CAPPED` domain, `cpufreq boost`, xnuhost's cpuset and
   `allowed` CPUs (all 8 is right; `0-1` means the vCPUs cannot reach the big cores), one `xnu-cpuN` thread on each CPU.
2. The parallel test on the phone's own cores, no session open: push `runtime/xnuhost/build/android/xnuhost` and the
   folder `build/android/testimage` to a scratch folder (`/data/local/tmp/par`), and run there as root the command line
   of the `parallel-arm64` target with `--set cpu-pin=2` (and once without): `PARALLEL N threads: ... speed-up` for 1,
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
SIGILL on the vCPU's thread, `xh_mmio_emulate` or `xh_sysreg_emulate` under the GIC's lock (a distributor store
recomputes all eight CPUs' lines, `gic.c` `dist_write` -> `update()`), and the signal return. Five signals per IPI, ten per
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

**The bench** (`make -C runtime/xnuhost rpc-arm64`, the test kernel's `testkernel-rpc`, docs/building.md section 4): a client
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

**What one system call costs, and where** (`make bench-arm64`, the same PC and load, p50): the null system call 24.0 us
kernel to kernel, of it svc -> the kernel has the trap 11.0 us (seccomp's or, under qemu, the marked instruction's signal,
the stub's register copy, the store and the vCPU's futex wake) and the release -> user code 12.0 us (the store, the futex
wake of the iOS thread, its way out of the futex and the signal return); each side sleeps in a futex every time (`futex
sleeps` = hand-offs). The phone's 36-37 us per call in `oneios-perf:` is not this: it is the kernel's own time after the
trap up to the vCPU's next user entry (0041), which for a call that blocks includes the switch and the next thread's way
back; the new `syscalls:` line says how much of it is which. On the phone (2026-11-12) each hand-off was 10-12 us at p50
(2.6a). Not measured on the phone yet: the same bench there (the command lines of 2.6c step 2 with `testkernel-rpc
testkernel-rpc-mode=0|1|2`).

### 2.7 Devices

All devices are virtio-mmio version 2 windows (`virtio.c`: split queues, indirect descriptors, the status
and feature handshake) at `0x0a000000 + n * 0x1000`, SPI `32 + n`, in this order: the disks, input, GPU.

| Device | File | Backed by | Guest driver it is written for |
| --- | --- | --- | --- |
| Block, read-only (`F_RO`) | `virtio_blk.c`, `blk.c` | `os.img`, `systemos.img`, `app.img`, a tar `data.seed` | UniXNU `NativeVirtioBlock` (node `virtio-mmio-N`, `compatible` `virtio,mmio`) |
| Block, writable | same | `<data>/data.img`, sparse, 32 GiB logical, flush = `fdatasync`, on the disk's flush thread ("Disks" below), timed (`disk NAME: 30 s: reads .., writes .., flushes N in T ms (max X ms)` every 30 s per busy disk, `since the start` at the stop, `disk NAME: a flush took N ms` for the first 16 of 500 ms or more: a flush holds every other request of the guest to that disk) | the Data store window (node `data-store`) |
| Input | `virtio_input.c`, `input.c` | `/dev/input/event*`: the multitouch events of the one device that is the screen (`INPUT_PROP_DIRECT`; a multitouch pointer device such as the S24 Ultra's `sec_touchpad` is not taken; the log line gives the screen's x and y ranges) and the three side keys, unchanged evdev codes, the panel's own axis ranges | UniXNU-droidvm `DroidVMVirtioInput.c` |
| GPU, 2D scanout | `virtio_gpu.c`, `display.c` | the display back end | UniXNU-droidvm `DroidVMVirtioGpuDisplay.cpp` under IOMobileFramebufferLegacy: `kernel/patches/oneios-hosted/display/` (a 2D resource per surface, up to 16384 backing entries each) |
| Console | `uart.c` | the log | XNU's PL011 console (`uart0`) |
| Bridge channel, network, sound | `runtime/bridges` (the bridges agent's; `docs/hardware.md`) | haptics and Bluetooth HCI; a tap with built-in DHCP; an AAudio helper process | `drivers/bridge`, `DroidVMVirtioNet.c`, `DroidVMVirtioSnd.c` |
| CPU frequency | `runtime/bridges` | a plain MMIO window per vCPU at `0x09000000` (`/arm-io/cpufreq`) | `DroidVMCpufreq.c` |

A task trap that no page-in can resolve is logged by xnuhost itself with the registers (`tasks: pid N (tid T):
trap kind K signal S far ... esr ... insn ...; pc ... lr ... sp ... fp ... x0 ...`): a fault address outside what an
iOS task may use (below 4 GiB or at and above the stub region), an undefined instruction, a breakpoint; the first 64
of a run. It shows a process's fatal fault when the kernel does not log that process (`sheng-exc-name`).

Read-only images are checked on read (`imageset.c`, `xh_verify_range`): each 1 MiB chunk is read whole and its
SHA-256 compared with `<image>.hashes` the first time a read touches it. The chunk is read and hashed outside the
image's lock, in a buffer of the reading thread, so several vCPUs check different chunks at once (the guest reads
the 5.8 GiB shared cache on all of them before its first process runs); the lock covers only the checked bits. The
hash uses the CPU's SHA-256 instructions when `AT_HWCAP` has `HWCAP_SHA2`, plain C otherwise. Tested: `make test`
(host build in plain C, the arm64 build's test image under `qemu-aarch64 -cpu cortex-a710`, which has SHA2); not
measured on the phone.

The bridges are linked in as `libonebridges.a` and come after the GPU in the window order: `bridge`, `net`,
`virtio-snd`. They are on in a takeover session and off in `--mode bench` (`--set bridges=0|1`). With them,
`/cpus` gets the host's cluster types (E vCPUs first), each vCPU thread is put on one host CPU of its cluster
(`cpu-pin=1`, section 2.6a; `cpu-pin=0`: any CPU of its cluster), and so is the iOS thread a vCPU runs (when the vCPU
changes). The speaker's output latency (since 2026-11-15, fix-audio-latency): once virtio-snd has the AAudio helper,
xnuhost asks it for its measured latency before the guest boots (`br_audio_probe_latency`, at most 2 s: the helper
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
a stand-in controller (`runtime/bridges/bt/hci_sim.c`) behind the same bridge channel and the same `/bluetooth` node
(transport-encoding 1, the made-up address of `/chosen mac-address-bluetooth0`); with the bridges off xnuhost makes a
bridge device that serves only it. One log line says which:
`bluetooth: on: the phone's controller through its HAL 1.1, address ...`, `bluetooth: on: a stand-in controller
(bluetooth=sim), address ...` or `bluetooth: off (...)` with the reason. Boot arguments whose `owner-hide-vmm=` list
names bluetoothd get an error line (`boot arguments: owner-hide-vmm names bluetoothd: ...`) and are left as they are:
24A446's bluetoothd crashes in a loop with the virtual machine hidden ([hardware.md](hardware.md) 4). So do boot
arguments that give bluetoothd `bt-transport-override=10`, read as bluetoothd reads it (`boot arguments:
bt-transport-override=10 gives bluetoothd its own simulated controller, in its own process: ...`; since 2026-11-15):
that transport is a simulated controller inside bluetoothd, and nothing reaches the phone's
([hardware.md](hardware.md) 4, "The virtual transport is bluetoothd's own").
Haptics (`haptics=` in `xnuhost.conf`, `--set haptics=...`; 2026-11-13): `auto`, the default, opens the phone's
force-feedback device (`haptics: on: /dev/input/eventN ...`) when the bridges are on; `off` gives iOS no actuator
(`haptics: off (haptics=off)`: no Actuator audio device, no `/product/haptics`); `sim` (a boot with no vibrator,
the PC) is a stand-in actuator (`runtime/bridges/haptics/haptic.c`) that takes, counts and logs every request
(`haptics: on: a stand-in actuator (haptics=sim)`), behind the same bridge channel; with the bridges off xnuhost
makes a bridge device that serves only the stand-ins (with `bluetooth=sim` one device serves both). Each played
effect is logged for the first 8, then at 16, 32, ...; `haptics: counters: ...` at those points and at the end
([hardware.md](hardware.md) section 5).
The network (`wifi=` in `xnuhost.conf`, `--set wifi=...`): `auto`, the default, makes the phone's wlan0 the guest's
en0 (`br_wlan_prepare`, [hardware.md](hardware.md) section 3): the `net` window's virtio-net device carries wlan0's
address in its configuration, its frames go to and from wlan0 by AF_PACKET, and the guest driver's station messages
(frames of type 0x88b5) go to nl80211 and the vendor's supplicant. It keeps what it changes on wlan0 (GRO,
`disable_ipv6`) in `/data/session/wifi.state`. No wlan0: one line (`wifi: off: no wlan0 ...`) and no `net` window.
`off` makes no network device; `tap` is the old route (a tap and `bin/oneios-net.sh`'s NAT), for a bench; `sim` is a
made-up radio for a boot with no phone (it also works with the bridges off): two networks, "OneiOS open" and "OneiOS
WPA2" (passphrase `password`), that the guest can scan and join, and no frames (`runtime/bridges/wifi/sim.c`).
A bridge whose Linux side is missing logs one line and is left out. At a stop the motor is stopped, the Wi-Fi
bridge leaves its network and gives wlan0's settings back (`br_wlan_close`), and with `wifi=tap`
`bin/oneios-net.sh down` removes the tap's rules. Every virtio node carries its SPI as `interrupts` and as
`droidvm,intid` (the name UniXNU-droidvm's transports read).

Where UniXNU's RamBlock keeps the Data seed in RAM and changed pages in a store window, xnuhost has
Linux's page cache and a sparse file: the store is a raw image.

**Disks: where a request is served** (`virtio_blk.c`; fix-widget-kill-loop, 2026-11-15). The guest's driver
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
iOS flushes and how long the phone's storage takes for it. A second, coarser line comes from `blk.c` itself: every 30 s
per disk with requests, `disk NAME: 30 s: reads R (M MiB), writes W (M MiB), flushes F in T ms (max S ms)`, at the stop
the same `since the start`, and the first 16 flushes of 500 ms or more as they happen (`disk NAME: a flush took N ms`).
The `blk.c` flush counts and times count every `fdatasync`, whichever side (vCPU or flush thread) ran it.

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
  answered when the helper sends `done {"XHDA", seq low, seq high, flags, 0}` after posting the buffer, so the guest
  does not draw into a surface that is still being read (`virtio_gpu.c` holds the reply on the fence list, as the
  display's: ctx 0). A helper that does not answer within 100 ms is answered for. One that leaves, or a newer
  helper, has its frames answered at once. `done` flags bit 0 (a frame it could not copy) turns direct frames off
  for that helper. 2D resources, a scanout outside RAM or of another format, and a helper without the flag use the
  memfd copy as before. `display-direct=0` (command line or `xnuhost.conf`) turns direct frames off for xnuhost.
  Every 10 s with frames the log has `display: 10 s: N frames copied by the helper from guest RAM (posted to in
  the layer: mean X ms, max Y ms), T not answered within 100 ms, C flushes of xnuhost's own copy`. Why not zero
  copies: surfaceflinger takes `AHardwareBuffer`s, and the NDK cannot wrap a udmabuf of guest pages as one (a
  gralloc handle would have to be forged), and the guest's surfaces cannot be made gralloc buffers either, since the
  guest allocates them in its own RAM. Droid-VM's host does one GPU blit instead, with a Vulkan dma-buf import that
  needs the Turnip driver its app ships (`plans/DISPLAY_PIPELINE_DECOUPLING.md`); this phone's own Vulkan driver is
  not known to import a udmabuf. Tested on the PC (`tests/test_display_mode.c` with the helper's `xhd.c` on the
  other end, `tests/test_display.c`, the held flush in `test_xnuhost.c`); not yet run on the phone.
  switch on One UI's side; its README has the phone test). Not run on a phone yet.
- **The frame's pixels** (`display-format=`, `--display-format`; `include/xnuhost/pixels.h`, which the helper
  includes too):

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
  `display-capture.py` (PC) reads format 1 only. Host-tested (`make test`: the 2D and blob paths, a pixel split
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
  4. 1440x3120 at 120 Hz, also what a bench run (whose host display is a monitor) assumes, without waiting.

  The log says which: `display: the mode is the configured one ...`, `display: the display helper reports the panel at
  1440x3120, 120 Hz`, `display: the built-in panel's largest DRM mode is ...`, or `... were not read (...): 1440x3120 at
  120 Hz assumed`, then `display: shm, guest frame 1440x3120 at 120 Hz, @3x (480x1040 points)`. The stride of the frame
  is the width times 4 (5760 bytes; 8 with `display-format=rgba16f`); the `stride` in the helper's `first frame posted` line is the layer buffer's, in
  pixels, and is whatever gralloc aligned the width to. Tested on the PC against a stand-in helper
  (`tests/test_display_mode.c`); not yet run on the phone.
- **The UI scale** is `scale`: 3 (default, 480x1040 points) or 2 (720x1560 points), from `--scale`/`--set
  scale=`, else `xnuhost.conf` of the image set, else its `overlay/userland/xnuhost.conf` (the userland hook's),
  else 3. The pixels are the same either way. One value goes to every place that states it:

  | Place | What reads it |
  |---|---|
  | `/chosen display-scale` (and `display-rotation` 0) | MobileGestalt's `main-screen-scale`; the display driver checks it |
  | `/product artwork-scale-factor` | the artwork scale (the identity tree's 3 is replaced) |
  | `/chosen/xnuhost display-width`, `display-height`, `display-scale`, `display-hz` | the mode as xnuhost has it, for the hosted kernel |
  | boot arguments `offline-legacy-unit-scale=N`, `sheng-vsync=HZ` | the display driver: its scale policy; the vsync tick and the refresh properties. Added unless the command line has them |
  | `/chosen text-fixups` | for 3 only, and only for build 24A446: two fixups of QuartzCore's `-[CADisplayMode preferredScale]`, which answers 2 on its own (`kernel/patches/oneios-hosted/display/README.md` "Scale"). `--set scale-fixup=0` leaves them out; `--text-fixup FILE,OFFSET,ORIGINAL,REPLACEMENT` (repeatable) adds others |

  `/product artwork-device-subtype` stays the identity's (2868). The Data seed's `identity.expect` holds the
  scale the image was built with; a `--set scale=` that differs from the image's makes `oneios-mg` report it.
- **`drm`.** Direct KMS: DRM master on `/dev/dri/card0`, a dumb buffer, `SETCRTC`, `DIRTYFB` per flush. For a
  session in which surfaceflinger and the composer are stopped. Written against the kernel's UAPI, never run.
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
    frame F ms after the touch-down` (the end is `xh_input_frame_shown`, called by `xh_display_flush`).
  - The filter (`xh_input_feed`) is portable and has its own host test (`test_input_filter`): the events that pass, the
    scan time, the resync of slots and keys after `SYN_DROPPED`, the still-screen timing.
- **The touch IC's power state** is the brightness bridge's (`runtime/bridges/brightness/README.md`, "The touch IC
  follows the display"): Samsung's IC is off while the panel is blank and One UI's input HAL, which wakes it, is
  not there in a session. The bridge writes `/sys/class/sec/tsp*/enabled` (`sec_touchscreen` only): `2,0`, `2,1`
  at its start, at every iOS display on (after the backlight), at every press of the power key (input.c calls
  `xh_input_power_key`, which main.c sets to the bridge's `br_bright_key`), and at the stop; `1,0`, `1,1` before the
  backlight when iOS turns its display off (its framebuffer's power request, `OB_BRIGHT_POWER`, hosted `0069`). A
  brightness of 0 no longer sleeps it (since 2026-11-14). `touch-sleep=0` (`--set touch-sleep=0`, or the
  image's `xnuhost.conf`) keeps the IC awake at a display off (it sets `ONEIOS_TSP_OFF=0` for the bridge). After a SIGKILL of xnuhost
  the IC is woken by One UI's own input HAL when the return brings the framework back.
- **`--input-script FILE`** (`input-script=FILE`; `src/input_script.c`, whose head has the format): touches and
  key presses from a text file, on the same virtio-input device, for a host with no touchscreen (the PC under
  qemu) and for repeatable checks. `abs` lines give the device its axes before the guest starts (a device
  without `ABS_MT_POSITION_X/Y` is not a touch screen to the guest), `ready` waits for the guest's driver
  (`xh_virtio_ready`: DRIVER_OK), then `tap`, `swipe`, `key`, `ev`, `wait`, `repeat`.
  `tools/input-demo.script` is a 1440 x 3120 panel with a tap, a swipe from the bottom edge and the three side
  keys, repeated. It runs beside the evdev reader, or alone with `--no-input`. Its events go through the panel's filter
  (`xh_input_feed`): each frame's end gets `EV_MSC` / `MSC_TIMESTAMP` with the time it is sent, and each scripted
  gesture gets the `input: touch:` line with its touch-to-frame time, so the latency can be measured without a finger.
  The filter passes the three side keys, and Home (`KEY_HOMEPAGE`) and the ringer (`KEY_MUTE`) for a host that has them.
- **`session/ready` (the ready rule, since 2026-11-15)** is created at the guest's first frame of its own **that
  shows something**, or `ready-content-wait` seconds (default 120, `--set ready-content-wait=N`, 0 to 480, or the
  image's `xnuhost.conf`) after its first frame of its own when every frame was black, or by the `ready` host call.
  The guest's display driver also presents the boot framebuffer (the console, and later a blank or CPU-copied frame
  drawn there): a flush of a resource whose backing starts at `boot_args.Video`'s address is shown but never counts,
  lit or not (`virtio_gpu.c` `frame_shown`). "Shows something" is `xh_pixels_lit` (`include/xnuhost/pixels.h`): a grid
  of every 16th pixel of every 16th row (90 x 195 = 17,550 pixels at 1440x3120, not the 18 MB frame) with one of R, G,
  B above 16 of 255 (half floats through the clamp to 8-bit sRGB); a lit 16x16 area is always seen. The check runs on
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
  screen and `oneios-tool` look for. Host tests: `test_pixels_lit` (an all-black frame with and without the X byte,
  a console's rows of text, one lit pixel on the grid, one off it, the level at and above 16, a 16x16 block at
  every place, half floats), `test_ready_rule` (lit at once, black then lit, the timeout by a frame and by the tick,
  0, the host call, once only, the key's range) and `test_virtio_gpu` (the console lit and not counted, the guest's
  black frame asked before its flush and asked again, a lit pixel, no question once ready).
  The main loop's copy of the boot framebuffer ends at the guest's first frame of its own, as before (it used to end
  at ready, which was that frame).
  **PC boot (2026-11-15, `--display shm`, qemu-aarch64, kernel.kc bf31eb0d, the PC image set, a seeded Data store,
  `display-capture.py` beside it):** `display-capture.py`'s first picture, at +0.5 s, is the empty memfd, black: what
  the display helper posted before this fix, a second after it connected, over the boot screen. Then 2742 flushes
  of the black boot framebuffer (not counted). backboardd opened the framebuffer at +534.7 s, swap 1 at +576.2 s,
  `guest ready: first frame (it has content)` at +576.3 s, and the capture's next picture (577.1 s after its hello,
  26 colours) is the Apple logo on black. Under qemu the guest's first frame is already the logo, so the old
  rule and the new one were ready at the same frame there; the black frames before the logo are the phone's (its
  frames come through the GPU's blobs). The timeout path has run in the host tests only.

- **The pen** (`src/pen.c`, spen 2026-11-15; `docs/hardware.md` 7f). Settings: **`pen=off`** (the default until a phone
  session proves it) or `pen=auto`; `pen-rotate=auto|0|90|180|270` (the digitizer's frame turned clockwise onto the
  panel's; auto: 0 when the digitizer and the panel are both portrait, else 90, a guess); `pen-mirror=1` (then mirrored
  left to right). With `pen=auto`, xnuhost (with `input` on) calls `xh_pen_lookup`: the default, `xh_pen_lookup_evdev`,
  opens each `/dev/input/event0..63` read only and takes the best device with `BTN_TOOL_PEN`, `ABS_X/Y` and
  `ABS_PRESSURE` (more for `ABS_TILT_X`, `BTN_STYLUS`, `ABS_DISTANCE` and a name with "pen" in it, such as Samsung's
  `sec_e-pen`); never one with a side key or multitouch positions; never grabbed. Another hardware backend replaces the
  lookup by setting `xh_pen_lookup` to its own function (same signature; nothing else names a Samsung device). When a pen
  is found, or the input script has `pen-` lines, the board gets a **second virtio-input device, `/arm-io/input-1`**,
  named "OneiOS pen" (`BTN_TOOL_PEN`, `BTN_TOOL_RUBBER`, `BTN_TOUCH`, `BTN_STYLUS`; `ABS_X/Y` 0..32767, `ABS_PRESSURE`
  0..4095, `ABS_DISTANCE` 0..255, `ABS_TILT_X/Y` -90..90 degrees; `INPUT_PROP_DIRECT`), and `oneios-pen=1` is added to
  the guest's command line unless it has an `oneios-pen=` (so `--boot-args "... oneios-pen=0"` keeps the guest's side
  off). The pen's descriptor is read on input.c's thread beside the others; its filter (`xh_pen_feed`) turns each evdev
  frame into one sample in those units (in range, tip, side button, eraser end; pressure only while the tip is down; a
  hover height never 0, half way for a device without `ABS_DISTANCE`; tilt by its resolution in units per radian, else
  as degrees within +-90, else its range's end as 90; at most +-89), sends only the fields that changed and nothing for
  a frame like the last, and reads the state again after `SYN_DROPPED`. No `MSC_TIMESTAMP` on the pen's device (the
  guest's scan-time code is the touch screen's). Log lines: `pen: /dev/input/eventN "NAME": read only, not grabbed; x
  A..B, y ..., pressure ..., distance ..., tilt x ... (res R); keys: ...; turned D degrees (why)`, or `pen: none found:
  why`; `pen: the guest's pen device is /arm-io/input-1 (the host's pen); oneios-pen=1`; `pen: the first event from
  NAME`; per time in range `pen: in range T ms: F frames (H hovering, C touching), S strokes, pressure up to P/4095,
  tilt x a..b y c..d degrees, the side button pressed B times; U unchanged frames not sent`. The script's lines
  (`pen-abs`, `pen-ev`, `pen-syn`, `pen-hover`, `pen-stroke`, `pen-button`, `pen-out`: the head of `input_script.c`) go
  through the same filter; `tools/input-demo.script` has a hover, two strokes (a pressure ramp; a tilted even one) and
  the side button. Host test: `test_pen` in `tests/test_xnuhost.c`.
- `session/ready` is created at the guest's first presented frame (or by the `ready` host call). The guest's
  display driver also presents the boot framebuffer (the console, and later a blank or CPU-copied frame drawn
  there): a flush of a resource whose backing starts at `boot_args.Video`'s address is shown but is not that
  first frame (`virtio_gpu.c` `frame_shown`).

### 2.9 What xnuhost must never do in a session

The OneOS fork's way back to One UI depends on these (`docs/agents.md`, "booter -> oneos"); they hold today
and any change that would break one goes on the board first:

- no `EVIOCGRAB` on any input device, and never on the side keys; the Volume Up + Volume Down chord is the fork's;
- no thread in a realtime class or above nice -10 (affinity and uclamp only);
- nothing leaves the mount namespace or the session's cgroup; no change to `oom_score_adj`; no signal to a
  process that is not xnuhost's own (the factory kills only its own process group);
- nothing under `/sys/power`, no `reboot(2)`, no `sys.powerctl`: a guest's "back to One UI" is `XH_HALT_RETURN`;
- nothing that needs a clean exit: every helper is a child inside the cgroup and dies with it.

Two things outlive a killed session and are not xnuhost's files: `vm.max_map_count` stays raised until
the phone reboots, and wlan0 keeps GRO off and `disable_ipv6` 1 if xnuhost is killed before `br_wlan_close`
(neither breaks One UI's Wi-Fi: [hardware.md](hardware.md) section 3, "Return"; `bin/wifictl restore
SESSION/wifi.state` puts them back). With `wifi=tap`, the tap's routing rules and iptables chains stay until
`oneios-net.sh down` (the tap itself goes with the process).

### 2.9a The USB gadget in the log (`src/usb.c`, 2026-11-13)

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
`test_xnuhost`'s `test_usb` checks it with a stand-in property source and state file: the first line, nothing
logged or read again while the serials and the file stay, a change of the file alone and of one property, a value
read only through its serial, a property that goes away, a controller name that is a path (`../../../proc/self`:
its file is never opened), and `usb-log`'s default and range. **Not run on the phone yet.**

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
| [DarwinBoot](https://github.com/UniXNU/DarwinBoot) `8544957`: `DarwinBoot.c`, `BootArgs.h`, `PackedRamdisk.h`, `Test/make-fixtures.py` | BSD-2-Clause-Patent | Mach-O and fileset parser, `boot_args`, trust-cache and packed-RAM-disk checks, placement order, the fixture shape | `runtime/loader/src/macho.c`, `boot.c`, `tests/fixture.h` (headers keep the notice) |
| [UniXNU-droidvm](https://github.com/Droid-VM/UniXNU-droidvm) `f9121cd`: `ipados27/scripts/make-droidvm-adt.py` | BSD-2-Clause-Patent | the device tree that boots iPadOS 27.0.1: nodes, properties and the reasons recorded for them | `runtime/loader/src/machine.c` |
| [ArmDTPlatform](https://github.com/UniXNU/ArmDTPlatform) `e8bd8d5`, [ArmCpu](https://github.com/UniXNU/ArmCpu) `1604083` | BSD-2-Clause-Patent | not copied: their requirements define the machine. The tree has what `configure()` demands; the GIC registers ArmCpu reads and writes, timer INTID 27, SGIs and PSCI `CPU_ON` are what `gic.c`, `sysreg.c` and `cpu.c` implement | `runtime/xnuhost/src/gic.c`, `sysreg.c`, `cpu.c` |
| [VirtioMmioBlock](https://github.com/UniXNU/VirtioMmioBlock) `4e1013e` | BSD-2-Clause-Patent | not copied: the register sequence of `NativeVirtioBlock::configureQueue` is the device model's test (in the test kernel and the unit tests) | `virtio.c`, `virtio_blk.c` |
| [RamBlock](https://github.com/UniXNU/RamBlock) `60a54ce` | BSD-2-Clause-Patent | the role (seed + persistent store), done with a sparse file; `DBRDISK1` packed RAM disks are accepted by the loader | `blk.c`, `boot.c` |
| iOSPlus10 `b2ffcac`: `external/slop/shim/loader.c`, `external/slop/tools/adt.py`, `docs/ios26-port-status.md` section 7 | no licence file in slop: nothing copied | the 608-byte `boot_args` layout; the ADT dump tool's behaviour (`xnuloader-inspect adt`); the one-global-key-set PAC model | `boot.h`, `tools/xnuloader-inspect.c`, section 2.6 |
| gVisor systrap, User-Mode Linux skas | design only | the stub, trap pages, service thread | `stub/`, `task.c` |

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
7. **C, not Rust**, as its own program in `runtime/`, not in `android/vphone-native`.
8. **GPU.** The plan's "forward KGSL calls" is dropped: Droid-VM measured the KGSL proxy at half the speed
   and retired it. The proven path on this SoC is virtio-gpu's DRM native context served by drm2kgsl
   (`docs/references.md`); see section 8.

## 6. Tests and what they prove

| Test | Where it runs | Result (2026-11-10) |
| --- | --- | --- |
| `runtime/loader`: `make test` | PC, ASan/UBSan | 150 checks pass: Mach-O and fileset parsing with DarwinBoot's rejected variants, the tree round trip and malformed trees, the board's tree, sidecars, placement in both `boot_args` layouts, hosted and firmware |
| `runtime/xnuhost`: `test_xnuhost` | PC, ASan/UBSan | 149 checks pass: instruction decoding, MMIO emulation, the GIC by registers and by calls, system registers and PSCI, virtio block/GPU/input driven as a guest driver does, the seccomp filter run in a BPF interpreter, configuration, the Data store |
| `make check-arm64` | the Android binary under `qemu-aarch64` | the test kernel's checks pass (98 on 2026-11-14, with `test_jit_icache`) |
| cross-build | NDK 28.2, `aarch64-linux-android30`, `-Wall -Wextra -Werror` | clean |

The test kernel (`tests/testkernel.c`) is our own freestanding program wrapped as a Mach-O linked at
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
or `PROT_NONE` page (checked with a probe on 2026-11-14), so under `make check-arm64` the never-touched alias's
first fault is the branch (the test prints a `NOTE`) and the cache maintenance fault path runs only on arm64
hardware (the phone, or `check-arm64` on an arm64 Linux host, which runs the binary without qemu). The decision
itself (an ESR with CM is a read, whatever WnR says) is checked on the host by `test_fault_access` in
`test_xnuhost` with the hardware's syndromes, and on the kernel's side by `hosted_selftest=1` (0071).

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
  it does, the fill fails and `task.c` logs `tasks: pid N: a deferred map (va 0x..., prot 7) failed`; that is the
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
   guest side UniXNU-droidvm's `DroidVMVirtioGpu3D.cpp` and the Metal plugin. `virtio_gpu.c` is 2D only.
8. Then: the bridges' first run on the phone (network, sound, haptics, Bluetooth are wired and untested),
   battery, a guest-initiated shutdown on `SIGTERM`, the uid drop and trap batching.
