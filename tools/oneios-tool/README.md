# oneios-tool: OneiOS's host tools in Rust

One binary, `oneios-tool`, with a subcommand per tool. Each tool that replaces a Python script is compared with that
script before it may replace it; the commands and the results are below. What is left: [todo_rust.md](todo_rust.md).

| Tool | Replaces | State | Same output as the old tool |
|---|---|---|---|
| `sym` | `scripts/debug/dsc.py` (**retired 2026-11-12**: deleted, its output frozen in `tests/sym/golden/`) | done | yes: byte for byte on 18 of 18 address sets (Linux, 2026-11-12; the Mac's run: 18 of 18) |
| `sym kernel`, `log panic` | naming a kernel panic's frames by hand (`atos -o xnu.macho` on the Mac, with each segment's move subtracted) | done; **not run on the Mac's log and kernel of the 2026-11-12 panic** (not on the Linux box) | compared with `llvm-symbolizer --inlining` on three Linux-built pairs: the stand-in panic's 19 addresses and 31,605 more per pair (311,605 on one) name the same function, file:line and inlined frames |
| `log` | nothing (new) | done | not applicable; 33 commands end with status 0, two runs equal |
| `image`, `remotezip`, `rangeserve` | `scripts/image/imagetool.py`, `scripts/fetch/remotezip.py` (**retired 2026-11-21**: deleted), `scripts/serve/rangeserve.py` (retired) | all 15 subcommands in Rust since 2026-11-13, and the image scripts run them | yes: 138 stand-in cases; on the real set 35 of 36 items and the two full builds, byte for byte (the 36th: the Linux set has no `kernel.kc`); the six last subcommands and `remotezip`: 25 of 25 side by side, the archive byte for byte, the real IPSW over HTTP (the section at the end) |
| `ioslog` | the manual steps of `docs/newtodo.md` "How to resume" step 2 (mount, copy, `unifiedlog_iterator`) | done; its first run on the phone's Data store (2026-11-13) failed in the APFS reader, fixed ("The APFS reader": checkpoints); not run on that store again yet | yes on 7 archives: CSV byte for byte from a `.logarchive`, from a Data-store image and from a mounted volume; stores with a cut-off newest checkpoint give the checkpoint before's log |
| `prelink` | the steps of `kernel/prelink/build.sh` | all 12 steps (`PRELINK_IMPL=rust`) | yes: every step's files and stdout byte for byte on two Linux-built kernels, `kernel.kc` `ffbe95f7...` and `cdd98e7e...` |
| `kernel` | the board archive generators of the kernel build (`build-oneios-*.py`, nine), `kernel/build-record.py`, the two `layout.py` and `port-legacy-display.py` | all ported, and the scripts **retired 2026-11-13** (deleted, their outputs frozen in `tests/kernelgen/golden/`): the kernel builds run the commands | yes: every file of the nine archives byte for byte (261 files, one line of two `objects.json` aside), the build record, the three kernelcache records, and `kernel/build-hosted.sh` gives the same kernel either way but for the build time it carries (2,074 bytes of time stamps and the UUID; Linux, 2026-11-13) |
| `kernel hosted-fixup`, `kdk-headers`, `iig-fix`, `series-check`, `drivers`; `prelink classes`, `stubs-check`, `unmapped`, `port-fixture`, `lead-between`, `lead-imports` | `kernel/hosted-fixup.py`, `kernel/kdk-headers.py`, `kernel/linux/iig-fix.py`, `kernel/tests/series-check.py`, `drivers/build.py`; `kernel/prelink/{classes,stubs-check,unmapped,port-fixture}.py`, `kernel/prelink/lead/{between,imports}.py` | all ported, and the scripts **retired 2026-11-13** (deleted, their output frozen in `tests/kernelpy/golden` and `tests/prelinkpy/golden`); **`kdk-headers` not run on a real Kernel Debug Kit or a Mac** | yes: 20 of 20 and 18 of 18 frozen cases, 10 of 10 on real inputs, and `kernel/build-hosted.sh` gives the same kernel but for the build time, the UUID and the `N_OSO` times (2,037 bytes; the section at the end) |
| `kernel hosted-series`, `hosted-archives`, `host-cc`, `display-capture` | the inline Python of `kernel/build-hosted.sh` and `kernel/linux/unixnu-layout.sh`, and `kernel/patches/oneios-hosted/display/display-capture.py` (**retired 2026-11-21**) | done; the kernel scripts run them | yes: the device copy's `board.toml` and defconfig and `host-cc`'s three files byte for byte; a kernel built with the new `build-hosted.sh` is main's but for the build time, the UUID and `N_OSO` (2,060 bytes; "kernel", below). `display-capture` not run beside a real boot |
| `package` | `runtime/package/pack.py` (the runtime package's Apple-file check, its manifest and archive) | ported, and the script **retired 2026-11-13**; `runtime/package.sh` and CI run the command | yes: 91 of 91 frozen cases (every refusal rule), `runtime/package/test.sh` 19 of 19 with the same output |
| `debug` | the Python helpers of the tests (`tests/ksym/{compare,kcmap,make-log}.py`, `tests/kernelgen/{kernel-diff,drivers-command}.py`, `tests/phone/mkstream.py`; on 2026-11-21 `tests/ioslog/{mkapfs,filters}.py`, `tests/app/mkapk.py`, `tests/sym/make-log.py` and the old helpers' test's tars: `mkapfs`, `ioslog-filters`, `mkapk`, `sym-make-log`, `sym-cases`, `mktar`, and the new `holes`) | done; the helpers deleted | yes: 24 of 24 comparisons, and each test before and after: the same output; the 2026-11-21 ones each against its script before it went ("debug", below) |
| `phone` | `scripts/phone/stage.sh`, the adb lines of `docs/building.md` section 6, the Mac's `sparsewrite.py` | `phone log`, `sums`, `image-check`, `manifest --stage` and `switch` ran on the S24 Ultra on 2026-11-16; the other subcommands are still unproven there | `stage` and `sums`: the same output and the same stage as `stage.sh` on a stand-in phone; 257 checks pass (2026-11-21: `tests/phone/run.sh`, the host phone's `status` line and `hw-backend.sh`'s host-device checks among them, 30 of them the fork scripts' USB steps, `usb.sh`, with a fork checkout that has them); `sparse-write`: the bytes and length of `sparsewrite.py` on 7 streams and a 32 GiB one, not more blocks, a quarter of the CPU (`tests/phone/sparse-compare.sh`) |
| `app` | the app lines of `docs/building.md` section 7 (new) | `app stage-scripts` ran on the S24 Ultra on 2026-11-16; the app was not built or installed with it | not applicable; 103 checks pass with a stand-in checkout (also as the submodule `app/` of a stand-in OneiOS checkout), gradlew, gh and phone (`tests/app/run.sh`); `app version` runs on the real clone |
| `serve` | nothing (new, 2026-11-15): the one process the OneOS app starts as root for every iOS step on the phone (requests in, JSON lines out; "serve", below) | written; **not run on the phone** (the Android arm64 binary builds, 14.6 MB, and was not run). Since 2026-11-21 in it: the image build (`make-image`), the root tweaks (`tweaks`) and the AEA decryption (`aea`, and `make-image`'s `decrypt`) | not applicable (new). 21 tests of the process on stand-in inputs, 12 checks on the stand-in phone; on the real set and kernel on the Linux PC: `prelink` gives build.sh's `kernel.kc` and work files (24 of 24), `apfs-copy` the tree the copy before gave and apfs-fuse gives, `hashes` the set's list, `sums` `sha256sum`'s sums |
| `image tweaks` | `userland/roottweak` and `userland/image-hook` (the root tweaks and the image build's userland hook; **retired 2026-11-21**: deleted with their tests, `userland/tweak` and build.sh's `--python-hook`) | done 2026-11-21 (`apply`, `hook`, `daemons`, `cdhash`, `boot-check`, and `serve`'s `tweaks`); `scripts/image/build.sh` runs it; **not run on the phone** | yes: 33 of 33 cases of `tests/tweakspy/compare-python.sh`, and build.sh on 24A446's real inputs with each hook: `data.seed` (`27ec4e10...`) and the six overlay files byte for byte ("image tweaks", below) |
| `cycle` | the test cycle of `docs/building.md` section 6 by hand (new) | written; **not run as a whole cycle on the phone or with real builds** (`phone log` itself ran on 2026-11-16) | not applicable; 117 checks pass with a stand-in checkout and phone (`tests/cycle/run.sh`) |
| `ios` | `scripts/ios/{ios-sign,macho-check,make-tbds}.py` (over the `ipsw` program), `userland/metal/shim/codesign` and `userland/metal/build.sh`'s inline Python (**retired 2026-11-21**: deleted) | done; the callers (`scripts/setup/ios-toolchain.sh`, `userland/helpers`, `userland/camera`, `userland/metal`) run it | yes: `sign` 96 of 96 byte for byte, `macho-check` 11 of 11 reports, `tbds` every exported name of 24A446's 4723 stubs (4.5 s; no `ipsw`), the Metal steps' tree, record and sidecar ("ios", below) |
| `json` | the inline `python3 -c 'import json ...'` lines of the shell scripts (new, 2026-11-21) | done | not applicable; 4 unit tests (the edited file as Python's `json.dump(indent=2)` wrote it) |

**No Python of this repository is left** (2026-11-21: the owner retired it; the last were `scripts/ios/`, the root tweaks' frozen original and the comparisons' Python halves, which went with it). The Python that still runs is other projects' pinned code inside the kernel and Metal builds (todo_rust.md section 7, "external"). The image scripts call `oneios-tool image`
(`IMAGETOOL_IMPL` is gone; `scripts/image/imagetool.py` is left in the tree, unused), as do the retired tools' callers (`kernel/prelink/build.sh`
and, since 2026-11-13, the kernel builds, `runtime/package.sh` and the kernel's and the prelink's hand tools: "kernel",
"package" and the group 2 section, below). (Before, one script used the Rust tool without being asked:
`scripts/image/kernel-inputs.sh` decodes the kernelcache with `oneios-tool image im4p` when the `lzfse` program is
not installed, and checks the result against the SHA-256 the kernel profile pins. It always does now.)

## Build and tests

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml     # -> tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml     # unit tests: synthetic data only, no Apple file
tools/oneios-tool/tests/compare-all.sh [PART...]                       # every comparison below whose inputs are named (its header)
```

Rust 1.95 or newer (apk-info-zip 1.1.1 needs 1.95, macos-unifiedlogs 1.88; built with 1.98.0, and 1.99.1 on the Mac). Linux x86_64 (2026-11-12): clean build 1 min 20 s
with two jobs, 51 unit tests pass (`oneios-common` 3, `oneios-image` 29 + 1, `oneios-ioslog` 4, `oneios-log` 7, `oneios-sym` 7),
`parity.py` (the image tool against the Python `imagetool.py`; both deleted on 2026-11-21): 138 cases, all the same. macOS arm64: the three first tools were built and compared there on 2026-11-12
(the numbers in [todo_rust.md](todo_rust.md)). On 2026-11-16 the complete release workspace built on macOS arm64,
`oneios-common` passed 16 tests, `prelink` audited a phone kernel, and `image apfs-copy` read the real SystemOS image there.
One portability fix was needed: Linux/Android's `sched_getscheduler`/`sched_setscheduler` is guarded out of other hosts;
`set_nice` still sets the nice value everywhere.

2026-11-15, Linux x86_64, with `serve` (branch `tool-app-progress`): `cargo test --release`, **175 tests pass, none
fail** (`oneios-common` 15, of them `job` 8; `oneios-image` 34 and its `tests/` 5 + 1 + 1, the new one
`apfs_copy.rs`; `oneios-ioslog` 4; `oneios-kernel` 5 + 12; `oneios-ksym` 4 + 9; `oneios-log` 8; `oneios-package` 6;
`oneios-phone` 29, of them the local forms 3; `oneios-prelink` 3; `oneios-serve` 8; `oneios-sym` 7;
`oneios-testkit` 2; `oneios-tool` 1 + `tests/serve.rs` 21). The box ran other agents' builds: a build took 10 to
22 minutes.

**For the phone itself (Android arm64, 2026-11-15; the first step of `docs/TODO.md`, "Make the iOS image on the
phone").** With the NDK's clang as the linker (API 30), from the Mac:

```sh
N=$ANDROID_NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin      # linux-x86_64 on Linux
rustup target add aarch64-linux-android
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$N/aarch64-linux-android30-clang \
CC_aarch64_linux_android=$N/aarch64-linux-android30-clang AR_aarch64_linux_android=$N/llvm-ar \
  cargo build --release --target aarch64-linux-android -p oneios-tool --manifest-path tools/oneios-tool/Cargo.toml
$N/llvm-strip -o oneios-tool-android tools/oneios-tool/target/aarch64-linux-android/release/oneios-tool
```

It built in 26 s on the Mac (13.9 MB stripped, a PIE for `/system/bin/linker64`). One change was needed:
`oneios-image`'s in-kernel copy calls the `copy_file_range` system call itself, because the libc crate has no wrapper
for Android. Run as root on the S24 Ultra, on the staged files of `/data/local/tmp/oneios` (One UI up, no session):
`ioslog boots data/data.img` 3.1 s, `ioslog show data/data.img --boot last -o FILE` 4.0 s (540,553 entries),
`image apfs-ls image/os.img /` 0.07 s. Not run on the phone yet: `prelink`, `remotezip`, `image apfs-copy`, the
`package` check. **In the runtime package** (branch `app-oneios-tool`): `runtime/package.sh` builds it this way
(API 30 linker, `llvm-strip --strip-all`) into `bin/oneios-tool` and beside the package as
`oneios-tool-VERSION-android-arm64` with its `.sha256`, in CI and in the release (`docs/releases.md`, "oneios-tool for
the phone"); the OneOS app runs that file as root, one `serve` per flow (`oneos/interface.md` section 9). On the
4-CPU Linux PC the first Android build took 23 min with `-j 2`.

With `serve` (2026-11-15, "serve" below) the same lines build on the Linux PC (NDK r28c, `linux-x86_64`, API 30;
`rustup target add aarch64-linux-android`): **14,556,592 bytes stripped** (13.9 MB before), a PIE for
`/system/bin/linker64`; 12 to 22 min on that PC while other agents' builds ran (270 to 480 s of CPU). One change was needed:
bionic calls the time-sharing class `SCHED_NORMAL` (the libc crate for Android has no `SCHED_OTHER`). **This build
was not run**: no phone on that box. On Android the tool puts itself at nice 10 (every command; `--nice` 10 to 19).
The measurement to run on the phone, as root with One UI up and no session:

```sh
adb push oneios-tool-android /data/local/tmp/oneios-tool && adb shell chmod 755 /data/local/tmp/oneios-tool
adb push tools/oneios-tool/tests/phone/serve-on-phone.sh /data/local/tmp/serve-on-phone.sh
adb shell su -c 'sh /data/local/tmp/serve-on-phone.sh' 2>&1 | tee serve-on-phone.txt     # the readings, sums, hashes, verify, apfs-copy, ioslog, a SIGTERM
adb shell su -c 'sh /data/local/tmp/serve-on-phone.sh prelink' 2>&1 | tee serve-on-phone-prelink.txt   # after its inputs were pushed (the script's header)
adb shell su -c "sh /data/local/tmp/serve-on-phone.sh fetch 'https://updates.cdn-apple.com/.../iPhone18,2_27.0.1_24A446_Restore.ipsw'"   # the profile's url
```

Each request runs with one thread, with all, and with one again (the first run also reads the files into the page
cache), under the shell's `time`; the script prints each request's last event and its `bye` line (`threads` 1,
`children` 0), compares the hash list with the staged one and the two copies with each other, and removes
`/data/local/tmp/oneios/work` at the end. A dry run of it on the PC, on a stand-in stage of the real set, is where
the PC's numbers in "serve" come from.

No Apple file is in the repository: the tests make their own stand-in caches, images and logs, and the comparison
scripts read Apple's files in place (`inputs/`, `out/`, both git-ignored).

## The command line

One clap layout for all tools (since the second Linux round, 2026-11-12; before, `sym`, `log` and `image` read
their arguments by hand). The subcommands, options, output and exit statuses are the ones from before: the
comparisons below were run again after the change and give the same results (cargo test, parity.py, the prelink comparison, ioslog on one archive, the real image set with SKIP_SLOW=1, sym and log). What a wrong command
line does is kept per tool, because the Python tools' callers and the comparisons see it: `image` prints its usage
and exits 1 (imagetool.py prints its own and exits 1), `sym` exits 1 (as `dsc.py`), `log` and `prelink` exit 2
for no subcommand (`prelink` also for an unknown one) and 1 for wrong arguments, `ioslog` and the top level exit
2. Kept from the hand-written parsers, on purpose:
- `image`: arguments after the ones a command reads are taken and ignored, and an option given twice counts
  the first time, as in imagetool.py (it reads its arguments by position and its options by name).
- `sym`: an address may be negative (`dsc.py`'s `int()` takes `-0x20`), so its words may start with a hyphen;
  clap then hands an option that follows a word over as a word, and `sym` reads it as the option it is.
- `log`: every subcommand takes every option (one it has no use for is ignored).
The help text is clap's now (`--help`); `image`'s old usage text is still what a wrong `image` command prints.

**`--jobs N` and `--nice N` (2026-11-15), before the tool's name:** `oneios-tool --jobs 4 --nice 15 image apfs-copy ...`.
- `--jobs N` (1 to 8) is the number of threads of the work that splits: the hash lists and the set's `verify`, the
  APFS copy, the prelink (the kexts' scans, and the steps of `prelink build` that run at the same time), and the
  range requests of `image fetch` and `remotezip get` when they name no `-j`. Without it a command that runs alone
  keeps the threads it always had (rayon's default: every CPU; 8 range requests), so the builds on a PC do not
  change. `oneios-tool serve` has its own `--jobs`, whose default is the online CPUs, at most 8.
- `--nice N` (10 to 19) puts the process at that nice value before its first thread starts (on Linux a nice value is
  a thread's, and a new thread takes its maker's) and out of a realtime class if it was started in one. **On
  Android the tool does this without the option, at nice 10;** `serve` does it everywhere. It never raises its
  priority and never asks for a realtime class.
- A wrong value is said on standard error, with exit status 2 (`--jobs 0`, `--jobs 9`, `--nice 5`).

The code is `oneios-common/src/job.rs`: the thread count, the nice value, and what a request of `serve` adds (its
stop, its progress, its temporary files, the heat and battery limits). A command that runs alone sees none of the
latter: every check is one atomic load.

## serve: one process for the app (the protocol)

The OneOS app runs the iOS steps on the phone itself (the owner's design, 2026-11-15): **one executable, started once
per flow as root, no JNI library.** `oneios-tool serve` is that process. The app writes one request per line to its
standard input and reads progress and result lines from its standard output. Kotlin only starts it, sends requests
and shows what comes back: every check and every decision is in the tool.

```sh
su -c 'oneios-tool serve'                      # the app: one su and one process start per flow
oneios-tool serve --stage DIR --request '{"id":"1","cmd":"sums","dir":"DIR/image"}'    # a PC, a test: one-shot
```

**State (2026-11-15): written and tested on a Linux PC; not run on a phone** (none on that box). The process, the
protocol, every way it ends and every command have tests on stand-in inputs
(`crates/oneios-tool/tests/serve.rs`: 27 tests start the process and read its lines, 7 of them downloads from a
server that is a bad network; `crates/oneios-serve`: 8 unit tests; `oneios-common`'s `job`: 9; `oneios-image`'s
`transfer` and `remotezip`: 8 and 4; `oneios-phone`'s local forms: 3; `tests/phone/serve.sh`: 14 checks on the
stand-in phone's stage, in both of its /proc modes), and the heavy commands ran on the real image set and kernel
there ("Measured on the Linux PC", below). The Android arm64 binary builds (14.8 MB stripped) and **was not run**.
The measurement for the phone: `tests/phone/serve-on-phone.sh` ("Build and tests", above).

| Option | Default | Meaning |
|---|---|---|
| `--stage DIR` | `/data/local/tmp/oneios` | the stage. Requests write only under it (and under each `--allow DIR`): a path outside is refused (`outside`). Its `control/` and `session/` files say whether an iOS session is open, and `log`, `log-list` and `cpu` read its files |
| `--allow DIR` | none | one more folder requests may write under (repeatable) |
| `--idle SECONDS` | 300 | the process ends when no request ran or waited and no line came for this long (0: never). The time counts from the last request's end or the last line |
| `--jobs N` | the online CPUs, at most 8 | worker threads of a request (1 to 8). A request's own `"jobs"` may lower it |
| `--nice N` | 10 | the process's nice value, 10 to 19. Set before any thread starts, so every thread has it. Never a realtime class: a process started in one (SCHED_FIFO, SCHED_RR) puts itself back to SCHED_OTHER |
| `--thermal A,B,C,D` | `44,46,45,50` | degrees C: the case pauses the work at A and stops it at B, the battery pauses at C and stops at D (the thermal bridge's HEAVY and TRAPPING levels, `runtime/bridges/thermal`). A pause ends 2 C below its limit. `--thermal off`: no check |
| `--battery-min PERCENT` | 10 | below this, and not charging, heavy requests are refused and a running one stops (0: no check) |
| `--request JSON` | none | run this request (repeatable, in order) instead of reading standard input; then exit with the last one's status (below). For a PC and for measuring |
| `--sys-root DIR` | none | read the phone's files (`/sys`, `/proc`, `/dev/cpuset`) below DIR: for tests |

A wrong option, or a `--request` that is not a request of a known command, is said on standard error before any
line, with exit status 3.

### Why JSON lines, not `@@` lines

The fork's root script answers with `@@RESULT`, `@@OK`, `@@REFUSED` and `@@FAILED` lines of plain words
(`RootReply.kt`). `serve` uses **one JSON object per line** instead, in both directions, because:
- a request has a name and several values, some of them lists (the members of an archive, the reviewed records of a
  prelink) and paths, which may hold spaces. `@@` lines have no quoting;
- several requests live in one process, so every answer line must carry its request's id;
- progress has numbers of several kinds on one line (bytes, percent, ETA, files);
- refusal reasons and log lines are free text and must not break the line format;
- the tool has `serde_json`, and the fork has its own strict reader (`JsonRead` in `core/model`): no new dependency.

What is kept from the root script: the ways a request ends (`ok`, `refused`, `failed`, and `cancelled` in
addition) and its exit statuses for the one-shot form (0 ok, 2 refused, 3 a wrong request, 4 failed, and 5
cancelled).

### The lines

Every line is one JSON object in UTF-8, ended by `\n`, without a line break inside. The tool's lines all begin with
`{"ev":`. A line on the tool's standard output that does not begin so is not the tool's (a banner of `su`): skip it.
The tool's standard error stays empty: what a step prints, on standard output or on standard error, is sent as
`line` events (the process keeps the app's pipe for itself and reads its own two outputs through pipes:
`crates/oneios-serve/src/capture.rs`).

**A request:** `{"id":"7","cmd":"NAME", ...}`. `id` is 1 to 64 characters of `A-Z a-z 0-9 . _ -` (a whole number is
taken too, and comes back as text), chosen by the app, and not the id of a request that is still running or
waiting; it is free again after the request's last event. Every other key is the command's own (the table below),
or one of the two every command takes: `"jobs"` (1 to 8: fewer worker threads than `--jobs`) and `"lines"` (how
many `line` events at most, default 5,000). The network requests take two more, `"connections"` and `"give_up_s"`
("Downloads on a mobile network", below). An unknown key is refused (`usage`), so that a typing mistake is not
silently ignored.

**One request runs at a time.** A request that comes while another runs **waits in a queue** (first in, first out,
at most 32 waiting; a 33rd is refused with `busy`). `ping`, `health`, `cancel`, `pause` and `resume` do not wait:
they are answered at once, also while a request runs.

**The events of a request,** in this order:

| Event | When | Fields |
|---|---|---|
| `queued` | the request was read and must wait | `id`, `cmd`, `ahead` (how many are before it, the running one included) |
| `start` | it begins | `id`, `cmd`, `jobs`, `steps`: the list of its steps, each `{"step":"copy","text":"Copying the files"}`, in the order they run. A step is **done** when the next one's `step` event comes, or the request's last event; the ones after it are **next** |
| `step` | a step begins | `id`, `step`, `n`, `of`, `text` |
| `space` | before a request writes much | `id`, `path` (the folder it writes to, or its nearest folder that exists), `need` and `free` in bytes, `reserve` (bytes that must stay free: 268,435,456). When `free < need + reserve` the request is refused with `space` |
| `progress` | at most four times a second, and at the start and the end of a step | `id`, `step`, `done`, `total`, `unit` (`bytes`; `ms` for `wait`), and with a known total `percent` (one decimal). After a second of work also `rate` (units per second) and `eta` (whole seconds left in this step). `items_done` and `items_total` (files) where a step counts them. `total` 0: not known, and no `percent` |
| `row` | one item of the result | `id`, `kind`, and the command's own fields |
| `line` | a line the step printed | `id`, `stream` (`out` or `err`), `text` (cut at 4,000 characters, then `"cut":true`). At most `lines` of them per request; the rest are counted in the last event's `lines_dropped` |
| `heat` | the work pauses or goes on | `id`, `state` (`paused`, `resumed`), `why` (`case`, `battery`), `text`, and the reading: `case`, `battery` (degrees C), `level` (percent), `charging` |
| `net` | a network request waits for the network, or the network is back | `id`, `state` (`waiting`, `resumed`), `text`, and with `waiting`: `try`, `next`, `left`, `why`, `error` (and `status`); with `resumed`: `tries`, `waited` ("Downloads on a mobile network", below) |
| `paused`, `resumed` | the answer to a `pause` or `resume` request | `id`, `text`, and when a step counts bytes: `step`, `done`, `total`, `unit` (the same section) |
| **one of the four last events** | it ends | below |

**The last event of a request** is exactly one of these, and nothing with its id comes after it:

| Event | Meaning | Fields | Status of the one-shot form |
|---|---|---|---|
| `ok` | done | `id`, `cmd`, `seconds`, `text` (one line for the screen), the command's own result fields | 0 |
| `refused` | the tool decided not to do it, or to stop it | `id`, `cmd`, `code` (a fixed word, below), `text` (the reason in plain words, for the screen) | 2 (`usage` and `unknown`: 3) |
| `failed` | something broke | `id`, `cmd`, `code` (`io`: a file could not be read or written; `internal`: the tool itself; `network`: no byte arrived for the request's `give_up_s`; `changed`: the server's file became another one while it was fetched; `failed`: anything else), `text` | 4 |
| `cancelled` | it was stopped from outside | `id`, `cmd`, `by` (`request`, `eof`, `sigterm`), `removed` (how many temporary files and folders were removed), `text` | 5 |

A request that is refused, fails or is cancelled **removes what it wrote**: its temporary files, and an output that
was not complete (outputs are written under another name, `NAME.part`, and get their name last). A download
keeps its part and the part's state file for a later run when the request says `"resume":true`, or when it was
paused and the end of the process stopped it ("Downloads on a mobile network", below); a part whose SHA-256 or
CRC-32 is not the expected one is never kept. A cancel that comes when the work is already done changes nothing:
the request ends with `ok`.

**Refusal codes** (`code` of `refused`; the text says the same in plain words):

| Code | Meaning |
|---|---|
| `usage` | the request is wrong: a key is missing, unknown or has a wrong value |
| `unknown` | no such `cmd` |
| `busy` | 32 requests are waiting already |
| `outside` | a path to write is not under the stage or an `--allow` folder, is not absolute, has `..`, or is a link |
| `not-found` | a file or a folder the request names is not there |
| `exists` | the output is there already |
| `space` | not enough free space (the `space` event has the numbers) |
| `session` | an iOS session is open: return to One UI first (heavy requests only) |
| `heat` | the case or the battery is at its stop limit |
| `battery` | the battery is below `--battery-min` and not charging |
| `mismatch` | a size, a CRC-32 or a SHA-256 is not the one expected (the text names the file) |
| `apple-file` | the runtime folder holds a file of Apple's |
| `audit` | the kernel collection's audit or stub check found a wrong binding |
| `not-ported` | the step is not in the tool yet (the AEA decryption: `make-image`'s `decrypt` step, below) |

**Lines without a request:**

| Line | When |
|---|---|
| `{"ev":"hello","proto":1,"tool":"0.1.0","pid":4711,"jobs":8,"nice":10,"realtime":false,"idle":300,"stage":"/data/local/tmp/oneios","thermal":[44.0,46.0,45.0,50.0],"battery_min":10,"cmds":[...],"kb":2,"note":"nice 10"}` | the first line, before any request is read. `tool`: the release's version (runtime/package.sh builds with `ONEIOS_VERSION=VERSION`), else the crate's `0.1.0` (a build by hand; every release up to v0.1.16 said `0.1.0` too: issue 75). `cmds`: every command this build knows. `kb`: the format of the OneOS app's pairing files (`kb.json` `kb`) the prelink reads: 2, the matcher folder with its fixtures alone (2026-11-22; a hello without it is a tool of format 1, which also needs XnuPrelink's Python sources there). `thermal` is `null` with `--thermal off`. `note`: what was done about the nice value and the class |
| `{"ev":"error","text":"..."}` | a line that is not a request: not JSON, not an object, no usable `id`. With `"id"` and `"code"`: a `cancel`, `pause` or `resume` of an id that is neither running nor waiting (`no-such-request`), a `pause` or `resume` that the request cannot take (`not-running`, `not-pausable`, `not-paused`: "Downloads on a mobile network", below), or a request whose id is one that is still running or waiting (`duplicate`: the line was not taken, the first request goes on). An id is free again as soon as its request's last event is sent: a request, `cancel`, `pause` or `resume` that names it then is the new one's (since 2026-11-21; before, an id reused right after its last event was refused `duplicate` while the runner cleared up, and a `cancel` of the new waiting request was never answered; `ONEIOS_SERVE_TEST_LINGER_MS` widens that gap for `tests/serve.rs` `an_id_is_free_as_soon_as_its_last_event_is_sent`) |
| `{"ev":"bye","why":"eof","requests":3,"cancelled":0,"threads":1,"children":0}` | the last line. `why`: `eof` (the app closed the pipe), `idle`, `sigterm`, `requests` (the one-shot form is done), `stdout` (never read: the reader is gone). `requests`: how many had their last event; `cancelled`: how many of them were cancelled. `threads` and `children` are what the process still has at that moment: 1 and 0 (`/proc/self/status` on Linux and Android, where there is one; `waitpid` says whether a child exists) |

### How it ends

- **`{"cmd":"cancel","id":"7"}`** stops request 7. A waiting one is taken out of the queue; the running one stops at
  its next check (between chunks, files, kexts and steps), removes what it wrote and ends with `cancelled`, `by`
  `request`. The queue goes on with the next request.
- **The app closes the pipe (EOF on standard input):** the running request is cancelled (`by` `eof`), the waiting
  ones are dropped (each gets `cancelled`), then `bye` and the process exits. EOF with nothing running: `bye` at
  once.
- **SIGTERM, SIGINT, SIGHUP:** the same as EOF, `by` `sigterm`.
- **Idle:** `bye` with `why` `idle` after `--idle` seconds without a request or a line.
- **The reader went away** (a write to standard output fails): the same as EOF, without lines.
- **How long a stop takes:** the work asks between blocks (64 KiB to 1 MiB), files, kexts and steps, and a network
  request that waits for its next try is woken at once. Three things can take longer: a network connection that
  gives no byte is given up after 5 seconds (a `serve` request never waits longer than that on a silent
  connection); a name lookup and a connect are the system's calls and end by their own time (a connect: 5 s for
  each address of the host, 15 s at most; a lookup on a phone without a network: **not measured**); and one step of
  the prelink runs to its end (the longest, `match`, took 4.2 s of CPU on a 4-CPU PC; **not measured on the
  phone**). A request that has not stopped 10 seconds after the end began is not waited for: the process removes
  the registered temporary files itself, says the request's `cancelled` (with an empty `cmd`) and exits (`bye` then
  has `"forced":true`). An exit ends every thread. The part and the state file of a download with `"resume":true`,
  or of one that was paused, are no temporary files: they stay. **This path has no test** (no command of the tool
  refuses to stop in the tests). A process that is killed with SIGKILL removes nothing: its `NAME.part` files
  stay, and the next request for the same output goes on from them (`"resume":true`) or removes them.
- A line of standard input that has no line end when the pipe closes is not taken.
- **Nothing is left behind:** the tool starts no other process in `serve` (no `adb`, no shell, no `git`, no
  `apfs-fuse`, no `llvm-mc`: the commands that need one are not in `serve`), a request's worker threads are made
  for it and joined before its last event, and `bye` reports the threads and children left (1 and 0).
- The exit status of `serve` on standard input: 0 when it ended in order, 5 when the end cut a request short, 3 for
  a wrong command line of `serve` itself. A request whose last event is out when the pipe closes has ended: it does
  not count as cut short, also when the runner has not cleared it yet (before 2026-11-21 it did, and an app that
  closed the pipe right after a request's last event got 5 now and then on a loaded box; the tests that end so failed
  under load).

### Downloads on a mobile network: pause, resume, retries, parallel ranges

(The owner's request of 2026-11-15, branch `tool-download-resume`.) Every network request of `serve` (`download`,
`fetch`, `zip-get`, `zip-ls`) goes through one transfer layer (`crates/oneios-image/src/transfer.rs`). What the app
can rely on:

**Two more requests,** answered at once like `cancel` (they do not wait in the queue; `hello`'s `cmds` lists them, so
an app can tell a tool that has them; `proto` stays 1: nothing that was there changed its form):

| Request | Answer | Meaning |
|---|---|---|
| `{"cmd":"pause","id":"7"}` | `{"ev":"paused","id":"7","step":"download","done":1234567,"total":9876543,"unit":"bytes","text":"paused at 1 MB of 10 MB"}` | the running request 7 (a `download`, `zip-get` or `fetch`) stops its transfer and **keeps what has arrived**: nothing is removed. `done` is the bytes that are in the part file; no byte is counted and no `progress` comes after `paused`. Its connections are closed (one that is inside a read is closed when that read returns, 5 s later at most; what it read is not kept). The request **stays the running one**: the queue waits behind it and the idle time does not run. A `pause` of a request that is paused already gives its `paused` again |
| `{"cmd":"resume","id":"7"}` | `{"ev":"resumed","id":"7","step":"download","done":1234567,"total":9876543,"unit":"bytes","text":"going on from 1 MB of 10 MB"}` | the paused request 7 goes on from the bytes it has. The count of failed tries and the give-up time start anew |

`pause` and `resume` of an id that cannot take them are answered with an `error` line that has the `id` and a
`code`: `no-such-request` (neither running nor waiting), `not-running` (it waits in the queue: nothing to pause yet),
`not-pausable` (the running request is not a `download`, `zip-get` or `fetch`, or it is one whose work is done and
whose last event comes now), `not-paused` (a `resume` of a request that is not paused). A `paused` or `resumed`
that comes when no step counts bytes (before the size is known, between two members) has only `id` and `text`.
**A paused request does not end before it is resumed:** a pause that came with the last byte holds the request
before its next step, and one that came while a finished transfer is checked and placed (a `download`'s `check`
and `place`, a member's CRC-32) lets that step end and holds the request before its last event. What a pause
holds is the network: no request is made and no wait for a new try runs while it is paused. `cancel` ends a paused
request at once (`cancelled`). To give the queue to other requests while a download is paused, cancel it with
`"resume":true` and send the same request again later: it goes on from the part.

**What is kept, and when a later request goes on from it.** A `download` writes `OUT.part` and, beside it, the state
file `OUT.part.state` (a member of `zip-get` and `fetch`: `NAME.part` and `NAME.part.state`, or `NAME.deflate.part`
and `NAME.deflate.part.state` for a deflated member). They are kept when the request ends before the file is whole
- with `"resume":true`: however it ends (cancelled, failed, the give-up time, heat, the end of the process);
- without it: only when the request **was paused** and the end of the process stopped it (`cancelled` by `eof` or
  `sigterm`: the app went away while the user had the download paused). A `cancel` request removes them.

A process that is killed (SIGKILL, the phone restarts) removes nothing. The same request with `"resume":true`, in
the same process or in a new one, **goes on from what the state file lists**; without `"resume":true` it removes the
part and starts again. **The app should send `"resume":true` with every download the user may pause or that should
survive a restart.**

The state file is one JSON object on one line:

```
{"v":1,"source":"https://host/xnu.macho","what":"file","size":104857600,"etag":"\"6917a5c1-6400000\"","last_modified":"Sat, 14 Nov 2026 21:41:21 GMT","offset":0,"length":104857600,"ranges":true,"done":[[0,58720256],[67108864,71303168]]}
```

| Key | Meaning |
|---|---|
| `v` | 1 |
| `source` | the URL (or the file's path) |
| `what` | `file` for a `download`; `member CRC32 NAME` for a member of an archive |
| `size` | the source's size in bytes (for a member: the archive's) |
| `etag`, `last_modified` | the server's `ETag` and `Last-Modified` headers as it sent them, `null` for one it did not send (a file's path: no `etag`, its modification time in nanoseconds as `last_modified`) |
| `offset`, `length` | which bytes of the source the part holds (a `download`: 0 and the size; a member: where its packed bytes begin, and how many they are) |
| `ranges` | false: the server has no range requests (below) |
| `done` | the byte ranges that are in the part, each `[first, end)` counted from `offset`, in order and merged |

It is written to `OUT.part.state.tmp`, synced and renamed over the state file, **after** the part's own data was
synced (`fdatasync`): a state file never lists bytes that a power loss could take from the part. It is written
when the transfer begins, at most every 3 seconds while bytes arrive, at a pause (a moment after `paused` is said:
the sync comes first), and when the transfer ends, whole or not; a kill loses at most the bytes since the last
write (they are fetched again). It holds no hashes of the segments: the sync order above makes the list true, and
the whole file's SHA-256 (or the member's CRC-32) is checked from the disk at the end anyway. When the file gets its
name the state file is removed. (The tool before this wrote a list of finished chunks, `OUT.part.done`: one that is
found is removed, and its part is fetched again.)

A later request **goes on only when** the state's `v`, `source`, `what`, `size`, `offset`, `length`, `etag` and
`last_modified` are what the server says now and the part has `length` bytes. Otherwise it **starts again from the
first byte** and says why in a `line`, for example `xnu.macho: the server's file changed (ETag "a" then, "b" now):
starting again from the first byte`. A server that sends neither header is compared by size alone (a `line` says
so). A range answer whose `ETag` or total size is another than the one the request began with ends the request
(`failed`, code `changed`): the next request starts again.

**Dropped connections.** A connection that drops or gives no byte for 5 s, a name lookup that fails, a refused or
timed-out connect, a TLS failure, and the answers 408, 429 and 5xx are **tried again**; any other status (403, 404,
416) ends the request. A piece goes on **from the byte it reached**, not from its start. **There is no retry
without a sleep.** Before each new try the connection's thread sleeps:
- **1 s after the first failure, doubling to 60 s,** each wait shortened by a random part of up to half (try 1:
  0.5 to 1 s; try 2: 1 to 2 s; from try 7: 30 to 60 s), so that connections that failed together do not come back
  together. A `Retry-After` of the server in seconds is waited when it is longer (60 s at most). Bytes that the
  transfer did not have start the count again.
- **0.1 to 0.2 s when the connection that broke had brought 1 MiB or more:** the network is there and nothing is
  said. (Measured, "The speed", below: with the first row's wait after every drop, a server that breaks each
  connection after about 4 MB made 1 GiB take 204 s instead of 41 s.) The next failure in a row waits the first
  row's time.

**Giving up:** when the network gave no new byte for `give_up_s` seconds (default 1,800) the request ends with
`failed`, code `network`; with `"resume":true` the part stays for the next request. The time counts while the
request tries, not while it is paused (by a `pause` or by the heat limit) or checks a member between two requests;
an answer's headers alone, and the bytes a stream without ranges brings a second time, do not count as bytes.

While it waits the request says so with a **`net` event:**

```
{"ev":"net","id":"7","state":"waiting","try":3,"next":3.4,"left":1771,"why":"dropped","error":"Connection reset by peer (os error 104)","text":"waiting for the network (try 3, next in 3 s): Connection reset by peer (os error 104)"}
{"ev":"net","id":"7","state":"resumed","tries":3,"waited":7.9,"text":"the network is back after 3 tries and 8 s"}
```

| Field | Meaning |
|---|---|
| `state` | `waiting`: a try failed, its wait is of the first row above, and **no connection of the request receives bytes**; `resumed`: the first byte after a `waiting` |
| `try` | the count of failed tries in a row, from 1. One `waiting` per count: eight connections that fail together give one event, not eight |
| `next` | seconds until the next try (one decimal; the text has whole seconds from 1 s on) |
| `left` | whole seconds until the request gives up |
| `why` | `dns` (the name lookup), `connect` (also a TLS failure), `timeout` (no byte for 5 s), `dropped` (the connection ended or broke), `server` (408, 429, 5xx: then also `status`) |
| `error` | the failure in the library's or the system's words |
| `tries`, `waited` | of `resumed`: how many tries failed, and for how many seconds no byte came |

One connection of several that fails while the others receive is tried again in the same way without an event (the
request's `ok` counts every repeated try in `retries`). **A `cancel` or a `pause` during a wait acts at once** (the
wait is a sleep on a condition variable, not a read and not a loop of short sleeps). No `progress` comes while
nothing arrives.

**Parallel ranges.** A file is fetched in pieces by up to `connections` connections at once (HTTP `Range`), each
block written in place into the part (`pwrite`) as it arrives, the pieces in rising order. `connections` is a key
of `download`, `zip-get` and `fetch`: 1 to 8, default 8, and never more than the request's `jobs`. The connections
are threads of the request (scoped: joined before its last event); a `download` has one more thread for the
SHA-256. **A piece is a 4th of a connection's share of the file, 8 to 256 MiB** (1 GiB by 8 connections: 32 pieces
of 32 MiB; a file under 16 MiB: one piece, one connection), and **each piece is one request on a new
connection.** The reason is ureq 2: a connection it takes back from its pool has no read timeout any more, so a
reused connection that goes silent would hold its thread for good and a cancel would not be seen; a new one has the
5 s. Large pieces keep the new connections few, and cost nothing when one breaks, because it goes on from its byte.
While requests succeed the host's address is the one that connected last (for 30 s: no name lookup and no second
connection per request); a request that fails looks the host up again and tries each address for 5 s.

**A server without range requests** (it answers a `Range` request with 200) is fetched in **one stream**; a
`line` says so. Such a download can go on only from its first byte: a drop, a pause and a later request all start
the stream again (`progress` goes back to 0; a resume that comes before the connection has seen the pause lets it
go on), and the state file says `"ranges":false`. Members of an archive need range requests: without them `zip-ls`,
`zip-get` and `fetch` fail as before.

**The SHA-256 of a `download`** is still checked before the file gets its name, on the bytes **read back from the
part file**, not on the bytes as they arrive: a thread hashes the part's unbroken beginning as it grows, while the
download runs. Pieces arrive out of order and SHA-256 has one order, so the hash follows at most `connections`
pieces behind; when the last byte is there the `check` step shows what is left to hash (little, when hashing is
faster than the network). After a resume in a new process the thread reads the bytes that were there already from
the disk (a hash state is not kept in the state file), again while the rest downloads. A member's CRC-32 is still
one pass over the finished part (a deflated member is inflated in that pass).

**Request keys** (beside `resume`): `connections` (`download`, `zip-get`, `fetch`), `give_up_s` (the same and
`zip-ls`: 1 to 86,400). `ok` of a `download` has also `connections`, `ranges` (false: one stream), `retries` (tries
that failed and were repeated) and `resumed_from` (bytes that were in the part when the request began).

**The library.** The transfer layer is ureq 2 (blocking, rustls with ring: no OpenSSL; it was in the tool already,
with the resolver hook that carries the address rule) and scoped threads of our own. The crates that split a file
into ranges and resume were looked at on crates.io on 2026-11-15: `http-downloader` 0.3 and `fast-down` 7 do both,
`trauma` 3 resumes one stream and does not split, `downloader` 0.2 does neither (and is LGPL); **all four are built
on reqwest and a tokio runtime.** That is a second HTTP and TLS stack in the binary and a runtime with threads of
its own, where `serve` gives a request its threads and joins them before its last event (`bye` reports the ones
left); and none has a place for the address rule or for this protocol's pause, `net` event and state file. They
were not built for Android: the reasons above decided. What was written is the policy (when to try again, what to
say, what a part is of): `crates/oneios-image/src/transfer.rs`, about 1,000 lines with its comments, without its
tests.

**What was tested (2026-11-15, Linux x86_64; not on a phone).** The server of the tests is `oneios-tool rangeserve`
with its options that make it a bad network ("rangeserve", below: bodies that break off at a random byte, 503s, no
range requests, another ETag, a slow line), started by each test on a free port of 127.0.0.1.
`crates/oneios-tool/tests/serve.rs` starts `serve` and reads its lines, 7 tests:
- **a pause and a resume in one process** (`a_download_is_paused_and_resumed`): `paused` has the bytes; for 1.5 s
  no `progress` and no last event come and the queue waits; the state file lists exactly the bytes `paused` said
  and the part holds them; `resumed`, then `ok` with the file's SHA-256, no part and no state file left. The same
  for a member of an archive, a `cancel` of the paused request (at once; the part goes, or stays with
  `"resume":true`), and the errors `no-such-request`, `not-running`, `not-pausable`, `not-paused`;
- **a new process goes on** (`a_download_goes_on_in_a_new_process_unless_the_servers_file_changed`): after a pause
  and the end of the pipe (without `"resume"`), after a pause and SIGKILL, and after the end of the pipe in the
  middle of a download with `"resume":true`; `ok`'s `resumed_from` is what the state file listed. **Another ETag at
  the same URL starts again,** and the `line` says why;
- **drops and 503s** (`drops_and_503s_are_tried_again_and_the_app_is_told`): the file arrives with its SHA-256;
  each `waiting` has its count, a `next` inside its row and its text; each `resumed` follows a `waiting`; no
  `progress` goes back; `zip-ls` and `zip-get` through the same server; a connection that had brought 1 MiB comes
  back without an event; the keys' limits;
- **a cancel and a pause during the wait** (`a_cancel_or_a_pause_during_the_wait_acts_at_once_and_the_wait_has_an_end`):
  in the third wait (2 to 4 s) a `cancel` gives `cancelled`, and a `pause` gives `paused`, in under a second; no try
  is made while it is paused; the resume counts from try 1; the end of the pipe during a wait ends the process;
  `give_up_s` 2 ends a request with `failed`, `network`, after 2 s;
- a server without ranges (one stream; after a pause the count goes back to 0); **the heat limits** on a running
  download (`heat` `paused` and `resumed` with no `net` event between, the stop limit ends it with `refused`,
  `heat`, and the part is gone on from afterwards); a pause that is said at once while every connection is inside
  a read that gets no byte.
Every one of them ends with `bye` and its `"threads":1,"children":0`. `oneios-image`'s unit tests (8 of `transfer`,
4 of `remotezip`) have the ranges, the waits' rows, the state file's line and each reason it is not gone on from,
and the same downloads in one process; `oneios-common`'s `job` has the pause, the hold and the bell.
**From Apple's server** (https, this PC's line, 2026-11-15): `zip-ls` of the profile's IPSW 0.6 s, `zip-get` of
`BuildManifest.plist` 0.8 s, and the kernelcache member (23 MB, three pieces) stopped by SIGTERM after 1.2 s with
8 MiB listed in its state file (with the server's `ETag` and `Last-Modified`) and gone on from by a second
process, CRC-32 as the archive says. In 4 of 10 runs of that member one connection was reset by the server inside
its TLS handshake and came back after its wait (`net` `waiting`, `connect`, then `resumed`). **Not tested: a phone, a
mobile network, a change of network in the middle** (Wi-Fi to mobile data), **a name lookup without a network.**

**The speed (2026-11-15, the 4-CPU Linux PC, 127.0.0.1, 1 GiB of random bytes; none of it is a phone's or a
network's speed).** `oneios-tool serve --request` with a `download`, `rangeserve` as the server, each number one
run of two that agreed within 0.1 s; "download" is the time from the `download` step's line to the `check`
step's.

| 1 GiB | The whole request | Of it the download step | |
|---|---|---|---|
| onto tmpfs, `origin/main` before this branch, `--jobs` 1 and 4 | 4.89 s, 4.74 s | 0.60 s, 0.45 s | then 4.3 s of SHA-256, one pass after the download |
| onto tmpfs, 1 connection | 4.31 s | 0.68 s | the SHA-256 runs beside the download: the request is as long as the hash (250 MB/s on this CPU, which has no SHA instructions) |
| onto tmpfs, 2 and 4 connections | 4.31 s, 4.33 s | 0.46 s, 0.40 s | 6, 10 and 18 requests for 1, 2 and 4 connections |
| onto the SSD (btrfs), `origin/main`, `--jobs` 1 and 4 | 9.21 s, 6.94 s | 4.93 s, 2.70 s | |
| onto the SSD, 1 and 4 connections | 5.48 s, 5.09 s | 5.48 s, 3.82 s | (the state file's syncs are in it) |
| onto tmpfs, **each body breaks off after 2 to 6 MB**, 1 connection | 41.1 s | 41.1 s | 270 drops, each tried again after 0.1 to 0.2 s, none said |
| the same, 4 connections | 10.4 s | 10.3 s | 254 drops |
| the same with the first row's wait (0.5 to 1 s) after every drop, before the short wait was made: 1 and 4 connections | 203.6 s, 52.9 s | | 269 drops, 538 and 524 `net` events |

On this PC the network is not the limit, so more connections only shorten the download step, by a third. What
they are for is a line where each connection is slow, and a server that breaks them: four connections wait at
the same time, not one after the other.

### Threads, nice, heat

- A request's work runs in `jobs` threads: a rayon pool made for the request (`--jobs`, default the online CPUs, at
  most 8). A transfer has `connections` scoped threads (at most `jobs`), the thread that started it writes the
  state file, and a `download` has one more for the SHA-256. The threads exist only while the request runs.
- The process runs at nice 10 (`--nice` 10 to 19) and never in a realtime class.
- **Heat and battery, from named files only** (never a folder list or a walk of `/sys` or `/proc`: a root tree walk
  panicked the phone's kernel on 2026-11-12). The files are the thermal bridge's: the case
  `/sys/class/power_supply/battery/lrp` (or `/sys/class/sec/sec-ap-thermistor/temperature` when there is no `lrp`),
  the cell `/sys/class/power_supply/battery/temp` (tenths of a degree), and `/sys/class/power_supply/battery/capacity`
  and `status`. Read when a heavy request begins and then at most every 2 seconds, by the worker threads' own
  checks (no thread of its own). At a pause limit every worker waits (`heat` `paused`, then `resumed`); at a stop
  limit the request ends with `refused`, `heat` (or `battery`). A phone without these files (a PC) has no check.
  The SoC's own zones are not read (the bridge finds them by listing `/sys/class/thermal`; the kernel's own trips
  act on them).
- **A session:** heavy requests are refused (`session`) while an iOS session is open: the stage's `control/boot`
  equals `/proc/sys/kernel/random/boot_id` and `control/state` is `starting`, `running` or `returning`, or
  `session/xnuhost.pid` names a live xnuhost. `ping`, `health`, `cpu`, `log` and `log-list` only read and run beside
  a session.

### The commands

Heavy (H): checked for heat, battery and an open session. Paths are absolute. A path to write must be under the
stage.

| `cmd` | H | Keys | Steps | Result (`ok`'s fields, `row`s) |
|---|---|---|---|---|
| `ping` | | | | `ok` at once |
| `health` | | | | `ok` at once: `case`, `battery`, `level`, `charging` (each only when its file is there), `state` (`ok`, `pause`, `stop`; `unknown`: no file), `session` (true: one is open) |
| `cancel` | | `id` of the request to stop | | that request's `cancelled` |
| `wait` | | `seconds` (default 1), `heavy` (default false: true makes it a heavy request), `file` (a temporary file it writes and removes) | `wait` | counts to `seconds` in `jobs` threads with `progress` (unit `ms`): for the app's own tests of progress, cancel, the queue and the limits. `ok`: `threads` (how many ran), `nice`, `realtime` |
| `download` | H | `url` (https, or a file's path; `http://127.0.0.1` and `http://localhost` too, for tests), `out`, `sha256`, `bytes` (optional: the size expected), `resume` | `download`, `check`, `place` | a file in parallel range requests of 8 MiB into `out.part`; the SHA-256 is checked before it gets its name. An `out` that is there with this SHA-256 is not fetched again. `ok`: `out`, `bytes`, `sha256` |
| `zip-ls` | | `source` (an https URL or a file), `match` (a regular expression, optional) | | `row` `member`: `name`, `bytes`, `packed`, `method`, `crc32`. `ok`: `members` |
| `zip-get` | H | `source`, `outdir`, `members`: a list of `{"member":"...","name":"..."}` (`name`: the file's name in `outdir`, optional), `resume` | `directory`, `download` | members of a ZIP (an IPSW) without downloading it, in parallel range requests; each member's CRC-32 is checked before it gets its name. `row` `member`: `name`, `out`, `bytes`, `crc32`. `ok`: `members`, `bytes` |
| `fetch` | H | `profile`, `indir`, `only` (a list of roles, optional), `source` (optional), `resume` | `directory`, `download` | `image fetch`: the members a profile names, checked against its pins (size, CRC-32, SHA-256, SHA-384), and `inputs.json`. `ok`: `indir`, `bytes` |
| `im4p` | H | `in`, `out` | `decode` | `image im4p`: the payload of an IM4P (the kernelcache). `ok`: `out`, `kind`, `bytes`, `sha256` |
| `aea` | H | `in` (an AEA image of the IPSW), `out`, and optionally `sha256` (the plain file's pin), `key_cache` (a folder under the stage where the key service's key is kept for the next image), `key_file` (the key service's PEM instead of fetching it; not with `key_cache`), `resume`, `give_up_s` | `key`, `check` (a resume), `decrypt`, `place` | `image aea-decrypt` ("image", "aea-decrypt"): the key from Apple's key service (HTTP as `download`), every byte authenticated, the plain file into `out.part`, renamed last; the `space` refusal before the first byte. A wrong key or a changed byte: `refused`, `auth`; a plain SHA-256 that is not `sha256`: `refused`, `mismatch` (nothing kept); `out` there already: `refused`, `exists` (unless it has `sha256`: then `already` true). `ok`: `out`, `bytes`, `sha256`, `encrypted`, `resumed_from`, `key` (`service`, `cache`, `pem`, `archive`, `none`), `compression`, `clusters`, `already` |
| `receive` | H | `from` (a file or a FIFO), `to`, `bytes`, `sha256`, `mode` (`"0644"` or `"0755"`), and optionally `name`, `sums_list`, `ids_list` | `copy`, `check`, `place` | the import's receive check: exactly `bytes` bytes of `from` into `to.part`, hashed as they are written (one pass; `oneios_root.sh receive` reads the file a second time); the size and the SHA-256 must be the ones given before the file gets its name and mode. `to` must not exist. With `sums_list`: the line `SHA256  NAME` is added to it, and to `ids_list` the line `DEV:INODE:SIZE:MTIME NAME`, as `oneios_root.sh receive` writes them. A FIFO that no writer opens is waited for 30 s. `ok`: `to`, `bytes`, `sha256` |
| `sums` | H | `dir`, `list` (default `DIR.sha256`), `strict` (with it a file of `dir` that the list does not name is a mismatch) | `scan`, `hash` | the stage's SHA-256 check: every file of `list` (`SHA256  PATH` lines, as `image.sha256` and `runtime.sha256`) hashed, the files in parallel. `row` `file`: `path`, `bytes`, `sha256`, `ok` (or `missing`). A difference: `refused`, `mismatch`. `ok`: `files`, `bytes` |
| `hashes` | H | `image`, `out`, `chunk` (default 1,048,576) | `hash` | `image hashes`: the chunk hash list of a file and its whole SHA-256 in one pass. `ok`: `out`, `bytes`, `sha256`, `chunks` |
| `verify` | H | `set`, `quick` | `verify` | `image verify` of a set folder against its `manifest.json`; a file that is not the listed one: `refused`, `mismatch` |
| `apfs-ls` | | `image`, `path`, `volume`, `newest`, `checkpoint` | | `row` `entry`: `name`, `mode`, `uid`, `gid`, `bytes`. `ok`: `entries` |
| `apfs-copy` | H | `image`, `src`, `dst`, `volume`, `newest`, `checkpoint` | `scan`, `copy`, `place` | a file or a folder out of a raw APFS image, without a mount, the files in parallel, into `dst.part`, then renamed. `dst` must not exist. `ok`: `dst`, `files`, `bytes`, `compressed`, `skipped` |
| `tweaks` | H | `data` (the copied Data tree, changed in place), `overlay` (the files go to `OVERLAY/userland`), `os_root` (the OS volume's files) **or** `os_image` (its raw image, read with our APFS reader, no mount; `os_volume` a number or a name, default the first), and optionally `build`, `device` (target identity; if absent, `ONEIOS_DEVICE`), `helpers`, `metal_plugin`, `camera_plugin` (each used when it is what image-hook wants: a folder, a `.bundle` folder (its `NAME.build.json` beside it is checked), a folder holding `H9ISP.mediacapture`), `scale` (2 or 3), `config` (another `userland/config`; default the one built into the tool), `args` (more `apply` options, a list: `["--group", "fewest"]`), `owners`, `info` (the hook's `ONEIOS_OWNERS`, appended to, and `ONEIOS_INFO`) | `tweaks`, `place` | `image tweaks hook` in this process ("image tweaks", below): the root tweaks on the Data tree and `boot-contract.json`, `owner-trust.json`, `report.json`, `report.txt`, `tweaks.manifest.json`, `xnuhost.conf` (`scale=` and, on non-20F66 builds, `pen=auto`) (and `bin/oneios-seed` when the helpers have it) in `OVERLAY/userland`: build.sh's hook's bytes. One `space` event (the plug-ins and helpers it places, and 16 MiB). The report as `line`s; `row` `off`: `item`. A wrong input (not an iOS root, a file that is not a property list, a group that is not there, a helper that is not signed, a plug-in that is not its build record's): `refused`, `input`. `ok`: `out`, `build`, `disabled_jobs`, `owner_cdhashes`, `first_start_unpacker`, `off`, `not_found` |
| `prelink` | H | `kernel` (xnu.macho), `kernelcache`, `profile`, `reviewed`, `more_reviewed` (a list, optional), `pairs`, `stubs`, `xnuprelink` (the folder with XnuPrelink's files), `reference` (optional), `out` (a folder), `keep_work`, `oneios_commit`, `xnuprelink_commit` (for the record, optional) | `check`, `native-input`, `merge-reviewed`, `roots`, `hosted`, `match`, `align`, `kclasses`, `structors`, `vtables`, `emit`, `audit`, `stubs-check`, `record`, `hashes`, `place` | `prelink build`: the steps of `kernel/prelink/build.sh` in this process, in `OUT/work` (removed at the end unless `keep_work`); `kernel.kc`, its hash list and its three records are put into `OUT` only when the audit and the stub check pass (else `refused`, `audit`). A kernelcache that is not the profile's: `refused`, `mismatch`. `ok`: `kc`, `bytes`, `sha256` |
| `stubs-check` | H | `kc` | `check` | `prelink stubs-check` alone; a bad stub: `refused`, `audit` |
| `make-image` | H | `indir`, `out` (absent or an empty folder: root's `begin image` makes `image.new`), `work` (the build's own files go to `WORK/build`), `sums_list`, `ids_list` (the stage's lists, written whole; optional), and optionally `profile` (the OneOS app sends `STAGE/runtime/lib/profiles/ios-27.0.1-iPhone18-2.toml` since fork `62a2383`; default: the staged runtime's `lib/profiles/` when it holds one file, else the one `INDIR/inputs.json` names, else of the IPSW profiles there (`image profile` reads them; a host profile `host-*.toml` is not one) the only one, else the only one of the staged kernel's iOS (`runtime.json` `kernel.profile`, `ios-<version>-<build>`), else the only one of those for iPhone18,2; else `refused`, `usage`, naming them. Up to v0.1.16 the default was only a folder of one file, and every released runtime packs four or more, so the app's request without `profile` was refused `usage`: issue 75), `fetch` (default true), `source`, `resume` (default **true**: a stop keeps the fetch's parts), `tweaks` (default true: the root tweaks run, `oneios_tweaks::run_hook` on the OS image; false: the seed is the OS volume's own `/private/var`), `helpers`, `metal_plugin`, `camera_plugin`, `scale`, `tweaks_config`, `tweak_args` (the tweaks' optional inputs; none: the userland config built into the tool; without `metal_plugin` (`camera_plugin`), the staged runtime's `lib/guest/` bundle (capture plug-in), runtime.json `metal.bundle` (`camera.path`), when the bundle's `oneios-target` is the image's `<device> <build>`: docs/image-format.md, "The Metal plug-in of an image made on the phone"); since 2026-11-25 also every other target the runtime lists in `metals` and `cameras` (`lib/guest/<build>/`), the one built for the image, `runtime_plugins` (default true; false: not the runtime's), `kc` (a `kernel.kc` to put in) or `no_kernel`, else `xnu` (default: the staged runtime's `runtime.json` `kernels` entry of the profile's `[os] build` when it has one that is not `kernel`'s, iOS 27.2.1's `kernel/24B156/xnu.macho`; else `kernel.path`), `kb` (default `STAGE/kb-<build>` for such a `kernels` entry, else `STAGE/kb`), `kernelcache` (default `KB/kernelcache.macho` when it is there, else the IPSW's member decoded), `keep_work`, `connections`, `give_up_s` | `check`, `directory`, `download`, `decrypt`, `place`, `data`, `tweaks`, `seed`, `kernelcache`, `kernel-check` ... `kernel-place` (`prelink build`'s sixteen), `manifest`, `verify`, `lists` | `image build` ("image build", below): the whole image set, `scripts/image/build.sh`'s steps in this process. On an iOS 16.5 set the staged runtime's A56 GPU plug-ins are for (runtime.json `metal_a56`), `manifest` also writes their CDHashes into owner-trust.json and its manifest entry, and `kernel.kc.gpuaddon` goes in after `verify` (a sidecar outside the manifest, named in the lists; before 2026-11-28 it went in first and `verify` refused it, `refused`, `mismatch`, "not in the manifest: kernel.kc.gpuaddon": issue 87). **One `space` event** at `check`, for everything it writes (the fetch and the copies inside say none of their own); short: `refused`, `space`, before anything is fetched or written. An AEA image is decrypted (`decrypt`, since 2026-11-21); a wrong key or a changed byte: `refused`, `auth`; a plain image that is not the profile's `plain_sha256`: `refused`, `mismatch`. Pausable (`pause` holds the fetch at once and the build between steps). A stop or a failure removes `WORK/build`, the set's files and the lists' lines; INDIR keeps what was fetched. `ok`: `files` (the set's files with `manifest.json`: the lists' lines), `format`, `version`, `build` (root's `finish image <files> <format> <version> <build>`), `out`, `bytes`, `manifest_sha256`, `seed_sha256`, `metal` (where the seed's Metal plug-in came from, or why there is none: the build's `Metal plug-in: ...` line), `kernel_sha256` |
| `package-check` | H | `dir` | `check` | `package check`: no Apple file in a runtime folder (else `refused`, `apple-file`) |
| `ioslog-boots` | H | `source` (the Data store's image, a `.logarchive`, a folder), `volume` | `read` | `ioslog boots`: `row` `boot`: `uuid`, `start`, `first`, `last`, `entries`; and the table as `line`s |
| `ioslog-show` | H | `source`, `out` (a file; without it the entries come as `line`s), `format` (`text`, `csv`, `jsonl`), `boot`, `since`, `until`, `proc` (a list), `pid`, `subsystem`, `category`, `type`, `grep`, `sort`, `volume` | `read` | `ioslog show`. `row` `shown`: `entries` (written), `of` (read). `ok`: `entries`, and with a file `out`, `bytes` |
| `log-list` | | | | the stage's logs by name (`phone log --list` on the phone itself): `row` `log`: `name`, `path`, `bytes`, `changed`; a log that is not there: `"missing":true`. `ok`: `logs` |
| `log` | | `file` (`xnuhost` (default), `switch`, `return`, `outcome`, `display`, `watchdog`, `usb`, or `logs/xnuhost-EPOCH.log`), `tail`, `grep` (a list), `filters` (a list of `panic`, `display`, `gpu`, `wifi`, `hid`, `mem`), `markers`, `since`, `out` (a file, optional) | | `phone log` on the phone itself: the named file of the stage, read once (no follow), filtered; the lines as `line`s or into `out`. `ok`: `lines`, `matched`, `bytes` |
| `cpu` | | | | `phone cpu` on the phone itself, from named files only (the CPUs 0 to 7 by name, Android's four cpusets, in a session xnuhost's own `cmdline`, `status`, `cpuset` and `stat`; the per-thread list stays with the adb form, which asks `ps`). `row` `cpu`: `n`, `core`, `capacity`, `related`, `governor`, `max_khz`, `scaling_max_khz`, `cur_khz`, `capped`; the report as `line`s. `ok`: `cpus` |

Not in `serve`: `phone` (the adb forms), `app`, `cycle` (they start `adb` or gradle), `--apfs fuse` (it starts
`apfs-fuse`: `serve` reads images with our reader only), `prelink hosted --x18` (it starts `llvm-mc`), and the
prelink record's `git` (its two commits are the request's `oneios_commit` and `xnuprelink_commit`, or null).

**In the tool since 2026-11-21,** so a whole image can be made on the phone by `serve` alone (not run on a phone yet):
the decryption of the IPSW's AEA images (the `aea` request, "aea-decrypt"; before: an outside program on the PC),
the steps of `scripts/image/build.sh` (`make-image`, "image build") and the root tweaks (`tweaks`, "image tweaks").

### An example

```
<- {"ev":"hello","proto":1,"tool":"0.1.0","pid":4711,"jobs":8,"nice":10,"realtime":false,"idle":300,"stage":"/data/local/tmp/oneios","thermal":[44.0,46.0,45.0,50.0],"battery_min":10,"cmds":["ping","health","cancel", ...],"kb":2,"note":"nice 10"}
-> {"id":"1","cmd":"apfs-copy","image":"/data/local/tmp/oneios/image/os.img","src":"/System/Library/CoreServices","dst":"/data/local/tmp/oneios/work/CoreServices"}
<- {"ev":"start","id":"1","cmd":"apfs-copy","jobs":8,"steps":[{"step":"scan","text":"Reading the volume"},{"step":"copy","text":"Copying the files"},{"step":"place","text":"Putting the folder in place"}]}
<- {"ev":"step","id":"1","step":"scan","n":1,"of":3,"text":"Reading the volume"}
<- {"ev":"space","id":"1","path":"/data/local/tmp/oneios/work","need":183500800,"free":52000000000,"reserve":268435456}
<- {"ev":"step","id":"1","step":"copy","n":2,"of":3,"text":"Copying the files"}
<- {"ev":"progress","id":"1","step":"copy","done":41943040,"total":183500800,"unit":"bytes","percent":22.9,"rate":167772160,"eta":1,"items_done":310,"items_total":2315}
-> {"id":"2","cmd":"sums","dir":"/data/local/tmp/oneios/image"}
<- {"ev":"queued","id":"2","cmd":"sums","ahead":1}
-> {"cmd":"cancel","id":"1"}
<- {"ev":"cancelled","id":"1","cmd":"apfs-copy","by":"request","removed":1,"text":"stopped; 1 unfinished file was removed"}
<- {"ev":"start","id":"2","cmd":"sums","jobs":8,"steps":[{"step":"scan","text":"Reading the list"},{"step":"hash","text":"Checking the files"}]}
   ...
<- {"ev":"row","id":"2","kind":"file","path":"kernel.kc","bytes":93700096,"sha256":"ffbe95f7...","ok":true}
<- {"ev":"ok","id":"2","cmd":"sums","seconds":9.4,"text":"12 files, 17.9 GB: all as listed","files":12,"bytes":17859571712}
   (the app closes the pipe)
<- {"ev":"bye","why":"eof","requests":2,"cancelled":1,"threads":1,"children":0}
```

### Measured on the Linux PC (2026-11-15), and what was compared

4 CPUs (no SHA instructions: one thread hashes 235 MB/s), a SATA SSD with btrfs, 24A446's image set. Other agents'
builds and a qemu boot ran on the box at a higher priority than the tool's nice 10 (load 25 to 30), so each
measurement ran in a scope with a CPU weight of its own (`systemd-run --user --scope -p CPUWeight=10000`); without
it the same requests took many times as long (the `receive` of a 94 MB file: 58.6 s), which is nice 10 doing what
it is for. The disk was shared too.
**None of this is the phone's speed.**

| Request (the one-shot form) | One thread (`--jobs 1`) | All threads (4) | The output |
|---|---|---|---|
| `sums` of 8 stand-in files of 150 MB on tmpfs | 5.39 s, 98% CPU | **1.53 s, 343%** (2 threads: 2.69 s) | each row's SHA-256 is `sha256sum`'s |
| `sums` of the real set against its `image.sha256` (22 files, 17.9 GB) | 104.7 s | 92.2 s | all as listed. One file is 10.4 GB: one SHA-256 stream, 44 s at this CPU's speed; the disk gave under 200 MB/s |
| `hashes` of `os.img` (10.4 GB, 9,880 chunks) | 81.6 s | 52.5 s, 172% | the list is the set's `os.img.hashes`, byte for byte, both times |
| `verify` of the set (21 files, every chunk) | | 81.9 s, 183% | `ok: ios-27.0.1-24A446-iPhone18-2 format 1, 21 files` |
| `apfs-copy` of `os.img` `/usr/libexec` onto tmpfs (486 MB, 644 files) | 1.39 s | **0.59 s, 248%** | the same tree as before ("image") |
| `apfs-copy` of `systemos.img`'s dyld cache onto the disk (82 files, 7,312,005,844 bytes) | 139.8 s | 116.3 s (before this change: 176.3 s) | the same tree; the disk decides (7 to 12% CPU) |
| `download` of 1.2 GB from `rangeserve` on 127.0.0.1 onto tmpfs | 7.46 s | 6.39 s | the file, byte for byte; 5.2 s of it is the SHA-256 check (one stream) |
| `receive` of 1.2 GB (a file to the disk, hashed as it is written) | | 9.8 s | placed with its mode; the two list lines as `oneios_root.sh` writes them |
| `prelink` | 14.84 s (`prelink build --jobs 1`) | 11.23 s, 124% (build.sh: 13.71 s) | `kernel.kc` `ffbe95f7...`, as build.sh's ("prelink build") |
| `ioslog-boots`, `ioslog-show` of the last boot into a file (a PC boot's Data store, 32 GiB sparse) | | 3.2 s, 0.4 s | 97,417 entries, 15,018,403 bytes |
| `zip-ls`, `zip-get` of `BuildManifest.plist`, `fetch` of three small members, from Apple's server | | 4.4 s, 1.9 s, 40.7 s | the CRC-32s and the profile's pins hold (the box has no IPv6 route: each first try waits) |

Where more threads do not help on that PC: the disk (a copy onto it, a set read from it), and one large file's
SHA-256. `sums` hashes the files of a list in parallel; for the image set, whose files have chunk hash lists,
`verify` checks every chunk in all threads.

A request that ends leaves nothing: in every run above `bye` says `"threads":1,"children":0`, and a SIGTERM in the
middle of an `apfs-copy` of `/System` ended with `cancelled`, `by` `sigterm`, status 5 and no file of the copy
left.

## The rule for libraries

Use a maintained library for every format and every generic job. A part stays hand-written only where no library
exists or where ours is measured to be faster; each such part says why in its module comment.

| Crate | Used for |
|---|---|
| `macos-unifiedlogs` 0.8.0 (Mandiant, Apache-2.0; git commit `3d5b27a`, pinned in `Cargo.lock`) | all decoding of the unified log: tracev3, timesync, uuidtext, shared-cache strings, message formatting. crates.io has 0.7.0 only, whose API has no lazy messages and no loading of string files by UUID, so the dependency names the commit |
| `clap` 4 (derive) | every tool's command line. Each crate defines its arguments (`Cli` or `Command`); `crates/oneios-tool/src/main.rs` puts them under one command, so `oneios-tool --help` and `oneios-tool TOOL --help` list everything |
| `csv`, `serde_json`, `chrono`, `uuid`, `regex` | `ioslog`'s CSV and JSON lines (the same crates `unifiedlog_iterator` writes them with), times, `--grep` |
| `tracing-subscriber` | `ioslog -v`: the decoder's warnings on stderr |
| `simple_asn1`, `lzfse_rust` | `image im4p`: the IM4P container (DER) and its LZFSE payload; the LZFSE and LZVN kinds of compressed APFS files |
| `aes` 0.8, `ctr` 0.9, `hkdf` 0.12, `hmac` 0.12 (RustCrypto, on `sha2` 0.10 with its `asm`) | `image aea-decrypt`: the AEA archive's key schedule (HKDF-SHA256), its MACs (HMAC-SHA256) and AES-256-CTR |
| `hpke` 0.12 (rozbb/rust-hpke, features `alloc`, `p256`), `p256` 0.13 (`pkcs8`, `pem`) | `image aea-decrypt`: the archive key out of Apple's key service's answer (RFC 9180 base mode, DHKEM(P-256, HKDF-SHA256), AES-256-GCM), the service's PKCS#8 PEM key. 0.12, not 0.15: the same RustCrypto generation as the crates above (one AES, SHA-2 and HMAC in the binary, not two) |
| `plist` | `image tweaks`: every property list the root tweaks read and write, binary and XML (`oneios-tweaks/src/plist.rs`: the crate's reader and writers, since 2026-11-21); `prelink roots` and `emit`: reading the kernelcache's `__PRELINK_INFO` (written back by hand: below) |
| `toml` | the kernel profile; `image profile`: the build profile |
| `indexmap`, `md-5`, `hmac`, `rand_mt`, `memchr` | `prelink`: Python's dict order, the matcher's function digests, the collection's FIPS seals, `random.Random`'s generator for `align`, the search for the kexts' inline pools |
| `tar` 0.4 (`Builder`, `Header`, `Archive`) | every tar the tools write and read: `data.seed`'s headers (`Builder` into a buffer: ustar, a pax extended header for an entry's xattrs, GNU long names; the files' contents are written at their offsets in parallel, `seed.rs`), the image archive (`image pack`) and the runtime package's tar (`package`), since 2026-11-21 |
| `serde_json`, `serde` | every JSON file the tools read and write: `oneios-common/src/json.rs` is serde_json's parser and its pretty (an indent of N spaces) or compact writer, since 2026-11-21 |
| `base64`, `percent-encoding` | the seed's xattr values and their pax keys (`base64.b64encode`, `urllib.parse.quote`) |
| `flate2` (`zlib-rs`) | `phone pull-data`, `phone sparse-write --gunzip`: the gzip stream of the phone's busybox; the zlib kinds of compressed APFS files. The backend is `zlib-rs` (pure Rust), the fastest of the three on the Data store's stream ("The sparse copy") |
| `zbitmap` 0.1 (MIT, github.com/valentinogrande/zbitmap) | LZBITMAP, the compression of most files of iOS 27's OS volumes (decmpfs kinds 13 and 14). A young crate: its output is checked against apfs-fuse on every compressed file of the two images |
| `apk-info-zip` 1.1 (Apache-2.0, github.com/delvinru/apk-info) | `app`: an APK's signing certificates (schemes v1 to v3.x) |
| `sha2` (`asm`), `rayon`, `memmap2`, `libc`, `anyhow` | hashing (the hash lists are `sha2` and `std::io::Write`), parallel work, file maps, `copy_file_range` (`image`, `sym`, `log`) |
| `object` 0.37 | `sym kernel`: all Mach-O reading. The collection's header and load commands (`LC_FILESET_ENTRY`, each member's `LC_SEGMENT_64`s and `LC_SYMTAB`, read at the member's offset with `MachHeader64::parse` and `load_commands`: the crate has no fileset type of its own), `xnu.macho`'s symbols, sections and UUID, its debug map (`ObjectMap`: the `N_OSO` and `N_FUN` stabs), archives of object files |
| `addr2line` 0.25 with its `loader` (on `gimli` 0.32; `cpp_demangle`, `rustc-demangle`) | `sym kernel`: all DWARF reading. `Loader::find_frames` gives function, file, line and the inlined frames of an address, from `xnu.macho`'s own DWARF, a dSYM, or the object files of the debug map; C++ names are demangled by `cpp_demangle` |

Hand-written, and why:

| Part | Why |
|---|---|
| The APFS reader (`crates/oneios-image/src/apfs.rs`, `apfs/files.rs`) | no maintained Rust crate reads iOS's sealed volumes, or any volume of a container but the first (`apfs` 0.4.1: neither; `apfs-core`: stubs; checked 2026-11-12, [todo_rust.md](todo_rust.md)). apfs-fuse is GPL, Linux only in practice, and needs FUSE: it is an optional second backend (`--apfs fuse`), run as a program. The decoders of compressed files are all libraries |
| `ioslog`'s choice of files, filters, text output, checks (`crates/oneios-ioslog`, 950 lines) | this project's own; everything that decodes a log file is the library's |
| `oneios-common/src/json.rs`: the value type | an object keeps the order its keys were set or read in (records and reports read in a fixed order) without serde_json's `preserve_order` feature, which must stay off (below). It is serde_json's `Serialize` and `Deserialize` of that tree; nothing of the text is written by hand |
| The prelink information's XML (`oneios-prelink/src/prelink_xml.rs`, 70 lines) | the kernel reads `__PRELINK_INFO` with `OSUnserializeXML`, which knows `&lt;`, `&gt;` and `&amp;` only; the `plist` crate writes `"` and `'` as `&quot;` and `&apos;` (quick-xml's `escape`), and 24A446's own prelink information has such strings (`<string>"Copyright ...`): the kernel would refuse the whole list. Reading is the `plist` crate |
| `oneios-image`: Python's `repr` and messages (`py.rs`, `oneios-common/src/pyrepr.rs`) | the words of the messages, which tests and the app's logs match; no file is written with them |
| `oneios-image/src/hashes.rs` | one read pass feeds the whole-file SHA-256 and the chunk hashes (1.9 to 2.9 times Python on the Mac); the list itself is a 32-byte header and the digests, written with `std::io::Write` |
| `oneios-sym`'s index | the project's own on-disk index: answers in 3 to 10 ms where `dsc.py` takes 0.03 to 53 s |
| `oneios-log` | xnuhost's own log format |
| `oneios-ksym`: the index (`index.rs`), the walk that fills it (`dwarf.rs`: which addresses, in how many threads, the runs of equal answers), the panic block's reader (`panic.rs`), the search for the member and segment of an address | the project's own on-disk index, as `sym`'s: a later call takes 4 to 10 ms and reads none of the 93 MB collection, the kernel or its 1,070 object files, and stays right when a kernel build replaces them. Everything the index holds is read by `object` and `addr2line`. Three things the libraries leave open are done by hand, each in a few lines: an object file's time is compared with its `N_OSO` stab's (`addr2line`'s loader reads a rebuilt file as it is), instruction sections are found by their flags (`object` knows `__TEXT,__text` only), and `_block_invoke` names are demangled as LLVM writes them |

`serde_json`'s `preserve_order` feature must not be enabled by any crate of this workspace: Cargo gives one set of
features to the whole binary, and with it macos-unifiedlogs writes the keys of a decoded state dump in insertion
order where its own `unifiedlog_iterator` writes them sorted (found by `tests/ioslog/compare.sh`: one line of 1,012,281
differed).

## json: a JSON file read or changed from a shell script (2026-11-21)

`oneios-tool json` (crate `crates/oneios-json`) is what the shell scripts use where they had an inline
`python3 -c 'import json ...'` (Python is retired from this repository: CLAUDE.md).

```sh
oneios-tool json get [--json] [--default TEXT] FILE|- PATH   # the value(s): a string as it is, else compact JSON; exit 1 when none
oneios-tool json lines [--missing TEXT] FILE|- PATH FIELD... # per element of the array at PATH: the fields, tab-separated
oneios-tool json len FILE|- PATH                             # elements, keys or bytes
oneios-tool json set [--string] FILE PATH VALUE              # set (or add the last key); the file is written again
oneios-tool json check FILE|-...                             # exit 1 when one is not JSON
# PATH: segments joined by '.': a key, an index (0, 1, ...), or KEY[FIELD=TEXT] (the elements of the array KEY
# whose FIELD is TEXT). '' or '.' is the whole document. e.g.
oneios-tool json get runtime.json kernel.build_id
oneios-tool json set out/k/xnu-inputs.json "patches[path=$PATCH].sha256" "\"$sum\""
```

An edited file keeps its keys' order and is written as Python's `json.dump(..., indent=2)` wrote it (two spaces,
non-ASCII as `\uXXXX`, a newline at the end), so a file the scripts edited before reads the same. Tests: 4 unit
tests (paths, selectors, the written bytes against `json.dumps`' for the same edits).

## switches and conf: xnuhost's switches and an image's xnuhost.conf (2026-11-25)

`oneios-tool switches` and `oneios-tool conf` (crate `crates/oneios-conf`) are what the OneOS app's Switches screen
runs, as root, from the staged runtime's `bin/oneios-tool`: the app parses no conf file and keeps no list of switches
of its own. No network, no Android API; a command reads one small file and the built-in catalog (3 ms on the build
laptop).

**One source:** the catalog is `runtime/switches` (crate `oneios-switches`, a member of `runtime/`'s workspace and a
path dependency here): every switch of [docs/switches.md](../../docs/switches.md) with its rule, default, off value,
group, title, help and phone state, and every other key of xnuhost.conf with its rule. xnuhost's own `switches:` line
is made from the same table, and `runtime/xnuhost/src/config/tests.rs` `the_catalog` fails when they disagree: a key
xnuhost's parser takes that the catalog lacks (or the other way), a switch whose rule takes another value than the
parser does, a key whose rule takes a value the parser refuses, a choice whose words are not its type's, the
catalog's number readers against xh-core's. A new switch is a row there, in the commit that adds it.

```sh
oneios-tool switches [--json] [--runtime DIR]        # the catalog (DIR/lib/switches.json with --runtime)
oneios-tool conf show FILE [--json] [--runtime DIR]  # the file as xnuhost reads it
oneios-tool conf check FILE [--json] [--runtime DIR] # its errors and unknown keys; exit 1 with an error
oneios-tool conf set FILE NAME=VALUE... [--json] [--runtime DIR]
oneios-tool conf unset FILE NAME... [--json] [--runtime DIR]
oneios-tool conf write FILE [--force] [--json] [--runtime DIR] < NEW-TEXT
oneios-tool conf restore FILE [--json] [--runtime DIR]
```

Exit status: 0 done; 1 refused, the file untouched (with `--json` a JSON `{"error": "..."}` on standard output;
`conf write` adds the `errors`, `unknown` and `warnings` of the text); 2 a wrong command line (clap's; also a `set`
word without `=`). With `--json` all output is UTF-8 JSON on one line; without it, short text lines.

**`switches --json`:** `{"version": 1, "switches": [...], "keys": [...], "key_prefixes": ["hw-backend"]}`. A
switch's fields (always all of them, `null` where one does not apply):

| Field | What |
|---|---|
| `name` | the key (a conf line's key, or a boot argument's name) |
| `where` | `conf` (its own `key=value` line) or `boot-arg` (a word of the file's `boot-args-add=` line: a kernel or plug-in switch) |
| `kind` | `bool`, `choice`, `int` or `text` |
| `values` | a choice's words, else null |
| `min`, `max` | an int's range, else null |
| `default` | the value when the file does not name it (main's behaviour; for a boot-arg the kernel's or the plug-in's) |
| `off` | the value that turns the change back (the behaviour before it); null for an option whose default is already off, or a setting with no "before" |
| `group` | Session, Memory, CPU, GPU, Wi-Fi, Video, Display, Sensors, Audio, Camera, iOS |
| `title`, `help` | a list row's name (40 bytes at most); one or two plain sentences: what it does, what off does |
| `phone` | `tested`, `not booted` or `broken` (docs/switches.md's column "Phone", for the default) |
| `warning` | text or null (`oneios-motion`: a kernel collection that still has AppleSPU panics with it; `ram-ondemand`: against the fixed-RAM rule) |
| `restart` | `session` for every switch: each is read once at a session's start. None needs Update the kernel; a boot-arg one needs a kernel or plug-in that has it (the current release's do) |
| `also` | words taken besides the kind's (`auto`, `keep`, or `0` for a range that starts above it) |
| `items`, `unique`, `trim` | a comma list (kind `text`): the items it takes, whether each only once, whether blanks around an item are taken; else null |
| `max_len` | a plain `text`'s limit in bytes (0: none); else null |
| `word` | null, or what any other one word means (`oneios-disable-allowed`: a product type) |
| `part` | who reads it: `xnuhost`, `fork` (the app's switch), `kernel`, `metal` (the Metal plug-in), `camera`, `display` |
| `auto` | null, or when xnuhost chooses another value itself with the switch not written (`oneios-als-spu`) |
| `line` | whether xnuhost's `switches:` line has it (`takeover-stops` not) |

A bool takes `1`/`yes`/`true`/`0`/`no`/`false` as a conf line, `0` or `1` as a boot-arg (the app writes `1` and
`0`). `keys`: `{"name", "kind" (also "size"), "values", "min", "max", "items", "unique", "trim", "max_len", "also",
"many"}` for every other key xnuhost reads (`pen`, `encoder`, `ram`, `boot-args`, ...; `many`: each line adds one, as
`text-fixup`). A key that starts with one of `key_prefixes` (`hw-backend-*`) is taken with any value. `--runtime DIR`
prints that runtime's `lib/switches.json` (in the runtime package since 2026-11-25, `runtime/package.sh` step 1d;
exit 1 when it has none: leave `--runtime` out), so the screen describes the xnuhost the phone has staged; `conf
--runtime DIR` checks with it. One entry:

```json
{"name":"display-present","where":"conf","kind":"choice","values":["ahb","window"],"min":null,"max":null,"default":"ahb","off":"window","group":"Display","title":"Present in hardware buffers","help":"ahb: the display helper posts frames in write-only hardware buffers, with no CPU cache work per frame (falls back by itself where the phone lacks them). window: through the layer's window, as before.","phone":"not booted","warning":null,"restart":"session","also":[],"items":null,"unique":null,"trim":null,"max_len":null,"word":null,"part":"display","auto":null,"line":true}
```

**`conf show FILE --json`:** `{"path", "text" (the whole file), "set" ({key: value}: every key xnuhost reads but
boot-args-add, the last line's value), "boot_args_add" ({word: value} of the boot-args-add= line xnuhost uses, its
last; `""` for a word without `=`, as `-v`), "unknown" ([keys xnuhost does not read, lines that are not KEY=VALUE]),
"errors" ([{"line": N, "message"}]), "warnings" ([{"line", "message"}]), "exists" (bool), "backup" (FILE.bak is
there)}`. A missing FILE is an empty file (exit 0). An **error** is what xnuhost refuses, and it then refuses the
whole file and the session does not start (`xnuhost: FILE:N: bad setting KEY`): a line without `=`, a key it does not
read (both are in `unknown` too), a kernel switch written as a key, a value the key's rule does not take; also a
boot-args-add= word of a kernel switch with a value its rule does not take, and a boot-args-add= of 512 bytes or
more. A **warning**: a key given again (the last wins), a boot-args-add= word given again (the first wins), a word
`boot-args=` names already (boot-args-add's is not used), a conf key written into boot-args-add=, a switch with a
`warning` set off its default. `host-device`, `panel-fallback`, `text-fixup`, `camera-model`, `clusters` and the
disks are checked for their length only. For example:

```sh
$ printf '# the S24 Ultra session\npen=auto\nencoder=auto\nboot-args-add=-v oneios-pool-cut=0\n# gpu-down=bottom\ndisplay-present=window\n' > xnuhost.conf
$ oneios-tool conf show xnuhost.conf --json
{"path":"xnuhost.conf","text":"# the S24 Ultra session\npen=auto\nencoder=auto\nboot-args-add=-v oneios-pool-cut=0\n# gpu-down=bottom\ndisplay-present=window\n","set":{"pen":"auto","encoder":"auto","display-present":"window"},"boot_args_add":{"-v":"","oneios-pool-cut":"0"},"unknown":[],"errors":[],"warnings":[],"exists":true,"backup":false}
$ oneios-tool conf set xnuhost.conf gpu-down=top --json        # exit 1, the file untouched
{"error":"gpu-down=top: gpu-down takes one of governor, bottom"}
$ printf 'gpu-down=top\nnot a line\n' | oneios-tool conf write xnuhost.conf --json   # exit 1
{"error":"the text has 2 errors: xnuhost would refuse the file; nothing written (--force writes it anyway)","errors":[{"line":1,"message":"gpu-down=top: gpu-down takes one of governor, bottom; xnuhost refuses the file"},{"line":2,"message":"\"not a line\" is not KEY=VALUE: xnuhost refuses the file"}],"unknown":["not a line"],"warnings":[]}
```

**`conf set FILE NAME=VALUE...`, `conf unset FILE NAME...`:** NAME is a catalog switch (or another key of `keys`, but
`boot-args-add` and the `many` ones, which `conf write` changes). A `conf` switch is its own `NAME=VALUE` line: the
last line of that key is changed in its place (a Windows line keeps its `\r`), its other lines removed, else the line
is added at the end. A `boot-arg` switch is a word of the last `boot-args-add=` line, kept one line: the other words
first in their order, then the switches' words in the catalog's order (the same line whatever order they were set
in); the line is added at the end when there is none, and removed when no word is left. Comments, blank lines and
unknown lines stay as they are. A value its rule does not take, an unknown NAME or a boot-args-add= over 511 bytes is
refused (exit 1) and nothing is written; the pairs of one command are changed together or not at all. `unset` returns
a switch to its default (its line or word removed). A file that is not UTF-8 is refused (edit it whole). Nothing is
written when the text does not change. `conf set xnuhost.conf gpu-down=bottom oneios-m2v-shader-fix=0` on the file
above leaves `boot-args-add=-v oneios-pool-cut=0 oneios-m2v-shader-fix=0` and adds `gpu-down=bottom` at the end (the
comment `# gpu-down=bottom` kept).

**`conf write FILE`:** the whole new text on standard input (the app's own editor), checked as `check` does and
written only when it has no error (exit 1 with them otherwise); `--force` writes it anyway (the app asks the user
first). **`conf restore FILE`:** FILE.bak goes back, and the text it replaces becomes FILE.bak (a second restore
undoes the first); exit 1 when there is no FILE.bak.

**Every write** (`set`, `unset`, `write`, `restore`): a temporary file in FILE's folder, given FILE's mode, owner (as
root) and SELinux label (where it can be read and set), written and synced, then renamed over FILE (a reader sees the
old text or the new, never half); a link is followed (the file it names is replaced, the link kept). The text it
replaces is kept as `FILE.bak` (one backup; a new file is 0644 and makes none). `--json` then prints the new `conf
show`.

Tests: `cargo test -p oneios-conf` (7: the JSON read back to the same rules; a file read as xnuhost reads it, its
errors and warnings; set and unset keeping every other line; the boot-args-add= merge and its stable order; the
refusals, all or nothing; the atomic write with its mode and one backup, restore twice, a link); `cargo test -p
oneios-switches` in `runtime/` (every switch's default and off pass its rule, the rules, the numbers); xnuhost's
`config::tests::the_catalog`; `runtime/package/test.sh` (`lib/switches.json` packed, in runtime.json, version 1,
read back by `switches --runtime` as this tree's catalog).

## ios: the iOS user-space toolchain's steps (2026-11-21)

`oneios-tool ios` (crate `crates/oneios-ios`) is what `scripts/ios/{ios-sign,macho-check,make-tbds}.py`,
`userland/metal/shim/codesign` and the three inline Python blocks of `userland/metal/build.sh` did, until 2026-11-21
(the owner retired Python from the repository). The toolchain itself (clang, ld64, the SDK) is docs/building.md
section 9.

```sh
oneios-tool ios sign [--identifier ID] [--entitlements PLIST] [--json] FILE...   # ad hoc, in place; CDHASH ARCH pages=4096 ID FILE
oneios-tool ios macho-check [--tbd DIR | --sdk] [--local DYLIB]... [--arch A] FILE...  # will it load in the guest? exit 1 if not
oneios-tool ios tbds [--cache DIR] [--image systemos.img] [--out DIR] [NAME...]   # the guest's .tbd stubs (out/ios27-tbd; an arm64 cache, iOS 16.5's, gives arm64-ios stubs)
oneios-tool ios tbd-diff [--a-listed] [--show N] A B                            # two stub trees, by what the linker reads
oneios-tool ios codesign ARGS...                         # `codesign` for the Metal build's scripts (shim/codesign runs it)
oneios-tool ios metal-stage-edit ROOT HOOKS JOBS KNOWS [--rt]   # userland/metal/build.sh stage: the staged copy's changes (since 2026-11-23 also: m2v-spare-command-pools.patch and m2v-idle-memory.patch in the bridge's list; since 2026-11-25 the shader fixes 90101, 90102 and their switch 90110 after them, on unless oneios-m2v-shader-fix=0 (since 2026-11-25), then m2v-compile-deadline.patch; turnip-idle-bo-cache.patch and turnip-submit-split.patch in the Mesa list; the camera edits of the frontend (since 2026-11-25 also NativeMetal 3c46b5e: the rebuilt compute function kept until the reflection is read); the command pool and GPU memory rows' calls in the plug-in; with --rt, METAL_RUNTIME=rt: the plug-in script links userland/metal/rt's bridge and does not build the old one)
oneios-tool ios metal-record ROOT BUNDLE RECORD HOOKS TARGET   # build.sh finish: NativeAdrenoMetal.build.json
oneios-tool ios bundle-record ROOT BUNDLE RECORD TARGET [NAME[=COMMIT]...]   # package.sh 3c: an A56 bundle's build record (CDHashes, pins)
oneios-tool image a56-gpu-place --stage STAGE --out DIR   # the app at Update the kernel: an iOS 16.5 set's kernel.kc.gpuaddon, owner-trust.json, manifest.json
oneios-tool ios gpuaddon BUNDLE OUTPUT [--gl GLBUNDLE]   # build.sh addon: the kernel.kc.gpuaddon sidecar (NGPUADD2); addons/NativeAdrenoMetal.bundle, or addons/OneiOSRadeonMetal.bundle for a folder of that name (the A56, userland/metal-amd); --gl: the A56's OneiOSRadeonGL.bundle beside it (userland/metal-amd/gl)
oneios-tool ios addon OUTPUT [--metal BUNDLE] [--camera DIR] [--gl GLBUNDLE]   # the boot addon with the Metal and/or the capture plug-in (and the A56's OpenGL ES), and OUTPUT.cdhashes (hosted 0090)
```

| Command | Was | Needs | Compared with the script (Linux PC, 2026-11-21) |
|---|---|---|---|
| `sign` | `scripts/ios/ios-sign.py` | cctools' `codesign_allocate` (as before: it makes the room) | 96 of 96 byte for byte: 24 made-up files (an executable, a dylib, a bundle, a 300 KB file; three identifier lengths; with and without entitlements), each signed and signed again, their `--json` records and lines; `userland/helpers/build.sh`'s five helpers byte for byte |
| `macho-check` | `scripts/ios/macho-check.py` | nothing (the script ran `llvm-nm`, `llvm-objdump` and needed PyYAML) | 11 of 11 reports line for line (with `--local`, `--sdk`, an unsigned file, a missing stub, the Metal bundle's two binaries); the one changed text: an unsigned file's hint names `oneios-tool ios sign`. 0.3 s for all 11 |
| `tbds` | `scripts/ios/make-tbds.py` over `ipsw dyld tbd` | nothing: the cache with oneios-sym's reader, each image's export trie read here, the cache's folder copied out of `systemos.img` with the APFS reader of `image apfs-copy` when `out/ios27-cache` is missing | 24A446 (4696 images): 4723 stubs in 4.5 s (about 6 minutes before). `tbd-diff --a-listed out/ios27-tbd NEW`: every exported name of every file the same; 4705 files equal in full, 18 differ only in that the documents of re-exported libraries copied into a file now list their own re-exported libraries (as those libraries' own stubs do). Without `--a-listed`: `ipsw` also listed 1,338,142 Objective-C classes and ivars that the images do not export (from their metadata; nothing can bind to them). Versions are `LC_ID_DYLIB`'s (CoreFoundation 5027.0.69; `ipsw` wrote 5027). `hello` and a bundle linked against each set bind the same symbols to the same libraries; `macho-check` gives the same 11 reports with either set |
| `codesign` | `userland/metal/shim/codesign` (Python) | `sign` | the four forms the base's scripts use; `--display` reads what `image tweaks cdhash` reads (the shim read it with roottweak's `macho.py`) |
| `metal-stage-edit` | build.sh `stage`'s inline Python | nothing | on copies of the pinned Droid-VM-UniXNU and NativeMetal trees, with and without the clang-23 warning: the same staged tree (`diff -r`) and the same lines; a hooks file of another revision is refused with the same message |
| `metal-record` | build.sh `finish`'s inline Python | scripts/ios/env.sh's variables | the same record and lines on the bundle in `out/userland`, but for its `check` text (`ios macho-check`) |
| `gpuaddon` | build.sh `addon`'s inline Python | nothing | the same sidecar byte for byte (12,286,344 bytes, 5 files: flate2's zlib-rs writes zlib's level 9 stream). Since 2026-11-24 a bundle folder named `OneiOSRadeonMetal.bundle` (the Galaxy A56's plug-in, `userland/metal-amd`) keeps its name: `addons/OneiOSRadeonMetal.bundle/...` and `addons/OneiOSRadeonMetal.publish`; any other folder is `NativeAdrenoMetal` as before (unit test `the_a56_bundle_keeps_its_name_in_the_sidecar`). Since 2026-11-25 `--gl OneiOSRadeonGL.bundle` (also `ios addon --gl`) adds the A56's OpenGL ES after it: `addons/OneiOSRadeonGL.bundle/...`, no marker; a GL bundle given as the Metal one, or another folder as `--gl`, is refused (unit test `the_a56_gl_bundle_goes_beside_the_metal_plugin`) |
| `addon` | nothing (new, 2026-11-23) | `image tweaks cdhash`'s Mach-O reader (oneios-tweaks) | new: its Metal part is `gpuaddon`'s byte for byte (unit test); the capture plug-in goes to `camera/MediaCapture/H9ISP.mediacapture`, which hosted 0090 mounts; `OUTPUT.cdhashes` has one `CDHASH NAME` line for each Mach-O file in it, for `phone addon`. `userland/camera/build.sh` makes `H9ISP.gpuaddon` with it |

`tbds` writes TBD v4 as the 24A446 stubs were laid out (one item per line); weak definitions and thread-local
variables in their own lists (the `ipsw` stubs had them in `symbols`), an Objective-C class in `objc-classes` when
both its class and metaclass are exported. `macho-check` reads TBD v4 with its own reader of the YAML part TBD uses
(`tbd.rs`: the 705 stubs of the iPhoneOS 26.5 SDK and every stub `tbds` writes); a TBD v5 (JSON) is refused.
Tests: 13 unit tests (DER integers and lengths as Python wrote them, the entitlements' DER, the TBD reader on an
SDK-shaped file, the writer's layout, the export trie, the links, versions, the Metal address replacement, a sidecar
read back) and 3 integration tests: `tests/synthetic.rs` (the signature of a made-up executable, without and with
entitlements, is the SHA-256 and CDHash `ios-sign.py`'s `signature()` gave for the same file; `macho-check` against
stubs written by the writer: bound, weak, local, flat, unexported and unstubbed symbols) and `tests/tbds.rs` (a
made-up split cache: the whole file of an image that re-exports another, the link, the record, an existing stub
kept, a name that is not there). Not run on a Mac (the Python ran there on 2026-11-12: docs/building.md section 9).

## debug: the tests' helpers

`oneios-tool debug NAME` (crate `crates/oneios-testkit`): what the comparison scripts under `tests/` need beside
the tool itself. Five of them were Python scripts beside their tests until 2026-11-13 (CLAUDE.md: no new Python);
each module has the script's name and description, takes what it took and prints what it printed.

| Command | Was | Used by | The same as the script |
|---|---|---|---|
| `debug kcmap members\|forward\|own ...` | `tests/ksym/kcmap.py` | `ksym-make-log`, `ksym-compare` (the test's own reader of a collection, apart from the tool's) | stdout and status of the three subcommands on the `pc` and `pc2` pairs, and of a wrong command line: 7 of 7 |
| `debug ksym-make-log KC KERNEL [--expect FILE]` | `tests/ksym/make-log.py` | `tests/ksym/compare.sh` | the log and the expect list, byte for byte, on both pairs: 4 of 4 |
| `debug ksym-compare --tool ... --kc ... --kernel ... --log ... --ref ... --index ...` | `tests/ksym/compare.py` | `tests/ksym/compare.sh` | `compare.sh` on the `pc` pair before and after: the same output line for line (the scratch folder's name and the times aside), 57 `same` lines, 31,605 of 31,605 addresses twice, `PASS` |
| `debug kernel-diff A.macho B.macho` | `tests/kernelgen/kernel-diff.py` | the check of two kernel builds ("kernel", below) | stdout and status on three pairs (two kernels of other sources, a kernel with itself, two sizes): 3 of 3 |
| `debug mkstream KIND [MIB DATA]` | `tests/phone/mkstream.py` | `tests/phone/sparse-compare.sh` | the SHA-256 of every stream (`runs`, `tail`, `zero`, `empty`, `lastbyte`, `dense`, `phone 64 8`, `phone 300 40`, `phone 4096 280`) and the status of a wrong kind: 10 of 10; `sparse-compare.sh` before and after: 35 `ok` lines, the same output |
| `debug stub-kc OUT GOOD BAD [NOTE]` | new | `tests/cycle/run.sh`, `tests/prelinkpy` | a made-up kernel collection for `prelink stubs-check` |
| `debug mkapfs OUT.img NAME=FOLDER... [--stale-block0]` | `tests/ioslog/mkapfs.py` (2026-11-21) | `tests/ioslog/compare.sh`; the code, `tests/support/mkapfs.rs`, is included by the tests of `oneios-image` (`apfs_files`, `apfs_copy`, `apfs_checkpoints`) and `oneios-tool` (`ioslog_checkpoints`), which ran the script | the image byte for byte, the line and the status: 6 of 6 cases (one volume of 1,003 files, two links and a file in two extents; three volumes with an empty one and `--stale-block0` twice; an empty volume; a non-ASCII name refused, status 1); a wrong command line ends with status 1 and another usage text; apfs-fuse reads the Rust image as its folder (`diff -r`) |
| `debug mkapk OUT.apk CERT.der [TEXT]` | `tests/app/mkapk.py` (2026-11-21) | `tests/app/run.sh` | 3 of 3 the same bytes but the 8 of the zip's DOS time stamp (the script wrote the time it ran, the command 1980-01-01, so two runs are the same) |
| `debug ioslog-filters TOOL SOURCE REF.csv WORK` | `tests/ioslog/filters.py` (2026-11-21) | `tests/ioslog/compare.sh` step 8 | stdout and status on a made-up 400-row reference (quoted commas, a message over two lines, text outside ASCII, two boots) with a stand-in tool, twice (with and without a failing `--grep`): 14 of 14 lines and the status the same. Not run on Mandiant's archives (not on this box) |
| `debug sym-make-log DSC_DIR OUT.log [--slide HEX] [--stacks N] [--seed N]`, `debug sym-cases WORK LOG...` | `tests/sym/make-log.py` and the address-set block of `tests/sym/compare-golden.sh` (2026-11-21) | `tests/sym/compare-golden.sh` | on the 24A446 cache: 4 of 4 logs the same after the version line (which names the command now), the same stdout and status, both errors status 1; the 12 set files the same; `compare-golden.sh` with them: **18 of 18** sets the same as the frozen golden (so the argument hashes, and the addresses drawn, are the script's) |
| `debug mktar [--format pax\|gnu] OUT < SPEC` | new (2026-11-21): the tar `userland/tests/test_helpers.py`'s `seed()` wrote with Python's tarfile | `userland/tests/host.sh` (the oneios-seed cases) | 16 of 16 archives byte for byte with `seed()` (8 cases, pax and GNU: long names, modes, owners, xattrs, `..`, `/abs`, a file under a link, a hard link, a cut archive) |
| `debug holes FILE [--map]` | new (2026-11-21): an inline block of `tests/phone/sparse-compare.sh` | `tests/phone/sparse-compare.sh` | the length, the blocks after an fsync, the SEEK_DATA/SEEK_HOLE regions; a unit test |
| `debug fixture guest-bundle\|guest-dylib\|guest-execute\|apple-dylib OUT` | new (2026-11-24) | `runtime/package/test.sh` part 5; `signed_macho` in `crates/oneios-testkit/src/fixture.rs` is the fixture of `oneios-package`'s lib/guest/ tests | made-up signed arm64e Mach-Os for iOS 27 (ad hoc bundle, library, executable; one library not ad hoc and `com.apple.*`): what the runtime package takes under `lib/guest/` and what it refuses. `oneios-tweaks`' macho reader reads their CDHash back (the tests) |

The sample of `ksym-compare` and the streams of `mkstream` are the ones the Python helpers drew: `pyrandom.rs` is
CPython's `random.Random` on the `rand_mt` crate's MT19937 (`random`, `getrandbits(32)`, `randrange`, `choice`,
`choices`, `randbytes`; a unit test holds six values of CPython 3.14). The strings that named the scripts name the
commands now (`3. ksym-make-log's list`, `kcmap with the segment's move`, the stand-in log's version line), so a
stand-in log is not byte for byte the one `make-log.py` wrote; the 24 comparisons above were made before that change.

No Python is left under `tests/` of the image, phone, app, cycle, ioslog, log and sym areas since 2026-11-21: the
helpers above, `tests/phone/sparsewrite.py` (the frozen original of `phone sparse-write`; `sparse-compare.sh` now
checks the copies against the stream itself and GNU `cp --sparse=always`'s copy, and reads the file system with
`debug holes`), `tests/image/parity.py`, `tests/image/compare-python.sh`, `tests/imagepy/` and
`tests/tweakspy/compare-python.sh` (the comparisons with the deleted Python tools; their results stay below) are
gone, and the inline `python3` of `tests/phone/lib.sh`, `run.sh`, `tests/cycle/run.sh` and `tests/log/run.sh` is
`oneios-tool json` and shell. `tests/compare-python.sh` is `tests/compare-all.sh` (its two Python parts, `parity`
and `image`, are gone).

## ioslog: iOS's unified log from the phone's Data store

Reads iOS's own log from a copy of the Data store (`data.img`) without mounting it, on Linux and macOS alike, and
writes it as text, CSV or JSON lines. It replaces these manual steps (macOS only): attach the image with `hdiutil`,
mount the Data volume, copy `db/diagnostics/{Persist,Special,Signpost,HighVolume,timesync}` and `db/uuidtext/*` into a
`.logarchive`, run `unifiedlog_iterator -m log-archive -i ARCHIVE -f csv -o OUT`.

```sh
oneios-tool ioslog show SOURCE [-f text|csv|jsonl] [-o OUT] [filters] [--sort] [--library-times]
oneios-tool ioslog boots SOURCE                       # the boots: UUID, start, first and last entry, entries
oneios-tool ioslog checks SOURCE [--boot last|all|UUID] [--examples N]
oneios-tool ioslog health [SOURCE] [--boot last|all|UUID] [--top N] [--detail] [--examples N] [-o FILE] [--stage DIR]
                                                      # hangs, watchdog kills, XPC timeouts, jetsam kills, crashes, stuck
                                                      # idle demotions, encode failures, VirtualAudio's start: one line per kind ("health" below)
oneios-tool ioslog keyboard [SOURCE] [the same options as health]
                                                      # the keyboard's path: key taps by app, hangs, asset locks and
                                                      # queries, language models, keyboard layouts ("keyboard" below)
oneios-tool ioslog haptics [SOURCE] [the same options as health]
                                                      # the path of a haptic, step by step: feedbacks asked, the engine,
                                                      # the server, the Actuator's chain, level and device ("haptics" below)
oneios-tool ioslog save SOURCE NAME.logarchive        # the copy the manual steps made, for other tools
# every subcommand: [--volume N|NAME] [--block0 | --checkpoint XID] [--apfs own|fuse|auto] [-v]
```

- `SOURCE`: a raw image of the Data store; or a `.logarchive` folder; or a folder that holds `db/diagnostics` and
  `db/uuidtext` (a mounted Data volume; also `private/var/db/...`, `var/db/...`, or `diagnostics` + `uuidtext`).
- The image: every volume of the container is tried in order and the first that holds `db/diagnostics` is read (the
  phone's Data volume is the third, named `Data`, with no role). `--volume 2` or `--volume Data` names one: a
  volume's number is its place in the container superblock's list (`nx_fs_oid`), as apfs-fuse's `vol=N` counts,
  and a place may be empty (the Data store iOS made on the PC has one volume, `Data`, in place 2). The
  container is read at its **newest checkpoint that passes the checks**, not as block 0 has it: block 0 holds the
  state of the last clean unmount only, every session ends with the kernel killed, and the newest checkpoint may
  be cut off in the middle of a write ("The APFS reader": the checks, the fall back to an older checkpoint).
  **Every command says on stderr which transaction it read**, how many newer checkpoints it skipped and why:
  `data.img: volume 2 (Data) is read at checkpoint xid 198 (superblock at block 20, volume superblock xid 198): the
  newest of 95; block 0 has xid 14; 1746 tree nodes checked`. `--checkpoint XID` reads that checkpoint (if it
  passes), `--block0` block 0's state (if what it names is still there); `oneios-tool image apfs-checkpoints
  data.img` lists every checkpoint with what passes and what is refused at it.
- Filters of `show` (all optional, combined with "and"): `--boot last|UUID-start`, `--since T`, `--until T`
  (`2026-11-12T14:12:00Z` or `"2026-11-12 14:12:00"`; the guest's clock, UTC), `--proc NAME` (the last part of the
  process path; repeatable), `--pid N`, `--subsystem PREFIX`, `--category NAME`, `--type error,fault,...` (the CSV's
  "Log Type", lower case), `--grep REGEX` (on the message). The cheap fields are tested first; a message is
  formatted only for `--grep` and for entries that are written.
- `--boot last`: the boot whose start is the newest in logd's time records (`timesync`); without such records, the
  last boot by its entries.
- Output: `csv` and `jsonl` are `unifiedlog_iterator`'s, field for field; `text` is one entry per line,
  `TIME PROCESS[PID] TYPE [SUBSYSTEM:CATEGORY] MESSAGE` (a message may hold line ends). Entries come in the
  decoder's order (file by file); `--sort` writes them by time and keeps the selected entries in memory.
  An existing output file is replaced (`unifiedlog_iterator` appends).
- `checks` counts, for one boot, the lines of the known problems, with the first and last time and a few examples:
  touch paths (`MAX_PATH_COUNT`), UISensor modes (`digitizerEnabled`, `pocketTouchesExpected`), backlight
  (`VMBLControl`, `No container for handle Display`), crashes and "Too many corpses", launch constraint violations,
  jetsam kills, watchdogs (`Extension is hanging on launch`, `0x8BADF00D`), audio (`configure_thread_for_realtime`,
  `timed out after 3.000s`), heic (`CMPhotoCompressionSessionAddImage`, `codec: 'hvc1'`, `Content load failed`: CMPhoto
  has no HEVC encoder, chronod's widget snapshots are white; expected 0 with the codec
  archive and `encoder=auto`, not yet seen on the phone, 2026-11-22). **The patterns are the texts the project's
  notes quote; they were written without the phone's log at hand.** A count of 0 means the text is not in the log.
- The guest's clock was about a day behind the host's in the 2026-11-12 sessions, so the log's dates are too.
- **Times on the phone's counter (fixed 2026-11-22).** The decoder library turns a boot's ticks into nanoseconds only for
  the timebase 125/3 (Apple's 24 MHz counter) and takes every other one as 1/1. The phone's counter runs at 19.2 MHz
  (timebase 625/12, 52.08 ns a tick), so before the fix each time was 52 times too near the time record before it: the
  2026-11-22 boot of 47.6 min read as 55 s, and the kernel's 10 s `oneios-cpu` lines were 0.19 s apart. Every command
  (`show`, `boots`, `checks`, `health`, `keyboard`, the `--since`/`--until` filters) now scales each time from the
  record the library placed it by (`Clock` in `src/lib.rs`; a boot of 125/3 or 1/1 is left as it is). Checked on that
  session's store: backboardd's AttentionAwareness lines carry its own clock (seconds since boot), and three gaps of it
  (121.75 s, 120.15 s, 217.14 s) are now the gaps of the entries' times to 1 ms; the `oneios-cpu` lines are 10.0 s
  apart. One case is not undone exactly [I]: an entry the library placed from the boot's start (a chunk with no base
  time) before the first record. `show --library-times` writes the library's own times, as `unifiedlog_iterator` does
  (its CSV of that store, `--proc wifid`, is byte for byte the old tool's); `tests/ioslog/compare.sh` and `debug
  ioslog-filters` use it. Test: `cargo test -p oneios-ioslog guest_clock` (the library's own resolver, a 19.2 MHz boot
  with three records, a 125/3 and a 1/1 boot).
- Reading iOS's log of a phone boot needs no copy of `data.img`: `oneios-tool phone pull-log DIR` runs `ioslog show` on
  the phone and pulls only the gzipped text (and writes the health and keyboard reports).
- Copying `data.img` from the phone is `oneios-tool phone pull-data OUT [--ioslog]` ("phone" below). Not there: an
  `Info.plist` in the saved archive (Apple's `log show --archive` wants one; Mandiant's tool does not).

### health: the counts that tell lag and heat apart

`ioslog health` (2026-11-15, the owner's request: every session shows the same lasting numbers) counts, for one
boot, the lines of iOS's own log that say that a main thread hung, a process was killed or a call timed out. One
line per kind: its count; for hangs the total seconds, the longest and how many were over 1 s; the top processes;
and, where the lines name something, those names.

```sh
oneios-tool ioslog health data.img                         # the last boot of a copy of the Data store
oneios-tool ioslog health data.img --detail                # under each kind: its texts, every process, examples
oneios-tool ioslog health data.img -o ioslog-health.txt    # the lines on stdout, the report with details in the file
oneios-tool ioslog health ioslog.txt                       # a text file that `ioslog show ... -o` wrote: every entry of it
oneios-tool ioslog health                                  # on the phone itself, as root: the staged data/data.img
```

Its output for the tests' log (`tests/ioslog/health.txt`, **written by hand, not a session**):

```
tests/ioslog/health.txt: the entries of a text file (ioslog show's), 42 entries, from 2026-11-15 09:00:00.000000Z to 2026-11-15 09:10:00.000000Z (600 s)
hangs              8  109.7 s in all, longest 61.27 s (Preferences), 5 over 1 s; Preferences 4 (64.9 s, longest 61.27 s, 2 over 1 s), SpringBoard 2 (40.1 s, longest 39.34 s, 1 over 1 s), MobileSafari 2 (4.8 s, longest 3.50 s, 2 over 1 s)
watchdog           5  chronod 3, SpringBoard 1, backboardd 1; named: com.example.clock.widget 2, com.example.weather.widget 2
xpc-timeouts       4  SpringBoard 3, Preferences 1; services: com.example.photos.service 2, com.example.settings.service 1
jetsam             4  MobileSafari 2, Preferences 1, nfcd 1
crashes            3  chronod 2, Preferences 1
stuck-idle         3  profiled 2, coreauthd 1
encode-fail        3  chronod 2, SpringBoard 1; errors: kCMBaseObjectError_UnsupportedOperation [-12782] 2, -16993 1
audio              6  started 0, webcam 1, failed at 1, failed 1, status 1, not ready 2; audiomxd 6; failures: RoutingSettings_N71.cpp:1196 1, webcamRecordingDSPList && "Need valid DSPConfig for webcam recording" 1
```

- **The head:** the source (for an image with its volume, as the other commands name it), the boot (`boot UUID`,
  `all boots`, or the entries of a text file), the entries read, the first and the last entry's time and the seconds
  between them.
- **A kind's line:** `NAME COUNT`, then, separated by `; `: for a kind whose lines carry seconds `T s in all, longest
  L s (PROCESS), N over 1 s`; the processes, the largest count first (`NAME COUNT`; for hangs `NAME COUNT (T s,
  longest L s, N over 1 s)`; beyond `--top N`, default 5: `and N more with COUNT`); and for a kind filed by the
  process that logged the line, when its lines name something: `named:`, `services:` or `errors:` and those names.
  A kind whose texts are steps of one thing (`audio`) gives each text's count first (`started 0, webcam 1, ...`), so
  that a step that never came shows as 0. A kind with no line is `NAME 0` (with those counts for such a kind). The lines are not indented;
  everything else below the head is.
- **`--detail`** adds, indented under each kind: `counts:` what the kind means; `first T, last T`; `texts:` each
  text's count (for a kind of several texts filed by the logger also who logged it: `extension 3 (chronod 3)`);
  `by process:` every process; the names; `logged by:` for the kernel's kinds; and `e.g.` example lines
  (`--examples N`, default 3). **`-o FILE`** writes the report with the details to FILE, whatever stdout shows.
- **A count is a number of log lines, not of events.** One event may be logged more than once: RunningBoard's exit
  status of one process is logged by every process that watches it (in the PC boot's log SpringBoard has one exit
  twice, as `Firing exit handlers for PID` and as `[osservice<NAME>:PID] Process exited`). So compare a count with
  the same count of another session, and read `texts:` for each text apart. A count is what `ioslog show SOURCE
  --boot last --proc NAME --grep TEXT | grep -c TEXT` gives for that process and text.
- **`--boot`**: `last` (default), `all`, or the start of a UUID, as `checks`. A text file names no boots: all of
  its entries are counted (`--boot UUID` is refused: choose the boot with `ioslog show --boot` when writing it).
- **SOURCE** is what every `ioslog` command takes, or a text file of `ioslog show` (`-f text`; a file whose first
  line is an entry). In the text form `[SUBSYSTEM:CATEGORY]` is taken off before the message is matched (a message
  that itself starts with `[word:word] `, in an entry without a subsystem, loses that start: the text cannot tell
  the two apart), and the times are the text's, cut to the microsecond. On the PC boot's Data store the report from
  the image and the one from `ioslog show`'s text of it are the same below the head (97,417 entries each).
- **Without SOURCE** (the form for the phone itself; the tool builds for Android arm64, "For the phone itself"
  above): `STAGE/data/data.img`, with `--stage DIR` (default `/data/local/tmp/oneios`). It only reads. Beside an
  open session (`STAGE/session/xnuhost.pid` names a running xnuhost) it says on stderr that the store is being
  written: what is counted is the newest checkpoint that passes the reader's checks, and what iOS logged last may
  not be in it yet. **Not run on the phone yet** (`ioslog boots` and `show` were, 3.1 s and 4.0 s; `health` is
  one more pass of the same decoder). With `health` in it the tool builds for `aarch64-linux-android` on Linux
  x86_64 (2026-11-15: NDK 28.2, API 30, the command above with `linux-x86_64`; a PIE for `/system/bin/linker64`,
  13.9 MB stripped); this box has no Android to run it on.

The kinds are one table, `KINDS` in `crates/oneios-ioslog/src/health.rs`:

| Kind | Texts (regular expressions on the message; a line counts once per kind, under the first that matches) | Filed by | Also |
|---|---|---|---|
| `hangs` | `Hang detected: N s` (HangTracer, `[com.apple.hangtracer:]`; `0.52s` and `0.52 s`) | the process that logged the line: the one that hung | total seconds, the longest and who, how many over 1 s (1.00 s is not over) |
| `watchdog` | `RBSProcessExitStatus\| domain:watchdog` (`exit status`), `Extension process termination watchdogged` (`extension`), `watchdog fired` in any case (`fired`) | the process that logged the line | `named:` RunningBoard's name of the process at the line's start (`[xpcservice<NAME(...)>:PID]`), else the first reverse-DNS name in the line |
| `xpc-timeouts` | `Initial XPC call to server timed out` (`initial call`), `Default XPC timeout` (`default`), `XPC timeout` (`timeout`) | the process that logged the line: the client | `services:` the first reverse-DNS name in the line |
| `jetsam` | `memorystatus: killing_idle_process` (`idle exit`: a clean daemon ended while idle), `memorystatus: killing` (`kill`: every other reason) | the process the line names (`pid N [NAME]`, `process N [NAME]`) | |
| `crashes` | the kernel's `NAME[PID] Corpse allowed N of 5` (`corpse`), `NAME[PID] Corpse failure, too many` (`too many corpses`) | the process the line names | |
| `stuck-idle` | `timed out stuck process` (`memorystatus_perform_idle_demotion() timed out stuck process PID [NAME], moving to idle band`) | the process the line names | |
| `encode-fail` | `CMPhotoCompressionSessionAddImage ... err =` | the process that logged the line | `errors:` what follows `err =`: a name of up to three words with its number (`kCMBaseObjectError_UnsupportedOperation [-12782]`, `unknown error [-12900]`), else one word (`-16993`). Until 2026-11-22 a name of more words was cut to its first (`unknown`) |
| `audio` | VirtualAudio's start in audiomxd (2026-11-23: no sound, volume or haptics in the v0.1.16 boot): `Ending Deferred Initialization` (`started`; 0: VirtualAudio never started), `Creating subport config for webcam recording` (`webcam`: made only when AVFCapture's AVGestalt answers its webcam key, `docs/hardware.md` 7), `FILE:LINE  PRECONDITION FAILURE` (`failed at`), `Precondition failure: TEXT` (`failed`), `VA Init Status: N` with N not 0 (`status`), `VirtualAudio PlugIn is not initialized yet` (`not ready`: each call refused after a failed start) | the process that logged the line (audiomxd) | `failures:` the file and line of a failed precondition and its text; the line gives each text's count (`texts_in_line`) |

**To add a kind, add a row** (name, what it means, the texts, `By::Logger` or `By::Named`, what the names are, the
expressions that find a name, the "over" limit, whether the line gives each text's count) and its lines to `tests/ioslog/health.txt` with their numbers in
`health_tests.rs`: the counter, the report, `cycle` and `summary.md` take the table as it is. A text may hold the
named groups `s` (a number of seconds: the kind then gets a total, a longest and an "over" count) and `who` (the
process, extension or service the line names).

Where the texts are from, and what is a guess:
- **Seen in a real log** (the Data store that iOS wrote on the PC, read on this Linux box): the `stuck-idle` line,
  the kernel's `Corpse allowed` line, RunningBoard's `[osservice<com.apple.chronod>:87] Process exited:
  <RBSProcessExitContext| specific, status:<RBSProcessExitStatus| domain:signal(2) code:SIGABRT(6)>>`. That log (one
  boot, 192 s, 97,417 entries): hangs 0, watchdog 0, xpc-timeouts 0, jetsam 0, crashes 1 (chronod), stuck-idle 26
  (24 processes), encode-fail 0; `grep -c` on `ioslog show`'s text gives the same 26 and 1.
- **The owner's quotes from the phone's log** (2026-11-15; the log is on the Mac): the hang text, the three
  watchdog texts, the three XPC texts, the CMPhoto text. `docs/hardware.md` 7g quotes the CMPhoto line whole
  (session 8644ae9, chronod, 58 times): `writeImageAtIndex: *** CMPhotoCompressionSessionAddImage: err =
  kCMBaseObjectError_UnsupportedOperation [-N] (codec: 'hvc1')`.
- **Guesses**, written without those lines at hand: where a watchdog or an XPC timeout line names its extension or
  service (`named:` and `services:` may be empty, or name something else in the line: the per-process counts do
  not depend on it); the forms of the jetsam line (XNU's source: `killing_idle_process pid N [NAME]`,
  `killing_specific_process`, `killing_top_process`, `killing process N [NAME]`; no jetsam kill is in the PC
  log). Crashes are the kernel's corpse lines only, so that a crash counts once (ReportCrash's and launchd's
  lines of the same crash are not counted; a crash without a corpse is not counted).

Where it runs by itself: `cycle`'s `collect` step and `phone pull-data --ioslog` write the report with its details
to **`ioslog-health.txt`** (the session's folder, or beside the copy) and print the head and the kinds' lines;
`summary.md` has them too ("cycle" and "phone" below). For another caller (the app's `serve`, branch
`tool-app-progress`): `health` is a plain subcommand, and `oneios_ioslog::health` is public (`read` gives a
`Report` with each kind's `Count`: the tallies by logger and by name; `write` is what `cycle` calls; `summary_of`
takes the kinds' lines out of a saved report). It has no progress or `serve` code of its own.

Tested (Linux x86_64, 2026-11-15): `cargo test -p oneios-ioslog` 11 tests (7 new in `health_tests.rs`: the table,
the hand-written log's counts per kind, process, name and text, the lines byte for byte, a log with nothing in it,
the text form's parsing, the sources and the stage form, the command line), `cargo test -p oneios-tool --test
ioslog_health` 1 test (the binary: stdout, `--detail`, `-o`, `--top`, `--examples 0`, the form without SOURCE, the
exit statuses 0, 1 and 2); `cargo test --release` for the whole workspace: 142 tests pass, 0 fail (these 8 are
new). The stand-in suites, `tests/phone/modes.sh` (with origin/main at `fb4ec88` merged in):
`host` and `stand-in` mode each **phone 232** checks (222 before), **app 103**, **cycle 122** (117 before), all
ok, no stand-in session left. The ten new phone checks: `pull-data --ioslog` on the stand-in's `data.img`, which
is no APFS store (it ends at `ioslog boots`, the copy stays, no `ioslog-health.txt`); `ioslog health` without
SOURCE (the stage's own `data.img` is the one read; a stage without a store is refused by name; on a stage whose
`data.img` is the hand-written log: the head and eight lines (seven before `audio`, 2026-11-23), the counts per kind and process, nothing on stderr;
`--top 1 -o FILE`); beside a stand-in session (it reads, and says on stderr that the store is being written; a pid
file of a process that is gone is no session). The five new cycle checks: `cycle --only collect` in a folder that
holds the hand-written log as `ioslog.txt` (the head and seven lines printed; `ioslog-health.txt` with the
details; `summary.md`'s section with the seven lines and without the details), in a folder with neither a store
nor a log (one line that says so, status 0), and in the folder of the failed `--pull-data` case, whose `data.img`
is no APFS store (`collect: ioslog health: ...`, status 1, no report). **The stand-in phone's `data.img` cannot
be read as a store, so `pull-data --ioslog`'s and `--pull-data`'s health of a real copy are not run by the
suites**. Run by hand instead (2026-11-15): a stand-in phone (`tests/phone/lib.sh`'s `make_phone`) whose
`data.img` is a copy of the Data store iOS wrote on the PC, then `oneios-tool phone pull-data OUT --ioslog`:
status 0, the copy is the store byte for byte (32 GiB, 1.5 GB written), `ioslog boots` and `checks` ran, then
`--- ioslog health OUT`, the head (`boot FB412AD9..., 97417 entries, ... (192 s)`), the seven lines (crashes 1
chronod, stuck-idle 26, the rest 0) and `ioslog-health.txt` beside the copy (27 lines).
**Not tested: the phone's own store** (it is on the Mac). The check to run there, with the owner's numbers from
that log:

```sh
oneios-tool ioslog health out/session-logs/2026-11-15/main-8644ae9-b/data.img --detail -o out/session-logs/2026-11-15/main-8644ae9-b/ioslog-health.txt
# expected: hangs: Preferences 38 (251.1 s, longest 61.27 s, ...), SpringBoard 25 (70.8 s, longest 39.34 s, ...)
#           watchdog: chronod 84        xpc-timeouts: SpringBoard 114, Preferences 18
#           (docs/hardware.md 7g has chronod's CMPhoto line 58 times in session 8644ae9: encode-fail, if -b is that boot)
# a number that differs: `texts:` under the kind has each text's count and who logged it; the same count by hand is
#   oneios-tool ioslog show .../data.img --boot last --proc chronod --grep 'watchdog' | grep -c watchdog
```

### keyboard: the keyboard's path in one boot

`ioslog keyboard` (2026-11-22, the owner's report: typing lags, a key comes out twice, the keyboard goes away in
Spotlight, keys do nothing in the captive portal's sheet) counts, for one boot, the lines of the keyboard's path with the
same counter and report as `health` (`crates/oneios-ioslog/src/keyboard.rs`, its own table `KEYBOARD`; health.rs's
`Counter::with` and `read_with` take any table). Its options are health's. One line per kind:

| Kind | What it counts |
|---|---|
| `key-taps` | UIKit's `KeyboardTouch` `touch down` / `touch up`, by the app that had the keyboard (`--detail`: downs and ups apart, per app). More ups than downs is where doubled keys start |
| `hangs` | HangTracer's main-thread hangs, as in health: a key waits for the app's main thread |
| `asset-lock` | MobileAsset's short-term lock of an asset set that failed (`_shortTermOpenSharedLockFile} (SYMLINK) ... errno:2`); the names are the asset sets |
| `asset-query` | UnifiedAssetFramework's queries (`UAF.retrieveAssetSetWithToken`, `assetsForQuery: <query: NAME`); the names are the asset types |
| `language-model` | `LM model is not valid yet`, `Unable to load ... model`, `embedding generation timeout` |
| `kb-screen` | UIKit's `Keyboard screen not found for window` |
| `kb-placement` | the keyboard laid out again: `endPlacementForInputViewSet` and `Notifying observers of geometry update` |
| `text-input` | TextInputUI's requests of the app's input session (`TUIInputSessionProxy handleRequest`, `TUIKeyboardInputManagerMux forwardInvocation`) |

```sh
oneios-tool ioslog keyboard data.img                        # the last boot of a copy of the Data store
oneios-tool ioslog keyboard ioslog.txt --detail -o kb.txt   # a text file that `ioslog show -o` wrote; details in kb.txt
```

The first run, on the 2026-11-22 20:28 session (runtime 0.1.15, `ioslog show --boot last -o` of its Data store, 262923
entries): `key-taps 72` (WebSheet 66: 12 downs and 54 ups; Spotlight 6), `hangs 114` (434.5 s; WebSheet 48 with 321.3 s,
the longest 52.21 s; Spotlight 30 with 76.9 s; SpringBoard 30), `asset-lock 94` (kbd 57; asset sets
com.apple.linguisticdata 71), `asset-query 165` (kbd 112), `language-model 8`, `kb-screen 25` (Spotlight 21),
`kb-placement 481` (WebSheet 231, SpringBoard 137, Spotlight 77), `text-input 90`. As in health, a count is a number of
log lines. That run's times ran 52 times too slow (the library's timebase, fixed 2026-11-22 above: the boot lasted
47.6 min, not 55 s); the counts do not change. Tests: `cargo test -p oneios-ioslog` (keyboard_tests.rs: lines written by hand in the
shapes of that session's lines, no Apple file).

### haptics: the path of a haptic in one boot

`ioslog haptics` (2026-11-23) counts, for one boot, the lines of a haptic's path in the order the haptic travels, with
the same counter and report as `health` (`crates/oneios-ioslog/src/haptics.rs`, its own table `HAPTICS`). Its options
are health's. Each line gives every step's count, so a 0 is visible. The first step with a 0 is where the haptic stops.
`phone pull-log` writes it as `ioslog-haptics.txt`.

| Kind | Its steps |
|---|---|
| `feedback` | UIKit's feedback generators, by app: `asked` (a generator plays a feedback), `core haptics` (played by a Core Haptics engine, on to the Actuator), `system sound` (played as a sound: no haptic) |
| `keyboard-mode` | the keyboard's feedback mode: `haptics on`, `off in Settings` (iOS's default: Settings > Sounds & Haptics > Keyboard Feedback > Haptic), `unsupported` (no closed-loop answer: no `/product/haptics`) |
| `engine` | the app's Core Haptics engine: `made`, `prewarm`, `start`, `events` (an event array sent to the server: a haptic to play), `timed out` (the server did not answer: hosted 0078) |
| `server` | audiomxd's haptic server: `up`, `prewarm`, `start` (a client started with haptics), `haptics muted` (an audio-only client), `preset missing`, `gave up` (hosted 0078's faults) |
| `chain` | VirtualAudio's DSP chain of the Actuator's stream: `made`, `no strips file` (hosted 0086's fault), `no volume command` |
| `level` | audiomxd's meter (RTAID) at the Actuator's nodes (`Actuator-Output`, the port's `PreDSP`, `PostDSP`): `silent` (-120 dB), `with a level` |
| `device` | the guest's Actuator device (the kernel's `ONEIOS HAPTIC:` lines): `opened`, `I/O started`, `stopped with requests` (an I/O run with loud periods), `stopped silent` (a prewarm, or samples lost on the way) |
| `system-sound` | the system sound server's actions: `with vibration`, `sound only` |

How to read it: `feedback` with `asked 0` means nothing was asked for (touch a switch, press an icon long). `engine`
with `events 0` means no haptic was sent to the server: a `prewarm` plays nothing, and its I/O run is silent by design.
`events` above 0 with `level` only `silent` and `device` only `stopped silent` means the samples are lost between the
synth and the device (the DSP chain).

```sh
oneios-tool ioslog haptics data.img                        # the last boot of a copy of the Data store
oneios-tool ioslog haptics ioslog.txt --detail -o h.txt    # a text file that `ioslog show -o` wrote; details in h.txt
```


### The comparison

```sh
# the reference: Mandiant's own tool at the commit Cargo.lock pins
git clone https://github.com/mandiant/macos-unifiedlogs && git -C macos-unifiedlogs checkout 3d5b27a
cargo build --release --manifest-path macos-unifiedlogs/examples/Cargo.toml -p unifiedlog_iterator
export REF_ITER=$PWD/macos-unifiedlogs/examples/target/release/unifiedlog_iterator
JSONL=1 tools/oneios-tool/tests/ioslog/compare.sh ARCHIVE.logarchive
```

`compare.sh` (its header lists the steps) checks, for one archive: (1, 2) `ioslog show ARCHIVE` in CSV and JSON
lines against `unifiedlog_iterator` on the same archive, with `cmp`; (3) a Data-store image made from the archive's
files by `oneios-tool debug mkapfs` (`tests/ioslog/mkapfs.py` until 2026-11-21; three volumes, the log in the third as `db/diagnostics` and `db/uuidtext`, block
0 one transaction behind, files over 8 blocks in two extents), which **apfs-fuse must read as the folder it was made
from** before the image is trusted; (4) `ioslog save IMAGE` against the manual copy out of apfs-fuse's mount, with
`diff -r`; (5) `ioslog show IMAGE` against `unifiedlog_iterator` on that manual copy; (6) the same from the mounted
volume; (7) `--block0` must find no log; (8) ten filter combinations, `--sort`, the text output and `checks`
against the same selection made from the unfiltered CSV by rules written apart from ioslog's (`oneios-tool debug
ioslog-filters`; `tests/ioslog/filters.py` until 2026-11-21). The JSON lines of
step 2 are the same to the byte because the archive's files have the same paths for both tools; from an image the
`evidence` field names `IMAGE:/db/diagnostics/...` instead (the CSV has no such column).

Result (Linux x86_64, 2026-11-12, Mandiant's test archives of BUILDING.md; every step the same for each):

| Archive | Entries | CSV bytes | `unifiedlog_iterator` | `ioslog show` (archive) | `ioslog show` (image) | Image: files, `save` |
|---|---|---|---|---|---|---|
| macOS 10.13 (high_sierra) | 569,796 | 238,965,139 | 2.5 s | 1.5 s | 0.9 s | 447 files, 0.04 s |
| macOS 11 (big_sur) | 747,616 | 353,341,596 | 3.5 s | 2.8 s | 2.4 s | 554 files, 0.3 s |
| macOS 11, private data on | 887,890 | 423,230,870 | 4.3 s | 2.8 s | 3.2 s | 577 files, 0.5 s |
| macOS 11, public and private mix | 1,287,628 | 615,105,718 | 8.8 s | 4.4 s | 3.8 s | 572 files, 0.3 s |
| macOS 12 (monterey) | 2,397,109 | 1,176,616,820 | 16.6 s | 9.4 s | 8.3 s | 1,687 files, 1.4 s |
| macOS 26 (tahoe) | 4,288,584 | 2,066,088,069 | 27.7 s | 20.0 s | 13.2 s | 910 files, 2.5 s |
| macOS 27 (tart_goldengate; the format of iOS 27) | 4,754,401 | 2,166,781,973 | 30.8 s | 20.0 s | 19.4 s | 1,048 files, 0.9 s |

The image has no `logdata.LiveData.tracev3` (only `log collect` writes one), so its entry count is lower than the
archive's. Run again on all seven after the third round's changes to the reader (compressed files, attributes kept
in streams, the apfs-fuse backend): every step the same, 140 checks. The box ran two other agents' jobs: the times are not clean.

`ioslog` decodes on one thread. Decoding the tracev3 files side by side was tried (2026-11-12) and is not in
the tool: the library's per-file function needs its oversize cache, whose module is private, its string storage is
made for one thread, and oversize payloads are looked up across files, so the files cannot be decoded
independently from outside the library without changing the output ([todo_rust.md](todo_rust.md) section 5 has
the details and what upstream would have to open).

**What this does not show**, because neither the phone's Data store nor the Mac's files are on the Linux box:

1. `ioslog` on the phone's own `data.img`. Its first run there (2026-11-13, on the Mac) failed with `short read
   at 1152903912689233920`; the cause and the fix are in "The APFS reader" below. The fixed reader was run on a
   Data store that iOS 27's own APFS wrote on the PC (the same failure before, `ioslog boots` after), on stand-ins
   with a cut-off newest checkpoint, and on the images below; **the phone's store itself has not been read again**
   (it stays on the Mac). To run there: `oneios-tool ioslog boots data.img`, `ioslog checks data.img`, and if
   either refuses, `oneios-tool image apfs-checkpoints data.img` (it prints no file name and no file contents:
   transactions, block numbers, object types, volume names).
2. `ioslog show` against the Mac's `out/session-logs/2026-11-12/ios3-log.csv` (made from `ios3.logarchive`). To
   run it there: `tests/ioslog/compare.sh out/session-logs/2026-11-12/ios3.logarchive`, and
   `oneios-tool ioslog show ios3.logarchive -f csv -o X && cmp X ios3-log.csv` (the same only if `ios3-log.csv` was
   made by `unifiedlog_iterator` of commit `3d5b27a`; version 0.7.0 formats some messages differently).
3. `checks` on a real session's log.

### The APFS reader

Our reader (`crates/oneios-image/src/apfs.rs`, `apfs/files.rs`, `apfs/check.rs`) is the default everywhere; apfs-fuse is the
independent reader it is checked against, and since the third Linux round also an optional second backend.

**Checkpoints: checks before every read, and the fall back** (`apfs/check.rs`, 2026-11-13). The first run on the
phone's real Data store failed: `ioslog boots` and `ioslog checks` ended with `short read at 1152903912689233920`.
That number is 0x0ffff0000ffff000, block 0xffff0000ffff. **The cause**, found again on a Data store that iOS 27's
own APFS wrote on the PC (`image apfs-info` of it gave the same number to the digit): before `ioslog` looked at
any checkpoint it counted the volumes with imagetool.py's reader, which reads **block 0's** superblock. On a used
store that superblock is old (transaction 14 on the PC's store, whose newest checkpoint is 198), and the block it
names as the container's object map has long been written again: it was a node of the extent reference tree by
then (`type 0x40000003/0xf, oid 8257812, xid 191`). A tree node's two empty free lists (`ff ff 00 00 ff ff 00 00`)
lie at offset 48, where an object map has its tree's address: read as that address they are block 0xffff0000ffff,
and the read of it was the "short read". The copy was good, the reader was wrong: nothing checked what a block
was before a field of it was used. Now:

- **Before a block is read**, its address is checked against the container's block count and against the file's
  length: the address of an object, a tree node's child, an object map's value, and a file extent's start and
  length. **Before a field of an object is used**, its header is checked: the Fletcher-64 checksum (`o_cksum`), the
  type (`o_type`), the object id (`o_oid`: a physical object's is its own address, a virtual one's the id it was
  looked up by, the container superblock's is 1), and a transaction (`o_xid`) that is **not newer than the
  checkpoint's**: a newer one means the block was freed and written again after that checkpoint. A tree node must
  also be the root or a child as its parent says, on the level below its parent, with its table and every key and
  value inside the node. What fails is **refused by name**: `refused the container's object map at block 8257812
  (0x7e0114) [type 0x40000003/0xf: a tree node, oid 8257812, xid 191], named by block 0: it is a tree node, not the
  container's object map`. No command ends with a bare short read any more.
- **The checkpoint.** Block 0 is taken only for where the checkpoint descriptor area is (`nx_xp_desc_base`,
  `nx_xp_desc_blocks`) and for the container's UUID. Every container superblock of the area is a candidate, tried
  from the highest transaction down (block 0's own superblock too, when the area has no good one of its
  transaction). A candidate passes when its superblock (checksum, type, oid 1, magic, block size, the same
  container UUID, the last block of its own run in the area), its checkpoint map blocks (each: checksum, type, its
  superblock's transaction, the last one marked last, every address inside the container) and the container's
  object map with **every node of its tree** pass. This is the order of Apple's "Mounting an Apple File System
  Partition" ("the container superblock that has the largest transaction identifier and isn't malformed").
  apfs-fuse (`ApfsContainer::Init`) takes the superblock with the highest transaction and a good checksum and
  goes no further back; this reader does, when an object below that superblock is refused. What fsck_apfs does was
  not looked at (no Mac on the box).
- **The volume.** For the volume asked for, at the newest candidate that passes: its superblock, its object map
  with every node of its tree, **every node of its file-system tree** (each child reached through the volume's
  object map, as a lookup reaches it), a sealed volume's file extent tree, and the roots of the extent reference
  tree and of the snapshot metadata tree. If one is refused, an older mapping of the volume in the same
  checkpoint's object map is tried when there is one (the format allows it; a real container's object map is
  `OMAP_MANUALLY_MANAGED`, has no snapshots and keeps one mapping per volume), then the next older checkpoint. So
  one volume can be read at an older checkpoint than the others. The used transaction and every candidate that was
  skipped, with its reason, are in the reader's result and on stderr; when none passes, the error lists each
  candidate with its reason.
- **Not checked:** file contents (APFS has no checksum of them; an extent is checked against the container when it
  is read), the ephemeral objects a checkpoint map names (the space manager, the reaper: never read here), the
  nodes of a sealed volume's hashed tree against their hashes (they carry no header: 32 zero bytes and
  `BTNODE_NOHEADER`, the root too; their shape is checked), the extent reference and snapshot metadata trees
  below their roots.
- **Which commands.** `ioslog` (every command), `image apfs-ls` and `apfs-copy` with `--newest`, and
  `image apfs-checkpoints` choose a checkpoint this way; `--checkpoint XID` takes one (and refuses it by name if it
  does not pass). `--block0` and `apfs-ls`/`apfs-copy` without `--newest` read block 0's state with the same
  checks, each object when a read reaches it (one candidate: nothing to choose). `image apfs-info`, `apfs-owners`,
  `tree-copy --owners-from` and `manifest` stay imagetool.py's reader, without these checks: they read the set's
  read-only images, which are written once, and must print what the Python tool prints.

```sh
oneios-tool image apfs-checkpoints data.img     # every checkpoint: does the container pass, does each volume, and why not
```

**The tests of it** (2026-11-13, Linux x86_64; no Apple file and no owner data in them). The stores:
`tests/support/apfs_store.rs` (Rust, for `cargo test`) takes two stores of `mkapfs.rs` (beside it; `mkapfs.py` until 2026-11-21), an older and a newer
state of the same three volumes (Preboot, xART, Data), and grafts them into one container with two checkpoints:
the older one stays where it is as transaction 5 (and block 0), the newer one's blocks follow it with every
address moved (object ids of physical objects, object map values and child addresses, the volumes' object map
addresses, file extents) and every checksum written again, and its superblock and checkpoint map become
transaction 6 in the second place of the descriptor area. Where the objects of each checkpoint are is read by a
walk of its own (not the reader's), so a test damages an object by name. apfs-fuse reads the grafted store as its
newer state (`ONEIOS_TEST_FUSE=1`, below). `crates/oneios-image/tests/apfs_checkpoints.rs` (5 tests, the reader)
and `crates/oneios-tool/tests/ioslog_checkpoints.rs` (1 test: the binary's `ioslog show -f csv`, `boots`, `checks`
and `save` on each store against the same four commands on the folder of the state that must come out, and what
stderr says):

| Case | The damage to the newest checkpoint (transaction 6) | Result |
|---|---|---|
| good | none | transaction 6 is read; `--checkpoint 5` and `--block0` give the older state |
| a | the superblock: one bit changed, or zeros from byte 64 on; its checkpoint map: a pattern from byte 512 on | transaction 5 is read; `1 newer of 2 skipped: xid 6 at block 4: refused the container superblock ... its checksum is wrong` |
| b | the superblock whole (checksum written again), its object map address 0xffff0000ffff; the object map whole, its tree's address 0xffff0000ffff; a tree node copied over the object map (the phone's case: its free lists are that address); address 0 | transaction 5; `refused the container's object map at block 281470681808895 (0xffff0000ffff), named by block 4: the address is past the container's end`, or `it is a tree node, not the container's object map` |
| c | half written, zeros or the pattern `ff ff 00 00` from an offset on (0, 64, 100, 600, 1024, 2000, 2048, 3000, the last 16 bytes): a node of the container's object map tree, the Data volume's superblock, its object map, a node of its object map tree, the root and a leaf of its file-system tree | transaction 5 for the Data volume, each refused by name with its type, oid and xid. When only the Data volume's objects are damaged, Preboot and xART are still read at transaction 6 |
| whole, and wrong | a leaf with a later transaction (9), another object id, a wrong level, an entry outside the node; an object map value past the container; a superblock with another container's UUID; one that is not the last block of its checkpoint; the file cut off inside the newer checkpoint's blocks | transaction 5, each with its reason (`its transaction 9 is newer than the checkpoint's (6)`, `the block is past the file's end`, ...) |
| history | a second mapping of the Data volume in transaction 6's object map (to transaction 5's volume superblock), and its newest tree damaged | checkpoint 6 is read with the volume's older state: `a newer state of the volume in this checkpoint was skipped` |
| d | every candidate: transaction 6's superblock, and transaction 5's object map (which block 0 names too) | status 1, one error: `none of the 3 container superblocks can be read:` and a line per candidate with its reason. With the Data volume damaged in both checkpoints only: `volume 2 cannot be read at any of the 2 checkpoints:`, the other volumes are read |
| at a read | a file extent whose block is 0xffff0000ffff (the node whole); a damaged leaf in block 0's state read with `--block0` | the store opens; the read is refused by name (`refused an extent of data stream N: ... it runs past the container's end`, `refused the node of the file-system tree ...`) |

No message of any case holds "short read". `cargo test` makes the log files as stand-ins (header chunks: no
entries), so there it is the files of `save` and the equal answers of the commands that are compared. With a real
archive the log itself is: `ONEIOS_TEST_LOGARCHIVE=system_logs_high_sierra.logarchive ONEIOS_TEST_FUSE=1
ONEIOS_TOOL=target/release/oneios-tool cargo test -p oneios-tool --test ioslog_checkpoints` (Mandiant's macOS 10.13
archive; the older state lacks the last of `Persist`'s three files): the nine stores of the cases above give the
CSV of the state that must come out to the byte (84,856,013 bytes for the older state, 116,141,721 for the newer),
the same `boots` and `checks`, the same 446 or 447 saved files, each command says the transaction it read; and
apfs-fuse reads the good store as its newer state.

**On stores and images written by Apple's APFS** (2026-11-13):

| Image | Result |
|---|---|
| A Data store that iOS 27's own APFS wrote on the PC, its kernel killed (32 GiB, 1.5 GB written; one volume, `Data`, in place 2; 94 checkpoints, transactions 105 to 198; block 0 at transaction 14) | The binary from before: `image apfs-info`, `ioslog boots`: `short read at 1152903912689233920`, the phone's number. Now: `ioslog boots` reads transaction 198 (`the newest of 95`), 1,746 tree nodes checked, one boot of 97,417 entries, 0.4 s of CPU. `apfs-checkpoints`: the ten newest checkpoints pass; at four older ones the container passes and the block named as the volume's object map is a tree node now; the other 80 and block 0 fail at the container (78: the block named as its object map is a tree node now; 3: that block is of a later transaction). `NEWEST=1 tests/image/apfs-read.sh data.img 2`: **7,213 files and links the same as apfs-fuse's (7,211 files, 1,490,636,746 bytes), 0 differ**. `--block0` is refused by name there (what block 0 names is gone) |
| 24A446 OS image (sealed, 10.4 GB) | `apfs-checkpoints`: both checkpoints pass, 56,870 tree nodes checked, 1.3 s. `apfs-info` and `apfs-owners private/var` (imagetool.py's reader) print what the binary from before prints (332 and 2,765,028 bytes) |
| 24A446 SystemOS image (sealed) | `apfs-read.sh` without and with `NEWEST=1`: 2,194 the same (2,192 files, 7,462,414,742 bytes), 0 differ, none skipped, as before; 7,233 nodes checked with `--newest`; `apfs-fuse.sh` with `ioslog` on a two-checkpoint store: 16 checks ok |
| 24A446 AppOS image (sealed) | `apfs-read.sh` without and with `NEWEST=1`: 1,155 the same, 0 differ, as before; `apfs-fuse.sh` ok |
| dfvfs's `apfs.raw` | 8 the same, without and with `NEWEST=1`, as before |

`tests/image/parity.py` (the image tool against imagetool.py on stand-ins; both deleted on 2026-11-21): 138 cases, all the same.
`cargo test` of the workspace: 92 pass, among them `oneios-image` 31 + 1 (`apfs_files.rs`) + 5
(`apfs_checkpoints.rs`) and `oneios-tool` 1 (`ioslog_checkpoints.rs`); 4 of `oneios-kernel`'s `tests/golden.rs`
fail on this box with and without this change (their files under `tests/kernelgen/golden/build/` are not in a
fresh checkout: `.gitignore` has `build/`). The seven archives of "The comparison" above were run again with this
reader: every step the same, 140 checks.
**Not done:** the phone's own store (on the Mac); `tests/image/compare-python.sh` on the real set (the Linux box's
set is not where the script looks; `apfs-info` and `apfs-owners` of its three images were compared with the
binary from before instead, six outputs the same to the byte); macOS (no Mac on the box).

**Compressed files (decmpfs)** are read since the third Linux round (`apfs/decmpfs.rs`): the inline kinds and the
resource-fork kinds of zlib (3, 4; `flate2`), LZVN (7, 8), stored (9, 10), LZFSE (11, 12; `lzfse_rust`) and
**LZBITMAP (13, 14; the `zbitmap` crate)**, which is what iOS 27 uses for most files (869 of the AppOS image's 930
compressed files). No decoder is ours: a decmpfs LZVN stream is a bare LZVN payload, which `lzfse_rust` decodes
once it is given the 12-byte header of an LZFSE stream's `bvxn` block and the end mark. A resource fork is an
attribute kept in a data stream of its own, which the reader now follows; a compressed file's size is its plain
size (from the attribute's header), as apfs-fuse shows it.

`tests/image/apfs-read.sh IMAGE [VOLUME]`: `apfs-copy` copies the volume, apfs-fuse mounts it, and every file
must be byte for byte the same, with the same names on both sides (Linux x86_64, 2026-11-12):

| Image | Result |
|---|---|
| dfvfs's `apfs.raw` (macOS, unsealed, 1 volume) | 8 files and links the same, with and without `--newest` (the round before; no compressed file) |
| 24A446 AppOS image (sealed) | **1,155 the same, 0 differ, none skipped** (930 were kept compressed); before decmpfs: 225 the same, 930 skipped |
| 24A446 SystemOS image (sealed; the dyld shared cache) | **2,194 the same (2,192 files, 7,462,414,742 bytes), 0 differ, none skipped** (1,980 were kept compressed, the shared cache's `.symbols` and `.dyldlinkedit` among them); `apfs-copy` 43 s; before: 214 the same, 1,980 skipped |

So all 2,910 compressed files of the two images come out as apfs-fuse gives them. Not checked: an image whose
files use the zlib resource-fork kind (4) or LZVN in the resource fork (8) for real (the unit tests make such
files; which kinds besides LZBITMAP the two images hold was not counted), and the OS image's volume (`os.img`).

**The apfs-fuse backend** (Linux; optional): `--apfs own|fuse|auto` on `image apfs-ls`, `image apfs-copy` and every
`ioslog` command. `own` is our reader, `fuse` mounts the volume read-only with `apfs-fuse -o ro,vol=N` on a new
folder under the temporary folder and reads the files there, and `auto` (the default) takes our reader and falls
back to apfs-fuse only when ours refuses the image or the volume, saying so on stderr (for `ioslog`: when ours can
read neither the container nor any volume; a volume it reads and that holds no log is an answer, and `--block0`
never falls back).
The mount is always taken down again (`fusermount3 -u`, else `fusermount -u`): at the end, after an error, and on
Ctrl-C or SIGTERM (a lazy unmount from the signal handler, then exit status 130). apfs-fuse is GPL-2.0: it is
started as a program and never linked. With `fuse`, `--volume` is a number (or a name our reader can look up), and
`--newest`/`--block0` have no meaning (apfs-fuse reads the container as it finds it).

```sh
tools/oneios-tool/tests/image/apfs-fuse.sh IMAGE [VOLUME [FOLDER]]     # IOSLOG_IMAGE=FILE adds ioslog with both readers
```

Result (2026-11-12, the SystemOS image, `System/Library/CoreServices`, and a Data-store stand-in made by
`mkapfs.py` from the macOS 10.13 archive; the AppOS image the same): `apfs-ls` of the root and of a folder and
`apfs-copy` of a folder give the same text and files with both readers; a file that is not APFS and a folder that
is not there end with status 1; with our reader made to refuse (`ONEIOS_APFS_TEST_REFUSE=1`, a hook for this
test), `auto` answers through apfs-fuse and `own` does not; Ctrl-C in a copy of the whole volume ends with status
130; `ioslog show -f csv` is the same 116,141,721 bytes with both; and after each of these nothing is left
mounted and the mount's folder is gone. 16 checks, all ok.

## image

`oneios-tool image CMD ...`: the commands, options, output and exit status of `scripts/image/imagetool.py`
(`docs/image-format.md`). In Rust: `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`,
`manifest`, `verify` of a set folder, and `im4p`. `check-ipsw`, `fetch`, `record-plain`, `aea-info`, `pack` and
`verify` of a `.tar.zst` start the Python tool (`IMAGETOOL_PY` names it). Only here:

```sh
oneios-tool image apfs-ls IMAGE [PATH] [--volume N|NAME] [--newest] [--apfs own|fuse|auto]      # a folder's entries: mode, owner, size, name
oneios-tool image apfs-copy IMAGE SRC DST [--volume N|NAME] [--newest] [--apfs own|fuse|auto]   # a file or a folder out of the image, no mount;
                                                                                                # compressed files come out plain
# both: --newest reads the newest checkpoint that passes the checks, --checkpoint XID that one (a used store; said on stderr)
oneios-tool image apfs-checkpoints IMAGE     # a used store's checkpoints: at each, does the container pass, does each volume ("The APFS reader")
oneios-tool image aea-decrypt IN OUT [...]   # an AEA image of the IPSW to its plain file, the key from Apple's key service ("aea-decrypt" below)
oneios-tool image apfs-check IMAGE [PATH] [-v]   # every compressed file read as iOS reads it; exit 1 names the ones iOS cannot read ("apfs-check" below)
oneios-tool image udif-raw IN OUT [--sha256 HEX] # a UDIF .dmg's Apple_APFS partition as raw APFS (iOS 16's OS image; "udif-raw" below)
```

**`apfs-copy` in threads (2026-11-15).** Our reader's copy lists the folders and reads the files' sizes in every
thread of the pool, makes the folders in order, then reads, decodes and writes the files in every thread (one
file is one thread's; at most 1 GiB of file contents is held in memory over all threads, and a larger file is
copied alone), then makes the links. Before, one entry was copied after the other. The result is the same: the
same files, bytes, links, counts and messages (a message about a compressed file that could not be decoded comes
at the end, in the old order). `ONEIOS_APFS_COPY=serial` runs the old path, as does a volume that names one path
twice. `ONEIOS_IMAGE_TIMING=1` prints the scan's and the files' time. Measured on the Linux PC (4 CPUs, other
agents' builds running: each run had a CPU weight of its own, best of 2 or 3 runs), 24A446's `os.img`:

| Copy | Before (one by one) | `--jobs 1` | `--jobs 2` | `--jobs 4` | The same output |
|---|---|---|---|---|---|
| `/usr/libexec` onto tmpfs (644 files, 486,030,196 bytes, 637 kept compressed) | 1.45 s, 98% CPU | 1.39 s | 0.75 s, 184% | **0.59 s, 248%** (2.5 times) | yes: `diff -r` of each tree against the one the binary before gives; the SHA-256 over every file's SHA-256 `c6450f46...` both |
| `/usr` onto the disk (2,658 files, 1,208,652,431 bytes, 2,505 kept compressed; SATA SSD, btrfs) | 3.15 s, 76% CPU | 2.83 s | 2.23 s | 2.65 s | yes: the same check, `20142944...` both |

On that PC's disk the copy waits for the disk to take the bytes (its kernel lets 256 MiB wait to be written), so
more threads gain little there; on tmpfs the threads are what counts. **Not measured on the phone.**
`tests/image/apfs-read.sh` (the copy against apfs-fuse, an independent reader) and
`crates/oneios-image/tests/apfs_copy.rs` (the threads' copy against the one-by-one copy on a made image, with 1, 2
and 8 threads, parts of a volume, and a stop) check it.

`im4p IN OUT` decodes the LZFSE payload with `lzfse_rust` (imagetool.py starts the `lzfse` program). On
`kernelcache.release.v54` of 24A446: 78,135,296 bytes, sha256 `349c4692...0c57e3`, which is the hash the kernel
profile pins (made with the `lzfse` program on the Mac), in 0.7 s. Not done: the two decoders side by side on one
host (the Linux box has no `lzfse` program).

### apfs-check: compressed files as iOS reads them (2026-11-23)

```sh
oneios-tool image apfs-check IMAGE [PATH] [--volume N|NAME] [--newest] [-v]
# IMAGE: 258055 files, 228550 kept compressed, all readable                       exit 0
# imagetool: IMAGE: 45044 files, 40179 kept compressed, 8340 that iOS cannot read:  exit 1, then the first 20:
#   /usr/lib/dyld: kind 4: invalid resource map: the map (50 bytes) is not Apple's: 6 bytes differ, from byte 2 (...)
# -v: each compressed file's layout on stderr (kind, sizes, the resource fork's header and map, or its chunk table)
```

Every compressed (decmpfs) file below PATH, in every thread: its layout as iOS's reader (AppleFSCompression) needs it,
then the whole decode to its plain size (`ApfsTree::compressed_problem`, `apfs/decmpfs.rs` `check_layout`). Our
decoder reads only the place of a resource fork's data, so a file can decode here and not on iOS. The layout check:
a kind 4 (zlib) fork's header, its data inside the fork, Apple's 50-byte resource map **byte for byte**, cmpf's chunk
count for the plain size and each chunk inside cmpf; the other fork kinds' chunk table (first offset just after it,
rising, not past the fork). `image verify` of a set folder runs it on each read-only volume (below).

**Why (OneiOS issue 73).** An A56 user's first start ended with `ChunkCompression.cpp:415: /usr/lib/dyld: invalid
resource map`, then `newfs_apfs did not run: 13` and the panic `offline scratch Data mount failed: 13`. The image was
the other A56 fork's (`Musicalista/OneiOS-A56` v1.0.0, `ios16.5-a56-image.tar.gz.part00/01`), whose `os.img` (2.47 GB,
an unsealed volume `System`) and `systemos.img` (12.5 MB) its own APFS writer made. Read on the Linux PC, 2026-11-23:

| Image | Compressed files | iOS cannot read | Time (20 CPUs) |
|---|---|---|---|
| 24A446 `os.img` (Apple's) | 228,550 of 258,055 | 0 | 3.2 s |
| 24A446 `systemos.img`, `app.img` (Apple's) | 1,980, 930 | 0 | |
| 20F66 `os.img` (Apple's, after `udif-raw`) | 165,519 of 179,361 | 0 | 3 s |
| the other fork's `os.img` | 40,179 of 45,044 | **8,340** (all kind 4, `dyld` among them) | |
| the other fork's `systemos.img` | 1,397 of 1,412 | **272** | |
| the other fork's `app.img` (20F66's AppOS as Apple made it) | 608 of 884 | 0 | |

Every one of the 8,612 differs from Apple's map in the same place: the map's first 16 bytes, which Apple's writer
leaves zero, hold a copy of the fork's header (as the classic Resource Manager writes it). The rest of the fork is
sound: our decoder decodes all of them. Apple's 20F66 `dyld` and theirs have the same plain size (922,912 bytes) and
different forks (325,570 and 318,163 bytes): theirs was compressed again by their writer. That iOS refuses a file for
those 16 bytes is inferred from this (the message names the map; no other difference was found), not read in Apple's
code; a boot of a set that passes the check on an A56 would prove it.

### udif-raw: iOS 16's OS image (2026-11-23)

```sh
oneios-tool image udif-raw IN OUT [--sha256 HEX]     # stdout: SHA256  BYTES  OUT
```

iOS 16's IPSWs ship the OS image as a UDIF disk image (20F66's `096-04088-077.dmg`, 4,728,546,953 bytes: a `koly`
trailer, a GPT disk of six partitions, LZFSE and stored chunks); 24A446's are raw APFS already. `udif-raw` writes the
one `Apple_APFS` partition as a raw image (`src/udif.rs`): its chunks decoded in every thread and written in place
(zeros and ignored chunks left as holes), then the file read once for the partition table's CRC-32 and its SHA-256,
against `--sha256` when given; `OUT.part` is renamed last, and a failure leaves nothing. Chunk types: zeros, ignored,
stored, zlib, LZFSE; ADC, bzip2 and LZMA are refused. **The CRC-32 leaves out the ignored chunks (type 2)**: 20F66's
table says `3abcd8a4`, which is the CRC-32 over its stored and LZFSE chunks; with its 609,648 ignored sectors counted
as zeros it is `60176318` (the first version of the converter checked that, and refused Apple's image).
20F66 on the Linux PC: 6 s, 5,628,755,968 bytes, SHA-256 `9f430a58...` (the profile's `plain_sha256`), volume
`SydneyF20F66.D22D221OS`. A profile marks such a member `udif = true` (docs/image-format.md section 2): `image build`
and `make-image` convert it in their `decrypt` step, record the raw image in `inputs.json` as an AEA member's plain
image is, and remove the `.dmg`. Tests: `udif::tests` (every chunk kind on a made image, a wrong pin, a wrong CRC-32,
no APFS inside, no trailer).

### aea-decrypt: the IPSW's AEA images, without an outside program (2026-11-21)

The OS image and the SystemOS cryptex of the IPSW are Apple Encrypted Archives (`043-69487-662.dmg.aea`,
`043-70037-715.dmg.aea` in 24A446). `image aea-decrypt` makes the plain image of one, here and on the phone (the
`aea` request of `serve`); the `ipsw` CLI did this on the PC before (`scripts/fetch/fetch.sh` uses this command
now; since 2026-11-21 no script needs `ipsw` at all: `ios tbds` makes the stubs it made).

```sh
oneios-tool image aea-decrypt IN OUT [--key-cache DIR] [--key-file PEM | --key base64:..|hex:..]
                                     [--sha256 HEX | --profile PROFILE] [--resume] [-j N]
# e.g. the SystemOS image, checked against the profile's plain_sha256, the key kept for the OS image:
oneios-tool image aea-decrypt inputs/24A446/043-70037-715.dmg.aea inputs/24A446/043-70037-715.dmg \
    --profile 'profiles/ios-27.0.1-iPhone18,2.toml' --key-cache inputs/24A446/.aea-keys
# stdout: SHA256  BYTES  OUT; stderr: the key's origin, the archive's layout and the time
```

- **The key.** The archive's auth data names Apple's key service: `com.apple.wkms.fcs-key-url` serves a P-256
  private key (PKCS#8 PEM, 241 bytes), and `com.apple.wkms.fcs-response` holds the archive key sealed to it by HPKE
  (base mode, DHKEM(P-256, HKDF-SHA256), HKDF-SHA256, AES-256-GCM, no info, no AAD), as blacktop/ipsw's
  `pkg/aea` reads it. The key is fetched with the tool's own HTTP client (`transfer`: its tries, waits and give-up
  time; https only, or 127.0.0.1 for tests), and kept only where `--key-cache DIR` says (file mode 0600, named by
  the key's id; both images of 24A446 name the same key, so the second one needs no request). A cached key that does
  not open the archive is fetched again. `--key-file` takes the PEM from a file, `--key` the archive key itself.
  No key is in git or in the tool.
- **Authenticated.** Profile 1 only (symmetric, unsigned: the IPSW's). Every byte is checked before it is used: the
  root header's HMAC (over the auth data, the salt and the profile word through the main key: a wrong key, or any
  changed byte of the header, fails here), each cluster header's HMAC (chained from the cluster before), each
  segment's HMAC over its encrypted bytes, the padding's HMAC, then AES-256-CTR, the segment's decompression
  (LZFSE in 24A446; LZVN, LZBITMAP, zlib and stored too) with its exact length, and its checksum (SHA-256 in 24A446).
  The archive's size must be the root header's. A refusal names the place (`segment 107 of cluster 9 (at byte
  999501932) does not authenticate: the file was changed`) and leaves no file.
- **Bounded memory, in threads.** One reader walks the cluster headers, `-j N` workers (default: the online CPUs,
  at most 8; `serve`: the request's) each read, check, decrypt and decompress one 1 MiB segment, one writer puts them
  in order into `OUT.part` and one more thread hashes the plain stream. Buffers go round a fixed pool (at most 64 MiB),
  whatever the archive's size. Every loop asks `job::check()`; the writer counts the bytes (`job::count`).
- **The output.** `OUT.part`, renamed to `OUT` last, after the plain SHA-256 matched `--sha256` or the profile's
  `plain_sha256` (of the member with IN's file name) when one is given (else the part is removed). An `OUT` that is
  there is refused, unless it has the SHA-256 asked for (then nothing is done). The room (the plain size, less what a
  resumed part holds) is checked before the first byte (`serve`: the `space` event and refusal).
- **Resume.** Without `--resume`, a stop, an error or a panic removes the part. With it, the part and
  `OUT.part.state` (the archive's id, the SHA-256 of its header, and the plain bytes in the part, written after an
  fdatasync every 512 MiB) stay, and the next run hashes the part's bytes again and goes on from the next segment
  (the cluster headers before it are still authenticated). A state of another archive starts over.

The library function (for `serve` and the image build in one request): `oneios_image::aea_decrypt::aea_decrypt(input:
&str, out: &str, opts: &AeaOptions) -> Result<AeaDone, Fail>`, with `AeaOptions { key: AeaKey::Service { cache:
Option<PathBuf> } | AeaKey::Pem(PathBuf) | AeaKey::Raw([u8; 32]), jobs: usize (0: job::jobs()), resume: bool,
plain_sha256: Option<String>, log: &dyn Fn(String) + Sync, embedded: bool }` (`embedded`: a step of a larger request,
`make-image`: it says no steps and asks for no room of its own, only the caller's `decrypt` progress); `AeaDone` has `out`, `bytes`, `sha256`, `encrypted`,
`resumed_from`, `key_from` (`service`, `cache`, `archive`, `pem`, `given`, `none`), `compression`, `checksum`,
`segment_size`, `segments_per_cluster`, `clusters`, `already`. Steps it says: `key`, `check` (a resume), `decrypt`
(progress in plain bytes), `place`.

Proof on the Linux PC (2026-11-21; i9-12900H, 20 CPUs with SHA-NI, NVMe with btrfs at 96% full; other agents' jobs
ran): both members fetched again from Apple (`image fetch --only os,systemos`, 3 minutes, CRC-32 and the SHA-256s of
inputs.json as before) and decrypted with the key from Apple's key service:

| Image | Plain bytes | Plain SHA-256 (= inputs.json, = the `ipsw`-made file) | Time, CPU | Peak RSS |
|---|---|---|---|---|
| SystemOS, onto the disk | 6,157,238,272 | `a41d72a6...ba11a4b` | 17.6 s (the disk) | 56 MB |
| SystemOS, onto tmpfs, 8 workers | | same | **3.1 s, 515%** (6.7 s with the box busy) | 53 MB |
| SystemOS, onto tmpfs, `-j 1` | | same | 25.1 s, 101% | 19 MB |
| OS, onto the disk | 10,359,930,880 | `bba6accc...3522ed4` (and `cmp` with inputs/24A446's file: the same) | 17.8 s, 150% (the disk) | 54 MB |
| SystemOS by `serve` (`aea` request, key fetched, cached under the stage) | | same | 12.8 s | 57 MB |

Refusals on the real file: a wrong `--key` (the root header), and one byte flipped at offset 1,000,000,000 of the
SystemOS archive (`segment 107 of cluster 9 ... does not authenticate`; flipped back after, the file's SHA-256 the
pin again); neither left a file. The profile now pins both members' `sha256` (the fetch checks it) and
`plain_sha256` (`--profile` and the manifest check it). Tests (`cargo test --release`): 14 in
`crates/oneios-image/src/aea_tests.rs` on archives made by a small encoder in the crate (`aea_decrypt::testkit`, no
Apple data): stored and LZFSE segments, three checksums, short and empty files, 1 and 4 workers; a wrong key; a
changed byte in each part of the archive (the scrypt byte, the auth data, the salt, the root header, the first
cluster's MAC, a segment header, a segment MAC, a segment, a later cluster, the padding), one byte less and more,
another profile; the plain pin, an `OUT` that is there; resume from a part, a state of another archive, a part
changed since; the HPKE unwrap with the right and a wrong P-256 key; the key from a PEM file, from a local HTTP
server and from the cache (mode 0600); and a failure at the first of many segments with 1, 2 and 8 workers (it
hung before the fix: the reader waited on a full queue of workers that had ended). `tests/serve.rs` has the `aea`
request (the key over HTTP, the cache, the `exists`, `mismatch`, `auth` and `usage` refusals). **Not run on the
phone.**

Comparison with the Python tool:

The two comparisons below, `tests/image/parity.py` (stand-in files: 138 cases, all the same) and
`tests/image/compare-python.sh` (the real set), ran the Python `imagetool.py` beside the tool; both were deleted
with it on 2026-11-21. Their results stay as they were.

Result on Linux x86_64 (2026-11-12; the set built here from the fetched IPSW with `scripts/image/build.sh --no-kernel
--no-hook --no-pack`, 13 files, 17,766,460,758 bytes; `FULL_BUILD=1 BUILD_ARGS=--no-hook`, nothing skipped; run
again in the third round after the seed's writer, the JSON writer and the encoders moved to libraries, with the
same result: 35 the same, the full builds' sets among them, Python 253 s and Rust 152 s): **35
items the same** (stdout, stderr, exit status and every file written, with `cmp`), and one that differs as
documented: `apfs-info kernel.kc`, because this set has no `kernel.kc` (no kernel was built on the box), so both
tools met a missing file, where Python ends with a traceback and the Rust tool prints one line (both exit 1; on a
file that is there and is not APFS, the kernelcache, the two are the same to the byte).

| Item | Same | Python | Rust |
|---|---|---|---|
| `profile` (JSON, `--shell`, `--members`) | yes (3) | 0.07-0.10 s | 0.00-0.02 s |
| `hashes os.img` (10.4 GB), `systemos.img` (6.2 GB) | yes (2) | 63.2 s, 35.2 s | 45.7 s, 28.4 s |
| `hashes app.img`, `app.img --chunk 4096` | yes (2) | 0.16-0.21 s | 0.09-0.10 s |
| `apfs-info` of os, systemos, app | yes (3) | 0.07-0.10 s | 0.00 s |
| `apfs-info kernel.kc` | the file is not in this set (above) | | |
| `apfs-owners os.img private/var`, systemos, app, a folder that is not there | yes (4) | 0.08-12.96 s | 0.00-0.59 s |
| `apfs-owners os.img` (the whole volume) | yes | 385.1 s | 2.7 s |
| `tree-copy private/var` (meta and tree), and into a folder that is not empty | yes (2) | 11.4 s | 3.0 s |
| `seed-pack` (copied tree; changed tree with owner rules; a bad owner rule) | yes (3) | 4.8 s, 7.7 s | 4.0 s, 3.6 s (while the comparison's other jobs ran; "seed-pack" below has the clean numbers) |
| `manifest` (also the set's own `manifest.json`; a set with a stray file) | yes (3) | 83.9 s | 50.5 s |
| `verify`, `verify --quick`, and 9 sets with one thing wrong each | yes (11) | 99.8 s | 50.3 s |
| **`build.sh --no-pack` with each tool: every file of the two sets** | **yes** | 207.9 s | 131.3 s |

The whole-volume `apfs-owners` and the two full builds were the open points of the first round (skipped, and
stopped part way): both are the same. The box's disk and CPUs were shared with other jobs; the hashing commands gain less
here (1.2 to 2.0 times) than on the Mac (1.9 to 2.9 times).
`IMAGETOOL_IMPL=rust scripts/image/build.sh ...` uses the Rust tool; the default stays `python`.

### seed-pack: where the time goes

`data.seed` (24A446: 20,798 entries, 6,002 files, 1.2 GB; 243 files of 1 MiB or more hold 1.0 GB) is written in
five steps, timed with `ONEIOS_IMAGE_TIMING=1`. Since the third Linux round the tar's members are written **in
parallel, each at its own offset**: every member's place is known once the headers are made, so there is no single
writer. A file of 1 MiB or more goes straight into its place with `copy_file_range` (Linux; `read` + `pwrite`
where the kernel or the file system refuses, and on macOS); the headers and the smaller files are gathered in
runs of about 4 MiB (one reused buffer per thread, one `pwrite` each). The padding of big files and the end of the
archive are never written: the file is new, and its length is set first. The headers sit in one buffer (no copy
per member), and the meta file is read while the tree is listed.

The floor on this box (Linux x86_64, 4 CPUs, btrfs with `compress=zstd:3` on a SATA SSD, `vm.dirty_bytes` 256 MB;
2026-11-12, the box idle, three runs each, the numbers are the three runs' range):

| What | tmpfs (`/tmp`) | the disk |
|---|---|---|
| `cat` of the tree's 6,002 files to `/dev/null`, warm cache | 0.19 s | |
| the same, cold cache (the tree's pages dropped with `posix_fadvise`) | | 5.9-6.4 s (4 and 16 readers at once: 5.9-6.0 s) |
| `cp --reflink=never` of the finished 1.2 GB seed, warm | 0.35 s | 2.4-3.0 s (earlier, with other jobs on the disk: 3.9-11.8 s) |

| `seed-pack`, warm cache | tmpfs | the disk |
|---|---|---|
| Python (`imagetool.py`) | 1.84-1.86 s | 3.8-4.3 s |
| Rust before (`8cc763e`: one reader thread, one writer) | 0.53-0.56 s | 2.4-3.5 s |
| **Rust now** | **0.43-0.45 s** | **2.5-2.7 s** |
| Rust now, `ONEIOS_SEED_SINK=map` | 0.28-0.29 s | 2.3-3.7 s |

Laps, warm, tmpfs: before `meta 0.031, list 0.032, lstat 0.009, headers 0.044, write 0.404-0.421 s`; now `meta +
list + lstat 0.054, headers 0.025, write 0.335-0.349 s` (`map`: `write 0.195-0.201 s`).

What the numbers say:
- **On the disk every tool is at the disk's own speed**: writing 1.2 GB there takes what `cp` takes (the kernel
  holds a writer back once 256 MB are waiting to be written). Python is 1.3 s above that, both Rust versions are
  within the noise of `cp`. The "5.0-6.0 s against 8.8-9.2 s" of the round before was this disk under other
  agents' jobs or with a cold cache, not the tool.
- **With a cold cache every tool is at the disk's read speed** (about 6 s for the tree, the same with 1, 4 or 16
  readers): Python 8.2-9.7 s, Rust before and now 5.8-6.0 s to tmpfs.
- **Where the disk does not hold it back (tmpfs)**: 4.2 times Python, 1.25 times the Rust before, and 0.08 s above
  `cp` of the finished file. Positioned writes do not run side by side on Linux: a buffered write holds the output
  file's lock while it copies (seen on btrfs and tmpfs: four writers were no faster than one), so the gain is the kernel-side copy and the
  smaller fixed parts, not four writers.
- `ONEIOS_SEED_SINK=map` maps the output and reads every file straight into its pages, which does run side by
  side: 0.28 s on tmpfs, below `cp`. It is **not the default**: on the disk it is no faster (and less even), and
  a full file system ends the process with SIGBUS instead of an error message. `ONEIOS_SEED_SINK=plain` is the
  path without `copy_file_range` (what macOS takes). The three write the same bytes (a unit test, and the real
  tree: sha256 `d9a7c0ae...` from each and from Python).
- Not measured: macOS (APFS has no 256 MB hold-back; the Mac's numbers of the first round were 1.4-1.5 s Python
  against 0.56-0.98 s Rust).

### image build: the whole set in one process (2026-11-21)

`oneios-tool image build` is `scripts/image/build.sh` in one process, as `prelink build` is `kernel/prelink/build.sh`:
the phone has no bash, no apfs-fuse and no Python, and `serve` starts no program. `serve`'s `make-image` request is
this (crate `crates/oneios-imagebuild`). The steps, in build.sh's order and with its checks:

| Step | What | build.sh's |
|---|---|---|
| `check` | the profile (`image profile --shell`, `--members`), the inputs that are there, the kernel's inputs, the set folder (absent or empty), and **one** room check for everything the build writes (`serve`: one `space` event). What the tool cannot do is refused here, before anything is fetched or written (`not-ported`: nothing since 2026-11-21, the AEA decryption and the root tweaks are in it) | the tool and input checks at its top |
| `directory`, `download` | `image fetch` of the profile's members into INDIR (resumable; a member that is there and whole is not fetched again; an AEA whose decrypted image `record-plain` recorded is not fetched) | `scripts/fetch/fetch.sh` before it |
| `decrypt` | the AEA images into their plain images (`aea_decrypt` of oneios-image, since 2026-11-21: the key from Apple's key service kept in INDIR/.aea-keys/, the profile's `plain_sha256`, resumable; only this step's progress, no steps or room of its own), then `record-plain` and the `.aea` removed; a UDIF member (`udif = true`, iOS 16's OS image, since 2026-11-23) into its raw APFS image the same way (`udif-raw`: `<member>.raw`, the table's CRC-32 and `plain_sha256` checked), then `record-plain` and the `.dmg` removed | `oneios-tool image aea-decrypt` in fetch.sh (`ipsw fw aea` before 2026-11-21) |
| `place` | the images and trust files into the set: hard links where the folder allows, else copies (`put`); each volume must pass `apfs-info` | `put`, `apfs-info` |
| `data` | the OS volume's `/private/var` copied out with **our APFS reader** (`src/data.rs`), with tree-copy's meta file: the same folders (0700), files (0600, both times the source's whole seconds), links and meta entries, in tree-copy's order; the three xattrs that only say how APFS stores a file are left out, as tree-copy leaves them out | apfs-fuse (hdiutil on a Mac) and `tree-copy` |
| `tweaks` | the root tweaks into the tree, the overlay and the owner rules: `oneios_tweaks::run_hook` ("image tweaks"), the OS volume read from `os.img` with our APFS reader; its record (`hook` `oneios-tool image tweaks`, `hook_sha256` of the tool's binary, `info`; no git) goes into the manifest. `--no-tweaks` (`"tweaks": false`) builds without them | the userland hook |
| `seed` | `seed-pack` into `data.seed`; the overlay into `overlay/` | `seed-pack`, `cp -R` |
| `kernelcache` | Apple's kernelcache member decoded (`im4p`), unless one is given (`--kernelcache`, the stage's `kb/kernelcache.macho`) | `kernel-inputs.sh` |
| `kernel-check` ... `kernel-place` | `prelink build` of the runtime's `kernel/xnu.macho` with it: its sixteen steps, the audit and `stubs-check` before `kernel.kc` is taken (`audit` refuses); unless a `kernel.kc` is given (`--kernel-kc`) | `kernel/prelink/build.sh`, then `--kernel FILE` |
| `manifest`, `verify` | `manifest` (the hash lists too) and `verify` of the set, every chunk, and every compressed file of the three volumes as iOS reads it (since 2026-11-23, "apfs-check") | the same |
| `lists` | the stage's lists of the set (`SHA256  PATH` and `DEV:INODE:SIZE:MTIME PATH`, sorted, `manifest.json` included: what root's `receive` writes for an import), then the set into its folder | replacing `out/image/BUILD` |

The image tool's commands run **in the process** (`oneios_image::run`, the standard output and error read back
through a temporary file for the call: `src/cmd.rs`), so the files are the commands' own. A stop (a cancel, the heat
and battery limits, the end of `serve`) or a failure removes what the build wrote: `WORK/build`, the set's files and
the lists' new lines; the fetched members and the fetch's parts in INDIR stay (they are what a later run resumes).
A `pause` holds the fetch's transfer at once and the build between any two steps.

```sh
oneios-tool image build PROFILE --indir DIR --out SET [--work DIR] [--no-fetch] [--source URL|FILE] [--resume] [--no-tweaks] \
  (--kernel-kc FILE | --no-kernel | --xnu XNU (--kb KB | --repo CHECKOUT) [--kernelcache FILE|-]) [--sums-list F] [--ids-list F] [--keep-work] [-j N]
# the PC: the v0.1.12 runtime's kernel, the pairing files as kernel/prelink/build.sh takes them, Apple's kernelcache decoded here
oneios-tool image build 'profiles/ios-27.0.1-iPhone18,2.toml' --indir inputs/24A446 --out out/image/24A446-rust \
  --no-tweaks --xnu RUNTIME/kernel/xnu.macho --repo . --kernelcache -
```

`--repo DIR` takes the pairing files of a OneiOS checkout (`kernel/unixnu/profiles/os/*-BUILD/profile.toml`,
`kernel/prelink/reviewed-BUILD-v54.json`, `lead/` and `user/`, `pairs-BUILD.json`, `stubs-BUILD.json`, XnuPrelink as
build.sh finds it); `--kb DIR` the stage's `kb/` as the OneOS app writes it (`profile.toml`, `reviewed.json`,
`lead.json`, `user.json`, `pairs.json`, `stubs.json`, `match/`). Exit status: 0 done, 1 failed, 2 refused (the line
says the code), 3 a wrong command line. The last lines say the set's files, bytes, the SHA-256 of `manifest.json`,
`kernel.kc` and `data.seed`, the time and the peak memory (getrusage).

**Compared (Linux PC, 2026-11-21; 20 CPUs, 30 GB RAM, NVMe, the images in the page cache).** Inputs: a hard-linked
copy of `inputs/24A446` (the PC's own decrypted images: AEA is not in the tool yet), the v0.1.12 runtime package's
`kernel/xnu.macho` (`326e2144...`), no root tweaks on both sides.

| Way | Time | Peak memory | The set |
|---|---|---|---|
| `kernel/prelink/build.sh` (KERNEL= that xnu.macho), then `scripts/image/build.sh --kernel ... --no-hook --no-pack` | 15.0 s (the set; the prelink before it ~14 s) | not measured (no `time -v` on the box) | the reference |
| `image build --no-fetch` (decodes the kernelcache, prelinks, builds) | 19.2 s, 53 s CPU | 1,066 MiB | **all 16 files the same SHA-256, `manifest.json` byte for byte, `kernel.kc` `276aadac...` = the prelink's** |
| `image build` with the fetch from Apple's URL (the members there: the directory read and every member's CRC checked) | 29.8 s | 1,065 MiB | the same 16 files; `inputs.json` unchanged |
| `serve` `make-image` with the app's keys and `"tweaks": false`, on a stand-in stage (the v0.1.12 runtime, the APK's `assets/kb/`, the members in `make/inputs`) | 37.6 s | | the same 15 files; `manifest.json` differs in one line, `profile.file`: the runtime package names the profile `ios-27.0.1-iPhone18-2.toml` (a package path has no comma), its SHA-256 is the same. One `space` event (need 3,202,307,260 bytes), the lists as `sums` reads them, `bye`: 1 thread, 0 children |
| with the root tweaks: `scripts/image/build.sh --python-hook` (the frozen Python hook) against `image build --repo .` (the Rust tweaks via `oneios_tweaks::run_hook`, the OS volume read from `os.img`; image-hook's defaults from the checkout) | 25.2 s against 21.3 s | 1,061 MiB | **all 21 files but `manifest.json` the same SHA-256** (`data.seed` `27ec4e10...`, the five overlay files, `kernel.kc`); `manifest.json` differs only in the `tweaks` record: `hook` `oneios-tool image tweaks` and the binary's SHA-256 for `userland/image-hook` and its SHA-256, no `commit` and `dirty` (no git), `info.tool` |

Disk: the images are hard links (no room); the set's own new files are `data.seed` (1,231,912,960 bytes),
`kernel.kc` (96,206,848) and the hash lists; the work folder holds the copied tree (1.2 GB, removed after the seed),
the seed, the decoded kernelcache (78 MB) and the prelink's work while they run. The room asked: 3.2 GB.
**Not run on the phone.** The AEA decryption is `make-image`'s `decrypt` step since 2026-11-21 (the runs above had the
PC's decrypted images). **From the encrypted
members** (2026-11-21, the Linux PC): `image build PROFILE --indir DIR --no-fetch --no-tweaks --kernel-kc
out/kernel/24A446/kernel.kc`, DIR holding the two `.aea` members fetched from Apple that day and the other members:
the key fetched from Apple's key service for the OS image and taken from `DIR/.aea-keys/` for SystemOS, both
decrypted to their pins, `verify` ok, 51.6 s, peak memory 1,041 MiB (the APFS copy's, not the decryption's);
`os.img`, `systemos.img`, `app.img` (their chunk hash lists) and the trust files the same as `out/image/24A446`'s.
On a stand-in IPSW whose OS and SystemOS members are AEA archives: `make_image_decrypts_the_aea_members_with_the_key_services_key`
(with the fetch, and with `fetch` false and only the encrypted members there, which `--no-fetch` refused as
missing inputs before).

Tests: `tests/serve.rs` `make_image_*` (four, on a stand-in IPSW served by `rangeserve`: every refusal and that it
writes nothing; one `space` event and the refusal when the room is short; pause, resume and cancel through the
fetch, the next request going on from the part, and a failed step removing everything it wrote while the fetched
members stay; the AEA members decrypted with a local key service's key, one `space` event, the decryption's progress
under `decrypt` and no steps of its own); `oneios-imagebuild`'s unit tests (the prelink's step names, the shell values, base64, the times).

### The image set without Python's bytes (2026-11-21)

The owner let byte parity with the retired Python go on 2026-11-21: the JSON is serde_json's, the tars the `tar`
crate's, the root tweaks' property lists the `plist` crate's ([todo_rust.md](todo_rust.md) section 11 has every
replacement and what was kept). Built on the Linux PC from `inputs/24A446` with the v0.1.12 runtime's
`kernel/xnu.macho` (`326e2144...`), the root tweaks on, by the tool before the change (`e5cb74a`) and after it:

```sh
oneios-tool image build 'profiles/ios-27.0.1-iPhone18,2.toml' --indir inputs/24A446 --out SET --work WORK \
  --no-fetch --xnu RUNTIME/kernel/xnu.macho --repo . --kernelcache -
oneios-tool image verify SET
```

| | before (`e5cb74a`) | after |
|---|---|---|
| time, peak memory | 20.1 s, 1,019 MiB | 17.5 s, 1,018 MiB |
| `data.seed` | `27ec4e10...`, 1,267,793,920 bytes | `5406d527...`, 1,265,063,424 bytes |
| `kernel.kc` | `276aadac...` | `276aadac...` (its records the same but for the work folder's name) |
| `manifest.json` | `f8320c5d...` | `a4c8bbd1...`: the seed's size and SHA-256, `tweaks.manifest.json`'s SHA-256 and the tool's own (`hook_sha256`) |
| `overlay/userland/tweaks.manifest.json` | `a62d5d80...` | `deacce95...`: the SHA-256 of the three property lists below |
| the other 18 files (the images, the hash lists, the trust files, `boot-contract.json`, `report.json`, `report.txt`, `owner-trust.json`, `xnuhost.conf`) | | the same SHA-256 |
| `image verify` | ok, 22 files | ok, 22 files |

The two seeds hold the same tree: GNU tar lists the same 20,851 entries in both (names, modes, numeric owners, sizes,
times, link targets), no entry has xattrs, and unpacked the files are the same except the three binary property lists
the tweaks write (`mobile/Library/Preferences/.GlobalPreferences.plist`, `com.apple.springboard.plist`,
`com.apple.migration.plist`): other bytes from the `plist` crate's writer (same size), the same values (read back with
the `plist` crate, a dictionary's order not counted). No property list of the seed or the overlay holds `&quot;` or
`&apos;`. The seed of the unit test (`seed::tests::build`: a name that is not ASCII, one of 127 bytes, xattrs, a link)
unpacks with `oneios-seed` built for the PC (`cc userland/helpers/oneios-seed.c`) to the same tree.

**A set built before still works.** Nothing re-derives a manifest or a seed and compares bytes: `image verify`,
`phone image-check`, the fork's verifier and xnuhost parse `manifest.json` and compare values; the phone's Data store
is matched by the seed's SHA-256 in the manifest of the set that is staged. With the new tool `image verify
out/image/24A446` (the set of 2026-11-11, from the Python chain: 20 files, 17,859,859,715 bytes) and `image verify`
of the "before" set both say ok. A new set's seed has another SHA-256, so the first boot of a newly staged set starts
the phone's Data store over (docs/image-format.md section 5); restaging the old set keeps its store.

**The PC boot of the new set** (docs/building.md section 5's command: `qemu-aarch64 -cpu cortex-a710` of the v0.1.12
runtime's `bin/xnuhost --mode bench --image SET --kernel SET/kernel.kc`, a new Data store, `--cpus 8 --ram 4G`, under
`timeout 900`; the box had a load of 7 to 22 from other agents' jobs): `OneiOS Data: seed verified: 6032 files
(1252792964 bytes), 14818 folders, 1 links, 0 extended attributes` at +158 s (the 20,851 entries; oneios-seed read the
new headers), SpringBoard running at +257 s, `guest ready: first frame
(it has content)` at +372 s and `session/ready` written (the boot of the old set on 2026-11-21, release3: SpringBoard
+217 s, ready +314 s, a box less loaded). No panic in the 900 s (`timeout` ended it: xnuhost `exit 0` on the signal), no jetsam kill; at the end lsd and
rapportd were the busy processes (rapportd's busy loop is the open item of the release-kernel entry in
docs/agents.md, seen with the old set too).


## sym

`oneios-tool sym [--slide HEX] ADDR...` names user addresses of a boot log in the image's dyld shared cache, as
`scripts/debug/dsc.py` did (the Python symbolizer it replaced; deleted on 2026-11-12, its output frozen as
hashes), from an index it builds once (`out/dsc-index/<cache UUID>/`, 1.07 GiB).
`ONEIOS_DSC_DIR` is the folder with `dyld_shared_cache_arm64e`. **No mount is needed** since the APFS reader
decodes compressed files: `oneios-tool image apfs-copy out/image/24A446/systemos.img
System/Library/Caches/com.apple.dyld out/dsc` copies the cache out of the image (82 files, 7,312,005,844 bytes,
24 s; `.symbols` and `.dyldlinkedit` are kept compressed in the image and come out plain). The index built from
that copy (18.6 s) is byte for byte the index built through an apfs-fuse mount (26.4 s): `meta.bin`, `syms.bin`,
`names.bin`, `calls.bin` compared with `cmp`. A mount still works (`apfs-fuse -o ro IMAGE out/dsc`; the folder is
`out/dsc/root/System/Library/Caches/com.apple.dyld`), but there every open of `.symbols` decompresses it again
(2.5 s for its first 16 bytes), which is why `dsc.py` is so slow through it.

**`sym find` and `sym callers` (2026-11-24, fix/a56-ios16-faults):** the other way round, for reading a crash's
neighbourhood without IDA.

```sh
oneios-tool sym find [--slide HEX] NAME...       # every kept symbol whose name holds NAME: address, image, name
oneios-tool sym callers [--slide HEX] ADDR...    # every b and bl of the cache's images that reaches ADDR, named
```

`find` answers from the index (the names `sym` would give: one per address of an image's `__TEXT`, local symbols
included); an `_objc_msgSend$SELECTOR` stub names an image that sends SELECTOR, which is how a selector's callers are
found (then `callers` of the stub). A NAME nothing holds is said, status 1. `callers` reads the cache itself (it needs
`ONEIOS_DSC_DIR`): each image's `__TEXT` is scanned for `b`/`bl` whose target is ADDR (parallel; 0.5 s over 20F66's
3.3 GB cache); a target nothing reaches is said too (an Objective-C method is reached through `objc_msgSend`, not by
a branch). With `--slide` the addresses are given and printed slid. Used on 20F66 (`ONEIOS_DSC_DIR=out/ios16-cache`,
the shared region's slide 0x2db08000) to follow SpringBoard's pc-0 crash from `CI::GLContext::init` back to
`+[CIContext internalGLContextWithOptions:]` (docs/gpu.md 5.2). Tests: `tests/synthetic.rs` `find_names_and_callers`.

**`sym dis`, `sym find --no-index` and partial copies (2026-11-25, ios272):** for re-finding the Metal plug-in's
QuartzCore addresses in another build (docs/gpu.md section 7, iOS 27.2.1) without blacktop's `ipsw`.

```sh
oneios-tool sym dis [--slide HEX] ADDR [COUNT]   # COUNT instructions (hex, default 0x40) from ADDR, as llvm-mc reads them,
                                                 # with each branch's, adrp's and adrp+add/load/store's address and its name
oneios-tool sym find --no-index NAME...          # every symbol whose name holds NAME, read from the cache itself: data too
```

`dis` reads the cache (`ONEIOS_DSC_DIR`) and needs `llvm-mc` (`LLVM_MC=PATH`); names come from the index, which keeps
code only. `find --no-index` reads each image's own symbol table and the `.symbols` file's local symbols. A partial
copy of a cache (the main file, `.01`, `.25.dylddata`, `.26.dyldreadonly`, `.symbols`, `.81.dyldlinkedit`: what
24B156's hooks needed) is read as far as it goes; the index says how many subcache files are not there and those
images get no names.

**`sym cstr`, `sym ptrs`, `sym refs`, `sym dis` (2026-11-25, fix/s24-fps-heat-camera):** reading the cache's data and
code, for answering "does iOS 27 still have this name, and who uses it" without IDA. All four read the cache itself
(`ONEIOS_DSC_DIR`) and name addresses from the index. The branch had its own `dis ADDR LEN` (llvm-objdump); the merge
into main (2026-11-25) kept main's `dis ADDR [COUNT]` (llvm-mc, docs/gpu.md section 7) and gave it the branch's naming
of strings and pointers.

```sh
oneios-tool sym cstr TEXT...                     # every address where TEXT stands as a whole C string (a selector, a class, a key)
oneios-tool sym ptrs [--slide HEX] ADDR...       # every 8-byte word that points at ADDR (its low 36 bits): selrefs, class refs, CFStrings
oneios-tool sym refs [--slide HEX] ADDR...       # every adrp + add/ldr/str pair in an image's __TEXT that forms ADDR, named
oneios-tool sym dis [--slide HEX] ADDR [COUNT]   # the dis above (llvm-mc): an adrp + add/ldr address with no symbol is named by
                                                 # the C string there, or the string or symbol a pointer there points at
```

A TEXT `cstr` does not find is said (status 1); `ptrs` and `refs` say each address nothing points at or forms. The
scans run on every core (`cstr` over 24A446's 7.3 GB cache: 12 s). Used on 24A446 (`ONEIOS_DSC_DIR=out/ios27-cache`)
to find that the capture stream's output names (`PrimaryScaler`, `SecondaryScaler`, `StillImage`, `VideoOutputHandlers`,
`PixelBufferHandler`, `BufferPool`) are iOS 27's too (CMCaptureCore), that the in-process animation manager is
AnimationKit's (`docs/s24-fps-heat-camera.md`), and that `+[XBApplicationSnapshotManifestImpl _outputFormatForSnapshot:]`
is still a class method. Tests: `tests/synthetic.rs` `strings_pointers_and_page_pairs`.

```sh
ONEIOS_DSC_DIR=<cache folder> tools/oneios-tool/tests/sym/compare-golden.sh
```

The script writes two stand-in logs from the cache (`oneios-tool debug sym-make-log`, fixed seeds: the addresses
`tests/sym/make-log.py` drew, which it replaced on 2026-11-21; the run after that: 18 of 18 again), takes its 18 address
sets from them, and compares `sym`'s stdout and exit status with `tests/sym/golden/24A446.txt`: per set, the
SHA-256 of the arguments, of `dsc.py`'s stdout, and its status, taken from the last run of both tools. Result
after `dsc.py` was deleted (2026-11-12, the cache copied out of the image with `apfs-copy`): 18 of 18 the same.
With another cache the sets are other addresses and the script says "other arguments".

**The comparison that froze them** (`tests/sym/compare-python.sh`, in git up to commit `06a9222`):

Result on Linux x86_64 (2026-11-12): **18 of 18 address sets byte for byte the same as `dsc.py`**, exit status
included. The box has no log from the phone, so the sets come from two stand-in logs that `make-log.py` wrote from
the real 24A446 cache (return addresses after real `bl` instructions, slides `0xedb4000` and `0x124d8000`; the
frames are drawn at random, not real call chains). The cache was read through apfs-fuse, where `.symbols` is
decompressed on every read: that is why `dsc.py` is far slower here than on the Mac.

| Case | Same | `dsc.py` | `oneios-tool sym` |
|---|---|---|---|
| a1-a4: whole thread backtraces (12-14 addresses), no `--slide` | yes (4) | 35-63 s | 0.16-0.25 s |
| a5, a6: crash backtraces (6 addresses), no `--slide` | yes (2) | 15-80 s | 0.19-0.85 s |
| b: the same six with `--slide` | yes (6) | 5-20 s | 0.002-0.44 s |
| c1, c2: addresses outside the cache | yes (2) | 4-6 s | 0.03-0.19 s |
| d1, d2: 2 addresses and 1 address ("NOT reliable") | yes (2) | 35-55 s | 0.10-0.19 s |
| e1: 4000 addresses with `--slide` | yes | 94 s | 14.4 s |
| e2: the same 4000, no `--slide` | yes | 649 s | 1.7 s |

**The 14.4 s of e1 was not apfs-fuse and not the tool** (settled in the third round): with `--slide` given, `sym`
never opens the cache (the same 0.03 s with no cache folder, with a local copy and through a fresh apfs-fuse
mount, the index warm; the same output each time). The time is the index's own pages coming from the disk: with
the four index files dropped from the page cache the same command takes 1.7-1.8 s, and in the comparison run it
was the first use of a just-built 1.07 GiB index on a disk that other jobs were writing. Nothing was changed.

The index: built in 26.4 s (1068.8 MiB; 4,696 images, 11,365,799 symbols kept). The Mac's run of the first round
(18 of 18 on sets from four real logs; `dsc.py` 0.03-53 s, `sym` 3-170 ms) is in [todo_rust.md](todo_rust.md).
Not compared on Linux: address sets from real phone logs (none on the box).

## sym kernel

`oneios-tool sym kernel` names the kernel addresses of an xnuhost panic: the member and segment of the kernel
collection (`kernel.kc`) each one is in, the function + offset, and for the kernel itself file:line and the
inlined frames from the unprelinked kernel (`xnu.macho`). `sym` names user addresses only (the dyld shared cache);
the frames of the phone's panic of 2026-11-12 23:15 were named by hand with `atos` on the Mac. Crate
`crates/oneios-ksym`; added 2026-11-13 on Linux.

```sh
oneios-tool sym kernel LOG                          # every panic block of an xnuhost session log (`-`: standard input)
oneios-tool sym kernel 0xfffffff00b54c094 ...       # addresses by hand (hex): of the collection, or running ones with --slide
oneios-tool sym kernel --symbol tcp_input           # where a function is: in the collection, in xnu.macho, running
oneios-tool sym kernel --xnu 0xfffffff007789b78     # an address of xnu.macho, as atos and llvm-symbolizer take them
oneios-tool sym kernel index [--force]              # build the index now; prints every kernel segment's move
oneios-tool log panic LOG                           # the same panic blocks, from the log tool
oneios-tool log stacks LOG                          # ends with them when the log has a panic
#   --kc PATH/kernel.kc  --kernel PATH/xnu.macho  --dsym PATH  --no-kernel  --slide HEX  --index DIR
#   -f text|json  --full-paths  --return  --no-call-sites  --stdin  --rehash  -j N
```

**The files.** `--kc` and `--kernel`, else `ONEIOS_KC` and `ONEIOS_KERNEL`, else the newest
`out/kernel/*/kernel.kc` and the `xnu.macho` that its prelink record names (`kernel.kc.build.json` beside it:
`inputs.kernel.path` or any `out/kernel/current-hosted*/xnu.macho` with `inputs.kernel.sha256`), else the newest
`out/kernel/current-hosted*/xnu.macho`. In a git worktree under `.claude/worktrees/` the checkout's own `out/` is
searched too. The output starts with the two paths and their SHA-256 (kept in `out/ksym-index/hashes.json` with the
size and time they were taken at, so a later call does not hash 93 MB again; `--rehash` does), where the DWARF came
from, and whether the two belong together: a `WARNING` when the record names another kernel, when a kernel segment
has another size in the two files, or when the symbols the collection keeps for the kernel (70,579 here) are not
all where this kernel's symbols land.

**Addresses.** Four kinds, told apart by where they lie:

| Address | From | Rule |
|---|---|---|
| in the collection | `panic(cpu N caller 0x...)` (also a head whose first letters the console lost: `anic(cpu 0 caller 0x...)`, the phone's panic of 2026-11-23; `oneios_common::panic_head` takes `anic`, `nic`, `ic`, `c` or none before `(cpu N caller 0x` at the start of a guest line, and `panic(cpu ` anywhere. `log summary`, `log timeline`, `phone log --panic` and the cycle's panic marker use the same rule), the `frame 0x...` lines (`hosted_panic` prints them `- vm_kernel_slide`) | the fileset's `LC_FILESET_ENTRY` members and each member's `LC_SEGMENT_64`s are read; the smallest segment that holds the address gives member and segment (every kext lists the shared `__LINKEDIT`: the first member has it) |
| in `com.apple.kernel` | the same | **xnu.macho address = address - (the segment's vmaddr in the collection - the same segment's vmaddr in xnu.macho), per segment.** The prelink places each segment on its own: here `__TEXT` moves by `0x180000`, `__DATA_CONST` by `0xc24000`, `__TEXT_EXEC` and `__KLD` by `0x386c000`, `__DATA` by `0x3a28000` (`sym kernel index` prints all ten). A symbol moves with the segment of its section (`n_sect`), not of its address: an end label sits on the next segment's first address, and `__LAST` has no size |
| of the running kernel (the "physical aperture") | the fault's `pc` and `lr`, in the panic's text and in its register dump | **collection address = running address - slide** (modulo 2^64), **slide = RAM - link base**. xnuhost maps the guest's RAM once, at `0x2000000000 + (link base mod 2 MiB)` (`runtime/xnuhost/src/mem.rs`, retried 16 GiB higher), copies every segment of the collection to `RAM + (vmaddr - link base)` (`runtime/loader/src/boot.rs`; link base = the lowest vmaddr of the collection's own segments, `macho.rs`) and runs it there with physical = virtual (`docs/booter.md` 2.2). The log has all three numbers: `kernel: ... link base 0xfffffff007004000` and `memory: N MiB at 0x2000004000, physical = virtual (slide 0x2ff9000000)`. So `pc 0x200454c094` is `0xfffffff00b54c094`, the panic's first `frame` line, and `lr 0x2003ff1038` is `0xfffffff00aff1038`, `0x7138` bytes after the `tcp_input` frame. `--slide HEX` gives the slide for addresses by hand or a log cut before its `memory:` line; the tool warns when the log's slide is not RAM - link base or its link base is not this collection's |
| anything else | | said in the line (`not in the collection (A to B); as an address of the running kernel (slide S) it is X: not in it either`), never dropped |

**Names.** Function + offset is the nearest symbol at or below the address in the same member and segment: for
the kernel from `xnu.macho`'s symbol table (C++ demangled; of two names of one address the one the DWARF gives
the function, else the last external one, so `_start` and not the `LowResetVectorEnd` label before it), for
another member from its own symbol table in the collection. **This collection keeps no symbols of its 134 kexts**,
so a kext frame is `member segment  __TEXT_EXEC+0xOFFSET`; kexts are at the kernelcache's addresses, so the
address is the one a disassembler of `kernelcache.macho` shows. `--no-kernel` (or no `xnu.macho`) names the kernel
from the symbols the collection keeps for it, without file:line.

**DWARF** is read by `addr2line` (on `gimli`) from where it is, in this order: `--dsym PATH`; `xnu.macho`'s own
`__DWARF` sections; a `.dSYM` in `xnu.macho`'s folder with its UUID; the kernel's **debug map**. The Linux build
has the last: `xnu.macho` carries no DWARF and no dSYM (`mach.development.bcm2837.dSYM` in the build tree is
empty), its 1,070 `N_OSO` stabs name the object files and archive members of the build tree
(`out/kernel-hosted-*/...`), which hold the DWARF. An object file whose time is not the one its stab recorded was
rebuilt since the link: its functions get no lines and the head says how many (lines of another build would be
wrong ones). `llvm-symbolizer` and `llvm-addr2line` do not read a debug map: on `xnu.macho` they give function
names only.

**The index** (`out/ksym-index/<kernel.kc SHA-256>/<xnu.macho SHA-256>/`, `kc-only` without a kernel;
`ONEIOS_KSYM_INDEX` or `--index` for another folder) is built by the first call for a pair of files: the members
and segments, every symbol at its collection address, and file, line and inlined frames of **every instruction of
the kernel** (2,471,575 addresses asked of `addr2line`, 761,328 runs of equal answers). `index.bin` 23.2 MiB,
`meta.json` 0.1 MiB. After that a call reads neither the collection nor the kernel nor an object file, so the
index stays right when the next kernel build replaces the object files. The threads (`-j N`, default: the CPUs)
each take a share of the addresses; `index.bin` is the same file for 1, 3 and 4 threads. Times on the Linux PC (4
CPUs; other agents' builds were running, load 3 to 12): **the first call for a pair 2.2 to 4.4 s** (the index
1.3 to 3.7 s with 4 threads; with `-j2` 1.7 to 2.8 s at load 3 and 6 to 9 s at load 8 to 12; hashing the two files
once is about 0.7 s), **later calls 4 to 10 ms** for a panic block of 19 addresses, the process from start to end
(0.18 s in one call; a guess: the index was no longer in memory. Not looked into).

**Output.** One line per address, `ADDRESS  member segment  function+offset  (file:line)`; in a panic block the
line starts with what the address is (`caller`, `pc`, `lr`, `frame`). The file:line is the place in the function
itself; the functions inlined there follow, each indented under the one it was inlined into, the last one being
where the address is. `caller`, `lr` and `frame` are return addresses: when the call (4 bytes before) is on
another line or in another function, a line `the call, at ADDRESS: ...` says where (`--no-call-sites` leaves it
out; a call that never returns can be the last instruction of its function, and then the return address is the
next function's first). `-f json` has every field (the address as given, `space`, `kc_address`,
`kernel_address`, `member`, `segment`, `symbol`, `raw_symbol`, `offset`, `frames` with whole paths and mangled
names, `call_site`, `note`). From the stand-in log of this box's kernel:

```
panic at xnuhost.log line 6 (cpu 4): Unexpected fault in kernel physical aperture at pc 0x00000020044edcd8, lr 0x0000002003ffe0cc (saved state: ...)
  running address = collection address + slide 0x2ff9000000 (RAM 0x2000004000 - link base 0xfffffff007004000)
  caller 0xfffffff00b4e80a4  com.apple.kernel __TEXT_EXEC  sleh_synchronous_sp1+0x54  (sleh.c:689)
             the call, at 0xfffffff00b4e80a0: sleh_synchronous_sp1+0x50  (sleh.c:681)
  pc     0x20044edcd8 (running; 0xfffffff00b4edcd8 in the collection)  com.apple.kernel __TEXT_EXEC  __ubsan_handle_mul_overflow_minimal+0x0  (ubsan_minimal.c:165)
  lr     0x2003ffe0cc (running; 0xfffffff00affe0cc in the collection)  com.apple.kernel __TEXT_EXEC  compute_iaj+0x1ec  (tcp_input.c:453)
               inlined compute_iaj_meat  (tcp_input.c:0)
             the call, at 0xfffffff00affe0c8: compute_iaj+0x1e8  (tcp_input.c:453)
                 inlined compute_iaj_meat  (tcp_input.c:439)
  frame  0xfffffff00adcbc30  com.apple.kernel __TEXT_EXEC  sleh_synchronous+0xc18  (sleh.c:999)
               inlined handle_abort  (sleh.c:1572)
                 inlined handle_kernel_abort  (sleh.c:2717)
  frame  0xfffffff00add05b4  com.apple.kernel __TEXT_EXEC  hosted_exception_upcall_entry+0x18
  frame  0xfffffff00aff70b4  com.apple.kernel __TEXT_EXEC  tcp_input+0x153c  (tcp_input.c:3723)
             the call, at 0xfffffff00aff70b0: tcp_input+0x1538  (tcp_input.c:3721)
```

### The comparison

```sh
tools/oneios-tool/tests/ksym/compare.sh                    # the pair `sym kernel` finds under out/kernel
tools/oneios-tool/tests/ksym/compare.sh KC KERNEL [LOG]    # these files; LOG: a real xnuhost session log
#   DSYM=PATH (the kernel's dSYM, when its object files are not on this machine)  SAMPLE=N  KEEP=1
```

The reference is `llvm-symbolizer --inlining --no-demangle` (LLVM 23.1.4) on a dSYM that `dsymutil` makes from the
same `xnu.macho` (5.6 s): LLVM's DWARF linker and reader against `addr2line` reading the object files through the
debug map. The Mac's log and kernel of the 2026-11-12 panic are not on this box (and a Linux-built kernel has
other addresses), so without `LOG` the script's log is a stand-in that `oneios-tool debug ksym-make-log` writes from the
two files: the panic's 17 functions in order, for each a return address after its `bl` to the function above it
in the backtrace (else after the middle one of its calls), the pc on the first frame, the lr after
`compute_iaj`'s call of the handler, both as running addresses with slide `0x2ff9000000`. The collection addresses
come from the test's own reader (`oneios-tool debug kcmap`, crate `oneios-testkit`: byte reads over a map, no Mach-O library), not from the
tool. `handle_kernel_abort` is not a symbol of the Linux build (inlined into `sleh_synchronous`): 16 frame lines.

Result on Linux x86_64 (2026-11-13), three pairs: `kernel.kc` `ffbe95f7` with `xnu.macho` `13ed0b70` (`pc`),
`cdd98e7e` with `399f50fa` (`pc2`), `ff87a7ee` with `be249267` (`fpr2`, the tool's own choice). **Every check the
same on all three:**

| Check | Result |
|---|---|
| the log's 19 addresses (caller, pc, lr, 16 frames): function, file:line and every inlined frame against `llvm-symbolizer` | 19 of 19 the same, per pair (17 with file:line; `hosted_exception_upcall_entry` and `Call_continuation` are assembly: function + offset on both sides) |
| the running pc and lr: collection address + slide = the log's address; the pc is the first frame's address | the same |
| the kernel address and the function `debug ksym-make-log` wrote each address for | 19 of 19 |
| each of the 17 functions' collection address three ways: the tool (`--symbol`), `debug kcmap` with the segment's move, the collection's own symbol table | 17 of 17 |
| every instruction of those 17 functions (11,605) and 20,000 more addresses of `__TEXT_EXEC` | 31,605 of 31,605 the same; 31,062 to 31,065 with file:line, 9,542 to 9,682 of them in inlined code. With `SAMPLE=300000` on `pc`: 311,605 of 311,605 (91,504 in inlined code) |
| the same again with the tool reading the dSYM (`--dsym`) instead of the debug map | the same numbers |
| the collection alone (`--no-kernel`): function + offset from the symbols it keeps | 19 of 19 |

At 16 to 21 of the 31,605 addresses (271 of 311,605) the two tools print different names of one address:
`llvm-symbolizer` takes the outermost function's name from the symbol table and of two names the last
(`inet_arp_handle_input`), the tool the one the DWARF has (`arp_ip_handle_input`). Both are symbols of that
address; the script counts these as the same and prints how many. Names are compared mangled, so the two
demanglers' spellings do not count (`cpp_demangle` writes an inherited constructor
`OSSharedPtr<OSArray>::intrusive_shared_ptr(...)`, LLVM `OSSharedPtr<OSArray>::OSSharedPtr(...)`).

**`compute_iaj` at `tcp_input.c:3723`** (the Mac's `atos -i` showed no inlined frame there). In this box's
kernel (the same source line): `compute_iaj` is **not** inlined into `tcp_input`. Of `tcp_input`'s 6,070
instructions none has `compute_iaj` or `compute_iaj_meat` as an inlined frame; `tcp_input+0x1538` is `bl
compute_iaj` on line 3721 (`compute_iaj(tp);`) and the instruction after it, the return address, is line 3723 (the
next statement), which is what the Mac's frame shows. `compute_iaj_meat` **is** inlined into `compute_iaj` (163 of
its 174 instructions), so the fault's lr in `compute_iaj` names `compute_iaj_meat` as an inlined frame (above).
For the Mac's kernel itself this is not checked: its `xnu.macho` is not here.

**Not run:** the Mac's log (`out/session-logs/2026-11-12/boot-0.1.7b/xnuhost.log`) and its kernel (`kernel.kc`
`8e7f4fdc`): not on this box. With them: `compare.sh PATH/kernel.kc PATH/xnu.macho PATH/xnuhost.log`, and
`DSYM=PATH/xnu.macho.dSYM` (or the Mac's build tree's dSYM) because a Mac-built kernel's debug map names the Mac's
object files; without any DWARF the script compares function names only and says so. Not run on a Mac at all (the
script needs `llvm-symbolizer`, which Xcode does not install). A real panic log from the phone: none on the box;
the block's format is `hosted_panic`'s source and XNU's `panic_with_thread_kernel_state` text.

Unit tests (`crates/oneios-ksym`, no Apple file): 13. A fileset-shaped Mach-O made in the test (two members, the
kernel's `__TEXT` moved by `0x180000` and `__TEXT_EXEC` by `0x3870000` as in the Mac build, a segment of no size,
an end label, two names of one address), the physical-aperture rule on the Mac log's numbers, the index with lines
given by hand, a panic block end to end (text and JSON), the pairing warnings, a debug map whose object files
changed or are gone, the collection alone, a plain kernel in place of a collection. `oneios-log`: 1 more (8).
`cargo test --release` for the whole workspace on Linux (2026-11-13): 73 tests pass (`oneios-common` 3,
`oneios-image` 31 + 1, `oneios-ioslog` 4, `oneios-ksym` 4 + 9, `oneios-log` 8, `oneios-phone` 3, `oneios-prelink`
3, `oneios-sym` 7). Run again after the change, because `sym`'s command line and `log stacks` were touched:
`tests/sym/compare-golden.sh` (18 of 18 sets the same as `dsc.py`'s frozen output) and `tests/log/run.sh` on the
two stand-in logs (33 commands, all status 0, two runs equal). `tests/compare-all.sh` (`compare-python.sh` until 2026-11-21) has the part `ksym`.

Limits:
- Kext frames have no function names (above). The pairing records of the prelink are not used for them.
- No end of a symbol is checked: an address after the last function of a segment gets that function with a large
  offset.
- `hosted_panic` prints at most 24 frames and stops at another stack; the register dump is cut at 1,024 bytes of
  message. A `frame` line joined with another record (text after the address) is not taken as an address.
- The index holds what the object files said when it was built. Built while some were stale, it has no lines for
  their functions until `sym kernel index --force` with the files back (or `--dsym`).
- `xnu.macho`'s own `__DWARF` sections (source 2) were not met in a real file; a dSYM (sources 1 and 3 share the
  reader) and the debug map were.

## log

`oneios-tool log stacks|timeline|summary|diff|gpu|cpu|display|perf|pen LOG...` reads an xnuhost session log
([todo_rust.md](todo_rust.md) has each subcommand). No Python original.

`log perf`'s table of the host vCPUs (`perf: cpuN` lines) has a column `host_wait%` since 2026-11-23: the vCPU thread's
wait for a host CPU (schedstat, docs/booter.md 2.6a); empty for an older log. Tested: `cargo test -p oneios-log`
(`synthetic.rs` `report_tables`).

**`log cpu`: the host's side (2026-11-23).** Besides the guest's `oneios-cpu` tables (each report, and by process),
two tables of the host: `host vCPUs by QoS` from xnuhost's `perf: qos:` line (each vCPU per period: `cpu`, `kind`
E or P, `realtime%`, `ui%`, `default%`, `background%` of its wall time in an iOS thread's turn of that class) and `host
clocks` from the thermal bridge's `thermal: N s clocks:` line (each cpufreq policy per period: `policy`, `cpus`,
`busy%`, `mean_mhz`, `top_mhz`, `at_top%`, `under_half%`; a policy with no time counted has `-`). What the lines are:
docs/booter.md 2.6e. A log from before 2026-11-23 has neither: both tables are empty.

**`log summary` (2026-11-22): a session's health on one screen.** Its second line is xnuhost's last `versions:` line
(docs/booter.md 2.9b: the runtime and its commit, the kernel's build id, the staged `kernel.kc`'s SHA-256, the image,
the app's versionCode) with its line number and, when the log holds different ones, how many; a log from before
v0.1.19 says `versions: no line`. A tail of a log has it too (xnuhost writes it again after `stopping:`). Then, for
the log's last boot (`--all`: every boot) it
prints the boot's stages with their time after the boot's start and their line (xnuhost starts, the RAM held, the
kernel, BSD root, launchd loaded and its first line, the first virtio driver up, SpringBoard and backboardd first
seen, backboardd's framebuffer, the first swap, the guest's first frame, guest ready, the first gesture, the end: xnuhost's
last record, or the head line of its last `xnuhost !!` group such as `fatal signal 6 (SIGABRT) on thread ...`; and,
from the OneOS app after 0.5.80, the row `how xnuhost ended` with the inner shell's `exit N` or `killed by signal S
(NAME)`, the one row a SIGKILL leaves, docs/booter.md "Log"; and the row `host short of memory at the end` with the
words of xnuhost's last `memory: the host is short of memory: ...` line when no `memory: the host's shortage is over`
came after it, docs/booter.md 2.2 "The host's pressure"),
then one row per part (start, memory, disks, virtio, tasks, cpu, perf, thermal, display, gpu, input, brightness,
haptics, battery, bluetooth, wifi, audio, camera, encoder (xnuhost's `encoder:`, `decoder:` and, since 2026-11-25, the media worker's `media: 30 s` lines, with the guest's `ONEIOS CODEC:` lines), usb, return, and iOS's own report lines: launchd, memory,
cpu, gpu, display, hid, wifi, iokit): how many lines it wrote, the problems among them (`xnuhost E` lines, and lines
with a word such as `failed`, `dropped`, `none returned`, `died on its own` that is not a zero count), its last line
and its first problem. Then the crashes and kills (panics, `sheng-exc` exceptions, the tasks' fatal traps, tasks
that died on their own, jetsam kills, virtio queues that held chains), the gestures and how many had no guest frame,
and the `xnuhost E` lines with the first one. Which line each part writes: docs/booter.md 2.9b.

```sh
oneios-tool log summary xnuhost.log                 # the last boot
oneios-tool log summary xnuhost.log --all           # every boot
oneios-tool log summary xnuhost.log --since 4900    # the rows from log second 4900 (the stages stay the whole boot's)
oneios-tool log summary xnuhost.log --since +120    # ... from 120 s after the boot's start
oneios-tool log summary xnuhost.log --last 60       # ... the boot's last 60 s
```

On `out/session-logs/2026-11-22/kbd-debug/xnuhost.log` (4584 lines, 0.09 s; written before the fixes of
docs/booter.md 2.9b, so it shows two of the wrong readings), shortened:

```text
boot 1 of 1 (lines 1-4584), 647.8 s of log
stages:
  xnuhost starts                         +0.0 s  line     19
  RAM held and locked                    +3.8 s  line     96
  the kernel starts                      +4.9 s  line    104
  BSD root mounted                       +6.4 s  line    349
  launchd loaded                         +6.7 s  line    486
  launchd's first line                  +28.9 s  line    555
  the guest's drivers: first up               -  (no line)
  SpringBoard first seen                +40.7 s  line    822
  backboardd first seen                 +47.1 s  line    841
  backboardd opens the framebuffer      +47.1 s  line    841
  the guest's first swap                +51.2 s  line    903
  the guest's first frame               +51.4 s  line    905
  guest ready                          +171.5 s  line   1514
  the first gesture                    +191.0 s  line   1606
  the end                              +647.8 s  (no stop line: the session still ran, or xnuhost was killed)
  NOTE: ready came 120.1 s after the guest's first frame (ready-content-wait: the frames read as black)
parts (lines, problems, the last line, the first problem):
  memory          73        -  +634.3 s memory: fixed RAM: held and locked; prefaulted 5632 of 5632 MiB, ...
  display         57       12  +644.4 s display: 10 s: 153 frames copied by the helper from guest RAM ...
                      +10.9 s  first: display: 10 s: 0 frames copied by the helper ... (line 494)
  gpu            188 28 (28 E)  +646.0 s gpu: 10 s level: frames 171, at the top 171; ...
                      +48.4 s  first: drm2kgsl: DRM2KGSL_PWR_CONSTRAINT: queues that ask (line 863)
  ...
crashes and kills: 0 panic(s), 0 exception(s) (sheng-exc), 0 fatal trap line(s) of tasks, 0 task(s) died on their
  own, 4 jetsam kill(s); 0 virtio queue(s) held chains
gestures: 66, 66 with no guest frame after the touch-down (xnuhost's word)
`xnuhost E` lines: 28; the first at +48.4 s (line 863): drm2kgsl: DRM2KGSL_PWR_CONSTRAINT: queues that ask
```

The `NOTE`, the 66 of 66 gestures and the 28 `E` lines are the three wrong readings fixed on 2026-11-22 (direct
frames read as black, direct frames not seen by the touch measure, drm2kgsl's facts at the error level). The
display's 12 problems are real: `N not answered within 100 ms` by the display helper. On
`gpu-debug/xnuhost-1795133100.log` (7040 lines, a session that ended with `exit 0`) the end is `+922.4 s line 7040
exit 0`, and the problems are the helper's `socket is gone` at the stop, 3 fatal trap lines of tasks and 22 touch
events dropped in the guest's HID queue. Tested: `tests/synthetic.rs` `summary_of_a_boot` (two boots, the stages,
the parts' counts and first problems, `--all`, `--since` both ways, `--last`, a refused `--since`) and the unit test
`problems_and_zero_counts` (`0 failed`, `refused 0 calls`, `timed out 0`, `-no_slto_panic` are not problems).

`log timeline` has the same new milestones since 2026-11-22: the RAM held, the guest's first frame, the first virtio
driver up, `return:` lines, tasks that died on their own, virtio queues that held chains or set FAILED, and input that
was dropped.

`oneios-tool log pen LOG [--json | --csv]` (spen, 2026-11-22; `crates/oneios-log/src/reports.rs` `pen`): the S Pen's
path through one session log (`docs/hardware.md` 7f). Four tables: the stages in order, each with its count, its first
line's time, line number and text, or `missing:` and the likely reason (host: device found, guest device, pen IC woken;
guest: transport, HID device, event driver, published, backboardd; host: first event, in range; guest: frames, queue);
the lines that end the path early (`pen: none found`, `pen=off`, `events dropped`, the guest's `oneios-pen=1, but ...`
and `not published`); one row per `pen: in range` line (ms, frames, hovering, touching, strokes, the most pressure, the
hover height, tilt, side-button presses, unchanged, coalesced, hovering under a pixel, sent; a field the line does not
have, as before 2026-11-22, is `-`); and the guest's counts (pen frames, HID reports, sent, stylus events given the
pencil's angles, of a layout not known, queued, dropped). The notes say whether the stylus wrapper is on the pen's
event driver (by the frames line, not by the publish line's `edge wrapper N`) and where iOS's own side is
(`oneios-tool ioslog ... --grep 'pencil digitizer'`). Tested: `pen_report` in `tests/synthetic.rs` (the PC boot's line
formats, a session without the pen, a host that found none) and the malformed-lines test; run on the phone sessions of
2026-11-22 (`kbd-debug/live2.log`: every host and guest stage up to backboardd, no event yet).

`oneios-tool log panic LOG [--kc PATH] [--kernel PATH]` prints the log's kernel panics named through `sym kernel`
("sym kernel" above), and `log stacks` ends with the same part when the log has a panic (`--no-sym` leaves it
out). Without a collection (none given, none under `out/kernel`) the addresses are listed unnamed, with the reason.

```sh
ONEIOS_LOG_DIR=<folder with *.log> tools/oneios-tool/tests/log/run.sh
```

Result on Linux x86_64 (2026-11-12), on the two stand-in logs of `make-log.py` (above): 33 commands, all status 0, no
panic, two runs of each equal; 1 s in all. `stacks` finds each log's slide from its addresses (`0xedb4000`: 5173 of
5173 distinct addresses follow a call) and names every frame through the `sym` index. The stand-in logs hold thread
lines and one crash only, so the tables (`gpu`, `cpu`, `display`, `perf`) and `timeline` had almost nothing to read:
their test on real lines is the Mac's run of the first round (67 commands on four real session logs, all status 0)
and the 7 unit tests on synthetic log text.

## phone

`oneios-tool phone [--serial S] CMD`: the phone on adb. Written against a stand-in (`tests/phone/fake-adb.sh`);
**run on the real phone on 2026-11-12** (the Mac, SM-S928B): `status`, `stage runtime` (runtime 0.1.7 from the
release's asset), `stage image`, `sums`, `image-check`, `switch`, `log --follow --markers`, `log --list`, `log --file`.
`return` has not run on it yet. `pull-data` ran there on 2026-11-13 over wireless adb as the first port's one
gzip stream and broke twice (below, "The copy that goes on"); the copy in chunks that replaced it **has not run
on the phone**. `scripts/phone/stage.sh` stays until they have.

```sh
oneios-tool phone status                      # root, the host phone, the installed app, the session, the boot screen's step (`control/bootui/progress`, `next`), `oneios_root.sh status` (read only)
oneios-tool phone stage runtime DIR           # an unpacked runtime package, through oneios_root.sh begin/receive/finish
                                              #   (finish's ten fields: runtime.json's kernel build_id, sha256, xnu last,
                                              #   or `none` each; the fork's script since 53abb04, fixed here 2026-11-21)
oneios-tool phone stage runtime-asis DIR      # the same with each file's own size and SHA-256
oneios-tool phone stage image DIR             # files of an image set at their paths; image.sha256 and image.ids follow
oneios-tool phone status --watch [SECONDS]    # the same again every 5 s until Ctrl-C
oneios-tool phone log [--file NAME] [--tail N] [--follow] [--save PATH | --save-auto] [filters] [--since S] [--analyze]
oneios-tool phone log --list                  # the logs by name, with sizes
oneios-tool phone sums                        # sha256 of the staged kernel.kc, its hash list, manifest.json, data.seed's line, xnuhost.conf, the runtime
oneios-tool phone image-check                 # the staged manifest.json against the staged files (image.sha256), as xnuhost will (read only)
oneios-tool phone manifest DIR [--out FILE] [--stage]   # the phone's manifest.json with DIR's files' entries set to theirs
oneios-tool phone addon FILE [--cdhashes LIST]   # a boot addon (ios addon) as image/kernel.kc.gpuaddon, owner-trust.json plus its CDHashes, a manifest
oneios-tool phone addon --remove              # no boot addon at the next boot (owner-trust.json is left as it is)
oneios-tool phone switch [--version-code N]   # busybox setsid sh oneios_root.sh switch <the installed app's versionCode> 1
oneios-tool phone return [--timeout 180] [--note TEXT]   # session/return.request, then waits for "One UI restored"
# only during a session (refused without one; "The session's own commands" below):
oneios-tool phone display-restart [--lib PATH] [--dex PATH] [--timeout 30] [--kill-after 3]
                                              # end the display helper (control/display.pid) and start it as the switch does;
                                              #   waits for `first frame posted` in control/display.log
oneios-tool phone touch status                # /sys/class/sec/tsp/enabled: 1 on, 0 off (read only)
oneios-tool phone touch wake                  # 2,0 then 2,1 to that node, then reads it back
oneios-tool phone pen status                  # /sys/class/sec/sec_epen/enabled (the S Pen's IC): 1 on, 0 off (read only)
oneios-tool phone pen wake                    # 2,0 then 2,1 to that node, then reads it back
oneios-tool phone backlight status            # the panel's level, max_brightness, One UI's own value (read only)
oneios-tool phone backlight set N             # 1..max_brightness (12750); the return writes One UI's own value back
oneios-tool phone cpu [--trace S] [--out F]   # cores, domains, top and capped clocks, cpusets, xnuhost's threads and uclamps (read only)
oneios-tool phone hw [--out DIR]              # the hardware map of a new port: fdt.dtb, iomem, interrupts, partitions, kernel.config (read only)
oneios-tool phone wifi p2p [--station IFACE] [--save FILE]   # the staged wifictl p2p as root (read only): every radio, Wonder,
                                              #   the AirDrop verdicts, Wonder's files; then a summary of those lines
oneios-tool phone wifi wonder [--seconds 30] [--freq 2437|5220|5745] [--country CC] [--save FILE]
                                              # wifictl wonder (docs/hardware.md 4a), its lines as they come (adb exec-out)
oneios-tool phone qrtr [--sensors] [--binary FILE] [--save FILE]
                                              # read only: can a root process beside One UI reach the sensor hub (SSC) over
                                              #   QRTR: sensorctl qrtr lists the QRTR nodes and services, says whether the SNS
                                              #   service (400) is there and answers a lookup; --sensors: every SUID and its
                                              #   attributes (no stream). --binary: this machine's sensorctl pushed to
                                              #   /data/local/tmp/oneios-sensorctl, run, removed (runtime/bridges/sensors/README.md)
oneios-tool phone procs [--seconds 5] [--sort cpu|mem] [--top N] [--kernel] [--raw] [--from FILE]
                                              # every process with its CPU share and RAM, with or without a session (read only):
                                              #   the session, the fork's watchers, the kernel's threads, and each Android or
                                              #   vendor process that still runs beside iOS ("The process list" below)
oneios-tool phone pull-log DIR [--boot last|UUID] [--keep]   # iOS's log only: ioslog show runs on the phone (the runtime's
                                              #   oneios-tool on the staged store; an open session is fine, its newest
                                              #   checkpoint is read), only the gzipped text crosses adb; then DIR/ioslog.txt,
                                              #   ioslog-health.txt, ioslog-keyboard.txt, ioslog-haptics.txt. 2026-11-22: 615713 entries, 11 MB over
                                              #   adb, 9.4 s (pull-data: 0.8-2.8 GB, about 7.5 min, the session ended first).
                                              #   A boot with no entries in the store as read (2026-11-23 beside an open
                                              #   session: a 20-byte gzip, nothing said) ends with an error that lists the
                                              #   store's boots (the phone's ioslog boots) and what to try: after the return,
                                              #   or --boot with a listed UUID
oneios-tool phone pull-data OUT [--ioslog]    # data/data.img in chunks, each checked against the phone's sha256, zero runs as holes;
                                              #   run again after a stop and it goes on (OUT.part, OUT.part.progress)
                                              #   [--chunk MIB] [--retries 5] [--backoff 2] [--stall 120] [--verify chunk|end] [--restart]
oneios-tool phone sparse-write OUT [--gunzip] [--expect-size N]   # stdin to OUT, zero runs as holes (no phone)
```

**The host phone in `status` (2026-11-21).** After `root: yes`, one line: `host: galaxy-s24-ultra (Galaxy S24 Ultra
(SM-S928, SM8650)), matched by ro.product.model=SM-S928B; defaults: pen=auto, encoder=auto` (xnuhost's table's S24 Ultra row since 7c70ef93; this mirror followed on 2026-11-28), or for another known phone its
defaults (`galaxy-s26-ultra ... defaults: ONEIOS_BRIGHT_MAX=2300, ONEIOS_AUDIO_PROBE_PREFILL=1`, `galaxy-a56 ... defaults: gpu=auto, panel-fallback=1080x2340@120, width=720, height=1560, scale=2, hz=60, return-graceful-ms=4000, astc=0, hw-backend-haptics=treble, hw-backend-fallback=1`), or `host: unknown
phone (ro.product.model=... ...): xnuhost keeps today's defaults (host-device=NAME forces one)`. One `adb shell` call as
the shell user: `getprop` of `ro.product.model`, `ro.product.device`, `ro.board.platform`, `ro.soc.model`,
`ro.soc.manufacturer`, and `/sys/devices/soc0/{machine,soc_id}`, `/proc/device-tree/compatible`; matched in xnuhost's
order against `crates/oneios-phone/src/host.rs`, a copy of `runtime/bridges/hw/host_device.rs`'s table (the unit test
`table_matches_the_bridges` reads that file and fails when they differ). It says what xnuhost's own start line will
say about the phone (`docs/hardware.md` 7h), not what a session's command line or image set overrides. Tested: the
unit tests (each phone, a codename, the SoC, the platform, soc0, the compatible list, unknown) and the stand-in phone
(`tests/phone/run.sh`: the S24 Ultra, S26 Ultra, A56, S25 Ultra, an unknown phone and soc0's machine with no
properties, via the stand-in's `getprop` reading `.fake/props`). Not run on a phone.

**The local forms (2026-11-15): the same readings on the phone itself, without adb.** The OneOS app runs the tool
on the phone (`oneios-tool serve`, "serve" above), where there is no adb and `serve` starts no shell. Four of the
commands above have a request there that does the same reading from the stage's and the phone's own files
(`crates/oneios-phone/src/local.rs`, and `sums` in `crates/oneios-serve/src/cmds.rs`):

| adb form | `serve` request | The same? |
|---|---|---|
| `phone sums` (`sha256sum` on the phone of six named files) | `sums` of a folder against its list (`image` and `image.sha256`, `runtime` and `runtime.sha256`): every file of the list, the files in parallel | every row's SHA-256 is `sha256sum`'s, and `kernel.kc`'s is `phone sums`'s, on the stand-in phone (`tests/phone/serve.sh`) |
| `phone log --list` | `log-list` | the seven named logs with their sizes, and the earlier sessions' (`logs/xnuhost-EPOCH.log`: the stage's own folder is listed) |
| `phone log [--file] [--tail] [filters] [--since]` (once, no `--follow`) | `log` | the filtered lines are `phone log`'s, as `line` events (`tests/phone/serve.sh`); `--follow`, `--save` and `--analyze` stay with the adb form |
| `phone cpu` | `cpu` | the same `cpuN`, `cpufreq boost`, `cpuset` and `xnuhost` lines from the same named files (the CPUs 0 to 7 by name, the four cpusets, xnuhost's `cmdline`, `status`, `cpuset`). Where the adb form lists xnuhost's threads (it asks `ps -T`), the local form gives xnuhost's own line from `/proc/PID/stat` (thread count, last CPU, nice): it starts no program and lists no folder of `/proc`. No `--trace` (perfetto is a program) |

They read **named files only** (a unit test reads the module's source for a folder listing: only the stage's `logs/`
is listed), write nothing unless an `out` file under the stage is asked for, and run beside a session. Tested on
stand-in files and on the stand-in phone; **not run on the phone**.

The rules of `docs/building.md` section 6 are in the tool (`crates/oneios-phone/src/adb.rs`):
- **One phone only.** `--serial`, `ANDROID_SERIAL`, or the only phone attached; several attached is refused, and
  so is a phone adb lists as anything but `device`.
- **Root first.** Every command asks `su -c 'id -u'` before anything else and says so when the answer is not 0
  (KernelSU's root does not survive a restart of the phone). Nothing is changed then.
- **Named files only.** No command sends `find`, `ls -R`, `du` or any walk of a tree to the phone (a walk of
  sysfs or /proc panicked the phone's kernel on 2026-11-12). `stage image` lists the *local* folder.
- **Nothing but status, return, log and the session's own commands runs beside a session.** A session is open when
  `session/xnuhost.pid` names a live process that is still an xnuhost (stage.sh's test). `sums` refuses too, which
  stage.sh's does not: it reads the whole kernel collection.
- **The session's own commands run only in a session** (`display-restart`, `touch`, `backlight`): without one they
  are refused and nothing is changed, because the display helper, the touch IC and the panel's level are One UI's
  then.
- **No restart, nothing under `/sys/power`**, and nothing of One UI's is written: only the stage, through the
  fork's root script or stage.sh's own commands, and in a session the touch IC's node and the panel's level, which
  the return hands back to One UI.
- Words that go into a script for root's shell (file names, `runtime.json`'s fields, the note of `return`) are
  refused or cut down unless they are letters, digits and `._+-/,`.

What each does:
- `image-check` reads the staged `image/manifest.json` and `image.sha256` (the sums the app and `stage image` write
  when they stage a file; nothing is hashed on the phone) and names every entry whose staged file has another
  SHA-256 or is missing. xnuhost refuses such a set ("trust caches of /data/image: a file of the image set is not
  the one manifest.json lists"): on 2026-11-12 23:04 a cycle staged the Mac's own `out/image/24A446/manifest.json`
  (another set: `data.seed` `58db87e4`, the phone's is `f201d6ca`) with a new kernel, and the session ended 2 s
  after One UI was stopped. `switch` runs it first and refuses before anything is stopped; `stage image` runs it
  after staging and warns.
- `manifest DIR` makes the manifest for files staged on their own (a new `kernel.kc` and `kernel.kc.hashes`): the
  phone's `manifest.json` (read with oneios-common's `json`, so the key order is kept, and written with an indent of 2,
  the layout the fork's `manifest_rekernel` awk reads line by line), each entry of DIR's
  files set to their SHA-256 and size. Every other entry must already list the staged file, and a file of DIR
  that the manifest does not list is refused. Printed, `--out FILE`, or `--stage`: DIR's files and the manifest
  staged together, then `image-check`. Run on the phone: the manifest it replaced by hand that night (21 entries,
  all equal to `image.sha256`). The kernel's boot addon `kernel.kc.gpuaddon` is the one file DIR may hold that the
  manifest does not list (xnuhost reads a sidecar beside the kernel itself): it is staged new, its lines added.
- `addon FILE` (2026-11-23, hosted 0090): `ios addon`'s sidecar staged as `image/kernel.kc.gpuaddon` with the phone's
  `overlay/userland/owner-trust.json` plus the CDHashes of `FILE.cdhashes` (`--cdhashes LIST`; sorted, each once, every
  other key kept) and a manifest for both, through `manifest --stage`. Refused: a file that is not NGPUADD2, one over
  64 MiB, a set whose manifest lists no owner-trust.json. `--remove` deletes the sidecar and its lines. Tested on the
  stand-in phone (`tests/phone/run.sh`, "phone addon"); not run on the phone yet.
- `switch` reads the versionCode from `dumpsys package com.fahadali.oneos` (not a fixed 115; `--version-code`
  for a phone without the app). A refusal of the preflight (`@@REFUSED ...`) is printed and is exit status 1. If
  `setsid` lets adb go before the script answers, the new `epoch` line of `switch.log` is the proof it started.
- `return` does nothing when no session is open. Otherwise it notes `return.log`'s length, writes the request,
  and reads only what the log gained, every 2 s, until `One UI restored` is there; then it prints
  `return.outcome`. At `--timeout` it ends with status 1 and the last lines of the log (and says when the phone
  no longer answers: a restart is the return's failsafe).
- `pull-data` copies `data/data.img` in chunks of `--chunk` MiB (256), each one adb stream of a range of that
  one named file, unpacked with `flate2` (`zlib-rs`) and written at its offset into `OUT.part` (a block of zeros
  is not written: "The sparse copy" below). Each chunk is checked against the phone's SHA-256 of the same range.
  A stream that breaks is tried again (`--retries`), and a run that stopped, was killed or gave up goes on at
  the first chunk that is not done when the same command is run again. `OUT.part` becomes `OUT` only when every
  chunk is verified and the store's size and modification time are what they were at the start. `--ioslog` then
  runs `ioslog boots OUT`, `ioslog checks OUT` and `ioslog health OUT`: the health report with its details goes
  to `ioslog-health.txt` in OUT's folder, and its head and one line per kind are printed ("health" above). A step
  that fails ends the command with status 1 (`ioslog boots ended with status 1`; `ioslog health: REASON`), and the
  copy stays. `phone log` itself reads the stage's log files only, never the Data store: `pull-data --ioslog` is
  the `phone` command that reads it. "The copy that goes on" below has the design, the options and the test.
- `sparse-write OUT` is the copy without the phone, and the first port's `pull-data` by hand (one stream, no
  resume, no sums): `adb exec-out "su -c
  '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" | oneios-tool phone sparse-write
  OUT --gunzip`. stdin goes to OUT; `--gunzip` unpacks it first; `--expect-size N` fails when another number of
  bytes came. It prints `OUT: N bytes, M of them written (S s)`.

- `log` reads one named file of the stage and runs beside a session (it only reads). `--file`: `xnuhost`
  (`session/xnuhost.log`, the default), `switch`, `return`, `outcome`, `display`, `watchdog`, `usb` (the names of
  `oneos/interface.md` section 3), or `logs/xnuhost-<epoch>.log` for an earlier session. `--list` prints each name
  with its size and time (`stat` of the named files, and the earlier sessions as the fork's read-only
  `oneios_root.sh logs` gives them): no folder of the phone is listed.
  - `--tail N`; `--follow` (`tail -n +K -f` on the file's descriptor; before each connection one root script
    looks at the file: `stat %i %Z`, `wc -l`, its first line, and for `xnuhost` switch.log's last `epoch`):
    - after a switch only a log changed at or after it counts (the cycle's watch keeps the same rule): the switch
      replaces `session/xnuhost.log` only about 40 s in, so a follow started right after `phone switch` says the
      file there is the last session's and waits for this session's, printing none of the old one;
    - when adb drops, the follow waits (1 to 5 s) and carries on after the last whole line it got when the file is
      the same one (the same inode and first line, and not fewer lines than were read): no line lost or twice;
      another file (another inode, even with the same first line, or another first line, or fewer lines) is read
      from its start;
    - while it reads, a look every 2 s ends the stream when the file was replaced (a switch while following), and
      the new file is read from its start (`-F` would follow the new file inside the same stream, so the count of
      lines read would mix the two files: the phone, 2026-11-12, printed the last session's log and, after a drop,
      the whole current one a second time).
    Ctrl-C ends it with status 0.
  - `--save PATH`, `--save-auto` (`out/session-logs/DATE/live-HHMMSS.log`): **every** line read goes to the file,
    whatever the filters print.
  - Filters (a line is printed when it matches any of them; none: every line): `--grep REGEX` (repeatable),
    `--panic` (`panic(`, `xnuhost !!`, `xnuhost E`, `sheng-exc`, `sheng-exit`, `crashed:`, `stopping:`, `exit`),
    `--display` (`oneios-display`, `sheng-vsync`, `ONEIOS BACKLIGHT`, `ONEIOS ALS`, `oneios-swap`), `--gpu`
    (`oneios-gpu*`, `sheng-gpu`, `oneios-adreno`), `--wifi` (`ONEIOS WIFI`, `oneios-wifi`, `sheng-wifi`,
    `oneios-net`), `--hid` (`oneios-hid`, `ONEIOS HAPTIC`, `oneios-haptic`, `ONEIOS VIBRATOR`, `oneios-actuator`, xnuhost's `haptics:` and `DroidVMAudio:`; fix-haptics 2026-11-13),
    `--mem` (`oneios-swap`, `oneios-fault-around`, `oneios-gpu-memory`, `memorystatus`, `jetsam`). `--pen` (spen,
    2026-11-22): xnuhost's `pen:` lines, the guest's `oneios-hid: pen:`, `droidvm input /arm-io/input-1` and
    `oneios-hid: virtio-input:` (the S Pen's path, `docs/hardware.md` 7f; tested on the PC boot's formats). The prefixes
    are the ones `docs/booter.md`, `docs/hardware.md`, `docs/gpu.md` and the hosted patches' README name; they were
    not checked against a real session's log here.
  - `--markers`: the stages of `oneos/interface.md` section 8, each once per boot (xnuhost, kernel, darwin,
    bsd-root, init, launchd, backboardd, SpringBoard, ready), and every panic and end line.
  - `--since SECONDS`: from that second of the log's own `[seconds.millis]` stamps on.
  - `--analyze`: when the reading ends (the file's end, or Ctrl-C of a follow), `oneios-tool log timeline` of the
    saved lines, and `oneios-tool log stacks --summary` when a `sym` index exists (`out/dsc-index/latest` here or
    in the main checkout, or `ONEIOS_DSC_INDEX`). It is not line by line: the analysis is of the log so far.

```sh
tools/oneios-tool/tests/phone/run.sh          # 258 checks against the stand-in phone (94 of them session-cmds.sh: display-restart, touch, backlight;
                                              #   24 of them wifi.sh, 2026-11-23: phone wifi p2p and wonder against a stand-in wifictl (on the Mac:
                                              #   251 ok, hw-backend and serve cpu skipped there; ALL OK);
                                              #   30 of them usb.sh: with a fork checkout that has --usb-watch (found as `app dir` finds it);
                                              #   34 of them hw-backend.sh (12 the bridgectl haptic checks, 2026-11-21): the bridges' samsung and treble backends on stand-in
                                              #   phones, their host-built tools; docs/hardware.md 7e;
                                              #   12 of them serve.sh, 2026-11-15: `oneios-tool serve` on the stand-in's stage,
                                              #   its local forms against the adb forms, the refusals in a session); ALL OK
                                              #   since 2026-11-25 also qrtr.sh's 14 (`phone qrtr` with a stand-in sensorctl: staged,
                                              #   --binary pushed and removed, a closed QRTR);
                                              #   since 2026-11-23 also procs.sh's 38 (`phone procs` on a made-up /proc). The Mac,
                                              #   2026-11-23: 265 ok, ALL OK (hw-backend.sh skipped: the bridges' host tools do not
                                              #   build there; before that day the suite failed on the Mac: lib.sh, below)
tools/oneios-tool/tests/cycle/run.sh          # phone log, status --watch and cycle: 117 checks; ALL OK
tools/oneios-tool/tests/phone/modes.sh        # phone, app and cycle in each /proc mode this host has (below); ALL OK
FAKE_PROC=stand-in tools/oneios-tool/tests/phone/run.sh   # one suite with the stand-in's own /proc (macOS's way)
tools/oneios-tool/tests/phone/sparse-compare.sh   # sparse-write against the stream and cp --sparse: 28 checks; ALL SAME ("The sparse copy" below)
```

The stand-in phone: `fake-adb.sh` runs what the tool sends on this machine, inside a folder that plays the
phone's file system (`/data/...` becomes `FOLDER/data/...`); `su` is a stand-in that can refuse; the stage's
`bin/oneios_root.sh` hands `status`, `begin`, `receive` and `finish` to **the OneOS fork's own root script under
its test root** (`ONEIOS_TEST_ROOT`), and plays `switch` (a process named xnuhost whose log grows with a boot's
lines) and the return (the restore line, `return.outcome`). `stage` and `sums` are compared with
`scripts/phone/stage.sh` itself on a second, equal stand-in: the same output and the same stage afterwards
(`runtime`, `runtime-asis`, `image`, a file that is not staged). Result, Linux x86_64, 2026-11-12: 64 checks, all
ok, two runs. What the stand-in cannot show: the real `su`, adb's behaviour when One UI's framework stops, the
time a 64 GiB store takes (6 min on the Mac with the Python script), the fork's real switch and return.

**/proc, and macOS.** The session check reads `/proc/PID/cmdline` on the phone (`adb.rs`
`session_check_script`; a unit test and a stand-in check, which compares the root script the tool sent, keep it
the phone's command byte for byte). The stand-in answers it in one of two modes, `FAKE_PROC`:
- `host`: the phone's commands read this machine's `/proc` (Linux; the default where `/proc/self/cmdline` can be
  read);
- `stand-in`: `/proc/...` in what the tool sends becomes the stand-in's own `FOLDER/.fake/proc/...`
  (`fake-rewrite.sh`, the same rewrite that maps `/data`), where each stand-in session (`fake-session.sh`, named
  `fake-xnuhost`) keeps `PID/cmdline` while it runs and removes it when it ends; `fake-adb.sh` drops the entry of
  a process that is gone, as the kernel does. macOS has no `/proc`, so this is its default; on Linux
  `FAKE_PROC=stand-in` runs the same path, and `modes.sh` runs the phone, app and cycle suites in both modes.
- **Several runs at once** (several agents on one box): each `modes.sh` run has its own folder
  (`out/rustport-modes/run.XXXXXX`, or `MODES_RUN=DIR`), gives each suite its own `WORK=` in it, and counts only its
  own stand-in sessions; the cycle suite runs its stand-in build steps on a tmux server of its own (`TMUX_TMPDIR` a
  short `/tmp/oneios-cycle-tmux.XXXXXX`, since a Unix socket path is at most 108 bytes and a deep worktree's work
  folder is longer; `TMUX` unset) and stops only that server at its end. The cycle's `summary.md` check reads
  the fork checkout's own `versionName`/`versionCode` (it moves with each upstream merge). Checked 2026-11-15: two
  `modes.sh cycle` runs at once, against the fork's `oneios` (0.4.86) and PR #3's merge (0.5.14): both 103 ok in both
  modes, no session left on the box's tmux server.

**macOS's bash 3.2 (2026-11-23).** The suites run under `/bin/bash`, which is 3.2 on the Mac. `lib.sh`'s
`file_entries` (2026-11-21) had a `case "$p" in bin/*) ...` inside `$( )`: bash 3.2 takes that `)` for the end of the
substitution, so the stand-in phone's `manifest.json` and `runtime.json` got no `files` list and many checks of
`tests/phone/run.sh` failed on the Mac (`manifest.json has no "files" list`). The pattern is now `(bin/*)`, which
both bash versions read. Seen and fixed while adding `procs.sh`: before the fix 168 ok and the rest FAILED, after it 265 ok and none FAILED.

Before this (the Mac, 2026-11-12) the stand-in on macOS read a `/proc` that is not there: the tool never saw a
stand-in session (status said none, the refusals in a session did not come, a return found "no session"), and
the session lived on into the next case, where the fork's root script refused the switch. Simulated on Linux with
the stand-in's `/proc` pointed at a folder that does not exist: the phone suite failed the Mac's 12 checks
exactly, the cycle suite 31 of the Mac's 34. Of the other three, `log --since` and (probably) the collected log's
comparison raced a log that kept growing because its session never ended. `log --analyze without a sym index`
failed on the Mac for another reason: `--analyze` looks for the index in the main checkout of the folder's
repository, and the test's folder lies inside the checkout's `out/` (the Mac's checkout has an index); the test's
folder is now a repository of its own.

Sessions: every case ends the stand-in session it left (`end_case`, so that one failure does not spread to the
next case), and `lib.sh`'s traps end every stand-in session of the suite (its process group: fake xnuhost and
what it started), and the suite's background jobs, on exit, a failure or Ctrl-C. Each suite's last check is that
none is left running. The checks use GNU coreutils and sed (`stat -c`, `date +%N`, `sed -i`, `timeout`,
`sha256sum`): on macOS put Homebrew's `coreutils` and `gnu-sed` gnubin folders first on PATH (the suites say so
and stop otherwise). `setsid` comes from perl where it is missing (macOS). The rewrite uses `sed -E`, which GNU's
and BSD's sed read the same way.
Ctrl-C while the tool runs: the tool catches SIGINT and ends with status 0, so bash runs no INT trap and would go
on; a small perl watcher in `lib.sh` turns the Ctrl-C into a SIGTERM for the suite, which then stops (status 143)
and ends its sessions.

Result, Linux x86_64, 2026-11-13 (`modes.sh`, two runs): `host` mode: phone 76, app 65, cycle 95 checks, all ok;
`stand-in` mode: the same counts, all ok; no stand-in session left after any suite; a Ctrl-C sent to a cycle
suite's process group while a session ran stopped it with none left, in both modes; `cargo test -p oneios-phone`:
5 passed. Before the change, on the same box: the host mode passed (phone 73, cycle 90 checks); the stand-in's
`/proc` pointed at nothing (the Mac's case) failed 12 and 31 (above). **Linux only**: macOS itself was not run
here; there, `stand-in` is the only mode, and the GNU tools and perl's setsid are the Mac's.

### The process list (`procs`, 2026-11-23)

`phone procs` answers "what of One UI still runs beside iOS, and what does it cost" (`docs/plan-ios-first-session.md`:
the plan it is stage 1 of, and what to capture on the phone). Read only, with or without a session; root is needed
(the phone hides other users' `/proc` entries from the shell user). **Not run on the phone yet.**

```sh
oneios-tool phone procs                       # two CPU readings 5 s apart, then the report
oneios-tool phone procs --seconds 30 --raw > procs.raw   # the phone's own lines, to keep
oneios-tool phone procs --from procs.raw --sort mem      # the report of a kept capture, with no phone
# session: xnuhost pid 21870
# host: CPU 352% of 800 (user 152%, sys 187%, irq 13%), 8 CPUs online      (xnuhost's `thermal: 30 s: host CPU` figure)
# memory: MemTotal 11084 MiB, MemAvailable 1996 MiB, swap 656 of 12287 MiB used, Shmem ..., Mlocked ..., page tables ...
# group                                              procs threads    cpu%  anon MiB  swap MiB   rss MiB
# the session (xnuhost, iOS's tasks, the helpers)    ...
# the fork's watchers (the way back)                 ...
# adb (this command's shell is in it)                ...
# the kernel's threads                               ...
# Android and the vendor                             ...
# init services: N running of M; zygote stopped, surfaceflinger running, ...
# Android and the vendor: N processes, by CPU, then anon + swap:
#     pid   cpu% anon MiB swap MiB  rss MiB  thr  service          command          role
# the session's own processes:        (xnuhost, the task factory, the helpers, the display helper; iOS's tasks as one line)
# the kernel's threads at 0.1% of a CPU or more (the first 8; --kernel: all):
```

- **What is read** (one script for root's shell, `procs.rs` `procs_script`): `/proc/stat` and every `/proc/PID/stat`
  (one `cat` of the files `ps -A -o PID=` names), the phone waits `--seconds`, the same again; then `grep -H` of six
  keys (`Uid`, `VmRSS`, `RssAnon`, `RssFile`, `RssShmem`, `VmSwap`) of every `/proc/PID/status`, `ps -A -w -o
  PID=,ARGS=`, `getprop` (init's services: `init.svc.NAME`, `init.svc_debug_pid.NAME`), `/proc/meminfo`, and the
  stage's `session/xnuhost.pid` and `control/display.pid`. Named files only: no glob, no directory walk, nothing
  written, no signal. No `smaps` or `smaps_rollup`: that read walks a process's page tables under its mmap lock,
  which would stall the maps and unmaps of iOS's tasks. So there is no PSS: `anon` (RssAnon) is a process's own
  pages, and `rss` counts shared file pages once per process.
- **cpu%** is of one CPU: a process's ticks in the span (utime + stime, and cutime + cstime: the children it waited
  for, so a shell loop is charged the short commands it starts) over the ticks one CPU had (the sum of `/proc/stat`'s
  `cpu` line over the CPUs online). A process that started inside the span counts from 0. A parent that reaps a
  long-lived child inside the span is charged that child's whole life once.
- **Groups.** The session: the pid of `session/xnuhost.pid` when that process is an xnuhost, and the pids of
  `control/display.pid`, each with everything it started. The fork's watchers: every process whose command line
  names `oneios_switch.sh`, `oneios_return.sh` or `oneios_root.sh`, with its children (the key readers). adb: `adbd`
  and its children, the tool's own shell among them. The kernel's threads: `PF_KTHREAD`. Android and the vendor:
  the rest.
- **service** is the init service whose pid init publishes for that process, or `NAME+` for a process a service
  started. **role** is what the repository's docs say about it (`procs.rs` `role`, the plan's table): `bridge: ...`
  (one of our bridges needs it), `keep: ...` (the way back, the keys and the TEE, the thermal and charge limits),
  `restart list; ...` (zygote's restart list, which the return restarts at every return), or `-`: **not
  classified, which is not "safe to stop"**.
- `--seconds 0`: one reading, no CPU figures, no wait. `--top N`: the first N Android processes. `--kernel`: every
  kernel thread. `--raw`: what the phone gave; `--from FILE` reads it again and needs no phone.
- Cost on the phone: three `ps`, two `cat`, one `grep`, one `getprop`, and a `sleep`.

Tested: `cargo test -p oneios-phone procs` (the script's words, a stat line whose name holds spaces and
parentheses, the groups, each figure of a session's report, both orders, one reading, a cut output, a pid file that
names no xnuhost, a shell loop's ended commands, the roles), and `tests/phone/procs.sh` from `run.sh` on a stand-in
phone with a made-up `/proc`, `ps`, `sleep` and `getprop` (the same scene through adb's stand-in, what was sent to
the phone, `--raw` and `--from`, a process that ended before the read).

### The session's own commands (`display-restart`, `touch`, `backlight`)

In the session of 2026-11-14 the Mac went to adb and scripts three times because the CLI had no command: to start
the display helper again (it had failed mid-session from a library fault, fixed since in `f7bb1c3`), to wake the
touch IC (`2,0` then `2,1` to `/sys/class/sec/tsp/enabled`, after a brightness of 0 had put it to sleep), and to
set the panel's level (5350 written to `/sys/class/backlight/panel0-backlight/brightness`). These are those three
(`crates/oneios-phone/src/session.rs`). **Not run on the phone yet**: written and tested against the stand-in.

What all of them keep:
- Root first, then **a session, or a refusal** (`no session is open: ... is One UI's while no session runs`):
  without a session the display helper, the touch IC and the panel's level are One UI's, and nothing is written.
- **xnuhost and iOS are never touched**: no signal, no file of `session/` written, nothing asked of the guest.
- **The return works during and after each.** The new helper's pid is in `control/display.pid`, where the return
  looks for it; the return writes One UI's own backlight value back; a restart that is waiting for its helper
  when a return comes ends with status 1 and the return goes on (tested).
- **Named files only**: `control/display.pid`, `control/display.log`, `control/state`, `control/base/backlight`,
  `session/return.request` (its existence), `/proc/PID/cmdline` of the one pid a pid file names,
  `/proc/PID/ns/mnt` of the script's own shell and of init, `/proc/PID/oom_score_adj` of the helper it started,
  `/sys/class/sec/tsp/enabled`, `/sys/class/backlight/panel0-backlight/brightness` and `max_brightness`,
  `/sys/fs/cgroup/oneios_session/cgroup.procs`. No listing, no `pidof` or `pkill`, nothing under `/sys/power`
  (a unit test and a stand-in check hold the list).

**`phone display-restart [--lib PATH] [--dex PATH] [--timeout 30] [--kill-after 3]`**

1. *The local files first.* `--lib` must be an arm64 shared object and `--dex` a dex (the first bytes), else the
   command stops before the phone is asked anything (a host build, or the two options swapped).
2. *The session's state.* Refused while the switch is still starting the session (`control/state` `starting`: the
   switch starts the helper itself) and while a return is under way (`control/state` `returning`, `returned` or
   `failed`, or `session/return.request` is there: the return ends the helper). The script that stops and starts
   the helper looks again itself (the session check, `control/state`, `session/return.request`) before it does
   either: a session that ended or began to return after the tool looked is refused there, with nothing stopped
   or started.
3. *`--lib`, `--dex`.* The file is pushed to `/data/local/tmp/oneios-display-tmp/` and root's script takes it from
   there: the staged file (`runtime/bin/liboneios-display.so`, `runtime/lib/oneios-display.dex`) must be there and
   listed in `runtime.sha256`; the pushed copy's SHA-256 must be the local file's; it is copied beside the staged
   file as `.part` (root's, the staged file's mode), hashed again, and renamed over it. The SHA-256 is printed for
   both sides (`--lib: local SHA PATH`, `--lib: phone SHA runtime/bin/...`). A copy on the phone that differs is
   refused: the staged file and the lists stay as they were, the pushed copy is removed and the running helper is
   not touched.
   **The runtime's lists are updated, not left** (the file's lines in `runtime.sha256` and `runtime.ids`, the way
   `stage image` rewrites `image.sha256` and `image.ids`). The reason: the fork's preflight hashes every file of
   the runtime against `runtime.sha256` and refuses the switch when one differs (`oneios_switch.sh`,
   `check_list runtime full`), and `phone sums` prints that list, not a new hash; the cycle's `stage` step decides
   by it whether to stage the runtime. A list left alone would name a file that is no longer there: the next
   switch refused (`runtime/bin/liboneios-display.so does not match its SHA-256`), and `sums` and the cycle saying
   the runtime is the package's when it is not. With the lines rewritten the lists say what is staged: the next
   switch starts with the new helper files, `phone sums` (after the return; it refuses in a session) shows them,
   and a cycle whose package has other files stages the runtime again. What stays as it was: `runtime.conf`'s
   version and the entry in the staged `runtime/runtime.json`, so the staged runtime is no longer the package they
   name; the command says so, and `phone stage runtime DIR` puts a whole package back. This is the one change of
   the stage that is made while a session is open: the two files are read by the helper on One UI's side only.
4. *The old helper.* By `control/display.pid`, and only a pid whose `/proc/PID/cmdline` holds `OneiOSDisplay` or
   `oneios-display` (the test of the fork's `end_display`): SIGTERM (the helper removes its layers and exits 0),
   and SIGKILL when it is still there after `--kill-after` seconds (surfaceflinger drops a dead client's layers).
   A pid that is gone, or that another process has by now, is left alone and said. /proc is not walked: a helper
   started by hand without the pid file is not found; it leaves when the new one connects (xnuhost serves one
   helper and closes the older one's socket: `runtime/display/README.md`, "Leaving").
5. *The new helper: the switch script's own command.* The fork has no way to start the helper again: `oneios_root.sh`
   has no verb for it and `oneios_switch.sh` no mode (read at OneOS `oneios` `3876dd5`: its modes are the session,
   `--check`, `--inner`, `--watch`, `--keys`, `--keys-poll`, `--feed`, `--bootui`, `--stop-loop`, `--usb-save`,
   `--usb-watch`, `--clocks-save`, `--clocks-tops`). So the tool sends the lines the switch runs ("The guest's
   screen" in `oneios_switch.sh`), built from named files of the stage:

   ```sh
   # DH=/data/local/tmp/oneios/runtime/bin/oneios-display: a regular file, executable, root's, not writable by others
   $NS /data/adb/ksu/bin/busybox setsid "$DH" --session /data/local/tmp/oneios/session --takeover --wait 60 \
       </dev/null >>/data/local/tmp/oneios/control/display.log 2>&1 &
   echo $! > /data/local/tmp/oneios/control/display.pid
   echo 0 > /proc/$!/oom_score_adj
   echo $! > /sys/fs/cgroup/oneios_session/cgroup.procs
   ```

   The same file with the same checks and arguments, stdin, log file and pid file, the same OOM score and cgroup
   (so the return's cgroup kill gets it too). The socket is the session's (`--session <stage>/session`, where
   xnuhost serves `display.sock`). The user is root and the environment is su's, as the switch's is; `CLASSPATH`
   is set by the launcher itself (`runtime/display/src/bin/launcher.rs`), which also finds the library and the dex next
   to itself. The switch does not change its own nice level before it starts the helper (only its watcher and key
   readers are reniced), so neither does this. The switch runs in init's mount namespace and enters it with
   `nsenter -t 1 -m` when its shell is in another; `$NS` is that `busybox nsenter -t 1 -m --` when
   `/proc/$$/ns/mnt` is not init's, and empty otherwise. A unit test compares the start line with the switch's.
6. *Ready.* `control/display.log`'s size is noted before the start and only what the log gains is read, twice a
   second; each whole line is printed once (a line still being written waits for the next look). The command ends
   with status 0 at `oneios-display: first frame posted` (in a session that is ready the helper posts the whole frame right after `layer up`,
   without waiting for a flush from xnuhost; before `session/ready` it waits for that file, since 2026-11-15:
   `runtime/display/README.md`), and says whether the session is still open. A helper that ended
   before that (two looks in a row: its pid is no helper any more) is status 1 with its last lines, which is how
   the fault of 2026-11-14 would have read (`cannot locate symbol ...`). After `--timeout` seconds without the
   line it is status 1 with the log's last lines; the helper is left running (it waits 60 s for xnuhost's socket).

**`phone touch status`, `phone touch wake`.** `status` reads `/sys/class/sec/tsp/enabled` (`1`: on, `0`: off).
`wake` writes `2,0` and then `2,1` to it (display state 2 "on", event 0 "early" then 1 "late": what One UI's input
HAL writes when the panel comes on, and what the brightness bridge writes when iOS's level leaves 0,
`runtime/bridges/brightness/README.md`), reads it back and prints it; status 1 when it does not read `1`. Only
that one named node: the bridge's own search of `tsp*` folders is not repeated here.

**`phone pen status`, `phone pen wake`** (spen, 2026-11-22). The same for the S Pen's IC, `/sys/class/sec/sec_epen/enabled`
(Samsung's Wacom driver: `2,1` runs its `wacom_enable`; `0` means its screen-off mode, which sends no positions;
`docs/hardware.md` 7f). Only in a session, only that node. The brightness bridge wakes it with the touch IC when xnuhost
gives iOS the pen (`ONEIOS_PEN_IC=1`); this is the check and the fix by hand. Unit-tested (the scripts name only
that node); the stand-in phone (`tests/phone`) has no `sec_epen` node, and the commands have not run on the phone.

**`phone hw [--out DIR]`** (2026-11-28, from YumikoHayamato/OneiOS `9ff6bbd9`; `crates/oneios-phone/src/hw.rs`). Read
only, refused during a session: the phone's hardware as its running kernel was given it, the first facts of a new port
(beside `tools/probe/s25-probe.sh`; [docs/forks.md](../../docs/forks.md) "What a new port still needs"). Into DIR
(default `phone-hw`): `fdt.dtb` (`/sys/firmware/fdt`, the flattened tree the bootloader handed Linux; checked for its
magic; `dtc -I dtb -O dts` decompiles it), `cmdline.txt`, `version.txt`, `cpuinfo.txt`, `meminfo.txt`, `iomem.txt`
(root: real addresses), `partitions.txt`, `interrupts.txt`, `cpuregs.txt` (MIDR and REVIDR of cpu0-7 by name),
`drm.txt` (`/sys/class/drm/card0-DSI-1/{status,modes}`), `by-name.txt` (`ls -l /dev/block/by-name`), `props.txt`,
`modules.txt` (`/proc/modules`), `hyp.txt` (`ls -lZ` of `/dev/gunyah`, `/dev/kvm`, `/dev/udmabuf` and AVF's
`crosvm`, `vm`, `pvmfw.img` by name; the hypervisor's properties; the package manager's virtualization features),
`avf.txt` (`vm info`, `crosvm version`, `crosvm run --help`: queries, no VM is made, 20 s each) and `config.gz`
(`/proc/config.gz`, also unpacked to `kernel.config`); `hw.txt` lists what came. One script for the texts (each after
a `=== NAME` line, each in a subshell whose stdin is /dev/null: on the fork's phone `vm info` waited on the script's
own stdin for ever), one `exec-out` per binary. Named files only (`hw::tests::the_script_reads_named_files_only`; 3
unit tests). Run by the fork on an SM-S938B on 2026-11-20; not run here on the S24 Ultra. The files carry the
handset's serial number: keep them in the git-ignored `logs/` or `out/`.

**`phone cpu [--trace SECONDS] [--out FILE]`** (fix-host-overhead, 2026-11-15; `crates/oneios-phone/src/cpu.rs`). Read
only, with or without a session: for each of cpu0-7 by name, the core (`regs/identification/midr_el1`), `cpu_capacity`,
`topology/cluster_id`, the frequency domain (`cpufreq/related_cpus`), `cpuinfo_max_freq`, the table's top, the boost
frequencies (`scaling_boost_frequencies`), `scaling_max_freq` (`CAPPED` when below the top), the governor and the current
clock; `cpufreq/boost`; the CPUs of `/dev/cpuset/{top-app,foreground,background,system-background}`; in a session
xnuhost's cpuset and allowed CPUs, each of its threads by the TIDs `ps -T -p PID` gives (the CPU it last ran on, nice,
cpuset, `uclamp.min/max` from `/proc/PID/task/TID/sched`), and the iOS threads of its task processes (the children of
`xnuhost-tasks`) counted by CPU and `uclamp.min`. `--trace` records a Perfetto trace (`perfetto -t Ns sched freq idle`)
to `/data/misc/perfetto-traces/oneios-cpu.pftrace`, copies it to `--out` (default `oneios-cpu.pftrace`) and removes it.
No glob and no directory walk (`cpu::tests::the_script_reads_named_files_only`). Tested: the script's text and the
report's parsing (2 unit tests); not run on the phone (no phone on the PC). What to read on the phone before and after a
change: the X4's top and boost, any `CAPPED` domain, xnuhost's cpuset (`allowed 0-7`), the vCPU threads one per CPU,
and the iOS threads' uclamp.min spread (1024 on the P cores while UI runs, 0 for background).

**`phone backlight status`, `phone backlight set N`.** `status` prints
`/sys/class/backlight/panel0-backlight/brightness`, `max_brightness` beside it, and One UI's own value as the
switch recorded it (`control/base/backlight`). `set N` takes 1 to `max_brightness` (12750 on this phone; 0 is
refused: it would turn the panel off), writes it, reads it back, and says that the return writes One UI's own
value back. Two things about what happens to the value afterwards (read in the code, not seen on the phone):
- *iOS's next brightness change writes over it*: xnuhost's brightness bridge writes the node whenever iOS's level
  changes, and knows nothing of a manual write.
- *The return restores One UI's value twice.* The fork's `oneios_return.sh` writes `control/base/backlight`
  back once xnuhost is gone, whatever the node holds; and the bridge's own close (`Bright::shutdown` in
  `runtime/bridges/brightness/session.rs`, which xnuhost's stop calls) always writes One UI's value, even when it
  equals the bridge's own last write, because the node may have been set outside the bridge (this command, or by
  hand). When this note was first written the C close skipped a value equal to its last write; `d4b26a6`
  (2026-11-14) made it always write, and the Rust port keeps that.

The stand-in for them (`tests/phone/session-cmds.sh`, sourced by `tests/phone/run.sh`; a phone of its own): a
display helper in the staged runtime (`fake-display.sh`, which needs its library and dex beside it and writes the
real helper's lines; flag files make it end at once with the 2026-11-14 linker error, never get ready, write its
readiness line in two pieces, or ignore SIGTERM); the stand-in session starts it as the switch does and the
stand-in return ends it by `control/display.pid`, writes `control/state`, and records and restores
`control/base/backlight` (`fake-session.sh`); `/sys/...` in what the tool sends becomes the stand-in's own
`FOLDER/sys/...` (`fake-rewrite.sh`), which holds `tsp/enabled`, the panel's two nodes and the cgroup's
`cgroup.procs`; after each root script the stand-in's "drivers" act (`.fake/drivers`: `tsp/enabled` reads `1`
after `2,1` was written to it, or stays `0` for an IC that does not wake); `readlink` and `nsenter` stand-ins
answer `/proc/PID/ns/mnt` and note the `nsenter` call; `FAKE_PUSH_CORRUPT=1` makes a pushed file arrive with one
byte more.

Result, Linux x86_64, 2026-11-14 (`tests/phone/modes.sh`): `host` mode: phone 200 checks (106 before; the 94 new
ones are `session-cmds.sh`'s), app 65, cycle 103, all ok; `stand-in` mode (the stand-in's own `/proc`, macOS's
way): the same counts, all ok; no stand-in session and no stand-in helper left after any suite; the phone suite
three times in each mode. `cargo test -p oneios-phone`: 15 passed (4 of them `session.rs`'s: the start line against
the switch's and the script's own look at the session, the named
files, the touch and backlight commands, the library and dex checks). What the 94 cover: every command refused
without a session (and with a pid file that names a process that is not an xnuhost), root asked for first; a
restart that works (the old helper ended by SIGTERM, the new one with the switch's arguments, pid file, log and
cgroup; only what the log gained printed; xnuhost the same process); through `nsenter` from another mount
namespace; a readiness line written in two pieces; an old helper that takes a SIGKILL; a pid file that names a
process that is gone, and one that names another process (not signalled); a helper that never gets ready
(`--timeout`) and one that ends at once; the refusals while the switch is starting and while a return is under
way, and a return asked between the tool's look and the script that acts (refused by the script); `--lib` and `--dex` (the copy, both sums, the mode, `runtime.sha256` and `runtime.ids`, every runtime file
then the one its list names), a copy that is not the file sent, a file of the wrong kind, a file the staged
runtime does not have; `touch status` and `wake`, an IC that stays off, a phone without the node; `backlight
status` and `set`, the range; a return while a restart waits; the return afterwards (One UI's level back, the
helper ended, no session, the commands refused again).
What the stand-in cannot show: the real helper (`app_process`, surfaceflinger), the real `su`'s mount namespace
and SELinux domain, whether adb lets the command go while the helper it started runs, the real nodes' answers
(`tsp/enabled` reading `1` after `2,1`), the fork's real return after a restart. `todo_rust.md` section 6, item 6
has the order to try them in on the phone.

### The copy that goes on (`pull-data`)

`crates/oneios-phone/src/pull.rs`. **Not run on the phone**: written and tested against the stand-in phone only.

The fault it answers (the Mac, wireless adb, 2026-11-13): the first port sent one `busybox gzip -1 -c data.img`
stream. It broke twice with `reading the stream: incomplete deflate stream`, after 1.6 of 32.0 GiB (871 s, about
1.8 MB/s) and after 0.6 GiB (213 s), with adb still connected, and each time the copy began at zero again.

What it does now:
- **Chunks.** The store is copied in chunks of `--chunk` MiB (default 256: 128 chunks for 32 GiB). Each chunk is
  one `adb exec-out` stream: `su -c 'busybox dd if=.../data/data.img bs=1M skip=N count=M 2>/dev/null | busybox
  gzip -1 -c'`. dd seeks to the range (`skip=` of a file), so a chunk costs its own bytes; its statistics go to
  `/dev/null` (on an adb that joins stdout and stderr they would follow the gzip stream). The stream is unpacked
  and written at its offset into `OUT.part`, which has the store's length from the start; a block of zeros is
  not written (`sparse::copy_at`). Not a byte past the range is written, and a stream of more or fewer bytes
  than the range, or whose gzip end (CRC, length) is missing, is an error.
- **Sums.** The phone's SHA-256 of a chunk is one more read of the same range of the same file (`... dd ... |
  busybox sha256sum`). It is compared with the SHA-256 of the range read back from `OUT.part` after it was
  written out (`fdatasync`); holes are hashed as zeros without being read (`SEEK_DATA`, `SEEK_HOLE`). If the
  two differ, the range is read back once more with every byte read, in case the file system's map of holes is
  wrong (then the map is not used again in that run).
  - `--verify chunk` (the default) asks for the sum **before** the chunk is copied. A chunk whose sum is that of
    zeros is a hole and is not copied at all: one read on the phone instead of two, and no stream. A chunk
    whose copy does not have the phone's sum is cleared and copied again at once.
  - `--verify end` copies every chunk, then asks for every sum; a chunk that differs is copied again then.
  - **Picked: `chunk`**, by the count of what the phone does, not by a measurement over wireless adb (there was
    no phone on the box, and the stand-in's times were not taken): both ways read and hash every chunk once on
    the phone; `chunk` saves the second read, the gzip and the stream of every chunk of zeros (most of a 32 GiB
    store with a few GiB of data), asks for the sum while the range is the phone's most recent read, and a
    stopped copy holds verified chunks only. `--verify end` is there to measure against on the phone.
- **The progress file**, `OUT.part.progress`: text, a head (the file's path on the phone, its size, its
  modification time, the chunk size), then a line appended per event and written out before the next step:
  `copied INDEX LENGTH SHA256 BYTES-WRITTEN BYTES-OVER-ADB` (the sum as read back) and `verified INDEX SHA256`.
  A line cut off by a kill does not count. A run that finds `OUT.part` and its progress file goes on at the
  first chunk that is not done, with the chunk size the copy began with; chunks copied under `--verify end` and
  not verified yet are verified, not copied again. `--restart` throws both away. A `.part` without a progress
  file (the first port's) is begun again; a progress file that is not this tool's is refused. Only one run
  writes a copy (`flock` on the progress file).
- **Retries.** A chunk whose stream breaks, comes short or long, stands still for `--stall` seconds (120; 0:
  never; it must be longer than the phone needs to hash one chunk), or whose sum differs is cleared (a hole is
  punched over its range, or zeros are written where the file system punches none: what the broken attempt
  wrote must not stay where the store has zeros) and copied again from its start, `--retries` times (5) with a
  pause of `--backoff` seconds (2) that doubles, at most 30 s. Then the command ends with status 1 and says
  which chunk failed, why, and that the same command goes on from there.
- **Ctrl-C or SIGTERM** ends the chunk that is being copied and says how to go on; a SIGKILL leaves the same
  state without the message.
- **The store must not change.** Its size and modification time (`stat -c '%s %Y'`, one named file) are read
  at the start, after every failed attempt and at the end. If they changed the command says so and stops,
  `OUT.part` stays and is not renamed. A later run refuses to go on from a copy of a store that has another
  size or time (`--restart`). A session that is open is refused before anything is read, as before.
- **Speed.** Each chunk prints one line: its bytes, the bytes written, the bytes over adb, its time and MB/s
  (of the store, and over adb), then the run so far. A retry prints the chunk, the error and the offset the
  chunk starts again at. While a chunk is copied a line every 2 s. The last line (stdout):
  `OUT: SIZE bytes, N of them written, M bytes over adb (T s, X MB/s, Y MB/s over adb); K chunk(s) of C MiB: A
  copied, B of zeros not copied, R retried, every chunk's sha256 is the phone's`. `M` counts the streams that
  broke too.

```sh
tools/oneios-tool/tests/phone/pull-resume.sh                 # the stand-in test; FAKE_PROC=stand-in for the Mac's mode
SPEED=4096:280 tools/oneios-tool/tests/phone/pull-resume.sh   # also times a 4096 MiB store: one stream, --verify chunk, --verify end
cargo test --manifest-path tools/oneios-tool/Cargo.toml -p oneios-phone pull   # 3 tests: the progress file's parser, the ranges
```

The test (`tests/phone/pull-resume.sh`, the stand-in phone with a busybox that breaks what the test asks for; a
store of 40 MiB and 12345 bytes in 11 chunks of 4 MiB, 7 with data): a plain copy; `--chunk 1`, `64` and the
default; a stream that breaks once, three times (also with adb ending with status 1), and more often than
`--retries` (the command gives up, the next run goes on and does not read the chunks that were done); a whole
stream of too few bytes; a run killed with SIGKILL, SIGTERM and SIGINT in the middle of a chunk and run again;
a second run beside the first (refused); a stream that stalls; a chunk whose first byte is not the phone's
(copied again; the byte the bad copy wrote where the store has a zero is gone), also under `--verify end`; a
copy begun with `--verify end` and finished with `--verify chunk`; the store growing and the store's content
changing during the copy; `--restart`; a `.part` without a progress file; a foreign progress file; the refusal
while a session is open. After every copy that ends well: `cmp` with the phone's file, not more bytes on disk
than the phone's file holds (`st_blocks`), no `.part` and no progress file left. Result, Linux x86_64 on btrfs,
2026-11-13: 105 checks, `ALL OK`, with the host's `/proc` and with the stand-in's own (`FAKE_PROC=stand-in`).
The same day: `tests/phone/modes.sh` phone 76, app 65, cycle 103 in both modes, `sparse-compare.sh` 35 checks
`ALL SAME`, `cargo test -p oneios-phone` 11 passed. `modes.sh` does not run it (it runs `tests/phone/run.sh`, whose two checks of the first port's
command are now of the ranged one). **Not measured:** the `SPEED=` run of the stand-in; the per-chunk lines of
the 40 MiB test store say nothing about a phone.

**To check on the phone, over wireless adb** (nothing of this is known):
1. That KernelSU's busybox has `dd` with `bs=1M skip= count=` and `sha256sum`, and how fast its `sha256sum` is
   (128 sums of 256 MiB; Android's own `sha256sum` may be faster: a guess).
2. `oneios-tool phone pull-data data.img`, then the same with `--verify end`: the last line's time and MB/s of
   each say which is faster there; whether 256 MiB is a good chunk (a break costs at most one chunk: about
   2 min at 1.8 MB/s of a chunk that is all data).
3. Break it on purpose (turn Wi-Fi off for some seconds; Ctrl-C) and run it again: it must go on.
4. On the Mac's APFS: that `SEEK_DATA` after `F_FULLFSYNC` is what the file reads as (if not, the tool says so
   once and reads every byte back), and the holes (`sparse.rs`'s macOS branch, still not run).

### The sparse copy (`sparse-write`, `pull-data`)

`crates/oneios-phone/src/sparse.rs` replaces the Mac's `sparsewrite.py` (kept as `tests/phone/sparsewrite.py`, the
comparison's reference, until 2026-11-21; since then `sparse-compare.sh` checks the copies against the stream and
against GNU `cp --reflink=never --sparse=always`'s copy: 28 of 28, every copy as few blocks as cp's). The stream is read 256 KiB at a time. A block that
holds only zeros is not written (`memcmp` against zeros: 10 to 25 GB/s here, a byte loop 1.1 to 1.4); the other
blocks are written as runs, each at its offset; the length is set at the end. A block is the file system's:
`statvfs`'s `f_frsize` (4096 on tmpfs, btrfs, ext4, f2fs and APFS), on macOS not less than a page (16 KiB on
Apple silicon, where a write allocates whole pages). The script's unit is its 4 MiB read.

What the file systems keep of it (Linux 7.3 x86_64, 2026-11-13). The data regions are `lseek`'s `SEEK_DATA` and
`SEEK_HOLE` after an `fsync`, the blocks `st_blocks`:

| File system | How | What it keeps |
|---|---|---|
| tmpfs (`/tmp`) | the unit test | every block that was not written is a hole: the data regions are exactly the blocks that hold data, `st_blocks` is their size |
| btrfs, `compress=zstd:3` (`/home`) | the unit test, `sparse-compare.sh` | the same. `st_blocks`, `SEEK_DATA` and `btrfs filesystem du` count an extent's size before compression, so zeros that are written (the script's, inside its 4 MiB reads) count as data: they are compressed extents of 128 KiB (`filefrag -v`: `encoded`), not holes. A block that was not written has no extent |
| ext4 | **not through the kernel** (a mount needs root; `udisksctl loop-setup`: "Not authorized"). `mkfs.ext4 -b 4096 -d` of a folder with the copy, then `debugfs` | `dump_extents`: 14 extents, the copy's 13 data regions (one in two parts); `Blockcount` is the data and one extent block; `dump` gives the stream's bytes. This shows ext4's format holding these holes and e2fsprogs reading them out of the copy (it finds them with `SEEK_DATA`), not the kernel's driver writing them |
| f2fs | **not tested** (a mount needs root; `sload.f2fs` writes every block of a file, so it shows nothing) | |
| APFS | **not run** (no Mac on the box) | below |

The `runs` stream of the comparison (154 MB; zero runs from 100 bytes to 70 MiB) on btrfs, the script's copy and
`sparse-write`'s:

```
the script   : 50331648 bytes held, 5 data regions   0x0..0x1400000 0x1800000..0x2800000 0x3000000..0x3400000 0x4400000..0x4800000 0x8c00000..0x9000000
sparse-write : 19181568 bytes held, 13 data regions  0x0..0x9000 0xc000..0x11000 0x14000..0x15000 0x1f000..0x31000 0x130000..0x231000 0x330000..0x331000
               0x730000..0xb31000 0xf30000..0x1331000 0x1b31000..0x2432000 0x3331000..0x3333000 0x4732000..0x4751000 0x8d50000..0x8d53000 0x8d55000..0x8d56000
```

**APFS.** Nothing of this ran on a Mac; the macOS branch is written from Apple's manual pages and from what
others measured, and the two tests below must run there.
- The format holds a hole of any whole number of its 4096-byte blocks: a file extent record with no block
  (`j_file_extent_val_t`; the inode's `INODE_IS_SPARSE`). Our reader returns zeros for it
  (`crates/oneios-image/src/apfs/files.rs`).
- Apple's driver is not documented and its source is not public. Measured by others (Howard Oakley,
  eclecticlight.co, "Sparse files are common in APFS", 2021: a threshold of 16 MB on an M1 Mac mini; pull
  requests of four other projects that corrected tests on macOS 26 and 27 with `SEEK_HOLE` and `st_blocks`,
  listed in this change's pull request): a write behind a hole shorter than about 16 MiB makes APFS fill the
  hole with zero blocks it allocates, whatever the order of the writes and whenever the length is set; a hole
  of 17 MiB or more stays; a 4 KiB write allocates 16 KiB on Apple silicon. That is why the first port's
  unit test failed on the Mac (a 9 MiB file with zero runs of 4 and 2 MiB: no hole at all, `sparse.rs:92`), and
  it means the script's copy there kept only the zero runs of 16 MiB and more.
- `fcntl(F_PUNCHHOLE)` gives blocks back (fcntl(2): "Deallocate a region and replace it with a hole ... Holes
  must be aligned to file system block boundaries"; `EINVAL` when the offset or the length is not a multiple of
  the block size). On macOS `sparse.rs` punches the hole in front of each write after the write, and the hole
  at the end after the length is set (holes of up to 64 MiB; a longer one was never seen filled). A punch that
  fails is said once on stderr and not tried again: the bytes are right either way. `ONEIOS_SPARSE_PUNCH=0`
  turns the punch off (the copy then holds what the script's did, or less); `=1` turns it on on Linux, where it
  is `fallocate`'s punch and changes nothing.
- **Guesses**, for the Mac to check: that a punched hole shorter than 16 MiB stays a hole while later writes
  follow it, and that a punch is cheap (the unit test prints the time of 1000; here 1.3 ms on tmpfs, 11 to 19 ms
  on btrfs).

```sh
cargo test --manifest-path tools/oneios-tool/Cargo.toml -p oneios-phone sparse -- --nocapture   # 4 tests; prints what the file system kept
ONEIOS_SPARSE_TEST_DIRS=/tmp:$PWD/out/t cargo test ... sparse -- --nocapture                     # in these folders (one per file system) instead of the temporary folder
tools/oneios-tool/tests/phone/sparse-compare.sh              # against the stream and cp --sparse: 28 checks; ALL SAME (MAP=1: every file's data regions)
SPEED=4096:280 tools/oneios-tool/tests/phone/sparse-compare.sh   # also the times on a 4096 MiB stream with about 280 MiB of data (6 more checks; 32768:2100 is the store's size)
```

- The unit tests: (1) seven streams (zero runs shorter than a block, of one block, of 3, 15, 18 and 60 MiB; data
  across the edge of two reads; a tail inside a block, with and without data; only zeros; one zero byte;
  nothing; no zeros) into a file, with the punch off and on. The file is the stream; the data regions the file
  system reports are exactly the blocks that hold data; `st_blocks` is at most their size plus 16 blocks and
  1 %. On macOS without the punch only the holes of more than 32 MiB must have stayed. A file system without
  holes (a 64 MiB file of only a length is not empty) gets the byte check alone. Each file's regions and blocks
  are printed. (2) The same streams through the writer against a model of APFS as described above: without the
  punch the short holes are allocated (23.1 MB for 3.4 MB of data in the first stream), with it exactly the
  pages that hold data are, and no punch is off a block's edge or past the file's end. (3) The zero check.
  (4) An error of the stream and one of the file. `cargo test -p oneios-phone` as a whole: 11 passed (three of them `pull.rs`'s).
- `sparse-compare.sh`: seven streams of `oneios-tool debug mkstream` (was `tests/phone/mkstream.py`; `runs`: zero runs and data runs shorter than
  4 KiB, between 4 KiB and 4 MiB, and longer; `tail`: ends inside a block; `zero`; `empty`; `lastbyte`: a data
  byte at the very end; `dense`; `phone`: 64 MiB shaped like the store) through the script, through
  `sparse-write`, and through `gzip -1` and `sparse-write --gunzip --expect-size`. Result, Linux x86_64 on btrfs,
  2026-11-13: 35 checks, `ALL SAME`: the script's copy is the stream, `sparse-write`'s two copies are the
  script's (`cmp`: bytes and length), both say the stream's length, and `sparse-write`'s copies never hold more
  blocks than the script's. The same on a stream of 32 GiB with 2.18 GB of data (`SPEED=32768:2100`, 41
  checks): the script's copy holds 4.52 GB in 459 regions, `sparse-write`'s 2.18 GB in 3648.

Speed (the same machine: i5-7500, 4 cores; other agents' builds ran most of the time, load 1 to 10, so wall times
of one command moved by a third and more from run to run; CPU times did not). Two streams of `debug mkstream
phone`: 32 GiB with 2.18 GB of data in 3648 runs (1.22 GB packed with `gzip -1`), and 4 GiB with 302 MiB of data
(165 MB packed).

| Reader | Fed the packed stream: wall, MB/s of the stream | CPU of the reader | Behind `debug mkstream \| gzip -1 -c`: wall |
|---|---|---|---|
| 32 GiB: the source alone (`> /dev/null`) | | | 4 rounds: 49.6, 62.6, 52.6, 53.3 s |
| 32 GiB: `gunzip -c \| sparsewrite.py` | 39.6 s, 868 MB/s | gunzip 11.5 s + python 10.6 s | 60.0, 72.7, 66.7, 66.3 s |
| 32 GiB: `sparse-write --gunzip` | 10.7 s, 3218 MB/s | 5.0 s | 52.5, 80.9, 59.0, 60.0 s |
| 4 GiB: the source alone | | | 5 rounds: 6.7 to 9.4 s, median 7.9 s |
| 4 GiB: `gunzip -c \| sparsewrite.py` | 2.6 s and 2.9 s, 1633 and 1486 MB/s | gunzip 1.4 s + python 1.3 s | 6.9 to 11.1 s, median 8.1 s |
| 4 GiB: `sparse-write --gunzip` | 1.1 s and 0.6 s, 3803 and 7469 MB/s | 0.6 s | 7.0 to 8.6 s, median 7.5 s |

Without gzip, from the stream as a sparse file: `sparsewrite.py < FILE` 41.8 s (16.1 s of CPU), `sparse-write <
FILE` 36.4 s (12.6 s); 4 GiB, two runs: 3.6 and 1.7 s against 2.0 and 1.2 s.

Behind `gzip -1` (the phone's side of the real copy) both readers wait for it, so the pipe is about as fast as
`gzip -1`. `sparse-write`'s pipe was the faster in three of the four 32 GiB rounds, by 10 to 12 % (the last two,
the quietest: 59.0 and 60.0 s against 66.7 and 66.3 s, 582 against 517 MB/s of the stream), and in 4 of the 5
rounds at 4 GiB; in the second round, the one with the most load, it was the slower. Its CPU is a quarter of
gunzip's and the script's (the whole pipe: 62 s of CPU against 80 s, of which the source is 56 s).
Fed a packed stream as fast as it can read, it is 2.4 to 4.8 times as
fast. (Feeding the pipe from the sparse file, `gzip -1 -c FILE |`, was tried first and says nothing about the
readers: reading 32 GiB of holes fills the page cache with zeros, and `gzip -1 -c FILE > /dev/null` alone took
68 to 123 s; in five such rounds the script's pipe took 75 to 103 s and `sparse-write`'s 80 to 107 s.) What was
measured to get there:
- the unpacker (the 4 GiB stream, `flate2`'s `MultiGzDecoder` into a 1 MiB buffer, CPU): `zlib-rs` 0.48 s,
  `zlib-ng` 1.48 s (it also needs cmake to build), `miniz_oxide` (flate2's default) 1.74 s; `zlib-rs` without
  flate2's default features (no choice of AVX2 code at run time) 1.9 s; GNU gzip 1.15's `gunzip -c` 0.9 s;
- the read: 64 KiB 0.58 s, 256 KiB 0.56 s, 1 MiB 0.58 to 0.63 s, 4 MiB (the first port's) 1.11 s of CPU for the
  whole copy;
- a second thread for the scan and the writes: 0.72 to 0.86 s of CPU against 0.54 s, the same wall time. So
  there is one thread.

## cycle

`oneios-tool cycle [options]`: one test cycle on the phone, eight steps in this order: `build`, `tests`, `stage`,
`switch`, `watch`, `hold`, `return`, `collect`. `docs/building.md` section 6 has the table of what each does.
**Not run on the phone, and not with the real build scripts**: tested with a stand-in checkout whose
`kernel/build-hosted.sh`, `kernel/prelink/build.sh`, `runtime/package.sh` and
`runtime/host-tests.sh` are stand-ins, and the stand-in phone. The stub check is the real one (`prelink stubs-check` of
the same binary, since 2026-11-13), on a made-up collection of `oneios-tool debug stub-kc` (666 good stubs, or 660 good
and 6 bad); the test before and after that change: 95 checks, the same lines.

```sh
oneios-tool cycle --dry-run [...]                 # what each step would do; nothing is started, no adb call
oneios-tool cycle --kernel-out NAME --runtime-version V --tests --hold 120 --name NAME
oneios-tool cycle --skip build,tests [--image DIR] [--runtime DIR] [--stay] [--ready-timeout 300] [--pull-data]
oneios-tool cycle --only return,collect --name NAME
```

- `--only A,B` / `--skip A,B` choose the steps; a word that is no step is refused.
- The checkout is `ONEIOS_ROOT`, else the folder at or above the current one with `scripts/bg.sh` and
  `oneos/interface.md`. Long builds are `scripts/bg.sh cycle-NAME ...` (tmux); the cycle waits for
  `logs/cycle-NAME.exit` and stops on anything but 0, with the log's last lines. Ctrl-C while it waits ends the
  job's tmux session.
- **The app's checkout** in the `build` step is the one `oneios-tool app` finds ("app" below: `--oneos DIR`,
  `ONEOS_DIR`, the current folder, `app/` of the checkout, `app/` of the main checkout, `OneOS` beside it). The
  step says the source's version and where it is
  (`app: the source is 0.4.86 (versionCode 115) in DIR/app (app/ of the OneiOS checkout); ...`; `summary.md` has
  the same after `app source:`). With no checkout it says `app: no source to compare (...)`, with the places
  looked in (an empty `app/`: with `git submodule update --init --remote app`), and goes on. `--force app` builds
  and installs that checkout (`app build`, `app install`); **with no checkout `--force app` ends the cycle**
  (`--force app: no OneOS checkout ...`, status 1, before the phone is touched; until 2026-11-15 it said the
  line above and went on to the phone without the build that was asked for).
- **Safety.** One phone lock for the whole cycle (`out/phone.lock` of the main checkout, `flock`; a second cycle
  is refused and told who holds it). Before the switch: root, no open session, the three scripts staged, no
  import in progress, the root script's preflight `ready 1`. From the switch on, whatever happens (a panic, no
  first frame in time, the session ending by itself, Ctrl-C, an error while watching), the return is made and
  waited for, unless `--stay` was given and Ctrl-C came in the hold. A failed build, a failed audit, stubs that
  are not all good or failing host tests end the cycle before the phone is touched.
- The exit status is 0 only when every step that ran ended well and the boot reached its first frame (when
  `watch` ran). A `collect` that failed (a log that could not be copied, `--pull-data`'s copy or its `ioslog`)
  fails the cycle too: status 1, `oneios-tool cycle: collect: ...`.
- **`ioslog-health.txt`** (2026-11-15): `collect` writes iOS's own counts of the last boot (hangs, watchdog
  kills, XPC timeouts, jetsam kills, crashes, stuck idle demotions, encode failures: `ioslog health`, "health"
  above) into the session's folder, prints `--- ioslog health SOURCE`, the report's head and one line per kind,
  and `summary.md` gets them as its last section (`## iOS's log: hangs, kills and timeouts`); the file also has
  each kind's texts, every process and example lines. The source is the folder's `data.img`: this run's
  `--pull-data` copy (after `ioslog boots` and `checks`), or the copy an earlier run of the same `--name` left
  there; without one, an `ioslog.txt` in the folder (iOS's log as text: `oneios-tool ioslog show SOURCE --boot
  last -o FOLDER/ioslog.txt`). A folder with neither gets one line that says so, and the step is ok; a `data.img`
  that cannot be read fails the step (`collect: ioslog health: REASON`). A `summary.md` written again later keeps
  the section as long as `ioslog-health.txt` is in the folder (the store's copy may be deleted). **So the numbers
  need the Data store's copy (`--pull-data`) or the log as text**; `collect` does not run the tool on the phone
  (the Android build is not staged with the runtime yet: on the phone itself the command is `oneios-tool ioslog
  health`).
- `summary.md` is written by `collect` also for a cycle that failed: the result, each step's time and result, the
  markers, the panics, the way back. **The result line is made from the same step results as the Steps table**:
  `every step that ran ended well` only when no step of the table failed; otherwise `**stopped**: REASON` (what
  ended the phone steps) and `**failed step**: STEP (REASON)` for each failed step. A boot that did not reach its
  first frame is a failed `watch` in the table.
- **The markers of a run that did not watch.** The times of the Markers section (seconds after the switch) are
  the watch step's. A run without it (`--only return,collect` after an earlier `--only stage,switch,watch` of the
  same `--name`) reads the markers from the session's saved log instead: `xnuhost.log` in the session's folder
  (the collect step's copy), else the phone's `session/xnuhost.log` by its name. The section then says that the
  watch did not run in this run and which of the two it read, and lists each marker with the log's own stamp; the
  panics are taken from the same log.

Result of `tests/cycle/run.sh`, Linux x86_64, 2026-11-12: 90 checks, all ok, three runs. Among them: a whole
cycle (the stand-in kernel built with `XNU_FILESET_REFERENCE=1`, the collection with the reference, the stubs
check, the runtime package unpacked, the host tests, only the differing files staged, the fork's own
`begin`/`receive`/`finish` and `status` under its test root, switch, the nine markers, the hold, the restore
line, the folder); a second cycle that builds and stages nothing; a newer patch that rebuilds; a panic; a boot
that never gets ready; Ctrl-C in the hold with and without `--stay`; the lock; the three build failures; the
three refused switches. What it cannot show: the real builds' times and outputs, the real switch and return,
adb while One UI's framework stops.

`phone log --follow` and the switch (2026-11-13; the phone's fault of 2026-11-12 above): the suite has 95 checks
now (four for this, and the last check, that no stand-in session is left running). A follow started right after
a switch while the last session's log is still there (the stand-in session replaces it 3 s in,
`.fake/log-delay`; the phone takes about 40 s) says so and waits (1), and prints none of its lines (2); then adb
drops (`FAKE_CUT`) while this session's log is read, and the follow carries on with no line twice (3); a drop
that comes back to a new file with the same first line (`FAKE_DROP_NEW`) reads it from its start (4). The
binary from before the fix, on the same suite: 1 to 3 fail as on the phone (the last session's log printed,
then after the drop "is a new file" and 15 lines printed twice); 4 passes there too (that new file had fewer
lines than were read). Results by mode below (`modes.sh`).

`summary.md` (2026-11-13; two faults of a cycle on the phone): its head said "every step that ran ended well"
while its Steps table had `collect | failed: ioslog boots ended with status 1`, and the summary of
`cycle --only return,collect` said "Markers: none seen" after the boot had been watched by an earlier
`cycle --only stage,switch,watch` of the same `--name`. The head took the phone steps' result and never looked at
`collect`'s; the markers were the ones this run's own watch had seen. Both are fixed as the two bullets above
say. The suite has 103 checks now, eight for this: a cycle with `--pull-data` on the stand-in phone, whose
`data.img` is no APFS store, so `ioslog boots` ends with status 1 (the cycle ends with status 1 after the return;
the head is `**failed step**: collect (ioslog boots ended with status 1)` and has no "ended well"; the steps
before it are ok and the logs and the store's copy are in the folder); the panic case's `watch` is failed in the
table and named in the head; and a split run, `--only stage,switch,watch` then `--only return,collect` with the
same name (the first leaves the session open and writes no summary; the second's summary says that the watch did
not run in this run, that the markers were read from the folder's `xnuhost.log`, and lists the markers `phone log
--markers` prints for that log, each stage once). The binary from before the fix fails five of the eight (the
three it passes: the cycle's first half of the split run, the folder's files, the return of the second half).
`modes.sh phone cycle`, Linux x86_64, 2026-11-13: `host` mode phone 76 and cycle 103 checks, `stand-in` mode the
same counts, all ok, no stand-in session left. The reading of the phone's own `session/xnuhost.log` when the
folder has no copy is not covered by a check (the stand-in's collect always copies one).

The app's checkout (2026-11-15, the fork as the submodule `app/`): the suite has 117 checks now, fourteen for this,
all dry runs but the last three: a checkout named with `ONEOS_DIR`; none found (said, the cycle goes on; with
`--force app` refused); `app/` empty (said with the submodule command; with `--force app` refused with it);
`app/` present (its version and `in DIR/app (app/ of the OneiOS checkout)`; `--force app` names it); `--oneos` and
`ONEOS_DIR` before a present `app/`, with the note; no gradlew started by a dry run; and one real
`cycle --only build --force app`, whose `app/app-android/gradlew` (a stand-in that fails) is what ran, with
`test assembleFullDebug`, and whose failure ended the cycle before any adb call. Not covered: a `--force app` that
builds and installs (the stand-in gradlew of this suite builds nothing; `tests/app` covers `build` and `install`
by themselves).

## app

`oneios-tool app [--oneos DIR] [--serial S] CMD`: the OneOS fork's app (`akshaynexus/OneOS`, branch `oneios`).
**The app was not built and not installed with this tool yet**: the owner's decision is that the Linux box builds no
app and gets no JDK 21 or Android SDK for it, so the tool is written for a host that has them (the Mac) and tested
with stand-ins. `app version` was run on the real clone.

```sh
oneios-tool app version [--no-phone]     # applicationId, versionName, versionCode, the commit; the installed app's version and certificate
#   versionName/versionCode as Gradle computes them: literal, or (the fork since 0.5.14) `143 + commitsSinceBase` and
#   `"0.5." + (14 + commitsSinceBase)` with commitsSinceBase = `git rev-list --count <versionBase>..HEAD` (0 when git
#   cannot tell). Until 2026-11-21 it read only the literal form: on the fork at 7e34fa4 `app version`, and the app and
#   cycle checks that use it, failed (5 checks).
oneios-tool app doctor                   # JDK 21, an Android SDK with platform 37, the GitHub Packages login
oneios-tool app build [--no-test]        # cd app-android && ./gradlew test assembleFullDebug; then the APK's path, size, sha256, version
oneios-tool app install [APK]            # adb install -r of the build output (or APK)
oneios-tool app install --release 0.4.86 [--repo akshaynexus/OneOS] [--dir out/app]   # the CI prerelease oneios-<version>, through gh
oneios-tool app stage-scripts            # the fork's oneios_switch.sh, oneios_return.sh and oneios_root.sh into the stage
oneios-tool app dir                      # the checkout's path alone (for scripts; the suites' fork_dir)
oneios-tool app kb 24B156 [--check] [--repo .]   # one build's pairing files (assets/kb/ for 24A446, assets/kb-24B156/) from this checkout's records
```

- **The checkout** (the fork becomes the submodule `app/` of OneiOS, 2026-11-15; in a checkout from before it
  there is no `app/`, nothing is said about it and the lookup goes on) is the first of:
  1. `--oneos DIR`;
  2. `ONEOS_DIR`;
  3. the current folder, when it is a checkout;
  4. `app/` of the OneiOS checkout the command runs in (`ONEIOS_ROOT`, else the folder at or above the current
     one with `scripts/bg.sh` and `oneos/interface.md`): the submodule, the default;
  5. `app/` of the OneiOS main checkout, when the command runs in a worktree (found through
     `git rev-parse --git-common-dir`): a new worktree's `app/` is empty;
  6. `OneOS` beside the OneiOS main checkout: the clone from before the submodule.

  Why this order: the owner's order is `app/` first, and among the places the tool finds by itself it is. A
  checkout **named by hand** (1 and 2; 3 is the folder the user stands in) still comes before it, as before the
  submodule, so that a named checkout is never silently replaced by the default. For the same reason a named folder that
  is no checkout is an error and the lookup does not go on to `app/`; and when `ONEOS_DIR` (easy to forget in a
  shell) takes the place of an `app/` that is a checkout, a `note:` line on stderr says so.
  - **A checkout is told by what it holds**, not by its name: `app-android/app/build.gradle.kts` (the version is
    read from it) and `switcher/assets/oneios_switch.sh` (the fork's own; upstream OneOS and a checkout on another
    branch have none).
  - **An `app/` that is an empty folder is not a checkout**: the submodule is not initialised (a clone without
    `--recurse-submodules`, a new worktree). It is passed over, with
    `note: DIR/app is empty: the app submodule is not initialised (in DIR: git submodule update --init --remote app)`
    on stderr when a later place is a checkout; when none is, that line is in the error, which lists every place
    looked in and why it was not taken (`is not there`, `is empty`, `is not the fork's checkout (no FILE)`) and
    exits with status 1. An `app/` that holds other files is passed over the same way.
  - The first line of every command says which checkout and how it was found, for example
    `OneOS checkout: /x/OneiOS/app (app/ of the OneiOS checkout)`. `cycle` and the suites' `fork_dir`
    (`tests/phone/lib.sh`, through `app dir`) use this same lookup.
- `version` reads `app-android/app/build.gradle.kts` (`applicationId`, `versionName`, `versionCode`; the `full`
  flavour's `-<oneos.series>` suffix when `gradle.properties` sets one; the `probe` flavour's `.probe` and
  `-probe`), the commit, the branch and whether the tree is dirty. With a phone on adb it adds the installed
  app's version (`dumpsys package com.fahadali.oneos`) and the SHA-256 of its signing certificate; with none it
  says why it did not ask. On the real clone (2026-11-12): `com.fahadali.oneos`, `0.4.86`, `115`, `f964d6e`
  (branch `oneios`), clean.
- **The certificate** of an APK is read with the `apk-info-zip` crate (the APK Signing Block, schemes v3.x, v3,
  v2, else the v1 JAR signature). For the installed app the APK is the one file `pm path` names, copied with
  `adb exec-out cat`.
- `doctor` prints `ok`, `MISSING` (the build needs it; exit status 1) or `absent` (only `install` needs it) per
  line: `gradlew`; a JDK 21 (`JAVA_HOME`'s `java`, else the one on `PATH`); an Android SDK (`ANDROID_HOME`,
  `ANDROID_SDK_ROOT`, `local.properties`' `sdk.dir`, `~/Library/Android/sdk`, `~/Android/Sdk`) with
  `platforms/android-37*`; the GitHub Packages login as `settings.gradle.kts` reads it
  (`~/.config/twidget/github.properties` `ghUsername` and `ghAccessToken`, else `GH_PACKAGES_USER` and
  `GH_PACKAGES_TOKEN`); `adb`; `gh`. **A token is never printed**, nor the user name: only where it was found,
  its kind by its prefix (`ghp_`: classic; `github_pat_`: fine-grained, which cannot read GitHub Packages and is
  reported as missing) and its length. Not checked: that the token really has `read:packages`.
- `install` prints the APK's size, SHA-256 and certificate, then asks the phone: root first, no session open,
  and **the installed app's certificate must be the APK's**. If it is not, the install is refused with the reason:
  the tool never uninstalls (that wipes the app's data). `--release VERSION` downloads
  `OneOS-oneios-VERSION-debug.apk` and its `.sha256` from the prerelease `oneios-VERSION` with
  `gh release download` (`GH` names the program) and checks the one against the other first.
- `kb BUILD` (2026-11-25, ios272) writes one iOS build's pairing files into the fork (`assets/kb/` for 24A446,
  `assets/kb-<BUILD>/` for another: 24B156) from this checkout: the OS profile `ios-*-BUILD/profile.toml`,
  `reviewed-BUILD-v54.json`, the lead's and user's reviewed files, `pairs-BUILD.json`, `stubs-BUILD.json`,
  XnuPrelink's `fixtures/fileset-*-symbols.json` (`match/fixtures/`, the folder kernel/prelink/build.sh takes) and
  `kb.json` (format 2: the build, the device, the profile's kernelcache SHA-256, each file's). `--check` writes nothing
  and fails on a file that differs or one that is not a pairing file. On the fork's `assets/kb/` of `38dc4f1`:
  `app kb 24A446 --check` says "none changed" (the folder was copied by hand until then). Unit tests: `kb::tests`.
- `stage-scripts` is for sessions started without the app (`docs/newtodo.md` step 4): each of the three files is
  pushed, copied to `NAME.part` in the stage as root (`root:root`, mode 0700) and moved into place, so what was
  there is replaced and never written through; the SHA-256 of each staged file is printed before and after and
  must be the fork's file's. The app pins these three by SHA-256: staging other versions than the installed
  app's makes the app's own checks refuse them until it stages its own again.

```sh
tools/oneios-tool/tests/app/run.sh            # 103 checks; ALL OK
```

On the Mac (2026-11-22) it runs with GNU coreutils and sed first on `PATH` (the suite says so when they are not)
under macOS's own `/bin/bash` 3.2: 103 checks, ALL OK, on the fork at its upstream 0.5.35 merge. Before that day
5 checks failed there (the two "refused while a session is open" and the three `stage-scripts` per file): bash 3.2
cannot parse a `case` pattern without its opening `(` inside `$(...)`, so `tests/phone/lib.sh`'s `file_entries`
wrote no JSON for the stand-in stage's manifest. The pattern is `(bin/*)` now.

The stand-ins: a OneOS checkout whose `gradlew` is a script that records its arguments and "builds" an APK; APKs
made by `oneios-tool debug mkapk` (`tests/app/mkapk.py` until 2026-11-21; a zip with an APK Signing Block v2 around a certificate made with `openssl`; the
signature is filler, the tool reads the certificate and does not verify the APK); a `gh` that serves a release's
two files; the stand-in phone of `tests/phone/`. Result, Linux x86_64, 2026-11-12: 65 checks, all ok. What they
cannot show: a real Gradle build, real APKs signed by `apksigner` or Gradle (v2 and v3 blocks as Android writes
them), `adb install` on Android, `gh` against GitHub.

The lookup with the submodule (2026-11-15): 103 checks, 38 more, in stand-in OneiOS checkouts (their own git
checkouts with `scripts/bg.sh` and `oneos/interface.md`): `app/` present (first, before `OneOS` beside; from a
folder inside the checkout; with `ONEIOS_ROOT` from outside; `app dir`; `app build` runs gradlew in
`app/app-android`); `app/` empty (alone: status 1 with the command; with `OneOS` beside: that one, the note on
stderr only); `app/` with other files, and with the build file but without `oneios_switch.sh` (passed over, said);
`--oneos` and `ONEOS_DIR` before a present and an empty `app/`; a named folder that is empty or no checkout (an
error, `app/` is not taken instead); the current folder; `OneOS` beside the checkout from a worktree; a worktree
with no `app/`, an empty one and its own; none found, in a checkout and outside every checkout. Seven of them use a
**real git submodule** (`git submodule add -b oneios` of the stand-in fork into a stand-in OneiOS checkout, then
`git clone` without `--recurse-submodules`): the clone's `app/` is an empty folder and the tool says so with the
command; after `git submodule update --init --remote app` it is the checkout, with the submodule's version; a new
worktree's `app/` is empty and the main checkout's is taken. The crate's own tests (`cargo test -p oneios-phone`,
`app::tests`, nine) check the order on made-up folders. `modes.sh`, Linux x86_64, 2026-11-15: `host` and
`stand-in` mode each phone 222, app 103, cycle 117 checks, all ok, no stand-in session left; `cargo test
--release`: 134 passed, 0 failed. **Not tested:** OneiOS's own `app/` submodule (it is added on another branch;
in this checkout there is no `app/`, and every suite here found the fork as `OneOS` beside the main checkout, the
sixth place), CI, macOS, a real build or install from `app/`.

## prelink

`oneios-tool prelink STEP ARGS...`: the steps of `kernel/prelink/build.sh`. They replaced the Python steps
(`kernel/prelink/STEP.py` and XnuPrelink's matcher and emitter), which were **retired on 2026-11-12** after the
comparison below; each step takes the arguments its Python original took and writes the same files, and each
module's header keeps that script's description. `build.sh` runs nothing else (no `PRELINK_IMPL` any more) and
builds the binary when it is missing or stale (`scripts/lib/oneios-tool.sh`).

| Step | Was (Python, retired) | Module |
|---|---|---|
| `roots KERNELCACHE OUT.json [PROFILE.toml]` | `roots.py` | `roots.rs` |
| `hosted KERNELCACHE OUT REPORT.json [--closure FILE] [--x18]` | `hosted.py` | `hosted.rs` (`--x18` starts `llvm-mc`, as Python does; not compared) |
| `native-input IN OUT REPORT.json` | `native-input.py` | `native_input.rs` |
| `merge-reviewed --reviewed F --native K --out F EXTRA...` | `merge-reviewed.py` | `merge_reviewed.rs` (reads the kernel's symbol table itself: no `llvm-nm`) |
| `match KERNELCACHE NATIVE --out MAP [--reference-kernel K] --xnuprelink DIR` | XnuPrelink's `fileset_match.py` (matcher version 8, pin `9b950f2`) | `matcher.rs` over `image.rs` (`kc_image.py`, `fileset_shape.py`) |
| `align`, `kclasses`, `structors`, `vtables` (the Python steps' options) | `align.py`, `kclasses.py`, `structors.py`, `vtables.py` | `align.rs` (+ `pyset.rs`), `kclasses.rs`, `structors.rs`, `vtables.rs` |
| `emit` (emit.py's options) | `emit.py` over XnuPrelink's `fileset_emit.py`, `fileset_layout.py`, `fileset_segmented.py` | `emit.rs`, `layout.rs`, `prelink_xml.rs` |
| `audit` (audit.py's options, `--against` and `--draft` too; exit 1 on a failure) | `audit.py` | `audit.rs` |
| `record --work DIR ... [--repo CHECKOUT] [--no-git] [--oneios-commit C] [--xnuprelink-commit C]` | `record.py` | `record.rs` (the three last options, 2026-11-15: the record's two commits given, or null, without starting `git`: the phone has none) |
| `pin PROFILE` | the `tomllib` one-liner of `build.sh` | prints `[kernel.prelink] kernelcache_sha256` |
| `port --from KC --to KC [--map OUT] [--report OUT] [--record IN=OUT]... [--fields KEY]... [--fields-loose] [--strict]` | new (2026-11-25, ios272) | `port.rs`: carries the address-keyed records (pairs, stubs, the reviewed files, the class layouts, the display record) from one kernelcache build to another (24A446 to 24B156), every check read again from TO's bytes; header of the file and kernel/prelink/README.md "Another build". Since 2026-11-25 also a data object named by its content (the one TO object that points to the same C string at the same offset: the `vnop_*_desc` objects). Not a step of `build.sh`: run by hand once per new build |
| `build --kernel K --kernelcache C --profile P --reviewed F [--more-reviewed F]... --pairs F --stubs F --xnuprelink DIR [--reference K] --out DIR [--repo DIR] [--no-git] [--remove-work]` | the whole of `kernel/prelink/build.sh` (new, 2026-11-15) | `build.rs`: every step in this process, for the phone and `serve` ("prelink build", below) |

`prelink classes` refuses a class argument with a space in it (2026-11-25): a list passed as one word (zsh does not
split an unquoted `$LIST`) was looked up as one name and recorded as "no static initializer" under the first class.

What the Rust steps still read from the matcher folder (`--xnuprelink`: `external/XnuPrelink`, or the OneOS app's
`kb/match`): its reviewed fixtures (`fixtures/fileset-*-symbols.json`), nothing else; a folder without one is refused
(`not-found`). The matcher signature is the SHA-256 of `oneios-prelink matcher <MATCHER_VERSION>`, a NUL, then
each fixture's file name, a NUL and its bytes, in name order. It is written into `match.map.json` and checked by
`emit`: it says which matcher rules and fixtures a map follows. Until 2026-11-22 it was XnuPrelink's own
signature, over its Python sources (`fileset_match.py`, `kc_image.py`, `fileset_shape.py`) too, which only kept it
equal to Python's; the app shipped those three files for that alone. The change moves only the signature: the same
kernel, kernelcache and fixtures give the same `kernel.kc` (checked 2026-11-22, below).
`matcher.rs` and `emit.rs` are ports of the pin `9b950f2`; when the pin moves, the port must follow, and the
comparison below shows a drift.

Hand-written here, and why (the library rule): the Mach-O reader (`image.rs`: the chains' walk order, the kernel
collection pointer formats 7 and 8 and fileset file offsets; `object` and `goblin` read none of the three),
CPython's `set` order and `random.sample` (`pyset.rs`: kept on 2026-11-21, when Python's bytes were let go
elsewhere, because the order is the matcher's and align's rule, not a layout: `pair_metaclass_vtables` takes the first
qualifying site in that order, and `align`'s holdout sample is drawn from a list made by iterating a set; another order
gives other pairs; no library has them; the generator is the `rand_mt` crate), and the prelink information's XML
writer (`prelink_xml.rs`: the kernel's `OSUnserializeXML` knows `&lt;`, `&gt;` and `&amp;` only, and the `plist`
crate's writer also writes `&quot;` and `&apos;`, which 24A446's strings need; reading is the `plist` crate). Libraries: `indexmap` (Python's dict order), `md-5`, `sha2`, `hmac` (the FIPS seals), `rand_mt`,
`plist`, `toml`, `base64`, `memchr`, `rayon` (the per-function scan, the audit's per-kext scan).

The comparison today is with a recorded run of the Rust chain (`tests/prelink/golden/`: per kernel, the name,
size and SHA-256 of every file a step writes and the SHA-256 of every step's stdout; no Apple bytes):

```sh
scripts/image/kernel-inputs.sh 'profiles/ios-27.0.1-iPhone18,2.toml'      # out/kernel-inputs/24A446/kernelcache.macho
KERNEL=<runtime 0.1.13>/kernel/xnu.macho XNUPRELINK=external/Droid-VM-UniXNU/kits/XnuPrelink \
  tools/oneios-tool/tests/prelink/compare-golden.sh     # 35 hashes; without KERNEL: roots and hosted only, 10 hashes
```

A golden exists for the kernelcache alone (`kernelcache.txt`) and for the released kernel (`326e2144...`, the
`kernel/xnu.macho` of the runtime package v0.1.13, from the release `kernel-xnu-...-703a6b34e73f`, no reference;
`kernel.kc` `276aadac...`, `c961e687...` since 2026-11-25: `prelink roots` leaves com.apple.driver.AppleSPU and the four
kexts that link it out, which changed select, the binds, the draft, emit, audit, record and `kernel.kc`, not the
pairing steps' files; recorded again then, 35 of 35 the same on a second run. `kernelcache.txt` is not in git:
the `.gitignore` rule `kernelcache*`, which keeps Apple's files out, takes it too, so each checkout records its own
with `UPDATE=1` and no KERNEL: 10 of 10 the same on a second run, 2026-11-25); for any other kernel the script says there is none and exits 3 (`UPDATE=1` records
that run instead). In the JSON records and the stdout the work folder, the commit of `kernel.kc.build.json` and
the folders of absolute input paths are taken out before hashing; the matcher folder's commit stays, so the
golden's `XNUPRELINK` is the kit at `b74ed7c` (another folder changes `kernel.kc.build.json` only). Both were
recorded on 2026-11-22 with the fixtures-only matcher signature: 35 of 35 and 10 of 10 the same on a second run,
and the binary before that change gives the same 10 for the kernelcache alone. Until then the goldens were the
Python chain's frozen output for two Linux-built kernels (`13ed0b70...`, `399f50fa...`; 40 of 40 each on
2026-11-12, after the Python steps were deleted); those kernels are gone and the goldens held Python's matcher
signature, so they were removed.

The signature change itself (2026-11-22), checked with `prelink build` on that kernel and the kernelcache, the binary
before it with the kit (Python sources and fixtures) and the binary after it with a folder of the fixtures alone:
`kernel.kc` the same (96,206,848 bytes, `276aadac...`), audit 0 failures both times, and every other file the same
once the out folder is taken out, but for `matcher_signature` (`b4a694e2...` before, `a3552ce4...` after) and
`match.key` (which also holds the binary's own SHA-256). The binary before refuses the fixtures-only folder.

**The comparison that froze them** (`tests/prelink/compare-python.sh` at commit `306b0a4`, the last with both
implementations):

Both chains run side by side, each step with the same arguments into the side's own folder, each side reading its
own earlier outputs; stdout, the exit status and every file written are compared with `cmp` (in the three records
that carry their folder's path, the path is taken out first). Result on Linux x86_64, 2026-11-12, kernelcache
`349c4692...`, the Linux-built kernel `13ed0b70...` with its reference `ac72f98d...` ("pc"); the box had a load of
7 from another agent's qemu boot (4 CPUs), so the times are loaded times:

| Step | Same | Python | Rust |
|---|---|---|---|
| `roots` with the profile, and without | yes (2) | 0.85 s, 0.33 s | 0.12 s, 0.11 s |
| `hosted --closure`, and over every kext | yes (2) | 6.1 s, 6.9 s | 1.4 s, 1.3 s |
| `native-input` of the kernel, and of the reference | yes (2) | 0.28 s, 0.21 s | 0.05 s, 0.02 s |
| `merge-reviewed` (161 records added) | yes | 0.30 s | 0.02 s |
| `match` with the reference (`match.map.json`: 11,458 of 21,793 functions, 540 data) | yes | 102.0 s | 5.4 s |
| `align` by the kernel's own order (480 pairs), by the reference's (24) | yes (2) | 18.5 s, 10.0 s | 1.8 s, 1.0 s |
| `kclasses` (157 classes), `structors` (233 pairs), `vtables` (587) | yes (3) | 4.6 s, 1.0 s, 2.2 s | 0.6 s, 0.7 s, 0.5 s |
| `emit` (`kernel.kc`, `kernel.kc.prelink.json`) | yes | 24.7 s | 2.2 s |
| `audit` (2,711 paired, 93 on the stub, 9 data unpaired, 0 failures), and with `--against` and `--draft` | yes (2) | 6.7 s, 8.8 s | 1.2 s, 1.9 s |
| `record` | yes | 0.78 s | 0.98 s |
| the chain | `kernel.kc` `ffbe95f7...` from both; `stubs-check.py`: 666 good | 194 s | 19 s |

The same on the second tree ("pc2", kernel `399f50fa...`, the pc tree's reference): every step the same,
`kernel.kc` `cdd98e7e...` from both, 666 good stubs; Python 159 s, Rust 17 s.
`PRELINK_IMPL=rust kernel/prelink/build.sh` on the pc kernel: 14 s (Python, on the less loaded box: 125 s); its
`kernel.kc`, `kernel.kc.hashes`, `kernel.kc.binds.json` and `kernel.kc.prelink.json` are the Python build's bytes,
and `kernel.kc.build.json` differs in the commit it records (`tools.oneios`) and nothing else.

Not compared: `hosted --x18` (minutes in Python), `align --explain` (a debugging print), an audit with failures
(none exists: both lists agree with both kernels), a Mac-built kernel (the Mac's `eae11352`: not on this box).

The rules now live in the Rust modules only (`roots.rs`: the hardware provider classes and the kexts left out by
hand; `matcher.rs`, `emit.rs`, `layout.rs`: XnuPrelink's at pin `9b950f2`). A change of a rule changes the outputs
and so the goldens: regenerate them with `UPDATE=1` in the same commit and say why.

### prelink build, and the threads (2026-11-15)

`oneios-tool prelink build` is `kernel/prelink/build.sh` in one process: the phone has no bash, and `oneios-tool
serve` starts no program (its `prelink` request is this). The steps, their arguments and their files are
build.sh's; `stubs-check` is added before `kernel.kc` is put in place, and `X18=1` is left out (it starts
`llvm-mc`). `OUT/work` holds the steps' files as build.sh leaves them (`--remove-work` removes it at the end; `serve`
does unless the request says `keep_work`), and `OUT` gets `kernel.kc`, `kernel.kc.hashes`, `kernel.kc.build.json`,
`kernel.kc.prelink.json` and `kernel.kc.binds.json` only after the audit and the stub check passed. `build.sh` itself
is unchanged and still runs one step after the other.

```sh
oneios-tool prelink build --kernel out/kernel/current-hosted/xnu.macho --kernelcache out/kernel-inputs/24A446/kernelcache.macho \
  --profile kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml --reviewed kernel/prelink/reviewed-24A446-v54.json \
  --more-reviewed kernel/prelink/lead/reviewed-lead-24A446-v54.json ... --pairs kernel/prelink/pairs-24A446.json \
  --stubs kernel/prelink/stubs-24A446.json --xnuprelink external/XnuPrelink [--reference REFERENCE] --out out/kernel/24A446
KERNEL=out/kernel/current-hosted-pc/xnu.macho REFERENCE=out/kernel/current-hosted-pc/reference.macho \
  tools/oneios-tool/tests/prelink/compare-build.sh        # build.sh, `prelink build` (one thread and all) and serve's `prelink`: every file
```

What runs in threads:
- **Steps that wait for nothing of each other run at the same time** (`prelink build` only): `native-input` and
  `merge-reviewed` beside `roots` and `hosted`; the two `align` beside `kclasses` and `structors`; `audit`,
  `stubs-check`, `record` and the hash list. `match`, `vtables` and `emit` run alone. Each step is still its own
  command with its own files, so the files are build.sh's; the steps' one-line reports may come in another order.
- **The per-kext work** of two steps (in `build.sh` too): `hosted` reads each kext's code for its rewrites and
  privileged instructions in every thread, then writes the rewrites in the kexts' order (the loop that did both
  one kext after the other is still there: it runs for `--x18`, and when two kexts share code or a rewrite falls
  on a load command, which is checked); `emit` searches each kext's code for its inline data pools and scans it
  for its branches and address pairs in every thread, then rebinds kext by kext in the kernelcache's order, as
  before (the rebinding allocates stubs and counts as it goes: its order is the output's).
- Between steps, and between the kexts of `hosted` and `emit`, a request of `serve` can be stopped.

Measured on the Linux PC (4 CPUs; other agents' builds were running, so each run had a CPU weight of its own),
kernelcache `349c4692...`, the Linux-built kernel `13ed0b70...` with its reference:

| Way | Wall | CPU | `kernel.kc` |
|---|---|---|---|
| the fifteen steps and `stubs-check` one after the other, the binary before this change | 13.17 s | | `ffbe95f7...` |
| the same, this binary (the kexts' scans of `hosted` and `emit` in threads) | 12.77 s | | the same; `hosted` 0.98 s before, 0.83 s after |
| `kernel/prelink/build.sh`, this binary (with the hash list; best of 3) | 13.71 s | 102% | the same |
| `prelink build --jobs 1` (best of 3) | 14.84 s | 96% | the same |
| **`prelink build`** (4 threads; best of 3) | **11.65 s** | 122% | the same |
| `serve`'s `prelink` request (4 threads; best of 3) | 11.23 s | 124% | the same |

`prelink build` with one thread is a second slower than build.sh: it hashes its inputs for the pairing's key with
the tool's own SHA-256 (this binary alone is 93 MB; build.sh uses `sha256sum`, which is nearly twice as fast on
that CPU) and adds `stubs-check`. With its independent steps at the same time it is 1.2 times as fast as build.sh
and 1.3 times as fast as itself with one thread.

**The same output:** `kernel.kc` is `ffbe95f7...` every way (the binary before this change, build.sh with this
one, `prelink build` with one thread and with all, and `serve`'s request); `compare-build.sh` compares
24 files of each way with build.sh's (`kernel.kc`, its hash list, the three records, and the 19 work files), byte
for byte: `ALL THE SAME` (in the four records that carry their folder's path the path is taken out first; the
record's two commits are null in `serve`, which starts no `git`). The work folder of the binary before this change
and of this one, step after step: 19 files the same (`kernelcache.hosted.macho` `6b1a938f...`, `hosted.json`,
`kernel.kc.prelink.json` and `kernel.kc.binds.json` among them).
`tests/prelink/compare-golden.sh` (every step against the frozen output of the Python steps) gives the same with
the binary before this change and with this one: 37 of 40 hashes are the golden's, and three files of the same
sizes are not (`match.map.json`, `kernel.kc.prelink.json`, `kernel.kc.build.json`: the three that carry the
matcher signature). The box's XnuPrelink checkout is at commit `b74ed7c7`, not at the pin the golden was frozen
with; **that this is the cause is a guess, not checked** (the checkout the golden was made with is gone from the
box).

**Where the time is.** The steps' CPU time, one after the other: `match` 3.9 s, `emit` 2.0 s, `align` 1.2 s and
0.9 s, `audit` 1.2 s, `record` 0.9 s, `hosted` 0.7 s, `kclasses`, `structors` and `vtables` 0.4 to 0.5 s each
(13.2 s wall for the fifteen runs). The kexts' scans are a small part of `hosted` and of `emit` (`hosted`: 0.98 s
before, 0.83 s after; `emit`: no change), so the threads gain little there; most of each step is serial work on
the whole kernelcache, which each step reads and parses again. What is left: [todo_rust.md](todo_rust.md) section 9.
**Not run on the phone.**

## kernel

`oneios-tool kernel`: the kernel build's own steps (crate `crates/oneios-kernel`). Each was a Python script,
**retired on 2026-11-13** after the comparison below; each module has its script's name and description, takes what
the script took and writes the same files. `kernel/build-hosted.sh`, `kernel/build-linux.sh` and
`kernel/build-macos.sh` run nothing else for these steps: they need Rust, and build the binary when it is missing or
stale (`stage_tool` in `kernel/lib/stage.sh`; `ONEIOS_TOOL=PATH` for another binary).

| Command | Was (Python) | Module | State |
|---|---|---|---|
| `kernel gen board` | `kernel/device/scripts/build-oneios-board.py` | `build_oneios_board.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen storage` | `kernel/patches/oneios-hosted/storage/build-oneios-hosted-storage.py` | `build_oneios_hosted_storage.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen aes` | `.../aes/build-oneios-aes.py` | `build_oneios_aes.rs`, `class_record.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen keystore` | `.../keystore/build-oneios-keystore.py` | `build_oneios_keystore.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen net` | `.../net/build-oneios-net.py` | `build_oneios_net.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen hid` | `.../hid/build-oneios-hid.py` | `build_oneios_hid.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen audio` | `.../audio/build-oneios-audio.py` | `build_oneios_audio.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen codec` | none: written in Rust (hevc-guest, 2026-11-15) | `build_oneios_codec.rs`, `staged/codec.rs`, `staged/codec_decode.rs` (2026-11-24) | the video encoder's archive `liboneioscodec.a` (below); its staged file's hash and its line are in `tests/kernelgen/golden/` |
| `kernel gen gpu` | `.../gpu/build-oneios-gpu.py` | `build_oneios_gpu.rs`, `class_record.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen display` | `.../display/build-oneios-display.py` | `build_oneios_display.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel build-record finish --host linux\|macos --work DIR --config NAME --out OUT [--repo CHECKOUT]`, `kernel build-record --self-test` | `kernel/build-record.py` | `build_record.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` (the frozen record gained `"linked"` on 2026-11-21, below) |
| `kernel build-record finish --host linux\|macos --work DIR --config NAME --kernel FILE [--post-link hosted-fixup\|seal]... [--repo CHECKOUT]` | new (2026-11-21) | `build_record.rs` | the hosted kernel's record (`kernel/build-hosted.sh`'s last step): of FILE, the build's final `xnu.macho`, written next to it as `FILE.build.json` and `series.txt`; `hosted: true`, `post_link` (also in the build id). Every record now counts its restricted sources in the kernel: `linked.XNU_SOURCE_CORECRYPTO14` = the corecrypto-2023 archive's global symbols (`object`'s archive reader) found among the kernel's external symbols; restricted when any is, or when the archive is missing. Checked: 878 of 1191 for a source kernel and 0 for `ONEIOS_CRYPTO=original`, as `llvm-nm -g` gives; a kernel's own local with an archive name (XNU's static `_d`, ed25519's global `_d`) is not counted. Tests: `--self-test` and `tests/golden.rs` `the_hosted_record_is_of_the_final_file` |
| `kernel layout hid\|net [--kernelcache KC] [--prelink-report JSON] [--out FILE] [--repo CHECKOUT]` | `.../hid/layout.py`, `.../net/layout.py` | `hid_layout.rs`, `net_layout.rs`, `layout.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel port-legacy-display [--kernelcache KC] [--fixture JSON] [--classes JSON] [--out FILE] [--repo CHECKOUT]` | `.../display/port-legacy-display.py` | `port_legacy_display.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel compile-command RECORD SOURCE OBJECT [--repo CHECKOUT]` | new | `drivers_build.rs` | prints the compile command the archives are built with (it was the check of this crate's copy against `drivers/build.py`, which is retired: `drivers_build.rs` is the port now) |
| `kernel hosted-series BOARD SERIES` | the first inline `python3` block of `kernel/build-hosted.sh` | `hosted_device.rs` | **retired 2026-11-21** (below, "The kernel build's last Python"): SERIES as the last entry of the device copy's `[kernel] patch_series` |
| `kernel hosted-archives BOARD DEFCONFIG ENTRY...` | its second block | `hosted_device.rs` | **retired 2026-11-21**: the ENTRYs in place of the board's own archive entry; the hosted defconfig's six force-loads and `CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y` |
| `kernel host-cc SETUP_TOOLCHAIN CLT CLANG` | the inline `python3` block of `kernel/linux/unixnu-layout.sh` (it imported UniXNU's `scripts/linux/setup-toolchain.py`) | `host_cc.rs` | **retired 2026-11-21**: `xnu-host-cc`, `unixnu-host-prefix.h` and `copyfile.h` from that file's text (read, never run) |
| `kernel display-capture --session DIR [--out PREFIX] [--seconds N] [--every S]` | `kernel/patches/oneios-hosted/display/display-capture.py` | `display_capture.rs` | **retired 2026-11-21**: the PC viewer of xnuhost's `display.sock` (Linux); also reads half-float frames, which the script refused |

`kernel gen audio`'s edit list (`staged/audio.rs`) gained the Speaker's hardware volume control on 2026-11-22
(fix-audio-volume-haptics, `docs/hardware.md` section 6, "Volume"): `synthetic/audio.in` has three more raw anchors and
`audio.out` the result (`cargo test -p oneios-kernel`, 14 of 14), and `staged.sha256` the staged `DroidVMAudio.cpp` of
`HOSTED_OUT=audio` (`9feea520...`, 37,706 bytes). That build's `stdout.txt` and the rest of `staged.sha256` were not
re-recorded (its log paths are the main checkout's through the worktree's `out` link, and its net line counts 7 slots
of IO80211Interface against the golden's 6: not this change).

On 2026-11-25 (fix/audio-volume-rt) the edit list gained the block "fix-audio-curve" (UniXNU-sheng's volume curve behind
`oneios-audio-curve`, `docs/hardware.md` section 6, "The volume curve"): its anchors are text of the earlier blocks, so
`synthetic/audio.in` is unchanged and `audio.out` has the result; `staged::audio::tests::the_volume_curve_is_shengs`
recomputes the two staged tables from the formula. `staged.sha256` has the staged `DroidVMAudio.cpp` of
`HOSTED_OUT=audiovol` (only that line re-recorded).

Shared: `gen.rs` (where a generator runs, Python's file and process calls as the scripts used them, the steps every
script repeated), `drivers_build.rs` (`drivers/build.py`: `record`, `command`, and since 2026-11-13 the whole script), `macho_object.rs` (a compiled
object's symbols and the signed entries of a vtable), `staged/` (the edit lists of the staged sources).

Since 2026-11-13 (hang) `kernel gen gpu` also compiles a staged copy of UniXNU-droidvm's `DroidVMVirtioGpu3D.cpp`
(`build/oneios-gpu/staged/`, edit list `staged/gpu.rs` `GPU3D`: three weak hooks that hand each blob's creation, deferred
close and release to `OneiOSAdreno.cpp`'s GPU memory report, `docs/gpu.md` 5.1; since the widgets work of the same day
five more: each IOSurface use a command buffer names and its end, each CPU lock of a surface, each submission, and the
physical segments of each blob of the caller's own pages, for the report's surface trail, `docs/gpu.md` 5.1 "Where a
surface's pixels go", and a sixth for the test switch `oneios-gpu-read-wait`). This list has no Python original:
its made-up source and the expected result are written by hand (`tests/kernelgen/golden/synthetic/gpu3d.in`, `.out`),
checked by `the_edit_lists_write_what_the_python_functions_wrote`. Run on the Linux PC: `cargo test -p oneios-kernel
-p oneios-tool --release --test golden`, 6 of 10 pass, the edit lists among them; the 4 others read
`tests/kernelgen/golden/build/`, which the repository's `.gitignore` (`build/`) keeps out of git, so they fail in any
fresh checkout (not caused by this change). `cargo test -p oneios-kernel` alone does not compile: `oneios-prelink`'s
`toml` needs the `serde` feature, which only the whole workspace turns on.

Since 2026-11-24 (a56-gpu, the Galaxy A56's Metal accelerator; `docs/a56-gpu.md` 5) `kernel gen gpu` also stages:
- NativeAdreno's `NativeAdrenoAccelerator.cpp`, with the new edit list `staged/gpu.rs` `ACCELERATOR`: the A56
  accelerator's open types and its CPU-composition filter in `copyProperty` and a new `serializeProperties`;
  `staged_accelerator_text`, with the made-up `synthetic/accelerator.in`/`.out`;
- two more edits of `GPU`: the Metal plug-in's names come from `OneiOSAdreno.cpp`;
- two more edits of `GPU3D`: each DRM context made is reported to `OneiOSAdreno.cpp`.

The archive's sources gain `gpu/OneiOSGpuChoice.h`. `the_gpu_edits_fit_the_pinned_sources` applies the three lists to
the real pinned files, when `external/` has them. Run in this worktree on the build laptop: `cargo test -q --test
golden` in `crates/oneios-kernel`, 15 of 15.

**`kernel gen codec`** (hevc-guest, 2026-11-15; the tenth archive, after `audio` in `kernel/build-hosted.sh`) builds
`build/oneios-codec/liboneioscodec.a`, the hosted kernel's video encoder: UniXNU-droidvm's virtio-media transport
(`DroidVMVirtioMedia.c`, the Mach record, `-DDROIDVM_DEVICE_DYNAMIC=1`; staged since 2026-11-25 with the edit of
`staged/codec_transport.rs`, 7: the event queue on the device's interrupt, `-I` the codec folder for
`OneiOSCodecWait.h`; checked by `the_transport_edit_fits_the_pinned_source`), its
`AppleVideoToolboxParavirtualizationDriver` (`DroidVMVTParavirt.cpp`, the IOKit record; the late object without
`__mod_init_func`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp` global), the sessions behind it (`DroidVMVTHost.cpp`,
staged into the archive's folder with the edits of `staged/codec.rs`, the encoder's 1 to 5 (the hvcC's profile,
tier and level bytes without the SPS's emulation prevention bytes, the tile session, 10-bit pictures), and
`staged/codec_decode.rs`, the decoder's 6 (2026-11-24: VP9 and AV1, `dcap`, `dcan`, the surfaces' kernel mappings
kept, the decode counters; `kernel/patches/oneios-hosted/codec/README.md` "The decoder")) and `kernel/patches/oneios-hosted/codec/OneiOSCodec.c`. It requires
eight definitions (the start, the transport, the registration and its initializer, and the four calls between the
three files) and that neither C++ object has a static initializer left, and writes `objects.json`. It reads no
record of the kernelcache and writes no bindings: the archive subclasses nothing of Apple's.
`ONEIOS_CODEC_RECORDS=DIR ONEIOS_CODEC_OUT=DIR` run it against a finished build's compile records, without a kernel
build (how it was first tried: the records of another agent's tree, four objects, no warning). No Python original;
its checks: `the_codec_edit_writes_its_frozen_text_and_fits_the_pinned_source` in `tests/golden.rs` (the edit list on
`tests/kernelgen/golden/synthetic/codec.in` against `codec.out`, written by hand, twice applied an error, and on the
real file where `external/` has it), and `tests/kernelgen/compare-golden.sh` after a build (the staged
`oneios-codec/DroidVMVTHost.cpp`'s hash in `staged.sha256`, the generator's line in `stdout.txt`). What it builds
and why: `kernel/patches/oneios-hosted/codec/README.md`. Run on the Linux PC, 2026-11-15: `cargo test -p
oneios-kernel -p oneios-tool --release --test golden`, 13 of 13 (the goldens under `tests/kernelgen/golden/build/`
are in git since the `.gitignore` exception); the hosted build `HOSTED_OUT=hevc` ran the step as
`archive-13-oneios-tool` (`build-oneios-codec: TREE/build/oneios-codec/liboneioscodec.a (4 objects: ...)`), and
`compare-golden.sh WORK` on its tree: 17 of 17 generated files the same, the 11 staged hashes and the ten generators'
output the same after `UPDATE=1` recorded two new lines (the codec's staged file and its output line) and one that
had gone stale on main (`native-legacy-display/LegacyDisplayProbe.cpp`, 44,290 bytes now: the display's edit list
grew after the goldens were taken). `UPDATE=1` used to remove the whole `golden/` folder first, `synthetic/` with
it (the made-up sources of `cargo test`, which the script does not record); it removes only what it writes now.

**`kernel gen NAME`** builds one board archive. UniXNU's driver runs it between the two kernel passes, in the build
tree, with `UNIXNU_DEVICE` set (and `ONEIOS_REPO`, which `kernel/build-hosted.sh` exports): an entry of
`board.toml`'s `[kernel] archives` that is not a `.py` or `.sh` script is run as a command, so the entries read
`"oneios-tool kernel gen aes"` and the build scripts put the binary on `PATH` under that name
(`$KERNEL_OUT/bin/oneios-tool`, a link to `ONEIOS_TOOL`; built first when missing or stale,
`scripts/lib/oneios-tool.sh`). The step's log is `logs/archive-NN-oneios-tool.log` (was
`archive-NN-build-oneios-NAME.log`). A failure ends as the scripts' did: the message on stderr, status 1.

What still runs Python, and why:
- (`drivers/build.py` ran here until 2026-11-13: `kernel gen board` builds `drivers/` in-process now, with
  `drivers_build.rs`, which is the script's port and no longer a copy of two of its functions: the group 2 section.)
- Other projects' own scripts, run as they are: UniXNU's `scripts/prepare-storage-headers.py` (`gen storage`; the
  script imported it and called `prepare`, the command runs it as a program and reads the `headers.json` it writes,
  the same report) and Droid-VM's `build-droidvm-gpu.py`, cut before its accelerator step (`gen gpu`).
- (`kernel/prelink/classes.py` ran here until 2026-11-13: `kernel layout` calls `oneios-prelink`'s `classes.rs` now.)
- (`kernel/patches/oneios-hosted/display/display-capture.py`, the PC boot's viewer, was kept until 2026-11-21: it is
  `kernel display-capture` now, below.)

Kept from the scripts on purpose: the generated files still say "Generated by build-oneios-NAME.py" (the bytes are
the scripts'; the module of that name is `build_oneios_NAME.rs`), and so do the comments in the patch folders'
sources and in three patches, which were not touched (a changed source or patch changes the build's inputs).
One line is not the scripts': `objects.json` of `audio` and of `net` listed the script itself (its copy in the build
tree) as the last source, so that a changed edit list changed the record; the command lists the staged source
there (`DroidVMAudio.cpp`, `DroidVMVirtioNet.c`: the upstream file with the edit list applied).

### The comparison

`tests/kernelgen/compare-python.sh WORK ENVFILE [NAME...]` (run at commit `a5b2d95`, the last with every script;
deleted on 2026-11-21 with the last of the Python): in the tree of a finished hosted build, each generator ran twice in the same place, the Python
script and then the command, in the environment UniXNU's driver gave the archive step (`tests/kernelgen/save-env.sh`
saves it from `/proc/PID/environ` while a build runs) and under the same `/usr/bin` shims. Every file either one
writes is compared (`diff -r`: objects, archives, staged sources, generated sources, logs, JSON records, links), and
so are stdout, stderr and the exit status. Result on Linux x86_64, 2026-11-13, clang 23.1.4, the tree of
`HOSTED_OUT=kgen` at `ea06d46` (a loaded box: the times are the compiler's, not the generators'):

| Generator | Same | Files | Python | Rust |
|---|---|---|---|---|
| `board` (`liboneiosdrivers.a`, `liboneiosboard.a`) | yes | 21 | 5.2 s | 3.6 s |
| `storage` | yes | 32 | 1.8 s | 1.4 s |
| `aes` | yes | 12 | 3.4 s | 0.7 s |
| `keystore` | yes | 8 | 0.9 s | 0.8 s |
| `net` | yes, but the one line of `objects.json` above | 40 | 2.4 s | 1.9 s |
| `hid` | yes | 22 | 2.3 s | 3.1 s |
| `audio` | yes, but the one line of `objects.json` above | 10 | 1.0 s | 0.8 s |
| `gpu` | yes | 95 | 3.9 s | 4.0 s |
| `display` | yes | 21 | 4.1 s | 3.4 s |
| `build-record` (`--self-test`; `finish` on that build: the record of 147 patches and 59 board sources, `series.txt`, the kernel, the link, stdout) | yes | 8 | - | - |
| `compile-command` (6 commands: a C and a C++ source with each of the three records) | yes | - | - | - |
| `layout hid`, `layout net`, `port-legacy-display` (on the kernelcache `349c4692...`) | yes, and each record is the committed one | 1 each | 4.2 s, 1.9 s, 0.7 s | 2.9 s, 2.8 s, 0.4 s |

The files of every rerun are also the ones the build itself had written (261 files), so the reruns stand for the
build.

**The kernel.** `kernel/build-hosted.sh` (`HOSTED_OUT=kgen XNU_FILESET_REFERENCE=1`, from an empty tree, the same
tree each time, the box loaded) with the Python generators, with the commands beside the scripts, and after the
retirement:

| | `xnu.macho` (24,728,008 bytes) | `reference.macho` | Time |
|---|---|---|---|
| Python generators (`ea06d46`) | `5da980a23d26cf14f30272b100533fbf8441c6222211db0f8c610ed41b24d771` | `815ca46e986a5661...` | 1253 s |
| `oneios-tool kernel gen` (`KERNELGEN_IMPL=rust` at `a5b2d95`) | `effea0af3597279d01be685e93bc7a6b50a37d6713fe444754ad8e00737c21a6` | `a51d90d1f28c63ba...` | 1011 s |
| The retired state (the scripts deleted, `stage_tool` builds the binary) | `4a5b8868df0776a77520bb2514e62c19e319faa878091c8420fcb17a4fff1c8b` | `a84207f4a13cd9c1...` | 1032 s (the tool's own build included) |

**The SHA-256 are not equal, and no two builds of this kernel are**: the file carries the time of its build.
`oneios-tool debug kernel-diff A B` (was `tests/kernelgen/kernel-diff.py`) names the place of every differing byte. Of the 2,074 that differ between the
first two kernels, 20 are the time in XNU's version string (`Thu Nov 12 23:53:46 IST 2026` and `Fri Nov 13 00:31:02 IST
2026`, twice in `__TEXT,__const`), 16 are the link's `LC_UUID`, and 2,038 are the values of the symbol table's
`N_OSO` entries (each object file's modification time); **nothing else differs**: every byte of code and data is the
same. The RELEASE reference, which has no board archive in it, differs between the two builds in the same three
ways (76 bytes), so this is the build, not the generators. And what the generators hand to the link is the same:
the ten archive folders of the build with the commands are, file for file, the ones of the Python build (261 files;
the one line of the two `objects.json` aside). The third build, of the retired state, differs from the Python build
in the same three ways and nothing else (2,074 bytes), and its archive folders are the same files again.

The Linux build in the main checkout (`out/kernel/current-hosted-pc`, `13ed0b70...`) is from an older source and
another tree, so it is no reference for this.

**The VM kernel.** `kernel/build-linux.sh` in the retired state (Linux, 482 s from an empty tree with UniXNU's source
cache): the committed `board.toml`'s entry ran as the command (`logs/archive-06-oneios-tool.log`; the repository
found from the device directory, as the script did without `ONEIOS_REPO`), and the record `kernel build-record`
wrote at the end (build id `xnu-12377.121.6-ios-27.0.1-24A446-0196b7c45972`, 94 patches, 17 board sources) is, with
`series.txt`, the kernel and the link, byte for byte what the retired `build-record.py` (put back for this check)
writes for the same build.

### The goldens

`tests/kernelgen/golden/` is the Python scripts' output, frozen before they were deleted:

| What | Frozen as | Checked by |
|---|---|---|
| The generated files that are this project's own text (17: the bindings `*.S` of aes, gpu, display, hid and net, `OriginalLegacyFramebufferABI.h`, the two probe sources, the crumbs header, hid's and net's `report.json`, the GPU's `status.json`), from the Python build above | whole, `golden/build/` | `tests/kernelgen/compare-golden.sh WORK` after a build; the ones made from a record alone (aes, gpu, the display's header, the probes) also by `cargo test` |
| The staged copies of other projects' sources and of Apple's `IOBlockStorageDevice.cpp` (11: since hevc-guest, 2026-11-15, also `oneios-codec/DroidVMVTHost.cpp`) | SHA-256, size and name, `golden/staged.sha256` | `compare-golden.sh WORK` |
| What the ten generators print (nine that were Python scripts, and `kernel gen codec`) | `golden/stdout.txt`, the tree's and the checkout's paths taken out | `compare-golden.sh WORK` |
| Each edit list applied to a made-up source by the script's own staging function (8 lists; the Contact Identifier change of 2026-11-12 is in `hid_transport`) | `golden/synthetic/NAME.in` and `NAME.out` | `cargo test` |
| The codec archive's edit lists (`staged/codec.rs`, hevc-guest 2026-11-15; `staged/codec_decode.rs`, 2026-11-24; no Python original), on a made-up source and on the pinned `external/UniXNU-droidvm/package/DroidVMVTHost.cpp` | `golden/synthetic/codec.in` and `codec.out` (written by hand; the decoder's anchors and output added from the edit list, 2026-11-24), and its results in the test | `cargo test` (`the_codec_edit_writes_its_frozen_text_and_fits_the_pinned_source`; the real file is skipped with a line when `external/` is not there) |
| The pen's edit list (`staged/hid_pen.rs`, spen 2026-11-15; no Python original), applied after `hid`'s to the pinned `external/NativeHidInput/NativeHidInput.cpp` | its anchors and results, in the test | `cargo test` (`the_pen_edits_fit_the_pinned_source`; skipped without `external/`) |
| `drivers/build.py`'s command for a made-up record, C and C++ | `golden/synthetic/command.*.json` | `cargo test` |
| `build-record.py`'s record of a made-up build (run from a made-up checkout; without the machine's part and the checkouts' commits) | `golden/synthetic/build-record.json`, `.series.txt` | `cargo test` |

No file of Apple's and no source of another project is in the goldens. Not frozen: objects and archives (they carry
the tree's path and the compiler's version), compiler logs, `objects.json` and `build-report.json` (the SHA-256 of
every source). `cargo test -p oneios-kernel -p oneios-tool`: 12 tests pass (Linux). `compare-golden.sh` on the tree
built with the commands: 17 of 17 generated files, the 10 hashes and the output the same.

The goldens follow the sources: a change of a record, of a header, of an edit list, of `OneiOSSideKeys.inc` or
`OneiOSTouchTime.inc`, of a call of an original function, or of an external pin changes them. Then run
`UPDATE=1 tests/kernelgen/compare-golden.sh WORK` after a build, in the same commit, and say why. The three
kernelcache records have no golden of their own: each command writes the committed record (the comparison above).

### The Mac

The commands use nothing of Linux alone (`std`, the `object`, `which` and `wait-timeout` crates; links are made
with `std::os::unix`), and take the same tools the scripts took (`llvm-ar`, `llvm-nm`, `llvm-objcopy`,
`llvm-objdump` from `PATH`, the storage generator's `/opt/homebrew/opt/llvm/bin/llvm-objcopy` when there is none,
`xcrun` for the SDK's `TargetConditionals.h`). **Checked on Linux only**: the comparison, the kernel builds with the
commands (`build-hosted.sh` three times, `build-linux.sh` once), `compare-golden.sh`, `cargo test`. Not run on a Mac: the build of this crate, `kernel/build-hosted.sh` with
the commands (Xcode 27.2), `kernel/build-macos.sh` with `kernel build-record`. `save-env.sh` is a Linux script
(`/proc`).

## The kernel's and the prelink's small scripts (group 2 of the Python retirement, 2026-11-13)

Eleven more scripts are commands now, each a module named for its script, with the script's description as the
module's header, the script's arguments, and its output byte for byte. All eleven are **retired** (deleted; their
output frozen under `tests/kernelpy/golden` and `tests/prelinkpy/golden`), and the callers run the commands.

| Command | Was (Python) | Module | Called by | Same output |
|---|---|---|---|---|
| `kernel hosted-fixup KERNEL` | `kernel/hosted-fixup.py` | `oneios-kernel/src/hosted_fixup.rs` | `kernel/build-hosted.sh` | yes: 3 frozen cases; the real kernel of a build (24,744,392 bytes, 7 sites): the same file and line |
| `kernel kdk-headers [--url URL] [--dmg FILE] [--apple-kernel-h] OUTPUT` | `kernel/kdk-headers.py` | `kdk_headers.rs` | `kernel/build-macos.sh` (`KDK_HEADERS=1`) | yes on a made-up package: 4 frozen cases (the tree of headers, our `Kernel.h`, the stamp, `--apple-kernel-h` with its early stop, a package that is no xar). **Not run on a real Kernel Debug Kit, and not on a Mac** (hdiutil, the HEAD request) |
| `kernel iig-fix PATH` | `kernel/linux/iig-fix.py` | `iig_fix.rs` | `kernel/linux/unixnu-layout.sh` | yes: 2 frozen cases; the Linux toolchain's real `iig`: the same file, and `unixnu-layout.sh` with the command gives the `iig` of the layout the script made |
| `kernel series-check [--repo CHECKOUT]` | `kernel/tests/series-check.py` | `series_check.rs` | `kernel/tests/host.sh` | yes: 7 frozen cases (this checkout, and one thing broken per case: every message of the script but two); this checkout with `external/` beside it |
| `kernel drivers [--records DIR] [--out DIR] [--check] [--repo CHECKOUT]` | `drivers/build.py` | `drivers_build.rs` (the generators' `record` and `command` were a copy of the script's: now the only one) | `kernel gen board` (in-process), `drivers/tests/host.sh`, the other agents after a driver change | yes: 4 frozen cases with a stand-in compiler; in a real kernel tree with the real compiler: `--check` and the whole build, all 16 objects byte for byte, `objects.json`, the archive's members, stdout, and the objects are the ones the kernel build itself made |
| `prelink classes --xnuprelink DIR --kernelcache KC [--prelink-report JSON] --out FILE CLASS...` | `kernel/prelink/classes.py` | `oneios-prelink/src/classes.rs` | by hand; `kernel layout hid\|net` (in-process: it ran the script) | yes: 2 frozen cases on the real kernelcache, and the record of the eleven classes is the committed `kernel/prelink/classes-24A446-v54.json` byte for byte |
| `prelink stubs-check KERNEL.KC` | `kernel/prelink/stubs-check.py` | `stubs_check.rs` | `oneios-tool cycle`, `tests/prelink/compare-golden.sh`, by hand | yes: 2 frozen cases on made-up collections (666 good; 660 good and 14 bad with the first ten listed); the two real collections `ffbe95f7` and `cdd98e7e`: `stubs: 666 good, 0 bad`, status 0 |
| `prelink unmapped ...` | `kernel/prelink/unmapped.py` | `unmapped.rs` | by hand, after a stub panic | yes: 3 frozen cases on the real kernelcache (three original functions with `--target`; eight return addresses in a kext, as original and as linked addresses); with the disassembly's text too on this machine (llvm-mc 23.1.4) |
| `prelink port-fixture ...` | `kernel/prelink/port-fixture.py` | `port_fixture.rs` | by hand, when XnuPrelink's fixture changes | yes: 3 frozen cases (50 of 154 functions carried over, 6 ambiguous; again on its own output; a reviewed file for another kernelcache) |
| `prelink lead-between A B [--add [NOTE]] [--repo CHECKOUT]` | `kernel/prelink/lead/between.py` | `lead_between.rs`, `lead.rs` | by hand | yes: 3 frozen cases on the `pc` kernel's collection (a paired block; `--add` where the counts differ; `--add` that adds one record) |
| `prelink lead-imports [--add] [--kext ID] [--repo CHECKOUT]` | `kernel/prelink/lead/imports.py` | `lead_imports.rs`, `lead.rs` | by hand | yes: 4 frozen cases on the `pc` kernel's collection (375 lines for every kext; one kext; `--add`: 2 records written, the same file; again after it) |

```sh
tools/oneios-tool/tests/kernelpy/compare-golden.sh                       # 20 cases: made-up inputs, no Apple file
tools/oneios-tool/tests/prelinkpy/compare-golden.sh                      # 10 cases: the kernelcache alone
tools/oneios-tool/tests/prelinkpy/compare-golden.sh PRELINK_OUT KERNEL   # + 8 cases: the lead tools, on a kernel's collection
#   UPDATE=1: write the goldens from this run
```

**How the goldens were frozen**: at the last state with both implementations, `IMPL=python UPDATE=1` wrote each
golden from the Python scripts, and the same script then ran the commands against it. Result on Linux x86_64,
2026-11-13: `kernelpy` **20 of 20** cases the same, `prelinkpy` **18 of 18** (10 on the kernelcache `349c4692...`,
8 on the collection `ffbe95f7...` of the kernel `13ed0b70...`), and 18 of 18 again with `FULL=1` (llvm-mc's
disassembly lines compared too, into a scratch golden). A case is the command's stdout, its stderr, its status and
the SHA-256 of every file it wrote. `tests/kernelpy/compare-python.sh WORK ENVFILE` ran both side by side on the
real inputs before the retirement (the drivers in a kernel tree, the fixup on a real kernel, the toolchain's `iig`,
this checkout's series): **10 of 10** the same. The `IMPL=python` switch of the compare-golden scripts and
`compare-python.sh` were deleted on 2026-11-21 (no Python is left to run).

What the goldens hold: `kernelpy/golden/cases.txt` is made from made-up inputs only (`oneios-tool debug fixture
hosted-kernel|kdk-pkg`, `tests/kernelpy/fixtures/`: a made-up `iig`, the drivers' list as it was). The `prelinkpy`
goldens are made from Apple's kernelcache and keep **names, addresses, sizes and SHA-256 only**: long outputs are
hashed, `unmapped`'s disassembly is counted, and the code words `lead-between` prints for an unpaired function are
taken out.

Differences that are on purpose:
- A wrong command line ends with clap's message and status 2 (the scripts: their docstring or a traceback, status
  1 or 2). A failure the scripts met with a traceback (a missing file, a name that is paired with nothing, a
  package shorter than a xar header) ends with one line and status 1.
- `drivers`: the list of sources is `drivers/sources.txt` (`UNIVERSE PATH` per line), not a list in the tool: it
  stays with the drivers, and a new driver is one line there. The generators' check that the checkout has
  `drivers/build.py` is a check for `drivers/sources.txt` now.
- `lead-imports`, `lead-between`: `--repo CHECKOUT` names the checkout whose files are read (the scripts took the
  one they lay in). A pair of the matcher without a name (47 data pairs of `ffbe95f7`) is no name; where such a
  target sits in a kext's import table, `lead-imports` stops with a message (the script: a TypeError).
- `series-check` lists the patches that are not in the series by name (the script: in the folder's order).
- `oneios-tool cycle` runs `prelink stubs-check` of its own binary; when stubs are bad its message is the count
  line and the first three bad stubs (it was the output's last four lines, which had lost the count).

**The kernel build with the commands** (`kernel/build-hosted.sh`, `HOSTED_OUT=pyp XNU_FILESET_REFERENCE=1`, from an
empty tree both times, the same tree, Linux):


| | `xnu.macho` (24,744,392 bytes) | `reference.macho` | Time |
|---|---|---|---|
| main `c1223f8` (`hosted-fixup.py`, `drivers/build.py` run by `kernel gen board`) | `fb290c4eddb0a19be0837773ec930a2b22d5d957aa8cea25247a240dd0d93992` | `6e58861659d31353...` | 985 s |
| this branch (the commands; the five kernel scripts deleted) | `0608178d5a727008b2527257a0a6206d3292371ef4f7fa7f59702aafebd6fac4` | `aa0b6dee25949f5f...` | 1168 s |

No two builds of this kernel have one SHA-256 (README "kernel", above). `oneios-tool debug kernel-diff` on the two:
**2,037 bytes differ: 8 in the build's time in XNU's version string, 16 in the link's `LC_UUID`, 2,013 in the values
of the symbol table's `N_OSO` entries; nothing else.** The same for the kernel before the fixup (2,037 bytes) and
for the RELEASE reference (64 bytes: 8, 16 and 40). The ten archive folders are the same files in both builds (183
files, `sha256sum`: 0 differ, `liboneiosdrivers.a`'s 16 objects among them), the build's step logs have the same
names, and `hosted-fixup` printed the same line both times (7 sites, the same addresses). The Linux toolchain's
layout was not rebuilt by these builds (it is shared on the box): `kernel/linux/unixnu-layout.sh` was run apart
into a prefix of its own, and its `iig` is the shared layout's, which `iig-fix.py` had fixed.

Checked on Linux only: everything above. Not run on a Mac: the crate's new modules, `kernel/build-hosted.sh` with
`kernel hosted-fixup`, `kernel/build-macos.sh` with `kernel kdk-headers` (hdiutil and the HEAD request are the
macOS path; ureq follows the redirect as urllib did, not tried), `kernel/prelink/build.sh` (unchanged by this
group: it ran none of these scripts). Nothing Linux-only is used: `std`, `flate2`, `roxmltree`, `lzma-rs`, `ureq`
(rustls), `regex`, `toml`; `llvm-nm` and `llvm-mc` come from `PATH` with Homebrew's keg-only LLVM added, as the
scripts did. `tests/kernelpy/compare-golden.sh` takes its `kdk-headers` cases from the golden on macOS (hdiutil
attaches no made-up image there).

Python the kernel build still runs: other projects' scripts only, run as they are (the list below, "The kernel
build's last Python"). The two inline `python3` blocks of `kernel/build-hosted.sh` that edited its copy of
`board.toml` and the defconfig were ported on 2026-11-21 (below).

### The kernel build's last Python (2026-11-21)

Python is retired from this repository (the owner, 2026-11-21). The kernel's and the runtime's scripts had five
inline `python3` blocks and one script left; each is a command or plain shell now, and the block or script is gone:

| Was | Now | Tests |
|---|---|---|
| `kernel/build-hosted.sh`: the block that put the hosted series last in the device copy's `[kernel] patch_series` | `oneios-tool kernel hosted-series BOARD SERIES` (`hosted_device.rs`) | unit tests that pin the edited text (a multi-line list, a one-line list, the first list only, an empty list as Python wrote it) and the refusal `build-hosted: board.toml has no [kernel] patch_series` |
| its block that put the nine archives in place of the board's own entry and added the defconfig's force-loads and `CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y` | `oneios-tool kernel hosted-archives BOARD DEFCONFIG ENTRY...` (`hosted_device.rs`) | unit tests of both edits and the three refusals; nothing is written when either file is refused (the block wrote `board.toml` before it read the defconfig); a CRLF file is read as Python's text mode read it. `kernel/tests/host.sh` runs both commands on copies of the committed files |
| `kernel/linux/unixnu-layout.sh`: the block that imported UniXNU's `scripts/linux/setup-toolchain.py` for `HOST_PREFIX_H`, `COPYFILE_H` and `HOST_CC` | `oneios-tool kernel host-cc SETUP_TOOLCHAIN CLT CLANG` (`host_cc.rs`): the three constants are read from the file's text (Python's string literals, `''' """ ' "`, `r`/`u`, the escapes), `HOST_CC` is filled in as `str.format` did, `{arch_defs}` from uname's machine as `platform.machine()`; a constant that is not one literal, or is assigned twice, is refused | unit tests on a stand-in file (escapes, a line continuation, `{{}}`, the paths as `pathlib` printed them, x86_64 and aarch64) and on what is refused |
| `kernel/hosted-dev.sh patch`: the block that set the patch's `sha256` in `xnu-inputs.json` | `oneios-tool json set FILE "patches[path=$PATCH].sha256" "\"$sum\""` (the sum: `kernel/lib/host.sh` `sha256`); a patch that is in no record yet is left, as the block left it | - |
| `kernel/patches/oneios-hosted/display/display-capture.py` | `oneios-tool kernel display-capture` (`display_capture.rs`, Linux): the same lines; also reads half-float frames (`display-format=rgba16f`, written to the PNG as sRGB, clamped), which the script refused | unit tests of the hello and flush messages, the PNG (read back: signature, CRCs, IHDR, the rows), the colour sample, half floats; a session against a stand-in xnuhost (a `SOCK_SEQPACKET` socket, the hello with a memfd, two flushes, the close) |
| `runtime/package/test.sh`: the manifest check, the stand-in Mach-O, the build record, the kernel's entry | POSIX shell and `oneios-tool json get\|lines\|set` (the Mach-O header with `printf`) | the 24 tests of `test.sh` |

Compared with the Python on the same inputs before the blocks were deleted (each block run once from `git show`):
the device copy's `board.toml` and `oneios_hosted_defconfig` (the committed files, this checkout's series) **byte for
byte the same**; `host-cc`'s three files on the pinned UniXNU (`62d0b11`) **the same** (also for a CLT path with
`//` and `/./`, which `pathlib` normalised) and the same as `out/toolchain/unixnu`'s, which the block made; `json
set` on a real `xnu-inputs.json` with a stale sum **the same file** as the block's. `display-capture` was not run
beside a boot (no PC boot in this change).

Python that the kernel build still runs, other projects' and pinned (not ours, so not ported): UniXNU's
`./unixnu` driver (`kernel/lib/stage.sh` `unixnu()`) and the scripts it runs from the build tree
(`prepare-xnu-source.py`, which fetches DriverKit headers from GitHub; `build-corecrypto-14.py`,
`build-libpthread-kernel.py`: `kernel/device/board.toml`'s archives), its seal `scripts/seal-source-crypto-kernel.py`
(`kernel/build-hosted.sh`), Droid-VM's `build-droidvm-gpu.py` (`kernel gen gpu`), UniXNU's
`scripts/prepare-storage-headers.py` (`kernel gen storage`), and xnu-linux-toolchain's `iig`. (XnuPrelink's
`emit_prelink.py`, which the OS profile names for UniXNU's own prelink, does not run: the build stops before the
prelink, `--skip-prelink`, and `kernel/prelink/build.sh` is `oneios-tool prelink`.) So `python3` stays a host package of the kernel build (`kernel/build-linux.sh`'s
check, `kernel/linux/setup-toolchain.sh`).

## package: the runtime package's Apple-file check (group 3 of the Python retirement, 2026-11-13)

`oneios-tool package check|manifest|archive` (crate `crates/oneios-package`, module `pack.rs`): what
`runtime/package/pack.py` did for `runtime/package.sh`, with the script's three commands and arguments. **The
script is retired** (deleted; its output frozen in `tests/packagepy/golden/cases.txt`), `runtime/package.sh` runs
the command (it finds or builds the binary: `scripts/lib/oneios-tool.sh`), and CI builds the tool before the host
tests and the package (`.github/scripts/setup-build.sh`: a current stable Rust, then `cargo build --release`; both
workflows run that script).

| Command | Does | Exit status |
|---|---|---|
| `package check STAGE` | refuses Apple files and anything the OneOS fork's unpacker rejects | 3 with `package: refused: ...` on stderr; 1 for a file that cannot be read |
| `package manifest STAGE VERSION GIT_COMMIT GIT_DIRTY MIN_FORK PLACEHOLDER` | the same check, then `STAGE/runtime.json` (every file's size, SHA-256 and mode, set to 0755 or 0644) | 3 also without an executable `bin/xnuhost` |
| `package archive STAGE MEMBERS MTIME OUT.tar` | the ustar archive of the listed files, owner 0:0, one time (where `tar` is not GNU tar) | 3 for a member that is no regular file |

**It refuses exactly what the script refused.** Every rule, in the script's order, with the script's message:

- By path: a link or special file; a path segment outside `[A-Za-z0-9_][A-Za-z0-9._+-]*`. These are found for the
  whole stage before any name or content is looked at; a folder's own entries come before its folders (`os.walk`
  with sorted names).
- By content, whatever the name: every Mach-O magic (thin and fat, both widths, both byte orders); an Image4
  container (a DER sequence with `IM4P`, `IMG4`, `IM4M` or `IM4R` in its first 24 bytes); `AEA1`; a UDIF trailer
  (`koly` 512 bytes before the end, or at the start); `dyld`, and `NXSB` at offset 0 or 32.
- A Mach-O is accepted only as the kernel that `kernel/` built: 64-bit little-endian, arm64, `MH_EXECUTE`, with
  `NAME.build.json` beside it that says `builder` `oneios-kernel`, the file's SHA-256, `kernelcache_kexts_added`
  and `prelinked` both `false` (the JSON value, nothing else), `apple_binary_inputs` and `restricted_sources` both
  `[]`, and a `build_id`. A kernel collection (`MH_FILESET`) is refused with or without a record; a second kernel
  is refused.
- Under `lib/guest/` (since 2026-11-24, not a rule of the script): a Mach-O is accepted only as our own iOS plug-in,
  a thin arm64 `MH_DYLIB` or `MH_BUNDLE`, signed ad hoc for iOS (`oneios-tweaks`' macho reader) with an identifier
  that is not `com.apple.*`; a file of `NAME.bundle/` must have the CDHash `NAME.build.json` beside the bundle
  lists for it under `files`. `lib/guest/NativeAdrenoMetal.bundle` must hold `NativeAdrenoMetal` and `oneios-target`,
  with `lib/guest/NativeAdrenoMetal.build.json` of the same `target`; `lib/guest/camera/H9ISP.mediacapture` only with
  that bundle. `package manifest` names them in runtime.json as `metal` and `camera` (null without them). Another iOS
  build's plug-ins in `lib/guest/<build>/` (named after the bundle's build, only beside the default's) are listed with the
  default's in `metals` and `cameras` (2026-11-25). Another build's kernel at `kernel/<build>/xnu.macho` (the build its
  record's `profile` names, only beside `kernel/xnu.macho`, one per profile) is listed with the default in `kernels`, each
  entry `kernel`'s plus `build`; `kernel` stays the default's. Tests: `pack_tests.rs`
  `another_builds_kernel_is_listed_in_kernels`, `a_second_target_is_listed_beside_the_default`.
- By name (lower case, 19 patterns): `kernelcache*`, `*cryptex*`, `*.ipsw`, `*.dmg`, `*.aea`, `*.img`, `*.simg`,
  `*.erofs`, `*.im4p`, `*.img4`, `*.im4m`, `*.kc`, `*.trustcache`, `buildmanifest.plist`, `restore.plist`, `*.shsh*`,
  `dyld_shared_cache*`, `*.kext`, `*.dext`.

```sh
tools/oneios-tool/tests/packagepy/compare-golden.sh     # 91 cases against pack.py's frozen output (made-up stages)
runtime/package/test.sh                                 # 29 tests of the whole runtime/package.sh (needs the NDK; part 5, METAL_DIR and CAMERA_DIR, needs external/metal2vulkan and mesa-droidvm for the licence texts)
cargo test -p oneios-package                            # 9 tests: every rule and its message (3 of them lib/guest/'s, on debug fixture's signed Mach-Os)
```

Result on Linux x86_64, 2026-11-13: the golden was written with `IMPL=python UPDATE=1` from `pack.py`, then the
command ran against it: **91 of 91 cases the same** (80 with status 3, 11 with status 0): 24 names, 14
contents, 29 Mach-O and kernel-record cases, 10 paths, links and a pipe, 4 orders of two refusals, 6 manifests (the bytes of
`runtime.json` three times, the modes it sets) and 4 archives (the tar's bytes: Python's `tarfile` in
`USTAR_FORMAT`, a name of 126 bytes split into the prefix field). `runtime/package/test.sh` before and after: **19
of 19, the same output** line for line (its lines carry each refusal's message).

Differences that are on purpose: where the script ended with a Python traceback and status 1 (a file that is a
Mach-O magic and under 16 bytes; a record that is JSON but no object), the command refuses with a message and
status 3: no package either way. The text after `unreadable build record:` is this parser's, not Python's. A wrong
command line: clap's message and status 2 (the script: its docstring, status 1).

`pack.rs` uses `oneios-common`'s `json` (serde_json: `runtime.json` with an indent of 2), `pyrepr` (the values in
the messages) and the `tar` crate's `Builder` (ustar; a name too long for ustar as a GNU long name; since 2026-11-21:
the archive is no longer Python tarfile's bytes, so the 4 archive cases of the comparison above are history). Checked on Linux
only; nothing Linux-only is used (`std::os::unix` for modes and links, as macOS has them). On macOS
`runtime/package.sh` takes `package archive` instead of GNU tar: that path was compared with `pack.py archive` here
(the 4 archive cases), not run on a Mac.

## image and remotezip: the IPSW, the AEA header and the archive (group 3, 2026-11-13)

The six subcommands that `oneios-tool image` still handed to `scripts/image/imagetool.py` are its own now, and
`scripts/fetch/remotezip.py` is `oneios-tool remotezip`. **No script calls `imagetool.py` any more**:
`scripts/image/build.sh`, `verify.sh`, `kernel-inputs.sh`, `seed-apfs-macos.sh` and `scripts/fetch/fetch.sh` run
the CLI (they find or build the binary, `scripts/lib/oneios-tool.sh`; `IMAGETOOL_IMPL` is gone). `imagetool.py`,
`remotezip.py` (which it imported) and `scripts/image/tests/` (its own tests) were left in place, unused, until the
owner decided: **deleted on 2026-11-21** (no Python of ours in the repository), with the comparisons that ran them
(`tests/imagepy/compare-python.sh`, `tests/image/parity.py`, `tests/image/compare-python.sh`). The commands and
results below are the record of that comparison.

| Command | Was | Module (`crates/oneios-image/src`) | Same output |
|---|---|---|---|
| `remotezip ls\|cat\|get\|head SOURCE ...` | `scripts/fetch/remotezip.py` | `remotezip.rs` | yes: 9 cases on a stand-in IPSW; 3 unit tests (the list and the fetch, a resumed fetch, a chunk marked done that is not: `test_image.py`'s `RemoteZipTest`) |
| `image check-ipsw PROFILE [--source S]` | `imagetool.py check-ipsw` | `ipsw.rs` | yes: 7 cases; **the real IPSW over HTTP** (12.7 GB, a ZIP64 directory): the same 16 lines |
| `image fetch PROFILE INDIR [-j N] [--only ROLES] [--source S]` | `imagetool.py fetch` | `ipsw.rs` | yes: 6 cases (`inputs.json` and every fetched file the same); four small members of the real IPSW over HTTP: the same output, files and `inputs.json` |
| `image record-plain INDIR ROLE PLAIN` | `imagetool.py record-plain` | `ipsw.rs` | yes: 1 case |
| `image aea-info FILE` | `imagetool.py aea-info` | `aea.rs` | yes: 2 cases |
| `image pack SETDIR OUT.tar.zst` | `imagetool.py pack` | `archive.rs` | yes until 2026-11-21: the archive of a stand-in set was the Python tool's byte for byte; since then the `tar` crate's `Builder` writes it (same members and order, ustar headers, two zero blocks at the end) |
| `image verify SET.tar.zst` | `imagetool.py verify` of an archive | `archive.rs` | yes: the good archive, and 8 refusals with imagetool.py's message each |

What ran (deleted since): `tests/imagepy/compare-python.sh` (25 cases, both tools side by side on one stand-in
IPSW), `cargo test -p oneios-image` (33 tests; `archive_tests.rs` ran imagetool.py beside the port where it was
there), `tests/image/parity.py` (the nine older subcommands: 138 cases), `scripts/image/tests/run.sh`
(imagetool.py's own tests, 23, build.sh among them).

Result on Linux x86_64, 2026-11-13: `compare-python.sh` **25 of 25** (stdout, the last line of stderr, the status and
every file left, per case); `cargo test -p oneios-image` 33 pass, where the port's archive and each of `verify`'s
refusals (a sidecar hash of another archive, a first file that is not the manifest, a file twice, files missing, a
file that is not listed, a changed file, an empty tar, a file that is no zstd stream) are compared with
`imagetool.py`'s on the same files; `parity.py` 138 of 138; `scripts/image/tests/run.sh` 23 of 23 with `build.sh`
on the CLI; `scripts/image/kernel-inputs.sh` on the real members: the same `kernelcache.macho` (`349c4692...`),
`devicetree.bin` and `SHA256SUMS` as before. **Not rerun: a full `scripts/image/build.sh` on the real inputs** (the
nine subcommands it uses were compared byte for byte on the real set and in two full builds in the earlier rounds,
"image" above; `pack` of the real 17 GB set was not run with either tool here).

`remotezip`'s connection rule is the script's: each address of the host gets at most 5 s, and an address family
that failed is tried last from then on (ureq's resolver hook tries the addresses and hands over the one that
answered). On this box (no IPv6 route; Apple's CDN answers AAAA first) the first request waits 10 s on two IPv6
addresses, as the script does, and every later one goes to IPv4 at once: `check-ipsw` 10 s (Python 12 s). The
script gave the last address the whole request timeout; here it gets 5 s too.

Differences that are on purpose: where the scripts ended with a traceback (a file that is no ZIP, `record-plain`
of a role that was not fetched, a wrong command line of `remotezip`), the commands end with one line and status 1.
The ZIP directory is read by 70 lines of our own (the end record, its ZIP64 form, the entries: two range
requests), not by the `zip` crate: the members' own headers are read only for the members that are fetched.
Libraries: `ureq`, `flate2`, `crc32fast`, `regex`, `plist` (the BuildManifest), `tar` (reading the archive), the
`zstd` program for the archive as in the script. `HTTP(S)_PROXY` is not read (urllib did).

Checked on Linux only. Not run on a Mac: these modules, `scripts/fetch/fetch.sh` and `scripts/image/build.sh` with
the CLI (`seed-apfs-macos.sh` now takes the profile from `oneios-tool image profile`).

**`scripts/serve/rangeserve.py` is `oneios-tool rangeserve DIR PORT`** (`rangeserve.rs`; retired, `scripts/serve/serve-image.sh`
starts the command): `tests/servepy/compare-golden.sh` asks 31 requests of the server with curl (whole files, a HEAD,
ranges of every form, ranges a file does not have, the names that are not served) and compares the status, the
headers that matter and the body's SHA-256 with the script's frozen answers: **31 of 31** the same; 2 unit tests. Not
compared: the `Server` and `Date` headers and the page of a 404 (Python's own). One difference: a `bytes=-N` range of an
empty file is 416 (the script answered 206 with no bytes). Not run through a tunnel, and not on a Mac.

**Not ported in this round** (group 3's rest): `scripts/ios/{ios-sign,macho-check,make-tbds}.py`. Ported on
2026-11-21 as `oneios-tool ios` and deleted ("ios", above).

## image tweaks: the root tweaks and the userland hook (group 4 of the Python retirement, 2026-11-21)

The owner agreed to the port on 2026-11-21 (the phone must make the whole image itself). Crate `crates/oneios-tweaks`,
the port of `userland/roottweak` (1643 lines) and `userland/image-hook`. The Python stayed in place as the frozen
original until the comparison below was done; **the owner retired it on 2026-11-21** (no Python of ours in the
repository): `userland/roottweak`, `userland/tweak`, `userland/image-hook`, `userland/tests/test_roottweak.py` and
`fakeroot.py`, build.sh's `--python-hook` and `tests/tweakspy/compare-python.sh` are deleted.

```sh
oneios-tool image tweaks apply --root OS_TREE --data DATA --out OUT [--layout sealed|rw] [roottweak apply's options]
oneios-tool image tweaks apply --root-image os.img [--root-volume N|NAME] --data DATA --out OUT ...   # no mount (sealed only)
oneios-tool image tweaks hook [--repo DIR] [--config DIR]    # hook API 1 from ONEIOS_* (docs/image-format.md 6)
oneios-tool image tweaks daemons [--group NAME] [--no-group NAME] [--keep LABEL] [--config DIR]
oneios-tool image tweaks cdhash FILE...
oneios-tool image tweaks boot-check --data DATA_VOLUME [--root OS_TREE] [--ignore-owner]
```

`apply`'s options are `roottweak apply`'s (`--group`, `--no-group`, `--keep`, `--disable`, `--scale 2|3`, `--build`,
`--any-version`, `--helpers`, `--metal-plugin`, `--metal-plugin-unchecked`, `--metal-client`, `--camera-plugin`,
`--appos`, `--no-setup|migration|watchdogs|awake|coreanimation`, `--lighter-visuals`, `--plain-keybag`,
`--keep-recover-on-crash`, `--config`), a later one over an earlier one as argparse has it. `hook` reads what
`image-hook` reads (`ONEIOS_HOOK_API` 1, `ONEIOS_OS_ROOT`, `ONEIOS_DATA`, `ONEIOS_OVERLAY`, `ONEIOS_OWNERS`,
`ONEIOS_INFO`, `ONEIOS_BUILD`, `ONEIOS_SCALE`, `ONEIOS_HELPERS`, `ONEIOS_METAL_PLUGIN`, `ONEIOS_CAMERA_PLUGIN`,
`ONEIOS_ROOTTWEAK_ARGS` split as `shlex.split` does), and one more: `ONEIOS_OS_IMAGE`, the OS volume's raw image
instead of a mount. `--repo DIR` gives image-hook's defaults (`DIR/out/userland/{helpers,NativeAdrenoMetal.bundle,camera}`)
and `DIR/userland/config`; without it the three config files built into the binary are read (the phone has no
checkout). `scripts/image/build.sh` runs `hook --repo ROOT` by default since 2026-11-21 (`--python-hook`, which ran
`userland/image-hook` instead, went with it the same day).

The library (`serve`'s `tweaks`, the image build on the phone): `oneios_tweaks::run_hook(&Hook) -> anyhow::Result<HookDone>`
(`src/hook.rs`: the fields are the hook's variables; `HookDone` has the summary that becomes the manifest's
`tweaks.info`), `oneios_tweaks::apply(&Apply)`, `bootcheck::check`, `macho::describe`. An error that is a wrong input
is `oneios_tweaks::Refused` (roottweak's `TreeError` and `ConfigError`); a stop of the request is `job::Stop`.

**The files.** Until 2026-11-21 every file was the Python's byte for byte (plistlib's writers by hand, Python's
JSON layout); the owner let that go on 2026-11-21:

- the property lists are read and written by the `plist` crate (`src/plist.rs` converts to and from its `Value`):
  binary (`bplist00`, which Apple's CoreFoundation and launchd read) or XML, whichever the file was; a dictionary's
  keys sorted when written, so the same value gives the same bytes; an XML date cut to the second (Apple's XML date
  has no fraction). The reader keeps the file's key order (the launchd cache's first job of a label wins). What
  went: plistlib's object order and scalar sharing, `repr(float)` in `<real>`, its base64 lines, and the refusal of
  a binary list that shares a container (it was there only because plistlib would write such a container once).
- the JSON by serde_json (oneios-common's `json`: an indent of 2 where it was 2, compact where it had no indent),
  the report's words as tweaks.py's.
- written as `NAME.part` and renamed (the trees' files too); a stop leaves no part (`job::temp`).

**On the phone.** No program is started (no `git`, no mount: `os_image` reads the OS volume in place), every tree
write asks `job::check()` and counts its bytes (`job::count`); as root the owners are set on the files as well.
Only the OS volume is read: the Python never reads SystemOS or App either.

### The comparison

`tests/tweakspy/compare-python.sh` (deleted with the Python on 2026-11-21) ran both sides on copies of the same stand-in inputs (the Python tests' `fakeroot.py`: a root, signed stand-in Mach-Os,
a Metal bundle with its build record, a camera plug-in, an App cryptex, a staged app): the status, standard
output, standard error's last line without the tool's name, every file of the trees, OUT, the owners and the
summary (type, mode, link target, SHA-256). **33 of 33 the same** (2026-11-21, Linux): the sealed layout with
every option, a second run, property lists of every type already in the Data tree (dates to the microsecond and
in 1601 and 4001, data, big and negative integers, text outside ASCII, nested and empty containers; binary and
XML), the rw layout, 20F66's xnuhost.conf, eight refusals, the hook five ways (with oneios-seed, the plug-in and its
record, a record that does not match, no bundle, API 2), `daemons`, `cdhash`, `boot-check` three ways. The script
was checked to see a difference (an extra option on one side: 4 lines).

On the real 24A446 inputs (2026-11-21, Linux; inputs/24A446, the Metal bundle of out/userland placed, no helpers,
no camera): `scripts/image/build.sh --no-pack --python-hook` against `scripts/image/build.sh --no-pack`:

| File | Python hook | Rust hook |
|---|---|---|
| `data.seed` (1,267,793,920 bytes) | `27ec4e10d773d4e68ee0007a50e31430dc67b79e84b793a37b48e5f469242ac8` | the same |
| `overlay/userland/boot-contract.json` | `b1b3b85f0f90ff3447ec7119f7824f9873ed96a79a329e7e0efb866b64da82e5` | the same |
| `overlay/userland/owner-trust.json` | `2e2823c28531d9675b51ebedb28db7210a0af9b9220803fc5d1ebf7f0a74b986` | the same |
| `overlay/userland/report.json` | `9cd5fe42acda832a569c67f31a098107b06c9c95e7653239dc7c19fc1dafb74d` | the same |
| `overlay/userland/report.txt` | `7d0d621c9210f5be7477f1634858bb1cb9c6b1e2c77079d67f0266a5264dcb4a` | the same |
| `overlay/userland/tweaks.manifest.json` | `a62d5d802a47acb36b260354680086fbcf4132bd38470999c2c699237bab5a90` | the same |
| `overlay/userland/xnuhost.conf` | `f916ca688219e3240fdf9aee0ab4140b32aa628665a43a3bd30ac90dd99c3200` | the same |
| `manifest.json` | `b60fa9f8...` | `bdb1cca6...`: differs only in the hook's record, by design: `tweaks.hook` (`oneios-tool image tweaks`), `hook_sha256` (the tool's binary), `dirty`, `info.tool` |

The images, the trust files and `kernel.kc` are the inputs, the same on both sides. 142 jobs disabled, the plug-in
placed (2 CDHashes). The OS volume read from `os.img` (`ONEIOS_OS_IMAGE`) instead of apfs-fuse's mount: the same
seed (`27ec4e10...` again), owner rules, summary, report and overlay.

**build.sh's record of the hook without Python** (2026-11-21). build.sh wrote the manifest's `tweaks` record
(`hook`, `hook_sha256`, `commit`, `dirty`, `info`) with an inline Python block; it is `oneios-tool json set` and
shell now (`relpath` for a `--hook FILE`, `sha256sum` or `shasum`), and `--seed apfs`'s edit of `seed.json` is
`json set --string ... format apfs-raw`. The two writers on the same inputs: 10 of 10 records the same (the built-in
hook, a hook in the checkout, one outside it, a path with `..`, each with an empty and a full summary). On 24A446's
real inputs with one binary (HEAD's, built in a clean worktree): HEAD's build.sh (the Python block) against this
one: all 21 files the same, `data.seed` `27ec4e10...`, the overlay as in the table, and `manifest.json` the same
but for `tweaks.commit` (another agent committed between the two runs; each run records `git rev-parse HEAD`).
The binary of the shared checkout at that time, which had another agent's uncommitted changes of `oneios-image`,
`oneios-tweaks` and `oneios-common` in it, gave another seed (`5406d527...`, 1,265,063,424 bytes) and another
`tweaks.manifest.json` (`deacce95...`): those changes (the `tar` and `plist` crates in place of the Python-byte writers, the owner's decision: todo_rust.md section 11), not build.sh's.

Tests: `cargo test -p oneios-tweaks`: the 38 cases of `userland/tests/test_roottweak.py` (deleted 2026-11-21;
`tests/roottweak.rs`, each named after the case it ports, on a Rust port of `fakeroot.py`) and 3 unit tests (property lists written and read back, Apple's binary `.GlobalPreferences.plist` among them; shlex); `tests/serve.rs` `tweaks_writes_what_the_hook_command_writes` (the request's files are the command's, byte for byte;
its refusals; `bye`'s children 0).

Different by design: the messages start with `oneios-tool image tweaks:` instead of `roottweak:`; an input Python
ends with a traceback for (a non-string `CFBundleExecutable`, a dictionary key that is not a string) is refused
with a message; a wrong command line ends with clap's usage (status 2, as argparse's).

Not done: **a run on the phone** (as root under `serve`); the Mac (nothing was built or run there).
