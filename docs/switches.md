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
- No switch for pure logging, and none for a crash fix whose "off" would be the abort again.

xnuhost logs every switch once, after the `versions:` line:

```text
switches: session-mode=hosted stop-loop=wait task-trim=1 ram-fit-terms=tasks uclamp-rule=work gpu-down=governor gpu-blob-entries=65532 wifi-ring-wait=1 wifi-send-wait=1 wifi-buffers=1 wifi-joined-scan=cache encoder-10bit=main10 encoder-warmup=1 display-post=apart oneios-wifi-rssi=1 oneios-telemetry-jobs=0 oneios-pool-cut=1 oneios-pressure-big=512 (docs/switches.md); all at their defaults
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
| `task-trim` | `0\|1` | `1` | 1: an idle task's host mappings are dropped and ram=auto counts 150 tasks awake (5376 MiB on the S24 Ultra); 0: no trim, every task at 4 MiB: exactly 3b8742a's sizing (`config::tests::ram_config`: 3528 MiB fit) | 07b10a8 | no |
| `ram-fit-terms` | `tasks\|old` | `tasks` | tasks: ram=auto's 3b8742a terms (tasks, hold share, 1536 MiB headroom locked); old: the terms before 3b8742a (headroom 768 MiB locked / 1 GiB unlocked, page tables 8 %, no task cost, no hold share; any of those keys given still wins). With `task-trim=0` too this is the v0.1.19 sizing | this build (3b8742a) | tasks: no; old: v0.1.19's sessions |
| `uclamp-rule` | `work\|all` | `work` | work: xnuhost alone sets a vCPU thread's uclamp (0 at idle, the released thread's floor, the guest's request); drm2kgsl's animation boost only for the frame's threads; background/utility 0. all: before 03d85ac: drm2kgsl's boost on every started vCPU thread and every iOS thread not background, the cpufreq device sets the vCPU thread's clamp, nothing at idle | 03d85ac + this build | no |
| `gpu-down` | `governor\|bottom` | `governor` | governor: the GPU level leaves the top when a frame fits at the governor's clock in a burst (learned ratio); bottom: when it fits at KGSL's lowest clock (before 3b36ff0: on the S24 Ultra it never came down) | 3b36ff0 + this build | bottom: every session up to 21:11 (the GPU at the top all session); governor: no |
| `gpu-blob-entries` | `16384..65532` | `65532` | the most entries of a guest 3D blob the device takes; 16384: the limit before 78c4b86 (an 80 MiB blob of single pages refused; drm2kgsl 0010 keeps that from aborting) | 78c4b86 + this build | no |
| `wifi-ring-wait` | `0\|1` | `1` | 1: a frame for iOS that finds its receive ring full waits for the guest's kick (20 ms data, 50 ms the station's own); 0: dropped at once (before cbf97ea) | cbf97ea + this build | 1: 2026-11-23 19:40 |
| `wifi-send-wait` | `0\|1` | `1` | 1: a frame of iOS's that finds wlan0's send buffer full waits (256 at most, 20 ms); 0: it fails at once (`send failed, last errno 11`, before b704922) | b704922 + this build | 0: 19:40 (the old code); 1: no |
| `wifi-buffers` | `0\|1` | `1` | 1: wlan0's AF_PACKET socket gets a 4 MiB receive and a 1 MiB send buffer; 0: the kernel's defaults (before cbf97ea) | cbf97ea, b704922 + this build | 1 (receive only): 19:40 |
| `wifi-joined-scan` | `cache\|low\|plain` | `cache` | cache: within 120 s of the last scan its networks answer, else a low-priority scan; low: every scan the radio's, low priority (cbf97ea); plain: every scan the radio's, no flags (before cbf97ea) | b704922, cbf97ea + this build | low: 19:40 (the driver took no flags: `flags 0x0`); cache: no |
| `encoder-10bit` | `main10\|8bit\|nv12` | `main10` | main10: the device takes P010 (iOS's 10-bit tiles) and the helper codes HEVC Main10 where the encoder offers it, else 8-bit of the high bytes; 8bit: P010 taken, Main10 never tried; nv12: the device lists and takes no P010, the guest sends NV12 (its high bytes). Not exact: before b3da193 the guest refused 10-bit tiles itself (stage 101), which the kernel no longer does | b3da193 + this build | no |
| `encoder-warmup` | `0\|1` | `1` | 1: the encoder helper starts at the guest's first virtio-media session and opens one HEVC encoder at once; 0: it starts at the first encode, no warm-up (its binder threads start either way: that is the fix of the hang, not a choice) | b3da193 + this build | no |
| `display-post` | `apart\|serial` | `apart` | apart: the display helper reads and answers each frame as it comes (a reader thread) while its poster waits for the layer's buffer, fills it and posts the newest picture; serial: xnuhost sends the helper `options` (`XHDO`, bit 0) and its reader reads nothing more until the last change is posted, the one loop's timing before 2026-11-24 (exact: the same order of read, answer, lock, fill, post). A helper older than this build ignores the message (it has the one loop anyway) | fix/frame-path | no |
| `boot-args-add=oneios-wifi-rssi=0` | `0\|1` | `1` | 1: the Wi-Fi driver posts RSSI_CHANGED records (the bars follow the signal); 0: none, wifid reads the signal at the join only (before cbf97ea). Kernel (net/OneiOSWifi.cpp) | cbf97ea + this build (db2eb42) | no |
| `boot-args-add=oneios-telemetry-jobs=1` | `0\|1` | `0` | 0: 0093 and 0094 disable the telemetry group (osanalyticshelper and its agent job, 16 uploaders); 1: the disable list is 0053's loops only and the enable pass enables the telemetry labels again (a store 0093 changed gets them back). Kernel, hosted 0096 | bad88c3, 30bdea6 + 0096 | 0093 alone: 20:23 (osanalyticshelper still ran: 0094) |
| `boot-args-add=oneios-pool-cut=0` | `0\|1` | `1` | 1: metal2vulkan releases a spare command pool that holds more than its last 10 s need plus 8 command buffers (the render server's small command-stream BOs go back); 0: the pools are kept as before (the plug-in sets `METAL2VULKAN_POOL_USE_WINDOW_S=0`). Metal plug-in only (the boot add-on), docs/gpu.md "Who keeps a blob" | 50873fe | no |
| `boot-args-add=oneios-pressure-big=0` | `0\|MB` | `512` | 512: at a memory pressure rise a process of at least 512 MiB footprint not yet notified at that level is notified first, the largest first (the render server and the apps that draw hear of it in the first 250 ms steps, not after 150-300 smaller processes); 0: XNU's order (least important band first, 250 ms apart: the GPU clients last). Kernel, hosted 0097; its `oneios-wired` and `oneios-pressure` lines are logging (no switch) | 0097 (fix/wired-names) | no |

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
task-trim=1
ram-fit-terms=tasks
uclamp-rule=work
gpu-down=governor
gpu-blob-entries=65532
wifi-ring-wait=1
wifi-send-wait=1
wifi-buffers=1
wifi-joined-scan=cache
encoder-10bit=main10
encoder-warmup=1
display-post=apart
# kernel switches (boot-args-add appends to boot-args; one line, all four words):
# boot-args-add=oneios-wifi-rssi=1 oneios-telemetry-jobs=0 oneios-pool-cut=1 oneios-pressure-big=512
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
  `switches::tests::words`.
- Metal: metal2vulkan's `tests::spares` (12, fix/gpu-blobs); the plug-in of this build is made with the patch.
- Kernel: 0096 applies after 0095; the kernel of this build has both boot-args (`strings`).
- Kernel: 0097 applies after 0096 (`git apply --check` on the patched tree). Built on the laptop with `HOSTED_OUT=wired ONEIOS_CRYPTO=original` and the series less 0095 (its `recount.c:753` panic is fixed apart): `xnu.macho` `9a604844`, 412 s, releasable; prelinked to `out/kernel/24A446-wired/kernel.kc` `2890c098` (`.hashes` `cfd918a8`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 8 of 8. `config::tests::the_switches` has `oneios-pressure-big=0`. Merged after the fixed 0095 (2026-11-24) and built again with the whole series: `xnu.macho` `32f88a27` (182 patches, releasable), `kernel.kc` `e79886d9` (`.hashes` `e06b673d`), audit 0 failures, stubs-check 666 good 0 bad; `kernel/tests/host.sh` 10 of 10, `cargo test -p xnuhost config::` 16 passed. Under qemu-aarch64 (8 vCPUs, 4 GiB, the integration runtime) 20 minutes, no panic, SpringBoard at +247 s, `oneios-wired` and `oneios-pressure` every 60 s; no pressure rise there, so the default 512 itself is not exercised (kernel/patches/oneios-hosted/README.md, "0097").
- Fork: `OneiosScriptTest.theStopLoopLooksOnceASecondWithARuntimeFromBeforeThePropertyWait` (`stop-loop=poll`: no
  `--prop-wait` call, its line).
- Not tested on a phone: any non-default value, and most defaults (column "Phone").

## Found while building the Metal plug-in (2026-11-23)

- `userland/metal/patches/metal2vulkan/0101-0103` are copied into the staged tree as `90101-...` but **never applied**:
  the base's `build-adreno-metal-bridge.py` applies a fixed list of names, not every `*.patch`. No plug-in built here
  has them. Left so on purpose (they change shader translation and no phone has run them); applying them is a change
  for its own commit, behind a switch. `userland/metal/build.sh` says so now (its comment and its `stage` line).
- `build-turnip-darwin.py` re-uses an existing `build/mesa-tu-ios27` with `meson configure` (options only), so a
  changed cross file never reached it (the helper of fix/gpu-blobs removed the directory by hand). `build.sh turnip`
  now keeps the cross file's inputs (the SDK, the four tools `xcrun --find` gives, the script's `cksum`) in
  `build/mesa-tu-ios27.cross-inputs` and removes the directory when they changed; a directory with no record is kept
  and recorded.
