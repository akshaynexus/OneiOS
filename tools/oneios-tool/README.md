# oneios-tool: OneiOS's host tools in Rust

One binary, `oneios-tool`, with a subcommand per tool. Each tool that replaces a Python script is compared with that
script before it may replace it; the commands and the results are below. What is left: [todo_rust.md](todo_rust.md).

| Tool | Replaces | State | Same output as the old tool |
|---|---|---|---|
| `sym` | `scripts/debug/dsc.py` | done | yes: byte for byte on 18 of 18 address sets (Linux, 2026-11-12; the Mac's run: 18 of 18) |
| `log` | nothing (new) | done | not applicable; 33 commands end with status 0, two runs equal |
| `image` | `scripts/image/imagetool.py` | 9 subcommands in Rust, the rest run the Python tool | yes: 138 stand-in cases; on the real set 35 of 36 items and the two full builds, byte for byte (the 36th: the Linux set has no `kernel.kc`) |
| `ioslog` | the manual steps of `docs/newtodo.md` "How to resume" step 2 (mount, copy, `unifiedlog_iterator`) | done; not run on the phone's Data store yet | yes on 7 archives: CSV byte for byte from a `.logarchive`, from a Data-store image and from a mounted volume |
| `prelink` | the steps of `kernel/prelink/build.sh` | 2 of 12 steps (`roots`, `hosted`) | yes for those two, byte for byte on the real kernelcache; the others are not ported |

**Python stays the default.** No build script calls `oneios-tool` unless it is asked to
(`IMAGETOOL_IMPL=rust` for `scripts/image/build.sh` and `verify.sh`), with one exception that changes no output:
`scripts/image/kernel-inputs.sh` decodes the kernelcache with `oneios-tool image im4p` when the `lzfse` program is
not installed, and checks the result against the SHA-256 the kernel profile pins.

## Build and tests

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml     # -> tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml     # unit tests: synthetic data only, no Apple file
python3 tools/oneios-tool/tests/image/parity.py -v                     # image against imagetool.py on stand-in files
tools/oneios-tool/tests/compare-python.sh [PART...]                    # every comparison below whose inputs are named (its header)
```

Rust 1.88 or newer (macos-unifiedlogs needs it; built with 1.98.0). Linux x86_64 (2026-11-12): clean build 1 min 20 s
with two jobs, 46 unit tests pass (`oneios-common` 3, `oneios-image` 29, `oneios-log` 7, `oneios-sym` 7),
`parity.py`: 138 cases, all the same. macOS arm64: the three first tools were built and compared there on 2026-11-12
(the numbers in [todo_rust.md](todo_rust.md)); `ioslog`, `prelink`, `image im4p` and the APFS file reader were not
built on a Mac yet.

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

## The rule for libraries

Use a maintained library for every format and every generic job. A part stays hand-written only where no library
exists or where ours is measured to be faster; each such part says why in its module comment.

| Crate | Used for |
|---|---|
| `macos-unifiedlogs` 0.8.0 (Mandiant, Apache-2.0; git commit `3d5b27a`, pinned in `Cargo.lock`) | all decoding of the unified log: tracev3, timesync, uuidtext, shared-cache strings, message formatting. crates.io has 0.7.0 only, whose API has no lazy messages and no loading of string files by UUID, so the dependency names the commit |
| `clap` 4 (derive) | every tool's command line. Each crate defines its arguments (`Cli` or `Command`); `crates/oneios-tool/src/main.rs` puts them under one command, so `oneios-tool --help` and `oneios-tool TOOL --help` list everything |
| `csv`, `serde_json`, `chrono`, `uuid`, `regex` | `ioslog`'s CSV and JSON lines (the same crates `unifiedlog_iterator` writes them with), times, `--grep` |
| `tracing-subscriber` | `ioslog -v`: the decoder's warnings on stderr |
| `simple_asn1`, `lzfse_rust` | `image im4p`: the IM4P container (DER) and its LZFSE payload |
| `plist`, `toml` | `prelink roots`: the kernelcache's `__PRELINK_INFO` and the kernel profile; `image profile`: the build profile |
| `sha2` (`asm`), `rayon`, `memmap2`, `libc`, `anyhow` | hashing, parallel work, file maps (`image`, `sym`, `log`) |

Hand-written, and why:

| Part | Why |
|---|---|
| The APFS reader (`crates/oneios-image/src/apfs.rs`, `apfs/files.rs`) | no maintained Rust crate reads iOS's sealed volumes, or any volume of a container but the first (`apfs` 0.4.1: neither; `apfs-core`: stubs; checked 2026-11-12, [todo_rust.md](todo_rust.md)). apfs-fuse is GPL, Linux only in practice, and needs FUSE |
| `ioslog`'s choice of files, filters, text output, checks (`crates/oneios-ioslog`, 950 lines) | this project's own; everything that decodes a log file is the library's |
| `oneios-common/src/pyjson.rs` | JSON text byte for byte as Python's `json` module writes it (indent, separators, `ensure_ascii`, key order), for files that are hashed or compared with `cmp`. `serde_json` does not write Python's separators and escapes, and its `preserve_order` feature must stay off (below) |
| `oneios-image`: hash lists, seed tar, manifest, Python's `repr` and messages | output that is hashed (`.hashes`, `data.seed`) or compared byte for byte with the Python tool; one read pass feeds the whole-file SHA-256 and the chunk hashes (1.9 to 2.9 times Python on the Mac) |
| `oneios-sym`'s index | the project's own on-disk index: answers in 3 to 10 ms where `dsc.py` takes 0.03 to 53 s |
| `oneios-log` | xnuhost's own log format |

`serde_json`'s `preserve_order` feature must not be enabled by any crate of this workspace: Cargo gives one set of
features to the whole binary, and with it macos-unifiedlogs writes the keys of a decoded state dump in insertion
order where its own `unifiedlog_iterator` writes them sorted (found by `tests/ioslog/compare.sh`: one line of 1,012,281
differed).

## ioslog: iOS's unified log from the phone's Data store

Reads iOS's own log from a copy of the Data store (`data.img`) without mounting it, on Linux and macOS alike, and
writes it as text, CSV or JSON lines. It replaces these manual steps (macOS only): attach the image with `hdiutil`,
mount the Data volume, copy `db/diagnostics/{Persist,Special,Signpost,HighVolume,timesync}` and `db/uuidtext/*` into a
`.logarchive`, run `unifiedlog_iterator -m log-archive -i ARCHIVE -f csv -o OUT`.

```sh
oneios-tool ioslog show SOURCE [-f text|csv|jsonl] [-o OUT] [filters] [--sort]
oneios-tool ioslog boots SOURCE                       # the boots: UUID, start, first and last entry, entries
oneios-tool ioslog checks SOURCE [--boot last|all|UUID] [--examples N]
oneios-tool ioslog save SOURCE NAME.logarchive        # the copy the manual steps made, for other tools
# every subcommand: [--volume N|NAME] [--block0] [-v]
```

- `SOURCE`: a raw image of the Data store; or a `.logarchive` folder; or a folder that holds `db/diagnostics` and
  `db/uuidtext` (a mounted Data volume; also `private/var/db/...`, `var/db/...`, or `diagnostics` + `uuidtext`).
- The image: every volume of the container is tried in order and the first that holds `db/diagnostics` is read (the
  phone's Data volume is the third, named `Data`, with no role). `--volume 2` or `--volume Data` names one. The
  container is read as its **newest valid checkpoint** has it (the superblock with the highest transaction and a
  good checksum in the checkpoint descriptor area), not as block 0 has it: block 0 holds the state of the last clean
  unmount only. `--block0` reads block 0's state.
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
  `timed out after 3.000s`). **The patterns are the texts the project's notes quote; they were written without the
  phone's log at hand.** A count of 0 means the text is not in the log.
- The guest's clock was about a day behind the host's in the 2026-11-12 sessions, so the log's dates are too.
- Not there: `ioslog pull` (copying `data.img` from the phone; `docs/newtodo.md` has the adb command), an
  `Info.plist` in the saved archive (Apple's `log show --archive` wants one; Mandiant's tool does not).

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
files by `tests/ioslog/mkapfs.py` (three volumes, the log in the third as `db/diagnostics` and `db/uuidtext`, block
0 one transaction behind, files over 8 blocks in two extents), which **apfs-fuse must read as the folder it was made
from** before the image is trusted; (4) `ioslog save IMAGE` against the manual copy out of apfs-fuse's mount, with
`diff -r`; (5) `ioslog show IMAGE` against `unifiedlog_iterator` on that manual copy; (6) the same from the mounted
volume; (7) `--block0` must find no log; (8) ten filter combinations, `--sort`, the text output and `checks`
against the same selection made in Python from the unfiltered CSV (`tests/ioslog/filters.py`). The JSON lines of
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
archive's. The box ran two other agents' jobs: the times are not clean.

**What this does not show**, because neither the phone's Data store nor the Mac's files are on the Linux box:

1. `ioslog` on a real `data.img` from the phone (an image written by iOS's own APFS, after a session). The reader's
   paths for it are tested on stand-ins only: `mkapfs.py`'s image (checked against apfs-fuse) and, for images
   written by Apple's APFS, the three below.
2. `ioslog show` against the Mac's `out/session-logs/2026-11-12/ios3-log.csv` (made from `ios3.logarchive`). To
   run it there: `tests/ioslog/compare.sh out/session-logs/2026-11-12/ios3.logarchive`, and
   `oneios-tool ioslog show ios3.logarchive -f csv -o X && cmp X ios3-log.csv` (the same only if `ios3-log.csv` was
   made by `unifiedlog_iterator` of commit `3d5b27a`; version 0.7.0 formats some messages differently).
3. `checks` on a real session's log.

The APFS reader against apfs-fuse on images written by Apple's APFS (`tests/image/apfs-read.sh IMAGE [VOLUME]`: both
copy the volume, every file must be the same, and the names apfs-fuse has more must be exactly the files
`apfs-copy` says it skipped):

| Image | Result |
|---|---|
| dfvfs's `apfs.raw` (macOS, unsealed, 1 volume) | 8 files and links the same, with and without `--newest` |
| 24A446 AppOS image (sealed) | 225 the same, 0 differ; 930 compressed files skipped |
| 24A446 SystemOS image (sealed; the dyld shared cache) | 214 the same (5,421,894,748 bytes), 0 differ; 1,980 compressed files skipped; 64 s |

Files kept compressed (decmpfs: most of the OS volumes, and the shared cache's `.symbols` and `.dyldlinkedit`) are
not read: the reader has no decoder for them. The Data store's log files are not compressed.

## image

`oneios-tool image CMD ...`: the commands, options, output and exit status of `scripts/image/imagetool.py`
(`docs/image-format.md`). In Rust: `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`,
`manifest`, `verify` of a set folder, and `im4p`. `check-ipsw`, `fetch`, `record-plain`, `aea-info`, `pack` and
`verify` of a `.tar.zst` start the Python tool (`IMAGETOOL_PY` names it). Only here:

```sh
oneios-tool image apfs-ls IMAGE [PATH] [--volume N|NAME] [--newest]       # a folder's entries: mode, owner, size, name
oneios-tool image apfs-copy IMAGE SRC DST [--volume N|NAME] [--newest]    # a file or a folder out of the image, no mount
```

`im4p IN OUT` decodes the LZFSE payload with `lzfse_rust` (imagetool.py starts the `lzfse` program). On
`kernelcache.release.v54` of 24A446: 78,135,296 bytes, sha256 `349c4692...0c57e3`, which is the hash the kernel
profile pins (made with the `lzfse` program on the Mac), in 0.7 s. Not done: the two decoders side by side on one
host (the Linux box has no `lzfse` program).

Comparison with the Python tool:

```sh
python3 tools/oneios-tool/tests/image/parity.py -v                      # stand-in files: 138 cases, all the same
SET=out/image/24A446 tools/oneios-tool/tests/image/compare-python.sh    # the real set (header: env and options)
```

Result on Linux x86_64 (2026-11-12; the set built here from the fetched IPSW with `scripts/image/build.sh --no-kernel
--no-hook --no-pack`, 13 files, 17,766,460,758 bytes; `FULL_BUILD=1 BUILD_ARGS=--no-hook`, nothing skipped): **35
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
| `seed-pack` (copied tree; changed tree with owner rules; a bad owner rule) | yes (3) | 12.0 s, 26.8 s | 14.9 s, 25.3 s |
| `manifest` (also the set's own `manifest.json`; a set with a stray file) | yes (3) | 83.9 s | 50.5 s |
| `verify`, `verify --quick`, and 9 sets with one thing wrong each | yes (11) | 99.8 s | 50.3 s |
| **`build.sh --no-pack` with each tool: every file of the two sets** | **yes** | 207.9 s | 131.3 s |

The whole-volume `apfs-owners` and the two full builds were the open points of the first round (skipped, and
stopped part way): both are the same. The box's disk and CPUs were shared with other jobs; the hashing commands gain less
here (1.2 to 2.0 times) than on the Mac (1.9 to 2.9 times).
`IMAGETOOL_IMPL=rust scripts/image/build.sh ...` uses the Rust tool; the default stays `python`.

## sym

`oneios-tool sym [--slide HEX] ADDR...` names user addresses of a boot log in the image's dyld shared cache, as
`scripts/debug/dsc.py` does, from an index it builds once (`out/dsc-index/<cache UUID>/`, 1.07 GiB).
`ONEIOS_DSC_DIR` is the folder with `dyld_shared_cache_arm64e`; on Linux mount the SystemOS image with apfs-fuse
(`apfs-fuse -o ro inputs/24A446/043-70037-715.dmg out/dsc`; the folder is
`out/dsc/root/System/Library/Caches/com.apple.dyld`).

```sh
ONEIOS_DSC_DIR=<cache folder> ONEIOS_LOG_DIR=<folder with *.log> tools/oneios-tool/tests/sym/compare-python.sh
tools/oneios-tool/tests/sym/make-log.py <cache folder> OUT.log     # a stand-in log where there is none from the phone
```

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

The index: built in 26.4 s (1068.8 MiB; 4,696 images, 11,365,799 symbols kept). The Mac's run of the first round
(18 of 18 on sets from four real logs; `dsc.py` 0.03-53 s, `sym` 3-170 ms) is in [todo_rust.md](todo_rust.md).
Not compared on Linux: address sets from real phone logs (none on the box).

## log

`oneios-tool log stacks|timeline|diff|gpu|cpu|display|perf LOG...` reads an xnuhost session log
([todo_rust.md](todo_rust.md) has each subcommand). No Python original.

```sh
ONEIOS_LOG_DIR=<folder with *.log> tools/oneios-tool/tests/log/run.sh
```

Result on Linux x86_64 (2026-11-12), on the two stand-in logs of `make-log.py` (above): 33 commands, all status 0, no
panic, two runs of each equal; 1 s in all. `stacks` finds each log's slide from its addresses (`0xedb4000`: 5173 of
5173 distinct addresses follow a call) and names every frame through the `sym` index. The stand-in logs hold thread
lines and one crash only, so the tables (`gpu`, `cpu`, `display`, `perf`) and `timeline` had almost nothing to read:
their test on real lines is the Mac's run of the first round (67 commands on four real session logs, all status 0)
and the 7 unit tests on synthetic log text.

## prelink

`oneios-tool prelink STEP ARGS...`: a step of `kernel/prelink/build.sh`, with the Python step's own arguments and
output files, so that each can be swapped and compared alone.

| Step | Python | State |
|---|---|---|
| `roots KERNELCACHE OUT.json [PROFILE.toml]` | `roots.py` | ported |
| `hosted KERNELCACHE OUT REPORT.json [--closure FILE] [--x18]` | `hosted.py` | ported (`--x18` starts `llvm-mc`, as Python does; not compared) |
| native-input, merge-reviewed, match (XnuPrelink's `fileset_match.py`), align, kclasses, structors, vtables, emit (`emit.py` over XnuPrelink's `fileset_emit`, `fileset_layout`, `fileset_segmented`), audit, record | | **not ported** |

```sh
scripts/image/kernel-inputs.sh 'profiles/ios-27.0.1-iPhone18,2.toml'      # out/kernel-inputs/24A446/kernelcache.macho
tools/oneios-tool/tests/prelink/compare-python.sh
```

Each step is run by both tools with the same arguments into its own folder; stdout, the exit status and every file
written are compared with `cmp`. Result (Linux x86_64, 2026-11-12, kernelcache sha256 `349c4692...`):

| Step | Same | Python | Rust |
|---|---|---|---|
| `roots` with the profile (`select.json`: 130 roots, 189 left out, 150 in the closure) | yes | 1.06 s | 0.05 s |
| `roots` without a profile (125 roots, 194 left out) | yes | 0.23 s | 0.05 s |
| `hosted --closure select.json` (`hosted.json`, `kernelcache.hosted.macho`: 5 MPIDR_EL1 and 11 pointer rewrites in 40,540,236 bytes of kext code) | yes | 5.3 s | 1.7 s |
| `hosted` over every kext | yes | 5.3 s | 0.8 s |

`kernel/prelink/build.sh` has no switch and runs the Python steps. `roots.rs` holds a copy of `roots.py`'s rule and
lists (the hardware provider classes, the kexts left out by hand): **a change to one must be made to the other**,
and the comparison shows when they drift.
