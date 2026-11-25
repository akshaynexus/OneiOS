# The display helper

Puts iOS's frames on the phone's panel during a OneiOS session. xnuhost keeps the guest's frame in a memfd and
serves it on `session/display.sock` (`docs/booter.md` 2.8); One UI's surfaceflinger and composer keep running and
hold the panel. `oneios-display` runs on One UI's side, as root, outside xnuhost's mount namespace: it takes the
frame from the socket and shows it on a surfaceflinger layer above everything else.

**On the phone since the sessions of 2026-11-11** (backboardd's frames on the panel, `README.md` "Status"; the 2026-11-12 23:45 boot logged `1440x3120 buffer, stride 1472, format 5`). The colour handling of 2026-11-13 (dataspaces, half floats) has not run there; its first test is `displayctl test --format rgba16f` below.

What the guest shows comes from its framebuffer driver (`kernel/patches/oneios-hosted/display/`): the kernel
console until `backboardd`'s first frame, then iOS. On the PC, `oneios-tool kernel display-capture` (tools/oneios-tool) is a
client of `display.sock` in this helper's place: it says when the picture changed and writes it as PNGs.

The helper is Rust, the crate `oneios-display` of the runtime's workspace ([runtime/RUST.md](../RUST.md)), ported 1:1
from C on 2026-11-23 (every log line, default and protocol layout). **The Rust helper first ran on the phone on
2026-11-23** (iOS's frames on the screen, the session's `oneios-display:` lines; docs/booter.md 2.6a); every other
phone result below is of the C helper.

```sh
runtime/display/build-android.sh OUT   # OUT/bin/{oneios-display,displayctl,liboneios-display.so}, OUT/lib/oneios-display.dex
runtime/package/elf-check.sh OUT       # the .so: NEEDED libandroid, libnativewindow, libdl, libm, libc; 0 failures
cd runtime && cargo test -p oneios-display   # host: the socket protocol, the fit, the damage rectangles, the copy,
                                             # displayctl, and the choice of the panel's native mode (TestModes.java)
```

`runtime/build.sh STAGE` runs `build-android.sh` and strips the four files into the stage. Needs NDK 28.2, rustup's
`aarch64-linux-android` target, a JDK (`javac`) and `d8` from the Android SDK's build-tools (`ANDROID_SDK_ROOT`,
`ANDROID_HOME`, or the SDK the NDK is in). No `android.jar`: the Java source reaches the framework by reflection only.
`build-android.sh` links the library with `cargo rustc --release --target aarch64-linux-android -p oneios-display
--lib --features jni --crate-type cdylib -- -C link-arg=-Wl,-z,defs -C link-arg=-Wl,-soname,liboneios-display.so`
(`-z defs`: a symbol that none of its libraries has fails the link; the dataspace calls are libnativewindow's),
builds the two programs with `+crt-static` in a target dir of their own (`$CARGO_TARGET_DIR-static`, default
`runtime/target/display-static`), and the dex with javac and d8 (min API 30). `runtime/build.sh` then checks the
library's NEEDED libraries and undefined symbols against the NDK's API 30 stubs (`runtime/package/elf-check.sh`;
`docs/building.md` section 4).

| File | What it is |
|---|---|
| `src/xhd/` | the portable core, shared with xnuhost (which links this crate): `mod.rs` (formats, dataspaces, `Rect`, `fit`, `copy`, `hold_first`), `protocol.rs` (the `display.sock` messages, `check_hello`, `connect`, `send_panel`, `recv_hello`, `drain`/`drain_direct`, `send_done`; xnuhost's side: `listen`, `send_hello`, `send_flush`, `memfd`), `direct.rs` (the direct frames: `Direct`, `Slot`, `Seg`, `copy_direct`), `snapshot.rs` (`Snapshot`, `hash_tile`), `buffers.rs` (`Buffers`); its tests in `src/xhd/tests.rs` |
| `src/xhd/run.rs` | the run loop, portable (2026-11-24): the reader thread (the socket, the snapshot, the answers) and the poster (the layer's buffer, the fill, the post) over a `Layer` trait; host-tested with a stand-in layer (`test_run_*` in `src/xhd/tests.rs`) |
| `src/jni.rs` | `liboneios-display.so` (feature `jni`, Android only: the `Java_OneiOSDisplay_n*` exports): the socket, the mapped frame, the layer's `ANativeWindow` as `xhd::run::Layer`, the stop signals |
| `src/frame_perf.rs` | the bounded, allocation-free frame-timing statistics |
| `src/sys.rs` | the system calls the helper and xnuhost's display share |
| `java/OneiOSDisplay.java` | `oneios-display.dex`: the layers (`SurfaceControl`, `BLASTBufferQueue`), the panel's mode, the placement |
| `src/bin/launcher.rs` | `oneios-display`: finds the dex and the library next to itself and execs `app_process` (same pid) |
| `src/bin/displayctl.rs` | `displayctl`: the phone test; stands in for xnuhost with a moving pattern |
| `tests/displayctl.rs`, `tests/java_modes.rs`, `tests/TestModes.java` | displayctl's checks (no harness: the binary is also the stand-in helper); the native-mode choice on the host's JVM (skipped without javac and java) |

The pixel formats and conversions are xh-core's (`xh_core::pixels`, the mirror of
`runtime/xnuhost/include/xnuhost/pixels.h`). Changes from the C: a direct slot whose row offsets would wrap a 64-bit
sum is refused (EINVAL) where the C let them wrap; a pixel split across two guest pages is swapped whole by
`copy_direct` (the C swapped whole pixels of each piece only). The C files the names below come from (`xhd.c`,
`jni.c`, `launcher.c`, `displayctl.c`, `frame_perf.c`, `tests/test_display.c`) are in git history before 2026-11-23.

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

- **Protocol.** Connects to `display.sock` (waiting for it to appear, `--wait`), works out the panel (below), sends
  `panel {"XHDP", width, height, hz, 0}`, the mode the layers will be placed in, from which xnuhost sizes the guest's
  frame in a session (`docs/booter.md` 2.8), then takes the hello and the memfd and
  maps the frame read-only. The session folder is writable by the guest, so nothing is trusted: the path must be
  a socket (not a link), the sizes are bounded, the stride must cover a row, the frame must fit in the memfd,
  flush rectangles are clipped, unknown messages are ignored.
- **Layers.** The frame's layer has buffers of the guest's size. The guest renders the panel's native mode
  (1440x3120), so in a session the layer covers the panel pixel for pixel: no matrix, no bars, no background
  layer (the log says `the frame is the panel's size: shown 1:1, not scaled`). A frame of another size (a
  mismatch: an override on xnuhost's side, or a panel that stayed in a lower mode) is scaled by the compositor
  to the largest rectangle of its aspect, centred, over a black layer that fills the bars (1320x2868 on
  1440x3120: 1436x3120 at x = 2). Both layers are at the top of the built-in display's stack.
- **The panel.** The modes come from surfaceflinger (`DisplayControl`, the way scrcpy reads them): the active
  one, and the native one, which is the largest the panel has at its highest refresh rate. With `--takeover` (a
  session: system_server is gone) the panel is asked for the native mode in **every** takeover: the display's
  desired mode specs are read, their default mode replaced and their refresh ranges opened to that mode's rate,
  and written back (`SurfaceControl.setDesiredDisplayModeSpecs`). Asked even when the panel is already in that
  mode, because One UI can leave it at its native resolution with the refresh ranges capped at 60 Hz (Motion
  smoothness: Standard), which neither the size nor the mode id shows (ported 2026-11-25 from
  orumanayurpanjayat-ux/OneiOS `84cb2ef`, CREDITS.md). The native mode is told by its id, not only its size
  (1440x3120 at 60 Hz is another mode than 1440x3120 at 120 Hz): when the active id or size differs (FHD+
  1080x2340 is One UI's default resolution setting) the helper waits up to a second (20 x 50 ms) for the mode to
  become active; when it is already active it does not wait. Then the display's projection is set to the active
  mode, unrotated, so the layer space is the panel's own pixels whatever rotation One UI left. If the mode is
  refused the panel stays as it was, and the frame is scaled only when its size differs. The return restarts
  surfaceflinger, which forgets both. The lines (`control/display.log`; `oneios-tool phone log --file display
  --grep 'panel modes|native mode'`): `panel modes: active WxH (mode id A, up to R Hz), native WxH at R Hz (mode
  id N)`, then one of `the panel is in its native mode WxH (mode id N), refresh ranges open to R Hz` (with `(it
  was already in that mode: only the ranges were asked for)` when it was), `the native mode (id N) is not active
  after 1 s: the panel stays at ...` or `the native mode was not asked for: ...`, each saying whether the frame is
  shown 1:1 or scaled. Not booted with this change yet: the proof on the phone is the first line's `mode id` and
  the second line, with the panel at 120 Hz under Motion smoothness Standard (`dumpsys SurfaceFlinger` beside the
  session, or the guest's frame rate). Without `--takeover` nothing of the display is
  changed and the logical size comes from the display manager. The layer votes for the mode's highest refresh
  rate (`setFrameRate`, 120 Hz on this panel), which is the rate the guest's vsync tick runs at.
  Whether One UI's resolution setting is a panel mode or only a logical size on this phone is not known; the
  helper's `panel modes: active ..., native ...` line of the first run says.
- **Frames.** One copy per frame, of the damaged rectangle only: the window copies back what the last buffer had
  and hands out the rectangle to draw. The layer's buffers are in the guest frame's own layout, so the copy is a
  `memcpy`: an 8-bit frame (hello format 1) goes into `BGRA_8888` buffers (format 5; the layer is marked opaque), a
  half-float frame (format 2, xnuhost's `display-format=rgba16f`) into `RGBA_FP16` buffers (format 22). If the phone
  gives no `BGRA_8888` buffer the helper falls back to `R,G,B,X` and swaps while it copies; with no `RGBA_FP16` one
  it falls back to `BGRA_8888` and encodes the half floats to sRGB, clamped (what is outside sRGB is lost). With
  direct frames (the default; `--no-direct` asks xnuhost for the old way) the helper says so in its `panel` report,
  xnuhost hands it guest RAM and a table of frame descriptions (`XHDM`), and a `frame` message (`XHDS`) makes the
  helper read that frame's rows straight from the guest's pages (the compositor's surface, scattered over RAM; the
  table has its segments) into its snapshot and answer `done` (`XHDA`) at once, which is when xnuhost answers the
  guest's flush; then it fills and posts a buffer from the snapshot. Until 2026-11-23 `done` went after the post, so
  the guest's next frame also waited for `ANativeWindow_lock` (a free buffer: to the panel's vsync when the queue is
  full, "buffer wait mean 4.1 ms" on the phone, 2026-11-22) and the fill; now it is drawn while the helper posts
  (`docs/gpu.md` 2.5). **Since 2026-11-24 (fix/frame-path) the read and the post are two threads** (`src/xhd/run.rs`):
  the reader reads each frame into the snapshot and answers as soon as it comes; the poster waits for a buffer, fills it
  from the snapshot under the snapshot's lock (so a post is always one whole frame as read) and posts the newest
  picture, so frames that came while it waited are one post. Before, one loop read a frame only after the post before
  it: the phone's line of 2026-11-23 19:06 had the read start 5.5 ms after xnuhost sent the frame (buffer wait mean
  3.6 ms), so the guest's next frame waited for a vsync on top of the read. Host test `test_run_answers_before_the_post`:
  with every lock of a stand-in layer taking 100 ms, 20 frames are each answered within 50 ms (all of them inside the
  first lock), and every post is one whole frame, in order. `display-post=serial` in xnuhost.conf (docs/switches.md)
  gives the one loop's timing back (`test_run_serial`). Not run on the phone yet. xnuhost copies nothing then. A frame that only xnuhost's own pixels changed is answered after
  its post, as before. The memfd stays for what xnuhost still draws itself
  (the boot console, 2D resources): it is put into the snapshot too. A frame that cannot be read (a slot reused, a
  segment outside RAM, no snapshot: half-float frames or no memory for it) is answered with the failure flag and
  xnuhost copies the frames itself from then on. The guest flushes its whole surface every frame, so the helper finds
  the damage itself: each scanline is read once into the cache, then its tiles are hashed there and copied into the snapshot
  only when their hashes differ ("The snapshot" below). An unchanged frame is answered at once and nothing is posted. A
  changed one: the layer's buffer is locked whole and the helper fills from the snapshot what that buffer lacks ("The
  layer's buffers" below). Protocol: `docs/booter.md` 2.8. Every 10 s the log has `10 s: N frames from xnuhost, U
  unchanged (nothing posted), P posted (M from guest RAM, C% of the rows changed, F% filled); guest pages read in one
  pass for mean ... ms, max ... ms, from ... ms after xnuhost sent the frame (mean; max ... ms); buffer wait mean ...
  ms; copy mean ... ms, max ... ms; posted mean ... ms, max ... ms after the read's answer; B buffers seen (K forgotten); this
  process P% of a core` (the "posted ... after the read's answer" part since 2026-11-24: from the first change the reader
  put into the snapshot to the post that showed it, the buffer wait and the fill), and once `frames come
  straight from guest RAM now, read once into the snapshot; each buffer gets what it lacks`. Pacing is the buffer queue's: the next buffer is handed out when
  surfaceflinger has taken the last, so the loop runs at the panel's vsync and every flush that came meanwhile is
  one post.
- **The first post waits for `session/ready` (since 2026-11-15).** With `--session DIR` the helper posts nothing
  until `DIR/ready` exists (`hold_first`): xnuhost makes it at the guest's first frame of its own that is not
  all black, or `ready-content-wait` seconds (120) after its first black one (`docs/booter.md` 2.8, "the ready
  rule"). Until then the layer is up but has no buffer, so the OneOS boot screen under it stays on the panel. The
  flushes that come meanwhile add up to the damage (the first post is the whole frame anyway); the helper looks for
  the file at every flush and every 250 ms between them, and only until its first post. Before this the helper
  posted the empty memfd (black) at once, a second after `layer up`, then the console's and backboardd's black
  frames: its opaque layer covered the boot screen and the fork ended the boot screen at that `first frame posted`,
  and the panel was black until iOS's Apple logo (phone, main 3233fed, 2026-11-15). Lines: `the first frame waits
  for DIR/ready (xnuhost's first frame of the guest's own with content); the boot screen stays`, then `first frame
  posted (...; held T s for session/ready, N wakes for flushes not shown)`. A helper started again in a session that is ready
  (`oneios-tool phone display-restart`) posts at once. `--socket PATH` (and `displayctl`) never waits. Not run on
  the phone yet; the host test is `test_hold_first`. The bars' black background layer (only for a frame that is not
  the panel's size) is still drawn before the hold: in a session the frame is the panel's size, so there is none.
- **Colour.** The buffers are tagged with a dataspace (`ANativeWindow_setBuffersDataSpace`, libnativewindow's: the
  library links `-lnativewindow` and `-Wl,-z,defs`; without the first, `1f7dd1f`'s library could not be loaded on the
  phone, 2026-11-14: `cannot locate symbol "ANativeWindow_getBuffersDataSpace"`, and the panel stayed on the OneOS
  splash while iOS drew), which says what their
  numbers mean, so that surfaceflinger turns them into the panel's colours: `SRGB` (`0x8810000`) for an 8-bit frame
  (iOS composites this display in sRGB: QuartzCore's `IOMFBDisplay::compositing_color_space` is `kCGColorSpaceSRGB`,
  `kernel/patches/oneios-hosted/display/README.md` "What iOS renders into"), `SCRGB_LINEAR` (`0x18410000`) for half
  floats (linear light, sRGB primaries, extended range: Display P3's colours are components below 0 and above 1). Before
  2026-11-13 the buffers had no dataspace (`UNKNOWN`: surfaceflinger's legacy handling). `--dataspace` overrides it for
  a test (`auto`, `unknown`, `srgb`, `p3`, `scrgb`, `scrgb-linear`). The helper sets no colour mode, no brightness
  and no HDR ratio of its own; a wide dataspace on screen may make surfaceflinger pick another colour mode of the
  panel while the layer is shown (its own choice, undone when the layer goes; the return restarts surfaceflinger).
  It logs what it found and what it set: `panel colour mode N (of ...); HDR types [...], luminance max ... nits, max
  average ..., min ...` (read only), `buffers tagged srgb (dataspace 0x8810000)`, and in `first frame posted` the
  buffer's format and dataspace and the guest frame's pixels.
- **Leaving.** On SIGTERM, SIGINT or SIGHUP it removes its layers and exits 0. When xnuhost closes the socket without
  that (since 2026-11-25), it removes its layers and **connects again**, up to 8 times (`connecting again (N of 8; a guest
  restart in place)`): a guest restart in place (iOS restarting itself, or its kernel starting the Data store over for a
  new seed, the first boot of every new image) re-execs xnuhost, which makes the socket again and waits 10 s for a
  helper. Before, the helper left there, xnuhost logged `display: no helper connected within 10 s`, and iOS ran on with
  nothing on the panel (phone, v0.1.30, the first boot of a 27.2.1 image). When xnuhost ended for good the return kills
  the helper (`oneios_return.sh`); else its `--wait` ends the next connect. Killed with SIGKILL, surfaceflinger drops a
  dead client's layers.

```
oneios-display (--session DIR | --socket PATH) [--takeover] [--wait SECONDS] [--logical WxH]
               [--hz N] [--format bgra|rgbx] [--dataspace auto|unknown|srgb|p3|scrgb|scrgb-linear] [--no-projection] [--no-direct]
```

`--format` chooses among the 8-bit layouts only; a half-float frame always gets `RGBA_FP16` buffers first.

Exit codes: 0 the session ended or it was asked to stop, 2 usage or a missing file, 3 no socket or no usable
hello, 4 the layer's buffers failed, 5 an exception (the framework's classes changed; the trace is on stderr).

It grabs no input device, uses no realtime thread, writes nothing under `/sys/power`, and changes nothing that
outlives surfaceflinger.

## Write-only buffers: `display-present=ahb` (2026-11-25, not booted)

Ported from YumikoHayamato/OneiOS `cdb297bbf` (the S25 Ultra fork of this repository, CREDITS.md; their C in
`jni.c`, rewritten here in Rust: `src/xhd/present.rs`, the NDK half in `src/jni.rs`). The window path's buffers are the
CPU's cached ones (`ANativeWindow_lock` is Surface::lock, which asks for SW_READ_OFTEN and SW_WRITE_OFTEN), and each
lock and each post cleans and invalidates the CPU caches over the whole buffer, whatever was written. Their simpleperf
on the S25 (1080x2340, 60 frames a second): `dcache_inval_poc` 36%, `dcache_clean_poc` 31%, `arch_sync_dma_for_cpu` and
`arch_sync_dma_for_device` 9% each, the copy 10%; with the buffers below 114 frames a second at 17.1% of a core against
60 at 49.5%. Our frame is 1440x3120, 1.8 times theirs; on our phone it is not measured yet.

How: three AHardwareBuffers of the frame's size that the CPU only writes (`CPU_WRITE_RARELY | CPU_READ_NEVER |
GPU_SAMPLED_IMAGE | COMPOSER_OVERLAY`, B,G,R,A, else R,G,B,X) on a child of the layer (`ASurfaceControl_createFromWindow`
on the layer's BLAST surface, so the layer's matrix and position place it), posted with ASurfaceTransaction
(`setBuffer` with the unlock's fence, `setDamageRegion` with the post's change; the first post shows the child, opaque,
with the frame's dataspace and the panel's rate as its frame-rate vote). The run loop's `Buffers` fills each buffer with
what it lacks, told apart by slot (`Locked::key`) instead of by address. At most two transactions are in flight (the
next lock waits for a completion, up to 1 s), and a buffer is written again only after the release fence the compositor
gave when it was replaced (up to 200 ms). The helper never reads the layer's buffers (it fills them from the snapshot),
so buffers the CPU cannot read lose nothing. Half-float frames keep the window path.

The choice: xnuhost's `display-present=ahb` (the default; docs/switches.md) sets bit 1 of the `options` message
(`OPTION_PRESENT_AHB`), which comes right after the hello; the poster reads it before its first lock. In the helper's
own environment `ONEIOS_DISPLAY_PRESENT=ahb|window` wins (a test by hand). Their child came from the Java half
(`ASurfaceControl_fromJava`, Android 14); ours is made by the native half from the layer's window (API 29, their
fallback) because the choice is known only after the hello, at the first post: so OneiOSDisplay.java is unchanged (its
`finally` removes the layer, which takes the child with it).

The fallback: whatever is missing (the option, an 8-bit frame, `AHardwareBuffer_isSupported`, the child, one of the
three allocations; later a lock or a post refused), the helper says why once and posts through the window as before
(`test_present_fallbacks`, `test_present_gives_up`). The way back to One UI: at the run's end the child is hidden and
taken off the layer in one transaction and the buffers are let go (`Present`'s drop, after up to 1 s for what is in
flight); every buffer goes with the process anyway, and the return restarts surfaceflinger.

Lines (stderr, the helper's log): one of `oneios-display: display-present=ahb: 3 write-only buffers (1440x3120
BGRA_8888, CPU_WRITE_RARELY: no CPU cache to maintain) on a child surface of the layer; at most 2 posts in flight`,
`oneios-display: display-present=ahb: WHY: the window path`, or `oneios-display: display-present=window: ...`; a later
`... : WHY: the window path from now on`; at the end `oneios-display: display-present=ahb: N posts, C completed; locks
that waited for a post in flight W (L past 1000 ms), for a release fence F`. The 10 s line is the same as before: its
`buffer wait` is then the wait for a completion or a release fence and the lock, its `copy` the fill. Proof on the
phone: `this process P% of a core` and `copy mean` down against a `display-present=window` boot, frames per 10 s up.
Host tests: `test_present_choice`, `test_present_fallbacks`, `test_present_ring` (the three slots in turn, the first
post whole with its dataspace, the damage), `test_present_in_flight_and_release` (the third lock waits for a
completion, the fourth for its release fence), `test_present_gives_up`, `test_present_run` (xnuhost's option through
the whole run loop onto a fake compositor). This is step 1 of the zero-copy panel pool (`docs/s24-fps-heat-camera.md`).

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
1440x3120 frame the way xnuhost does, starts the helper on it and draws for 5 s: eight colour bars, darker at the
top, inside a white frame two pixels wide, with a white square that bounces. Then it closes the socket; the
helper removes its layer and One UI is back. Nothing of the display's state is changed (no `--takeover`).
The test path launches the helper with `--socket` (never `--session`: a standalone test has no guest, so the
session's `ready` hold would wait for a marker XNU never makes). Extra helper options are allowlisted to
`--hz`, `--format`, `--dataspace` and `--no-projection`; `--takeover`, `--session`, `--socket`, `--lib`,
`--wait` and `--logical` are refused before any helper starts. Each animation step sends one union damage
flush (erased + drawn square together), not two per-rectangle flushes. `src/frame_perf.rs` is a bounded,
allocation-free frame-timing statistics module with a host unit test; helper-side log wiring is still open.

```sh
runtime/display/build-android.sh out/display
adb shell 'mkdir -p /data/local/tmp/oneios-display-test'
adb push out/display/bin/oneios-display out/display/bin/liboneios-display.so \
         out/display/lib/oneios-display.dex out/display/bin/displayctl \
         /data/local/tmp/oneios-display-test/
adb shell su -c 'chmod 0755 /data/local/tmp/oneios-display-test/oneios-display /data/local/tmp/oneios-display-test/displayctl'
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test'
```

What to see: the pattern over everything (status bar and navigation bar included), the white frame whole on all
four sides of the picture, at the very edges of the screen when One UI's resolution setting is WQHD+ (with FHD+
or HD+ the test, which changes nothing of the display, shows the frame scaled to that size), the square moving smoothly, colours in this
order from the left: white, yellow, cyan, green, magenta, red, blue, dark grey. Red and blue swapped means the
buffer format is not what the copy assumed.

What to send back: the command's whole output. It ends with `displayctl: ok` when the helper connected, took
the frames and exited 0 after the socket closed. The helper's own lines (`oneios-display: ...`) say the guest
frame, the panel, the placement, the buffer it got and the number of frames.

Variants:

```sh
# longer, or another frame size (a mismatch: scaled, with bars)
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test --seconds 20 --size 1320x2868'
# options after -- go to the helper: the swapping copy, a fixed refresh rate, another dataspace
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test -- --format rgbx'
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test -- --hz 60'
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test -- --dataspace unknown'
# wide colour: a half-float frame (what xnuhost's display-format=rgba16f serves), see below
adb shell su -c '/data/local/tmp/oneios-display-test/displayctl test --seconds 20 --format rgba16f'
# what surfaceflinger has while it runs (second shell)
adb shell su -c 'dumpsys SurfaceFlinger --list' | grep OneiOS
# clean up
adb shell su -c 'rm -rf /data/local/tmp/oneios-display-test'
```

If the helper hangs, `displayctl` ends it 3 s after the test; if `displayctl` itself is killed,
`adb shell su -c 'pkill -f OneiOSDisplay'` removes the layer.

**Wide colour (`--format rgba16f`).** The same bars in half floats, and in the lowest quarter of the screen eight
patches from the left: sRGB's red, Display P3's red, sRGB's green, P3's green, sRGB's blue, P3's blue (P3's as linear
extended sRGB: components below 0 and above 1), white at 1.0, white at 2.0. What to look for:
- the helper's lines `buffers tagged scrgb-linear (dataspace 0x18410000)` and `first frame posted (... format 22
  RGBA_FP16, dataspace 0x18410000 scrgb-linear ...)`. `no buffer in format 22 RGBA_FP16 (...), using BGRA_8888` means
  this phone's gralloc or surfaceflinger refused half floats: then the band shows each pair the same.
- each P3 patch visibly more saturated than the sRGB patch on its left (red and green most; blue barely). The same
  colour in a pair means the layer was clipped to sRGB. Compare in One UI's Settings > Display > Screen mode both
  "Natural" (colour-managed) and "Vivid" (Samsung stretches every layer to the panel's gamut; say which mode the
  phone was in). A photo of the band and the command's output are what to send back.
- the 2.0 white the same as the 1.0 white is expected: the helper asks for no HDR headroom (no
  `setExtendedRangeBrightness`), so surfaceflinger shows extended range at SDR white.
- the line `panel colour mode ...; HDR types ..., luminance max ...`: the panel's own numbers (what the guest's EDR
  headroom and nits should be set from, `drivers/brightness`).
- after it, One UI's colours, brightness and resolution as before (nothing of the display was changed).

## Not done

- The dataspaces, `RGBA_FP16` buffers and the half-float path (2026-11-13) are host-tested only (the C helper's `make test` then: 110 +
  10 checks; today `cargo test -p oneios-display`): whether this phone's surfaceflinger takes an `RGBA_FP16` BLAST layer, and how One UI's screen modes
  show `SRGB` and `SCRGB_LINEAR` layers, is the wide-colour test above. No HDR headroom is asked for.
- Not every framework call has been seen working on Android 17 (the native-mode request and the projection, `--takeover`, have no log of a run here): the hidden methods used are all
  in `java/OneiOSDisplay.java`, and each has a fallback or a logged failure.
- The copy is not zero-copy: one CPU copy a frame remains (`docs/booter.md` 2.8 says why; `display-present=ahb` takes
  away the cache maintenance of the whole buffer around it, not the copy). The helper maps all of
  guest RAM read-only for it; the pages it touches count in its resident size as well as xnuhost's.
- Direct frames ran on the phone on 2026-11-22 (S24 Ultra, runtime 0.1.15 with the offset fix, `docs/booter.md`
  2.8). Before that fix, xnuhost gave the helper the window's address instead of the address of the RAM memfd's
  offset 0, so every page was read 16 KiB too low and the screen showed flickering stripes of other memory. With
  the fix the picture is correct. The helper's 10 s lines: 24-43% of the rows copied while iOS moves, hash mean
  5.3-5.9 ms, buffer wait mean 4.3 ms, copy mean 0.5-1.0 ms (max 7.0 ms), 9-23% of a core; xnuhost's line: posted
  to in the layer mean 10.4-11.5 ms, 0 not answered within 100 ms. A sign of wrong offsets: 100% of the rows
  copied in every frame on a still screen. Whether answering the flush after the post slows the guest's swaps is
  not measured yet.
- **The snapshot (2026-11-22; row-read optimization 2026-11-24).** The helper's own copy of the picture (`Snapshot`,
  `src/xhd/snapshot.rs`, 18 MB at 1440x3120 and one hash per tile, made before the first post), which every post is
  filled from. A direct frame is read into it when it comes (`Snapshot::update`): each scanline is copied from the
  guest's pages into a full-row scratch buffer once, then divided into tiles and hashed from that same copy. A changed
  tile is copied into the snapshot only when its hash differs. So the hash always says what the snapshot holds and a
  failed partial row cannot publish mixed data. At 1440 pixels wide the helper now uses 24 tiles of 60 pixels each;
  small frames keep the original four-tile layout. This avoids repeated segment-table walks within each row and narrows
  the changed rectangle for localized updates. The change has not been measured on the S24 Ultra yet.
  Before the 2026-11-22 one-pass change, the hash read all 18 MB and then changed tiles were read a second time:
  `guest pages read for mean 5.2-6.0 ms, max 8.6-21.2 ms` on the phone. The earlier one-pass version measured on the
  build host (Apple M4, in the host tests' container, `-O2`, a 1440x3120 frame of stride 5888 in scattered 16 KiB
  pages) at 3.2-3.3 ms a frame where the old hash and copy cost 9.2-9.3 ms (unchanged frame), and 3.6-4.7 ms against
  9.3-10.2 ms with 30% of the rows changed. Those are measurements of the earlier one-pass implementation, not this
  row-read change; the phone's line is still needed for a result. xnuhost's own pixels (the memfd) are put over it
  (`Snapshot::put`), and their tiles lose their hashes, so the next direct frame takes them from the guest again.
  The 10 s line says how long the read took and how long after xnuhost's send it began (`from X ms after xnuhost
  sent the frame`: `XHDS`'s fifth word is the send time since 2026-11-22). The guest does not draw into a frame's
  surface while the helper reads it. Its display driver lands the frame in flight (waits for the flush's answer, which
  xnuhost holds until `done`) before it posts the next one (`DroidVMVirtioGpuDisplay.cpp`, `land()`), and the
  compositor needs that next swap to free the surface again.
- **The layer's buffers (2026-11-22): the fix of "when the UI changes it glitches and updates".** Until then the
  helper locked only the changed tiles' rectangle and left the rest to Surface's copy-back. AOSP's `Surface::lock`
  (`libs/gui/Surface.cpp`) copies back `mDirtyRegion - dirty` from the last posted buffer and keeps `mDirtyRegion`
  by subtracting the handed-out buffer's own last dirty region. That is right for two buffers, but with three or more
  a change posted in another buffer is lost wherever this buffer's old region covered it. The buffer then shows that
  area as it was two or more frames ago, for one frame: a widget or a page that changes flashes back to an older
  picture of itself and then updates. xnuhost's own copy never did this, because the guest flushes its whole surface,
  so every post was whole. Now the buffer is locked whole (Surface copies nothing back) and the helper fills what
  that buffer lacks from the snapshot: the union of what every post changed since it was last filled, and this post's
  change (`Buffers`, `src/xhd/buffers.rs`; C: `struct xhd_buffers`). Buffers are told apart by the address the lock maps them at. A buffer not seen yet,
  or forgotten (more than 8), is filled whole, and so is every buffer when one is not of the frame's size. The 10 s
  line has `B buffers seen (K forgotten)` and `F% filled`. Test: `test_buffers` in `src/xhd/tests.rs` (from `test_display.c`), a model of
  `Surface::lock` on an 8x8 picture through 300 frames of changing rectangles. The old way shows a wrong picture
  with 3 and 4 buffers and none with 2, and the new way none with 1 to 5 buffers, in turn or in any order. How many
  buffers this phone's queue has is not known yet: the phone's line says it.
- The native-mode request (`setDesiredDisplayModeSpecs` by reflection) is unproven like the rest; a refusal is
  logged and the frame is scaled. At 1440x3120 a full-frame copy is 18 MB, 2.2 GB/s at 120 Hz when every frame
  is all damage.
- A guest that truncates the memfd kills the helper (SIGBUS) and the screen goes dark; the session and the way
  back are not affected.
- Direct frames have a handshake (`done`, above), the memfd's flushes have none: xnuhost can write the memfd while
  a row is copied. A guest frame whose GPU work had not ended when it was posted (a fence that retires early: not
  seen, not ruled out) would be read part-drawn. Its tiles then hash as what was read, so the next frame puts the
  finished pixels in.
- Rotation: the picture is the guest's portrait frame; the panel is not rotated.

## When the panel goes dark while iOS draws (2026-11-12)

In the 15:35 session the helper printed `the socket closed after 7 frames` and `layer removed` about a minute into
the boot, while xnuhost went on flushing frames: the panel stayed dark until the helper was started again by hand
(the switch script's own command). Neither side said why. Now:
- xnuhost closes the helper's socket only when the helper is gone (`EPIPE`, `ECONNRESET`, `ENOTCONN`) and logs
  `display: the helper's socket is gone (...): closed`. Any other send error (a full queue, `ENOBUFS`, `ENOMEM`)
  drops that one flush and keeps the socket: `display: a flush to the helper failed (...): dropped, the socket
  kept (N so far)`, once per 1000.
- The helper logs why its read ended: `oneios-display: xnuhost closed the socket` or `oneios-display: reading
  xnuhost's socket failed: <error>`.

**To get the picture back during a session: `oneios-tool phone display-restart`** (`tools/oneios-tool/README.md`,
"The session's own commands"; refused without a session). It ends the helper that `control/display.pid` names
(only a pid whose `/proc/PID/cmdline` is a display helper's; SIGTERM, and SIGKILL after 3 s), starts a new one with
the switch script's own command (the same file and arguments, stdin, `control/display.log`, `control/display.pid`,
OOM score 0, the cgroup `oneios_session`, init's mount namespace), and waits for the new helper's `layer up` and
`first frame posted`, which it prints. `--lib PATH` and `--dex PATH` first put a new `liboneios-display.so` or
`oneios-display.dex` into the staged runtime (`runtime/bin/`, `runtime/lib/`; the SHA-256 printed for the local
file and the phone's, refused when they differ; `runtime.sha256` and `runtime.ids` follow, so the next switch's
preflight takes the new files): the way to try a rebuilt helper without ending the session, as after the library
fault of 2026-11-14 (above, "Colour"). xnuhost and iOS are not touched, and the return ends the new helper by the
same pid file. Tested against the stand-in phone only (`tools/oneios-tool/tests/phone/session-cmds.sh`); not run on
the phone yet.

To get the picture back by hand during a session (as root on the phone, the switch script's own command):
`busybox setsid /data/local/tmp/oneios/runtime/bin/oneios-display --session /data/local/tmp/oneios/session --takeover --wait 60 >> /data/local/tmp/oneios/control/display.log 2>&1 & echo $! >> /data/local/tmp/oneios/control/display.pid`
(the pid appended to `control/display.pid`: the fork's boot-screen hand-off and the return find the helper by it; without it the
OneOS boot screen stays up under the helper's layer, as on 2026-11-13, and shows when the helper ends.)

**The OneOS boot screen and this layer (2026-11-14, read in the fork 3876dd5) [F, I].** The fork's renderer
(`OneOSBootStatus`, `control/bootui/renderer.log`) puts its layer at z 2,000,000,000; this helper's is at
2,147,483,646 and opaque, so it is always above. The helper does not re-create or re-place its layer on a display
off or on, a power-key press or a refresh-rate change: nothing in it listens for them, and iOS's display off is a
black (blank) frame on the same layer. So once the helper has posted a frame, the boot screen can only show through
when this layer is gone, i.e. the helper ended or was killed [I: the 2026-11-13 helper had been started late, by
hand, after the libnativewindow fault, and was not in `control/display.pid`, so the hand-off never ended the
renderer]. The fork's feeder ends the renderer once a helper in `control/display.pid` logs `first frame posted`, or
`session/ready` exists and the helper logged `layer up` (OneOS `switcher/assets/oneios_switch.sh` `display_up`); since
OneOS `fix-bootui-handoff` the switch starts each session with an empty `display.log`, so an earlier session's line
cannot end it early.
**The black screen between the boot screen and the Apple logo (phone, main 3233fed, 2026-11-15), fixed here and in
xnuhost [F for the code, not run on the phone].** Before the fix both of the fork's markers came with black
frames: `first frame posted` a second after `layer up` (the helper posted the empty memfd at once, and the frame
stayed black or the console's), and `session/ready` at backboardd's first frame, which is black. Whichever ended the
renderer, this opaque layer had covered it with black, and it stayed black until iOS drew its logo. Now
`session/ready` comes at the guest's first frame with content (`docs/booter.md` 2.8, "the ready rule") and this
helper posts nothing before it (above, "The first post waits for `session/ready`"), so both markers come with the
logo: the boot screen shows until the logo replaces it. The fork needs no change: its markers, its 30 s rule for a
renderer that never drew, and the dead-man timer (600 s without `ready`) are as they were. If iOS only ever draws
black, `ready` comes `ready-content-wait` (120) seconds after its first frame: then the helper posts that black
frame, the boot screen ends, and the panel is black as before. On the phone, read: in `control/display.log` the
`first frame posted (...; held T s for session/ready ...)` line; in `session/xnuhost.log` `display: the guest's first
frame of its own: all black ...` (where the old rule was ready) and `guest ready: first frame with content, T s and N
frames after the guest's first (black) frame`; in `control/bootui.log` the feeder's `the boot screen ends` line at
the same time, and on the glass the boot screen until the Apple logo.
