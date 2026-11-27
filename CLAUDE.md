# Working on OneiOS

Goal: iOS 27 (iPhone18,2, 27.0.1 `24A446`) booted next to One UI on a rooted Galaxy S24 Ultra (SM-S928, SM8650,
KernelSU), switched to and back the way OneOS switches Androids. XNU runs as a Linux process (xnuhost) on One UI's
kernel. Read first: [README.md](README.md) (status), [docs/architecture.md](docs/architecture.md),
[docs/building.md](docs/building.md).

## The owner's rules (read these first)

> **Fix, do not only probe (2026-11-15).** Every attempt (a commit, a phone boot, a helper's task) is a **fix**, or a
> fix plus a lasting debugging tool. Never a change that only adds log lines or switches "to see on the next boot".
> 1. Start from the logs we already have: `out/session-logs/`, iOS's own log (`oneios-tool phone pull-log`,
>    `oneios-tool ioslog`), `xnuhost.log` (`oneios-tool log summary`), the `oneios-cpu`, `oneios-top`, `oneios-mem`,
>    `thermal` and `perf:` lines. Trace the fault to its cause in the code.
> 2. Fix the cause in our code. Look for leaks, busy loops, polling without a sleep, the same event sent again and
>    again (iOS redraws or asks again), work repeated per frame or per key, and slow paths (a syscall here costs about
>    70 us, a page fault about 30 us: 20-30 times an iPhone).
> 3. Say what the fix changes, how it was tested (host tests), and which phone lines prove it.
>
> A lasting tool (an `oneios-tool` command, a cheap log line that stays on) is fine; a one-boot probe is not. Give
> this rule to every helper you start. Do not leave a task half done: finish it, or say exactly what is left.

> **iOS's RAM is never swapped (2026-11-15).** Fixed, locked RAM (`ram-giveback=0`, prefault, `mlock`) at the most
> that fits, all CPU cores, no caps of ours on the GPU; keep only the headroom the way back needs
> (`ram-session-reserve`).

> **No `sleep`, no `until`/`for` wait loops (2026-11-22).** Start a long job (build, host tests, kernel build, copy)
> with a background task and wait for its completion notice; follow a live log with `oneios-tool phone log --follow`
> under a filter. Never a polling loop, in the foreground or the background.

> **Wait with the Monitor tool, never `tmux wait-for` (2026-11-25).** A long job (release, build, tests) runs in tmux
> with its output in a log file; follow it with the Monitor tool on `tail -n 0 -F LOG | grep --line-buffered -E
> "<progress>|<the end line>|error|FAILED"` (every end, the failures too, not only success), and read the log when
> its end line arrives. No `tmux wait-for` in the foreground or in a background task, no `sleep`.

> **No qemu boots: test on the phone (2026-11-24).** Do not boot the kernel or the runtime under qemu (the 25-minute
> test-kernel or `--mode bench` boots) to check a change, unless the owner asks for it. Run the fast host tests
> (`runtime/host-tests.sh`, whose qemu `boot` part now runs only with `QEMU=1`; `kernel/tests/host.sh`, unit tests), build, and hand the files
> to the Mac for a phone boot: the phone is the test. A helper on the build laptop says "not booted" and gives the
> Mac what to stage and the proof lines.

- **No Python.** Retired 2026-11-21: no `.py` file, no `python3` in a script, CI or a doc's commands. Tools are
  `oneios-tool` (Rust) or plain POSIX shell; JSON in a script is `oneios-tool json`. Only other projects' pinned code
  (UniXNU, Droid-VM, Mesa) still runs Python inside the kernel and Metal builds (`tools/oneios-tool/todo_rust.md` 7).
- **No Apple files in git or in releases.** IPSWs, cryptexes, kernelcaches, `kernel.kc`, the image set, device trees
  and anything built from them stay in `out/` and `inputs/` (git-ignored). `runtime/package.sh` refuses them.
- **The way back to One UI must always work.** xnuhost and its helpers never grab the side keys (event0, event2,
  event4), run realtime threads, leave their namespace or cgroup, lower their OOM score, write under `/sys/power`, or
  reboot (`oneos/interface.md`). Phone tools run as root beside One UI and restore what they change.
- Do not send the owner's email address to any service.

## Keep the docs up to date (same commit)

A change is not done until the docs that describe it say so, in the same commit. Never leave a doc describing
behaviour that is gone or a command that no longer works. Write facts: the exact commands, what was tested and what
not; mark guesses as guesses.

| You changed | Update |
|---|---|
| How far the boot gets | `README.md` "Status", `kernel/patches/oneios-hosted/README.md` "How far it boots" |
| A build or run step, a script's options, an output path | `docs/building.md` (and the script's header comment) |
| How the parts fit together | `docs/architecture.md` |
| xnuhost, the loader (`runtime/loader`, the device tree), `hostcall.h` | `docs/booter.md` |
| A kernel patch | `kernel/patches/oneios-hosted/series` comments and `README.md` |
| The prelink, `kernel.kc` | `kernel/prelink/README.md` |
| The kernel build, the iPhone profile | `kernel/README.md`, `kernel/profiles/iphone18-2-ios27.md` |
| The image set, the IPSW profile | `docs/image-format.md` |
| Root tweaks, daemon policy, the device identity, guest helpers | `docs/userland.md` |
| A hardware bridge or driver (touch, pen, audio, camera, encoder, Wi-Fi, ...) | `docs/hardware.md` and its `README.md` |
| The GPU path | `docs/gpu.md` |
| The display helper | `runtime/display/README.md` |
| The runtime package, releases, CI, the `app/` submodule | `docs/releases.md` (and `oneos/interface.md` for the fork's ABI) |
| `oneios-tool` (any command) | `tools/oneios-tool/README.md`, `docs/building.md` section 10 (phone and cycle: sections 6 and 7) |
| A reference repo or pin | `docs/references.md`, `scripts/setup/get-references.sh` |

## The build host and long jobs

- **Every build and test runs on the build laptop (owner's rule, 2026-11-23):** `ssh akshay@100.76.16.53` (Linux, 20
  cores; checkout `/home/akshay/Documents/GitHub/OneiOS` with `secrets/oneios-debug.keystore` and `inputs/`). Push the
  work to a branch, `git pull` there, run long jobs in tmux with their output in a log that ends with a line such as
  `EXIT=$?`, and follow that log with the Monitor tool (from the Mac: `ssh ... 'tail -n 0 -F LOG | grep
  --line-buffered ...'` as the Monitor's command). Releases are made there (`scripts/release/release.sh`).
- The Mac (`/Users/akshaycm/Documents/GitRepos/OneiOS`) edits, commits and pushes, and drives the phone (adb,
  `oneios-tool phone`); copy back only what the phone needs. Its own build path (Xcode, the NDK, Apple's `container`
  for `runtime/host-tests.sh`) still works, but do not run builds or tests there.
- Builds: `runtime/build.sh STAGE` (runtime), `kernel/build-hosted.sh` with your own `HOSTED_OUT=<name>` (never the
  default tree), then the prelink (`kernel/prelink/README.md`). `scripts/bg.sh NAME CMD` runs a job in tmux if it must
  outlive the session.
- Never end your turn while one of your builds, copies or boots runs: wait for its notice, read the result, commit.
- **Disk:** check `df -h /System/Volumes/Data` before a kernel build or a copy. Do not keep copies of `data.img` (a
  32 GiB sparse file): read iOS's log with `phone pull-log`. Remove a worktree and its branch once it is merged.

## The phone

All phone work goes through `oneios-tool phone` (`status`, `switch --version-code N`, `return`, `stage
runtime|runtime-asis|image`, `manifest DIR --stage`, `sums`, `log [--follow] [--grep] [--pen]`, `pull-log`,
`display-restart`, `touch`, `pen`, `backlight`, `cpu`, `procs`), `oneios-tool app` and `cycle`. Use adb only where the CLI has
no command yet (say so, and add the command).

- **Logs:** `oneios-tool phone pull-log DIR` (iOS's log of a boot in seconds, beside an open session, with
  `ioslog health`, `ioslog keyboard` and `ioslog haptics`); `phone log --save FILE` for `xnuhost.log`; then `oneios-tool log summary`,
  `log stacks`, `log gpu|cpu|display|perf|pen`. `phone pull-data` (the whole store) only when the store itself is needed.
- **A test boot:** stage the runtime (`phone stage runtime-asis DIR` for a copy of the phone's runtime with new
  binaries) and the image's changed files (`phone manifest DIR --stage`, e.g. `kernel.kc`, `kernel.kc.hashes`,
  `overlay/userland/xnuhost.conf`), then `phone switch`; keep the kernel you replace (its sha256 in `phone sums`) so
  you can stage it back.
- **Settings of a session** live in the image's `overlay/userland/xnuhost.conf` (`pen=auto`, `encoder=auto`,
  `boot-args=...`). The kernel's debugging switches (`oneios-ubt=`, `oneios-sysfail=`) are lasting tools: use them in
  a boot that tests a fix, and take them out of the config again after.

## Several agents and helpers

- Commit with a pathspec only (`git commit -- <paths>`), never `git add -A`; never commit another agent's hunks.
  `git pull --rebase origin main` (or fetch and merge) before `git push origin main`.
- A helper works in its own worktree and branch. Merge it into `main` after its host tests pass (and a phone boot,
  when it changes what runs on the phone), then remove its worktree and branch.
- Kernel changes go under `#if XNU_HOSTED` as a numbered patch in the series. Fix the cause of a panic; never silence
  an assert without a reason written in the patch.
- Forks for other phones send pull requests to `akshaynexus/OneiOS` `main` ([CONTRIBUTING.md](CONTRIBUTING.md)); keep
  device code behind a backend option or a per-device profile, never change the S24 Ultra defaults. `docs/forks.md`
  maps the forks.

## The iOSOnAnd tracker (2026-11-24)

The public per-phone status of every iOS-on-Android port is on the iOSOnAnd tracker
(<https://hackios.appaiproxy.com>). This repo is tracked as `akshaynexus/OneiOS`, phone `s24-ultra`, maintained by
the owner. The `/track` skill (`.claude/skills/track/SKILL.md`) has the commands and the rules.

- **Agents update it.** As the owner chose at the setup (`auto`, `ask` or `off`, asked once and kept in
  `~/.config/iosonand/agent-prefs.json`; the `/track` skill): when a phone boot proves an area works, or shows it
  broken or changed (boot, touch, wifi, gpu, thermal, audio, ...), update that area's state with `status-set`: what was seen, the phone, the date, and a
  public source (the pushed commit or doc that records the boot). Do it after the docs are pushed. A change that is
  only built or host-tested changes no state.
- **Only claim what the repo or a log shows.** Ask the owner before you change a state the owner set by hand.
- **Ask before posting any image or video** to the tracker, whatever the setting.
- **The skill for other ports:** `skills/iosonand/SKILL.md` is the generic iosonand skill (for iOSOnAnd and forks;
  link it from a fork's CLAUDE.md or AGENTS.md).
- **Sign in with the gist proof only** (`node cli.mjs login`); never `login --token`. Read `cli.mjs` before the first
  run. No email address, no personal detail, no Apple file in anything sent to the tracker.
- Not signed in on this machine (`whoami` gives 401): skip the update and tell the owner to run `/track` once.
- A contributor who ports a phone (CONTRIBUTING.md "Add a device") is pointed at `/track` for their own fork and phone.
