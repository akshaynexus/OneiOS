# runtime/bridges/camera: the Galaxy S24 Ultra's cameras for iOS

Owner: fable-camera. The whole path: `docs/hardware.md` section 7. Kernel side: `drivers/camera`. iOS user space:
`userland/camera`.

**State (2026-11-11): built, host-tested, never run on the phone.** The phone was not attached. Everything about the
phone below is from sources ([F]) or inference ([I]); what must be tried is marked [U].

```sh
make test          # 130 host checks (ASan, UBSan); no phone, no Apple files
make               # build/android/oneios-camera, camctl, libonecamera.a   (NDK r28c)
tests/host.sh      # both; CI runs it
```

## What it is

```
 camera HAL (Samsung's provider, One UI's kernel)                      guest (XNU in xnuhost)
        |  binder                                                              ^
 oneios-camera  -- NV12 frames -->  memfd ring  -->  xnuhost: oneios,camera  --+-- drivers/camera -> /dev/oneios-camera
 (helper process)  <-- START/STOP -- SOCK_SEQPACKET --  (virtio-mmio device)        -> userland/camera (IOSurfaces)
```

| File | What |
|---|---|
| `helper.c` -> `oneios-camera` | the helper process: xnuhost is static, Android's camera libraries are shared, so (like `oneios-audio`) a small dynamic executable opens the camera. It holds a camera only between START and STOP |
| `backend_ndk.c` | Camera2 NDK (`ACameraManager`, `AImageReader`, YUV_420_888) through cameraserver |
| `backend_hal.c` | the provider HAL (`android.hardware.camera.provider.ICameraProvider/internal/0`, AIDL) called over binder the way cameraserver calls it: libbinder_ndk's C API, parcels written by hand to the AIDL files, our own gralloc buffers (`AHardwareBuffer`) |
| `core.c` | one stream: the back end's picture converted to NV12 straight into the ring; the test back end (a moving picture) |
| `ring.c` | the frame ring: a memfd, 4 places of 1920x1080 NV12, a lock per place so a reader never takes a torn frame |
| `convert.c` | three-plane YUV to NV12 (planar, NV12 and NV21 layouts, row padding), NV12 to RGB/BGRA (BT.601 full range), PNG |
| `meta.c` | Android's `camera_metadata` buffer read without its library; which camera is which lens; size and rate choice |
| `link.c`, `virtio_camera.c` -> `libonecamera.a` | xnuhost's end: starts the helper, maps the ring read-only, and the `oneios,camera` device (commands answered at once; each frame copied once, ring to guest buffer) |
| `camctl.c` | the phone test |

A frame: NV12, at most 1920x1080, with the sensor's timestamp, a sequence number that rises by one per frame the
camera made (a gap = frames nobody read), the camera's role and how it is turned. Selection: camera by role (`wide`,
`front`, `ultrawide`, `tele3`, `tele5`; assigned from each camera's facing and 35 mm equivalent focal length), picture
size (the same if the camera has it, else the largest of the same shape below it), frames a second (the camera's fixed
range at that rate, else the nearest).

## Which route, and why

Both are built; `auto` picks **the NDK while system_server runs, the provider HAL when it does not**.

- **Camera2 NDK through cameraserver does not work in a session** [F AOSP android15-release, I for Samsung's build].
  `CameraService::validateClientPermissionsLocked` (frameworks/av, `CameraService.cpp`) refuses every caller outside
  cameraserver's own process unless `checkPermissionForPreflight(android.permission.CAMERA)` passes; that is
  `PermissionChecker` (frameworks/native `libs/permission`), which asks the `permission_checker` binder service. The
  service is in system_server. With it gone `getService()` waits 10 s, gives up, and `checkPermission` returns
  `PERMISSION_HARD_DENIED`: `ERROR_PERMISSION_DENIED` for uid 0 too. The other two checks would pass: the uid-state
  check (`UidPolicy::isUidActiveLocked`: "Non-app UIDs are considered always active", and "if activity manager is
  unreachable, assume everything is active"), and app-ops (no `AppOpsManager` for a system native client, uid below
  10000 with no package name). The vendor-facing `android.frameworks.cameraservice.service.ICameraService/default`
  ends in the same `connectDevice`, so it is no way round.
- **The provider HAL directly needs nothing of system_server**: it is what cameraserver itself talks to, and what a
  GSI's framework reaches on Samsung's vendor half. Cost: cameraserver does not know the camera is open (it is idle in
  a session, and the return restarts it), and the parcels are ours (interface versions 1 to 3, later fields skipped
  by each parcelable's size).
- With One UI running the NDK is the polite way (cameraserver keeps its book, the privacy indicator shows), and it is
  the cross-check for the HAL back end: the same camera, the same picture.

## Into xnuhost (done: `runtime/xnuhost/src/main.c`, `runtime/loader`)

`br_camera_helper("<runtime>/bin/oneios-camera", backend)` then `br_camera_create(base, spi, camera)` ->
`XL_DEV_CAMERA`, node `/arm-io/camera`, compatible `oneios,camera`. `br_camera_close` at the stop. Environment:
`ONEIOS_CAMERA=0` leaves the device out; `ONEIOS_CAMERA_BACKEND=auto|ndk|hal|test`; `ONEIOS_CAMERA_VERBOSE=1`.
`runtime/build.sh` stages `bin/oneios-camera` and `bin/camctl`. `test` gives the guest a moving picture with no camera.

The session: the helper starts with xnuhost and ends with it (its socket closes; `PR_SET_PDEATHSIG`; it is in the
session's cgroup). It opens no input device, asks for no realtime class or nice value, writes nothing under `/sys`,
and never blocks xnuhost: commands to it are non-blocking sends, a full socket drops a frame note, not a frame.
SIGKILL is enough for xnuhost; for the HAL see "In a session", point 4.

## On the phone: camctl (root, One UI running)

```sh
cd /home/akshay/Documents/GitHub/OneiOS && make -C runtime/bridges/camera
adb -s R5CX315X6FT push runtime/bridges/camera/build/android/camctl runtime/bridges/camera/build/android/oneios-camera /data/local/tmp/
adb -s R5CX315X6FT shell chmod 755 /data/local/tmp/camctl /data/local/tmp/oneios-camera

# 1. The cameras: by cameraserver, then by the HAL (the HAL lists more ids: every sensor and Samsung's hidden ones).
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend ndk list'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal list'"

# 2. 90 frames from the wide camera, one written as a PNG, the frame rate reported. Both back ends.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend ndk --camera wide --frames 90 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend hal --camera wide --frames 90 --out /data/local/tmp/oneios-camera-wide-hal.png capture'"
adb -s R5CX315X6FT pull /data/local/tmp/oneios-camera-wide.png
adb -s R5CX315X6FT pull /data/local/tmp/oneios-camera-wide-hal.png

# 3. The other cameras, 1920x1080, 60 fps. A role the list does not show: use --camera id:<id> from the list.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera front --size 1920x1080 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera ultrawide capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera tele3 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera tele5 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera wide --fps 60 --frames 180 capture'"

# 4. Through the helper process, as xnuhost uses it (socket, ring descriptor, frame notes).
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --camera wide helper /data/local/tmp/oneios-camera'"

# No camera at all: the transport alone.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend test capture'"
```

A good run prints `frames: 90 read, 0 missed`, `fps: 30.00 by the sensor's time`, `picture: 1280x720 NV12 ... mean luma
<not near 0>` and `wrote /data/local/tmp/oneios-camera-wide.png`; exit code 0. Options: `--camera wide|front|ultrawide|
tele3|tele5|id:ID|INDEX`, `--size WxH` (default 1280x720), `--fps N` (30), `--frames N` (60), `--out FILE.png`,
`--nv12 FILE` (the same frame raw), `-v`. Close One UI's Camera app first: one client per camera.

What can go wrong, and what it means:

| Line | Meaning |
|---|---|
| `ndk: openCamera failed ... status -10013` (permission denied), after 10 s | cameraserver could not ask system_server: expected in a session, not with One UI running |
| `hal: no service android.hardware.camera.provider.ICameraProvider/internal/0` | another instance name on this build: `adb shell service list \| grep camera.provider` |
| `hal: open: exception -8, camera status 2` | camera in use (One UI's Camera app, face unlock) |
| `hal: configureStreams ... status 1` | the HAL refused the stream (size, usage or session parameters): send the `-v` output |
| frames start, then `no frame arrived` with the hal back end | the HAL's calls back into camctl are refused: `adb shell su -c dmesg \| grep avc` |
| `mean luma` near 0, `(black ...)` | the lens is covered, or the stream is not what it says |

## In a session (system_server stopped): still to check [U]

A quick stand-in for a session, over USB adb only: `adb -s R5CX315X6FT shell "su -c stop"` stops zygote and
system_server (also surfaceflinger, audioserver and netd: the screen goes dark; cameraserver and the HALs stay);
`adb -s R5CX315X6FT shell "su -c start"` brings One UI back. Between the two, or in a real session:

1. `camctl --backend ndk --camera wide capture`: expected `permission denied` after about 10 s. If it captures,
   Samsung's cameraserver differs from AOSP here and the NDK route works everywhere.
2. `camctl -v --backend hal --camera wide capture`, then `front` and `ultrawide`: the route a session uses. The
   first run on the phone decides whether Samsung's HAL takes a plain YCbCr_420_888 stream with empty session
   parameters from a client that is not cameraserver, and whether it manages buffers itself (`-v` prints it).
3. `camctl list` with no `--backend` must say `back end: hal`; `camctl helper /data/runtime/bin/oneios-camera` in the
   session's namespace.
4. Kill the helper while it streams (`kill -9`), then capture again. If the camera stays "in use", the provider
   kept our session: its init service must then be restarted at the return (oneos asked for the name on the board;
   `adb shell getprop | grep -i 'init.svc.*camera'`).
5. After the return to One UI: the Camera app opens and shows a picture.
6. The sensor's timestamps: whether `fps by the sensor's time` is right for all five (the time base differs per HAL).

## Not done

- Stills at the sensor's full size (the 200 MP sensor's binned 12 MP and up): the transport carries at most
  1920x1080; a photograph is the preview frame. A second, larger stream needs a larger guest buffer or tiles.
- Exposure, focus, zoom and flash controls from iOS (the HAL's own auto modes run); the torch.
- Two cameras at once (one stream; iOS's dual-camera modes).
- The microphone for video is the audio bridge's.
