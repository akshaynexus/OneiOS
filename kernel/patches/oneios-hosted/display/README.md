# The display: IOMobileFramebuffer over xnuhost's virtio-gpu scanout

Owner: q4a-framebuffer. Date: 2026-11-11. The guest's display driver, in the hosted kernel: what `backboardd`
opens as the main display, and how its frames reach `session/display.sock`.

**State, plainly.** It builds into the hosted kernel and starts at boot under `qemu-aarch64`: the class registers
under the collection's `IOMobileFramebufferLegacy`, the service starts on the boot video, IOSurface makes its
default surface (at the panel's native size, 1440x3120), `/chosen display-scale` is 3 (or 2), and the virtio-gpu
scanout presents. What `backboardd` then does
with it is in "What ran" below. Nothing here has run on the phone.

## What it is made of

Nothing new was written for the framebuffer itself. It is the display Droid-VM runs on iPadOS 27, bound to the
iPhone18,2 kernelcache and to xnuhost:

| Piece | From | What |
|---|---|---|
| `NativeLegacyDisplay.cpp`, `NativeGOPPresenter.cpp` | `external/NativeMobileDisplay` (UniXNU), unchanged | A subclass of the original `IOMobileFramebufferLegacy` of `com.apple.iokit.IOMobileGraphicsFamily`. Apple's class keeps everything `backboardd` talks to (the user client, the swap queue, power state, vsync clients, properties); the subclass supplies the 24 slots a panel driver must: the mode, swap validation, the transfer of a frame, completion, the refresh attributes |
| `LegacyDisplayProbe.cpp` | `external/UniXNU-droidvm/package`, staged with the edits of `staged/display.rs` `DISPLAY` (the log hooks, the parent class, the half-float offer) | Registers the class, makes the service on a nub that carries the boot video as device memory, attaches the nub under `/arm-io` as `gop-display` (`device_type` `display-subsystem`), publishes the scale, hands frames to the virtio scanout |
| `DroidVMVirtioGpuDisplay.cpp` | `external/UniXNU-droidvm/package`, staged with `staged/display.rs` `SCANOUT` (each resource carries its scanout format: half floats with `oneios-display-wide=1`) | The scanout client of the virtio GPU: a resource per IOSurface (up to 32, by its guest pages), `SET_SCANOUT` and `RESOURCE_FLUSH` per frame from its own thread; the boot video as a resource too |
| `OneiOSDisplay.cpp` | here | The kernel's two entries, the parent class under xnuhost's slide, the display's switches as defaults, the log lines, Droid-VM's default-surface factory |
| `legacy-display-24A446-v54.json` | `oneios-tool kernel port-legacy-display` (was `port-legacy-display.py` here) | The original class in this kernelcache: 379 slots with their diversities, the fields and functions the backend uses |
| `oneios-tool kernel gen display` | `tools/oneios-tool/crates/oneios-kernel/src/build_oneios_display.rs` (was `build-oneios-display.py` here) | The archive (below) |
| `display-capture.py` | here | A client of `display.sock` for the PC: says when the picture changed and writes PNGs |

The transport (`DroidVMVirtioGpu.cpp`) and the 3D half are in the GPU's archive (`../gpu`, `docs/gpu.md`).

## The class for this kernelcache (`oneios-tool kernel port-legacy-display`)

Droid-VM's reviewed fixture (`legacy-display-24A446.json`) is for the iPad17,3 kernelcache of the same build.
`oneios-tool prelink classes` already reads the class from the iPhone18,2 one (`classes-24A446-v54.json`: 0x1dd8
bytes, 379 slots, as on the iPad). `oneios-tool kernel port-legacy-display` shows the family's code is the same code at another
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
oneios-tool kernel port-legacy-display      # rewrites legacy-display-24A446-v54.json (run in the checkout, or --repo)
```

Both kexts are in the collection's closure as roots (`kernel.kc.prelink.json`), with their personalities.

## The archive (`oneios-tool kernel gen display`)

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

- **Mode.** The boot video: `boot_args.Video`, which xnuhost makes the size of the phone panel's native mode
  (1440x3120 on the Galaxy S24 Ultra; `--width/--height` for another, `docs/booter.md` 2.8), 32 bit B,G,R,X. The
  guest renders every pixel of the panel; nothing is scaled between its frame and the glass. One layer
  (`IOMFBNumLayers` `MaxNumLayers` 1). A surface of scattered 4 KiB pages has 4388 backing entries at this size.
- **Scale.** `/chosen display-scale` = 3 or 2 and `display-rotation` = 0. xnuhost writes both into the device
  tree and passes `offline-legacy-unit-scale=N`; the probe finds the words there and checks they are that value
  (`scale status 0` in the log). 3 (the default) is 480x1040 points, 2 is 720x1560; the pixels are the same.
  The value is xnuhost's `scale` (`--set scale=2`, or the image set's `overlay/userland/xnuhost.conf`).
  **QuartzCore does not read `display-scale`.** `-[CADisplayMode preferredScale]`, read in this build's
  `dyld_shared_cache_arm64e.01` at 0x40b1758 (the same code at the same offset as in Droid-VM's iPad cache):
  the mode's own preferred scale when it has one; else, in a virtual device, 2 (or the mode's native-scale field
  when that is more); else 2 for a mode taller than 1080 pixels, 1 otherwise. This framebuffer's modes carry no
  preferred scale, so the unchanged answer is 2. For 3 xnuhost adds two text fixups to `/chosen text-fixups`
  (`config.c` `xh_scale_fixups`: the virtual device's floor 2 -> 3 at 0x40b17a8, the height test's result -> 3 at
  0x40b17bc), which the kernel applies to those pages when they are paged in, only where the original bytes
  match. **Not verified in a running guest** (no frame of the compositor's yet): whether the fixups are applied
  to shared-cache pages under xnuhost, and what UIKit then lays out, are open.
- **Vsync and the refresh rate.** The family sends the compositor's vsync message; nothing in it produces the
  tick (Apple's panel subclass does, from the vblank). The backend's work-loop timer stands in while a client has
  vsync enabled, at `sheng-vsync=N` Hz: xnuhost passes the panel's rate (120 on this phone; `--hz N`). Above
  60 Hz the backend also reports the rate the way a ProMotion panel does: `getRefreshAttributes` answers a
  minimum refresh interval of 1/120 s and keeps the family's maximum of 1/24 s (a range of 24 to 120 Hz for
  the render server), and `setDisplayRefreshProperties` publishes that as `IOMFBDisplayRefresh` before any
  client can find the display. `sheng-vsync-fixed=1` reports one fixed rate instead. With no boot argument at
  all (a kernel started without xnuhost's) the tick is 60 Hz and the family's 1/60 s stays. The tick with a
  client, and what backboardd makes of the properties, have not run.
- **Swaps.** A swap is accepted when it is one full-panel, untransformed surface in B,G,R,A (or Apple's `&BGA`
  spelling, or half-float `RGhA`), or a blank frame. An accepted B,G,R,A frame is offered to the virtio scanout,
  which presents the surface's own pages (no copy in the guest) and completes the swap; a half-float frame, or one
  the scanout refused, is converted or copied into the boot video by the CPU and that is presented. The family
  retires the swap and wakes the client either way. With `oneios-display-wide=1` (xnuhost's `display-format=rgba16f`)
  a half-float frame is offered to the virtio scanout as it is too, in xnuhost's format `0x52476841` ('RGhA', 8
  bytes a pixel): "Colour, depth and the panel" below.

Switches (boot arguments; an absent one has the value shown while the display is on):
`oneios-display=1` (0: no display at all, the console only), `oneios-display-wide=0` (1: half-float frames to the
scanout as they are; xnuhost passes it with `display-format=rgba16f`), `offline-legacy-unit-scale=3`, `sheng-vsync=60` (xnuhost
always passes these two, from its `scale` and the panel's rate, unless the command line already has them), and
Droid-VM's stages, which there is no reason to change: `offline-mobile-graphics-load`, `offline-legacy-register`,
`offline-legacy-gop`, `offline-legacy-phase=4`, `offline-legacy-pm`, `offline-legacy-validate`,
`offline-legacy-transfer`, `offline-legacy-cpu-present`.

## Colour, depth and the panel (fix-display-colour, 2026-11-13)

The fault (phone, 2026-11-12 23:45): HDR and colour depth look off, and the blurs do not look like iOS's. Read in
24A446's QuartzCore, CoreMaterial, UIKitCore and libMobileGestalt (the guest's dyld cache, `out/ios27-cache`, with
`ipsw dyld disass`/`symaddr`/`str` and `llvm-objdump` on the extracted images; addresses are the cache's) and in the
real iPhone18,2 device tree (`out/kernel-inputs/24A446/devicetree.bin`, `ipsw dtree --json`). [F] read in the code or
tree; [I] inferred; [U] not known.

### What iOS renders into

`CA::WindowServer::IOMFBDisplay::framebuffer_pixel_format` (0x184461838), asked on every update [F]:

1. `'RGhA'` (64-bit half float) when the display's EDR headroom (`Display::edr_headroom`: `CA_DISPLAY_EDR_HEADROOM`,
   else the brightness capabilities') is above `CA_FP16_MIN_HEADROOM` and the panel's own headroom above 8, or when
   `CA_FORCE_FP16_FRAMEBUFFER` or `CA_ALLOW_EDR_MIRRORING` is set, or the display's own flag (+0x6840), or a few
   compression cases, or **`CADeviceIsVirtualized`** (MobileGestalt `IsVirtualDevice`, which is `/defaults vmm-present`
   = 1 here).
2. Else, when the display supports ICC colour management (`IOMFBDisplay::supports_icc_color_management`, display
   +0x7580) and `CADeviceSupportsUniversalCompression`: **`'b3a8'`**, which becomes **`'w40a'`** unless the display
   takes `b3a8` (+0xd0c): 10-bit extended-range ("XR") wide-colour formats, sRGB-encoded with values outside 0..1, or
   their compressed `&b38`/`&w4a`. **This is a real iPhone 17 Pro Max's framebuffer** [I: the branch a device with
   ICC and universal compression takes].
3. Else **`'BGRA'`**: 8 bits a channel.

`supports_icc_color_management` is set in `AppleDisplay::update_display_limits` from the IOMFB property
`IOMFBSupportsICC`, and where the display has none, from `CADeviceDisplaySupportsICC` (`MGIsDeviceOneOfType` over a
list of product hashes, or the default `CAEnableDisplayICC`), and only for a display type with
`(+0x2c8 & 0x1c00) == 0` [F]. This display's own code publishes no `IOMFBSupportsICC` (NativeLegacyDisplay sets
`IOMFBNumLayers` and the refresh properties) [F]; whether Apple's family class publishes one is what the new
`oneios-display: properties:` lines say [U]. The Metal plug-in holds the headroom at 1, the FP16 threshold at 1e30, the mirroring option
off and QuartzCore's virtualized byte at 0 in backboardd (`docs/gpu.md` 7), so the render server takes branch 2 or 3.
This display takes only `BGRA`, `&BGA` and `RGhA` (NativeLegacyDisplay's `nativeScanoutFormat`); a `w40a` swap is
refused (reason 4). The phone shows frames, so the render server there chose `BGRA` or `RGhA` [I]; **which one, and
whether any swap was refused for its format, is what the new line `oneios-display: the compositor swaps format ...`
says** (below).

The colour space: QuartzCore composites this display in **sRGB**: `IOMFBDisplay::compositing_color_space` is
`CAGetColorSpace(36)`, and `get_cg_name`'s table gives `kCGColorSpaceSRGB` for 36 [F]. On a real iPhone that is
extended-range sRGB (the XR formats carry Display P3's colours as components below 0 and above 1) and the panel's
own gamut is applied by the display hardware (ICC matrices, `IOMFBDisplay::set_icc_matrix_and_trc`) [F, I]. Here:

| Step | What is kept |
|---|---|
| `BGRA` framebuffer | 8 bits, sRGB-encoded; Display P3's colours and EDR clipped to sRGB in the render server |
| `RGhA` framebuffer, default path | half floats; then `convertHalfRows` (NativeLegacyDisplay, `sheng-fb-half=1`: linear light encoded to sRGB) into the 8-bit boot video by the CPU: negative and above-1 components clipped, so the same as `BGRA`, at the cost of a CPU conversion of each frame |
| virtio scanout, xnuhost's `display.sock` | `B,G,R,X` 8 bits (hello format 1) |
| display helper (before 2026-11-13) | `BGRA_8888` (format 5) with **no dataspace** (`UNKNOWN`) |

So the guest's picture is 8-bit sRGB everywhere, and P3 colours (wallpapers, photos, the vibrant tints of the
materials) are clipped before they leave the guest. One UI's compositor then saw an untagged buffer.

### What iOS reads about the panel

The IOMFB properties QuartzCore reads (`IOMobileFramebufferCopyProperty`/`copy_display_property`) [F]:
`AppleDisplay`'s constructor: `IOMFBLTPOFlashMitigationNeeded`, `primary`, `SupportsAOTPowerSaving`,
`IOMFBSupportsGPLite`, `IOMFBSupportsSecureIndicator`, `IOMFBSupportHWSecureAnimation`, `IOMFBSupportsLFC`,
`CursorPlane`, and MobileGestalt `DisplayExtendedProperties`, `main-screen-scale`; `update_display_limits`:
`DisplayPipeStrideRequirements`, `DisplayPipePlaneBaseAlignment`, `IOMFBMaxSrcPixels`, `IOMFBMaxCompressedSizeInBytes`,
`IOMFBCompressionSupport`, `IOMFBNumLayers`, the bandwidth limits, `PCCCabalEnable`, `IOMFBRGBMultiPlaneSupportKey`,
`IOMFBYUVSupportKey`, `IOMFBYUVPackedUnSupported`, `IOMFBYUV420UnSupported`, `W40a_Blending_OK`, **`IOMFBSupportsICC`**,
`IOMFBDisplayRefresh`, `IOMFBSupportsRotation`, `IOMFBDisableFlip`, `IOMFBBlendDisabledKey`,
`IOMFBCompressedSourceSurfaceWidth`, `IOMFBSwapAppleLookSupported`, `IdleCachingMethod`; `update_framebuffer_locked`:
`DisplayAttributes` (`Luminance`, `Chromaticity`, `DefaultColorSpaceIsSRGB`: read for virtual and external displays),
`Transport`, `IOMFBUUID`, `DisplayContainerID`, `VirtualDisplayMode`, `edrScaleinGP`; `begin_update`: `Brightness_Scale`.
The EDR headroom and nits come from the brightness capabilities (`update_brightness_capabilities`: `MaxNits`,
`MinNits`, `MaxNitsEDR`, `EDRPotentialHeadroom`), i.e. CoreBrightness and the backlight service (`drivers/brightness`;
`capabilities = {MaxNits = 0 ...}` was seen before, `docs/hardware.md` 7a).

MobileGestalt answers QuartzCore asks [F]: `IsVirtualDevice` (40 places: the framebuffer format, compression, EDR,
modes, and `MetalContext::capture_backdrop_surface_compute`, which gives up the compute blur for the render
fallback on a virtual device), `OLEDDisplay` (`CADeviceHasLCDPanel`), `HasExtendedColorDisplay` (`CADeviceUseCIF10`),
`IOSurfaceFormatDictionary` (universal compression), `DeviceSupportsDCP`, `ChipID` (many `CADevice*` answers),
`DeviceClassNumber`, `DeviceSupportsASTC`.

The materials [F]: UIKit's `-[UIDevice _graphicsQuality]` is the constant 100 (high) in 24A446 (0x184a02540,
`mov w0, #0x64; ret`): no MobileGestalt answer lowers UIKit's backdrops. CoreMaterial's low-quality list for the
dynamic blur radius names iPad5,x and iPad7,x only. MaterialKit honours Reduce Transparency (an accessibility
setting). MobileGestalt's `UIBackgroundQuality` (`/product ui-background-quality`) is asked by no image of the cache
by name or by its obfuscated key (`ipsw dyld str`). So nothing found in the IOMFB or MobileGestalt answers turns the
materials off; what differs from an iPhone is the 8-bit sRGB output (materials' vibrancy and P3 tints clipped, banding
in wide blurs) [I], QuartzCore's virtualized answer in processes that do not load the Metal plug-in [F], and the
GPU translation of CoreAnimation's blur kernels (metal2vulkan on Turnip, `docs/gpu.md`) [U].

**The guest's `/product` is not the iPhone's** [F]: xnuhost copies `/product` from `<image>/identity.adt`, which no
script makes, so the guest's `/product` has `product-name "OneiOS guest"`, `artwork-scale-factor` and `model` only.
The iPhone's own (the real tree) has, among about 200 more: `artwork-display-gamut P3`, `oled-display 1`,
`graphics-featureset-class APPLE10`, `ui-background-quality 100`, `ui-weather-quality 100`, `island-notch-location
144`, `display-corner-radius`, `supports-burnin-mitigation 1`, `primary-calibration-matrix syscfg/DPCl`. A run with
`--set identity=out/kernel-inputs/24A446/devicetree.bin` gives them; not made the default here (it changes many
MobileGestalt answers at once; the userland profile's `artwork-display-gamut "sRGB"` and xnuhost's `P3` default with
an identity disagree, `docs/userland.md` 5).

The real iPhone18,2 tree's panel and brightness values [F, 16.16 numbers divided out]: `/backlight`
`EDRPotentialHeadroom` 8.0, `EDRReferenceHeadroom` 12.0, `edr-max-nits` 1600, `user-accessible-max-nits` 1060,
`override-boot-brightness-nits` 80, `panel-gamma` 2200 (2.2); `/exbright` `LmaxProduct` 3000, `LminProduct` 10,
`DBVMin` 117, `DBVMax` 2047; `/arm-io/disp0` `display-default-color` 0, `max-avg-bpp`/`max-peak-bpp` 891290
(13.6 bits a pixel, DCP's compressed budget). The S24 Ultra's panel's own numbers are what the display helper's
`panel colour mode ...; HDR types ..., luminance max ...` line reports.

### The fix (ours), and what it does not do yet

- **The display helper** tags its buffers: `SRGB` for 8-bit frames, which is what they are (`runtime/display/README.md`
  "Colour"); `SCRGB_LINEAR` and `RGBA_FP16` buffers for half-float frames.
- **A wide path, opt-in** (xnuhost `display-format=rgba16f`, `docs/booter.md` 2.8): xnuhost keeps half floats in its
  frame and passes `oneios-display-wide=1`; this archive then offers `RGhA` frames to the virtio scanout as they are
  (LegacyDisplayProbe's offer and DroidVMVirtioGpuDisplay's resources carry the format: `staged/display.rs` `DISPLAY`
  and `SCANOUT`; a 2D resource is `RESOURCE_CREATE_2D` in `0x52476841` with the stride / 8 as its width, a blob's
  `SET_SCANOUT_BLOB` names that format), with no CPU conversion in the guest; the Metal plug-in asks the render
  server for half floats when the boot argument is there (`userland/metal/DisplayWide.inc`, `CA_FORCE_FP16_FRAMEBUFFER`;
  built on the Mac). The default stays 8-bit `BGRA`, as before.
- **Measured at each boot** (new log lines): `oneios-display: half-float frames: ...` (which path),
  `oneios-display: properties: ...` (every property of the display service, once, after the start: what QuartzCore
  can read), and `oneios-display: the compositor swaps format 'BGRA' (0x42475241, 0 planes, 1440x3120, stride 5760):
  8-bit B,G,R,A, sRGB; taken (reason 0)` once per surface format the render server swaps (`REFUSED` and reason 4 for
  a format this display does not take, such as `w40a`).
- Not done: `w40a`/`b3a8` (10-bit XR) are not taken; no `IOMFBSupportsICC` is published (publishing `true` would
  select `w40a`, which is refused); the EDR headroom stays 1 (the plug-in, and no `MaxNitsEDR` from the backlight);
  the helper asks surfaceflinger for no HDR ratio; the identity tree is not in the image sets.

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

- A surface of scattered 4 KiB pages needs up to 4388 backing entries at 1440x3120 (3765 at 1320x2868); the limit
  for a 2D resource was 4096 and is now 16384, as for blobs.
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
  (These three boots ran at the size xnuhost used then, 1320x2868.)
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

**The native mode (2026-11-11, native-display).** One boot with q4a's kernel (a collection that has the display,
no kernel change) and the new xnuhost, `--display shm`, no size given: xnuhost logged `guest frame 1440x3120 at
120 Hz, @3x (480x1040 points)` and `text fixups: 2 for /chosen text-fixups`; the guest `default surface status 0
valid 1 1440x3120 stride 5760 bytes 17973248`, `late probe: stage 10, ... scale status 0 (/chosen display-scale
3)`, `panel 1440x3120 at ... (17971200 bytes); virtio scanout state 2, 2D resources`; `display-capture.py` took
`hello 1440x3120 stride 5760` and wrote a 1440x3120 PNG. What that boot did after the display started is in
`docs/agents.md` (native-display's note). The image set it ran was built before this change (its Data seed's
`identity.expect` still names 1320x2868).

Not run, anywhere: a swap (validation, the offer to the virtio scanout, the CPU copy into the boot video, the
half-float conversion, retirement), the vsync tick with a client, a blank frame, the kernel mapping of the boot
video being written (it is made at start; nothing drew into it), whether QuartzCore takes @3x, the blob scanout,
anything on the phone. xnuhost's two changes are covered by its unit tests (`make -C runtime/xnuhost test`:
13189 checks).

## Power: why no frame was shown, and the fix (lead, 2026-11-11)

Found with `oneios-iouc=backboardd` and `oneios-display-report=N` (`docs/building.md` 5) on the PC and on the phone,
and by reading the family's code in the 24A446 v54 kernelcache:

- backboardd registers for vsync (selector 72) and submits swaps (4 begin, 5 submit, 6 wait: all succeed, 64 and
  more on the PC), and the backend was never asked to validate or kick one: `kicks 0`, `swaps validated 0`.
- The family's `swap_submit` (0xfffffe000a18c63c) queues a swap only while the display object's word at +7180 is
  not 0. Otherwise it releases the swap and returns success (0xfffffe000a18cfcc).
- That word is the power state. One function writes it, the family's `setPowerState` (0xfffffe000a1915f4, through
  the gated setter at 0xfffffe000a18a428), which the power manager calls. The word beside it, +7184, is what
  `NativeLegacyDisplay`'s start announces.
- The backend registers two power states and asks for state 1 at start. Under this kernel the power manager never
  calls back: `pm stage 2 calls 2 callbacks 0`, `+7180 0 +7184 1`, the same on the PC and on the S24 Ultra.
  **Why it does not call is not known.**
- User space cannot switch it on: the user client's power request (selector 12, 0xfffffe000a17c6e0) answers
  `kIOReturnUnsupported` unless the display object's byte at +140 is set, which only a platform subclass does.

The fix, in `OneiOSDisplay.cpp` (`oneios_display_power_on`, at the end of the late probe): the power manager gets
2 seconds; if it has not called, `setPowerState(1, display)` is called on the display, the same virtual with the
same arguments. Log: `oneios-display: power: ...; the family's power word was 0 and is 1`.
`oneios-display-power=0` leaves it out. The offset 7180 is this kernelcache's.

Run on the phone (S24 Ultra, `a472a41`): `power: the power manager did not call setPowerState in 2 s: called here; the family's power word was 0 and is 1`, then swap submits (selector 5) for the first time and the Apple boot logo on the panel (screen capture and the glass). Not run on the PC. Not known: whether anything
else in the family asks the power manager for the state (IOService's own `getPowerState()` still says 0).
