# runtime/bridges/camera: the Galaxy S24 Ultra's cameras for iOS

Owner: fable-camera. The whole path: `docs/hardware.md` section 7. Kernel side: `drivers/camera`. iOS user space:
`userland/camera`.

**State (2026-11-11): built, host-tested; one phone run, which listed the cameras and opened one but got no frame**
("The first phone run" below: the cause found in the sources, fixed, the fix not yet run on the phone). The HAL back
end has never run. Facts from sources are marked [F], inference [I], what must be tried [U].

```sh
make test          # 131 host checks (ASan, UBSan); no phone, no Apple files
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
| `backend_ndk.c` | Camera2 NDK (`ACameraManager`, `AImageReader`, YUV_420_888) through cameraserver; starts the process's binder threads; a logical camera's hidden physical cameras (the telephotos) as physical streams; every callback counted and printed |
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
range at that rate, else the nearest). A role goes first to a camera listed by itself, then to a logical camera, last
to a physical camera that is only reached through its logical camera.

## The first phone run (2026-11-11), the cause and the fix

SM-S928B, Android 17 (`CP2A.260605.016.S928BXXU6EZIF`), fork 0.4.75, runtime 0.1.1, root through KernelSU, One UI
running:

```
$ camctl --backend ndk list
back end: ndk, 4 cameras
 0  id 0  wide      back  6.30 mm (35 mm equivalent 22)  sensor 4080x3060  turned 90   logical
 1  id 1  front     front 3.30 mm (25)                   sensor 4000x3000  turned 270
 2  id 2  ultrawide back  2.20 mm (14)                   sensor 4000x3000  turned 90
 3  id 3  other     front 3.30 mm (30)                   sensor 3392x2544  turned 270
 (YUV sizes up to 1920x1080; fps ranges 15-15 24-24 15-30 30-30 15-60 60-60)
$ camctl --backend ndk --camera wide --frames 90 capture
oneios-camera: ndk: camera 0 (wide) 1280x720 at 30-30 fps
camera: id 0 (wide), asked 1280x720 at 30 fps, got 1280x720 at 30 fps
no frame arrived
```

So with One UI running a root client is let in: the camera opened, the session was configured and the repeating
request accepted, with no error. The 3x and 5x are not cameras of their own: they sit behind logical camera 0.

**Cause [F AOSP `android17-release`, I that it is the only one]: the process had no binder threads.** Everything
cameraserver sends a client comes as a binder call *into* the client: the device callbacks (`ICameraDeviceCallbacks`:
capture started, result, error, idle; `frameworks/av/camera/ndk/impl/ACameraDevice.cpp`, `ServiceCallback`) and the
pictures themselves, because the `AImageReader`'s buffer queue lives in the client and cameraserver's request thread
calls its `dequeueBuffer`/`queueBuffer`. An app gets binder threads from zygote. A plain executable has none, and
neither `libcamera2ndk` nor `libmediandk` starts any (no `startThreadPool` in `camera/ndk/impl/` or
`media/ndk/NdkImageReader.cpp`). `openCamera`, `createCaptureSession` and `setRepeatingRequest` still succeed:
cameraserver's calls back *during* a call of ours are served on the calling thread. After that cameraserver's first
`dequeueBuffer` waits for a thread that never comes: no frame, and no error either, since the error callback is a
binder call too. That is exactly the run above. The old code also passed no capture callbacks and printed no session
callback, so nothing showed where it stopped.

What was read and ruled out as the cause [F the same branch, `services/camera/libcameraservice/CameraService.cpp`,
`utils/AttributionAndPermissionUtils.cpp`]:

- uid state: `UidPolicy::isUidActiveLocked` returns true for every uid below 10000 ("Non-app UIDs are considered
  always active"). A root client is never held for not being the top app. An idle uid is not starved of frames
  anyway: cameraserver disconnects it with `ERROR_CAMERA_DISABLED`.
- app-ops: a native client with a uid below 10000 is a "system native client" and gets no `AppOpsManager`
  (`BasicClient`'s constructor), so no camera op is checked or started.
- the camera privacy switch and device policy refuse at `openCamera` (`ERROR_DISABLED`); the open succeeded.
- the reader: `AImageReader_new` already asked for YUV_420_888 with the CPU-read usage; the request already was
  `TEMPLATE_PREVIEW` with the reader's window as its target. Not the cause.
- [I] Samsung's cameraserver may add a policy of its own for native clients. Nothing in the run points to it, and
  it cannot be read; the callbacks now print whatever it sends.

**The fix (`backend_ndk.c`, `backend_hal.c: cam_binder_threads`):**

- the binder thread pool is started before the camera manager is made (`ABinderProcess_setThreadPoolMaxThreadCount(4)`,
  `ABinderProcess_startThreadPool`: platform symbols of `libbinder_ndk`, found with `dlsym` as the HAL back end
  already did for itself). Ordinary threads: no realtime class, no nice value;
- the repeating request carries capture callbacks (started, completed, failed with its reason, sequence ended or
  aborted, buffer lost); the session's (active, ready, closed) and the device's (disconnected, error with its code's
  name) are printed; every NDK status is printed with its name; a failed frame-rate entry is no longer ignored;
- at the stop, when no picture came (or with `-v`), one line with all the counters and a `stopped at:` line that
  names the stage;
- the reader is made with `AImageReader_newWithUsage(..., AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, 4)` (the same usage
  as before, now said). Callback threads: the NDK's own (the camera manager's looper for the device and capture
  callbacks, the reader's looper for pictures); the binder pool feeds both.

**Telephotos:** `ndk_list` adds, after cameraserver's list, each logical camera's physical cameras that are not
listed by themselves (`ACameraMetadata_isLogicalMultiCamera`, i.e. `ACAMERA_LOGICAL_MULTI_CAMERA_PHYSICAL_IDS`; their
characteristics by `ACameraManager_getCameraCharacteristics` on the physical id). They get roles like any camera
(`tele3` below 90 mm equivalent, `tele5` above) and the list shows `physical camera of id 0`. Capturing one opens
the logical camera and makes the stream with `ACaptureSessionPhysicalOutput_create(window, <physical id>)`; the
request is the logical camera's preview template with that window as the target. [U] whether Samsung's HAL gives a
physical YUV stream alone; if `createCaptureSession` refuses it (`ERROR_STREAM_CONFIGURE_FAIL`), the second way is
the logical camera with a zoom ratio: `camctl --camera wide --zoom 3` (or `5`) puts `ACAMERA_CONTROL_ZOOM_RATIO`
into the request and cameraserver picks the lens. `--zoom` is in `camctl` only, not in the helper.

**If frames still do not arrive with binder threads:** the `stopped at:` line and the callbacks say where. Then the
HAL back end is the route with One UI running too (`--backend hal`, the Experimental rows of the fork's screen); it
has never run, so its first run decides.

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
#    -v: the steps and the session's callbacks. -vv: every capture callback and every picture.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk --camera wide --frames 90 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend hal --camera wide --frames 90 --out /data/local/tmp/oneios-camera-wide-hal.png capture'"
adb -s R5CX315X6FT pull /data/local/tmp/oneios-camera-wide.png
adb -s R5CX315X6FT pull /data/local/tmp/oneios-camera-wide-hal.png

# 3. The other cameras, 1920x1080, 60 fps. A role the list does not show: use --camera id:<id> from the list.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera front --size 1920x1080 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera ultrawide capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera tele3 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera tele5 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend hal --camera wide --fps 60 --frames 180 capture'"

# 3a. The telephotos through cameraserver: physical cameras of logical camera 0 (the list shows them after the
#     four listed cameras, with their roles). If the physical stream is refused: the logical camera with a zoom ratio.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk list'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk --camera tele3 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk --camera tele5 capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk --camera wide --zoom 3 --out /data/local/tmp/oneios-camera-zoom3.png capture'"
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -v --backend ndk --camera wide --zoom 5 --out /data/local/tmp/oneios-camera-zoom5.png capture'"

# 4. Through the helper process, as xnuhost uses it (socket, ring descriptor, frame notes).
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --camera wide helper /data/local/tmp/oneios-camera'"

# No camera at all: the transport alone.
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl --backend test capture'"
```

### When no frame arrives: the diagnostic run

Everything at once: every callback and picture from camctl (`-vv`), cameraserver's own state while the stream should
run (`dumpsys media.camera`: the client, the stream, its buffer counts), and the log. Three files to send back:

```sh
adb -s R5CX315X6FT shell "su -c '/data/local/tmp/camctl -vv --backend ndk --camera wide --frames 90 capture > /data/local/tmp/camctl-ndk.txt 2>&1 & sleep 6; dumpsys media.camera > /data/local/tmp/camctl-dumpsys.txt 2>&1; wait; logcat -d -t 20000 > /data/local/tmp/camctl-logcat.txt'"
adb -s R5CX315X6FT pull /data/local/tmp/camctl-ndk.txt
adb -s R5CX315X6FT pull /data/local/tmp/camctl-dumpsys.txt
adb -s R5CX315X6FT pull /data/local/tmp/camctl-logcat.txt
```

On the phone alone (a root shell, the staged runtime's `bin/camctl` or `/data/local/tmp/camctl`), the same line
without `adb ... shell "su -c '...'"`. `camctl-ndk.txt` ends, when no picture came, with for example:

```
oneios-camera: ndk: callbacks: session active 1, ready 0, closed 0; captures started 0, completed 0, failed 0; buffers lost 0; device errors 0; images 0 (unreadable 0)
oneios-camera: ndk: stopped at: the session went active but the sensor started no capture (the HAL holds the request)
```

| `stopped at:` | Meaning |
|---|---|
| `no callback at all after setRepeatingRequest` | cameraserver's calls do not reach this process (the state of the first run), or it never began the request: `camctl-dumpsys.txt` shows whether the client and its stream exist |
| `the session went active but the sensor started no capture` | the request is with the HAL and no shutter came back |
| `captures fail or lose their buffer` | the `capture failed` / `picture buffer lost` lines above it have the frame and the reason |
| `captures start but no result and no picture comes back` | the HAL started the sensor and returns nothing |
| `results arrive without pictures` | the reader's buffer queue: format, usage or size |
| `the device error above` | `device N: error ...` has the code: `ERROR_CAMERA_DISABLED` is a policy (this uid, the privacy switch), `ERROR_CAMERA_DEVICE` the HAL, `ERROR_CAMERA_SERVICE` cameraserver |

A good run prints `frames: 90 read, 0 missed`, `fps: 30.00 by the sensor's time`, `picture: 1280x720 NV12 ... mean luma
<not near 0>` and `wrote /data/local/tmp/oneios-camera-wide.png`; exit code 0. Options: `--camera wide|front|ultrawide|
tele3|tele5|id:ID|INDEX`, `--size WxH` (default 1280x720), `--fps N` (30), `--frames N` (60), `--out FILE.png`,
`--nv12 FILE` (the same frame raw), `--zoom RATIO` (ndk), `-v`, `-vv`. The diagnostic lines go to stderr with the
prefix `oneios-camera:`; the result lines on stdout are unchanged. Close One UI's Camera app first: one client per
camera. For the helper inside xnuhost: `ONEIOS_CAMERA_VERBOSE=1` is `-v`, `ONEIOS_CAMERA_VERBOSE=2` is `-vv`.

What can go wrong, and what it means:

| Line | Meaning |
|---|---|
| `no frame arrived`, and `ndk: stopped at: ...` before it | the table under "When no frame arrives" |
| `ndk: no binder threads (...)` | this Android's `libbinder_ndk` lacks `ABinderProcess_startThreadPool`: no callback and no picture can arrive |
| `ndk: device 0: disconnected ...` or `ndk: device 0: error N ...` | cameraserver took the camera away, with the reason |
| `ndk: ACaptureSessionPhysicalOutput_create failed` or `createCaptureSession failed ... ERROR_STREAM_CONFIGURE_FAIL` for `tele3`/`tele5` | the HAL gives no physical stream alone: `--camera wide --zoom 3` |
| `ndk: openCamera failed ... status -10013 ERROR_PERMISSION_DENIED`, after 10 s | cameraserver could not ask system_server: expected in a session, not with One UI running |
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
