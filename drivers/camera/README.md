# Camera (kernel side)

Owner: fable-camera. The whole path: `docs/hardware.md` section 7. Host: `runtime/bridges/camera`. iOS user space:
`userland/camera`.

**State (2026-11-11): compiles with the kernel's own compile records and links against `xnu.macho`; never run.**

| File | What |
|---|---|
| `oneios_camera_proto.h` | the `oneios,camera` virtio device's protocol, shared with xnuhost and the helper: a command queue (`INFO`, `START` role/size/rate, `STOP`), a frame queue of whole-frame buffers (64-byte header, NV12 up to 1920x1080) |
| `oneios_camera.c` (Mach universe) | the transport, after `../bridge/OneiOSBridge.c`: three contiguous frame buffers (9.3 MiB, taken once at start, only when the device is there), commands answered in place, the newest frame handed on and older ones given back |
| `oneios_camera_dev.c` (BSD universe) | `/dev/oneios-camera` (root:mobile 0660): one client; the first read starts the camera, each read gives the newest frame; write selects camera and size; close stops the camera |
| `oneios_camera_abi.h` | what a reader sees: the 120-byte `NCFRAME1` header and 24-byte command of iOSPlus10's `/dev/negronicam` and HakiOS's `/dev/hakios-camera`, with format 2 (NV12) and five sensors |
| `oneios_camera.h` | the transport's API and the node's pure parts (command handling, frame checks, the header), which `tests/test_camera_dev.c` runs on the host (37 checks) |

Why a character device and not an IOKit service: iOS finds cameras through Apple's ISP driver and its firmware
protocol, which cannot be imitated; the camera enters through a plug-in in the capture server
(`userland/camera/README.md`), and the plug-in that exists for that route reads this node's layout.
`OB_CH_CAMERA` of the bridge channel stays reserved and unused: the camera has its own device.

- Device tree: `/arm-io/camera`, `compatible = "oneios,camera"`, `reg`, `droidvm,intid` (or `interrupts`); made by
  xnuhost (`XL_DEV_CAMERA`). Without the node: `ONEIOS CAMERA: off: no /arm-io/camera in the device tree` and no
  `/dev/oneios-camera`.
- Started by `oneios_drivers_start()`. Boot argument `oneios-camera=0` leaves it off.
- Console: `ONEIOS CAMERA: on: N cameras from the host so far, 3 frame buffers of 3110464 bytes, interrupt|polling`.
- The frame's `time` is `mach_absolute_time()` when the reader took it; the sensor's own time (host clock) is in
  `host_time_lo/hi` for intervals.
- Build: in `../build.py`'s list; `../build.py --check` compiles both. Test: `tests/host.sh` (CI).
