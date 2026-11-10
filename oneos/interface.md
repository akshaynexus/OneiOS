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
  `FAILED at: ...` (gpuctl); `brightctl info`'s `panel: ...` line and `levels`' `restored N`; camctl's
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
| `logs/` | earlier sessions' `xnuhost-<epoch>.log` (the last 5) | return |

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
gone), and feeds Samsung's software watchdog. Gone: zygote, zygote_secondary, system_server and every app. So no InputDispatcher: read
`/dev/input/event*` yourself. Do not `EVIOCGRAB` the volume-key device (see 6), or handle the chord yourself.

## 5. What xnuhost must do for the switch

- **Stay in its mount namespace**, with every process it starts (iOS task processes included). The return
  kills by namespace; a process that leaves it is a failed return and the phone reboots.
- **Write only under `/data/data` and `/data/session`.** Never to anything of One UI's.
- **Ready signal:** create the file `/data/session/ready` when the guest's first frame is on screen
  (or when it would be, while there is no display yet). Until then the switch's dead-man timer runs: no
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
3. Checks that nothing of the session is left and One UI's vold, keystore2, surfaceflinger and SELinux mode
   are as they were at the switch, on the same boot. Then starts One UI's zygote in place.
4. Any failed check: reboot (the failsafe). A reboot is always a correct way back.

The Volume Up + Volume Down chord is read on the host side with `getevent` (which does not grab), so it
works whatever xnuhost is doing, as long as xnuhost does not grab that device.

## 7. Overlay mode

Not part of this interface. The fork only does the takeover session.
