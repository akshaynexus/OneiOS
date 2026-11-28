# Switches: every recent change, one setting each

The owner (2026-11-23): "Merge it all with switches so we can test everything at once." One build has every change.
Each change that alters what runs can be turned back by **one** setting in the image's
`overlay/userland/xnuhost.conf` (or a kernel boot-arg there, through `boot-args-add=`). One boot tests everything; a
fault is pinned by turning one switch off.

Rules for a switch (and for every helper that lands a behaviour change from now on, `docs/HANDOFF-2026-11-23.md` 5):

- One key, read once at the start, cheap where it acts (one relaxed load, or a value fixed at the start).
- The default is the new behaviour (main's). The other value is the behaviour before the change, as close as the
  code allows; the table says where it is not exact.
- A row in this table in the same commit: key, values, default, what it changes, the commit, whether a phone ran it.
  And its row in the switch catalog, `runtime/switches/src/lib.rs` (`SWITCHES`: where, the rule of its values,
  default, off value, group, a short title, one or two sentences of help, the phone state, a warning), in the same
  commit: xnuhost's `switches:` line is made from that list, and `config::tests::the_catalog` fails when it and
  xnuhost's parser disagree.
- No switch for pure logging, and none for a crash fix whose "off" would be the abort again.

**The OneOS app's Switches screen reads this catalog** (since 2026-11-25): `bin/oneios-tool switches --json
--runtime DIR` (the staged runtime's `lib/switches.json`) lists every switch with its title, help, group, values and
phone state, and `bin/oneios-tool conf show|set|unset|check|write|restore` reads and changes the image's
`overlay/userland/xnuhost.conf` (a boot-arg switch as a word of its one `boot-args-add=` line), checking every value
as xnuhost does, so a file xnuhost would refuse is not written without the user's `--force`
([tools/oneios-tool/README.md](../tools/oneios-tool/README.md) "switches and conf"). The app keeps no list of its own:
a new switch is on the screen with the runtime that has it.

xnuhost logs every switch once, after the `versions:` line:

```text
switches: session-mode=hosted stop-loop=wait takeover-reclaim=1 takeover-swappiness=keep task-trim=1 timer-coalesce=1 ram-fit-terms=tasks ram-ondemand=0 uclamp-rule=work gpu-down=governor gpu-kgsl-ui-rb=0 gpu-off-level=1 gpu-blob-entries=65532 wifi-ring-wait=1 wifi-send-wait=1 wifi-buffers=1 wifi-joined-scan=cache encoder-10bit=main10 encoder-warmup=1 display-post=apart display-present=ahb decoder=auto decoder-codecs=h264,hevc,vp9,av1 astc=1 sensors=auto sensors-hz=50 sensors-light=auto oneios-motion=0 oneios-als-spu=0 oneios-wifi-rssi=1 oneios-telemetry-jobs=0 oneios-pool-cut=1 oneios-pressure-big=512 oneios-disable-allowed=1 oneios-nested-exact=1 oneios-upgrade-flush=0 oneios-fill-fast-fault=1 oneios-audio-rt=1 oneios-audio-curve=1 oneios-codec-irq=1 oneios-idle-spares=1 oneios-bo-cache-idle=1 oneios-keep-room=8 oneios-dl-full=1 sheng-vsync-min=60 oneios-compile-s=120 oneios-snapshot-astc=0 oneios-camera-notify=1 oneios-m2v-shader-fix=1 oneios-dont-care=0 oneios-queue-priority=1 (docs/switches.md); all at their defaults
```

When some are changed, the end says which: `not at the default: wifi-send-wait gpu-down`. Kernel switches are read
from `boot-args` there (the kernel's default when the boot-args do not name them).

## The table

Values `0|1` take `0/1`, `no/yes`, `false/true` (xnuhost's flags). "Phone" says whether a phone session has run the
change (the default value), and whether one has run the other value.

| Key | Values | Default | What the default does / the other value | Commit | Phone |
|---|---|---|---|---|---|
| `session-mode` | `hosted\|takeover` | `hosted` | takeover: the opt-in Android takeover (docs/booter.md 2.9d) | a3077a9, fork 1d9b58f | takeover booted 2026-11-23 21:11 (kernel b5f5378a), not with this build |
| `takeover-stops` | `default\|none\|NAME,...` | `default` | takeover only: the services the switch stops (`none`: none) | a3077a9, fork 1d9b58f | 21:11 boot: default list |
| `stop-loop` | `wait\|poll` | `wait` | wait: the fork's stop loop sleeps on init's properties (`xnuhost --prop-wait`); poll: one `getprop` look a second, no `--prop-wait` (the 0.2 s loop of before is not brought back). Read by the fork's switch; xnuhost checks it | a3077a9, fork 1d9b58f + this build | wait: 21:11 boot; poll: no |
| `takeover-reclaim` | `0\|1` | `1` | takeover only: 1: after the stops, before xnuhost, the fork's switch runs `sync`, `drop_caches` 3 and each zram device's `compact`, so that the page pools and caches MemAvailable did not count are in it for ram=auto (its line measures the MiB); nothing to put back. 0: none of it (before takeover-ram). Read by the fork's switch; xnuhost checks it | takeover-ram (docs/booter.md 2.9d, "More RAM in the takeover"), fork takeover-ram | no |
| `takeover-swappiness` | `keep\|0..200` | `keep` | takeover only: N: `vm.swappiness` N for the session (One UI's saved first; every return writes it back); keep: not touched (as before). The lever for a lower headroom. Read by the fork's switch; xnuhost checks it | takeover-ram, fork takeover-ram | no |
| `task-trim` | `0\|1` | `1` | 1: an idle task's host mappings are dropped and ram=auto counts 150 tasks awake (5376 MiB on the S24 Ultra); 0: no trim, every task at 4 MiB: exactly 3b8742a's sizing (`config::tests::ram_config`: 3528 MiB fit) | 07b10a8 | no |
| `ram-ondemand` | `0\|1` | `0` | **option C, off; against the fixed-RAM rule of 2026-11-15, for the owner to choose.** 1: the guest's RAM is not prefaulted; each page is locked as iOS first touches it (`MLOCK_ONFAULT`: never swapped while held) and free pages are given back; with `ram=SIZE` above what fits, the top is backed only while iOS uses it, and when iOS fills it the valve opens or the OOM killer ends xnuhost (docs/booter.md 2.2, "On demand"). 0: the fixed, locked RAM | takeover-ram | no |
| `timer-coalesce` | `0\|1` | `1` | 1: a vCPU's host timer is armed only when the guest's deadline comes earlier; a later deadline or a cancel leaves it, and a signal before the deadline arms it for the deadline without raising the line (docs/booter.md 2.4); 0: every new deadline and cancel is a `timer_settime`, every signal raises the line (before). `perf: ... timer_set N (M to the host timer, K kept it; R early signals armed it again)` | fix/unmap-fanout | no |
| `ram-fit-terms` | `tasks\|old` | `tasks` | tasks: ram=auto's 3b8742a terms (tasks, hold share, 1536 MiB headroom locked); old: the terms before 3b8742a (headroom 768 MiB locked / 1 GiB unlocked, page tables 8 %, no task cost, no hold share; any of those keys given still wins). With `task-trim=0` too this is the v0.1.19 sizing | this build (3b8742a) | tasks: no; old: v0.1.19's sessions |
| `uclamp-rule` | `work\|all` | `work` | work: xnuhost alone sets a vCPU thread's uclamp (0 at idle, the released thread's floor, the guest's request); drm2kgsl's animation boost only for the frame's threads; background/utility 0. all: before 03d85ac: drm2kgsl's boost on every started vCPU thread and every iOS thread not background, the cpufreq device sets the vCPU thread's clamp, nothing at idle | 03d85ac + this build | no |
| `gpu-down` | `governor\|bottom` | `governor` | governor: the GPU level leaves the top when a frame fits at the governor's clock in a burst (learned ratio); bottom: when it fits at KGSL's lowest clock (before 3b36ff0: on the S24 Ultra it never came down) | 3b36ff0 + this build | bottom: every session up to 21:11 (the GPU at the top all session); governor: no |
| `gpu-kgsl-ui-rb` | `0\|1` | `0` | 0: render server and front app run at KGSL priority 8 (out of Android's 4-7 UI ringbuffer, avoiding tile-memory collisions with One UI; drm2kgsl patch 0011, G5 of `docs/ports-2026-11-25-gpu.md`); 1: priority 4 (in the UI ringbuffer, as before) | `feat/takeover-drm-gpu` | no |
| `gpu-off-level` | `0\|1` | `1` | 1: hold KGSL at `KGSL_CONSTRAINT_PWR_MIN` while iOS's display is off (`brightness::display_on() == false`, drm2kgsl patch 0011, T4 of `docs/ports-2026-11-25-gpu.md`) so standby wakes do not boost the GPU; 0: render server stays at the governor's choice (as before) | `feat/takeover-drm-gpu` | no |
| `gpu-blob-entries` | `16384..65532` | `65532` | the most entries of a guest 3D blob the device takes; 16384: the limit before 78c4b86 (an 80 MiB blob of single pages refused; drm2kgsl 0010 keeps that from aborting) | 78c4b86 + this build | no |
| `wifi-ring-wait` | `0\|1` | `1` | 1: a frame for iOS that finds its receive ring full waits for the guest's kick (20 ms data, 50 ms the station's own); 0: dropped at once (before cbf97ea) | cbf97ea + this build | 1: 2026-11-23 19:40 |
| `wifi-send-wait` | `0\|1` | `1` | 1: a frame of iOS's that finds wlan0's send buffer full waits (256 at most, 20 ms); 0: it fails at once (`send failed, last errno 11`, before b704922) | b704922 + this build | 0: 19:40 (the old code); 1: no |
| `wifi-buffers` | `0\|1` | `1` | 1: wlan0's AF_PACKET socket gets a 4 MiB receive and a 1 MiB send buffer; 0: the kernel's defaults (before cbf97ea) | cbf97ea, b704922 + this build | 1 (receive only): 19:40 |
| `wifi-joined-scan` | `cache\|low\|plain` | `cache` | cache: within 120 s of the last scan its networks answer, else a low-priority scan; low: every scan the radio's, low priority (cbf97ea); plain: every scan the radio's, no flags (before cbf97ea) | b704922, cbf97ea + this build | low: 19:40 (the driver took no flags: `flags 0x0`); cache: no |
| `encoder-10bit` | `main10\|8bit\|nv12` | `main10` | main10: the device takes P010 (iOS's 10-bit tiles) and the helper codes HEVC Main10 where the encoder offers it, else 8-bit of the high bytes; 8bit: P010 taken, Main10 never tried; nv12: the device lists and takes no P010, the guest sends NV12 (its high bytes). Not exact: before b3da193 the guest refused 10-bit tiles itself (stage 101), which the kernel no longer does | b3da193 + this build | no |
| `encoder-warmup` | `0\|1` | `1` | 1: the encoder helper starts at the guest's first virtio-media session and opens one HEVC encoder at once; 0: it starts at the first encode, no warm-up (its binder threads start either way: that is the fix of the hang, not a choice) | b3da193 + this build | no |
| `display-post` | `apart\|serial` | `apart` | apart: the display helper reads and answers each frame as it comes (a reader thread) while its poster waits for the layer's buffer, fills it and posts the newest picture; serial: xnuhost sends the helper `options` (`XHDO`, bit 0) and its reader reads nothing more until the last change is posted, the one loop's timing before 2026-11-24 (exact: the same order of read, answer, lock, fill, post). A helper older than this build ignores the message (it has the one loop anyway) | fix/frame-path | no |
| `display-present` | `ahb\|window` | `ahb` | ahb: xnuhost's `options` message carries bit 1 (`OPTION_PRESENT_AHB`) and the display helper posts its frames into three write-only AHardwareBuffers (`CPU_WRITE_RARELY \| CPU_READ_NEVER \| GPU_SAMPLED_IMAGE \| COMPOSER_OVERLAY`) on a child of its layer with ASurfaceTransaction, at most two in flight, each refilled after its release fence: no CPU cache maintenance of the whole buffer at each lock and post (ported from YumikoHayamato/OneiOS cdb297bbf, whose S25 went from 60 frames a second at 49.5% of a core to 114 at 17.1%; `runtime/display/src/xhd/present.rs`). Where anything of it is missing (half-float frames, no such buffers, no child surface, an allocation, lock or post refused) the helper says why once and takes the window path by itself. The helper's log: `oneios-display: display-present=ahb: 3 write-only buffers (...) on a child surface of the layer` or `...: WHY: the window path`. window: it locks and posts the layer's window, as before (exact). The helper's `ONEIOS_DISPLAY_PRESENT=ahb\|window` wins over both. A helper older than this build ignores the bit | fix/s24-fps-heat-camera | no |
| `decoder` | `auto\|off` | `auto` | auto (2026-11-24): the video device decodes too (card `oneios-encoder+decoder`): iOS's paravirtualized VideoToolbox decoder gets the phone's MediaCodec for H.264, HEVC, VP9 and AV1 (8-bit 4:2:0; a 10-bit stream, a size outside 96..4096 or above 4096x2304 is refused and decoded in software). Needs `encoder=auto` or `sim` (the device and media pool are the encoder's). off: an encoder only, `decs` lists nothing and iOS decodes in software (videocodecd, WebKit), as before. Exact: the guest's decoder half is the same code either way, it finds no device | fix/hw-decode | no |
| `decoder-codecs` | `h264,hevc,vp9,av1` (any of them, comma separated) | all four | the codecs the device offers (ENUM_FMT of OUTPUT, so `decs` and `dcap`); one left out is decoded in software. Pins a codec whose MediaCodec decoder misbehaves | fix/hw-decode | no |
| `sensors` | `off\|auto\|ssc` | `auto` | **On by default since the merge into main (2026-11-25, the owner: every new fix on; it was off on the branch).** auto: xnuhost reads the phone's sensor hub (SSC) over QRTR, no Android framework (`runtime/bridges/sensors`): the accelerometer, gyroscope and magnetometer become the guest's motion device (`/arm-io/input-N`, `oneios-motion=7`, 15 with a magnetometer; only with a collection without `com.apple.driver.AppleSPU`, see `oneios-motion`) and the front light sensor the brightness bridge's light (`oneios-als-spu=1`), streams paused while iOS's display is off; when the hub does not answer (no QRTR, no service 400) the old path: no motion device, the rear light sensor. ssc: the same, but no rear light sensor when the hub has no light. off: neither, as before (exact: no socket is opened). The way back with auto: the bridges' close calls `Hub::close`, which stops every stream on the hub, wakes its one thread (eventfd) and joins it, and the sockets close; a phone with no QRTR (the Galaxy A56's Exynos: `socket(AF_QIPCRTR)` fails at once) or no service 400 gets the old path, a boot at most 1.5 s longer per unanswered ask. Proof: `sensors: on: ssc ...` or `sensors: off: source old ...`, the `sensors: 60 s:` line; settle the route first with `oneios-tool phone qrtr` | fix/sensors-als | no (not booted) |
| `sensors-hz` | `5..200` | `50` | the accelerometer's and gyroscope's rate on the hub (the magnetometer at most 25 Hz); one motion frame to the guest per accelerometer sample | fix/sensors-als | no |
| `sensors-light` | `auto\|0..7` | `auto` | which of the hub's `ambient_light` sensors: auto the first whose name and vendor do not say "rear" (each is logged: `sensors: light N: ...: used`); N that one | fix/sensors-als | no |
| `boot-args-add=oneios-motion=0` | `0..15` (bits) | the kernel's `0`; xnuhost adds 7 (15 with a magnetometer) with `sensors=auto` or `ssc` when the hub has an accelerometer **and the kernel collection has no `com.apple.driver.AppleSPU`** (fix/motion-spu-clash; v0.1.30 added nothing) | bit 0 accelerometer, 1 gyroscope, 2 device orientation, 3 compass: UniXNU-droidvm's `AppleSPUHIDDevice`s (locationd, CoreMotion, backboardd's rotation) published when xnuhost gives a motion device; 0: nothing published, as before (the kernel's default; a command line's value is kept). Kernel, the HID archive (`kernel/patches/oneios-hosted/hid`). v0.1.29 panicked at kernel start with it (S24 Ultra, 2026-11-25 05:05, 24B156): "OSMetaClass: Kext com.apple.driver.AppleSPU class AppleSPUHIDDevice is a duplicate". IOKit loads the kext of every personality that matches, whatever its score, and AppleSPU's `AppleSPUGestureDriver` matched the motion devices. Fixed at the cause: `prelink roots` leaves AppleSPU and the four kexts that link it out of both collections (`kernel/prelink/README.md`, "Which kexts"); a collection prelinked before still has it, and then xnuhost gives no motion device (`sensors: motion: not given to the guest: ... has com.apple.driver.AppleSPU`) and the kernel registers no motion class (`oneios-hid: motion: com.apple.driver.AppleSPU is in this kernel collection ...`) | fix/motion-spu-clash | no (not booted; v0.1.29's panic is fixed in the code) |
| `boot-args-add=oneios-als-spu=0` | `0\|1` | the kernel's `0`; xnuhost adds 1 when the light comes from the hub (`sensors=auto`, the default) | 1: the light sensor is published as an iPhone's SPU light sensor (`{0xff00, 4}`, Transport SPU, `ALSCurveInfo`), its event driver's lux events turned into ambient light events, the last reading re-posted within 15 ms of a wake (CoreBrightness's 250 ms first-sample timeout); 0: the HID Sensor page device for `AppleUSBALSService`, as before (exact). On with `sensors=auto` since the merge into main; host-tested are the descriptor and the lux fields only (`drivers/tests/host.sh`), not the event path. Kernel, `drivers/brightness` and the HID archive | fix/sensors-als | no |
| `boot-args-add=oneios-wifi-rssi=0` | `0\|1` | `1` | 1: the Wi-Fi driver posts RSSI_CHANGED records (the bars follow the signal); 0: none, wifid reads the signal at the join only (before cbf97ea). Kernel (net/OneiOSWifi.cpp) | cbf97ea + this build (db2eb42) | no |
| `boot-args-add=oneios-telemetry-jobs=1` | `0\|1` | `0` | 0: 0093 and 0094 disable the telemetry group (osanalyticshelper and its agent job, 16 uploaders); 1: the disable list is 0053's loops only and the enable pass enables the telemetry labels again (a store 0093 changed gets them back). Kernel, hosted 0096 | bad88c3, 30bdea6 + 0096 | 0093 alone: 20:23 (osanalyticshelper still ran: 0094) |
| `boot-args-add=oneios-pool-cut=0` | `0\|1` | `1` | 1: metal2vulkan releases a spare command pool that holds more than its last 10 s need plus 8 command buffers (the render server's small command-stream BOs go back); 0: the pools are kept as before (the plug-in sets `METAL2VULKAN_POOL_USE_WINDOW_S=0`). Metal plug-in only (the boot add-on), 624bab3 add-on only: the rt runtime reads no `METAL2VULKAN_POOL_USE_WINDOW_S` (its own reaper trims pools), docs/gpu.md "Who keeps a blob" | 50873fe | no |
| `boot-args-add=oneios-pressure-big=0` | `0\|MB` | `512` | 512: at a memory pressure rise a process of at least 512 MiB footprint not yet notified at that level is notified first, the largest first (the render server and the apps that draw hear of it in the first 250 ms steps, not after 150-300 smaller processes); 0: XNU's order (least important band first, 250 ms apart: the GPU clients last). Kernel, hosted 0097; its `oneios-wired` and `oneios-pressure` lines are logging (no switch) | 0097 (fix/wired-names) | no |
| `boot-args-add=oneios-disable-allowed=0` | `0\|1\|NAME` | `1` | 1: before launchd the kernel renames the 19 `DisableAllowed` keys of the sealed `/System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist` (same length, nothing else changes) in a copy that 0031's union shows over LaunchDaemons, so launchd honours disabled.plist for every job (osanalyticshelper and its agent, diagnosticextensionsd, symptomsd-diag, OTATaskingAgent, softwareupdated ran although disabled); NAME: the plist of that product type; 0: no copy (an earlier boot's removed), launchd runs the marked jobs whatever disabled.plist says (before 0098). With `oneios-telemetry-jobs=1` the telemetry labels are enabled in disabled.plist, so they run either way. Kernel, hosted 0098; `oneios-kill` (0100) is logging (no switch) | 0098, 0100 (fix/disable-allowed) | no |
| `boot-args-add=oneios-nested-exact=0` | `0\|1` | `1` | 1: each task keeps a page bitmap (1 KiB per 32 MiB block it filled in) of the shared region; a shared-region flush is sent only to the tasks that have a page of its range, and forgets those pages; 0: 0045's block bits only (a flush goes to every task that filled any page of the block). Kernel, hosted 0101 | fix/unmap-fanout | no |
| `boot-args-add=oneios-upgrade-flush=1` | `0\|1` | `0` | 0: an entry changed to the same page with no less access (arm_clear_fast_fault: the reference or modified fault taken; pmap_enter of the same page again) keeps the tasks' host mappings; 1: such a change unmaps the page in every task that may have it, as before. Kernel, hosted 0101 | fix/unmap-fanout | no |
| `boot-args-add=oneios-fill-fast-fault=0` | `0\|1` | `1` | 1: a user fault whose entry waits for its reference or modified fault is resolved on the fill path (arm_fast_fault, then the fill); 0: it goes the exception path (sleh, arm_fast_fault, the return and the refill) as before. Kernel, hosted 0101 | fix/unmap-fanout | no |
| `boot-args-add=oneios-audio-rt=0` | `0\|1` | `1` | 1: each audio engine thread of the guest (speaker, mic, Actuator) is a time-constraint thread of the guest's scheduler (period and constraint 5 ms, computation 2.5 ms) and waits for its period with no timer slop (`TIMEOUT_URGENCY_SYS_CRITICAL`); no host thread is realtime. 0: kernel priority 81 and `clock_delay_until`, as before (the speaker's periods 1.4-1.6 ms late on average, up to 98 ms behind the VM's pageout threads, phone 2026-11-24 01:17). Kernel (the audio archive's staged `DroidVMAudio.cpp`); its `late periods:` line and the split late count are logging (no switch). docs/hardware.md 6, "Late periods" | fix/audio-late | no |
| `boot-args-add=oneios-audio-curve=0` | `0\|1` | `1` | 1: the speaker's volume control reports -36..0 dB over its 128 steps (0.28125 dB a step) and the speaker engine applies UniXNU-sheng's curve to the step iOS sets: -36 x (1 - step/128)^2 dB (a third of iOS's volume about -16 dB, half -9 dB; step 0 silent). iOS spreads its volume evenly over the steps (the phone, 2026-11-24: each key press 7-8 steps), so the curve is applied in the guest, not through the range map. 0: -64..0 dB in 0.5 dB steps, applied as reported (half the volume about -32 dB: the quietness of 2026-11-24, `speaker volume 70 of 128 (-29.0 dB ...)`). Kernel (the audio archive's staged `DroidVMAudio.cpp`, block "fix-audio-curve"); its change lines name the curve, and `OneiOSAudio.c`'s report has `speaker volume N of 128 (-X.Y dB applied ...)` after a change (logging, no switch). docs/hardware.md 6, "Volume" | fix/audio-volume-rt | no |
| `boot-args-add=oneios-idle-spares=0` | `0\|1` | `1` | 1: the Metal plug-in's idle trim (every 2 s) also frees the images and buffers metal2vulkan's recyclers parked past their second and the spare dedicated image blocks past their 10 s (they went only as another object was parked, taken or returned, so a still process kept up to 128 + 64 MiB parked and every spare); 0: they age only as before (the plug-in sets `METAL2VULKAN_IDLE_SPARES=0`). Metal plug-in only (the boot add-on), 624bab3 add-on only: the rt runtime reads no `METAL2VULKAN_IDLE_SPARES` (it has no such spares), `userland/metal/m2v-idle-memory.patch`, docs/gpu.md "The idle memory" | this build | no |
| `boot-args-add=oneios-bo-cache-idle=0` | `0\|1` | `1` | 1: every 60 s the plug-in gives back the BOs in Turnip's cache of freed BOs that no allocation took for 30 s (`tu_darwin_bo_cache_trim_idle`); 0: the cache goes back only past its budgets or at a pressure rise, as before (the render server kept 78 MiB at a still screen). Metal plug-in only (the boot add-on), both add-ons (the same Turnip and the same row code, `CommandPoolRow.inc`), `userland/metal/turnip-idle-bo-cache.patch` | this build | no |
| `astc` | `0\|1` | `1` (`0` on the Galaxy A56, its host table entry) | 1: the device tree's `/product graphics-featureset-fallbacks` keeps APPLE2 and MTL2,2, so MobileGestalt answers DeviceSupportsASTC and CoreUI and Core Animation hand ASTC images to the GPU (NativeMetal's frontend lays them out, `DVMLinearTextureLayout`; the Adreno samples them) instead of decoding them on the CPU; UniXNU-zizhan measured moving fps 46.9 -> 88.6 with it. xnuhost says `gpu: astc=1: /product graphics-featureset-fallbacks keeps APPLE2 and MTL2,2 (DeviceSupportsASTC)`. 0: both dropped, as before (runtime/loader `machine.rs` `set_featuresets`) | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-snapshot-astc=1` | `0\|1` | `0` | 0: SpringBoard's app snapshots are JPEG (opaque) or PNG, never ATX (`+[XBApplicationSnapshotManifestImpl _outputFormatForSnapshot:]`, ported from ios-garnet; `userland/metal/SnapshotFormat.inc`): with `astc=1` iOS would compress each snapshot to ASTC on the CPU, seconds a picture, and the scene-update watchdog (10 s) kills the app. iOS's log: `OneiOS snapshots: JPEG or PNG, not ATX (hooked)`. 1: iOS's own choice | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-dl-full=0` | `0\|1` | `1` | 1: the CADisplayLinks of the in-process animation manager (AnimationKit's and UIKit's `InProcessAnimationManager`) run at the panel's rate: the 20..120, 30 preferred they ask becomes the display's own (ported from UniXNU-zizhan 42629e5, `userland/metal/DisplayLinks.inc`; every other link keeps its rate). iOS's log: `OneiOS display links: hooked (made M fps 1 range 1)`, then `... in-process animation link at the panel's rate (asked ...)`; the first 4 other links that ask under 120 Hz: `... kept: CLASS asks ...`. 0: every rate as asked (before) | fix/s24-fps-heat-camera | no |
| `boot-args-add=sheng-vsync-min=24` | `24..HZ` | `60` | 60: above 60 Hz the guest's display reports a 60..120 Hz range to the render server (the longest refresh interval 1/60 s), so Core Animation no longer picks 24, 30 or 40 Hz for light animations (`staged/display.rs` LEGACY in NativeLegacyDisplay.cpp's `getRefreshAttributes`; the kernel says `oneios-display: refresh range 60..120 Hz` once). 24 (or 0): the family's 1/24 s, a 24..120 range (before). `sheng-vsync-fixed=1` forces the panel's `1/hz` interval at both ends (now also honoured by `staged/display.rs` LEGACY). Since 2026-11-28 xnuhost no longer adds `sheng-vsync-fixed=1` above 60 Hz (it did from v0.1.39): the default is this 60..120 range, 120 Hz for touch, scrolling and transitions and 60 Hz for light animations (thermals); `boot-args-add=sheng-vsync-fixed=1` keeps every animation at the panel's rate. Kernel | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-keep-room=0` | `0\|MiB` | `8` | 8: metal2vulkan keeps at most an 8 MiB free range in each of its pools for the coming placements (`METAL2VULKAN_KEEP_ROOM_MIB`, Droid-VM patch 0027, set by `userland/metal/KeepRoom.inc`); the half block it kept (32 MiB once a pool has 64 MiB) made a new 64 MiB block whenever no block had 32 MiB in one piece: backboardd held 440 MiB of pooled blocks with 264 MiB placed. 0: the runtime's half block (before). `/private/var/ngpu/debug/keep-room-mib` still wins. 624bab3 add-on only (rt has no keep room). Proof: the GM row's `p` nearer `u` | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-compile-s=0` | `0\|1..600` | `120` | 120: every Metal client but backboardd gives a pipeline compile 120 s before `Compile(Budget(Time))` (`METAL2VULKAN_COMPILE_DEADLINE_S`, `userland/metal/m2v-compile-deadline.patch` and `CompileDeadline.inc`): cameracaptured's noise-reduction kernels failed at 20 s with every core busy, and the capture graph never started. The render server keeps 20 s. 0: 20 s everywhere (before). 624bab3 add-on only | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-camera-notify=0` | `0\|1` | `1` | 1: the capture plug-in's stream posts StreamStarting and StreamStarted around a Start, StreamStopping and StreamStopped around a Stop, as H10ISP's does (ported from Jesvi-iOS; BWFigVideoCaptureDevice waits for StreamStopped); iOS's log `ONEIOS CAMERA: ios: cameracaptured[N]: stream: posted StreamStarted`. 0: none posted (before) | fix/s24-fps-heat-camera | no |
| `boot-args-add=oneios-m2v-shader-fix=0` | `0\|1` | `1` | **On by default since the merge into main (2026-11-25, the owner: every new fix on; it was off until then).** 1: the Metal translator's Adreno shader fixes, `userland/metal/patches/metal2vulkan` 0101 (a constant table indexed at run time and larger than 4 KiB is read from the shared constant blob instead of a private copy per thread, which an Adreno backs per fiber) and 0102 (pointer constants that are casts or constant GEPs of a blob global are lowered instead of refused); 0: both refused or `Private` as before, the same translations as a plug-in without them. The plug-in sets `METAL2VULKAN_SHADER_FIX=1` unless the boot argument says 0 (`userland/metal/ShaderFix.inc`), read once per process (`0110-shader-fix-switch.patch`) and hashed into the device profile's fingerprint only when on, so the shader cache keeps the two kinds of translations apart and the entries made with 0 are those of a plug-in without the patches. Every Metal client, backboardd included. Metal plug-in only (the boot add-on), 624bab3 add-on only (the rt bridge has no such patches: the switch does nothing there). In xnuhost's `switches:` line since the merge into fix/s24-fps-heat-camera. Proof: iOS's log (`oneios-tool phone pull-log DIR`), each Metal process: `OneiOS shader fix: on (oneios-m2v-shader-fix=1, the default): METAL2VULKAN_SHADER_FIX=1`, or `OneiOS shader fix: off (oneios-m2v-shader-fix=0)`. The first boot with it on translates each shader again (the profile's fingerprint changed). Ported from orumanayurpanjayat-ux/OneiOS `d157b43` (leusernamealadeen), which applied them in every build | port/oruma-2026-11-24 | no (not built into a plug-in yet, not booted) |
| `boot-args-add=oneios-dont-care=1` | `0\|1` | `0` | rt add-on only. **Off by default since 2026-11-28** (it was 1 in v0.1.36-v0.1.40). 0: Metal's DontCare and MultisampleResolve stores store, as rt's own design needs: rt fixes a store as a rendering begins, and an encoder that ends its rendering early (`RenderCommandEncoder::interrupt`: a memory barrier after a draw that wrote memory, an attachment read's feedback move or shadow copy, a copy of a texture over a buffer, a visibility result's copies) begins it again with `LOAD`, so a `DONT_CARE` store before such a split leaves Turnip's tiles unwritten and the draws before it are lost (garbage where they were; a resolving pass resolves stale samples over them). Read in the code as the cause of v0.1.40's garbage in the middle of the screen during page transitions (userland/metal/rt/README.md, "DontCare stores and split passes"). 1: they are Vulkan `DONT_CARE` on Turnip (rt 0005, Droid-VM 0021's measurement: 37 ms against 9 ms a pass of 64 blended 4x draws), for an A/B of the speed only (`METAL2VULKAN_DONT_CARE_STORES=1`, `userland/metal/rt/RtSwitches.inc`). The 624bab3 add-on has Droid-VM 0021 always | port/m2v-rt | no (the default 0 not booted yet) |
| `boot-args-add=oneios-queue-priority=0` | `0\|1` | `1` | rt add-on only: 1: the render server's queues at the high global priority (rt 0006, Droid-VM 0012); 0: the driver's default (`METAL2VULKAN_QUEUE_PRIORITY=default`) | port/m2v-rt | no |
| `boot-args-add=oneios-codec-irq=0` | `0\|1` | `1` | 1 (2026-11-25): (in xnuhost's `switches:` line since the merge into main) the video codec's transport (the staged `DroidVMVirtioMedia.c`, edit 7 of the codec archive) takes its event queue on the media device's SPI (`droidvm,intid`, ArmCpu's `armcpu_spi_attach`) and its event worker sleeps until it, with a guard of 25 ms while a session streams and 1 s otherwise; the command queue keeps `VIRTQ_AVAIL_F_NO_INTERRUPT` (xnuhost answers a command inside its notify, and since this change honours the flag on the media device, `Device::quiet_queues`). 0: the pin's poll, every 1 ms while a session streams and every 50 ms otherwise, both queues `NO_INTERRUPT` (so xnuhost raises nothing). Not exact: on both values events now reach `DroidVMVTHost.cpp` from the event worker only and without the transport's lock (Droid-VM 96e8c1f's order), and a command no longer reaps events while it waits (no command waits on xnuhost). A window without an SPI, or one ArmCpu does not attach (16 slots), is polled as with 0. Not yet in xnuhost's `switches:` line (kept out of `runtime/xnuhost/src/config.rs` while fix/s24-fps-heat-camera edits that list). Proof: `ONEIOS CODEC: transport: interrupt (SPI N), 30 s: ...` with each counters line (`kernel/patches/oneios-hosted/codec/README.md`, "The transport on the interrupt"). Ported from Droid-VM/UniXNU-droidvm 96e8c1f and 2cd4c8e | fix/codec-irq | no (built, not booted) |

### The Metal add-on: 624bab3 or rt

Not a key of `xnuhost.conf`: which boot add-on (`image/kernel.kc.gpuaddon`) the Mac stages, with
`oneios-tool phone addon FILE` (and `phone addon --remove` for the image's own plug-in). Both carry the same Turnip and
the same camera plug-in; they differ in the bridge's runtime (`userland/metal/rt/README.md`).

| Add-on | Built by | Default | What |
|---|---|---|---|
| 624bab3 | `userland/metal/build.sh` (`METAL_RUNTIME` unset) | yes, until a phone boot of rt passes | the old runtime and its 56 patches; the fallback |
| rt | `METAL_RUNTIME=rt userland/metal/build.sh` | no | metal2vulkan `development` 6462199fa (`metal2vulkan-runtime` 0.2) with `userland/metal/rt/patches` 0001-0008 and the rewritten bridge; not booted |

Each build writes `NativeAdrenoMetal.runtime` (`624bab3` or `rt`) beside its bundle; the CDHashes of a pair the Mac
was given are in that hand-off, and `oneios-tool phone sums` shows the staged one.

Not switched, on purpose:

- drm2kgsl 0010 (78c4b86: no abort at a context's end with a refused blob) and the kill-of-pid-0 guard (df485a0): crash
  fixes; off is the abort.
- The fatal-signal lines and end records (df485a0, fork bd4bbf2), the Wi-Fi traffic line (cbf97ea), the memory
  pressure line (3b8742a), `perf: qos:`, `thermal: 30 s clocks:`, `cpu: policyN: governor`, the E/P hand-off count
  (03d85ac), the `host_wait%` column (3b36ff0), 0095's `thread calls:` line: logging only.
- The encoder helper's binder threads and its open watchdog (b3da193): the fix of the hang and its log line.
- Older keys that already were switches: `map-defer`, `gpu-level`, `gpu-priority`, `cpu-pin`, `ram-*`, `pen`,
  `encoder`, `wifi` (docs/booter.md section 1).

## A sample `xnuhost.conf` fragment

Every switch at its default (the build's behaviour). Change one line to turn one change back:

```text
# docs/switches.md: every switch at its default
session-mode=hosted
takeover-stops=default
stop-loop=wait
takeover-reclaim=1
takeover-swappiness=keep
task-trim=1
timer-coalesce=1
ram-fit-terms=tasks
ram-ondemand=0
uclamp-rule=work
gpu-down=governor
gpu-kgsl-ui-rb=0
gpu-off-level=1
gpu-blob-entries=65532
wifi-ring-wait=1
wifi-send-wait=1
wifi-buffers=1
wifi-joined-scan=cache
encoder-10bit=main10
encoder-warmup=1
display-post=apart
display-present=ahb
decoder=auto
decoder-codecs=h264,hevc,vp9,av1
astc=1
sensors=auto
sensors-hz=50
sensors-light=auto
# (oneios-motion and oneios-als-spu are not written here: xnuhost adds them when the hub gives the parts; a value
# written here is kept as it is, so oneios-motion=0 would turn the motion devices off)
# kernel and plug-in switches (boot-args-add appends to boot-args; one line, all twenty-two words):
# boot-args-add=oneios-wifi-rssi=1 oneios-telemetry-jobs=0 oneios-pool-cut=1 oneios-pressure-big=512 oneios-disable-allowed=1 oneios-nested-exact=1 oneios-upgrade-flush=0 oneios-fill-fast-fault=1 oneios-audio-rt=1 oneios-audio-curve=1 oneios-codec-irq=1 oneios-idle-spares=1 oneios-bo-cache-idle=1 oneios-keep-room=8 oneios-dl-full=1 sheng-vsync-min=60 oneios-compile-s=120 oneios-snapshot-astc=0 oneios-camera-notify=1 oneios-m2v-shader-fix=1 oneios-dont-care=0 oneios-queue-priority=1
```

## Tests

- xnuhost: `config::tests::the_switches` (each key's values and refusals, the image's conf and the command line over
  it, the switches line), `config::tests::ram_config` (`task-trim=0` 3528 MiB fit; `ram-fit-terms=old` 5688 MiB fit
  with the lock, a given key wins, 1 GiB headroom unlocked), `gpu3d_kgsl::policy::tests::down_with_a_wide_clock_range`
  (`gpu-down=bottom` stays at the top in the phone's case), `cpu::tests::release_floor` (`uclamp-rule=all`'s floor).
- Display (fix/frame-path): `config::tests::the_switches` (`display-post`), `display::tests::the_serial_post_option` (the
  options message after the hello with `serial`, none at the default), the helper's `test_run_serial` (a frame waits for
  the post before it) and `test_run_answers_before_the_post` (the default: answers within 50 ms of 100 ms locks).
- Bridges: `switches::tests::words_and_defaults`, `wifi::uplink::tests::no_wait_when_switched_off`; the encoder's
  `switches::tests::words` (with `decoder-codecs=`'s words); the decoder's `tests/decoder.rs` (`decoder=off`: the card
  and the formats of an encoder only; `decoder-codecs=vp9`: VP9 alone).
- Metal: metal2vulkan's `tests::spares` (12, fix/gpu-blobs); the plug-in of this build is made with the patch.
- Kernel: 0096 applies after 0095; the kernel of this build has both boot-args (`strings`).
- Kernel: 0097 applies after 0096 (`git apply --check` on the patched tree). Built on the laptop with `HOSTED_OUT=wired ONEIOS_CRYPTO=original` and the series less 0095 (its `recount.c:753` panic is fixed apart): `xnu.macho` `9a604844`, 412 s, releasable; prelinked to `out/kernel/24A446-wired/kernel.kc` `2890c098` (`.hashes` `cfd918a8`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 8 of 8. `config::tests::the_switches` has `oneios-pressure-big=0`. Merged after the fixed 0095 (2026-11-24) and built again with the whole series: `xnu.macho` `32f88a27` (182 patches, releasable), `kernel.kc` `e79886d9` (`.hashes` `e06b673d`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 10 of 10, `cargo test -p xnuhost config::` 16 passed. Under qemu-aarch64 (8 vCPUs, 4 GiB, the integration runtime) 20 minutes, no panic, SpringBoard at +247 s, `oneios-wired` and `oneios-pressure` every 60 s; no pressure rise there, so the default 512 itself is not exercised (kernel/patches/oneios-hosted/README.md, "0097").
- Kernel: 0098 (`oneios-disable-allowed`) and 0100 (`oneios-kill`, logging) apply after 0097; `kernel/tests/host.sh` builds 0098's rename from the patch with a made-up plist (all 11 of host.sh pass), and the same rename of the real 24A446 plist leaves a plist plistutil reads, only the 19 keys changed. Built with `HOSTED_OUT=disallow ONEIOS_CRYPTO=original` (184 patches): `xnu.macho` `7ecd6622`, releasable; `kernel.kc` `a07f5426` (`.hashes` `80688378`), audit 0 failures, stubs-check 666 good 0 bad. `config::tests::the_switches` has `oneios-disable-allowed=0` (16 passed). Not booted.
- Kernel: `oneios-audio-rt` (fix/audio-late): the audio archive built against `HOSTED_OUT=wired`'s tree (`oneios-tool kernel gen audio`, no warning); `cargo test -p oneios-kernel` (the edit list on the synthetic source, the staged hash); `config::tests::the_switches` has `oneios-audio-rt=0`. Then the kernel, built on the laptop with the whole series (`HOSTED_OUT=audiolate ONEIOS_CRYPTO=original`, 686 s, 182 patches, releasable): `xnu.macho` `6d40f8e0`; prelinked to `out/kernel/24A446-audiolate/kernel.kc` `853068df` (`.hashes` `fa7166ba`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 10 of 10. Not booted.
- Kernel, merged (0098, 0100 and `oneios-audio-rt`): built from the merged tree, `xnu.macho` `0f51c0e3` (184 patches, releasable), `out/kernel/24A446-merged/kernel.kc` `af1321f7` (`.hashes` `ef107ec1`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 11 of 11, `cargo test -p xnuhost config::` 16 passed (`the_switches` with both). Not booted.
- Fork: `OneiosScriptTest.theStopLoopLooksOnceASecondWithARuntimeFromBeforeThePropertyWait` (`stop-loop=poll`: no
  `--prop-wait` call, its line).
- Kernel: 0101 (fix/unmap-fanout) applies after 0100 (`git apply --check` on the patched tree). Built on the laptop with
  `HOSTED_OUT=unmap ONEIOS_CRYPTO=original` and the merged series (185 patches): `xnu.macho` `346e8b67`, 415 s,
  releasable; prelinked to `out/kernel/24A446-unmap/kernel.kc` `a041bc20` (`.hashes` `c81e4ea7`), audit 0 failures,
  stubs-check 666 good 0 bad; `kernel/tests/host.sh` 10 of 10. xnuhost `timer-coalesce`: `config::tests::the_switches`
  (its values, a refusal, the line with the three kernel switches); `runtime/host-tests.sh` (no qemu part): lint,
  lint-android, port, links and package pass; workspace 100 of 101 in onebridges, the one failure
  `audio::test_audio_helper::audio_helper` (`underruns` 1, a test that needs the host to run its threads on time; it
  passed run alone, and this change touches no bridge). Before the owner's no-qemu rule, two partial qemu boots of
  the same code (all four off against the defaults): kernel/patches/oneios-hosted/README.md "0101". Not booted on the
  phone.
- The integration build of 2026-11-24 (main d3b802f: takeover-ram, m2v-update, frame-path, m2v-rt, hw-decode, with
  main's 0098, 0100 and 0101; fork e00fb92), on the build laptop: `runtime/host-tests.sh` passes (361 cargo results, 0
  failed; qemu boot skipped), `kernel/tests/host.sh` 11 of 11, `config::` 16 passed, the fork's `./gradlew test
  --rerun-tasks` 734, 0 failures. Kernel `HOSTED_OUT=i2 ONEIOS_CRYPTO=original` (185 patches, the decoder's codec
  edit): `xnu.macho` `4fb1cf6a`, releasable; `kernel.kc` `4db993fc` (`.hashes` `e1b3ee7e`), audit 0 failures,
  stubs-check 666 good 0 bad. Add-ons with the camera plug-in (`fda818f2`): 624bab3 (NativeAdrenoMetal `080821ef`,
  Turnip `5652c89d`; 12,330,152 bytes) and rt (`7a398d78`, `bbe6c736`; 9,538,920 bytes). Runtime 0.1.21-dev
  `0fc186a8` (xnuhost `7f33cd68`, oneios-encoder `79bc4a70`). APK 0.5.87 (216) `b5d72170`, the shared debug key.
  Handed to the Mac as `out/handoff/integration2-d3b802f/`. Not booted.
- Kernel: `oneios-audio-curve` with `oneios-audio-rt` (fix/audio-volume-rt: fix/audio-late merged on main e882296, and
  the volume curve), on the build laptop: `cargo test -p oneios-kernel` (17 + 15: `the_volume_curve_is_shengs`, the
  edit list on the synthetic source), `cargo test -p xnuhost config::` 20 passed (`the_switches` with both words),
  `kernel/tests/host.sh` 13 of 13. Kernel `HOSTED_OUT=audiovol ONEIOS_CRYPTO=original` (193 patches, releasable; the
  audio archive compiled without a warning): `xnu.macho` `68774879`; prelinked to `out/kernel/24A446-audiovol/kernel.kc`
  `ff1c70e5` (`.hashes` `b32aeb51`), audit 0 failures, stubs-check 666 good 0 bad; the staged `DroidVMAudio.cpp`
  `6769f38c` (45,220 bytes) in `golden/staged.sha256`. `runtime/host-tests.sh` (no qemu part): workspace, lint,
  lint-android, port, links and package pass; `audio::test_audio_helper::audio_helper` run alone passed (112 s, load
  average 11.9 at its start, 5.0 at its end). Runtime 0.1.27-dev at 1c3e86a (xnuhost `b3581f79`) and the kernel handed
  to the Mac as `out/handoff/audiovol-1c3e86a/`. Not booted.
- fix/s24-fps-heat-camera (2026-11-25, build laptop; `docs/s24-fps-heat-camera.md` "The fixes on this branch"):
  `config::tests::the_switches` (the seven new words in the line, `astc`'s values and a refusal),
  `config::tests::host_device` (the A56's `astc=0`; a command line's `astc=1` wins), `hw::test_hw_backend::host_device`
  (`astc` a device key), `loader.rs` `machine_identity_tree` (both feature-set lists with and without `astc`),
  `oneios-kernel` `golden.rs` (LEGACY fits the pinned NativeLegacyDisplay.cpp), `oneios-sym` `synthetic.rs`
  `strings_pointers_and_page_pairs`; `kernel/tests/host.sh` 13 of 13; `userland/camera/tests/host.sh` (26 and 101
  checks); `runtime/host-tests.sh` (no qemu part): lint, lint-android, port, links and package pass; the workspace (`cargo test --workspace --no-fail-fast`) 397 of 398, the one failure `audio::test_audio_helper::audio_helper` (1 underrun: a test that needs the host to run its threads on time; it failed under a load average of 21 from other agents' builds, passed in this branch's first run, and this branch changes no audio code). Kernel `HOSTED_OUT=s24fps ONEIOS_CRYPTO=original` (193 patches):
  `xnu.macho` `8971ecc9`, releasable, has `sheng-vsync-min` and `oneios-display: refresh range`; `kernel.kc`
  `a821fd1e` (`.hashes` `196e4c94`), audit 0 failures, stubs 666 good 0 bad. Add-on (624bab3 with
  `m2v-compile-deadline.patch` and the four new `.inc` files, + the camera plug-in): NativeAdrenoMetal `25bfacc8`,
  Turnip `ffee9a7c`, H9ISP `59c042c5`; 12,333,520 bytes. Not booted.
- fix/s24-fps-heat-camera round 2 (2026-11-25, build laptop, after the merge of main a1200e5): `runtime/host-tests.sh`
  (no qemu part): workspace (404 passed, 0 failed, `audio::test_audio_helper::audio_helper` among them), lint,
  lint-android, port, links (the cdylib with the AHardwareBuffer and ASurfaceTransaction imports links with `-z defs`),
  package pass. `display-present`: `config::tests::the_switches` (its values, a refusal, the default `ahb`, the word in
  the line, with `oneios-m2v-shader-fix`), `display::tests::the_serial_post_option` (the options after the hello: bit 1
  alone at the defaults, both bits with `serial`, no message with `window`), the helper's `test_present_choice`,
  `test_present_fallbacks`, `test_present_ring`, `test_present_in_flight_and_release`, `test_present_gives_up`,
  `test_present_run`. The Metal ports: `cargo test -p oneios-ios` (`the_camera_edits_apply_once_each` with the 3c46b5e
  edit), `git apply --check` of `turnip-submit-split.patch` on the staged Mesa; `userland/camera/tests/host.sh` (26 and
  101 checks); `audio_helper` again alone: passed. Built: the add-on (NativeAdrenoMetal `4054ab4c`, Turnip `90adff87`,
  H9ISP `59c042c5`; 12,334,688 bytes; its strings have the Turnip warning, `METAL2VULKAN_COMPILE_DEADLINE_S` and the
  shader fix's line), runtime 0.1.28-dev at 8ded46c (xnuhost `44bbc51c`, liboneios-display.so `4994ef8d`), the kernel of
  e84a46e unchanged; handed to the Mac as `out/handoff/s24fps-8ded46c/`. Not booted.
- Not tested on a phone: any non-default value, and most defaults (column "Phone").

## Found while building the Metal plug-in (2026-11-23; shader fixes enabled 2026-11-24)

- `userland/metal/patches/metal2vulkan/0101-0103` are copied into the staged tree as `90101-...`; the base's
  `build-adreno-metal-bridge.py` applies a fixed list of names, not every `*.patch`. Until 2026-11-25 no plug-in built
  here had them. Since then (port/oruma-2026-11-24) `ios metal-stage-edit` appends 90101, 90102 and the switch 90110
  to the bridge's list, behind `oneios-m2v-shader-fix` (the table), on by default since the merge into main (2026-11-25). 0103 stays in no list: it does
  not apply on the bridge's stack (`crates/metal2vulkan-runtime/src/memory.rs`, checked 2026-11-25 on the pin plus the
  51 patches of the base's list and OneiOS's three).
- `build-turnip-darwin.py` re-uses an existing `build/mesa-tu-ios27` with `meson configure` (options only), so a
  changed cross file never reached it (the helper of fix/gpu-blobs removed the directory by hand). `build.sh turnip`
  now keeps the cross file's inputs (the SDK, the four tools `xcrun --find` gives, the script's `cksum`) in
  `build/mesa-tu-ios27.cross-inputs` and removes the directory when they changed; a directory with no record is kept
  and recorded.
- (2026-11-25, fix/s24-fps-heat-camera) After the merge of main's 90101, 90102 and 90110 the bridge script stopped:
  `Translator source differs from the pinned revision plus its recorded patches`. The staged translator
  (`out/metal/root/.cache/gpu-reference/metal2vulkan`) had this branch's `m2v-compile-deadline.patch` applied right
  after `m2v-idle-memory.patch`, and the script takes only the pin plus a prefix of its list, which now has the three
  shader patches first. `build.sh stage` now keeps a checksum of the staged bridge script and every translator patch
  in `build/m2v-patches.stamp`; when it changed (or no stage recorded one and the tree has changes) the source goes back
  to the pin, said in a `metal: the translator's patch list or a patch changed since the last stage: ...` line, and
  the script applies the whole list.
