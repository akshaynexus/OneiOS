# Camera (kernel side)

Owner: fable-camera. The whole path: `docs/hardware.md` section 7. Host: `runtime/bridges/camera`. iOS user space:
`userland/camera`.

**State (2026-11-12): compiles with the kernel's own compile records (`drivers/build.py --check`, 16 sources) and is
in the kernel of `HOSTED_OUT=cam`; never run.** The 2026-11-11 phone run had the transport up (`ONEIOS CAMERA: on: 3
cameras`) and nothing in iOS reading it (`userland/camera/README.md`, "Where the camera broke").

| File | What |
|---|---|
| `oneios_camera_proto.h` | the `oneios,camera` virtio device's protocol, shared with xnuhost and the helper: a command queue (`INFO`, `START` role/size/rate, `STOP`), a frame queue of whole-frame buffers (64-byte header, NV12 up to 1920x1080) |
| `oneios_camera.c` (Mach universe) | the transport, after `../bridge/OneiOSBridge.c`: three contiguous frame buffers (9.3 MiB, taken once at start, only when the device is there), commands answered in place, the newest frame handed on and older ones given back |
| `oneios_camera_dev.c` (BSD universe) | `/dev/oneios-camera` (root:mobile 0660): one client; the first read starts the camera, each read gives the newest frame; write selects camera and size; close stops the camera. Its work is the session API of `oneios_camera.h` (`oneios_camera_session_*`), which the user client uses too: one client at a time, either way |
| `OneiOSCameraService.cpp` (IOKit universe) | `AppleH9CamIn` on `IOService:/IOResources/oneios-camera`: the class CMCapture 27's probe matches, so that it loads `/System/Library/MediaCapture/H9ISP.mediacapture` (the plug-in). Its user client `AppleH9CamInUserClient`, which `cameracaptured`'s sandbox profile allows: selector 0 the host's cameras, 1 a command (`struct oneios_camera_command`), 2 the newest frame into the caller's buffer (scalar out: bytes). Boot argument `oneios-camera-plugin=0`: not published |
| `oneios_camera_abi.h` | what a reader sees: the 120-byte `NCFRAME1` header and 24-byte command of iOSPlus10's `/dev/negronicam` and HakiOS's `/dev/hakios-camera`, with format 2 (NV12) and five sensors |
| `oneios_camera.h` | the transport's API and the node's pure parts (command handling, frame checks, the header), which `tests/test_camera_dev.c` runs on the host (37 checks) |

Why a character device first, and then an IOKit service too: iOS finds cameras through Apple's ISP driver and its firmware
protocol, which cannot be imitated; the camera enters through a plug-in in the capture server
(`userland/camera/README.md`): CMCapture 27 loads it itself for the class `AppleH9CamIn`, which `OneiOSCameraService.cpp` publishes. The node stays for `camctl`-style tests and for a reader outside the sandbox; inside `cameracaptured` the plug-in reads through the user client, which its sandbox profile allows.
`OB_CH_CAMERA` of the bridge channel stays reserved and unused: the camera has its own device.

- Device tree: `/arm-io/camera`, `compatible = "oneios,camera"`, `reg`, `droidvm,intid` (or `interrupts`); made by
  xnuhost (`XL_DEV_CAMERA`). Without the node: `ONEIOS CAMERA: off: no /arm-io/camera in the device tree` and no
  `/dev/oneios-camera`.
- Started by `oneios_drivers_start()`. Boot argument `oneios-camera=0` leaves it off.
- Console: `ONEIOS CAMERA: on: N cameras from the host so far, 3 frame buffers of 3110464 bytes, interrupt|polling`,
  then `ONEIOS CAMERA: AppleH9CamIn published: ...`, and `ONEIOS CAMERA: user client opened by NAME` (the first 20).
- The frame's `time` is `mach_absolute_time()` when the reader took it; the sensor's own time (host clock) is in
  `host_time_lo/hi` for intervals.
- Build: in `../build.py`'s list; `../build.py --check` compiles all three. Test: `tests/host.sh` (CI).
