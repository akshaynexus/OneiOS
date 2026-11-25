# S24 Ultra: frame rate, CPU heat and the camera (branch fix/s24-fps-heat-camera)

The owner's goal of 2026-11-25: on the Galaxy S24 Ultra (iOS 27.0.1 24A446) raise Metal compositing to 60 fps, then
120 (today 40-58 fps on the idle lock screen, 20-32 in launches and Control Center, about 10 fps in app open and close
animations); bring the CPU down from every cluster at its top clock and a 92-95 C SoC; and get a camera preview that is
not black. Use what other ports on the iOSOnAnd tracker already solved.

## The fixes on this branch (2026-11-25; built and host-tested on the build laptop, not booted)

Each is one switch in docs/switches.md, at its new behaviour by default (that file's rule); none has run on a phone.
"Before" is the switch's other value.

| Fix | Where | Switch (default; before) | Proof lines on the phone |
|---|---|---|---|
| **GPU memory: the pools' room.** metal2vulkan kept a 32 MiB free range in each pool and made a 64 MiB block whenever no block had it in one piece (backboardd: 440 MiB pooled, 264 MiB placed). Now 8 MiB | `userland/metal/KeepRoom.inc` (sets `METAL2VULKAN_KEEP_ROOM_MIB`, Droid-VM patch 0027) | `oneios-keep-room=8`; `0` | the `GM p A/BM u CM` rows: B nearer C; `oneios-mem: free` above the 1-36 MiB of the gpumem log; `oneios-perf: kernel_task` `VM_pageout_scan` and `VM_compressor` us per 30 s down; `oneios-wired` GPU blobs lower |
| **Animation rate: UIKit's links.** The in-process animation manager (AnimationKit's in 24A446) asked 20..120, 30 preferred; its links now run at the display's rate. Ported from UniXNU-zizhan 42629e5 | `userland/metal/DisplayLinks.inc` | `oneios-dl-full=1`; `0` | iOS's log: `OneiOS display links: hooked (made M fps 1 range 1)` in SpringBoard, then `... SpringBoard in-process animation link at the panel's rate (asked 20..120, 30)` (or the `kept: CLASS asks ...` lines naming the class to match if not); xnuhost's `frame: 10 s` and `display: 10 s` frame counts during swipes |
| **Animation rate: the panel's range.** The guest's display offered 24..120 Hz; now 60..120 | `staged/display.rs` LEGACY (NativeLegacyDisplay.cpp `getRefreshAttributes`), default in `OneiOSDisplay.cpp`; kernel | `sheng-vsync-min=60`; `24` | the kernel's `oneios-display: refresh range 60..120 Hz` (once) |
| **ASTC on.** `/product graphics-featureset-fallbacks` keeps APPLE2 and MTL2,2 (DeviceSupportsASTC): ASTC images go to the GPU, not ImageIO on the CPU (zizhan 46.9 -> 88.6 moving fps) | xnuhost `astc` -> `Machine::astc` (`runtime/loader/src/machine.rs`); the A56's table entry `astc=0` | `astc=1`; `0` | xnuhost: `gpu: astc=1: /product graphics-featureset-fallbacks keeps APPLE2 and MTL2,2 (DeviceSupportsASTC)`; icons, wallpapers and app-switcher cards drawn (a screenshot); where CoreUI lays ASTC data out through Metal, iOS's log has `native-metal: texture layouts are DroidVM's linear ones (block-compressed formats; format N first)` once a process |
| **App snapshots not ATX.** With ASTC on, SplashBoard would compress each snapshot to ASTC on the CPU (blank switcher cards, the 10 s scene watchdog). JPEG or PNG instead, from ios-garnet | `userland/metal/SnapshotFormat.inc` (SpringBoard) | `oneios-snapshot-astc=0`; `1` | iOS's log: `OneiOS snapshots: JPEG or PNG, not ATX (hooked)`; no app killed by the scene-update watchdog when it leaves the screen |
| **Camera: the compile deadline.** cameracaptured's compute pipelines failed `Compile(Budget(Time))` at 20 s; every client but backboardd now has 120 s | `userland/metal/m2v-compile-deadline.patch` (`Budget::standard`), `CompileDeadline.inc` | `oneios-compile-s=120`; `0` (20 s) | no `oneios-gpu-text: cameracaptured[N] problem compute pipeline ...: Compile(Budget(Time))` |
| **Camera: the stream's notifications.** StreamStarting/Started around a Start, StreamStopping/Stopped around a Stop, as H10ISP's; from Jesvi-iOS | `userland/camera/OneiOSCaptureDevice.c` `post()` | `oneios-camera-notify=1`; `0` | `ONEIOS CAMERA: ios: cameracaptured[N]: stream: posted StreamStarting`, `... the graph starts the stream (format N)`, `posted StreamStarted`, `... the first frame`, `PrimaryScaler: the first picture given`; xnuhost `virtio: 60 s` with `camera#0 q0 +N` while the app is open |
| **The display helper's buffers write-only** (round 2). Each `ANativeWindow_lock` and post cleaned and invalidated the CPU caches over the whole 18 MB buffer (85% of the helper's time on the fork's S25); now three `CPU_WRITE_RARELY` AHardwareBuffers on a child of the layer, posted with ASurfaceTransaction, falling back to the window by itself. From YumikoHayamato/OneiOS cdb297bbf (CREDITS.md), rewritten in Rust | `runtime/display/src/xhd/present.rs`, `src/jni.rs`; xnuhost's `options` bit 1 (`runtime/xnuhost/src/display.rs`) | `display-present=ahb`; `window` | the helper's `oneios-display: display-present=ahb: 3 write-only buffers (1440x3120 BGRA_8888, ...) on a child surface of the layer` (or `...: WHY: the window path`); its 10 s line's `this process P% of a core` and `copy mean` down and frames per 10 s up against a `display-present=window` boot; at the end `display-present=ahb: N posts, C completed; ...` |
| **GPU hang at a render server's start** (round 2). Turnip cut a submission of more than 128 buffers every 128, sometimes inside a command buffer, which then ran on BV and BR without its thread control: `gmu: GPU hang detected`, the guest stalled 1-2 s (Droid-VM: 5-10% of backboardd's starts). From Droid-VM/UniXNU fb9fb47 | `userland/metal/turnip-submit-split.patch` (after `turnip-idle-bo-cache.patch`) | none (a hang fix) | no GPU hang or fault in xnuhost's `gpu:` lines and KGSL's log at a render server start (`oneios-tool log gpu`); no `darwin: N command buffers' streams with no beginning among them` in iOS's log. Turnip's build id changes: every driver pipeline compiles once at its first use after this add-on |
| **cameracaptured's crash in the reflection** (round 2). A compute pipeline rebuilt with its unset constants zero freed its function before `nmb_compute_pipeline_reflection` read it (SIGSEGV at 0, CCPortrait's kernels). From Droid-VM/NativeMetal 3c46b5e | a frontend edit in `ios metal-stage-edit` (`frontend_camera_edits`) | none (a crash fix) | no cameracaptured crash report naming `nmb_compute_pipeline_reflection`; no `oneios-kill` exit of cameracaptured with the Camera app open |

The two Metal switches `oneios-keep-room` and `oneios-compile-s` act in the default (624bab3) add-on only; the rt
runtime has neither (userland/metal/rt/README.md). Every switch is in xnuhost's `switches:` line.

**A lasting tool:** `oneios-tool sym cstr`, `sym ptrs`, `sym refs`, `sym dis` (tools/oneios-tool/README.md "sym"):
strings, pointers (both pointer forms; iOS 27's are offsets from the cache's base), adrp pairs and code of the dyld
shared cache, named.

**Read and closed (no change):**
- The camera's names. `VideoOutputConfigurations`, `VideoOutputsEnabled`, `VideoOutputHandlers`, `PixelBufferHandler`,
  `BufferPool` and the outputs `PrimaryScaler`, `SecondaryScaler`, `StillImage` are in 24A446's CMCaptureCore
  (`oneios-tool sym cstr`, `out/ios27-cache`): the plug-in's names are iOS 27's. iOS 27 adds outputs (`IntermediateTap`,
  `FrontEndPyramidScale0/1`, `StillImageRaw`, `PointCloud`, ...), said once by name as `stream: an output not here:`.
- The swap fence. Our guest display takes the render server's work number at the swap (`DroidVMVirtioGpuDisplay.cpp`,
  the offer reads `droidvm_virtgpu_submitted()`), the fence zizhan's 49dee66 moved to; nothing to port.
- `+[XBApplicationSnapshotManifestImpl _outputFormatForSnapshot:]` is a class method in 24A446 (its block's symbol),
  so the ios-garnet hook applies.

**Tests (build laptop):** `runtime/host-tests.sh` without QEMU (no qemu part): lint, lint-android, port, links and package pass; the workspace (`cargo test --workspace --no-fail-fast`) 397 of 398, the one failure `audio::test_audio_helper::audio_helper` (1 underrun: a test that needs the host to run its threads on time; it failed under a load average of 21 from other agents' builds, passed in this branch's first run, and this branch changes no audio code); `kernel/tests/host.sh` 13 of 13;
`cargo test -p oneios-sym` 9 (the new `strings_pointers_and_page_pairs`) and `-p oneios-kernel` (the golden test
stages LEGACY into the pinned NativeLegacyDisplay.cpp); `userland/camera/tests/host.sh` (reader 26, formats 101 checks);
`runtime/loader/tests/loader.rs` `machine_identity_tree` (both feature-set lists), `config::tests::host_device` (the
A56's `astc=0`, a command line's `astc=1` wins) and `config::tests::the_switches` (the line with the seven new words).

**Round 2 (2026-11-25, after the merge of main a1200e5; build laptop):** `runtime/host-tests.sh` without QEMU:
workspace 404 passed, 0 failed (`audio_helper` passed in it, and again run alone: `cargo test -p onebridges
audio_helper`, 2 passed, load 4.6 at its start; its one failure of round 1 was the host's load, as said), lint,
lint-android, port, links (the display library with its AHardwareBuffer and ASurfaceTransaction imports links with `-z
defs`), package pass; the display helper's six `test_present_*` and xnuhost's `the_serial_post_option` and
`the_switches` among them (docs/switches.md "Tests"); `cargo test -p oneios-ios` 22 (the camera edits with 3c46b5e's);
`userland/camera/tests/host.sh` (reader 26, formats 101 checks; the camera code did not change). No kernel file changed
since e84a46e (the merge brought only `kernel/release-kernel.txt`, the A56 release pin), so no kernel build and no
`kernel/tests/host.sh`. Built: the Metal add-on in the worktree's `out/metal` (`userland/metal/build.sh`, its stage
reset the staged translator once for the new patch order: docs/switches.md "Found while building the Metal
plug-in"); the runtime package 0.1.28-dev at 8ded46c (`runtime/package.sh` with the s24fps kernel, the plug-ins).

### The zero-copy panel pool (step 1 in code since round 2; steps 2 and 3 a plan)

Today the display helper copies each changed frame from guest pages into the layer's buffer on the CPU (up to 18 MB,
5.5-6.8 ms a frame; docs/gpu.md 2.5 item 2). zizhan's panel pool (128 MiB: 1 copy buffer and 6 zero-copy slots) is
the DPU scanning out the render server's own surfaces. Ours, in three steps:

1. **The helper's own buffers, posted with ASurfaceTransaction: done in code (round 2, not booted).** The helper
   allocates three AHardwareBuffers of the frame's size and posts them on a child of its layer with
   `ASurfaceTransaction_setBuffer` and the unlock's acquire fence instead of `ANativeWindow_lock`/`unlockAndPost`
   (`runtime/display/src/xhd/present.rs`, switch `display-present=ahb|window`, the default `ahb`). YumikoHayamato's
   measurement made this the first step: the helper's time was the CPU caches' maintenance of the window's buffer, not
   the copy, so the buffers are `CPU_WRITE_RARELY | CPU_READ_NEVER | GPU_SAMPLED_IMAGE | COMPOSER_OVERLAY` (the CPU
   still fills them; no cache to maintain). What step 2 adds to it: `GPU_FRAMEBUFFER` usage (a GPU writes them) and
   the buffers' dma-buf fds to xnuhost over `display.sock` (SCM_RIGHTS, a new `XHDB` message after the hello); the
   slot ring, the release fences and the fallback to the window stay as they are.
2. **A GPU copy instead of the CPU's.** xnuhost imports each fd into KGSL (`IOCTL_KGSL_GPUOBJ_IMPORT`, dma-buf type)
   beside the guest surface's blob (already a KGSL object, drm2kgsl's), and at each flush submits a 2D blit from the
   surface to the next free pool buffer on drm2kgsl's own context; the helper gets the buffer's index and a sync-file
   fence (`KGSL_TIMESTAMP_EVENT_FENCE`) to post with. Removes the 5.5-6.8 ms read and the snapshot's tile hashing; a
   guess: the blit is well under 1 ms at the GPU's bandwidth. Cost: one GPU job a frame on a GPU the render server is
   using.
3. **No copy.** The render server's surfaces themselves as the pool: SET_SCANOUT_BLOB of a pool buffer exported to the
   guest as a blob, so backboardd renders into what the panel shows (zizhan's zero-copy slots). Needs the guest to
   allocate its framebuffer surfaces from that pool (IOSurface backing in the virtio GPU path) and the helper to hold a
   buffer until the guest's next swap retires it. Larger; only after 2 is measured.

Steps 2 and 3 get a switch of their own (`display-zero-copy=0|1`) when they are code; step 1's is `display-present`.
Measure step 1 first (a boot with `ahb` against one with `window`): if the helper's share of a core and its copy time
fall as on the S25, the CPU copy left is the 5.5-6.8 ms read of the guest's pages, which step 2 removes.

Watch zizhan's warning: answering the swap before the latch (their `pong_zc_async=1`) took vsync pacing away from
backboardd (frames over 25 ms 7% -> 31%). Step 2 answers when the blit is queued, as `display-post=apart` does now;
compare the `frame:` line's interval p95 against `display-post=serial` in the first boot.

### What the Mac stages (not booted)

Everything is in `out/handoff/s24fps-8ded46c/` on the build laptop (README.txt, SHA256SUMS of its 48 other files; 231 MB):
copy the folder to the Mac. It replaces `s24fps-e84a46e` (removed): the same kernel, a new runtime and add-on. The
integration build (`out/handoff/integration2-d3b802f`) boots first; this one after it.

```sh
H=out/handoff/s24fps-8ded46c
oneios-tool phone sums                                   # note the phone's kernel.kc sha256 (to stage it back)
oneios-tool phone stage runtime-asis $H/runtime           # runtime 0.1.28-dev, commit 8ded46c, xnuhost 44bbc51c
mkdir -p stage && cp $H/image/kernel.kc $H/image/kernel.kc.hashes stage/
oneios-tool phone manifest stage --stage                 # kernel.kc a821fd1e (.hashes 196e4c94), xnu.macho 8971ecc9;
                                                         # unchanged since e84a46e: not needed where phone sums shows it
oneios-tool phone addon $H/addon/default.gpuaddon --cdhashes $H/addon/default.gpuaddon.cdhashes
                                                         # NativeAdrenoMetal 4054ab4c, Turnip 90adff87, H9ISP 59c042c5
oneios-tool phone switch --version-code N                # then: phone log --save, phone pull-log DIR
```

`overlay/userland/xnuhost.conf`: nothing to add for the defaults (every fix is on; since the merge into main 2026-11-25 `oneios-m2v-shader-fix` too). The
first boot runs them all with `display-post=apart` and `display-present=ahb`; a second boot with
`display-present=window` is the display helper's comparison (its share of a core, copy time, frames per 10 s), a third
with `display-post=serial` the frame-pacing one. The first boot after this add-on recompiles every Turnip driver
pipeline once (the patch changes Turnip's build id). To pin a fault, one switch off (`$H/xnuhost.conf.switches` lists
them all): `astc=0` (and then `oneios-snapshot-astc` does nothing), `display-present=window`, or one line
`boot-args-add=oneios-dl-full=0 sheng-vsync-min=24 oneios-keep-room=0 oneios-compile-s=0 oneios-camera-notify=0` with
only the words wanted. The Turnip split and the reflection fix have no switch (a hang and a crash fix).

The lines that prove each fix are in the table above; the first ones to look for: `versions: ... 8ded46c ...`,
`switches: ... ; all at their defaults` (41 words on the branch, with `display-present=ahb` and `oneios-m2v-shader-fix=0`; on main since the merge `oneios-m2v-shader-fix=1`, with the audio, codec and sensors words too), `gpu:
astc=1: ...`, the kernel's `oneios-display: refresh range 60..120 Hz`, the helper's `oneios-display:
display-present=ahb: 3 write-only buffers (...) on a child surface of the layer`, and in `phone pull-log`'s iOS log
`OneiOS display links: hooked (made M fps 1 range 1)` and `OneiOS snapshots: JPEG or PNG, not ATX (hooked)`. Not
booted.

### What is left

- A phone boot of this build (the integration build first: `out/handoff/integration2-d3b802f`, docs/switches.md, then
  this one), reading the proof lines above, `display-present=ahb` against `window`, `display-post=apart` against
  `serial`. Then the merge into main (CLAUDE.md: a phone boot first).
- After the boot: if `OneiOS display links: ... kept: CLASS` names the animation's target, match it; if free memory is
  still near 0, the next memory step (the render server's screen-sized dedicated targets, then the guest's RAM size in
  takeover); the camera's next stop in its lines.
- The zero-copy panel pool (above): step 1 in code (round 2, not booted); steps 2 and 3 not started.
- From `docs/ports-2026-11-25-gpu.md`, not on this branch: T2 (virtio-media interrupts), M1 (the plug-in on
  Droid-VM's 11-16 stack), G3 (the priming list), T4, G5, T5 (that file's section 8).
- Not read: hoshikv/UniXNU-peridot, slayedkaito/xnu-kernel-peridot-amiaxf, UniXNU-sheng and UniXNU-uke patch lists,
  iOSPlus10 origin/main; Jesvi-iOS, darwin-lisa, ios-redfin and hakios-metal for the camera beyond `post()`.

## State at the hand-off of 2026-11-25

No code had changed on this branch at the hand-off (the fixes are above): this section holds the findings that led to
them. Nothing built, nothing booted then.

### Our own log: why the CPU is hot (read, `out/session-logs/2026-11-24-mac-gpumem/live.log`)

Kernel.kc `e79886d9`, runtime 0.1.21-dev `74c56ef`, takeover mode, 5376 MiB of guest RAM. Line numbers are of that
file.

- **The guest runs out of free memory, and GPU blobs are most of what is wired.** `oneios-mem: free` falls from 3928
  MiB (line 915) to 23-36 MiB from +97 s (lines 2109, 2932, 4392) and 1 MiB (4080); `wired` 2173-3105 MiB of 5129. The
  `oneios-wired` line counts `GPU blobs: own 1476, imported 304` (2425) up to `own 1859, imported 624` (4087), that is
  about 2-3 GiB of GPU blobs. xnuhost's own count agrees: `gpu: 10 s: host blobs 2773 (2542 MiB)` (4093).
- **That pressure is what drives the unmap storm and the CPU.** With free near 0 the pageout and the compressor run all
  the time: `oneios-perf: kernel_task ... VM_compressor 3746618 VM_compressor 3527132 ... VM_pageout_scan 2081473` us
  per 30 s (2623), `compressor 253 MiB holding 899 MiB` (4080). The pageout's reference clears unmap pages that are
  touched again at once: per 30 s 661,738-1,091,450 faults go to vm_fault (26-38 s of CPU, more than one vCPU), nearly
  all `refilled` (a page that was there; 2392, 4361). The 11-12 M unmaps per 30 s of the earlier periods are this
  plus the shared-region fan-out that hosted 0101 narrows (kernel README "0101"). 0101 makes each such fault and
  flush cheaper; it does not remove the pressure. The host: 680-724% of 800, xnuhost 300-409%, SoC 93.8-95.7 C, every
  policy at its top clock 95-100% of the time (lines 1435-3074).
- **Who holds the GPU memory.** `oneios-gpu-clients` (2776): backboardd 1080+395 MiB, WebKit's GPU process 376+300,
  WidgetRenderer 204+31, the posters 73+52 each. backboardd's own blobs at the end: 755 MiB (`oneios-gpu-kinds`, 4640);
  its metal2vulkan census `GM p 13/440M u 264M e 0/0M d 8/246M s 0/0M` (4660): 440 MiB of pooled blocks with only 264
  MiB placed in them (176 MiB of room, 40%), and 8 dedicated blocks of 246 MiB (about 30 MiB each: screen-sized render
  targets, a guess); 976 blobs under 1 MiB. So about 176 MiB of backboardd alone is room in 64 MiB pooled blocks.
- **The posters restart.** MercuryPosterExtension pids 247, 409, 420, 429, 436; CollectionsPoster 425, 428, 451, 461,
  490; AegirPoster, WeatherPoster, RhizomePoster likewise, with ReportCrashService and osanalyticshelper running beside
  them (`oneios-cpu-sys` lines 1706-4617). Who ends them is not in this log: hosted 0100's `oneios-kill` line (on main,
  not booted) names the killer and the reason in the next boot. 0098 (`oneios-disable-allowed`) stops
  osanalyticshelper and the other disabled jobs that ran.
- **The host's own memory is tight too.** `host MemAvailable 1942 MiB of 11084` (2007, 2856); xnuhost's trim then ran
  under pressure (idle time 2 s): `tasks: trim: 60 s: +797 trimmed (+796 under pressure) ... the unmaps 14020 ms`
  (line after 2370). The refaults it causes are the cheap kind (`filled from the tables`, about 3 us each), so it is not
  the main cost, but 14 s of unmaps a minute on the trim thread is not nothing.
- **Frames.** `display: 10 s:` 2-454 frames per 10 s, the helper's answer 5-18.5 ms mean, max 25-52 ms (lines
  1430-3681). The guest's frame is 1440x3120 at @3x (line 32): every full read is 18 MB on the CPU.

### Where the fixes on main stand (not booted)

hosted 0098 (`oneios-disable-allowed`; that log's kernel `e79886d9` had none of 0098, 0100, 0101), 0100 (`oneios-kill`, logging), 0101 (`oneios-nested-exact`,
`oneios-upgrade-flush`, `oneios-fill-fast-fault`), xnuhost `timer-coalesce`, the Metal plug-in's `oneios-pool-cut`,
`oneios-idle-spares`, `oneios-bo-cache-idle`, the display helper's two threads (`display-post=apart`), the camera's
`ModuleInfo` sensor ID, format rule and front-camera formats (`a5c244d`), hosted 0090. All in the integration build
(docs/switches.md "Tests"), handed to the Mac as `out/handoff/integration2-d3b802f/`, not booted.

### What the leads gave

See "The leads" below: the three research helpers' reports of 2026-11-25, each stopped early at the hand-off.

### What is left, and the next step

1. Boot the integration build on the S24 first: it carries every fix above. Read `frame: 10 s`, `display: 10 s`,
   `thermal: 30 s`, `oneios-perf:   pmap:` and `0101:`, `oneios-kill`, `oneios-mem: free`, the `GM` rows, and the
   camera's `ONEIOS CAMERA`, `oneios-camera` and CMCapture lines in `phone pull-log`.
2. CPU and heat (a fix, next): take the memory pressure away at its source. Candidates, in order: (a) metal2vulkan's
   pooled block room (backboardd's 176 MiB of 440 unplaced: a smaller top block size or less room kept ahead, behind a
   switch; the plug-in's note that a 16 MiB block hung the render server on the iPadOS guest must be checked first);
   (b) the render server's screen-sized dedicated targets; (c) the guest's RAM size in takeover (5376 MiB with host
   MemAvailable at about 2 GiB: the owner's fixed-RAM rule, `ram-session-reserve`).
3. Frame rate: the CPU copy of up to 18 MB per frame (docs/gpu.md 2.5 item 2) is the step the leads remove (a
   zero-copy panel pool); see the leads below for what it would take in our display path.
4. Camera: after the boot of step 1, the first stop in the capture plug-in's or CMCapture's lines.

## The leads

### Camera (research helper, stopped early: the leads Jesvi-iOS, darwin-lisa, ios-redfin, hakios-metal and iOSPlus10's camera notes are NOT read yet)

From our own logs and code:

- **No frame was ever asked for in the gpumem boot.** `camera#0 q0` moves only in the first `virtio: 60 s` line (+2:
  the camera list at the device's creation); no later line lists `camera#0`, also while the Camera app was open
  (+280-400 s). `ONEIOS CAMERA: user client opened by cameracaptured` (108506, 108746, 108838) is a device creation
  (`cameras_there()`), not a stream start. So the graph never called the stream's Start (`OneiOSCaptureDevice.c`
  `stream_start` -> `worker`, about lines 1249-1345): the preview is black before frame delivery. cameracaptured ran at
  143-158% CPU meanwhile. That boot has no iOS log (`phone pull-log` was not run), so its plug-in lines are missing.
- **The Metal plug-in failed CMCapture's compute pipelines on time.** The heap fix works (`problem heap options
  0x600... made without them`, no `heap refused`), but with the app open: `oneios-gpu-text: cameracaptured[62] problem
  compute pipeline ...: Compile(Budget(Time))` five times and more (function constants `kMotionThreshold`,
  `kDirection_float`, `kIIRUpdateCoefficient`: temporal noise reduction or stabilisation kernels, which can be in the
  preview graph: a guess). `Budget(Time)` is metal2vulkan's 20 s per compile
  (`external/metal2vulkan/crates/metal2vulkan-air/src/budget/mod.rs` `STANDARD_DEADLINE`, through `pipeline/options.rs`
  and `sources.rs`). Guess, not checked: the 20 s also counts the time a compile waits for a turn
  (`pipeline/budget.rs`).
- **The boot of 2026-11-23 13:45 (its iOS log, `~/handoff-2026-11-23b/session-logs/boot-jobs/ioslog.txt`)** still has
  `Fig assert: "function" ... function is nil (FigMetalContext.m:565)` four times with no `missing NBLibrary` line
  (so not the selector fixed then), `binary archive refused: InvalidFile` 19 times, `Failed to prewarm PTRenderPipeline
  (-1)`. Those are Portrait and SDOF, not the preview.
- **What could still keep it black after main's fixes (guesses from the code, ranked):** (1) the graph never reaches
  Start (failed pipelines make a node fail to prepare); (2) `stream_set_property` (`OneiOSCaptureDevice.c` about
  1092-1131) takes the handler only from `VideoOutputHandlers`' `PixelBufferHandler` and the outputs only as
  `PrimaryScaler`, `SecondaryScaler`, `StillImage`: iOS 16's names, never seen on iOS 27; (3) the pool key `BufferPool`;
  (4) frames carry only PortType metadata; (5) the time base of `f->time` against `mach_absolute_time` (not checked in
  `drivers/camera`); (6) `all_streams[]` is overwritten at each of the about eight device creations a boot.
- **Fixes to make (ranked):** (1) the compile deadline from when the compile runs, not from its queueing, or a longer
  budget for clients other than the render server (`userland/metal/rt/bridge/src/abi/shader.rs` `budget()`, the
  runtime's `pipeline/options.rs`, `sources.rs`), with a lasting line per failed pipeline (function, constants, wait
  and compile time); (2) NBLibrary's `newFunctionWithName:constantValues:*` says once per name why it gave nil, then
  fix that cause; (3) read in the 24A446 cache what BWFigCaptureStream sets for `VideoOutputHandlers` and
  `VideoOutputConfigurations` and take those names; (4) a lasting session-progress line in the plug-in (Start, Stop,
  `FormatIndex`, each output configured, enabled, handled); (5) fuller per-frame metadata. Proof lines: no
  `Compile(Budget(Time))` for cameracaptured, no `function is nil`, `the graph starts the stream`, `the first frame`,
  `PrimaryScaler: the first picture given`, `camera#0 q0 +N` in `virtio: 60 s` with the app open.

### Metal and frame rate (research helper, stopped early: only qiumochen/UniXNU-zizhan was read)

Licences (from their LICENSE and NOTICE files; GitHub detects none): UniXNU-zizhan, UniXNU-peridot, UniXNU-sheng,
UniXNU-uke "BSD-2-Clause-Patent" (UniXNU contributors); oosexgang/hakios-metal MIT; rafsanbasunia/ios-garnet APSL 2.0
for XNU, MIT/Apache/BSD third-party parts, Linux ath11k for Wi-Fi (read its THIRD_PARTY_LICENSES.md before taking a
file). The tracker files the Poco F6 120 fps claim under `slayedkaito/xnu-kernel-peridot-amiaxf` (branch
peridot-2.3.4, on hakios-metal), not hoshikv/UniXNU-peridot. zizhan runs iPadOS 18.7.10 (22H374) with its own kernel
GPU driver and DPU scanout, and measures "moving fps" of a synthetic swipe, not the panel's.

- **"g298" is zizhan's GPU test round 298, not a patch.** The change in it: commit `42629e5`, `userland/ui_compat18.c`
  `link_with`, `display_link_rates()`, ported from hakios `7caa457` (claimed right swipe 78 -> 115 fps). It hooks
  `+[CADisplayLink displayLinkWithTarget:selector:]`, `-setPreferredFramesPerSecond:` and
  `-setPreferredFrameRateRange:`; for a link whose target is `UIKit.InProcessAnimationManager` (or
  `UIViewInProcessAnimationManager`) the rate asked (UIKit asks 20..120, 30 preferred) becomes 0, the panel's rate.
  Switch `pong_dl_full=0`. Their result (`docs/evidence/2026-11-04-g298-displaylink.txt`): moving fps 56.7-58.4 -> 95.3,
  the frame interval p50 after a release 33.2 -> 8.3 ms. Their reading (`docs/progress/2026-11-01-120hz-and-cadence.md`):
  most slow frames were CoreAnimation itself choosing 60/40/30 Hz for light animations (GPU 0.3-2.5 ms a frame), and
  telling CA the panel is 60 Hz made it worse (44.6 -> 40.3). **Ours:** not present, and our guest's vsync reports a
  24..120 Hz range (`sheng-vsync=120`, `getRefreshAttributes`), which invites the same choice. The hook needs code in
  SpringBoard and the apps; the Metal plug-in's constructors (`DisplayWide.inc`, `BacklightMode.inc`, appended in
  `userland/metal/build.sh stage`) are the nearest injection we have (a guess that every UI process loads it).
  `sheng-vsync-fixed=1` (an existing switch) is worth one boot beside it.
- **ASTC on** (`73df2a4`, `e3be892`, `bf7d7f2`, `636bf6f`): MobileGestalt answers DeviceSupportsASTC true in
  SpringBoard and backboardd, so CA uploads ASTC images to the GPU instead of decoding them in ImageIO; NativeMetal's
  frontend got `NBBlockTextureLayout` (compressed, mip and array textures, copied with bounds checks). Their result:
  moving fps 46.9 -> 88.6. **Ours:** forced off in `runtime/loader/src/machine.rs` `set_featuresets` (APPLE2 and MTL2,2
  dropped: SplashBoard's snapshots in AGX's ASTC layout). To do: port `NBBlockTextureLayout` into our staged
  `NativeBridgeMetalFrontend.mm` (a patch in userland/metal), keep APPLE2 and MTL2,2 behind a loader switch.
- **Swap-fence snapshot and the zero-copy panel pool** (`49dee66`, their XNU patch `0039-zizhan-swap-fence-snapshot`,
  `drivers/PongAdreno/ZizhanZcFence.h`, `PongAdrenoScanout.cpp zcFlip`, `drivers/PongPower/ZizhanPanelPool.h`): the
  flip waited on a "latest submitted" fence that moved with the next frame (up to 42 ms); now the fence taken at the
  swap. The 128 MiB panel pool holds 1 copy buffer and 6 zero-copy slots (`pong_dpu_bufs=1`). open_zoom 53.2 ->
  58.6-59.8 fps. **Ours:** the DPU zero-copy is what our display helper's CPU copy is (`runtime/display`
  `Snapshot::update`, 5.5-6.8 ms a frame; docs/gpu.md 2.5 item 2). Not read yet: which fence our guest display waits on
  before its flush (`OneiOSDisplay.cpp`). **A caution:** zizhan rejected `pong_zc_async=1` (the swap answered before
  the latch): it took vsync pacing from backboardd, frames over 25 ms 7% -> 31%. Our `display-post=apart` is the same
  shape: compare the `frame:` line's interval p95 against `display-post=serial` in the first boot.
- **768 MiB BO pool** (`e1bb22d`, `PongAdrenoCp.cpp`): BO allocation in a full pool took 40-77 ms inside open and close
  animations; 1024 MiB removed every fallback. **Ours:** blobs over guest pages, Turnip's cache and metal2vulkan's
  pools; we have no figure for blob creation time during an app open.
- **WebKit's GPU process 256 MiB window** (`a3e672b`): a correctness fix (Safari's texture refusals); ours is drm2kgsl
  0004. **`636bf6f`** reads boot-args once per process (`__sysctl` was SpringBoard's largest main-thread call at app
  open).
- **120 Hz panel:** zizhan switched the panel's DSI timing for real (60.5 -> 120.2 Hz). Their next step, and one for
  us: lock CA's vsync tick to the panel's real TE (our guest tick is a free timer; the helper's `setFrameRate` is a vote).

Ranked for us: (1) the display-link rate filter for UIKit's in-process animations (low cost, behind a switch); (2)
ASTC on (medium, behind a loader switch, needs screenshots); (3) the guest's vsync locked to the helper's present
times over `display.sock` (medium); (4) no CPU copy in the display path (high); (5) a lasting blob/BO creation time in
`gpu: 10 s host`. Not read: hoshikv/UniXNU-peridot, slayedkaito/xnu-kernel-peridot-amiaxf, ios-garnet dev,
UniXNU-sheng and UniXNU-uke (patch lists against userland/metal/patches and rt/patches), iOSPlus10 origin/main, hakios
`7caa457`, the tracker's `/api/devices/<slug>/gpu`. The clones were in this session's scratchpad (temporary).

### CPU and heat (research helper, stopped early: no lead repo read)

- **That log ran none of main's CPU fixes.** Kernel.kc `e79886d9` is older than 0098, 0100 and 0101, and the runtime's
  switches line (21) has no `timer-coalesce`.
- Clocks follow load; no clamp holds them up: WALT's tunables (lines 85-88) hispeed_load 90, boost 0, and every policy
  is 85-95% busy. The vCPUs do idle (lines 809-816: 60-74% idle early); under load 0% idle and 28-44% "waited for a
  host CPU" (1855-1862, 2311-2318, 4277-4284). The thermal state "heavy" at SoC 96.5 C (1401, 1435); iOS "trapping"
  by the case at 46.2 C (3580); our CPU step acts only at a 50 C case, so it never ran.
- Who burns it: the posters (new pids every 7-22 s, 38-206% each, 3123-4616), osanalyticshelper 31-85% (0098's
  target; over its 15 MB limit at 3554, 4179), four ReportCrashService after two `brk #1` traps (1742, 2842),
  cameracaptured and the Camera app left open (143-158% and 89-132%, 3843-4104), backboardd 85-139% animating,
  kernel_task 50-112%; VM_pageout_scan 14.7-18.1 s per 30 s (2877, 3087, 3347). KGSL imported_mem 2.88 GB (3077);
  jetsam 29 kills by the end, 27 idle-exit (4714).
- Rates: xnuhost task traps 51,404-89,505/s (1409, 1841, 3029), `task_unmap 11647376 queued` (3029); pmap unmaps
  11.1-12.4 M against 0.84-0.97 M maps per 30 s, 11.86 M of them from 42,548 shared-region flushes (about 280 tasks
  each, 3084: 0101's target); madvise 59,688 calls at 174 us mean (10.4 s per 30 s), backboardd 13,532 at 244 us
  (3085, 3096); `made runnable -> IPI sent` 33 ms mean (3092); 128,266 sched_setaffinity per 30 s, 41,146 across E
  and P (3031).
- The posters' killer is not in that log (no trap of theirs, no jetsam kill before the loop). The one clue:
  `oneios-gpu-text: MercuryPosterExt[408] no-device device: OutOfMemory(Device)` (3027), at the loop's start. 0100's
  `oneios-kill` will name it.
- Leads: Axiang-dev/UniXNU-caihong is archived and stops at the shared cache (no user space); the tracker's houji is
  Kizashi3/UniXNU-houji and its sheng 111027866/UniXNU-sheng; no port on the tracker claims a thermal fix that carries
  over. They run XNU natively or in a VM, where a reference clear is a cheap access-flag fault and an IPI a microsecond,
  so their pmap and scheduler settings do not map one to one (unverified).
- Ranked fixes: (0) boot main first; (1) a modified-bit clear write-protects the host page instead of unmapping it
  (docs/gpu.md 2.5 item 4(b), a new host op such as a protect order, kernel patch under XNU_HOSTED; switch idea
  `oneios-refmod-protect`; proof: madvise's mean down from 174 us, backboardd's own unmaps falling); (2) a
  reference-only clear of a shared-region page keeps the host mapping (switch idea `oneios-ref-keep=nested`; proof:
  VM_pageout_scan under 1 s per 30 s); (3) the posters' loop at its cause once `oneios-kill` names it (if it is
  OutOfMemory(Device), the GPU memory above); (4) fewer cross-cluster moves at a release (`task.rs`, a guess); (5) the
  thermal step from "heavy" by the SoC (the owner's call: the all-cores rule).

### The next step

Boot the integration build (main) on the S24 with `phone pull-log`, then in this order: the memory pressure (the GPU
blobs and metal2vulkan's block room: it feeds the pageout, the unmap storm and probably the posters'
OutOfMemory(Device)), the display-link rate filter and ASTC for the frame rate, and the camera's first stop in its
lines (compile budget, the output names of 24A446).
