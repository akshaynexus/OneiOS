# Working on OneiOS

Goal: iOS 27 (iPhone18,2, 27.0.1 `24A446`) booted next to One UI on a rooted Galaxy S24 Ultra (SM-S928, SM8650,
KernelSU), switched to and back the way OneOS switches Androids. XNU runs as a Linux process (xnuhost) on One UI's
kernel. Read first: [README.md](README.md) (status), [docs/architecture.md](docs/architecture.md),
[docs/building.md](docs/building.md), and [docs/agents.md](docs/agents.md) (the team board) when several agents work.

## Keep the docs up to date (every change, same commit)

A change is not done until the docs that describe it say so. Update the relevant doc **in the same commit** as the code
(or the next commit, before you report the work finished). Never leave a doc describing behaviour that is gone, a
command that no longer works, or a status that has moved on. If you are unsure which doc, check this table:

| You changed | Update |
|---|---|
| How far the boot gets (a new stage reached, a new stop) | `README.md` "Status", `kernel/patches/oneios-hosted/README.md` "How far it boots" |
| A build or run step, a script's options, a prerequisite, an output path | `docs/building.md` (and the script's own header comment) |
| How the parts fit together (a new component, a moved boundary, a new device or channel) | `docs/architecture.md` |
| xnuhost, the loader, `hostcall.h` | `docs/booter.md` (`hostcall.h`'s own comments too) |
| A kernel patch (added, changed, reordered) | `kernel/patches/oneios-hosted/series` comments and `README.md` (or the patch's `000N-README.md`) |
| The prelink, `kernel.kc`, pairing records | `kernel/prelink/README.md` |
| The VM kernel build, pins, the iPhone profile | `kernel/README.md`, `kernel/profiles/iphone18-2-ios27.md` |
| The image set, the IPSW profile, the fetch | `docs/image-format.md`, the profile's comments |
| Root tweaks, daemon policy, guest helpers | `docs/userland.md` |
| A hardware bridge or its iOS-side driver | `docs/hardware.md` and the bridge's or driver's `README.md` |
| The GPU path | `docs/gpu.md` |
| The display helper | `runtime/display/README.md` |
| The runtime package, `runtime.json`, releases, CI | `docs/releases.md` |
| What the OneOS fork reads or does (session ABI, stage layout, start/stop) | `oneos/interface.md` (and the fork's `docs/oneios.md`) |
| A new reference repo or pin | `docs/references.md`, `scripts/setup/get-references.sh` |
| A phone check tool's commands or output | its `README.md` "On the phone" section |
| The Rust tools (`tools/oneios-tool`: sym, log, image, ioslog, prelink, kernel, phone, app, cycle), a comparison with the tool they replace | `tools/oneios-tool/README.md` (commands, the comparison and its result, crates), `tools/oneios-tool/todo_rust.md` (what is left), `docs/building.md` section 10 |
| The phone and app workflow (`oneios-tool phone`, `app`, `cycle`: a command, an option, a step, a phone rule) | `tools/oneios-tool/README.md` ("phone", "app", "cycle"), `docs/building.md` sections 6 and 7, and the stand-in phone's tests (`tools/oneios-tool/tests/phone`, `app`, `cycle`) |

Write docs as facts: what it does, the exact commands, what was tested and what was not. Mark guesses as guesses.
The board (`docs/agents.md`) is for coordination between agents; it is **not** a substitute for the docs above.

## Rules

- **No Apple files in git or in releases.** IPSWs, cryptexes, kernelcaches, kexts, `kernel.kc`, the image set and
  anything built from them stay in `out/` and `inputs/` (git-ignored). Releases carry only this project's own code.
  `runtime/package.sh` refuses Apple files; never work around it.
- **Shared checkout** (several agents commit to `main` here): commit with a pathspec only
  (`git commit -- <your paths>`), never `git add -A`; `git pull --rebase origin main` before `git push origin main`
  (if other agents' unstaged files block the rebase, fetch and rebase only when origin moved). Never commit another
  agent's hunks; leave other agents' uncommitted files alone.
- **Kernel builds:** use your own tree with `HOSTED_OUT=<name>`; the default tree belongs to the lead. tmux does not
  carry environment variables: pass them inside the command (`scripts/bg.sh NAME env HOSTED_OUT=x kernel/build-hosted.sh`).
  Never edit a build script while a build that runs it is going.
- **Long jobs** (builds, qemu boots, downloads) run in tmux via `scripts/bg.sh NAME CMD...`, every qemu run under
  `timeout`. Kill your own tmux sessions and qemu processes before you finish. `/tmp` is a 16 GB tmpfs: check
  `df -h /tmp` before big jobs.
- **Never end your turn while one of your builds or boots is still running.** Nothing wakes you when it finishes, so the
  result would never be read or committed. Stay in the turn, check the job every few minutes (`scripts/bg.sh -s NAME`
  and its log) until it ends, then commit, push and report.
- **Kernel changes** go under `#if XNU_HOSTED`, as a numbered patch in `kernel/patches/oneios-hosted/series`.
  Fix the cause of a panic; never silence an assert or skip a check without a real reason written in the patch.
- **The way back to One UI** must always work. xnuhost and everything it starts must never: grab the side keys'
  input devices (event0, event2, event4), run realtime threads, leave its namespace or cgroup, lower its OOM score,
  write under `/sys/power`, or reboot. See `oneos/interface.md` and the fork's `docs/oneios.md`.
- Phone tools run as root with One UI running and must restore whatever they change.
- **Phone and app work goes through `oneios-tool`** (`phone` status/switch/return/stage/sums/log/pull-data, `app`,
  `cycle`). Use adb, the shell scripts or other tools directly only when the CLI fails or lacks the command, then
  say so and fix or extend the CLI.
- Do not send the owner's email address to any service.
