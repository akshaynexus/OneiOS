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
        [--config FILE] [--set KEY=VALUE] [-v]
xnuhost --selftest            # on the phone: page size, address bits, PAC keys across fork, memfd, seccomp
```

| Input | Default | What |
| --- | --- | --- |
| `--image DIR` | required | the image set (`docs/image-format.md`) |
| `--kernel FILE` | `DIR/kernel.kc`, else `DIR/kernel.macho` | the kernel collection: `MH_FILESET`, or an `MH_EXECUTE` kernel |
| `FILE.trustcache`, `.vroot`, `.gpuaddon`, `.ramdisk`, `.adt` | optional | DarwinBoot's sidecars beside the kernel, same formats and checks |
| `DIR/identity.adt` | optional | the device's own decoded device tree: model, target-type, board name and `/product` are taken from it. No script writes one (2026-11-13: `scripts/image/build.sh` does not, and the PC's set `out/image-pc/24A446` has none), so `/product` is only `product-name`, `artwork-scale-factor` and `model`: none of the iPhone's own `/product` (`artwork-display-gamut P3`, `oled-display`, `graphics-featureset-class APPLE10`, `ui-background-quality 100`, ...), which MobileGestalt answers from. `--set identity=out/kernel-inputs/24A446/devicetree.bin` (the decoded tree as it is) gives them for one run (`kernel/patches/oneios-hosted/display/README.md`, "What iOS reads about the panel") |
| `--display-format bgrx\|rgba16f` (`display-format=`) | `bgrx` | the frame's pixels (section 2.8): 8-bit sRGB, or half floats that keep wide colour and EDR |
| `DIR/xnuhost.conf` | optional | `key=value` lines: `boot-args`, `disk`, `disk-rw`, `data-store`, `scale`, `scale-fixup`, `text-fixup`, `ram`, `ram-percent`, `ram-host-floor`, `host-mem-log`, `ram-giveback`, `ram-prefault`, `ram-lock`, `ram-session-reserve`, `ram-unlock-below`; the command line wins |
| `DIR/overlay/userland/xnuhost.conf` | optional | the same, written by the userland hook: `scale=3` or `scale=2`, the UI scale the image's Data seed was made for (`docs/userland.md` "The screen") |
| `--width W --height H`, `--hz N` (or `width=`, `height=`, `hz=` in the image set's `xnuhost.conf`) | the display helper's report of the panel | the guest's display mode (section 2.8); for a PC run or another panel |
| `--scale 2\|3` (`--set scale=2`) | the image set's, else 3 | the UI scale: pixels per point |
| `DIR/os.img`, `systemos.img`, `app.img` | as present | read-only disks, in this order |
| `--data DIR` | none | `data.img`: the Data store, sparse, created on the first start; `unique-chip-id` |
| `DIR/data.seed` | as present | a tar seed is served as the last read-only disk (the guest makes the Data volume and unpacks it); an APFS seed image is copied into a new store |
| `--session DIR` | none | `xnuhost.pid`, `ready`, `return.request`, `display.sock` |
| `--cpus` | the host's cores, at most 8 | 1 to 8 |
| `--ram SIZE` or `--ram auto` (`ram=`) | `auto` | the guest's RAM: a multiple of 2 MiB, 256 MiB to 64 GiB. `auto`: the least of `ram-percent` of the host's `MemTotal` and `MemTotal` less `ram-host-floor`, rounded down to 256 MiB (below) |
| `--set ram-percent=N` | 90 | `ram=auto`'s share of `MemTotal`, 10 to 100 |
| `--set ram-host-floor=SIZE` | 1.5G | what `ram=auto` always leaves the host |
| `--set host-mem-log=SECONDS` | 30 | a `memory: host MemAvailable` line every so often; 0 = only at the start and the stop |
| `--set usb-log=SECONDS` | 3 | how often the phone's USB gadget is read for its `usb:` line (section 2.9a), which is logged at the start, on every change and at the stop; 0 = off |
| `--set ram-meminfo=FILE` | `/proc/meminfo` | where `ram=auto` reads `MemTotal` and `MemAvailable` (tests) |
| `--set ram-giveback=0\|1` | 1 | 0: the RAM is **held fixed for the session** (section 2.2, "Fixed RAM": prefaulted, locked where Linux allows, no free pages given back). 1: 0058's way (faulted in as used, free pages given back). Off by default until it has run on a PC boot of iOS and on the phone |
| `--set ram-prefault=0\|1`, `ram-lock=0\|1` | 1, 1 (with `ram-giveback=0`) | prefault the whole memfd on a nice-19 thread; `mlock` it while prefaulting (all or nothing) |
| `--set ram-session-reserve=SIZE` | 1G | with `ram-giveback=0`: `ram=auto` takes no more than `MemAvailable` at the start less this, and the lock is tried only if the RAM fits in it |
| `--set ram-unlock-below=SIZE` | 256M | with the RAM locked: the host's `MemAvailable` under this (read once a second) unlocks it once for the rest of the session; 0 = never |

**The guest's RAM (`ram=auto`, the owner's decision of 2026-11-12: iOS gets at least 90% of the phone's RAM).**
In a session One UI's apps and system_server are gone, so the host needs little: the guest gets
`min(ram-percent% of MemTotal, MemTotal - ram-host-floor)`, rounded down to 256 MiB. On a 12 GB phone (MemTotal
about 11.2 GiB, an assumed figure until the first log line from the phone) that is the floor's 9.5 GiB (9728 MiB); 90%
would be 10.0 GiB. A host where that leaves the guest less than 1 GiB is refused (exit 2, "give --ram SIZE"); a host
with no `/proc/meminfo` gets the old 4 GiB. By default the RAM is a memfd that is never populated or locked (`ram-giveback=0` holds it fixed instead: section 2.2, "Fixed RAM"): the host gives it
pages only as the guest touches them (`ram_discard` gives them back). The log says what was chosen, and then what the
host has left:

```text
memory: ram=auto: host MemTotal M MiB, MemAvailable A MiB; the guest gets G MiB: the least of 90% (.. MiB) and MemTotal less the host's floor of 1536 MiB (.. MiB), in 256 MiB steps
memory: host MemAvailable A MiB of M MiB; the guest's RAM holds H of G MiB; given back N MiB of free pages in C calls (F failed, T ms)
memory: the guest's RAM: R MiB in the host's RAM, W MiB swapped out by the host; host swap S of Z MiB used, page tables P MiB
                                                                                     every host-mem-log seconds, and at the stop
```

The second line (2026-11-13): `R` is the memfd's pages in the host's RAM (`mincore` over xnuhost's mapping,
`xh_ram_resident` in `src/discard.c`, 64 MiB per call), `W` = `H` - `R`: pages the guest touched that Linux swapped
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
  loop shows it until the guest is ready. The guest's display driver maps it too (`kernel_map` takes any page of
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
- **Fixed RAM (`ram-giveback=0`, `src/ramfix.c`, fix-fixed-memory, 2026-11-13; off by default).** The owner's requirement:
  iOS's RAM must not grow and shrink during a session. The phone's c2d281a session showed why: the memfd was faulted in
  bit by bit (4,904 -> 6,010 -> 6,254 MiB), 527 MiB of free pages given back in 22,688 calls, then 669 MiB of it swapped
  out by the host (zram) with the host's MemAvailable at 842 MiB: every step a fault in a vCPU or a task. With
  `ram-giveback=0`:
  - xnuhost sets `XH_FEATURE_RAM_FIXED` in `xh_info.features` and refuses `ram_discard` (-EPERM, counted). A kernel
    without 0067 stops giving back after the first refusal (0058's "free pages are no longer given back"); 0067 (written,
    not yet in the series) never asks and keeps 0058's free-queue order (freed pages first: it costs nothing, and where the
    lock fails it keeps iOS on pages the host has kept resident).
  - A thread `xnuhost-ram` (SCHED_OTHER, nice 19, never realtime) prefaults the whole memfd in 16 MiB steps from the
    moment it is mapped, while the boot goes on: `mlock` of each step (which faults it in and locks it), or, when the
    lock is off or failed, one read per host page (a read of a hole through the shared mapping allocates a zeroed page;
    not `fallocate`, whose pages stay "not up to date" and are not counted by `mincore`). The lock is all or nothing: on
    the first failure (EPERM: no CAP_IPC_LOCK; ENOMEM: RLIMIT_MEMLOCK or no memory) what was locked is unlocked, the
    rest is prefaulted unlocked, the errno is logged, the boot goes on. The lock is tried only if the RAM fits in the
    host's MemAvailable less `ram-session-reserve`.
  - `ram=auto` adds a third bound: `MemAvailable` at the start less `ram-session-reserve` (1 GiB: the iOS tasks' page
    tables, KGSL's own memory, the display helper and the daemons that stay), so a RAM held from the start fits beside
    what the host keeps running. The phone's c2d281a numbers suggest the host needed far more than the 1.5 GiB floor
    beside a 6.3 GiB guest [inference: the session's MemTotal and start MemAvailable were not in the notes]; the next
    phone boot's `ram=auto` line says which bound decides.
  - The valve: while locked, the thread reads MemAvailable once a second; under `ram-unlock-below` (256 MiB) it unlocks
    the RAM once (the host may swap it again, today's way; nothing is given back) and logs it.
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
    then `memory: the guest's RAM is held: N MiB prefaulted in T ms, locked (mlock: the host cannot swap it)|not locked
    (mlock: ERRNO; the host may swap it)`, and every host-mem-log seconds a third `memory:` line: `memory: fixed RAM:
    held and locked; prefaulted N of N MiB, locked L MiB (VmLck; host Mlocked M MiB); give-back refused C calls; since the
    last line: host swap-ins +I, swap-outs +O pages, xnuhost major faults +F[; KGSL: name value, ...]` (KGSL's files under
    `/sys/class/kgsl/kgsl/proc/PID`, as KGSL prints them: the host's own GPU memory for xnuhost). At the stop:
    `memory: the guest's RAM: N MiB held to the end, L MiB of it locked; the exit gives all of it back to the host`.
  - Tested: the host tests (`test_ramfix`: prefault keeps every byte, a lock failing part way with ENOMEM and at once
    with EPERM falls back and unlocks, a real lock within the PC's 8 MiB RLIMIT_MEMLOCK shows in VmLck and the valve
    undoes it, past the limit a clean fallback, the thread, the refusal, the return; the sizing in `test_config`), and
    the test kernel under qemu-aarch64 (`make check-arm64`'s second boot runs with `ram-giveback=0`: the refusal, the
    prefault, the sizing line; on the PC mlock fails with ENOMEM: RLIMIT_MEMLOCK 8 MiB, no root). **Not run:** a PC
    boot of iOS with it, the kernel 0067, the phone (where xnuhost is root: the lock itself).
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
registers, `struct xh_cpu`, `struct xh_vector` (31 calls; additions are appended and `vector->size` says which exist), `struct xh_upcalls`, `struct xh_info`, frames
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
  interface by trapped `ICC_*` registers, or the `irq_ack/irq_eoi/irq_enable/ipi` host calls.

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

`runtime/bridges/thermal` reads the phone's temperatures every 5 s from the start. At heavy pressure and above it
gives a step (1-3), and `main.c`'s `thermal_step` narrows the vCPU threads' affinity (`br_cpu_plan_cap`: first the X4
is left out, then only the slowest P domain and the E cores, then the E cores only) and bumps `xh_cpu_place_epoch`, so
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

### 2.7 Devices

All devices are virtio-mmio version 2 windows (`virtio.c`: split queues, indirect descriptors, the status
and feature handshake) at `0x0a000000 + n * 0x1000`, SPI `32 + n`, in this order: the disks, input, GPU.

| Device | File | Backed by | Guest driver it is written for |
| --- | --- | --- | --- |
| Block, read-only (`F_RO`) | `virtio_blk.c`, `blk.c` | `os.img`, `systemos.img`, `app.img`, a tar `data.seed` | UniXNU `NativeVirtioBlock` (node `virtio-mmio-N`, `compatible` `virtio,mmio`) |
| Block, writable | same | `<data>/data.img`, sparse, 32 GiB logical, flush = `fdatasync` | the Data store window (node `data-store`) |
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
changes). Bluetooth (`--bluetooth`,
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
24A446's bluetoothd crashes in a loop with the virtual machine hidden ([hardware.md](hardware.md) 4).
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

### 2.8 Display and input: the choice

- **Default `shm`.** In a OneOS takeover session One UI's surfaceflinger and composer keep running and
  hold the panel (`oneos/interface.md` section 4), so KMS is not available and the only way to the glass is
  a root SurfaceControl layer, which needs the framework's classes (OneOS draws its boot status that way,
  with a dex run by `app_process`). xnuhost therefore keeps the frame in a memfd and serves it on
  `<session>/display.sock` (`SOCK_SEQPACKET`): `hello {"XHD1", width, height, stride, format}`
  with the memfd as `SCM_RIGHTS`, then `flush {"XHDF", x, y, w, h}` per changed rectangle. The helper sends one
  message the other way, straight after it connects: `panel {"XHDP", width, height, hz, 0}`. The helper that
  owns the layer and copies from the memfd is `runtime/display` (`bin/oneios-display`, started by the fork's
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
  at its start, after the backlight when iOS's level leaves 0, and at the stop; `1,0`, `1,1` before the backlight
  when the level becomes 0. Without it touch is dead after iOS's first display sleep. `touch-sleep=0` (`--set touch-sleep=0`, or the
  image's `xnuhost.conf`) keeps the IC awake at level 0 (it sets `ONEIOS_TSP_OFF=0` for the bridge). After a SIGKILL of xnuhost
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
| `make check-arm64` | the Android binary under `qemu-aarch64` | the test kernel's 59 checks pass |
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
memfd for a task is **not** covered here; see section 7.)

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
