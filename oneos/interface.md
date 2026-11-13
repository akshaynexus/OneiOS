# What the OneOS fork expects (session ABI 1)

Owner: oneos. The fork is `akshaynexus/OneOS`, branch `oneios` (`/home/akshay/Documents/GitHub/OneOS`).
Its side of this is `docs/oneios.md`, `switcher/assets/oneios_switch.sh`, `switcher/assets/oneios_return.sh`
and `app-android/app/src/full/assets/root/oneios_root.sh` there.

Anything below that the booter, release-kernel or image-build cannot meet: change it on the board
(`docs/agents.md`, Interfaces) and the fork follows. The fork reads only what is written here.

## 1. Runtime package (release-kernel packs it, booter's xnuhost is in it)

- Release asset of `akshaynexus/OneiOS`: `oneios-runtime-<version>-arm64.tar.zst`, and next to it
  `oneios-runtime-<version>-arm64.tar.zst.sha256` holding one `sha256sum` line (`<64 hex>  <file name>`).
  `<version>` is `[0-9A-Za-z][0-9A-Za-z._-]*`. The app takes the latest release, or a pinned tag.
  The app can also import the same `.tar.zst` from a file ("I have the file"), for builds before a release.
- zstd frame, window at most 2^27 (`zstd -19 --long=27` is fine), at most 512 MiB packed, 2 GiB unpacked.
- Plain ustar/pax/GNU tar, regular files and directories only (no links, no devices), relative paths without
  `..`, either at the top level or under one top folder. Paths match `[A-Za-z0-9_][A-Za-z0-9._+-]*` per segment.
- `runtime.json` at the top (first member if possible: the app then needs one pass):

```json
{
  "format": 1,
  "name": "oneios-runtime",
  "version": "0.1.0",
  "arch": "arm64",
  "session_abi": 1,
  "image_formats": [1],
  "xnuhost": "bin/xnuhost",
  "files": [
    { "path": "bin/xnuhost", "size": 1234567, "sha256": "<64 hex>", "mode": "0755" },
    { "path": "bin/stub", "size": 4096, "sha256": "<64 hex>", "mode": "0755" }
  ]
}
```

  `files` lists every file of the package except `runtime.json` itself. `mode` is `0755` or `0644`
  (default `0644`). A file not in the list, a listed file missing, a wrong size or SHA-256: the app refuses
  the whole package. `xnuhost` must be one of `files` with mode `0755`. `image_formats` are the image-set
  `format` values this runtime boots; the switch refuses an image set whose format is not listed.
- Only OneiOS's own code. Nothing of Apple's: the kernel collection with Apple's kexts is part of the image set.

### Phone checks in the package

`bin/gpuctl`, `bin/bridgectl`, `bin/brightctl`, `bin/displayctl` and `bin/camctl` are tools the fork's Hardware
checks screen runs as root beside One UI, never in a session (`oneios_root.sh check <id>`; the fork's
`docs/oneios.md`, "Hardware checks", has the command and the time limit of each). What the fork relies on, so
change these only together with the fork's `OneiosChecks.kt`:

- the file names above, and `bin/oneios-audio` as `bridgectl audio`'s argument, `bin/oneios-display` beside
  `displayctl`;
- exit code 0 for a pass; and the last line `ok` (bridgectl, gpuctl), `displayctl: ok`, `brightctl: ok`;
  `FAILED at: ...` (gpuctl; run with no argument it now also does `routes` and `large`, which add lines before the
  last one, `PASS`/`FAIL` per memory route among them, and pin about 100 MiB for a few seconds: same exit code and
  last line); `brightctl info`'s `panel: ...` line and `levels`' `restored N`; camctl's
  `back end: NAME, N cameras`, `frames: N read, M missed`, `picture: ...`, `wrote FILE.png`, and the failure
  lines of its README's table;
- `displayctl` makes its folder under `$TMPDIR`; `camctl --out FILE.png`;
- SIGTERM ends a tool (brightctl and displayctl put back what they changed on it). At its time limit the fork
  sends SIGTERM, 2 s later SIGKILL, and then writes the backlight level and the light sensor's `enable` back itself.

A tool the package lacks is shown as "Not in this runtime", not as a failure.

## 2. Image set (image-build)

- One `.tar.zst` the owner builds locally and imports in the app. Never downloaded. Same tar rules as above,
  window at most 2^27 (`zstd -T0 -19 --long=27`: the app decompresses on the phone and keeps the window in
  memory), any size that fits the phone; one file at most 16 GiB.
- `manifest.json` at the top (first member if possible):

```json
{
  "format": 1,
  "name": "oneios-image",
  "os": "iOS",
  "version": "27.0.1",
  "build": "24A...",
  "device": "iPhone18,2",
  "files": [
    { "path": "os.img", "size": 9876543210, "sha256": "<64 hex>" }
  ]
}
```

  `files` lists every file except `manifest.json`. The app checks `format`, every size and SHA-256 while it
  unpacks; root checks each file again as it writes it. Sub-folders are allowed (`overlay/...`).
  Keys the app does not know are ignored, so image-build may add its own.
- If `docs/image-format.md` says otherwise for any of these keys, tell oneos: the verifier follows that file.

## 3. Stage on the phone

`/data/local/tmp/oneios`, root:root, mode 0700. Separate from OneOS's Android stage (`/data/local/tmp/oneos`).

| Path | What | Written by |
|---|---|---|
| `bin/oneios_root.sh` | the app's root script (pinned by SHA-256) | app |
| `oneios_switch.sh`, `oneios_return.sh` | session scripts (pinned) | app |
| `runtime/` | the unpacked runtime package, `runtime.json` included; read-only in a session | app, as root |
| `runtime.sha256` | `sha256sum` list of `runtime/` | root at staging |
| `image/` | the unpacked image set, `manifest.json` included; read-only in a session | app, as root |
| `image.sha256`, `image.ids` | `sha256sum` list of `image/`; `dev:inode:size:mtime` of each file | root at staging |
| `data/` | the guest's writable state (Data store, anything xnuhost keeps). Survives a new runtime or image | xnuhost |
| `session/` | this session: `xnuhost.pid`, `xnuhost.log`, `ready`, `return.request`, `tmp/`, the baseline | switch, xnuhost |
| `logs/` | earlier sessions' `xnuhost-<epoch>.log` (the last 5; the next switch moves `session/xnuhost.log` there, so the epoch is that switch's time and the file's own time is its session's last line) | switch |
| `switch.log`, `return.log`, `return.outcome`, `control/display.log`, `control/watchdog_feeder.log`, `control/usb.log` | the fork's own logs: the switch, the return with the watcher and the key readers, how the last return ended, the display helper's output, the watchdog feeder, the session's USB watcher (section 6a) | switch, return |

## 4. How the switch starts xnuhost

After its preflight and the stop of One UI's zygote, `oneios_switch.sh` runs, as root (KernelSU's domain,
SELinux stays as One UI had it, usually Enforcing), detached, in a **new mount namespace**:

- The stage is bound over `/data`, so inside the namespace `/data` **is** the stage and nothing else of
  One UI's `/data` can be seen (no app data, no lock-screen or key files). `/data/runtime` and `/data/image`
  are read-only binds. `/system`, `/vendor`, `/apex`, `/dev`, `/proc`, `/sys` are the phone's own.
- Working directory `/data`. stdin `/dev/null`. stdout and stderr appended to `/data/session/xnuhost.log`
  (on the phone: `/data/local/tmp/oneios/session/xnuhost.log`).
- Command line (fixed for ABI 1):

```sh
/data/runtime/bin/xnuhost --session-abi 1 --mode takeover \
    --runtime /data/runtime --image /data/image --data /data/data --session /data/session
```

  The same values are in the environment: `ONEIOS_SESSION_ABI`, `ONEIOS_MODE`, `ONEIOS_RUNTIME`,
  `ONEIOS_IMAGE`, `ONEIOS_DATA`, `ONEIOS_SESSION`, and `TMPDIR=/data/session/tmp`.
  The binary named by `runtime.json`'s `xnuhost` is what runs (the path above is the usual one).
- Must be runnable there: static, or linked only against `/system` and `/apex` libraries.

What is running around it: One UI's kernel, init, vendor HALs, **surfaceflinger and the vendor composer**
(left running for a root SurfaceControl layer, plan section 5.5), audioserver, netd, vold, keystore2, the
radio. The switch holds the kernel wakelock `oneios_session` for the whole session (One UI's power manager is
gone), and feeds Samsung's software watchdog. It saves One UI's clock caps (each cpufreq policy's
`scaling_min_freq`/`scaling_max_freq`, `/sys/kernel/gpu/gpu_max_clock`, KGSL's `max_gpuclk`) and, once system_server
is gone, writes the hardware's range; the return writes One UI's values back before zygote (`docs/hardware.md` 7d). Gone: zygote, zygote_secondary, system_server and every app. So no InputDispatcher: read
`/dev/input/event*` yourself. Do not `EVIOCGRAB` the volume-key device (see 6), or handle the chord yourself.

## 5. What xnuhost must do for the switch

- **Stay in its mount namespace**, with every process it starts (iOS task processes included). The return
  kills by namespace; a process that leaves it is a failed return and the phone reboots.
- **Write only under `/data/data` and `/data/session`.** Never to anything of One UI's.
- **Ready signal:** create the file `/data/session/ready` when the guest's first frame is on screen
  (or when it would be, while there is no display yet). With the guest's framebuffer driver
  (`kernel/patches/oneios-hosted/display/`) that is `backboardd`'s first frame; the kernel console, which the
  driver also shows, does not count. Until then the switch's dead-man timer runs: no
  `ready` after `ONEIOS_RETURN_MAX` seconds (600) means an automatic return to One UI.
- **Exit means return.** When the xnuhost process ends for any reason, the watcher returns to One UI.
  Exit code 0 = the guest shut down; anything else is logged as a failure.
- **A guest restart is not an exit.** When the guest restarts itself (iOS's own restart, or the kernel starting the
  Data volume over for a new image set's seed), xnuhost starts again in place: same pid, cgroup and namespace
  (`exec` of itself), so the watcher sees nothing and `ready` stays as it was. Only a guest that restarts 8 times in
  a row without getting ready ends xnuhost, with exit code 3, which is a return like any other exit. Nothing for
  the fork to do.
- **Return asked from inside** (a guest app, a shutdown): create `/data/session/return.request`
  (content free, one line is logged). The watcher sees it within a second.

## 6. Stop and return

`oneios_return.sh` (run by the watcher, by Volume Up + Volume Down held 3 s, or by hand):

1. `SIGTERM` to the xnuhost pid. xnuhost should flush `/data/data` and end its tasks; it has **20 s**.
2. `SIGKILL` to every process still in the namespace.
   **Asked of the fork, not in it yet (radio2, 2026-11-12):** after this step and before zygote, when
   `session/wifi.state` exists (xnuhost was killed before its own stop), run `runtime/bin/wifictl restore
   /data/local/tmp/oneios/session/wifi.state`: it puts wlan0's GRO and `disable_ipv6` back and removes the file.
   Without it nothing breaks (`docs/hardware.md` section 3, "Return": IPv6 off is how One UI's framework itself
   leaves wlan0 between networks, GRO off only costs receive speed); a clean stop of xnuhost does it itself.
3. Checks that nothing of the session is left and One UI's vold, keystore2, surfaceflinger and SELinux mode
   are as they were at the switch, on the same boot. Then starts One UI's zygote in place.
4. Any failed check: reboot (the failsafe). A reboot is always a correct way back.

The Volume Up + Volume Down chord is read on the host side with `getevent` (which does not grab), so it
works whatever xnuhost is doing, as long as xnuhost does not grab that device.

## 6a. USB and adb during a session

**The rule: adb over USB works for the whole session, across unplug and replug, and the return leaves the USB
gadget as One UI had it.** It is the fork's to keep (fork branch `fix-usb-adb`, akshaynexus/OneOS PR #1, **not merged yet**, 2026-11-13; its `docs/oneios.md`, "USB and
adb during a session"); xnuhost only reads and logs it (`docs/booter.md` 2.9a).

**Today the fork only records and logs (the repairs are off by default):** until a phone session has shown them right,
the USB watcher and the return write nothing and log each repair they would make. A session turns them on with the
file `usb-repair` in the stage at the switch (`su -c 'touch /data/local/tmp/oneios/usb-repair'`; recorded as
`repair=1` in `control/base/usb`). So by default a session is as before for USB.

Why the fork has to: the USB gadget's functions are set by One UI's framework (UsbDeviceManager in system_server,
through the USB gadget HAL or init's `sys.usb.config` triggers of `init.usb.configfs.rc`), on every plug-in and
unplug. The switch stops system_server, so from then on nobody re-arms the gadget: whatever unbinds it or drops
adb from it during the session stays. adbd and the UDC binding themselves outlive the framework (this morning's
session, 2026-11-13 at c1223f8, kept USB adb from before the switch to after the return with the cable in).

What the fork does (`oneios_switch.sh`, `oneios_return.sh`):

1. **At the switch's baseline** (One UI whole) it records `control/base/usb`: `sys.usb.config`, `sys.usb.state`,
   `persist.sys.usb.config`, `sys.usb.controller`, `sys.usb.configfs`, `init.svc.adbd`, the gadget's UDC file
   (`/config/usb_gadget/g1/UDC`) and whether USB debugging is on (adb in `sys.usb.config` or
   `persist.sys.usb.config`, or `settings get global adb_enabled` = 1). `switch.log`: `USB of One UI: ...`.
2. **After the framework's stop**, with USB debugging on, a USB watcher (`oneios_switch.sh --usb-watch`, a shell loop
   in init's namespace and the root cgroup, never in xnuhost's; not a realtime thread; `control/usb.log`) reads the
   gadget every 3 s and logs it when it changed, and makes at most one repair every 21 s, each only back to One UI's
   own value: adbd not running (`ctl.start adbd`); adb gone from `sys.usb.config` (One UI's config again); the UDC
   file empty while One UI had it bound (One UI's controller written into it); a cable in (`/sys/class/power_supply/usb/online`
   = 1) for 15 s with `/sys/class/udc/<sys.usb.controller>/state` still `not attached`, `attached` or `powered`
   (`sys.usb.config none`, then One UI's, as OneOS's adb watchdog of the Android sessions does; twice per plug-in at
   most, so a charger that never enumerates is left alone). With USB debugging off it does nothing.
3. **At the return** the watcher ends in step 1 (`control/usb-watch.off`, then its pid; it also ends on the state
   `returning`), and in step 4, before One UI's zygote starts, `sys.usb.config` (with its triggers), adbd and the UDC
   file go back to the record where they differ (`return.log`: `usb: before the restore: ...`, `usb: after the
   restore: ...`). Logged only; never a reason for the failsafe.

Only properties and named files: the UDC by the name in `sys.usb.controller`, never a listing of `/sys/class/udc`,
the gadget or `/sys/class/power_supply`. Nothing persist is written, nothing under `/sys/power`, no reboot, no input
device. The stand-in phone checks all three (`tools/oneios-tool/tests/phone/usb.sh`, from `run.sh`, against the
fork's own scripts; skipped, with a line, against a fork checkout that has no `--usb-watch` yet). **Not run on the phone yet.**

What was found about the fault of 2026-11-13 (the Mac saw no USB device during the session at c2d281a): see the fork's
PR and `docs/oneios.md` there. In short: nothing of xnuhost or the bridges opens anything of USB, the gadget,
configfs, Type-C or a power supply but read-only `power_supply` files, and sets no property (the sources at c2d281a;
the diff c1223f8..c2d281a has no such change: 0062 is the guest's own IOKit trace); the fork's scripts touched no USB
property or file before this change; and the Mac did not see the phone on USB **before** the switch either (that
session was staged over wireless adb for that reason), with One UI's framework still running. So that session's
missing USB was not caused by the switch; the cable, the Mac's port, or a gadget state One UI already had are left
(a guess until the lines asked for in the PR come back).

## 7. Overlay mode

Not part of this interface. The fork only does the takeover session.

## 8. Log lines the fork reads (Session logs, fork 0.4.76)

The fork's **Session logs** screen (its `docs/oneios.md`, "Session logs"; `OneiosLogs.kt`, `oneios_root.sh logs` and
`logfile`, both read only) shows `session/xnuhost.log` and `logs/xnuhost-*.log` and puts a summary above each: how
far the boot got, the kernel panic, the processes that died, how the session ended. Its *Collect logs* takes the
same files, redacted. It reads these lines and nothing else, so change one only together with the fork's
`OneiosLogs.kt` and `OneiosLogsTest`. A line it does not find is simply not in the summary.

- **The framing** of `runtime/xnuhost/src/log.c`: `[seconds.millis] xnuhost I|E|D text`, `[seconds.millis] guest
  text` (the tag is `guest` padded to nine characters), and `xnuhost !! text 0x<16 hex> 0x<16 hex>` with no time.
  Its chips are E (`xnuhost E`), !! (`xnuhost !!`), guest, and the word `panic`.
- **Stages**, in this order, the furthest one seen being "how far the boot got":
  `xnuhost I xnuhost: session ABI` (also the start of a new boot after a restart in place);
  `xnuhost I cpu0: entering the kernel at`; console `Darwin Kernel Version`; console `BSD root: `;
  console `load_init_program: attempting to load`; console `com.apple.xpc.launchd|`; the first console line with
  `backboardd[`; the first with `SpringBoard[` or `crashed: SpringBoard.`; `xnuhost I guest ready: first frame`.
- **Panic:** console `panic(cpu N caller 0x...): TEXT`.
- **Processes that died:** console `sheng-exit: NAME[PID] status 0xS reason ns N code 0xC` (the signal is S's low
  7 bits), `Process[PID] crashed: NAME.`, `[NAME: killed] exiting with signal N`.
- **The end:** `xnuhost I stopping: WHY (reason R, code C)` with WHY one of `signal N`, `the guest halted on cpuN`,
  `PSCI SYSTEM_OFF`, `PSCI SYSTEM_RESET`, `kernel fault` (any other WHY is shown as it is) and R one of
  `XH_HALT_*`; `xnuhost I exit N` with the exit codes of `docs/booter.md`; `xnuhost I the guest asked for a
  restart: starting again`; `xnuhost E the guest restarted N times without getting ready`;
  `xnuhost !! fault inside a host call: signal, address` and `fault on a host thread: signal, address` (the first
  value is the signal); `xnuhost !! kernel fault with no exception upcall`.

An exit with code 2 (setup) has no line of its own: the fork shows the last `xnuhost E` line of a log that never
entered the kernel. A line that said so (`exit 2`) would be better, and the fork would read it as it is.
