# userland/camera: the capture plug-in (iOS side of the camera)

Owner: camera (was fable-camera). The host side: `runtime/bridges/camera`. The kernel side: `drivers/camera`. The
whole path: `docs/hardware.md` section 7.

**State (2026-11-12): built for iOS 27 with Xcode (arm64e, 4 KiB pages, `macho-check.py` PASS against the guest's own
24A446 stubs) and run on the Mac against macOS 27's CMCapture (35 checks).** In iOS on the phone (2026-11-22, iOS's log
`out/session-logs/2026-11-22/s7-ioslog`): `Camera driver AppleH9CamIn is active`, `loaded`, `the device made: stream 0
Back Camera (17 formats)`, `stream 1 Front Camera (8 formats)`, `Created FigCaptureDeviceRef`, `user client opened by
cameracaptured`; then CMCapture made no source: `Fig assert: "modelSpecificAVCaptureSessionPlist"`, err -12780,
`Created 0 in-memory built-in backings`. Its per-model folder comes from MobileGestalt's `HWModelDescriptionForCamera`
(`/product unique-model`), which the guest's tree did not have; the loader writes it now with the camera nodes
(`docs/hardware.md` 7, "The camera nodes"), and hosted 0084 removes MobileGestalt's cache once when the tree changes.
Its value is "OneiOSAP" since 2026-11-23 (with "V54AP" AVGestalt's webcam answer stopped all audio): CMCapture's
folder `OneiOS` is hosted 0088's link to `V54` (`docs/hardware.md` 7, "The model name stopped all audio").

**2026-11-23 (runtime 0.1.16, `out/session-logs/2026-11-23/boot-0.1.16`):** the nodes worked (the Camera icon, the
app opens, V54's plist read), and the preview was black: CMCapture freed the Back stream right after its
`BWFigCaptureStream` init (`initializing`, then `dealloc` for `Back`), then `unable to find a stream with port type
PortTypeBack`, err -12780, and every device of the plist `will be disabled`. Cause [F, 24A446 cache]:
`-[BWFigCaptureDevice initWithFigCaptureDevice:deviceID:]` drops every Back, BackTelephoto and BackSuperWide stream
when `FigCaptureExternalCameraReplacesBuiltIn`, which is `FigCaptureIsRunningInVirtualization`
(`kern.hv_vmm_present`). Fix: xnuhost adds `cameracaptured` to `owner-hide-vmm=` (`camera-vm=show` in `xnuhost.conf`
turns it off), so only cameracaptured sees no VM; and the front stream is `PortTypeFrontSuperWide` (V54's front
devices look for that port) with the formats of "Front Ultra Wide Camera". Host-tested, not run on the phone.
Tested on the build laptop: `runtime/host-tests.sh` (all nine parts pass), `runtime/build.sh` (the Android xnuhost),
`userland/camera/build.sh` (arm64e, 4 KiB pages, CDHash `3927e183...`) and `tests/host.sh` (26 reader checks; the Mac
part skipped on Linux).

**What reaches the phone how:** the back camera needs only the new xnuhost (the runtime): the plug-in of the phone's
Data store already gives a `PortTypeBack` stream, and only the VM check dropped it. The front port is in the plug-in,
which lives in the Data store. Since hosted 0090 (2026-11-23) a new plug-in reaches an existing Data store as the boot
addon ("Getting it to the phone"), no new seed.

**2026-11-23, second boot (main 44d9c49; iOS's log `rel-logs/ioslog-int2/ioslog.txt` on the build laptop): the back
stream stayed (the VM fix worked), the preview was black, and cameracaptured crashed again and again.** The causes, each
read in the 24A446 shared cache (`docs/hardware.md` 7, "The second 2026-11-23 boot", has the code addresses):
the stream did not list `ModuleInfo`, so CMCapture had no sensor ID (`Unexpected sensor ID ((null)) found for
PortTypeBack`); no stream format matched a named one (`failed to resolve format 'Video-FES-60'` and all others: the
formats had no front-end scaler output size and no SIFR binning factor); and the Metal plug-in refused
FigMetalAllocator's heap options (0x600200) and had no `newFunctionWithName:constantValues:pipelineLibrary:error:`
(`function is nil`), after which cameracaptured died with SIGSEGV. **Fixes (host-tested on the build laptop, not run on
the phone):** `ModuleInfo` `{sensorID}` from `CameraSetup.plist` `DefaultSensorIDs` (back `0x0903`, front `0x0914`);
the stream formats from `oneios_camera_formats.c` (CMCapture's own rule, checked in `tests/test_formats.c`, and run by
the plug-in over its own list at each create); `x420` and `x422` outputs; the camera opened at the graph's Start only
(it was opened at every stream reservation, for nothing); the two Metal fixes (`docs/gpu.md`, "What the camera needs");
the boot addon (hosted 0090, `oneios-tool ios addon`, `phone addon`).

**2026-11-23 13:45 (boot-jobs, the plug-in by the boot addon; iOS's log `session-logs/boot-jobs/ioslog.txt` on the build
laptop): cameracaptured lived, the plug-in's formats passed its own check (`29 of 29`), and the Camera app still showed
nothing.** What the log shows: the `device: streams asked for (1)` at 08:17:26 and `streams given back (2)` at 08:17:28
are the vendor publishing the device and its 2 s close timer (`Setting up timer to power off ISP ... in 2.00 seconds`),
not a capture session. The Camera app (pid 289, 08:18:15) opened on the front camera (`Photo/FrontWide`), and `Can't add
<AVCaptureDeviceInput: ... [Front Camera]> because the device does not support AVCaptureSessionPresetHigh`: its session
had no input (`connections: count = 0`), so the graph never started a stream. The causes, each read in the 24A446 cache
and the plists ("How CMCapture picks a stream format", "How CMCapture builds a companion"):
- **Every format of the front devices failed** (`failed to resolve format 'Video-16x10-FES-30'`, ..., and the
  `AVCaptureDeviceFormat '1080p30-16x9-422-10Bit' is derived from FigCaptureStreamFormat 'Video-16x10-FES-30', but that
  format couldn't be found` lines): their named formats are in the devices' `deviceInfoBase` files, which the plug-in did
  not read, and "Front Camera" (the plist's "Front Virtual Camera") resolves against the same stream. **Fix:** the front
  stream has the formats of all three front devices with their bases. The 10-bit 4:2:2 (`x422`) output was there already.
- **`failed to resolve format 'Photo[FESCompanionFormat]'`** and the two `[MidFrameSynchronizationOptimizedCompanionFormat]`:
  CMCapture sets `FES` and `OptimizedForMidFrameSynchronization` in those companions itself. **Fix:** its companion table.
- **`failed to copy WhiteBalanceGain (err=-12787)`**, both streams: asked with requireSupported for the source's
  `DefaultWhiteBalanceGains` (`{RedGain, GreenGain, BlueGain}`, integers with 12 fraction bits). **Fix:** listed and
  answered 4096 each (a gain of 1: the picture comes white-balanced).
- **`Fig assert: "pixelFormatIsSupported"`**: the 11 ProRes RAW (`btp2`) device formats; expected, no change.
- **`Failed to open streams for device Back Telephoto Camera`**: no telephoto stream, CMCapture disables that device and
  the dual and triple ones by itself; expected, no change (`OneiOSCaptureDevice.c`, `known`, says why).
Host-tested on the build laptop (`tests/host.sh`: 26 reader checks, 101 formats checks), built for iOS (`build.sh`);
not run on the phone. Whether the Camera app then opens the back or the front camera, and why it chose the front
(`Attempted to change to mode Portrait with an unsupported device (BackWide). Auto device for both positions
unsupported`), is not known.

## Where the camera broke (iOS's own log, sessions of 2026-11-12)

iOS's capture stack never touched our camera. Every start of `cameracaptured` (21 a session) logged, in order:

```
<<<< FigCaptureCommon >>>> fcc_probeCameraDriverService_block_invoke: Camera driver not started in time
<<<< FigCaptureCommon >>>> FigCaptureGetCaptureDeviceCreateFunction: Failed to load CaptureDeviceCreate function
<<<< BWFigCaptureDeviceVendor >>>> -[BWFigCaptureDeviceVendor _createDevice:reason:clientPID:figCaptureDevice:]: Cannot create device without create function!
<<<< FigCaptureSourceBackingsProvider >>>> csu_createSourceInfoDictionariesFromAVCaptureSessionPlistForCaptureDeviceIDs: Error -12786 while creating FigCaptureDevice.
<<<< FigCaptureSourceBackingsProvider >>>> cs_getBackingsForBuiltInCameras: 0 total in-memory backings
```

So every client saw no camera (`callservicesd`: "Supports Back Camera: NO"). The kernel side was up
(`ONEIOS CAMERA: on: 3 cameras from the host so far`), but nothing in iOS read it: the old plug-in was in no image,
and nothing would have loaded it.

## How iOS 27 finds a camera [F, read in the 24A446 shared cache]

- `fcc_probeCameraDriverService` (CMCapture) adds an `IOServiceMatching(name)` first-match notification for each row
  of its table `sCameraDriverInfo` and waits 5 s:

  | Service class | Plug-in that is `dlopen()`ed | Function looked up |
  |---|---|---|
  | `AppleCamera` | `/System/Library/MediaCapture/ISP.mediacapture` | `ISPCaptureDeviceCreate` |
  | `AppleH16CamIn` | `/System/Library/MediaCapture/H16ISP.mediacapture` (in the shared cache) | `H16ISPCaptureDeviceCreate` |
  | `AppleH13CamIn`, `AppleH10CamIn`, **`AppleH9CamIn`**, `AppleH6CamIn` | `.../H13ISP`, `H10ISP`, **`H9ISP`**, `H6ISP.mediacapture` | **`H9ISPCaptureDeviceCreate`** for H9 |

- `FigCaptureGetCaptureDeviceCreateFunction` loads the matched row's plug-in and symbol, and
  `BWFigCaptureDeviceVendor` makes every device with it: `create(allocator, options, &device)` (x0, x1, x2 at the
  call in `_createDevice:reason:clientPID:figCaptureDevice:`).
- So the camera enters **through Apple's own plug-in loader**: no method of CMCapture is replaced, no
  `DYLD_INSERT_LIBRARIES`. The kernel publishes `AppleH9CamIn` (no kext of the collection has that class; the name
  is only in the sandbox profiles), and this plug-in is `/System/Library/MediaCapture/H9ISP.mediacapture`. That
  folder is empty on the sealed OS volume; hosted patch 0056 shows the Data seed's folder over it. `AppleCamera` is
  left alone: the ANE driver and `mobilerepaird` look for it.
- The object layout, read in `H16ISP.mediacapture` (`kH16ISPCaptureDeviceBaseClass`, `kH16ISPCaptureDeviceClass`,
  `kH16ISPCaptureStreamBaseClass`, `kH16ISPCaptureStreamClass`), is HakiOS's iOS 16.5 layout: base class version 1
  (size, equal, invalidate, finalize, description, copyProperty, setProperty, ...), device class version 3
  (ProcessingSessionCreate, RequestStreams, RelinquishStreams, PearlCalibration), stream class version 2 (Start, Stop,
  AddAttachmentsToSampleBuffer).
- Frames: `cameracaptured` runs sandboxed. Its profile lists `AppleH9CamInUserClient` among the user clients it may
  open; `/dev/oneios-camera` is not known to be allowed. So the plug-in reads frames through that user client
  (`drivers/camera/OneiOSCameraService.cpp`), and uses the node only where the user client is not there.

## How CMCapture picks a stream format [F, read in the 24A446 shared cache, 2026-11-23]

- **The sensor ID.** `BWFigCaptureStream` copies the stream's `ModuleInfo` (with requireSupported: a name missing from
  SupportedPropertiesDictionary is answered -12787 by CMCapture itself), and `+[FigCaptureCameraParameters
  sensorIDStringFromModuleInfo:]` makes `[NSString stringWithFormat:@"0x%04x", ModuleInfo[@"sensorID"]]`
  (`kFigCapturePropertyValue_SensorID` of CMCaptureCore). `csu_createMultiStreamSourceFormatsArrayAndAddAVCompressionSettingsAttribute`
  then looks the camera's tables up by port type and that string, and logs `Unexpected sensor ID (...) found for ...`
  when there is none. V54's `CameraSetup.plist` keys its tuning so (`DefaultSensorIDs`: PortTypeBack `0x0903`,
  PortTypeBackSuperWide `0x0972`, PortTypeBackTelephoto `0x0973`, PortTypeFrontSuperWide `0x0914`, ...); the plug-in
  reads that table at the first create.
- **The named formats.** For each name of the device's `MultiStream` `FigCaptureStreamFormats` (an entry with
  `Unsupported` is skipped) `csu_resolveFormatIndexForNamedStreamFormat` (0x1b07b7644) takes the first stream format
  (SupportedFormatsArray, `Experimental` ones skipped) whose keys agree with the entry's:

  | The plist entry | The stream format (CMCaptureCore's key) | Rule |
  |---|---|---|
  | `SensorWidth`, `SensorHeight` | `SensorWidth`, `SensorHeight` | equal |
  | `Binned` | `VideoIsBinned` | equal |
  | `HDR` | `SensorHDRMode` | HDR when the mode is above 0 |
  | `FES`, `FESWidth`, `FESHeight` | `FrontEndScalerOutputWidth`, `...Height` | the format has an output size when FES; the sizes equal when the entry gives them |
  | `MaxFrameRate` | `VideoMaxFrameRate` | equal, when the entry gives one |
  | `SIFREnabled`, `SIFRBinningFactor` | `SIFREnabled`, `SIFRBinningFactor` | equal; with SIFR on both, the entry's factor (else 2 when binned, 1 when not) equals the format's (at least 1) |
  | `QuadraBinningFactor` | `QuadraBinningFactor` | none on either side, or the format's equals the entry's (2 when the entry has none) |
  | `OptimizedForMidFrameSynchronization` | the same name | equal |
  | `SecureSigningFaceIDSupported` | `SensorSigningEnabled` | equal |

  Companions are in "How CMCapture builds a companion" below.
- **The pixel format.** `csu_createSourceFormatDictFromDeviceFormat` asks every output of the resolved stream format's
  `SupportedOutputs` for the device format's `PixelFormatType` (default `420v`; `420f` for some device types): V54's
  device formats (and their `Variants`: `1080p30-422-10Bit`, ...) ask `x420` (36) and `x422` (27), both given, and `btp2`
  (ProRes RAW, 16, not given: a Bayer raw picture cannot be made from the phone's finished frames). So each
  non-experimental `btp2` format gives one `Fig assert: "pixelFormatIsSupported"` (FigCaptureSourceBackingsProvider.m:7470)
  and stays off: 3 of the Back Camera, 8 of "Front Ultra Wide Camera" (11 in the boot of 2026-11-23 13:45, all of them).
- **The devices of a port.** Every device of the plist whose port is a stream's resolves its own named formats against that
  one stream (`Resolving formats for X, using N stream formats`): on the front port "Front Ultra Wide Camera", "Front
  Camera" (the plist's "Front Virtual Camera") and "Front Ultra Wide Metadata Camera". V54's front devices keep their named
  formats in `deviceInfoBase` files of the same folder (`FrontUltraWideCamera-DeviceInfoBase.plist`: 22;
  `FrontVirtualCamera-DeviceInfoBase.plist`: 15; `FrontMetadataCamera-DeviceInfoBase.plist`: 1); the session plist's own
  entry of "Front Ultra Wide Camera" has only its 8 ProRes RAW ones. CMCapture asks both (phone 2026-11-23: it failed the 22
  base names, not the 8). The plug-in reads each device's base too and makes the front stream's formats of all three.

## How CMCapture builds a companion [F, read in the 24A446 shared cache, 2026-11-23]

`csu_createMultiStreamSourceFormatsArrayAndAddAVCompressionSettingsAttribute` (0x1b07b2f7c), after a named format resolved:
a `mutableCopy` of the parent's entry, a few `setObject:forKeyedSubscript:` calls with the companion's entry, then the
resolver with the name parent + `[...]`. "If there": the companion is asked first (a `cbz`); "take": its value is set as
it is, so none removes the key. `oneios_camera_formats.c` has the table (`ocf_companion_rule`), the plug-in runs it on the
plist's dictionaries and the host test on the keys the resolver reads:

| Companion (key of the parent) | Name | Steps |
|---|---|---|
| `FESCompanionFormats` (an array, one each) | `[FESCompanionFormat]` | FES true; FESWidth, FESHeight, MaxFrameRate taken; CaptureStillsFromVideoStream false |
| `NonSIFRCompanionFormat` | `[NonSIFRCompanionFormat]` | SIFREnabled false, SIFRBinningFactor removed; SensorWidth/Height if there; FES, FESWidth, FESHeight, MaxFrameRate taken |
| `FullBinSIFRCompanionFormat` | `[FullBinSIFRCompanionFormat]` | SIFRBinningFactor 2 |
| `MidFrameSynchronizationOptimizedCompanionFormat` (skipped when it has `Unsupported`) | `[MidFrame...CompanionFormat]` | SensorWidth/Height, FES, FESWidth/Height, SIFRBinningFactor if there; OptimizedForMidFrameSynchronization true |
| `ActionCameraCompanionFormat` | `[ActionCameraCompanionFormat]` | SensorWidth/Height, SensorCropWidth/Height, SIFRBinningFactor if there; FES removed |
| `DepthCompanionFormat`, `FSDNetCompanionFormat` | `[DepthCompanionFormat]`, `[FSDNetCompanionFormat]` | FESWidth, FESHeight taken; FES true; SensorWidth/Height, SIFRBinningFactor, MaxFrameRate if there |
| `StereoVideoCompanionFormat` | `[StereoVideoCompanionFormat]` | SensorWidth/Height, SensorCropWidth/Height, SIFRBinningFactor, StereoVideoAEMaxGain if there; FES, FESWidth, FESHeight, CropWidthForFES, CropHeightForFES removed |
| `FaceIDCompanionFormat` | `[FaceIDCompanionFormat]` | SecureSigningFaceIDSupported true |
| `HasSensorHDRCompanionFormat` (a flag of the parent) | `[SensorHDRCompanionFormat]` | HDR true |

`UltraHighResCompanionFormat` is not resolved there; the plug-in keeps a format for it made the old way (its keys over the
parent's). Before this table the plug-in only laid the companion's keys over the parent's, which missed what CMCapture sets
itself: `Photo[FESCompanionFormat]` (V54's companion names a size and a rate but no `FES`) and the two
`[MidFrameSynchronizationOptimizedCompanionFormat]` (no `OptimizedForMidFrameSynchronization` in the plist) did not
resolve on the phone. Checked on V54's own plists (out/, a scratch program over the library, nothing kept): the old way
gives exactly those 3 failures of 28 asked on the back, CMCapture's way none; the front as it was (the 8 ProRes RAW
formats) fails 41 of 49, the three devices with their bases none.

## Files

| File | What |
|---|---|
| `OneiOSCaptureDevice.c` | `H9ISPCaptureDeviceCreate`: a FigCaptureDevice (CMCaptureDevice's class) with a stream for each of the back wide and the front camera that the kernel lists. Each stream's formats are the device's own `CMCapture.framework/V54/AVCaptureSession.plist` stream formats (`FigCaptureStreamFormats` of every device on the stream's port: "Back Camera"; "Front Ultra Wide Camera", "Front Virtual Camera" and "Front Ultra Wide Metadata Camera", each with its `deviceInfoBase` file; read at the first create, every key kept, companions made as CMCapture makes them, so that each AVCaptureDevice format of those files resolves). The HakiOS property set, `ModuleInfo` and `WhiteBalanceGain`. Frames scaled into the pools the graph gives each output (420f, 420v, BGRA) and handed to its `PixelBufferHandler` |
| `oneios_camera_formats.c`, `.h` | plain C: what a named stream format of the plist asks for, the keys of our stream format made from it, CMCapture 24A446's rule for "this format is that name" (`csu_resolveFormatIndexForNamedStreamFormat`, "How CMCapture picks a stream format" below), its companion table (`ocf_companion_rule`, "How CMCapture builds a companion") and `WhiteBalanceGain`'s fixed point |
| `oneios_camera_scale.c` | plain C: the kernel's NV12 frame scaled into 420f/420v, x420/x422 (10-bit video range) or BGRA buffers (moved out of the device file on 2026-11-23, so the host test runs them) |
| `OneiOSCamera.c`, `.h` | the log (`ONEIOS CAMERA: ios: <process>[pid]: ...` to the unified log, `/dev/console` and `/private/var/oneios/log/camera.log`, each where the sandbox lets it), the off switch, a line when it is loaded |
| `oneios_camera_reader.c`, `.h` | the kernel's frames: AppleH9CamIn's user client (`-DONEIOS_CAMERA_IOKIT`, the iOS build) or `/dev/oneios-camera`; checks each header; copies the NV12 planes |
| `build.sh` | `out/userland/camera/H9ISP.mediacapture` and `cdhashes.txt` |
| `tests/host.sh` | CI: the reader against stand-ins (26 checks), the formats and scalers (`tests/test_formats.c`, 101 checks: V54's named formats resolve each to its own format, the 2026-11-23 formats do not, each compare of the rule; each companion of the table on V54's Back Camera numbers, the three that failed on the phone made the old way do not resolve and made CMCapture's way do; the three front devices' formats on one stream, each name to its own, and the 8 ProRes RAW formats alone resolve none of them; the white balance gain of 1), then `host-mac.sh` |
| `tests/host-mac.sh`, `test_device_mac.m` | on a Mac, our create function under macOS 27's own CMCapture: the device is of `FigCaptureDeviceGetClassID`'s class; the stream answers (port, formats from a stand-in plist, pixel size, clock, supported properties); **macOS's `BWFigCaptureDeviceVendor` makes a `BWFigCaptureDevice` over ours with our function**; a frame from a stand-in node reaches the PrimaryScaler's handler, scaled into the test's pool (35 checks; since 2026-11-23 also a `deviceInfoBase` stand-in, an FES companion and `WhiteBalanceGain`, 40 checks: written on Linux, **not run**, it needs the Mac) |

What the property names are worth: they are HakiOS's (iOS 16). (2026-11-25: the stream's output and handler names are iOS 27's
too, in 24A446's CMCaptureCore; the stream now posts its Start and Stop notifications, `oneios-camera-notify`;
`docs/hardware.md` 7, "The stream that never started".) Every property iOS 27 asks for that is not answered,
and every one it sets that is not used, is logged once by name. That list, from the first session with the
plug-in, is the next step.

## Getting it to the phone

```sh
userland/camera/build.sh          # -> out/userland/camera/H9ISP.mediacapture, cdhashes.txt
# (the target: env.sh's IOS_TARGET, arm64e iOS 27 by default; IOS_MIN=16.5 IOS_TARGET=arm64-apple-ios16.5 for a 16.5
# guest, its signature then checked as arm64; the 16.5 CMCapture ABI is not proven)
# then the image build (docs/building.md section 1): the image hook takes out/userland/camera (ONEIOS_CAMERA_PLUGIN=DIR for another)
```

- The image hook (the root tweaks' `camera-plugin` tweak, `oneios-tool image tweaks`) writes it to
  `/private/var/oneios/addons/MediaCapture/H9ISP.mediacapture` in the Data seed and its CDHash to
  `overlay/userland/owner-trust.json` (library validation in `cameracaptured`). **A new seed starts the phone's Data
  store over** (settings, joined Wi-Fi networks, everything iOS wrote).
- **Without a new seed (hosted 0090, 2026-11-23):** `build.sh` also writes `H9ISP.gpuaddon` and its
  `H9ISP.gpuaddon.cdhashes` (`oneios-tool ios addon`); `oneios-tool phone addon out/userland/camera/H9ISP.gpuaddon`
  stages it as `image/kernel.kc.gpuaddon`, with the phone's `owner-trust.json` plus the plug-in's CDHash and a manifest
  for both. At the next boot the kernel writes it to `/private/var/ngpu/camera/MediaCapture/H9ISP.mediacapture` and 0056
  shows that folder instead of the seed's. With a rebuilt Metal plug-in too, one sidecar holds both: `oneios-tool ios
  addon OUT --metal out/userland/NativeAdrenoMetal.bundle --camera out/userland/camera`, then `phone addon OUT`.
  `phone addon --remove` goes back to the seed's plug-ins. The kernel must have 0090.
- The kernel must have AppleH9CamIn and hosted 0056 (one kernel build has both).
- Off without a rebuild: the boot argument `oneios-camera-plugin=0` (no AppleH9CamIn, no mount), or the file
  `/private/var/oneios/camera/off` (the plug-in makes no device).

## What the next session's logs must show

Kernel log (`xnuhost.log`):

```
ONEIOS CAMERA: on: 3 cameras from the host so far, 3 frame buffers of 3110464 bytes, interrupt
ONEIOS CAMERA: AppleH9CamIn published: CMCapture's probe loads /System/Library/MediaCapture/H9ISP.mediacapture
oneios-camera-addon: /private/var/oneios/addons/MediaCapture over /System/Library/MediaCapture: stage 5, status 0 (...)
ONEIOS CAMERA: user client opened by cameracaptured
```

Since the camera nodes (2026-11-22), `xnuhost.log` also has `camera: iOS is told of a rear camera and a front camera
(3 cameras from the helper)`, `identity: <16 hex digits> (/chosen oneios-identity: ...)` and, from hosted 0084,
`oneios-mg-cache: identity ... (marker none): cache removed (0), marker written (0)` on the first boot of an existing
Data store (`(marker the same): cache kept` after it).

Since 2026-11-23 `xnuhost.log` also has `camera: cameracaptured is not told of the VM (owner-hide-vmm gets it): ...`,
`owner-hide-vmm=cameracaptured` in the `placed: ... boot-args` line, and (hosted 0062) `oneios-iokit: cameracaptured[N]
sysctl kern.hv_vmm_present -> 0 (the virtual machine hidden by owner-hide-vmm)`.

Since fix-audio-camera-model (2026-11-23; `docs/hardware.md` 7, "The model name stopped all audio") the model name is
"OneiOSAP" (`camera-model=`), so `xnuhost.log` has `camera: unique-model OneiOSAP (camera-model=): CMCapture's tables
through the kernel's link to V54, ...` and the kernel's `oneios-cmcapture-model: /private/var/oneios/addons/CMCapture
over /System/Library/PrivateFrameworks/CMCapture.framework: OneiOS -> V54 (link made 0), stage 5, status 0`; iOS's log
has no `Fig assert: "modelSpecificAVCaptureSessionPlist"` with it either (CMCapture reads `OneiOS/AVCaptureSession.plist`,
which is V54's), and no `Creating subport config for webcam recording` from audiomxd. With `camera-model=V54AP` there
is no `oneios-cmcapture-model:` mount (`... is there already ...: nothing mounted`) and audio stops again.

iOS's log (and `xnuhost.log` too if the sandbox lets `cameracaptured` write the console): `Camera driver AppleH9CamIn
is active` in place of "not started in time"; `ONEIOS CAMERA: ios: cameracaptured[..]: loaded`; `the device made:
stream 0 Back Camera (17 formats)`; no "Cannot create device without create function"; no `Fig assert:
"modelSpecificAVCaptureSessionPlist"` and a non-zero `cs_getBackingsForBuiltInCameras: Created N in-memory built-in
backings`. MobileGestalt logs no answers: its `still-camera` shows as the Camera icon on the home screen (and, with
`phone pull-data`, in the rebuilt cache). After the device: two `BWFigCaptureStream ... initializing` lines (`Back` and the front one) with no
`dealloc` right after the first, no `unable to find a stream with port type PortTypeBack` and no `Failed to open
streams for device Back Camera`. When the Camera app opens:
`device: streams asked for`, `the graph starts the stream`, `the first frame`, `PrimaryScaler: the first picture
given`. Lines that say where it stops: `error '...' trying to load dylib` (the mount or the trust list),
`stage 1 status 2` (no plug-in in this Data store), `the kernel's camera: Operation not permitted` (the sandbox
refused the user client), `no answer for X` (a property to add).

Since fix-camera-preview-stream (2026-11-23; the kernel with 0090, the plug-in and the Metal plug-in by `phone addon`),
the lines that prove each step, in order. `xnuhost.log`: `oneios-camera-addon: boot addon: N files (-1 none), 1 of the
capture plug-in (camera/), M of the Metal plug-in (addons/)`, then `oneios-camera-addon: /private/var/ngpu/camera/MediaCapture
over /System/Library/MediaCapture: stage 5, status 0`; with M above 0 also `oneios-gpu-addon: /private/var/ngpu/addons over
/System/Library/Extensions: stage 5`, and `oneios-gpu-plugin: ... code signature of ...: error 0` (the CDHashes trusted).
iOS's log (`phone pull-log`):
1. `ONEIOS CAMERA: ios: cameracaptured[N]: .../V54/AVCaptureSession.plist: read; .../V54/CameraSetup.plist: read`.
2. `stream Back Camera: M stream formats for 17 named formats and K companions of the plist's Back Camera; X of X resolve
   by CMCapture's rule (all)`: the plug-in's own check (a name listed after "these do not" is a key to add). Since
   fix-camera-formats (2026-11-23): `.../V54/FrontUltraWideCamera-DeviceInfoBase.plist: read` (and the Virtual and Metadata
   ones), then `stream Front Ultra Wide Camera: M stream formats for 46 named formats and K companions of the plist's Front
   Ultra Wide Camera, Front Virtual Camera, Front Ultra Wide Metadata Camera; X of X resolve by CMCapture's rule (all)`
   (2026-11-23 13:45: 8 named formats, of "Front Ultra Wide Camera" alone).
3. No `failed to copy ModuleInfo`, no `failed to copy WhiteBalanceGain`, no `Unexpected sensor ID`, no `failed to resolve
   format` (13:45 boot: `Photo[FESCompanionFormat]`, the two `[MidFrameSynchronizationOptimizedCompanionFormat]` of the
   back, all 41 of the front), no `Fig assert: "streamFormatDict"` and no `... is derived from FigCaptureStreamFormat ...,
   but that format couldn't be found` (`1080p30-16x9-422-10Bit` was a front format: its `Video-16x10-FES-30` is in the
   front device info base); `pixelFormatIsSupported` 11 times per source lookup, the ProRes RAW (`btp2`) device formats
   only. `Failed to open streams for device Back Telephoto Camera` (and Back Ultra Wide, Back Dual, Back Dual Wide, Back
   Triple, Front Infrared, Front TrueDepth, Back Time of Flight, Back LiDAR, Desk View) stays: CMCapture disables the
   devices whose port has no stream, which is V54's list without them (`OneiOSCaptureDevice.c`, `known`).
   The Camera app: no `Can't add <AVCaptureDeviceInput: ... [Front Camera]> because the device does not support
   AVCaptureSessionPresetHigh`, and its session's `building configuration: ... connections: count =` above 0.
4. No `native-metal: heap refused`; at most one `native-metal: heap options 0x600200: bits 0x600000 are not Metal's known
   ones; made without them`; no `missing NBLibrary newFunctionWithName:constantValues:pipelineLibrary:error:`, no
   `function is nil`; no `exited due to SIGSEGV` for cameracaptured.
5. With the Camera app open: `device: streams asked for (1)`, `stream Back Camera: format N`, `stream Back Camera: the
   graph starts the stream (format N)`, `stream Back Camera: reading frames through AppleH9CamIn`, `stream Back Camera: the
   first frame: 1920x1080, camera 0, turned 90`, `Back Camera PrimaryScaler: the first picture given (WxH '420f' from
   1920x1080)`: a picture on the screen. A photo: `stream: a photograph asked for StillImageCaptureNow` and `stream Back
   Camera: the photograph is given`.
6. Before the app opens, no `reading frames through AppleH9CamIn` (the camera opens at the graph's Start only).
If cameracaptured still crashes, boot with `sheng-exc-name=newfs_apfs,cameracaptured` in `xnuhost.conf`'s boot
arguments: the kernel logs each exception's pc, lr, far and return addresses (give them to `oneios-tool sym`).

## Not done

1. Run in iOS. Whether the 27 capture graph takes this device as it is, what it asks that is missing (the log
   lists it), and whether it needs more of HakiOS's replaced methods (stabilisation, noise reduction, the portrait
   prewarm): not known.
2. The front camera: since 2026-11-23 the port `PortTypeFrontSuperWide` and the formats of V54's three front devices
   with their device info bases (4896x4896 sensor formats of a square sensor; the phone's front camera gives 4:3 pictures
   up to 1920x1080, scaled). Not run with the bases (boot-jobs ran it with the 8 session formats only: no front format).
3. The device tree nodes for MobileGestalt (`/arm-io/isp` `camera-rear`, `camera-front`, `/product/camera`,
   `/product unique-model`): added 2026-11-22 (the Camera icon was not there without them: Camera.app's `SBAppTags`
   is `still-camera`). iOSPlus10 saw the app hang with the nodes and no frames on iOS 16; on 27 the source list needs
   `unique-model` first. Not run on the phone yet.
4. Stills above 1920x1080; focus, exposure and zoom from the sensor; the torch; two cameras at once.
5. The other cameras, mapped to the iPhone18,2's (V54's plist): the ultra-wide (the helper's `ultrawide`, camera 2) as
   a `PortTypeBackSuperWide` stream with "Back Ultra Wide Camera"'s formats; a telephoto (the 5x `tele5`, a physical
   camera behind logical camera 0) as `PortTypeBackTelephoto` with "Back Telephoto Camera"'s formats (V54's telephoto
   is 4x; the 3x has no iPhone place and stays a zoom of the wide). The virtual devices (Back Dual Wide, Back Triple)
   need `SupportedSynchronizedStreamsGroups` with two or three streams that run at once: the kernel's camera has one
   client and one camera at a time, so that needs streams of more than one camera in `drivers/camera` and the helper.
   With each stream, its `/product/camera` flag (`rear-cam-superwide-capability`, `rear-cam-telephoto-capability`).
6. ~~A way to put a new plug-in into the phone's Data store without a new seed~~: done 2026-11-23 (hosted 0090, `phone
   addon`); not run on the phone yet.
7. ~~`WhiteBalanceGain` and the companions' parameters~~: done 2026-11-23 (fix-camera-formats), not run on the phone.
   `WhiteBalanceCalibrations` (asked without requireSupported, for the source's `WhiteBalanceCalibrations`) is not
   answered; the next log says whether anything needs it.
