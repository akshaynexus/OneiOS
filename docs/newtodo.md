# Paused work (2026-11-13): what is done, what is next, how to resume

The owner paused the work on 2026-11-13 at about 15:00 IST. This file is the hand-over. Two machines worked on it:
the Mac (Xcode, the phone on its USB, the phone sessions) and a Linux PC (CachyOS, x86_64, 4 CPUs: the kernel, the
runtime, `oneios-tool`, PC boots under qemu-aarch64). Read this with `README.md` "Status", `docs/agents.md` and the
docs each item names. **Proven** = ran on the phone; **built** = compiled and host-tested, not run on the phone.

## State at the pause

- `main` holds everything below. Every open branch was merged in one integration tree, its kernel was built and
  prelinked once (audit 0 failures, stubs 666 good), and the runtime host tests ran on it, then it went to `main`.
- **Off by default on `main`** (merged so that nothing is lost, inert until turned on): the fixed RAM
  (`ram-giveback=0`), the graceful return (`return-graceful-ms=`), the wide display path (`display-format=rgba16f`),
  the bigger HID queue (`oneios-hid-queue-kib=`), the GPU read wait (`oneios-gpu-read-wait=1`), the stand-in
  actuator (`haptics=sim`), the fork's USB repairs (the file `/data/local/tmp/oneios/usb-repair`), AAudio's low
  latency (`ONEIOS_AUDIO_PERF=low`).
- **On by default and new on `main`, none of it run on the phone yet:** the audio helper's lower latency (#21),
  `pull-data` in chunks with resume and per-chunk sums (#25), the Wi-Fi scan TIMESTAMP and channel list (#22),
  xnuhost's `usb:` lines (#24), the haptics, widget and display counters (#16, #17, #15), sRGB-tagged frames (#15).
- **The phone:** on the Mac's side; last session `main` `c2d281a` (kernel.kc `1be6e0c9`, runtime 0.1.9-dev
  `b2b59858`), the Data store kept (do not stage a new seed: a new seed starts the store over). Root is not
  persistent: after a restart the owner roots it again.
- **Machines:** the Linux PC has the NDK, Rust, libdispatch (kernel builds), qemu-aarch64, apfs-fuse, the IPSW in
  `inputs/24A446`, a PC image set in `out/image-pc/24A446`. It has no Xcode (iOS-side code builds on the Mac only)
  and no phone. Box-wide queues on the PC: kernel builds through `flock /tmp/oneios-kbuild.lock`, PC boots through
  `flock /tmp/oneios-qemu.lock` with `--ram 4G` or less.

## Done on 2026-11-12/13 (all on `main`)

| PR | What |
|---|---|
| #1, #2 | the Rust port: `oneios-tool` (sym, log, image, ioslog, prelink; later phone, app, cycle) |
| #3 | `sym kernel` and `log panic`: kernel panic frames named (per-segment kc→xnu.macho, physical aperture, DWARF) |
| #4, #11 | the kernel patch generators, `build-record`, drivers, prelink helpers, package check, image pack/verify/fetch, rangeserve in Rust; the Python retired (1:1, goldens). CLAUDE.md: no new Python |
| #5 | WebKit's crash loop: diagnosis (the CODE SIGNING line is normal; a919ff3 turned a kernel panic into a per-process crash) and a JIT test |
| #6, #13, #25 | the sparse copy; ioslog reads a used Data store (checkpoint fallback, block checks); `pull-data` in chunks with resume |
| #7 | battery: the first state carries no change message (the chime's real cause is elsewhere: item 4) |
| #8, #10, #22 | Wi-Fi: a reserved unit for en0, the DHCP log fix, 0061 `oneios-ubt-count`, the scan TIMESTAMP at 0x8d0 and CHANNELS_INFO |
| #9, #20 | Bluetooth: 0062 IOKit probe trace, `bluetooth=sim`, the reading of bluetoothd's VM path |
| #12 | 0060: the lsd recovery file a killed session left is removed |
| #14 | 0066: free-page runs; the thermal step no longer stacks P vCPUs; the GPU memory counters |
| #15-#25 | the paused faults below (each partly done, safe, most of it off by default) |
| OneOS #1 | the fork: USB state recorded at the switch, a USB watcher in the session, restored at the return |

## Open work, in order (the detail of each is in the doc it names, and in "Fault detail" below)

1. **The next phone boot** (one session answers most of the items below). Build from `main`. Boot arguments, in
   the image's `xnuhost.conf`, beside the default ones:
   `oneios-ubt=configd,wifid,logd,Spotlight,kbd,backboardd,Preferences,InputUI oneios-ubt-at=20 oneios-ubt-every=10
   oneios-ubt-count=150 oneios-iouc=kbd,Spotlight,WidgetRenderer_Default,chronod oneios-sysfail=configd,kbd,Spotlight
   oneios-hid-queue-kib=256 owner-hide-vmm=bluetoothd,BlueTool`. In the session: Wi-Fi with a known network in
   range, type a Wi-Fi password for a minute, stay on a widget page, flip Bluetooth, tap keys with click sounds on,
   plug and unplug USB, then return. After it: `oneios-tool phone pull-data data.img` and the searches of each item.
2. **Wi-Fi joins** (`docs/hardware.md` 3): look for `ONEIOS WIFI: scan record 1 ... TIMESTAMP` a few ms before now,
   `channels info: N channels (the host's list)`, then `join:`; in iOS's log `AUTO-JOIN: Scan Results (N>0)`.
   Then configd's 22 s between `*STACK*` and en0 from the `oneios-ubt` samples, and wifid's 17 fault entries.
3. **The keyboard stops after 3-4 characters** (`docs/newtodo.md` 12, PR #23): with `oneios-hid-queue-kib=256`,
   `oneios-hid: touch screen: event queue 256 KiB` and `N dropped` 0; whether the whole UI or only the keys stop;
   the MobileAsset lock line's errno, the error after InputUI's `Failed to access`. A/B boot:
   `oneios-enable-jobs=com.apple.analyticsd,com.apple.coreduetd,com.apple.siriknowledged,com.apple.modelmanagerd,com.apple.modelcatalogd,com.apple.intelligenceplatformd`.
4. **The chime** (`docs/hardware.md` 7c, PR #19): the host side of the graceful return is done. **Write the guest
   power-off** behind `oneios_power_request()` (`drivers/bridge/OneiOSBridge.c`): a hosted patch (0059 is free)
   that starts iOS's clean power-off as a power-button shutdown does, a halt, never a reboot. Then a PC boot with
   `--set return-graceful-ms=10000`, then the phone; only then make 10000 the default.
5. **Memory held fixed** (`docs/booter.md` 2.2 "Fixed RAM", PR #18): uncomment 0067 in the series, build, PC boot
   before/after with `ram-giveback=0`, then the phone with `ram-giveback=0`; make it the default once the PC and
   the phone are clean. GPU memory is the guest's own RAM, so no separate pool is needed (`docs/gpu.md` 2).
6. **Haptics and the power key** (`docs/hardware.md` 5, PR #17): the counters show where a tap stops. PC boot with
   `--set haptics=sim`; phone: `Actuator: user client N opened by audiomxd`, audiomxd's `actuator port` lines; the
   power-key boot with `oneios-actuator=0`. **Power key (fix-power-key, 2026-11-14; confirmed broken in the 9f6dc38
   session):** a stand-in for the button haptics service ButtonResolver waits for, `oneios-button-haptics=1` (off by
   default; built, not on the phone) and, since a PC boot showed backboardd taking the buttons for an external keyboard
   (`eventSource: externalKeyboard`, `destinations:<none>`), `oneios-hid-buttons-builtin=1` (off by default); the 15:35
   boot also brought `BacklightMode.inc` (`oneios-bl-mode=0` is the test of that). The A-E boots and the lines to read are in `docs/hardware.md` 5, "The power
   key: a stand-in for the button haptics service".
7. **Blank widgets** (`docs/gpu.md` 5.1, PR #16): the `oneios-gpu-surfaces/pixels/locks/sync` lines and the host's
   `gpu: KGSL imports` say where the image is lost; the four guesses and how to read them are there. The likely
   fix (guess 1) is in the Metal plug-in: the Mac builds it, with a new trust list and seed.
8. **Bluetooth** (`docs/hardware.md` 4, PR #20): with `owner-hide-vmm=bluetoothd,BlueTool`, does `Booting chipset`
   appear; then what the non-VM path needs (likely an `AppleOLYHAL` with `HWIdentifiers`). Also the PC's
   root-device stall with the Bluetooth driver on.
9. **Display colour and blur** (`kernel/patches/oneios-hosted/display/README.md`, PR #15): on the phone
   `displayctl test --format rgba16f` (a photo of the P3 patches), the compositor's swap format lines; the Mac
   builds `userland/metal/DisplayWide.inc`; open: 10-bit `w40a`, EDR keys for the backlight, an `identity.adt` in
   the image set. Ask the owner for real iPhone 18 Pro screenshots (Control Center over the Home Screen, Settings'
   navigation bar over content).
10. **Audio** (`docs/hardware.md` 6, PR #21): `latency about N ms` (about 100 expected, was 130-137), the owner's
    click-recording test, then `oneios-audio-latency=` set from it and made the default.
11. **USB and adb in a session** (`oneos/interface.md` 6a, PR #24, OneOS #1): first check the cable and the Mac's
    port with One UI whole; a session with repairs off (plug, unplug, plug), then one with the `usb-repair` file.
    The fork's three new app tests run first in its CI.
12. **`pull-data` over wireless adb** (PR #25): the chunked copy has run on the stand-in only; check busybox's `dd`
    and `sha256sum` on the phone, `--verify end` against `--verify chunk`, a break and a resume.
13. **Slow Home Screen** (0060): `oneios-ls-state:` and `oneios-icons-state:` lines; lsd's fixed start work stays.
14. **WebKit** (`docs/newtodo.md` 10): the WebContent crash report's exception type decides the fix.

## Decisions waiting for the owner

- `scripts/image/imagetool.py` and `scripts/fetch/remotezip.py` are unused on `main` (the Rust commands replace
  them): delete them?
- `userland/roottweak` (Python group 4): port it to Rust? A mistake changes the seed, so only with a byte-for-byte
  seed comparison.
- `scripts/ios/{ios-sign,macho-check,make-tbds}.py` are still Python (not ported yet).
- The token for `gh`: rerunning a failed CI job needs **Actions: Read and write**.
- The WebKit agent of 2026-11-12 ran on Opus 4.8, not 5.5 (PR #5: diagnosis and a test only). Redo it on 5.5?

## Fault detail (written by each fault's agent; newest notes first in each)


1. **Memory pressure stalls the UI** (15:35 boot, fresh store). The guest has 4,070 MiB; GPU blobs hold about
   1.6 GB and backboardd's share grows 760 -> 930 MiB in 30 s; swap does not work ("Failed to open swap file",
   errno 2 and 30); `VM_pageout_scan` takes ~95% of a vCPU; every page it frees fans out unmaps to almost every task
   (1.47 million round trips per 30 s, ~1.5 vCPUs); page fills ~660 us. Fixes, in order:
   - **Deferred unmaps: built and host-tested (2026-11-12), not on the phone.** A task with no running thread gets
     its unmaps queued (up to 256 with the maps) and carried out by the stub before its next run. The WIP
     (`a97687f`) let another thread of the task run while a release or an order was still carrying the queued
     unmaps out; fixed (`task.unmaps_out`), with a two-vCPU check in the test kernel (`docs/booter.md` 2.6). The PC
     benchmark (qemu): the `task_unmap` call 17-18 us -> 1.0-1.2 us; a fault, fill and unmap ~53 -> ~36 us. On the phone:
     read the `perf:` line's `task_unmap N queued, M ordered` and the `VM_pageout_scan` share.
   - **RAM: the owner's decision: iOS gets at least 90% of the phone's RAM. `ram=auto` built and host-tested
     (2026-11-12), the default; not on the phone.** Guest = min(90% of MemTotal, MemTotal - 1.5 GiB) in 256 MiB steps
     (about 9.5 GiB on this phone); `ram-percent=`, `ram-host-floor=`, `host-mem-log=` (30 s), also in the image set's
     `xnuhost.conf`; the choice and the host's MemAvailable in the log (`docs/booter.md` 1). Checked: the window,
     `/chosen dram-size`, `boot_args.memSize`, `xh_info.ram_size` (test kernel); the GPU pool is fixed (8 MiB of
     response buffers; blobs are guest pages). **A large guest boots (the PC, 2026-11-12):** `ram=auto` on the PC
     (MemTotal 15,872 MiB) gave 14,080 MiB; XNU set up its ranges (`zones` 24G, the same at 2 GiB) and booted through
     launchd to SpringBoard in 420 s under qemu with no panic. OOM score unchanged.
   - **Free pages back to the host: hosted 0058, built and run on the PC (2026-11-12), not on the phone.** The goal: iOS
     keeps all of its ram=auto memory; the host gets back only pages iOS has freed (the owner's decision: no pressure
     feedback, no cache cap, XNU's targets, memorystatus and jetsam unchanged). What filled the memfd was not the file
     cache: stock XNU's free queue is FIFO, so the boot walked through every never-touched page and the host kept all of
     them (control boot at `ram=auto`, 14,080 MiB: 12,266 MiB held at 990 s while XNU had 9,048 MiB free; the PC's
     MemAvailable fell under 1 GiB and the run was stopped). 0058 enters freed pages at the free queue's head and
     never-touched or given-back ones at its tail, and gives back free pages above the free target + 32 MiB with
     hysteresis (`ram_discard`, a hole in the memfd). Same boot with it: 4,973 MiB held at 990 s, XNU's view the same
     (free 9,084 MiB; active, inactive, speculative, wired within 15 MiB of the control's), launchd 292 s and SpringBoard
     333 s (control 306 s and 361 s), no panic; `hosted_mapcheck=1` boot: launchd, SpringBoard and the daemons, no panic, no `hosted check:` line, 893 MiB given back with
     `oneios-free-report-stress=1` (4 GiB). Cost: a page given back and
     used again costs the host's refill (+5.7 us per 16 KiB under qemu); on by default, `oneios-free-report=0` turns it
     off (`kernel/patches/oneios-hosted/README.md`, 0058). On the phone: the `memory:` line's `holds` against
     `oneios-mem:`'s in-use figures; if `host MemAvailable` still falls under ~300 MiB, lower `ram-percent` in the
     image's `xnuhost.conf`; check `/sys/kernel/mm/transparent_hugepage/shmem_enabled` (a shmem huge page setting
     may keep punched pages).
   - **Fixed RAM (fix-fixed-memory, 2026-11-13): built and host-tested, OFF by default (`ram-giveback=0` turns it on),
     not run in a PC boot of iOS or on the phone.** The owner's requirement: RAM and GPU memory set once at the start.
     xnuhost prefaults the whole memfd on a nice-19 thread, `mlock`s it where Linux allows and it fits in MemAvailable
     less `ram-session-reserve` (1 GiB; `ram=auto` gets the same bound), refuses `ram_discard`, unlocks once if the host's
     MemAvailable falls under `ram-unlock-below` (256 MiB), and logs a third `memory: fixed RAM:` line (locked, refused,
     host swap-ins/outs, major faults, KGSL's per-process memory). Kernel 0067 (the kernel told not to give back, order
     kept) is written but not in the series. No separate GPU pool: the blobs are guest pages of the same memfd
     (`docs/gpu.md` 2, "No separate GPU pool"). Details, safety and log lines: `docs/booter.md` 2.2 "Fixed RAM". Next: a PC
     boot before/after (`--ram 4G`, same Data store; then `--set ram-giveback=0`), kernel 0067 built, then the phone with
     `ram-giveback=0` in the image's `xnuhost.conf`.
   - **Swap: built (the Linux PC, 2026-11-12), not on the phone.** Hosted 0057 makes `/private/var/vm` root:wheel 0755 before
     launchd in every Data store and, on each swap file failure, logs which volume the folder was on and whether it
     was read-only; the roottweak's `data_layout` puts the folder in new seeds. errno 30 is not settled (the sealed
     root before the Data mount, or a Data volume gone read-only: `kernel/patches/oneios-hosted/README.md`, 0057).
     **Settled on the PC:** both are early tries before the Data mount (errno 2: the freezer's budget probe before the
     root is mounted; errno 30: the creation thread's first `swapfile0` on the sealed root's read-only
     `/private/var/vm`); 0057's folder is then there on the Data volume. No swap file was made in 20 min at 2 GiB on
     the PC (no swap-out asked). **Swap serves only frozen apps on iOS (read in xnu-12377 and seen on the PC):** iOS's
     kernel (`CONFIG_FREEZE`) defaults to `VM_PAGER_FREEZER_WITH_SWAP` (`vm_compressor_mode is 32` in the PC's log): the
     compressor is on, `swap_is_active` is off and `freezer_swap_is_active` on, so a general compressed segment is never
     queued for swap-out (`C_SWAPOUT_REG` needs `VM_CONFIG_SWAP_IS_ACTIVE`); only the freezer's segments
     (`C_SWAPOUT_FREEZER`, frozen apps), donated early segments (`C_SWAPOUT_DONATE`) and, with the boot argument
     `vm_compressor_scavenger_enabled=1` (off), old ones (`C_SWAPOUT_RIPE`) go to swap. So swap cannot relieve the
     pageout scan of the 15:35 session; [I] it helps only once iOS's freezer freezes apps (userland decides that; not
     seen on the PC). The device tree's `/defaults kern.vm_compressor` could set another mode (not set in the PC's
     image; the phone's log line `vm_compressor_mode is N` says). On the phone: a `Failed to open swap file` after the
     Data mount, or `swapfile0`. Compiled and prelinked on the Linux PC (`HOSTED_OUT=pc`,
     `kernel.kc` `ffbe95f7...`; `kernel/prelink/README.md`, "A Linux-built kernel against the list").
   - **backboardd's GPU memory growth** (gpu): read, not fixed (2026-11-12). Upstream NativeMetal (at our pin)
     already frees the runtime's empty 64 MiB blocks every 2 s even while backboardd draws (`watchIdleMemory`),
     drops held surface imports and every kept block on a memory-pressure rise and keeps no spare room while it stays
     raised (`watchImportPressure`, `NativeBridgeMetalFrontend.mm`). metal2vulkan keeps up to 2 empty blocks a pool
     and frees a stale one only when a placement of that pool is returned (`memory.rs` `Pool::release`/`stale`), which
     the 2 s timer covers. So the 170 MiB per 30 s is one of: IOSurface imports CoreAnimation still retains
     ("wired"), partly used blocks, or blobs the guest released that the host still holds. **On the phone:** the
     kernel's GPU report's clients line (own vs wired MiB for backboardd) and sizes line ("64+" count) every 10 s
     across the 30 s of growth, and `m2v-mem.txt` in backboardd's temporary folder on the Data volume (copied with the
     logarchive procedure below); then the fix goes where the number is (iOS side: built on the Mac).
     **2026-11-13 (hang, branch `fix-ui-hang-memory`): the holder is in backboardd itself, which of its memory is not
     known yet.** The phone's `blobs +182 closed +17` per 10 s means the client does not close them (the kernel frees
     what is closed, at once or after the client's fenced work; the host frees at `RESOURCE_UNREF`): read end to end,
     `docs/gpu.md` 5.1 "Who keeps a blob, and who frees it". Wired memory falling back when the UI settles fits the
     plug-in's 2 s trim of empty blocks, so the growth while busy is not yet a leak. New counters, so the next log names
     the kind: `oneios-gpu-kinds:` and `oneios-gpu-ages:` per client (own blobs in the task or the kernel, mappable or
     not; wired by the mapping's VM tag: IOSurface, malloc, other; sizes under 1 MiB; ages; closed blobs still held and
     the oldest wait), and xnuhost's `gpu: 10 s: host blobs N (M MiB), made +A unref'd +B` against the guest's live count.
     No plug-in change is written: which plug-in object to change waits for those lines.
2. **The power key does nothing in iOS** since the haptics change (15:35 boot): the presses reach backboardd (34 HID
   events, 0 dropped) but no `request_power_change`. **Read in 24A446 (2026-11-12, `docs/hardware.md` 5, "The power
   key since the Actuator"):** ButtonResolver's AOP interface waits for Haptics.framework's button haptics loader and
   the SPU haptics driver, and libMobileGestalt reads `/product/haptics closed-loop`; [I] with closed-loop = 1 the
   resolver is never ready. Waits for the phone: boot with `oneios-actuator=0`, press the side key (lock, wake), read
   ButtonResolver's `HasHaptic` lines; the fix candidates are written there. Hypothesis (not proven): with closed-loop haptics, backboardd's
   BKButtonHapticsController (its queue waits forever for button "click states") now sits in the button path. Quick
   test: boot with `oneios-actuator=0`. Fix: satisfy the click-state setup, or tell iOS the side buttons have no
   haptics. Not started.
   **Read further (fix-haptics, 2026-11-13, `docs/hardware.md` 5, "Haptics do nothing"):** the node sets two
   MobileGestalt answers, `DeviceSupportsHaptics` (the node exists) and `DeviceSupportsClosedLoopHaptics` (its
   `closed-loop`); ButtonResolver and Haptics.framework read neither (ButtonResolver: `HomeButtonType` and
   `MGIsDeviceOneOfType` only), so the node does not change whether ButtonResolver gets ready. backboardd's
   `BKButtonHapticsController` exists in every boot and blocks any caller of `playHapticForClickState:clickSpeed:`
   (MIG `_BKHIDXXPlayHaptic`) until ButtonResolver is ready. So the haptics fault and this one are not proven to share
   a cause; [I] the node may add a caller of that path or a lock path that waits on audiomxd's haptic server. The test
   is still `oneios-actuator=0`, now with the `oneios-ubt=SpringBoard,backboardd` lines after a press (the stacks to
   look for are in hardware.md).
2a. **Haptics and vibration do nothing** (23:45 boot): every layer "on", no request seen anywhere. Counters in every
   layer, the PC's stand-in actuator (`haptics=sim`) and what 24A446 reads: built 2026-11-13 (fix-haptics),
   `docs/hardware.md` 5, "Haptics do nothing"; the next boot's `ONEIOS HAPTIC: counters:` and xnuhost's `haptics:
   counters:` lines show the layer where a tap stops.
3. **The display helper dropped after 7 frames** (dark panel while iOS drew; restarted by hand). Fixed in `2c7bbb5`
   (xnuhost keeps the socket on transient send errors and logs; the helper logs why its read ended); host tests were
   started, not read; not on the phone.
4. **Brightness:** requests with the key `DisplayBrightness` were ignored ("no brightness in it"). **Handled (built,
   host-tested decoder, the driver compiled into the Linux PC's kernel of 2026-11-12, not on the phone):** 24A446's CoreBrightness sends the slider's position 0..1 (a
   float) or `{Brightness, Commit, Period}`; the driver takes it when unambiguous and logs what came
   (`drivers/brightness/README.md`, "DisplayBrightness"). If the log says `the integer 0 or 1`, the float was lost in
   IOKit's serialization and backboardd's side must send `brightness-nits`. Then check whether the
   slider moves the panel; auto-brightness: CoreBrightness refuses the light sensor (`AppleUSBALS: couldn't find the
   device version`).
5. **Camera:** try the Camera app; the log lines to read are in `drivers/camera/README.md` / `runtime/bridges/camera/README.md`.
   Waits for the phone. Steps: in One UI first `bin/camctl` (`runtime/bridges/camera/README.md`, "On the phone") to
   see the host's cameras; then a session with the image set that has the plug-in (`data.seed` with
   `H9ISP.mediacapture`, `oneios-camera-addon: ... stage 5, status 0` in the kernel's log), open Camera, and read
   cameracaptured's lines in iOS's log (`fcc_probeCameraDriverService` finding AppleH9CamIn, the plug-in's
   `H9ISPCaptureDeviceCreate`, its first frame).
6. **Audio latency** was ~130-140 ms on the host path (AAudio's fast/MMAP paths need a realtime thread, which is
   forbidden). **Built and host-tested 2026-11-13 (fix-audio-latency), not on the phone:** the stage shares and the
   changes are in `docs/hardware.md` section 6, "Latency per stage". The helper's level now follows the server's
   pulls (the burst of 1600 is the mixer's 33 ms period [I, AOSP's sizing rules]), its target comes back down after
   underruns, a backlog's silence is cut, AAudio's buffer is a burst plus the target plus 5 ms; LOW_LATENCY was
   studied (a blocking writer gets no FAST track and no realtime thread: AOSP source). In the host test's harness the
   guest-send-to-server-read time went 73 -> 33 ms (the phone's shape) and 268 -> 22 ms after a 200 ms start stall;
   predicted host path about 98-106 ms (Android's mixer and HAL after the read, 65-73 ms, are unchanged). The
   reported latency is unchanged (6480; 5040 predicted, set by boot argument after the phone measures; the kernel build of
   that default was stopped at the hand-off), LOW_LATENCY is opt-in (`ONEIOS_AUDIO_PERF=low`). On the phone: read the lines that section lists (`the server
   pulls N frames every M ms`, `margin`, `latency about N ms`) and time a key click against a recording.
7. **Bluetooth:** the uart.sk transport (e73c11d) has not been tried in iOS. Waits for the phone. Steps: a session,
   Settings > Bluetooth on, then the expected lines of `docs/hardware.md` 4 ("Expected lines, in order"), from
   `ONEIOS BT: dev: bluetoothd[N] opens /dev/btwake` to `bluetooth: the controller hears devices`; if they stop, the
   last `bluetoothd: command 0x....` line names the command.
8. **Hotel Wi-Fi:** iOS has an address; check that the captive-portal page shows and the internet works.
9. **CLAUDE.md's side-key rule** names event0/event2/event4, but the numbers move between boots (in the 15:35 boot
   event4 was the vibration motor). Proposed wording, by name: never grab `gpio-keys`, `pmic_pwrkey`, `pmic_resin`.
   Waits for the owner.
10. **WebKit content processes crash in a loop; Safari hangs** (2026-11-12 23:45 boot). Each `com.apple.WebKit`
    content process (pids 300-331, ~20 in 20 s) logs, in order, `triggered unnest of range ... of DYLD shared
    region`, then `CODE SIGNING: ... vm_map_entry_reinit_code_signing_after_copy: curprot cannot be write+execute.
    removing execute access`, then crashes; after ~20 corpses iOS says `Too many corpses being created`. MobileSafari
    sits at 70-101 % CPU (spinning on the content process it keeps losing). **Investigated 2026-11-12, not fixed; the
    host side is proven sound, the cause is phone-specific and needs the crash report.** What was ruled in/out:
    - The **CODE SIGNING line is benign** and matches a real device: `vm_map_entry_reinit_code_signing_after_copy`
      strips execute from the **copy** of a `used_for_jit` entry (not the executable one) on every iOS map, because
      `VM_MAP_POLICY_ALLOW_JIT_COPY` is always false on iOS (`VM_MAP_IS_ALIEN` is unconditionally true). It fires
      during WebKit's own `mach_vm_remap` of its JIT mirror and during corpse `vm_map_fork`. It is a marker, not the
      fault.
    - **a919ff3 reveals, it does not cause.** Before it, a task `brk` (a WebKit `RELEASE_ASSERT`/`__builtin_trap`,
      e.g. `brk #0xbb08`, or a PAC `brk #0xc471`) reached iOS as a kernel physical-aperture data abort and panicked
      the whole kernel (the 23:15 tcp_input panic was one such brk). After it, a task's `brk` becomes a clean
      `EXC_BREAKPOINT`, so each content process now dies on its own and respawns — the loop we see. The same CODE
      SIGNING line was on the 23:15 boot, i.e. the JIT path was exercised before too.
    - **SPRR/APRR fast JIT permissions are not used.** WebKit (`JSC::initializeJITPageReservation`) reads a commpage
      byte (`com.apple`'s fast-permissions version) to choose the inline `msr sprr_uperm_el0` path (which would be an
      illegal instruction on the S24's standard Arm cores) versus the separated-W^X-heaps path. Our kernel's commpage
      leaves that byte zero (open-source `commpage.c` never writes it), so WebKit takes separated heaps. The CODE
      SIGNING line (from the remap in that path) confirms it.
    - **The host carries separated-heaps JIT.** New test-kernel case `test_jit` (`runtime/xnuhost/tests/testkernel.c`,
      `make check-arm64`, passed under qemu-aarch64 2026-11-12): a W+X `task_map`, the same physical page mapped
      read-execute at a second address, a thread writing code through the read-write alias and running it through the
      read-execute alias with i-cache maintenance. So the hosted pmap/host-mmap W^X aliasing is not the cause.
    - **Open, phone-only:** (a) whether One UI's kernel (SELinux/KernelSU) permits a W+X mapping of the RAM memfd for
      a task process — qemu-user has no SELinux, so `test_jit` cannot cover it (`docs/booter.md` 7); if it refuses,
      `task.c` logs `tasks: pid N: a deferred map (va 0x..., prot 7) failed`. (b) a WebKit JIT assert under 0027's
      keys-off PAC on a 39-bit user VA (the `jitCagePtr`/arm64e JIT cage, same 39-bit class as the libobjc bug 0027
      fixed). The next boot needs the WebContent crash report to decide:
      `oneios-tool ioslog show data.img --proc com.apple.WebKit.WebContent` and `--proc MobileSafari`, plus
      ReportCrash's `.ips` under `/private/var/mobile/Library/Logs/CrashReporter` for `com.apple.WebKit.WebContent`:
      read the **exception type** and faulting **pc/address**. `EXC_BAD_INSTRUCTION` at an `msr sprr_uperm_el0`
      means the commpage byte is non-zero on the phone (force it to 0); `EXC_BAD_ACCESS` at an address with bits
      39-46 set means the keys-off/39-bit-VA pointer-tagging class; `EXC_BREAKPOINT` at a WebKit `RELEASE_ASSERT`
      names the failing check (file/line are in the `brk` operands). Also read xnuhost.log for the `deferred map ...
      prot 7 failed` line. The `IOUC RootDomainUserClient failed MACF in process pid 282, MobileSafari` line is a
      separate sandbox/entitlement denial for the IOPMrootDomain user client; harmless to JIT, noted for the hang.
11. **iOS hangs while the keyboard is up (typing a Wi-Fi password, 2026-11-13 10:2x-10:47, main `c1223f8`).** The
    guest ran on; frames stopped (`30 s: flushes 10`), backboardd's GPU memory grew 783+147 -> 1232+202 MiB in 80 s,
    the host had 672 MiB available, the thermal step was 2 (trapping), Spotlight, kbd, Preferences and backboardd
    were the busiest. Read and changed on `fix-ui-hang-memory` (built and tested on the PC; not on the phone):
    - **The thermal step stacked three P vCPUs on a core** (proven from the code and the log's `0x60`): step 2 lost the
      E cores it means to add, and cpu-pin put six P vCPUs on two cores. Fixed: `0x63`, and no pinning when a kind
      has fewer cores than vCPUs (`runtime/bridges/thermal/README.md`, "Fixed 2026-11-13"). The cores each step allows
      are unchanged.
    - **`oneios-cpu:` divided by the nominal 10 s** (proven from the code): a late report thread (a stall) inflated
      every share [inference: the four busiest added up to 1,440 % of a CPU on eight vCPUs at +1119]. It now divides by
      the measured time and says the seconds.
    - **kernel_task's share is likely mostly idle threads** [inference] (oneios-perf on the PC: `oneios-perf: kernel_task: ... 686% of a vCPU;
      most: idle #4 ...`, 198 of 206 s idle threads): the phone's 340 % is not work until the perf line's `most:`
      names a thread that is not `idle`.
    - **0058's give-back took 131 s in the guest for 205 MiB** (0.57 s of it in xnuhost): every run was one 4 KiB
      page. Hosted 0066 takes long runs and larger batches and splits the time
      (`kernel/patches/oneios-hosted/README.md`, 0066).
    - **The host's memory**: a second `memory:` line says how much of the guest's RAM the host swapped out (zram),
      the host's swap and page tables [guess, not measured: with 672 MiB available, Linux swapping guest pages
      would give the 0.4-0.95 s faults and interrupts of perf at 1174].
    - Guessed, not proven: kbd and Spotlight spin on work this guest cannot do fast (the keyboard's language model
      and Spotlight's queries without the Neural Engine) or on GPU work that waits behind backboardd's; the keyboard
      not responding (fault 12) is the same load. **On the phone next:** the boot arguments and log searches of
      `docs/gpu.md` 5.1 "The next phone boot (hang)".
12. **The keyboard often does not come up or does not answer** (23:45 and 23:15 boots: kbd 24-40 %, Spotlight 96-102 %,
    `5105 HID events queued ... 6 dropped`). Not read past fault 11's load; the same samples and searches decide.
    **2026-11-13, main `c2d281a`: in Settings > Wi-Fi's password field the keys stop after the 3rd or 4th character.**
    Read on branch `fix-keyboard` (iOS's log of the `c1223f8` hang session, the 24A446 images and shared cache, a PC
    boot's Data store and iOS log):
    - **Changed (ours):** backboardd's HID event queue was Apple's 16 KiB default and drops events that do not fit; a
      dropped lift leaves a touch down for good, and a stuck touch plus the next finger is the keyboard's two-finger
      trackpad mode, where keys type nothing [hypothesis, not reproduced]. The touch screen and buttons can now get a
      256 KiB queue with `oneios-hid-queue-kib=256`, off by default until a boot has run it (`kernel/patches/oneios-hosted/hid/README.md`, "backboardd's event queue").
    - **The MobileAsset lock error is not a symlink fault** (read in the cache): `SYMLINK` is MobileAsset's name for
      opening an asset set's lock through its `latest` link under `/private/var/MobileAsset/AssetsV2/locks`;
      `com.apple.linguisticdata` is a UAF auto-asset that is only downloaded from Apple. This Data store has never been
      online (Wi-Fi was being joined) and the PC's store has no `AssetsV2/locks` at all. The keyboard's base data
      (`/System/Library/LinguisticData`, `TextInput`) is in the OS image. The line's `errno:N` decides: 2 is "not
      downloaded" (expected offline); 1, 13 or 62 would be ours (sandbox, permissions, a link loop). No seed change.
    - **`volume does not support data protection` is by design** (sqlite, every process, the PC too): the Data
      volume is mounted without `MNT_CPROTECT` (no Secure Enclave; keybagd says `mount is with NO CPROTECT`), sqlite
      drops its protection flags and opens the store; per-file classes are kept as an attribute (hosted 0022
      `owner-dataprotect`). A real iPhone mounts with `MNT_CPROTECT`. Not changed.
    - **InputUI's arbiter error** is `runOperations: %s Failed to access %@ remote service: %@` (UIKitCore): its XPC
      proxy to the focused client (Preferences) failed. The error after it (connection interrupted, invalidated, or a
      sandbox refusal) decides; not readable from the summary.
    - **PSGInputSuggester** is kbd's ProactiveInputPredictions: the "triggering endpoint" is a local FST language model
      and lexicon, the "serving endpoint" suggestd's `com.apple.proactive.input.suggester` (suggestd is enabled).
    - **AttentionAwareness's `AWDigitizerButtonKeyboardAttentionEvent` is one class for digitizer, button and keyboard
      events** (its `validateMask` in the cache): one event per touch is iOS's design, not our digitizer looking like a
      keyboard. About 900 events times 6 clients is little load.
    - **Disabled jobs the keyboard's processes may reach** (their entitlements' service names against the daemon
      policy): kbd: coreduetd (`coreduetd.knowledge`), aned, assistantd, maild; InputUI: modelmanagerd, modelcatalogd,
      siriknowledged (`com.apple.siri.uaf.service`, one of the two UAF endpoints), assistantd; Spotlight: coreduetd,
      duetexpertd, routined, watchlistd, familycircled. With coreduetd off nobody makes `/var/mobile/Library/CoreDuet`
      (PC: `Non-priveleged client unable to find knowledge directory, calling out for help`, then errno 2). Not changed
      without evidence: the A/B for the next boot is the existing `oneios-enable-jobs=` (hosted 0044).
    - **Next phone boot** (after the `c2d281a` session's lines are read): add `oneios-hid-queue-kib=256` and add
      `Preferences,InputUI` to `oneios-ubt=`; type in the password field and note whether the whole UI or only the keys
      stop. Read: `oneios-hid: touch screen: event queue 256 KiB` (with no `already open`), the state line's
      `N dropped` (0 expected), `input: touch:`; in iOS's log (`oneios-tool ioslog show data.img --boot last ...`):
      `--grep 'Unable to obtain shared lock'` (its `errno:`), `--proc InputUI --grep 'Failed to access'` (the error
      after it), `--grep 'KeyboardInputScene'`, `--proc kbd --grep 'warmUp|ICPredictionManager|timeout'`,
      `--grep '[Tt]rackpad|two.finger|UIKeyboardTrackpad'` (trackpad mode), `--proc backboardd --grep 'TouchEvents|path'`.
      A second boot as the A/B of the disabled jobs: `oneios-enable-jobs=com.apple.analyticsd,com.apple.coreduetd,
      com.apple.siriknowledged,com.apple.modelmanagerd,com.apple.modelcatalogd,com.apple.intelligenceplatformd`
      (the kernel's buffer is 256 characters; analyticsd must stay in the list).
13. **Every widget is blank or white** (23:45 boot) while `WidgetRenderer_Default[174]` has a Metal device (plug-in and
    Turnip loaded, `AGXAccelerator` opened 4 times, `WidgetRenderer_ 28+3 MiB`). **Read, instrumented, not fixed
    (2026-11-13, branch `fix-widgets`).** Read in 24A446, the plug-in and the kernel: the widget renderer hosts the
    widgets' SwiftUI views; RenderBox draws them with Metal in that process into IOSurfaces (or lets Core Animation
    draw), and backboardd composites the surfaces with its own device; each process imports a surface's own pages
    (`CREATE_BLOB_USER`: wired, no copy, its own host resource and udmabufs over the same RAM range), and nothing on the
    path copies, clears or zeroes a surface; the IOSurface mapping XNU makes for a second task shares the object
    (`mach_make_memory_entry_share`). So no fault on our side was found by reading. New lines show where the image is
    lost: the kernel's `oneios-gpu-surfaces` (who writes which IOSurface, whether the write finishes, whether
    backboardd reads it after), `oneios-gpu-pixels` (what the pages of each process's IOSurface imports hold: zero,
    white, flat, drawn; whether backboardd's imports are the widget renderer's pages), `oneios-gpu-locks`,
    `oneios-gpu-sync`; xnuhost's `gpu: KGSL imports ...` (whether KGSL keeps the guest's memory IO-coherent; drm2kgsl
    patch 0007). Guesses, in the order to test them: RenderBox drawing widgets into its own two-plane 4:4:4 surfaces
    that NativeMetal does not render into (Droid-VM's iPad showed that until the plug-in told RenderBox
    `MTLGPUFamilyApple2`), seen as `Unsupported ... 444` in the widget renderer's log with its surfaces `zero`;
    widget content never reaching the renderer (`placeholder` in chronod's and the renderer's log: not the GPU);
    backboardd sampling a widget's surface while it is still being drawn (`read while written`: this plug-in pin
    never asks the kernel's `SurfaceAwaitWriters`; test switch `oneios-gpu-read-wait=1`, off by default). **Left, in
    order:** (1) a whole hosted kernel build with the branch, the prelink (audit 0 failures, stubs check) and a PC boot
    with `oneios-gpu-metal=1` (no panic; the work paused before the link); (2) the Mac's runtime and kernel for the
    phone; (3) **on the phone:** the boot arguments, lines and searches of `docs/gpu.md` 5.1 "The next phone boot
    (widgets)"; (4) the fix where those lines put the loss (the plug-in's RenderBox family answer, chronod, the read
    order, or cache maintenance if KGSL drops coherence).

## Later

- **Rust tools:** `tools/oneios-tool/todo_rust.md` section 7 lists the Python that is left and why; ioslog in
  parallel waits for macos-unifiedlogs to open its oversize-string module.
- The board's L1-L3 items (a direct KGSL path after measuring, `min_runtime` in the image).
- Turnip sub-allocation and batched pipeline-cache writes (a plug-in rebuild with a new trust list).
- The PC's root-device stall with the Bluetooth driver on (`docs/hardware.md` 4).

## How to resume

1. Read this file, then `docs/agents.md`'s last notes. Saved logs (git-ignored): `out/session-logs/2026-11-12/`:
   the xnuhost logs of every session of the day (`combo1`, `diag`, `c2`, `c3`, `c5` = the 15:35 boot), iOS's own
   log of the 14:12 session (`ios3.logarchive`, parsed `ios3-log.csv`), the baseline config and `phone-baseline.txt`.
2. iOS's own log of a session: copy the Data store after the session (read only, no session running), fast:
   `oneios-tool phone pull-data data.img` (`--ioslog` then reads the log out of the copy without a mount).
   **Over wireless adb** the first port's one gzip stream broke twice on 2026-11-13 ("incomplete deflate stream"
   after 1.6 and 0.6 of 32 GiB, about 1.8 MB/s, adb still connected) and began at zero each time. It now copies in
   chunks of 256 MiB (`--chunk`), each one adb stream of a range of the one named file (`busybox dd ... skip=
   count= | busybox gzip -1 -c`), checks each against the phone's SHA-256 of the same range (a chunk of zeros is
   not copied at all), tries a broken chunk again (`--retries 5`), and keeps `data.img.part` and
   `data.img.part.progress`: **when it stops, is killed or gives up, run the same command again and it goes on
   at the first chunk that is not done** (`--restart` begins again; a store that changed since is refused). The
   last line gives the time, the bytes over adb and the bytes written. This copy in chunks **has not run on the
   phone yet** (stand-in test: `tools/oneios-tool/tests/phone/pull-resume.sh`); what to check there is in
   `tools/oneios-tool/README.md` "The copy that goes on". The one stream by hand (no resume, no sums) is
   `adb exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" |
   oneios-tool phone sparse-write data.img --gunzip`. `sparse-write` replaced `gunzip -c | python3 -I
   sparsewrite.py data.img` (the script of 2026-11-12, kept as `tools/oneios-tool/tests/phone/sparsewrite.py`:
   6 min, 2.2 GB of blocks for the 32 GiB store): the same bytes, a quarter of the CPU, and fewer blocks on
   Linux's file systems; its macOS part (APFS) has not run yet
   (`tools/oneios-tool/README.md` "The sparse copy"). The way of 2026-11-12, by hand on the Mac: attach the copy
   read-only
   (`hdiutil attach -readonly -noverify -nobrowse -imagekey diskimage-class=CRawDiskImage -nomount`), mount the
   Data volume `mount -t apfs -o rdonly,nobrowse,noowners`, copy `db/diagnostics/{Persist,Special,Signpost,
   HighVolume,timesync}` and `db/uuidtext/*` into a `.logarchive`, and parse it with Mandiant's macos-unifiedlogs
   (`unifiedlog_iterator -m log-archive -i ARCHIVE -f csv -o OUT`).
3. Build the set from `main` in a clean worktree (`git worktree add --detach out/parent-wt origin/main`, link
   `external/`), on the Mac or the Linux PC (both build the kernel; Rust is a prerequisite: the scripts build
   `oneios-tool` when it is missing): `scripts/bg.sh k flock /tmp/oneios-kbuild.lock env HOSTED_OUT=prelink
   XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`, then `env REFERENCE=out/kernel/current-hosted-prelink/reference.macho
   KERNEL=out/kernel/current-hosted-prelink/xnu.macho kernel/prelink/build.sh` (the audit must show 0 failures;
   `oneios-tool prelink stubs-check` 666 good), `runtime/package.sh VERSION`; on the Mac only, the Metal and camera
   plug-ins (`userland/metal/build.sh`, `userland/camera/build.sh`, with `out/ios27-tbd` linked); and the image
   (`env INPUTS=<main>/inputs/24A446 OUT=out/image scripts/image/build.sh --no-pack PROFILE`) only when the seed or
   the trust list changes (a new seed starts the phone's Data store over).
4. Stage and run through `oneios-tool` (CLAUDE.md's rule; `docs/building.md` sections 6-7): `oneios-tool phone
   stage runtime DIR` / `stage image DIR`, `phone sums`, `app stage-scripts` when the app is not used, then
   `phone switch`, `phone log --follow --save-auto`, `phone return` (or the whole of it: `oneios-tool cycle`).
   The manual forms (`scripts/phone/stage.sh`, `oneios_root.sh switch <versionCode> 1`,
   `touch .../session/return.request`) are the fallback when the CLI fails. On the phone read named files only,
   never a tree walk of sysfs or /proc (one panicked the phone's kernel on 2026-11-12).
