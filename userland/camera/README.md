# userland/camera: the capture plug-in (iOS side of the camera)

Owner: fable-camera. The host side: `runtime/bridges/camera`. The kernel side: `drivers/camera`. The whole path:
`docs/hardware.md` section 7.

**State (2026-11-11): the reader is written and host-tested; the plug-in is written and syntax-checked only; the
capture device object is not written.** Nothing here has been built for iOS (no iPhoneOS SDK on the build machine)
or run (the guest does not reach launchd yet). iOS shows no camera until the last part exists.

## How iOS 27 finds a camera, and what that asks of us

- Apple's path [F iOSPlus10 `docs/camera-plan.md`, `docs/laurel_sprout/camera.md` section 3; I for iOS 27's names]:
  the capture servers (`com.apple.coremedia.capturesource`, `capturesession`; in `cameracaptured` since iOS 17, in
  mediaserverd before) ask `+[BWFigCaptureDeviceVendor sharedCaptureDeviceVendor]` for a FigCaptureDevice. Apple's
  vendor makes it from the ISP driver's user client (`AppleH16CamIn` on an A19; `AppleH10CamIn` on the iPhone X) and
  the ISP's firmware protocol. No other camera can serve that protocol, so an IOKit driver that stands in for
  AppleH16CamIn is not a way: **the camera enters through a plug-in in the capture server**, not through IOKit.
- The route that has run on other ports (HakiOS; iOSPlus10's negroni and Mi A3, iOS 16.7): replace
  `sharedCaptureDeviceVendor` so that, where the stock one gives nil, it returns
  `[[BWFigCaptureDeviceVendor alloc] initWithDeviceCreateFunction:ours]`; our function makes a FigCaptureDevice with
  one FigCaptureStream per camera, and the stream hands the capture graph CVPixelBuffers. A few more CMCapture
  methods are replaced for what a camera without Apple's ISP lacks (stabilisation, noise reduction, the portrait
  filter prewarm).
- Here the frames are finished NV12 from One UI's camera HAL, so there is **no ISP in the guest**: iOSPlus10's
  `camera_isp.c` (demosaic, white balance, exposure loop) has no counterpart, and exposure, gain and focus commands
  are ignored by the kernel node.
- A reusable iOS-side path exists only as that source: iOSPlus10 `userland/camera/` (`camera_device.c` 2487 lines,
  `media_hooks.c`; HakiOS, MIT). UniXNU, UniXNU-droidvm and Droid-VM have no camera path (their virtio-media work is
  video decode). `camera_device.c` is written to iOS 16.5's FigCaptureDevice and FigCaptureStream vtables and
  property names; it does not carry over to CMCapture 27 unchanged.

## Files

| File | What | State |
|---|---|---|
| `oneios_camera_reader.c`, `.h` | the reader of `/dev/oneios-camera`: selects camera and size, reads a frame, checks its header, copies the picture into a pixel buffer's two planes (any bytes per row) | host-tested: 26 checks |
| `OneiOSCamera.c`, `.h` | the plug-in: constructor (only in `cameracaptured` / `mediaserverd`; `/private/var/oneios/camera/off` turns it off), a report of which of the route's classes and methods the loaded CMCapture has, the frame pump (node -> IOSurface-backed 420f CVPixelBuffers from a pool -> a callback), and the vendor hook, installed only when `oneios_camera_device_create` is linked in | syntax-checked against stand-in headers; not built for iOS |
| `build.sh` | `out/userland/camera/OneiOSCamera.dylib`, arm64e, ad hoc signed, its CDHash for `owner-trust.json` | not run (no SDK) |
| `tests/` | `host.sh` (CI), `test_reader.c`, `stub-sdk/` (our own stand-in declarations, not Apple's headers) | |

**Frames reach iOS as IOSurfaces** in `pump_run`: a `CVPixelBufferPool` with
`kCVPixelBufferIOSurfacePropertiesKey`, format `420f` (the HAL's pictures are BT.601 full range), one copy per
frame from the reader's memory into the surface's planes. (The kernel node is read(), not mmap: a character device
cannot be mapped in this XNU.)

**The hook table for the iPhone18,2 cache**: iOSPlus10's table carries the 20H392 addresses as a guard. This plug-in
finds every class and method by name at run time, so there is no address to re-find; instead its first load in a
session writes to `/private/var/oneios/log/camera.log` which of the nine are in CMCapture 27 (`found`, `the class has
no such method`, `no such class`). That report is the input for the next part.

## Not written

1. **`oneios_camera_device_create()`**: the FigCaptureDevice and FigCaptureStream objects for CMCapture 27 (the
   CoreMedia base-object vtables, the property set the session reads, the formats array as in the device's
   `AVCaptureSession.plist`, the `VideoOutputHandlers` call with a CMSampleBuffer per pixel buffer, a still from
   the stream). Start from iOSPlus10 `camera_device.c`; drop the ISP; one stream per `ONEIOS_CAMERA_*` sensor
   (`oneios_camera_pump_start`). It needs the 24A446 shared cache (CMCapture, CMCaptureCore) on the build machine
   (the SystemOS image is not fetched here) and a guest that runs to SpringBoard to try it.
2. **Loading the plug-in into `cameracaptured`.** On iOS 16 the bundle sat in `/System/Library/Audio/Plug-Ins/HAL`
   and mediaserverd loaded it as an audio driver. `cameracaptured` loads no audio drivers [I], and the system volume
   is sealed. Ways, for userland and the kernel to choose: `DYLD_INSERT_LIBRARIES` for that job (a launchd override
   from `oneios-init`, plus the kernel's owner policy letting dyld honour it for an owner-trusted library), or the
   read-only bind mount of `addon_mounts` over a plug-in folder that process does load.
3. **The device tree nodes** that make SpringBoard show Camera and MobileGestalt answer `still-camera`,
   `rear-facing-camera`, `front-facing-camera`, `video-camera`: `/arm-io/isp` with `camera-rear` and `camera-front`,
   and `/product/camera` [F iOSPlus10, from HakiOS]. **Only together with a working device object**: with the nodes
   and no frame path the Camera app hangs in `-[AVCaptureSession init]` [F iOSPlus10]. Not added to the booter's
   tree.
4. The app's camera permission: tccd's sandbox extension for the camera failed on iOSPlus10's kernel until a patch
   (their 0386 / 2903); whether the hosted kernel needs the same is not known.
5. `cameracaptured` is in the `idle` class of `userland/config/daemons.toml` ("no camera sensors published"); it
   stays enabled, which is what this needs.
