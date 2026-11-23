# Plan: iOS as the main system of a session (ios-first-session, 2026-11-23)

Goal (the owner): while a session runs, iOS is the phone's main system. One UI's Android userspace does not run, so
iOS gets the CPU, the RAM and the GPU as directly as this design allows. The Linux kernel stays the hardware layer.

**Research and design. Nothing here ran on the phone.** Built and tested in this step: stage 1, `oneios-tool phone
procs` (section 6). Built since (2026-11-23, host-tested): the session setting the later stages go behind,
`session-mode=takeover` (section 5a), and in it stages 2a, 2b and 4 (section 5b, takeover-stages; not run on a phone). Marks: **[F]** read in this repository, in the fork's scripts at `43fc1b3`, or in a session log;
**[I]** inference; **[U]** unknown until measured or tried on the phone; **[guess]** a number with no measurement.

## 0. The limits (the owner, 2026-11-22)

- No bypass, no stop and no work-around of TrustZone, Knox, the bootloader lock, the hypervisor, verified boot or any
  other security protection. The Linux kernel is not replaced and not changed.
- Only what root (KernelSU) on the running stock system allows: `setprop ctl.stop|ctl.start` of init services, named
  files of `/proc` and `/sys`, the kernel's own device nodes (`/dev/dri/card0`, `/dev/kgsl-3d0`, evdev, nl80211).
- The way back stays a switch (`oneos/interface.md` 5, 6). A reboot stays the failsafe, never the plan.
- So this plan never touches the key and TEE chain (qseecomd, keymint, gatekeeper, weaver, vaultkeeper, Samsung's TLC
  daemons, keystore2, vold), the service managers, init, ueventd, logd, lmkd, the thermal and charge daemons, or the
  modem's daemons. They stay as One UI left them.

## 1. The inventory: what of Android still runs in a session

### 1.1 What the repository documents [F]

A session stops zygote, zygote_secondary and with them system_server and every app; the stop loop keeps zygote, its
second one and the boot animation stopped (`oneios_switch.sh --stop-loop`). Everything else that init started stays
(`oneos/interface.md` 4, `docs/hardware.md` head, `docs/architecture.md`).

| Process (init service) | Why it runs in a session | Needed by | Direct kernel interface instead | Verdict |
|---|---|---|---|---|
| `surfaceflinger` | holds the layer the display helper posts to | display | DRM/KMS on `/dev/dri/card0` (xnuhost's `drm` back end) | replace: stage 3 |
| the vendor composer (`vendor.qti.hardware.display.composer`) | DRM master: it owns the panel | display | the same | replace: stage 3 |
| `oneios-display` (`app_process`, a Java runtime with the framework's classes; ours, on One UI's side) | copies each frame into surfaceflinger's buffer | display | none needed with KMS | goes with stage 3 |
| the gralloc allocator (`...display.allocator`) | buffers of the camera and the codec | camera, encoder | none | keep |
| `audioserver` | AAudio's server (`media.audio_flinger`, `media.aaudio`) | audio (`oneios-audio`) | ALSA alone cannot route (section 2) | keep |
| the audio HAL (Samsung's `ISecAudioHal`; AudioReach: PAL, AGM, the ADSP graph) | routing, the amplifiers' protection, calibration | audio | no safe one (section 2) | keep |
| `cameraserver` | One UI's camera service; refuses root without system_server | nothing in a session (the helper uses the provider HAL) | - | stop: stage 2 |
| the camera provider HAL | sensors, ISP, 3A | camera (`oneios-camera`, HAL back end) | no safe one | keep (stop only with `camera=off`: later) |
| the Codec2 HAL (`c2.qti.hevc.encoder`) | hardware encoder | encoder (`oneios-encoder`, MediaCodec) | V4L2 `iris` nodes exist [I], the HAL is the proven client | keep |
| `media` (mediaserver), `mediaextractor`, `mediametrics`, `media.tuner` | One UI's media services | [U] whether MediaCodec in the helper needs mediaserver's resource manager | - | extractor, metrics, tuner: stop (stage 2); `media`: after one encoder check |
| `wpa_supplicant` (vendor, lazy) | keys, 4-way handshake, roaming | Wi-Fi joins (AIDL over `/dev/binder`) | none: the chip's handshakes are the supplicant's | keep |
| the Wi-Fi HAL (`android.hardware.wifi-service`), Samsung's Wi-Fi service, `cnss-daemon` [I] | the chip's start and firmware | Wi-Fi (wlan0 exists because of them) | none | keep |
| `wificond` | the framework's nl80211 client | nothing (the bridge scans by nl80211 itself) | nl80211 (already used) | stop: stage 2 |
| `netd` | the framework's routes, DNS, firewall | nothing ("netd ... is not asked for anything", `docs/hardware.md` 3) | AF_PACKET on wlan0 (already used) | stop: stage 2, after one Wi-Fi check |
| the Bluetooth HAL (`vendor.bluetooth-1-1-qti`, HIDL) | power, UART, firmware download | Bluetooth (hwbinder client) | none safe: the HAL owns the UART and the firmware | keep |
| `thermal-engine-v2`, `vendor.samsung.hardware.thermal-service`, the health HAL, the charger | the kernel's limits, the charger's limits | safety | sysfs is already what our bridges read | keep, always |
| the vibrator HAL, the sensors HAL, the lights HAL [I: expected on this vendor image, not read on the phone] | idle | nothing with `hw-backend=samsung` (evdev force feedback, `als_rear` by evdev, backlight sysfs) | already direct | measure; stop only if they cost something |
| `servicemanager`, `hwservicemanager`, `vndservicemanager` | every HAL lookup of our bridges; the way back | Wi-Fi, Bluetooth, audio, camera, encoder | none | keep, always |
| `vold`, `keystore2`, keymint, gatekeeper, qseecomd and the TEE daemons | One UI's storage and keys | the way back (Gate A checks vold's and keystore2's pids) | none | keep, never touched |
| `init`, `ueventd`, `logd`, `lmkd`, `adbd`, KernelSU's daemon, `tombstoned` | the system, adb, root | the way back, debugging | none | keep, always |
| the modem's daemons (rild and the like) | calls, SMS, data | nothing (calls and SMS are never goals) | none | not classified: they hold modem state; do not stop before a restart test beside One UI |
| the fork's watchers: the stop loop, the return watcher, the key readers (`getevent`), the watchdog feeder, the USB watcher | the way back | the way back | - | keep; make the stop loop cheaper (stage 2a) |
| Samsung's and Qualcomm's other daemons | [U]: not listed anywhere in this repository | [U] | [U] | measure first (`phone procs`) |

### 1.2 What the session logs show [F]

Session `out/session-logs/2026-11-22/kbd-debug/xnuhost-s9.log` (6 minutes, boot to the Home Screen and use):

- **CPU.** `thermal: 30 s: host CPU` reads 278 to 375 % of 800. In the same spans the guest's own count
  (`oneios-cpu: 10 s: ... (all N%)`) reads 217 to 343 %. The difference is 30 to 60 % of one CPU. It holds everything
  on the host that is not iOS's own CPU time: xnuhost's device and GPU threads, the display helper's copies,
  surfaceflinger, the composer, the kernel's threads (`irq` alone is 9 to 15 %), the fork's loops, every Android
  daemon. So **Android's userspace costs at most about half a CPU of eight** in this session [I], and probably much
  less. iOS already runs on all eight cores (`cpuset /`, `cpu-pin=1`). The CPU that a stop of Android gives back is
  small. The log cannot split the difference: that is stage 1's command.
- **RAM.** `memory: ram=auto: host MemTotal 11084 MiB, MemAvailable 8618 MiB; the guest gets 5632 MiB` (the image's
  `ram-session-reserve=2G`, written on 2026-11-16 after a kill with the host at about 1 GiB). So with zygote and
  system_server gone, **2,466 MiB of the host's RAM is not available**: the kernel, the zram pool (549 MiB of swap
  used at the start), and every process that stayed. Six minutes later: `host MemAvailable 1763 MiB`, `page tables
  433 MiB`. Outside the guest and the tasks' page tables the session took 8,618 - 5,632 - 433 - 1,763 = 790 MiB
  (KGSL's own memory, the kernel memory of the task processes and threads, the helpers, surfaceflinger's buffers).
- **Display latency.** `display: 10 s: N frames copied by the helper ... (posted to in the layer: mean 11.3 ms, max
  19.2 ms)`: each frame waits about 11 ms for surfaceflinger's buffer before the helper fills it, and surfaceflinger
  then latches it at a later vsync.
- **The fork's stop loop** (`oneios_switch.sh --stop-loop`, read in the script): three `getprop` every 0.2 s, for
  the whole session. That is 15 short processes a second [F]. Their cost is not in any log [U]; `phone procs`
  counts it (the fork's group, with the children it waited for). **Built (stage 2a, section 5b):** one wait on
  init's property area (`xnuhost --prop-wait`), no process while nothing starts.
- Session 4 of 2026-11-12 (`out/phone-baseline/2026-11-12/session4-survey.txt`): the load line's `5/3139`: 3,139
  threads existed on the host. No survey so far lists processes.

### 1.3 What the parent has to measure (read only)

`oneios-tool phone procs` (section 6) gives the missing list. Three captures, each kept as the phone's own lines:

```sh
T=tools/oneios-tool/target/release/oneios-tool; D=out/session-logs/$(date +%F)
$T phone procs --seconds 30 --raw > $D/procs-oneui.raw            # One UI running, the phone idle: the baseline
$T phone procs --seconds 30 --raw > $D/procs-session-idle.raw     # a session, 2 minutes after ready, the Home Screen still
$T phone procs --seconds 30 --raw > $D/procs-session-busy.raw     # the same session while scrolling and typing
$T phone procs --from $D/procs-session-idle.raw --sort mem        # the report: by RssAnon + VmSwap
$T phone procs --from $D/procs-session-busy.raw                   # the report: by CPU
```

What to read in the reports:

1. The line `Android and the vendor`: its `cpu%` is the answer to "what does Android cost in CPU"; its `anon MiB` plus
   about a third of its `swap MiB` is the most RAM a stop of all of it could free.
2. Each process with `role` `restart list; no bridge uses it`: the sum of their `anon` and `cpu%` is stage 2's gain.
3. `surfaceflinger` and the composer (in the Android list) and the `app_process` line under `the session's own
   processes` (the display helper): stage 3's gain.
4. The fork's group `cpu%`: the stop loop's cost.
5. Every process with `role` `-` and more than 20 MiB of `anon` or 0.5 % of a CPU: name it in this file's table.

One more read, of named files (the CLI has no command for it yet; `adb shell su -c 'cat FILE'`): the `.rc` file of
each stop candidate, for the words `critical`, `onrestart` and `oneshot`: `/system/etc/init/cameraserver.rc`,
`mediaserver.rc`, `mediaextractor.rc`, `mediametrics.rc`, `wificond.rc`, `netd.rc`, `surfaceflinger.rc`,
`audioserver.rc`, and the composer's under `/vendor/etc/init/` (its name is in `init.svc.*` of the capture's `@svc`
part). A service marked `critical` is never stopped.

## 2. The replacements, bridge by bridge

| Bridge | Today | Direct interface | Can Android's part go? |
|---|---|---|---|
| GPU | drm2kgsl on `/dev/kgsl-3d0`, guest pages by udmabuf | already direct | nothing of Android's is used |
| Touch, keys, pen | evdev, never grabbed | already direct | nothing used (the input HAL only woke the touch IC: the brightness bridge does that) |
| Backlight, touch IC power | `/sys/class/backlight/panel0-backlight/brightness`, `/sys/class/sec/tsp/enabled` | already direct | nothing used |
| Light sensor | `als_rear` by evdev | already direct | nothing used [I: the sensors HAL is not in the path] |
| Haptics | the CS40L26 force-feedback evdev device (`/dev/input/event5`) | already direct (`hw-backend=samsung`) | the vibrator HAL is not used |
| Battery, thermal | `power_supply` and thermal-zone sysfs | already direct | not used by us; **kept for safety** |
| Display | surfaceflinger layer through `app_process` | **DRM/KMS** | **yes: stage 3** (below) |
| Audio | AAudio: `oneios-audio` -> audioserver -> the audio HAL -> the ADSP | ALSA PCM nodes exist, but see below | **no** |
| Camera | `oneios-camera` -> the provider HAL (session), cameraserver (One UI) | none safe | provider: no; cameraserver: yes |
| Encoder | `oneios-encoder` -> MediaCodec -> the Codec2 HAL | none proven | no ([U] for mediaserver) |
| Wi-Fi | AF_PACKET + nl80211 direct; joins by the vendor supplicant | frames and scans already direct | supplicant and the chip's HAL: no; wificond, netd: yes |
| Bluetooth | the HIDL HAL over `/dev/hwbinder` | none safe | no |

**Audio, why not ALSA [F for the parts list, I for the conclusion].** The phone's audio is Qualcomm AudioReach
(`docs/hardware.md` 6: `pineapple-mtp-snd-card`, `agm`, `arpal-lx`). The sound card's PCM devices are front ends
without a route of their own. The route is a graph that user space builds on the ADSP: the HAL's PAL and AGM
libraries send it from the vendor's calibration files, and the same graph carries the speaker amplifiers'
protection (WSA884x, Cirrus). A tinyalsa client alone gets no sound. A client that links the vendor's PAL and AGM
libraries would be a second audio HAL of ours: it depends on Samsung's private libraries and calibration, breaks
with a vendor update, and a wrong graph can drive the speakers without protection. So the audio HAL stays, and
audioserver with it (AAudio's MMAP exclusive path already has the DSP read our buffer: no mixer runs for us). Cost:
[U], read it in `phone procs`.

**Camera, Bluetooth, Wi-Fi joins, the encoder.** Each HAL holds what only the vendor's code can do on this phone:
the ISP's tuning and 3A, the Bluetooth firmware download and UART, WPA handshakes, the encoder's firmware protocol.
They are idle when iOS does not use them [I]. They stay.

### 2.1 Display: DRM/KMS with surfaceflinger and the composer stopped

xnuhost's `drm` back end (`runtime/xnuhost/src/display.rs`, `docs/booter.md` 2.8; written in C, ported to Rust on 2026-11-23) is written and never run [F]: DRM
master, one dumb buffer, legacy `SETCRTC`, `DIRTYFB` per flush, `DROP_MASTER` at the close. What it needs before it
can replace the surfaceflinger path:

1. **The master.** Only one process is the DRM master: today the composer. `SET_MASTER` fails while the composer
   runs (the code says so in its error line). So the composer must be stopped first, and surfaceflinger with it (it
   is the composer's client and ends with it). Only the fork can do that (`ctl.stop`): xnuhost never signals a
   process that is not its own (`docs/booter.md` 2.9).
2. **A probe before any code [U -> F].** Whether Qualcomm's SDE driver takes a legacy `SETCRTC` of a dumb buffer on
   this panel is not known. Qualcomm's own recovery screen draws with atomic commits and SDE's plane properties
   [I: recalled from Qualcomm's `minui` DRM back end, not checked here]. A 1440-pixel-wide DSC panel can also need
   two layer mixers, so two planes side by side [I]. A new read-only `displayctl drm` (stage 3a) settles it beside
   One UI, with no master: the driver's name and version, `DRM_CAP_DUMB_BUFFER`, the connectors and their modes (is
   1440x3120 at 120 Hz in the list), the CRTCs, every plane with its type, formats and property names, the
   connector's and CRTC's property names.
3. **Atomic commits** [I: expected from 2]: `DRM_CLIENT_CAP_UNIVERSAL_PLANES` and `DRM_CLIENT_CAP_ATOMIC`, one or
   two planes on the panel's CRTC with the standard properties (`FB_ID`, `CRTC_ID`, `SRC_*`, `CRTC_*`) and the SDE
   ones the probe lists.
4. **Two buffers and a flip per frame.** One buffer written while the panel reads it tears. The flip's event is the
   frame's "shown": it answers the guest's held flush (today the helper's `XHDA`), and it gives the panel's real
   vsync time (today `sheng-vsync` is a free timer).
5. **The mode.** Today the helper asks surfaceflinger for the native mode. With KMS xnuhost picks 1440x3120 at
   120 Hz from the connector's list itself (the code takes the `PREFERRED` mode: [U] which one that is while One UI
   is set to FHD+).
6. **Panel power and brightness.** Brightness is already sysfs. iOS's display off becomes a real CRTC off (today: a
   black frame and the backlight at its floor). [U] what Samsung's panel driver does at the next on (whether the
   level must be written again).
7. **A boot picture of xnuhost's own.** The OneOS boot screen is a surfaceflinger layer: it is gone with
   surfaceflinger. xnuhost shows the boot from its first modeset (the boot console's frames exist already). The
   fork's `display_up` rule needs another marker than the helper's `first frame posted` (`session/ready` alone).
8. **Later, no copy at all.** Today the helper copies 24 to 43 % of the rows of a frame while iOS moves [F]. With
   KMS the guest's scanout surface can be the plane's buffer: its pages as a dma-buf (udmabuf, as the GPU path
   already makes them), imported with `PRIME_FD_TO_HANDLE`, `ADDFB2`. [U]: whether SDE's IOMMU maps a udmabuf, and
   whether one frame fits one udmabuf (18 MB, at most 1,024 runs of pages, `docs/gpu.md` 4). surfaceflinger's path
   cannot do this at all (`docs/booter.md` 2.8, "Why not zero copies").
9. Not in the first step: `display-format=rgba16f` (the back end takes 32-bit pixels), HDR.

**The way back from it** [F: read in `oneios_return.sh`; I: that it holds with both stopped]. Every return already
restarts the composer and surfaceflinger and waits for both (`ctl.restart` of the composer, `IComposer/default`
found, a new surfaceflinger pid, `SurfaceFlinger` registered, the boot animation), and each return on the phone so
far did exactly this. The script finds the composer by `init.svc.NAME` being set, not by `running`; a stopped
service is started by `ctl.restart`; a missing surfaceflinger gets `ctl.start`. Its comment already names "with
--display drm, on the panel itself". xnuhost's exit, a SIGKILL included, closes the DRM file: the kernel drops the
master and the buffers. What the fork must add: stop the two (and keep them stopped: the stop loop), start no
helper and no boot screen, when the session's display is `drm`.

## 3. What iOS gains

**RAM.** `ram=auto` gives the guest `(MemAvailable at the start - ram-session-reserve - 256 MiB for the GPU) / 1.08`
in 256 MiB steps (`docs/booter.md` 2.2). Today [F]: (8,618 - 2,048 - 256) / 1.08 = 5,846 -> **5,632 MiB**.
**Since 2026-11-23** the formula counts the tasks on their own (`docs/booter.md` 2.2, "The terms of 2026-11-23"):
`(MemAvailable - 1536 MiB headroom - 256 MiB - 400 tasks * 4 MiB) / 1.07`, 4,608 MiB from 8,561; two sessions with
5,632 MiB ran out of host memory. The paragraphs below are of the formula before that; a MiB freed before xnuhost
starts now raises the guest by 0.93 (1 / 1.07) as before, and the headroom is the measured swap-in floor. Stage 4 as judged for the
takeover (section 5b): the stops free their RAM before xnuhost starts; the headroom stays until stage 3.

- Each MiB that a stopped process frees before xnuhost starts raises `MemAvailable at the start` by one, so the
  guest by 0.93. The size of that is [U] until `phone procs` runs. A bound: 2,466 MiB are not available at the
  start, and the kernel's own share of that (slab, page tables, stacks, the zram pool, KGSL, the dma-buf pools) is
  probably more than half [guess]. Stage 2's services: 60 to 150 MiB [guess]. Stage 3's three processes
  (surfaceflinger, the composer, the `app_process` helper with its Java runtime): 150 to 300 MiB [guess]. Together
  one 256 MiB step for the guest, perhaps two.
- The larger term in the same formula is the headroom. The image's 2 GiB (2026-11-16) leaves 1,763 MiB available
  six minutes into this session. With the processes of sections 1 and 2 gone and a measured list of what the host
  still needs, the headroom can go back toward the default 768 MiB: (8,618 - 768 - 256) / 1.08 = 7,031 ->
  **6,912 MiB**, five steps more. That is a decision for after the measurement, with the 2026-11-16 kill in mind:
  the capture `procs-session-busy.raw` shows who held the host's RAM at such a moment.
- **The host's floor.** `ram-host-floor` (1.5 GiB) is not the binding term today and stays. The real floor of a
  session becomes: the kernel, the kept processes of section 1 (measured), the valve (256 MiB) and its margin
  (256 MiB), the iOS tasks' kernel memory. iOS's RAM stays fixed and locked; nothing here changes that rule.

**CPU.** At most about half a CPU of eight [I, section 1.2], split among the kernel's threads, our own host threads
and Android. Stage 2 and 3 remove surfaceflinger's and the composer's work per frame (at up to 120 Hz), the
helper's copies, and the daemons' wakeups. The stop loop's 15 processes a second go with stage 2a. Expect a few
percent of the phone's CPU and a little less heat, not a faster iOS: iOS's speed is bounded by the trap path
(`docs/booter.md` 2.6a), which this plan does not change.

**GPU.** Nothing of Android's uses the GPU in a session but surfaceflinger's client composition, and one opaque
layer is composed by the display hardware [I]. With stage 3 xnuhost is the GPU's only user.

**Latency.** Stage 3 removes the wait for surfaceflinger's buffer (11 ms on average) and surfaceflinger's own
latch: about one to two frames at 120 Hz [I]. Step 8 of section 2.1 removes the copy.

## 4. The way back

The return's order today [F, `oneios_return.sh` at `43fc1b3`]: SIGTERM to xnuhost (20 s), SIGKILL to the
namespace, the helper and the boot screen ended, the backlight and the clocks written back, Gate A (vold, keystore2,
servicemanager are the same processes; SELinux as it was), the boot counters, **the composer restarted,
surfaceflinger restarted or started, the boot animation**, audioserver stopped and started, cameraserver restarted,
`media`, `mediaextractor`, `mediametrics` started, wificond restarted, `perfmond` and `media.tuner` restarted when
they ran at the switch, netd restarted, zygote started, system_server waited for. Any failed check: the reboot.

That list is zygote's own restart list. **Every service on it is restarted at every return already**, so stopping
it for the session adds no new step to the way back: this is why stage 2 starts with exactly these.

Rules for everything this plan stops:

1. Only `setprop ctl.stop NAME` by the fork's switch, after zygote's stop, never by xnuhost. Nothing persistent is
   written (no `persist.` property, no file of One UI's). A reboot therefore always brings every service back.
2. Before each stop the switch appends the service's name to `control/base/stopped`. The return starts what that
   file names and the list above does not, in reverse order, before zygote.
3. Never a service whose `.rc` says `critical`, never one of section 0's list.
4. The stop loop keeps the stopped services stopped (another service's `onrestart` can start them), as it does
   zygote.
5. A service that does not come back at the return is a failed check: the existing failsafe.
6. A service outside zygote's list is first restarted beside One UI (`ctl.restart NAME`, One UI in use), three
   times. Only when One UI stays whole does a session stop it.

A possible later form for a daemon that costs RAM, is idle, and fails rule 6: do not stop it, **park** it. Move it
to a cgroup of its own, freeze it, and page its memory out to zram (`process_madvise` with `MADV_PAGEOUT`, what One
UI itself does to cached apps). The return thaws it. Not planned now: a frozen binder service blocks its callers,
so each candidate needs the same "nobody calls it" proof as a stop.

## 5. The stages, smallest first

| Stage | What | Risk | Test | Undo |
|---|---|---|---|---|
| **1** (done here) | `oneios-tool phone procs`: the process list with CPU and RAM, read only | none: named files are read, nothing is written | unit tests, the stand-in phone; then the three captures of 1.3 on the phone | - |
| **2a** (built, section 5b) | the fork's stop loop without 15 `getprop` a second: one long-lived wait on the three properties (a small `propwait` in the runtime: `__system_property_wait`, as `usb.c` reads serials), or the same loop at 1 s | a zygote that starts is stopped up to 1 s later | the fork's script tests; `phone procs`: the fork's group at about 0 % | the old loop |
| **2b** (built for `session-mode=takeover`, section 5b) | the switch stops zygote's restart list where no bridge uses it: `cameraserver`, `mediaextractor`, `mediametrics`, `media.tuner`, `perfmond`, `wificond`; then `netd` (after a session's Wi-Fi join without it) and `media` (after one HEIC encode without it) | a kept HAL waits for a stopped service and spins or blocks [U] | stand-in tests of the fork's scripts (the record, the return's starts); a session: `phone procs` before and after, Wi-Fi join, a sound, a photo tile, the return in place | the return starts them; take the name off the list |
| **3a** | `displayctl drm`: the read-only probe of 2.1 step 2, beside One UI | none: no master, nothing set | host test against a stand-in device; one run on the phone | - |
| **3b** | the fork's part alone: a session with `display=none` in which the switch stops surfaceflinger and the composer, 60 s, automatic return | the panel stays dark for the session (the dead-man timer and the chord return); the composer or surfaceflinger does not come back: the failsafe reboot | the fork's script tests; one session: `return.log` shows the composer and surfaceflinger started, One UI's screen back | `display=shm` (today's path) stays the default |
| **3c** | xnuhost's KMS back end as the probe says (atomic, two buffers, flips), `display=drm` | a dark or wrong picture: return by the chord or the timer; nothing persists | host tests against a stand-in DRM device (vkms in the Linux container [U]); a session: the Apple logo, touch to frame, `display:` lines | `display=shm` |
| **3d** | panel power at display off, the real vsync, then the import of the guest's surface (no copy) | as 3c | as 3c, the `display: 10 s` line with 0 rows copied | the copy path |
| **4** (judged, section 5b: no change of the headroom until stage 3) | the RAM that is now free: `ram-session-reserve` from the measured floor; any HAL that `phone procs` shows large and idle (rule 6, or park) | the host runs short: the valve, then the OOM kill of xnuhost, then the return (existing) | a long session with `memory:` lines; `phone procs` at the tightest moment | the image's `xnuhost.conf` |

Stage 2 and stage 3b need changes in the fork (`akshaynexus/OneOS`, branch `oneios`) and then lines in
`oneos/interface.md` (a session setting the switch reads, e.g. the image's `xnuhost.conf` `display=` and a stop
list). Nothing of that ABI is changed by this step.

## 5a. The vehicle: `session-mode=takeover`, the opt-in Android takeover (2026-11-23)

The owner (2026-11-23): "port this as an option we can use ... as we need to get better speeds later it would be part
of the iOS hosted session / Android takeover option we have". Built (port-opt-in-takeover-session, host-tested only,
**not run on a phone**): the session setting the stages above asked for. `oneos/interface.md` 4a has the contract,
`docs/booter.md` 2.9d xnuhost's side, the fork's `docs/oneios.md` "Android takeover" its side.

- `session-mode=hosted` (the default) is today's session and stays the one stages 2-4 compare against: nothing of the
  stages runs in it.
- `session-mode=takeover` (the app's box on the Switch sheet, or the image's `xnuhost.conf`; the host table's S24 Ultra
  and S26 Ultra rows only) is where each stage goes in, behind the same switch. It holds what YMGPwcca/OneOS's S26
  experiment had that fits the way-back rules: iOS gets the touchscreen alone (`EVIOCGRAB` of `sec_touchscreen`, never a
  key), and a CPU cap of the switch's own, put back by the return (since takeover-stages: from the cell at 50 C only,
  section 5b). Since takeover-stages (2026-11-23) also stage 2b's stops (section 5b). Its way back is section 4's: the
  chord and the in-place return first, the restart only when the in-place return cannot put One UI back.
- Next in this mode, in the order of section 5: **3a** (`displayctl drm`, the read-only probe, beside One UI), then
  **3b/3c** (`display=drm`: surfaceflinger and the composer stopped) only with `session-mode=takeover`, then the
  headroom of stage 4 from a takeover session's own `memory:` lines. Each stage adds its line to the table of
  `oneos/interface.md` 4a ("What the mode changes, and how each is put back") in the same commit.
- Not taken from the experiment, and why: section 0 and `docs/s26.md` (a restart as the only way back, the fixed
  deadline, lowered OOM scores, the 40 % cap from the start, the S26-only watchdog path).

## 5b. Stages 2a, 2b and 4 as built (takeover-stages, 2026-11-23; host-tested, not run on a phone)

The owner (2026-11-23 20:30): iOS is far too slow in the hosted session; build the plan's stages into the takeover.
What the logs of that day say (`out/session-logs/2026-11-23-mac-*`, all hosted): `thermal: 30 s: host CPU` 640-735 %
of 800 for the whole session, xnuhost 260-430 % of it; the SoC at 92-95 C (the kernel's trips), the case 44-50.1 C, the
cell up to 43.6 C; `ram=auto` 4,608 MiB from MemAvailable 8,499 MiB (s2023, the formula of 2026-11-23), the host's
lowest MemAvailable in that session 2,257 MiB; the owner's `phone procs`: "Android and the vendor" 150 processes,
about 740 MiB swapped. No `phone procs` capture is in `out/` on the build laptop, so the stops' own sizes are not
known yet [U]: the first takeover session measures them (below).

**2a, both modes.** `oneios_switch.sh --stop-loop XNUHOST` (the switch passes the staged xnuhost) runs `XNUHOST
--prop-wait 60 init.svc.zygote=running init.svc.zygote_secondary=running init.svc.bootanim=running [init.svc.NAME=running
for each of control/base/stopped]` (`runtime/xnuhost/src/propwait.rs`, `docs/booter.md` 2.9d): a futex wait on init's
property area, no process while nothing starts, woken by any property change for one look at its names. A name that
runs is stopped at once (it was up to 0.2 s), and looked at again 0.2 s later. A runtime without `--prop-wait`
answers 2 and the loop looks once a second (3 `getprop` a second instead of 15). `control/stop-loop.log` has a line for
each stop; the return kills the wait by its pid file so the loop ends at once, and logs `stop loop: N stops of a
service that ran again; keeps ... stopped: waits on ...`.

**2b, takeover only.** After zygote's stop, system_server's end and the cap's start, before the stop loop and
xnuhost, the switch's `takeover_stops` stops `takeover-stops=` of the image's `xnuhost.conf` (default `cameraserver
mediaextractor mediametrics media.tuner perfmond wificond`; `netd` and `media` only when named, as section 5's table
says: a Wi-Fi join and an encode without them are not yet checked; `none`). Rule 3 is checked on the phone: a service
whose `.rc` (of `/system`, `/system_ext`, `/product`, `/vendor`, `/odm`, `/apex/*/etc`) has `critical`, or that no
`.rc` names, is not stopped; nor one whose `init.svc` is not `running`. Rule 2: each name is written to
`control/base/stopped` before its `ctl.stop`. Rule 4: the stop loop keeps them stopped. Rule 5: the return starts
zygote's restart list as before, then each name of the file once more if it does not run, waits up to 10 s each, and
fails safe (the restart) before One UI's framework starts if one does not run; a stopped `netd` gets `ctl.start`
(init runs no `onrestart` for a stopped service) and zygote is started at once. Rule 6 does not apply: every name is
on zygote's list. The switch's line: `Android takeover: stopped NAMES (from ...; control/base/stopped)[; not stopped:
NAME(why) ...]; host MemAvailable A -> B MiB`; the return's: `Android takeover: its stops run again: NAMES`.

**4, judged.** The stops come before xnuhost and the switch waits (5 s at most) for each to end, so their RAM is in
`MemAvailable` when `ram=auto` reads it: 0.93 MiB of guest per MiB, through the formula on `main` (not forked: the
RAM helpers keep changing its task terms). The headroom is not lowered for the mode: 1,536 MiB is the measured
swap-in floor of 2026-11-23 (heavy swap-in at 1,077-1,265 MiB available, none from 1,503 MiB up), and that working set
(surfaceflinger, the composer, the display helper, xnuhost's code, the page cache) is the same in a takeover until
stage 3; the stopped daemons are idle. `docs/booter.md` 2.9d, "The RAM in `takeover`".

**The CPU cap, re-judged.** The first cap (the case at 46/48/50 C, the cell at 46/47/48 C) would have cut the clocks at
once in the sessions above and ended them at the case's 50 C. Hosted has no cap of ours either and runs on the
kernel's trips; the case is the skin, the SoC has its junction trips. The cap now acts on the cell only: 80 % at
50 C, 60 % at 52 C, a return at 55 C, above any hosted reading and under a Li-ion cell's usual 60 C [a guess];
`oneos/interface.md` 4a.

**Tests** (the build laptop, 2026-11-23): xnuhost `cargo test -p xnuhost` (`propwait::tests`, 6;
`config::tests::session_mode` with `takeover-stops=`); the fork's `OneiosScriptTest` (the stop loop's wait and its
fallback, the stops and their restore, a service that does not come back, the stop list's words, the cap on the
cell, the hosted session unchanged). **Not tested:** anything on a phone: bionic's `__system_property_wait` under
KernelSU, the `.rc` files' places on One UI 8, whether a kept HAL calls a stopped service and waits [U], the RAM the
stops free, the CPU they cost (the fork's group in `phone procs`).

**The first takeover session (the Mac).** Before: `oneios-tool phone procs --seconds 30 --raw > procs-oneui.raw`
(One UI idle). In the session, 2 minutes after ready: `phone procs --seconds 30 --raw > procs-takeover-idle.raw`, then
`phone procs --from procs-takeover-idle.raw --sort mem`: the stopped names must be absent, the fork's group about 0 %
CPU. The lines: the switch's `Android takeover: stopped ...; host MemAvailable A -> B MiB` (`/data/local/tmp/oneios/switch.log`,
or `oneios-tool cycle`'s `collect`), xnuhost's `memory: ram=auto: ... MemAvailable B' MiB; the guest gets N MiB` (compare with
8,499 -> 4,608 of s2023), `thermal: 30 s: host CPU`, `control/cpu-cap.log`, and after the return `return.log`'s `stop
loop: ...` and `Android takeover: its stops run again: ...`, then `getprop init.svc.NAME` = `running` for each name of
`control/base/stopped`.

**3b-3d: not built.** The exact next step is 3a, read only, beside One UI: `displayctl drm` (section 2.1 step 2: the
driver's name and version, `DRM_CAP_DUMB_BUFFER`, connectors and modes, CRTCs, planes with types, formats and property
names), one run on the phone, its output in this file. Then 3b, the fork's part alone: with `session-mode=takeover`
and the image's `display=none`, the switch adds `surfaceflinger` and the composer (`init.svc` of its `COMPOSER_SVC`)
to `control/base/stopped` after the boot screen ends, starts no display helper and no boot screen, and the return's
existing composer and surfaceflinger restarts (Gate B) put them back; one 60 s session with the automatic return,
`return.log` showing both started. 3c (xnuhost's KMS back end, atomic, two buffers) only after 3a's output.

**The RAM, takeover-ram (2026-11-23; the owner: "at least 10 GB of RAM for iOS").** The full judgement and the
numbers are `docs/booter.md` 2.9d, "More RAM in the takeover": 10 GB locked does not fit on this phone (it needs 12.5
GiB of MemAvailable with today's terms, 10.5 GiB with every term cut hard; MemTotal is 10.8 GiB); the largest locked
guest is 5,376 MiB today and about 6.4 GiB with every way below [estimate]. Built for the takeover, behind switches:
`takeover-reclaim=1` (on: the caches dropped and zram compacted after the stops, its line measures the MiB),
`takeover-swappiness=N` (off: vm.swappiness for the session, put back by the return; the lever for a lower headroom),
xnuhost's `memory: the host at the start ...` and `... at its lowest MemAvailable ...` lines (every kernel part and the
processes by name: they give stage 2's next stops and stage 3's gain: surfaceflinger's, the composer's and the display
helper's anonymous memory), and `ram-ondemand=1` (option C, off: a guest above what fits, backed on demand and never
swapped while it fits, for the owner to choose). Stage 3 stays as above: 3a first.

## 6. Stage 1 as built: `oneios-tool phone procs`

`oneios-tool phone procs [--seconds 5] [--sort cpu|mem] [--top N] [--kernel] [--raw] [--from FILE]`
(`tools/oneios-tool/crates/oneios-phone/src/procs.rs`, `tools/oneios-tool/README.md` "phone"). Read only, with or
without a session, root needed (the phone hides other users' `/proc` entries from the shell user).

- It reads, by name: `/proc/stat` and every `/proc/PID/stat` twice, `--seconds` apart (the pids from `ps -A`); then
  six keys of every `/proc/PID/status`, the command lines (`ps`), init's services (`getprop`), `/proc/meminfo`, and
  the stage's `session/xnuhost.pid` and `control/display.pid`. No glob, no walk, no `smaps` (a page-table walk under
  the process's mmap lock would stall iOS's tasks).
- CPU share: a process's ticks (its own and those of the children it waited for) over the ticks one CPU had in the
  span. The host line is xnuhost's `thermal:` figure, so the two can be compared.
- Groups: the session (xnuhost and what it started, the display helper), the fork's watchers, adb, the kernel's
  threads, Android and the vendor. Each Android process: `cpu%`, `anon`, `swap`, `rss`, threads, its init service
  (`NAME`, or `NAME+` for a child), the command, and `role`: section 1.1's table as far as the names are known. `-`
  means not classified, never "safe to stop". Then the session's own processes one by one (xnuhost, the task
  factory, the helpers, the display helper) with iOS's task processes as one line, and the busy kernel threads.
- Tested on the Mac, 2026-11-23: 7 unit tests (`cargo test -p oneios-phone procs`: the script's words, the parser,
  the groups, the report's figures, a shell loop's ended commands, the roles) and 38 checks on the stand-in phone
  (`tests/phone/procs.sh` from `run.sh`: a made-up `/proc`, `ps`, `sleep` and `getprop`; the whole suite: 265 ok, ALL
  OK, `hw-backend.sh` skipped there as before). **Not run on the phone**: toybox's `ps -A -w -o PID=,ARGS=` and
  `grep -H -E`, and `init.svc_debug_pid.*` being readable, are assumed, as `phone cpu` assumes its `ps` fields. If the
  phone's `ps` refuses `ARGS`, the report still comes, with the 15-character names of `/proc/PID/stat`.

## 7. Open questions

- [U] The whole process list of a session, and each cost: section 1.3.
- [U] Which daemon, if any, spins or waits in a loop because system_server is gone.
- [U] SDE's KMS interface on this panel: stage 3a.
- [U] Whether MediaCodec in `oneios-encoder` needs mediaserver; whether a kept HAL calls netd. (Stage 2b stops neither by default; `takeover-stops=...,netd,media`
  tries it in one session.)
- [U] Whether a kept HAL calls a stopped service of stage 2b and waits for it; what each stop frees (`phone procs` in
  the first takeover session, section 5b).
- [U] What killed xnuhost on 2026-11-16 (lmkd or the kernel's OOM killer): `phone procs` in a tight moment, and
  `dmesg`'s kill line after such a return.
