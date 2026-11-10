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
```

Command line (the first line is what the OneOS fork runs, `oneos/interface.md` section 4):

```sh
xnuhost --session-abi 1 --mode takeover --runtime DIR --image DIR --data DIR --session DIR
        [--kernel FILE] [--cpus N] [--ram SIZE] [--boot-args TEXT] [--display shm|drm|none]
        [--width W --height H] [--no-input] [--no-tasks] [--config FILE] [--set KEY=VALUE] [-v]
xnuhost --selftest            # on the phone: page size, address bits, PAC keys across fork, memfd, seccomp
```

| Input | Default | What |
| --- | --- | --- |
| `--image DIR` | required | the image set (`docs/image-format.md`) |
| `--kernel FILE` | `DIR/kernel.kc`, else `DIR/kernel.macho` | the kernel collection: `MH_FILESET`, or an `MH_EXECUTE` kernel |
| `FILE.trustcache`, `.vroot`, `.gpuaddon`, `.ramdisk`, `.adt` | optional | DarwinBoot's sidecars beside the kernel, same formats and checks |
| `DIR/identity.adt` | optional | the device's own decoded device tree: model, target-type, board name and `/product` are taken from it |
| `DIR/xnuhost.conf` | optional | `key=value` lines: `boot-args`, `disk`, `disk-rw`, `data-store`; the command line wins |
| `DIR/os.img`, `systemos.img`, `app.img` | as present | read-only disks, in this order |
| `--data DIR` | none | `data.img`: the Data store, sparse, created on the first start; `unique-chip-id` |
| `DIR/data.seed` | as present | a tar seed is served as the last read-only disk (the guest makes the Data volume and unpacks it); an APFS seed image is copied into a new store |
| `--session DIR` | none | `xnuhost.pid`, `ready`, `return.request`, `display.sock` |
| `--cpus`, `--ram` | 4, 4G | 1 to 8; a multiple of 2 MiB |

Log: standard error (the switch appends it to `session/xnuhost.log`). Lines `xnuhost I/E/D` are the host's,
`guest` lines are the kernel's console, `xnuhost !!` lines come from signal context.

Stop: `SIGTERM`, `SIGINT` or `SIGHUP`: tasks are killed, the Data store is synced, exit 0. Today the guest
is not asked to shut down first (there is no guest to ask); APFS is crash-consistent, the store is synced.

| Exit code | Meaning |
| --- | --- |
| 0 | the guest shut down (halt, PSCI SYSTEM_OFF), a stop signal, or a return asked from inside (then `session/return.request` exists) |
| 2 | bad arguments or a failed setup (no kernel, bad image, no memory) |
| 3 | the guest asked for a restart (PSCI SYSTEM_RESET) |
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
  sets), `/buttons`, `/bluetooth`, `/arm-io` with the GICv3, `wlan`, `uart0` and one node per virtio window
  under the names and `compatible` strings UniXNU's drivers match; `/chosen/xnuhost` for a hosted kernel.
- `boot.c`: `boot_args` (1024-byte command line of Darwin 23+, or the 608-byte one), the sidecar checks
  (trust caches, packed RAM disk `DBRDISK1`, GPU addon, root policies) and the placement in DarwinBoot's
  order: kernel, `boot_args`, tree, RAM disk, trust caches, addon, policies; memory-map entries filled.

The difference from a firmware loader is one parameter: `virt_base`. DarwinBoot passes the kernel's link
base and the kernel maps itself there. xnuhost passes the address where the window **already is**, a
multiple of 2 MiB away from the link base, which is the slide the kernel finds when it rebases itself.

### 2.2 Memory

- One memfd is the guest's RAM. xnuhost maps it once at `0x2000000000 + (link base mod 2 MiB)` (retrying
  higher), read-write-execute. **Physical address = that address** (`boot_args.physBase == virtBase`):
  one number names a byte for the kernel's pointers, its physical addresses (DMA addresses in virtqueues,
  page tables it may still write) and, minus the base, the offset a task maps.
- The top of the memfd, outside the kernel's window, is the boot framebuffer (`boot_args.Video`); the main
  loop shows it until the guest's display driver presents.
- The board's device windows are a `PROT_NONE` reservation at their physical addresses
  (`0x08000000..0x0b000000`), so a device register's physical address is also its pointer and every access
  traps.
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
| `tlbi`, `at`, EL1 `dc`/`ic` | nothing |
| any other EL1/EL2 register (and `TPIDRRO_EL0` writes) | a per-CPU shadow: reads return what was written |

The shadow does **not** give a write its hardware meaning: page tables, vectors and PAC key registers
have no effect. Loads and stores on the I/O aperture are decoded (`insn.c`: register, immediate, pre/post
index, pairs, sign extension) and run against the device models.

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
- **Faults, undefined instructions, breakpoints, FP exceptions** arrive the same way as SIGSEGV/SIGBUS,
  SIGILL, SIGTRAP, SIGFPE, with the fault address and Linux's ESR when it gives one.
- **Running a thread.** `thread_run(thread, state, trap)` releases the thread with the given registers (the
  handler returns through `rt_sigreturn`) and blocks the vCPU until the thread traps again: the vCPU "is in
  user mode". If the vCPU's interrupt line rises meanwhile, xnuhost signals the thread and it stops with
  `XH_TRAP_INTERRUPT`. The kernel's side is a loop: run, handle the trap as `sleh_synchronous` would, run.
- **copyin/copyout**: `task_read`/`task_write` (`process_vm_readv/writev`), or the kernel's own walk of its
  pmap to the RAM.
- **The thread register.** iOS reads its TSD base from `TPIDRRO_EL0`, which Linux keeps 0 and user space
  cannot write. The host keeps the base in `TPIDR_EL0` (`xh_user_state.tsd`) and `text_fixup()` rewrites
  `mrs xN, TPIDRRO_EL0` to `mrs xN, TPIDR_EL0` in a page before it is mapped executable (after the kernel
  validated its signature). Risks: a data word in text with that encoding; code that writes `TPIDR_EL0`.
- **Plain arm64 binaries**: `thread_set_pac(thread, 0)` turns the keys off for that thread
  (`PR_PAC_SET_ENABLED_KEYS`).

### 2.7 Devices

All devices are virtio-mmio version 2 windows (`virtio.c`: split queues, indirect descriptors, the status
and feature handshake) at `0x0a000000 + n * 0x1000`, SPI `32 + n`, in this order: the disks, input, GPU.

| Device | File | Backed by | Guest driver it is written for |
| --- | --- | --- | --- |
| Block, read-only (`F_RO`) | `virtio_blk.c`, `blk.c` | `os.img`, `systemos.img`, `app.img`, a tar `data.seed` | UniXNU `NativeVirtioBlock` (node `virtio-mmio-N`, `compatible` `virtio,mmio`) |
| Block, writable | same | `<data>/data.img`, sparse, 32 GiB logical, flush = `fdatasync` | the Data store window (node `data-store`) |
| Input | `virtio_input.c`, `input.c` | `/dev/input/event*`: the touchscreen's multitouch events and the three side keys, unchanged evdev codes, the panel's own axis ranges | UniXNU-droidvm `DroidVMVirtioInput.c` |
| GPU, 2D scanout | `virtio_gpu.c`, `display.c` | the display back end | UniXNU-droidvm `DroidVMVirtioGpuDisplay.cpp` under IOMobileFramebufferLegacy |
| Console | `uart.c` | the log | XNU's PL011 console (`uart0`) |
| Bridge channel, network, sound | `runtime/bridges` (the bridges agent's; `docs/hardware.md`) | haptics and Bluetooth HCI; a tap with built-in DHCP; an AAudio helper process | `drivers/bridge`, `DroidVMVirtioNet.c`, `DroidVMVirtioSnd.c` |
| CPU frequency | `runtime/bridges` | a plain MMIO window per vCPU at `0x09000000` (`/arm-io/cpufreq`) | `DroidVMCpufreq.c` |

The bridges are linked in as `libonebridges.a` and come after the GPU in the window order: `bridge`, `net`,
`virtio-snd`. They are on in a takeover session and off in `--mode bench` (`--set bridges=0|1`). With them,
`/cpus` gets the host's cluster types (E vCPUs first), each vCPU thread is put on its cluster's host CPUs,
and so is the iOS thread a vCPU runs (when the vCPU changes). `--bluetooth hidl` bridges the phone's
controller (`/bluetooth` transport-encoding 3, the phone's address); the default `virtual` keeps encoding 10.
A bridge whose Linux side is missing logs one line and is left out. At a stop the motor is stopped and
`bin/oneios-net.sh down` removes the tap's rules. Every virtio node carries its SPI as `interrupts` and as
`droidvm,intid` (the name UniXNU-droidvm's transports read).

Where UniXNU's RamBlock keeps the Data seed in RAM and changed pages in a store window, xnuhost has
Linux's page cache and a sparse file: the store is a raw image.

### 2.8 Display and input: the choice

- **Default `shm`.** In a OneOS takeover session One UI's surfaceflinger and composer keep running and
  hold the panel (`oneos/interface.md` section 4), so KMS is not available and the only way to the glass is
  a root SurfaceControl layer, which needs the framework's classes (OneOS draws its boot status that way,
  with a dex run by `app_process`). xnuhost therefore keeps the frame in a memfd and serves it on
  `<session>/display.sock` (`SOCK_SEQPACKET`): `hello {"XHD1", width, height, stride, format 1 = B,G,R,X}`
  with the memfd as `SCM_RIGHTS`, then `flush {"XHDF", x, y, w, h}` per changed rectangle. The helper that
  owns the layer and copies from the memfd is `runtime/display` (`bin/oneios-display`, started by the fork's
  switch on One UI's side; its README has the phone test). Not run on a phone yet.
- **`drm`.** Direct KMS: DRM master on `/dev/dri/card0`, a dumb buffer, `SETCRTC`, `DIRTYFB` per flush. For a
  session in which surfaceflinger and the composer are stopped. Written against the kernel's UAPI, never run.
- **`none`.** Frame in memory only.
- **Input** is read from evdev directly and not grabbed, so the switch's Volume Up + Volume Down watcher
  keeps working beside it.
- `session/ready` is created at the guest's first presented frame (or by the `ready` host call).

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
the phone reboots, and the network bridge's routing rules and iptables chains stay if xnuhost is killed
before it runs `oneios-net.sh down` (the tap itself goes with the process).

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
disk by trapped loads and stores, with a sector read through a virtqueue; `text_fixup`, `ram_discard`; and a
task: a mapped code page, a fault on an unmapped page, the kernel mapping it, the store retried and visible
in the kernel's view, a breakpoint, a looping thread preempted by the timer.

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
- Trap cost is a signal plus two futex hand-offs per system call or fault; not measured.
- `task_map` is one round trip to the service thread per call; no batching in the ABI yet.
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
4. **First run on the phone** of everything in section 6 that qemu cannot run: `--selftest`, the test kernel
   with seccomp, trap cost. Then fix what breaks.
5. **launchd**: verified root over the block devices, the Data volume made by the guest on first start from
   the tar seed, trust caches (`trust/*.trustcache` are IM4P; the loader's sidecar takes DarwinBoot's
   envelope: a conversion at image build or in xnuhost), the shared region, `text_fixup` wired into the
   kernel's page-in.
6. **backboardd**: the display helper (root SurfaceControl layer reading `display.sock`), HID digitizer
   from virtio-input, AppleKeyStoreState and SoftwareAES in the kernel.
7. **SpringBoard**: Metal. virtio-gpu 3D: the DRM native context (capset 6) with drm2kgsl on
   `/dev/kgsl-3d0` in-process, blob resources over the RAM memfd through `/dev/udmabuf`, per-ring fences;
   guest side UniXNU-droidvm's `DroidVMVirtioGpu3D.cpp` and the Metal plugin. `virtio_gpu.c` is 2D only.
8. Then: the bridges' first run on the phone (network, sound, haptics, Bluetooth are wired and untested),
   battery, a guest-initiated shutdown on `SIGTERM`, image chunk-hash checks on
   read (`<image>.hashes`), the uid drop and trap batching.
