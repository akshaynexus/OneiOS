# The display: IOMobileFramebuffer over xnuhost's virtio-gpu scanout

Owner: q4a-framebuffer. Date: 2026-11-11. The guest's display driver, in the hosted kernel: what `backboardd`
opens as the main display, and how its frames reach `session/display.sock`.

**State, plainly.** It builds into the hosted kernel and starts at boot under `qemu-aarch64`: the class registers
under the collection's `IOMobileFramebufferLegacy`, the service starts on the boot video, IOSurface makes its
default surface, `/chosen display-scale` is 3, and the virtio-gpu scanout presents. What `backboardd` then does
with it is in "What ran" below. Nothing here has run on the phone.

## What it is made of

Nothing new was written for the framebuffer itself. It is the display Droid-VM runs on iPadOS 27, bound to the
iPhone18,2 kernelcache and to xnuhost:

| Piece | From | What |
|---|---|---|
| `NativeLegacyDisplay.cpp`, `NativeGOPPresenter.cpp` | `external/NativeMobileDisplay` (UniXNU), unchanged | A subclass of the original `IOMobileFramebufferLegacy` of `com.apple.iokit.IOMobileGraphicsFamily`. Apple's class keeps everything `backboardd` talks to (the user client, the swap queue, power state, vsync clients, properties); the subclass supplies the 24 slots a panel driver must: the mode, swap validation, the transfer of a frame, completion, the refresh attributes |
| `LegacyDisplayProbe.cpp` | `external/UniXNU-droidvm/package`, staged with five lines added | Registers the class, makes the service on a nub that carries the boot video as device memory, attaches the nub under `/arm-io` as `gop-display` (`device_type` `display-subsystem`), publishes the scale, hands frames to the virtio scanout |
| `DroidVMVirtioGpuDisplay.cpp` | `external/UniXNU-droidvm/package`, unchanged | The scanout client of the virtio GPU: a resource per IOSurface (up to 32, by its guest pages), `SET_SCANOUT` and `RESOURCE_FLUSH` per frame from its own thread; the boot video as a resource too |
| `OneiOSDisplay.cpp` | here | The kernel's two entries, the parent class under xnuhost's slide, the display's switches as defaults, the log lines, Droid-VM's default-surface factory |
| `legacy-display-24A446-v54.json` | `port-legacy-display.py` | The original class in this kernelcache: 379 slots with their diversities, the fields and functions the backend uses |
| `build-oneios-display.py` | here | The archive (below) |
| `display-capture.py` | here | A client of `display.sock` for the PC: says when the picture changed and writes PNGs |

The transport (`DroidVMVirtioGpu.cpp`) and the 3D half are in the GPU's archive (`../gpu`, `docs/gpu.md`).

## The class for this kernelcache (`port-legacy-display.py`)

Droid-VM's reviewed fixture (`legacy-display-24A446.json`) is for the iPad17,3 kernelcache of the same build.
`kernel/prelink/classes.py` already reads the class from the iPhone18,2 one (`classes-24A446-v54.json`: 0x1dd8
bytes, 379 slots, as on the iPad). `port-legacy-display.py` shows the family's code is the same code at another
address and carries the reviewed meanings over; it fails rather than guess:

- All 215 slots whose target is in IOMobileGraphicsFamily's text are at one distance from the iPad's (-0x101d40).
  The other 164 are the kernel's and are taken from this kernelcache.
- Diversities are equal slot by slot. The three pure-virtual slots (214, 216, 257) differ in the table entry
  (`__cxa_pure_virtual` under another diversity), and what counts is what callers authenticate with: the fixture's
  value must appear as `movk xN, #d, lsl #48` in this family's text (it does; the table entry's does not).
- The family's two functions the backend calls (the vsync sender, `setDisplayRefreshProperties`) move by that
  distance; IOSurface's eight (descriptor, width, height, stride, format, planes, create, allocate) by the one
  distance (-0x1005a0) at which all eight of the fixture's byte checks hold in this IOSurface.
- The field offsets (swap queue head, tail and current, vsync clients, width, height, default surface, surface
  root; the swap request's rectangles, masks, options, size, surfaces, links) are unchanged; the two instructions
  the fixture checks them by are read back.

```sh
kernel/patches/oneios-hosted/display/port-legacy-display.py      # rewrites legacy-display-24A446-v54.json
```

Both kexts are in the collection's closure as roots (`kernel.kc.prelink.json`), with their personalities.

## The archive (`build-oneios-display.py`)

`kernel/build-hosted.sh` copies the script into the build's device repository, appends it to the board's archives
(after the GPU's) and sets `CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y` in the build's copy of the hosted defconfig. With
that option UniXNU's kernel build links `build/native-legacy-display/libNativeLegacyDisplay.late.a` and compiles
the two calls in `bsd_init.c` (`native_legacy_display_probe` after the graphics family is loaded,
`native_legacy_display_late_probe` after `bsd_autoconf`: IOSurface's root matches on `IOBSD`). Both run before the
root is mounted, so the display is up about ten seconds into a boot under qemu.

As Droid-VM's `build-droidvm-legacy-display.py`: the class header is generated (each slot under a name whose
pointer-authentication discriminator is the slot's diversity), the compiled table is checked entry by entry, and
every original function is reached through a stub. Three things differ, all because the collection runs slid
under xnuhost and the kernel is compiled as a kernel extension is:

1. A stub is `bti c; b <name>.original` with an absolute symbol at the linked address, which XnuPrelink's emitter
   retargets (428 branches into the two kexts in `kernel.kc.prelink.json` `native_original_branches`). Droid-VM's
   absolute literal would be the unslid address.
2. **The parent's vtable is defined by the archive.** A qualified call of a parent method
   (`IOMobileFramebufferLegacy::start(provider)`) goes through the parent's vtable, loaded by address
   (`-fapple-kext`). The original table's address as an absolute symbol is wrong under the slide, and its entries
   are signed for their own addresses. So `__ZTV25IOMobileFramebufferLegacy` is a table of this kernel: the same
   379 slots, each signed by the linker for its place (IA, address, the slot's diversity), pointing at a stub to
   the original function or at the kernel's own method. Original objects never see it.
3. The parent's metaclass is data: its link address and the instance size go in as numbers.
   `OneiOSDisplay.cpp` compares the live class with them (after adding the slide) before anything is registered,
   and slides the owned class's superclass link before the module load ends (as `../aes` does).

The probe's entries are compiled under other names and wrapped by `OneiOSDisplay.cpp`; the probes of other
archives that Droid-VM's late probe starts (key store, cpufreq, GPU power, touch, HID) are compiled to nothing,
so those archives keep their own names.

## The mode, the scale, vsync, swaps

- **Mode.** The boot video: `boot_args.Video`, which xnuhost sets from `--width/--height` (1320x2868 by default,
  the iPhone18,2 panel), 32 bit B,G,R,X. One layer (`IOMFBNumLayers` `MaxNumLayers` 1).
- **Scale.** `/chosen display-scale` = 3 and `display-rotation` = 0, published by the probe
  (`offline-legacy-unit-scale`, default 3 here). 1320x2868 at @3x is 440x956 points. Whether iOS 27's QuartzCore
  honours 3 on this device is not known: iPadOS 27's runs any panel taller than 1080 pixels at @2x whatever the
  property says (`external/UniXNU-droidvm/ipados27/board.toml`), and the phone path of
  `-[CADisplayMode preferredScale]` has not been read.
- **Vsync.** The family sends the compositor's vsync message; nothing in it produces the tick (Apple's panel
  subclass does, from the vblank). The backend's work-loop timer stands in while a client has vsync enabled:
  60 Hz by default (`sheng-vsync=N`, 0 for none; above 60 the refresh properties report that rate too). On the
  phone the helper's layer runs at the panel's 120 Hz: pass `sheng-vsync=120` there.
- **Swaps.** A swap is accepted when it is one full-panel, untransformed surface in B,G,R,A (or Apple's `&BGA`
  spelling, or half-float `RGhA`), or a blank frame. An accepted B,G,R,A frame is offered to the virtio scanout,
  which presents the surface's own pages (no copy in the guest) and completes the swap; a half-float frame, or one
  the scanout refused, is converted or copied into the boot video by the CPU and that is presented. The family
  retires the swap and wakes the client either way.

Switches (boot arguments; an absent one has the value shown while the display is on):
`oneios-display=1` (0: no display at all, the console only), `offline-legacy-unit-scale=3`, `sheng-vsync=60`, and
Droid-VM's stages, which there is no reason to change: `offline-mobile-graphics-load`, `offline-legacy-register`,
`offline-legacy-gop`, `offline-legacy-phase=4`, `offline-legacy-pm`, `offline-legacy-validate`,
`offline-legacy-transfer`, `offline-legacy-cpu-present`.

## The kernel patch (`0020-display.patch`)

Two changes, under `#if XNU_HOSTED`:

- `bsd_init.c` `native_mobile_graphics_load_probe`: the graphics family is loaded and the backend registered
  without boot arguments (UniXNU treats both as experiments a boot argument asks for).
- `hosted_pmap.h` `hosted_kernel_map_locked`: a kernel mapping of the boot framebuffer is given to `kernel_map`.
  The framebuffer is memory of xnuhost's RAM object above the kernel's window; the pmap filled only pages of the
  window, so IOKit's mapping of it (the backend's boot mapping, where blank and CPU-copied frames are drawn)
  would fault forever. The range is `xh_info.video`'s; xnuhost's `kernel_map` already takes any page of its RAM.

## Scanout, and `session/ready`

On the PC and wherever xnuhost has no renderer the device offers no blob resources, so each surface is a 2D
resource with its pages attached (`RESOURCE_CREATE_2D` in B,G,R,X with the surface's row length as the width,
`ATTACH_BACKING`), and a frame is `TRANSFER_TO_HOST_2D`, `SET_SCANOUT`, `RESOURCE_FLUSH`: xnuhost copies the rows
into its frame and posts the rectangle on `display.sock`. With a renderer (the phone) the surfaces are guest blobs
and a frame is `SET_SCANOUT_BLOB` and a flush (`docs/gpu.md`).

Two changes in xnuhost's device (`runtime/xnuhost/src/virtio_gpu.c`) came with this:

- A surface of scattered 4 KiB pages needs up to 3765 backing entries at 1320x2868; the limit for a 2D resource
  was 4096 and is now 16384, as for blobs.
- The scanout client also presents the boot video (the console, until the compositor's first frame). That picture
  is shown, but **`session/ready` is created at the first frame of a surface that is not the boot framebuffer**.

## What the 3D / Metal path needs later (not done, not blocking)

- With a renderer the client already uses blobs (`droidvm-virtio-gpu-blob`, on when the device offers
  `RESOURCE_BLOB` and `CREATE_GUEST_HANDLE`). The blob's size is rounded up past the surface for Turnip's
  over-fetch using a 16 KiB guest page (`GuestPage` in `DroidVMVirtioGpuDisplay.cpp`); this kernel's pages are
  4 KiB, so the "rest of the last page" case may describe memory that is not the surface's. Check before the first
  3D frame.
- A swap waits for the GPU work submitted before it (`droidvm_virtgpu_submitted`, `_finished_by`, the display
  holds): those are the 3D half's and answer "nothing outstanding" until a Metal client exists.
- iOS 27's render server picks half-float framebuffers once a Metal device renders: the backend converts them on
  the CPU (a full-frame conversion per swap). The GPU conversion is `NativeGOPPresenter`'s hand-off
  (`sheng-fb-gpu=1`), which needs a privileged Metal client in user space.
- `IOSurfaceAccelerator` (the scaler) is not in the collection: a layer CoreAnimation wants scaled by the display
  has nowhere to go. One layer is advertised so that it composites instead.
- EDID (the refresh rate from xnuhost), zero-copy scanout to surfaceflinger and a cursor are not done
  (`docs/gpu.md` section 6).

## What ran, what did not

On the PC (`qemu-aarch64 -cpu cortex-a710`, 8 vCPUs, 4 GiB, `--display shm`, the real image set, 2026-11-11), three
boots; the last with a kernel built from scratch from the series 0001-0017 + 0020 and its own collection:

- About ten seconds in: `oneios-display: probe: stage 3, class registration 0, object 8104 bytes (parent 1), init 1`;
  the family's own `IOMobileFramebufferLegacy::start` logs; `default surface status 0 valid 1 1320x2868 stride 5376`;
  `late probe: stage 10, IOSurface load 0 root 1, tree node 1, probe 1, start 1, scale status 0 (/chosen
  display-scale 3), power stage 2 status 0`; `panel 1320x2868 ...; virtio scanout state 2, 2D resources`.
- The boot video reaches `display.sock` through the guest's driver: `display-capture.py` counted about 1.1 flushes
  a second (1020 in the 15 minutes of the last boot) of the console's picture. `session/ready` was not created
  (the console is not the first frame).
- After launchd's early boot: `framebuffer opened by thermalmonitord[49]`, then `by backboardd[53]`, then by
  `MobileGestaltHelper`, each `status 0, client IOMobileFramebufferUserClient`. **backboardd finds and opens the
  framebuffer.**
- **No swap was submitted and no frame of iOS's reached `display.sock`.** Four and a half minutes after it opened
  the framebuffer, backboardd died in `libsystem_pthread` `_pthread_wqthread_setup` (a new workqueue thread whose
  first system call, number 372, returned -1; `brk`), and three minutes later the kernel stopped with `Console
  I/O from interrupt-disabled context` (a `printf` in `hosted_user_loop`; the same stop ended the second boot).
  Both are the hosted user-thread layer's (the board's Q6), not the display's. chronod still aborted once in
  QuartzCore's `query_displays`, 42 s after backboardd opened the framebuffer: the render server had no display to
  offer yet.

Not run, anywhere: a swap (validation, the offer to the virtio scanout, the CPU copy into the boot video, the
half-float conversion, retirement), the vsync tick with a client, a blank frame, the kernel mapping of the boot
video being written (it is made at start; nothing drew into it), whether QuartzCore takes @3x, the blob scanout,
anything on the phone. xnuhost's two changes are covered by its unit tests (`make -C runtime/xnuhost test`:
13189 checks).
