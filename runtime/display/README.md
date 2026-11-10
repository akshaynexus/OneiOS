# The display helper

Puts iOS's frames on the phone's panel during a OneiOS session. xnuhost keeps the guest's frame in a memfd and
serves it on `session/display.sock` (`docs/booter.md` 2.8); One UI's surfaceflinger and composer keep running and
hold the panel. `oneios-display` runs on One UI's side, as root, outside xnuhost's mount namespace: it takes the
frame from the socket and shows it on a surfaceflinger layer above everything else.

**Never run on a phone so far.** It builds, and the host tests pass; the first run is `displayctl test` below.

What the guest shows comes from its framebuffer driver (`kernel/patches/oneios-hosted/display/`): the kernel
console until `backboardd`'s first frame, then iOS. On the PC, `display-capture.py` in that folder is a client of
`display.sock` in this helper's place: it says when the picture changed and writes it as PNGs.

```sh
make               # build/android: oneios-display, liboneios-display.so, oneios-display.dex, displayctl
make test          # host tests: the socket protocol, the fit, the damage rectangles, the copy, displayctl
```

Needs NDK 28.2, a JDK (`javac`) and `d8` from the Android SDK's build-tools (`ANDROID_SDK_ROOT`, `ANDROID_HOME`,
or the SDK the NDK is in). No `android.jar`: the Java source reaches the framework by reflection only.

| File | What it is |
|---|---|
| `src/xhd.c`, `xhd.h` | the portable core: `display.sock` client (and the serving side for the tests), hello checks, damage rectangles, letterbox fit, row copy |
| `src/jni.c` | `liboneios-display.so`: the socket, the mapped frame, the copy into the layer's buffers, the stop signals |
| `java/OneiOSDisplay.java` | `oneios-display.dex`: the layers (`SurfaceControl`, `BLASTBufferQueue`), the panel's mode, the placement |
| `src/launcher.c` | `oneios-display`: finds the dex and the library next to itself and execs `app_process` (same pid) |
| `src/displayctl.c` | `displayctl`: the phone test; stands in for xnuhost with a moving pattern |
| `tests/test_display.c` | the host tests |

## How the layer is made, and why

By a dex that `app_process` runs, on the framework's hidden `SurfaceControl` classes: a `SurfaceControl.Builder`
BLAST layer with no parent, a `BLASTBufferQueue` on it, and the queue's `Surface`. This is what the OneOS fork's
`OneOSBootStatus` does for its entry and return screens (`switcher/patches/OneOSBootStatus.java`), which draw
while the framework is down, so it is the one way already shown to work with surfaceflinger alone on these
phones. `app_process` starts its own runtime; no zygote and no system_server are needed, only surfaceflinger and
servicemanager.

Not native libgui through `dlopen`: `SurfaceComposerClient` and `Surface` are C++ with mangled, unversioned
symbols and class layouts that change between Android releases (this phone is on Android 17), and there is no NDK
call that makes a parentless layer (`ASurfaceControl_create` needs a parent). The Java classes are the same
library underneath with a surface that changes far less; where a method was renamed, the dex falls back.

The frame's path is native all the same. The dex only makes and places the layers; the copy goes through the
NDK's `ANativeWindow` on that `Surface`, from the mapped memfd straight into the layer's gralloc buffer.

## What it does

- **Protocol.** Connects to `display.sock` (waiting for it to appear, `--wait`), takes the hello and the memfd,
  maps the frame read-only. The session folder is writable by the guest, so nothing is trusted: the path must be
  a socket (not a link), the sizes are bounded, the stride must cover a row, the frame must fit in the memfd,
  flush rectangles are clipped, unknown messages are ignored.
- **Layers.** The frame's layer has buffers of the guest's size (1320x2868 by default) and is scaled to the
  panel by the compositor: the largest rectangle of the guest's aspect, centred (on 1440x3120: 1436x3120 at
  x = 2). A black layer under it fills the bars. Both are at the top of the built-in display's stack.
- **The panel.** The active mode and its highest refresh rate come from surfaceflinger (`DisplayControl`, the way
  scrcpy reads them). With `--takeover` (a session: system_server is gone) the display's projection is set to the
  active mode, unrotated, so the layer space is the panel's own pixels whatever resolution setting or rotation
  One UI left; the return restarts surfaceflinger, which forgets it. Without `--takeover` nothing of the display
  is changed and the logical size comes from the display manager. The layer votes for the mode's highest refresh
  rate (`setFrameRate`, 120 Hz on this panel).
- **Frames.** One copy per frame, of the damaged rectangle only: the window copies back what the last buffer had
  and hands out the rectangle to draw. `B,G,R,A` buffers take the guest's bytes as they are (`memcpy`; the layer
  is marked opaque); if the phone gives no such buffer the helper falls back to `R,G,B,X` and swaps while it
  copies. Pacing is the buffer queue's: the next buffer is handed out when surfaceflinger has taken the last, so
  the loop runs at the panel's vsync and every flush that came meanwhile is one post.
- **Leaving.** On SIGTERM, SIGINT or SIGHUP, and when the socket closes (xnuhost ended, or a newer helper
  connected), it removes its layers and exits 0. Killed with SIGKILL, surfaceflinger drops a dead client's layers.

```
oneios-display (--session DIR | --socket PATH) [--takeover] [--wait SECONDS] [--logical WxH]
               [--hz N] [--format bgra|rgbx] [--no-projection]
```

Exit codes: 0 the session ended or it was asked to stop, 2 usage or a missing file, 3 no socket or no usable
hello, 4 the layer's buffers failed, 5 an exception (the framework's classes changed; the trace is on stderr).

It grabs no input device, uses no realtime thread, writes nothing under `/sys/power`, and changes nothing that
outlives surfaceflinger.

## In a session

The OneOS fork (`oneios` branch) starts it from `oneios_switch.sh` once xnuhost runs, from the runtime the
preflight hashed: `runtime/bin/oneios-display --session <stage>/session --takeover --wait 60`, in a session of
its own, in the cgroup `oneios_session`, pid in `control/display.pid`, output in `control/display.log` (the
guest cannot write `control/`). It is not needed for the session to run: with a runtime that has no helper, or
a helper that fails, the session goes on with a dark screen and the way back works as before.
`oneios_return.sh` ends it after xnuhost (SIGTERM, one second, SIGKILL) before it restarts the composer and
surfaceflinger; the watcher kills it before it starts the return; the cgroup kill gets it in any case.

In the runtime package: `bin/oneios-display`, `bin/liboneios-display.so`, `lib/oneios-display.dex`.

## The phone test: `displayctl`

With One UI running, as root. `displayctl` makes a session folder of its own under `/data/local/tmp`, serves a
1320x2868 frame the way xnuhost does, starts the helper on it and draws for 5 s: eight colour bars, darker at the
top, inside a white frame two pixels wide, with a white square that bounces. Then it closes the socket; the
helper removes its layer and One UI is back. Nothing of the display's state is changed (no `--takeover`).

```sh
make -C runtime/display
adb shell 'mkdir -p /data/local/tmp/oneios-display-test'
adb push runtime/display/build/android/oneios-display runtime/display/build/android/liboneios-display.so \
         runtime/display/build/android/oneios-display.dex runtime/display/build/android/displayctl \
         /data/local/tmp/oneios-display-test/
adb shell su -c 'chmod 0755 /data/local/tmp/oneios-display-test/oneios-display /data/local/tmp/oneios-display-test/displayctl'
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test'
```

What to see: the pattern over everything (status bar and navigation bar included), the white frame whole on all
four sides of the picture, thin black bars left and right of it, the square moving smoothly, colours in this
order from the left: white, yellow, cyan, green, magenta, red, blue, dark grey. Red and blue swapped means the
buffer format is not what the copy assumed.

What to send back: the command's whole output. It ends with `displayctl: ok` when the helper connected, took
the frames and exited 0 after the socket closed. The helper's own lines (`oneios-display: ...`) say the guest
frame, the panel, the placement, the buffer it got and the number of frames.

Variants:

```sh
# longer, or another frame size
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test --seconds 20 --size 1179x2556'
# options after -- go to the helper: the swapping copy, a fixed refresh rate
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test -- --format rgbx'
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test -- --hz 60'
# what surfaceflinger has while it runs (second shell)
adb shell su -c 'dumpsys SurfaceFlinger --list' | grep OneiOS
# clean up
adb shell su -c 'rm -rf /data/local/tmp/oneios-display-test'
```

If the helper hangs, `displayctl` ends it 3 s after the test; if `displayctl` itself is killed,
`adb shell su -c 'pkill -f OneiOSDisplay'` removes the layer.

## Not done

- No run on the phone yet, so every framework call is unproven on Android 17: the hidden methods used are all
  in `java/OneiOSDisplay.java`, and each has a fallback or a logged failure.
- The copy is not zero-copy. For that xnuhost would have to keep the guest's frame in a gralloc buffer
  (`AHardwareBuffer`) instead of a memfd and pass that; `virtio_gpu.c` and the socket's hello would change.
- If One UI's resolution setting switches the panel's mode itself (not only the projection), the helper shows on
  that mode; it does not ask surfaceflinger for the 1440x3120 mode.
- A guest that truncates the memfd kills the helper (SIGBUS) and the screen goes dark; the session and the way
  back are not affected.
- The guest can write the frame while a row is copied: tearing is possible, as there is no frame handshake in
  the protocol.
- Rotation: the picture is the guest's portrait frame; the panel is not rotated.
