# oneios-tool: OneiOS's host tools in Rust

One binary, `oneios-tool`, with a subcommand per tool. Each tool that replaces a Python script is compared with that
script before it may replace it; the commands and the results are below. What is left: [todo_rust.md](todo_rust.md).

| Tool | Replaces | State | Same output as the old tool |
|---|---|---|---|
| `sym` | `scripts/debug/dsc.py` (**retired 2026-11-12**: deleted, its output frozen in `tests/sym/golden/`) | done | yes: byte for byte on 18 of 18 address sets (Linux, 2026-11-12; the Mac's run: 18 of 18) |
| `sym kernel`, `log panic` | naming a kernel panic's frames by hand (`atos -o xnu.macho` on the Mac, with each segment's move subtracted) | done; **not run on the Mac's log and kernel of the 2026-11-12 panic** (not on the Linux box) | compared with `llvm-symbolizer --inlining` on three Linux-built pairs: the stand-in panic's 19 addresses and 31,605 more per pair (311,605 on one) name the same function, file:line and inlined frames |
| `log` | nothing (new) | done | not applicable; 33 commands end with status 0, two runs equal |
| `image`, `remotezip`, `rangeserve` | `scripts/image/imagetool.py`, `scripts/fetch/remotezip.py` (**both left in place, unused: the owner decides**), `scripts/serve/rangeserve.py` (retired) | all 15 subcommands in Rust since 2026-11-13, and the image scripts run them | yes: 138 stand-in cases; on the real set 35 of 36 items and the two full builds, byte for byte (the 36th: the Linux set has no `kernel.kc`); the six last subcommands and `remotezip`: 25 of 25 side by side, the archive byte for byte, the real IPSW over HTTP (the section at the end) |
| `ioslog` | the manual steps of `docs/newtodo.md` "How to resume" step 2 (mount, copy, `unifiedlog_iterator`) | done; its first run on the phone's Data store (2026-11-13) failed in the APFS reader, fixed ("The APFS reader": checkpoints); not run on that store again yet | yes on 7 archives: CSV byte for byte from a `.logarchive`, from a Data-store image and from a mounted volume; stores with a cut-off newest checkpoint give the checkpoint before's log |
| `prelink` | the steps of `kernel/prelink/build.sh` | all 12 steps (`PRELINK_IMPL=rust`) | yes: every step's files and stdout byte for byte on two Linux-built kernels, `kernel.kc` `ffbe95f7...` and `cdd98e7e...` |
| `kernel` | the board archive generators of the kernel build (`build-oneios-*.py`, nine), `kernel/build-record.py`, the two `layout.py` and `port-legacy-display.py` | all ported, and the scripts **retired 2026-11-13** (deleted, their outputs frozen in `tests/kernelgen/golden/`): the kernel builds run the commands | yes: every file of the nine archives byte for byte (261 files, one line of two `objects.json` aside), the build record, the three kernelcache records, and `kernel/build-hosted.sh` gives the same kernel either way but for the build time it carries (2,074 bytes of time stamps and the UUID; Linux, 2026-11-13) |
| `kernel hosted-fixup`, `kdk-headers`, `iig-fix`, `series-check`, `drivers`; `prelink classes`, `stubs-check`, `unmapped`, `port-fixture`, `lead-between`, `lead-imports` | `kernel/hosted-fixup.py`, `kernel/kdk-headers.py`, `kernel/linux/iig-fix.py`, `kernel/tests/series-check.py`, `drivers/build.py`; `kernel/prelink/{classes,stubs-check,unmapped,port-fixture}.py`, `kernel/prelink/lead/{between,imports}.py` | all ported, and the scripts **retired 2026-11-13** (deleted, their output frozen in `tests/kernelpy/golden` and `tests/prelinkpy/golden`); **`kdk-headers` not run on a real Kernel Debug Kit or a Mac** | yes: 20 of 20 and 18 of 18 frozen cases, 10 of 10 on real inputs, and `kernel/build-hosted.sh` gives the same kernel but for the build time, the UUID and the `N_OSO` times (2,037 bytes; the section at the end) |
| `package` | `runtime/package/pack.py` (the runtime package's Apple-file check, its manifest and archive) | ported, and the script **retired 2026-11-13**; `runtime/package.sh` and CI run the command | yes: 91 of 91 frozen cases (every refusal rule), `runtime/package/test.sh` 19 of 19 with the same output |
| `debug` | the Python helpers of the tests (`tests/ksym/{compare,kcmap,make-log}.py`, `tests/kernelgen/{kernel-diff,drivers-command}.py`, `tests/phone/mkstream.py`) | done; the helpers deleted | yes: 24 of 24 comparisons, and each test before and after: the same output ("debug", below) |
| `phone` | `scripts/phone/stage.sh`, the adb lines of `docs/building.md` section 6, the Mac's `sparsewrite.py` | written; **not run on the phone** (none on the box) | `stage` and `sums`: the same output and the same stage as `stage.sh` on a stand-in phone; 76 checks pass (`tests/phone/run.sh`); `sparse-write`: the bytes and length of `sparsewrite.py` on 7 streams and a 32 GiB one, not more blocks, a quarter of the CPU (`tests/phone/sparse-compare.sh`) |
| `app` | the app lines of `docs/building.md` section 7 (new) | written; **the app was not built or installed with it** (the owner's decision for this box) | not applicable; 65 checks pass with a stand-in checkout, gradlew, gh and phone (`tests/app/run.sh`); `app version` runs on the real clone |
| `cycle`, `phone log` | the test cycle of `docs/building.md` section 6 by hand (new) | written; **not run on the phone or with real builds** | not applicable; 90 checks pass with a stand-in checkout and phone (`tests/cycle/run.sh`) |

**The image scripts, the kernel builds, the prelink and the runtime package run no Python tool of this repository any more** (2026-11-13; still Python: `scripts/ios/`, the root tweaks). The image scripts call `oneios-tool image`
(`IMAGETOOL_IMPL` is gone; `scripts/image/imagetool.py` is left in the tree, unused), as do the retired tools' callers (`kernel/prelink/build.sh`
and, since 2026-11-13, the kernel builds, `runtime/package.sh` and the kernel's and the prelink's hand tools: "kernel",
"package" and the group 2 section, below). (Before, one script used the Rust tool without being asked:
`scripts/image/kernel-inputs.sh` decodes the kernelcache with `oneios-tool image im4p` when the `lzfse` program is
not installed, and checks the result against the SHA-256 the kernel profile pins. It always does now.)

## Build and tests

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml     # -> tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml     # unit tests: synthetic data only, no Apple file
python3 tools/oneios-tool/tests/image/parity.py -v                     # image against imagetool.py on stand-in files
tools/oneios-tool/tests/compare-python.sh [PART...]                    # every comparison below whose inputs are named (its header)
```

Rust 1.95 or newer (apk-info-zip 1.1.1 needs 1.95, macos-unifiedlogs 1.88; built with 1.98.0, and 1.99.1 on the Mac). Linux x86_64 (2026-11-12): clean build 1 min 20 s
with two jobs, 51 unit tests pass (`oneios-common` 3, `oneios-image` 29 + 1, `oneios-ioslog` 4, `oneios-log` 7, `oneios-sym` 7),
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
| `simple_asn1`, `lzfse_rust` | `image im4p`: the IM4P container (DER) and its LZFSE payload; the LZFSE and LZVN kinds of compressed APFS files |
| `plist`, `toml` | `prelink roots` and `emit`: the kernelcache's `__PRELINK_INFO` and the kernel profile; `image profile`: the build profile |
| `indexmap`, `md-5`, `hmac`, `rand_mt`, `memchr` | `prelink`: Python's dict order, the matcher's function digests, the collection's FIPS seals, `random.Random`'s generator for `align`, the search for the kexts' inline pools |
| `tar` 0.4 (`Header`) | `image seed-pack`: every 512-byte ustar block of `data.seed`, field by field (`crates/oneios-image/src/tar.rs`; what stays ours and why: below) |
| `serde_json` (`ser::Formatter`), `serde` | every JSON file the tools write: Python's `json.dumps` layout is a `Formatter` (`oneios-common/src/pyjson.rs`) |
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
| `oneios-common/src/pyjson.rs`: the value type and the parser | the writer is serde_json's now (a `Formatter`: `", "` and `": "`, Python's indent, `\uXXXX` for everything outside ASCII, `repr(float)`). The value type keeps a dict's insertion order without serde_json's `preserve_order` feature, which must stay off (below); the parser stays ours because serde_json reads neither Python's `NaN` and `Infinity` nor integers beyond 64 bits as integers |
| The seed's tar: four details of the header and the layout (`crates/oneios-image/src/tar.rs`, 100 lines without its tests) | `data.seed`'s SHA-256 is pinned in the image's trust list, and a changed seed starts the phone's Data store over. The `tar` crate writes the block's fields as Python does, except: **(1)** the checksum field (`set_cksum`: seven octal digits and a NUL, `0010715\0`; Python: six digits, a NUL and a space, `010715\0 `), in every header; **(2)** names (`set_path` moves the front of a long name into the ustar `prefix` field and refuses what does not fit; Python's pax format never uses `prefix`: the first 100 characters with `?` for each one that is not ASCII, and the whole name in a `path` record); **(3)** the extended header's own block (`Builder::append_pax_extensions`: an empty name and NUL mode, uid and gid fields; Python: `././@PaxHeader` and `0000000\0`; the records' text is the same); **(4)** the end (`Builder::finish`: two zero blocks; Python fills up to a multiple of 10240 bytes). So the block is `tar::Header` with these four done by hand, and `Builder` is not used (it also writes through one writer, and the seed is written at offsets in parallel) |
| `oneios-image`: Python's `repr` and messages (`py.rs`) | compared byte for byte with the Python tool's output |
| `oneios-image/src/hashes.rs` | one read pass feeds the whole-file SHA-256 and the chunk hashes (1.9 to 2.9 times Python on the Mac); the list itself is a 32-byte header and the digests, written with `std::io::Write` |
| `oneios-sym`'s index | the project's own on-disk index: answers in 3 to 10 ms where `dsc.py` takes 0.03 to 53 s |
| `oneios-log` | xnuhost's own log format |
| `oneios-ksym`: the index (`index.rs`), the walk that fills it (`dwarf.rs`: which addresses, in how many threads, the runs of equal answers), the panic block's reader (`panic.rs`), the search for the member and segment of an address | the project's own on-disk index, as `sym`'s: a later call takes 4 to 10 ms and reads none of the 93 MB collection, the kernel or its 1,070 object files, and stays right when a kernel build replaces them. Everything the index holds is read by `object` and `addr2line`. Three things the libraries leave open are done by hand, each in a few lines: an object file's time is compared with its `N_OSO` stab's (`addr2line`'s loader reads a rebuilt file as it is), instruction sections are found by their flags (`object` knows `__TEXT,__text` only), and `_block_invoke` names are demangled as LLVM writes them |

`serde_json`'s `preserve_order` feature must not be enabled by any crate of this workspace: Cargo gives one set of
features to the whole binary, and with it macos-unifiedlogs writes the keys of a decoded state dump in insertion
order where its own `unifiedlog_iterator` writes them sorted (found by `tests/ioslog/compare.sh`: one line of 1,012,281
differed).

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

The sample of `ksym-compare` and the streams of `mkstream` are the ones the Python helpers drew: `pyrandom.rs` is
CPython's `random.Random` on the `rand_mt` crate's MT19937 (`random`, `getrandbits(32)`, `randrange`, `choice`,
`choices`, `randbytes`; a unit test holds six values of CPython 3.14). The strings that named the scripts name the
commands now (`3. ksym-make-log's list`, `kcmap with the segment's move`, the stand-in log's version line), so a
stand-in log is not byte for byte the one `make-log.py` wrote; the 24 comparisons above were made before that change.

Python left under `tests/`: `tests/phone/sparsewrite.py` (the frozen original `phone sparse-write` is compared
against; `sparse-compare.sh` also has three inline `python3` reads of the file system and the clock beside it),
`tests/image/parity.py`, `tests/ioslog/{filters.py,mkapfs.py}`, `tests/sym/make-log.py`, `tests/app/mkapk.py` (older
rounds' helpers: not part of this change) and inline `python3` lines in `tests/phone/lib.sh`, `tests/phone/run.sh`
and `tests/kernelgen/compare-python.sh`.

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
  `timed out after 3.000s`). **The patterns are the texts the project's notes quote; they were written without the
  phone's log at hand.** A count of 0 means the text is not in the log.
- The guest's clock was about a day behind the host's in the 2026-11-12 sessions, so the log's dates are too.
- Copying `data.img` from the phone is `oneios-tool phone pull-data OUT [--ioslog]` ("phone" below). Not there: an
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
`tests/support/apfs_store.rs` (Rust, for `cargo test`) takes two stores of `mkapfs.py`, an older and a newer
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

`tests/image/parity.py` (the image tool against imagetool.py on stand-ins): 138 cases, all the same.
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

```sh
ONEIOS_DSC_DIR=<cache folder> tools/oneios-tool/tests/sym/compare-golden.sh
```

The script writes two stand-in logs from the cache (`tests/sym/make-log.py`, fixed seeds), takes its 18 address
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
| in the collection | `panic(cpu N caller 0x...)`, the `frame 0x...` lines (`hosted_panic` prints them `- vm_kernel_slide`) | the fileset's `LC_FILESET_ENTRY` members and each member's `LC_SEGMENT_64`s are read; the smallest segment that holds the address gives member and segment (every kext lists the shared `__LINKEDIT`: the first member has it) |
| in `com.apple.kernel` | the same | **xnu.macho address = address - (the segment's vmaddr in the collection - the same segment's vmaddr in xnu.macho), per segment.** The prelink places each segment on its own: here `__TEXT` moves by `0x180000`, `__DATA_CONST` by `0xc24000`, `__TEXT_EXEC` and `__KLD` by `0x386c000`, `__DATA` by `0x3a28000` (`sym kernel index` prints all ten). A symbol moves with the segment of its section (`n_sect`), not of its address: an end label sits on the next segment's first address, and `__LAST` has no size |
| of the running kernel (the "physical aperture") | the fault's `pc` and `lr`, in the panic's text and in its register dump | **collection address = running address - slide** (modulo 2^64), **slide = RAM - link base**. xnuhost maps the guest's RAM once, at `0x2000000000 + (link base mod 2 MiB)` (`runtime/xnuhost/src/mem.c`, retried 16 GiB higher), copies every segment of the collection to `RAM + (vmaddr - link base)` (`runtime/loader/src/boot.c`; link base = the lowest vmaddr of the collection's own segments, `macho.c`) and runs it there with physical = virtual (`docs/booter.md` 2.2). The log has all three numbers: `kernel: ... link base 0xfffffff007004000` and `memory: N MiB at 0x2000004000, physical = virtual (slide 0x2ff9000000)`. So `pc 0x200454c094` is `0xfffffff00b54c094`, the panic's first `frame` line, and `lr 0x2003ff1038` is `0xfffffff00aff1038`, `0x7138` bytes after the `tcp_input` frame. `--slide HEX` gives the slide for addresses by hand or a log cut before its `memory:` line; the tool warns when the log's slide is not RAM - link base or its link base is not this collection's |
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
two stand-in logs (33 commands, all status 0, two runs equal). `tests/compare-python.sh` has the part `ksym`.

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

`oneios-tool log stacks|timeline|diff|gpu|cpu|display|perf LOG...` reads an xnuhost session log
([todo_rust.md](todo_rust.md) has each subcommand). No Python original.

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
`return` and `pull-data` have not run on it yet. `scripts/phone/stage.sh` stays until they have.

```sh
oneios-tool phone status                      # root, the installed app, the session, `oneios_root.sh status` (read only)
oneios-tool phone stage runtime DIR           # an unpacked runtime package, through oneios_root.sh begin/receive/finish
oneios-tool phone stage runtime-asis DIR      # the same with each file's own size and SHA-256
oneios-tool phone stage image DIR             # files of an image set at their paths; image.sha256 and image.ids follow
oneios-tool phone status --watch [SECONDS]    # the same again every 5 s until Ctrl-C
oneios-tool phone log [--file NAME] [--tail N] [--follow] [--save PATH | --save-auto] [filters] [--since S] [--analyze]
oneios-tool phone log --list                  # the logs by name, with sizes
oneios-tool phone sums                        # sha256 of the staged kernel.kc, its hash list, manifest.json, data.seed's line, xnuhost.conf, the runtime
oneios-tool phone image-check                 # the staged manifest.json against the staged files (image.sha256), as xnuhost will (read only)
oneios-tool phone manifest DIR [--out FILE] [--stage]   # the phone's manifest.json with DIR's files' entries set to theirs
oneios-tool phone switch [--version-code N]   # busybox setsid sh oneios_root.sh switch <the installed app's versionCode> 1
oneios-tool phone return [--timeout 180] [--note TEXT]   # session/return.request, then waits for "One UI restored"
oneios-tool phone pull-data OUT [--ioslog]    # data/data.img: gzip -1 on the phone, zero runs as holes here
oneios-tool phone sparse-write OUT [--gunzip] [--expect-size N]   # stdin to OUT, zero runs as holes (no phone)
```

The rules of `docs/building.md` section 6 are in the tool (`crates/oneios-phone/src/adb.rs`):
- **One phone only.** `--serial`, `ANDROID_SERIAL`, or the only phone attached; several attached is refused, and
  so is a phone adb lists as anything but `device`.
- **Root first.** Every command asks `su -c 'id -u'` before anything else and says so when the answer is not 0
  (KernelSU's root does not survive a restart of the phone). Nothing is changed then.
- **Named files only.** No command sends `find`, `ls -R`, `du` or any walk of a tree to the phone (a walk of
  sysfs or /proc panicked the phone's kernel on 2026-11-12). `stage image` lists the *local* folder.
- **Nothing but status, return and log runs beside a session.** A session is open when `session/xnuhost.pid` names a live
  process that is still an xnuhost (stage.sh's test). `sums` refuses too, which stage.sh's does not: it reads the
  whole kernel collection.
- **No restart, nothing under `/sys/power`**, and nothing of One UI's is written: only the stage, through the
  fork's root script or stage.sh's own commands.
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
  phone's `manifest.json` (read with `pyjson`, so the key order and Python's layout are kept), each entry of DIR's
  files set to their SHA-256 and size. Every other entry must already list the staged file, and a file of DIR
  that the manifest does not list is refused. Printed, `--out FILE`, or `--stage`: DIR's files and the manifest
  staged together, then `image-check`. Run on the phone: the manifest it replaced by hand that night (21 entries,
  all equal to `image.sha256`).
- `switch` reads the versionCode from `dumpsys package com.fahadali.oneos` (not a fixed 115; `--version-code`
  for a phone without the app). A refusal of the preflight (`@@REFUSED ...`) is printed and is exit status 1. If
  `setsid` lets adb go before the script answers, the new `epoch` line of `switch.log` is the proof it started.
- `return` does nothing when no session is open. Otherwise it notes `return.log`'s length, writes the request,
  and reads only what the log gained, every 2 s, until `One UI restored` is there; then it prints
  `return.outcome`. At `--timeout` it ends with status 1 and the last lines of the log (and says when the phone
  no longer answers: a restart is the return's failsafe).
- `pull-data` is the adb line of `docs/newtodo.md` "How to resume" step 2 with the Mac's `sparsewrite.py` built in:
  `adb exec-out "su -c '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'"`, unpacked
  with `flate2` (`zlib-rs`), written to `OUT.part` (a block of zeros is not written, the length is set at the
  end: "The sparse copy" below) and renamed to `OUT` only when as many bytes came as the phone's `stat -c %s` of
  that file says, before and after. `--ioslog` then runs `ioslog boots OUT` and `ioslog checks OUT`.
- `sparse-write OUT` is that copy without the phone, and `pull-data` by hand: `adb exec-out "su -c
  '/data/adb/ksu/bin/busybox gzip -1 -c /data/local/tmp/oneios/data/data.img'" | oneios-tool phone sparse-write
  OUT --gunzip`. stdin goes to OUT; `--gunzip` unpacks it first; `--expect-size N` fails when another number of
  bytes came. It prints `OUT: N bytes, M of them written (S s)`.

- `log` reads one named file of the stage and runs beside a session (it only reads). `--file`: `xnuhost`
  (`session/xnuhost.log`, the default), `switch`, `return`, `outcome`, `display`, `watchdog` (the names of
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
    `--mem` (`oneios-swap`, `oneios-fault-around`, `oneios-gpu-memory`, `memorystatus`, `jetsam`). The prefixes
    are the ones `docs/booter.md`, `docs/hardware.md`, `docs/gpu.md` and the hosted patches' README name; they were
    not checked against a real session's log here.
  - `--markers`: the stages of `oneos/interface.md` section 8, each once per boot (xnuhost, kernel, darwin,
    bsd-root, init, launchd, backboardd, SpringBoard, ready), and every panic and end line.
  - `--since SECONDS`: from that second of the log's own `[seconds.millis]` stamps on.
  - `--analyze`: when the reading ends (the file's end, or Ctrl-C of a follow), `oneios-tool log timeline` of the
    saved lines, and `oneios-tool log stacks --summary` when a `sym` index exists (`out/dsc-index/latest` here or
    in the main checkout, or `ONEIOS_DSC_INDEX`). It is not line by line: the analysis is of the log so far.

```sh
tools/oneios-tool/tests/phone/run.sh          # 76 checks against the stand-in phone; ALL OK
tools/oneios-tool/tests/cycle/run.sh          # phone log, status --watch and cycle: 95 checks; ALL OK
tools/oneios-tool/tests/phone/modes.sh        # phone, app and cycle in each /proc mode this host has (below); ALL OK
FAKE_PROC=stand-in tools/oneios-tool/tests/phone/run.sh   # one suite with the stand-in's own /proc (macOS's way)
tools/oneios-tool/tests/phone/sparse-compare.sh   # sparse-write against sparsewrite.py: 35 checks; ALL SAME ("The sparse copy" below)
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

### The sparse copy (`sparse-write`, `pull-data`)

`crates/oneios-phone/src/sparse.rs` replaces the Mac's `sparsewrite.py` (kept unchanged as
`tests/phone/sparsewrite.py`, the comparison's reference). The stream is read 256 KiB at a time. A block that
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
tools/oneios-tool/tests/phone/sparse-compare.sh              # against sparsewrite.py: 35 checks; ALL SAME (MAP=1: every file's data regions)
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
  (4) An error of the stream and one of the file. `cargo test -p oneios-phone` as a whole: 8 passed.
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
- **Safety.** One phone lock for the whole cycle (`out/phone.lock` of the main checkout, `flock`; a second cycle
  is refused and told who holds it). Before the switch: root, no open session, the three scripts staged, no
  import in progress, the root script's preflight `ready 1`. From the switch on, whatever happens (a panic, no
  first frame in time, the session ending by itself, Ctrl-C, an error while watching), the return is made and
  waited for, unless `--stay` was given and Ctrl-C came in the hold. A failed build, a failed audit, stubs that
  are not all good or failing host tests end the cycle before the phone is touched.
- The exit status is 0 only when every step that ran ended well and the boot reached its first frame (when
  `watch` ran). A `collect` that failed (a log that could not be copied, `--pull-data`'s copy or its `ioslog`)
  fails the cycle too: status 1, `oneios-tool cycle: collect: ...`.
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

## app

`oneios-tool app [--oneos DIR] [--serial S] CMD`: the OneOS fork's app (`akshaynexus/OneOS`, branch `oneios`).
**The app was not built and not installed with this tool yet**: the owner's decision is that the Linux box builds no
app and gets no JDK 21 or Android SDK for it, so the tool is written for a host that has them (the Mac) and tested
with stand-ins. `app version` was run on the real clone.

```sh
oneios-tool app version [--no-phone]     # applicationId, versionName, versionCode, the commit; the installed app's version and certificate
oneios-tool app doctor                   # JDK 21, an Android SDK with platform 37, the GitHub Packages login
oneios-tool app build [--no-test]        # cd app-android && ./gradlew test assembleFullDebug; then the APK's path, size, sha256, version
oneios-tool app install [APK]            # adb install -r of the build output (or APK)
oneios-tool app install --release 0.4.86 [--repo akshaynexus/OneOS] [--dir out/app]   # the CI prerelease oneios-<version>, through gh
oneios-tool app stage-scripts            # the fork's oneios_switch.sh, oneios_return.sh and oneios_root.sh into the stage
```

- **The checkout** is `--oneos DIR`, else `ONEOS_DIR`, else the current folder when it is one (it has
  `app-android/settings.gradle.kts`), else `OneOS` beside the OneiOS main checkout, found through
  `git rev-parse --git-common-dir`, so it works from a worktree. The first line of every command says which one
  and how it was found.
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
- `stage-scripts` is for sessions started without the app (`docs/newtodo.md` step 4): each of the three files is
  pushed, copied to `NAME.part` in the stage as root (`root:root`, mode 0700) and moved into place, so what was
  there is replaced and never written through; the SHA-256 of each staged file is printed before and after and
  must be the fork's file's. The app pins these three by SHA-256: staging other versions than the installed
  app's makes the app's own checks refuse them until it stages its own again.

```sh
tools/oneios-tool/tests/app/run.sh            # 65 checks; ALL OK
```

The stand-ins: a OneOS checkout whose `gradlew` is a script that records its arguments and "builds" an APK; APKs
made by `tests/app/mkapk.py` (a zip with an APK Signing Block v2 around a certificate made with `openssl`; the
signature is filler, the tool reads the certificate and does not verify the APK); a `gh` that serves a release's
two files; the stand-in phone of `tests/phone/`. Result, Linux x86_64, 2026-11-12: 65 checks, all ok. What they
cannot show: a real Gradle build, real APKs signed by `apksigner` or Gradle (v2 and v3 blocks as Android writes
them), `adb install` on Android, `gh` against GitHub.

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
| `emit` (emit.py's options) | `emit.py` over XnuPrelink's `fileset_emit.py`, `fileset_layout.py`, `fileset_segmented.py` | `emit.rs`, `layout.rs`, `plistpy.rs` |
| `audit` (audit.py's options, `--against` and `--draft` too; exit 1 on a failure) | `audit.py` | `audit.rs` |
| `record --work DIR ... [--repo CHECKOUT]` | `record.py` | `record.rs` |
| `pin PROFILE` | the `tomllib` one-liner of `build.sh` | prints `[kernel.prelink] kernelcache_sha256` |

What the Rust steps still read from the XnuPrelink checkout (`--xnuprelink`, `external/XnuPrelink`): its reviewed
fixtures (`fixtures/fileset-*-symbols.json`) and the files its matcher signature is the SHA-256 of
(`fileset_match.py`, `kc_image.py`, `fileset_shape.py` and the fixtures). The signature is written into
`match.map.json` and checked by `emit`, as XnuPrelink does: it says which matcher rules a map follows.
`matcher.rs` and `emit.rs` are ports of the pin `9b950f2`; when the pin moves, the port must follow, and the
comparison below shows a drift.

Hand-written here, and why (the library rule): the Mach-O reader (`image.rs`: the chains' walk order, the kernel
collection pointer formats 7 and 8 and fileset file offsets; `object` and `goblin` read none of the three),
CPython's `set` order and `random.sample` (`pyset.rs`: `align`'s holdout sample is drawn from a list made by
iterating a set; no library has them; the generator is the `rand_mt` crate), and `plistlib`'s XML writer
(`plistpy.rs`: the rewritten prelink information is inside `kernel.kc`, whose hash is recorded; reading is the
`plist` crate). Libraries: `indexmap` (Python's dict order), `md-5`, `sha2`, `hmac` (the FIPS seals), `rand_mt`,
`plist`, `toml`, `base64`, `memchr`, `rayon` (the per-function scan, the audit's per-kext scan).

The comparison today is with the frozen output of the Python chain (`tests/prelink/golden/`: per kernel, the name,
size and SHA-256 of every file a step writes and the SHA-256 of every step's stdout; no Apple bytes):

```sh
scripts/image/kernel-inputs.sh 'profiles/ios-27.0.1-iPhone18,2.toml'      # out/kernel-inputs/24A446/kernelcache.macho
KERNEL=out/kernel/current-hosted-pc/xnu.macho REFERENCE=out/kernel/current-hosted-pc/reference.macho \
  tools/oneios-tool/tests/prelink/compare-golden.sh     # 40 hashes; without KERNEL: roots and hosted only, 10 hashes
```

A golden exists for the kernelcache alone and for the two kernels the Python chain was run on (`13ed0b70...` and
`399f50fa...`, both Linux-built, in the main checkout's `out/kernel/current-hosted-pc` and `-pc2`); for any other
kernel the script says there is none and exits 3 (`UPDATE=1` records that run instead). In the JSON records and
the stdout the work folder, the commit of `kernel.kc.build.json` and the folders of absolute input paths are
taken out before hashing. Result, 2026-11-12, after the Python steps were deleted: 40 of 40 the same for each
kernel, 10 of 10 for the kernelcache alone.

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
| `kernel gen gpu` | `.../gpu/build-oneios-gpu.py` | `build_oneios_gpu.rs`, `class_record.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel gen display` | `.../display/build-oneios-display.py` | `build_oneios_display.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel build-record finish --host linux\|macos --work DIR --config NAME --out OUT [--repo CHECKOUT]`, `kernel build-record --self-test` | `kernel/build-record.py` | `build_record.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel layout hid\|net [--kernelcache KC] [--prelink-report JSON] [--out FILE] [--repo CHECKOUT]` | `.../hid/layout.py`, `.../net/layout.py` | `hid_layout.rs`, `net_layout.rs`, `layout.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel port-legacy-display [--kernelcache KC] [--fixture JSON] [--classes JSON] [--out FILE] [--repo CHECKOUT]` | `.../display/port-legacy-display.py` | `port_legacy_display.rs` | **retired 2026-11-13**: deleted, its output frozen in `tests/kernelgen/golden/` |
| `kernel compile-command RECORD SOURCE OBJECT [--repo CHECKOUT]` | new | `drivers_build.rs` | prints the compile command the archives are built with (it was the check of this crate's copy against `drivers/build.py`, which is retired: `drivers_build.rs` is the port now) |

Shared: `gen.rs` (where a generator runs, Python's file and process calls as the scripts used them, the steps every
script repeated), `drivers_build.rs` (`drivers/build.py`: `record`, `command`, and since 2026-11-13 the whole script), `macho_object.rs` (a compiled
object's symbols and the signed entries of a vtable), `staged/` (the edit lists of the staged sources).

Since 2026-11-13 (hang) `kernel gen gpu` also compiles a staged copy of UniXNU-droidvm's `DroidVMVirtioGpu3D.cpp`
(`build/oneios-gpu/staged/`, edit list `staged/gpu.rs` `GPU3D`: three weak hooks that hand each blob's creation, deferred
close and release to `OneiOSAdreno.cpp`'s GPU memory report, `docs/gpu.md` 5.1). This list has no Python original:
its made-up source and the expected result are written by hand (`tests/kernelgen/golden/synthetic/gpu3d.in`, `.out`),
checked by `the_edit_lists_write_what_the_python_functions_wrote`. Run on the Linux PC: `cargo test -p oneios-kernel
-p oneios-tool --release --test golden`, 6 of 10 pass, the edit lists among them; the 4 others read
`tests/kernelgen/golden/build/`, which the repository's `.gitignore` (`build/`) keeps out of git, so they fail in any
fresh checkout (not caused by this change). `cargo test -p oneios-kernel` alone does not compile: `oneios-prelink`'s
`toml` needs the `serde` feature, which only the whole workspace turns on.

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
- `kernel/patches/oneios-hosted/display/display-capture.py` is kept as it is: it is no generator but a viewer for a
  PC boot ([docs/building.md](../../docs/building.md) section 5), Linux only (a `SOCK_SEQPACKET` socket with the
  frame's memfd passed over it), and the tool is to build and run on the Mac too.

Kept from the scripts on purpose: the generated files still say "Generated by build-oneios-NAME.py" (the bytes are
the scripts'; the module of that name is `build_oneios_NAME.rs`), and so do the comments in the patch folders'
sources and in three patches, which were not touched (a changed source or patch changes the build's inputs).
One line is not the scripts': `objects.json` of `audio` and of `net` listed the script itself (its copy in the build
tree) as the last source, so that a changed edit list changed the record; the command lists the staged source
there (`DroidVMAudio.cpp`, `DroidVMVirtioNet.c`: the upstream file with the edit list applied).

### The comparison

`tests/kernelgen/compare-python.sh WORK ENVFILE [NAME...]` (at commit `a5b2d95`, the last with every script; since
the retirement it can still run `compile-command`, whose Python side stays): in the tree of a finished hosted build, each generator runs twice in the same place, the Python
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
| The staged copies of other projects' sources and of Apple's `IOBlockStorageDevice.cpp` (10) | SHA-256, size and name, `golden/staged.sha256` | `compare-golden.sh WORK` |
| What the nine generators print | `golden/stdout.txt`, the tree's and the checkout's paths taken out | `compare-golden.sh WORK` |
| Each edit list applied to a made-up source by the script's own staging function (8 lists; the Contact Identifier change of 2026-11-12 is in `hid_transport`) | `golden/synthetic/NAME.in` and `NAME.out` | `cargo test` |
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
the commands (Xcode 27.2), `kernel/build-macos.sh` with `kernel build-record`. `compare-python.sh` and `save-env.sh`
are Linux scripts (`/proc`, `bwrap`).

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
#   UPDATE=1: write the goldens from this run   IMPL=python: run the scripts (only at a commit that has them)
```

**How the goldens were frozen**: at the last state with both implementations, `IMPL=python UPDATE=1` wrote each
golden from the Python scripts, and the same script then ran the commands against it. Result on Linux x86_64,
2026-11-13: `kernelpy` **20 of 20** cases the same, `prelinkpy` **18 of 18** (10 on the kernelcache `349c4692...`,
8 on the collection `ffbe95f7...` of the kernel `13ed0b70...`), and 18 of 18 again with `FULL=1` (llvm-mc's
disassembly lines compared too, into a scratch golden). A case is the command's stdout, its stderr, its status and
the SHA-256 of every file it wrote. `tests/kernelpy/compare-python.sh WORK ENVFILE` ran both side by side on the
real inputs before the retirement (the drivers in a kernel tree, the fixup on a real kernel, the toolchain's `iig`,
this checkout's series): **10 of 10** the same; it says "retired" now.

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

Python the kernel build still runs: UniXNU's own driver and scripts (`external/Droid-VM-UniXNU`: another project's),
and the two inline `python3` blocks of `kernel/build-hosted.sh` that edit its copy of `board.toml` and the
defconfig (not in this group's list; they change no file of this repository).

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
- By name (lower case, 19 patterns): `kernelcache*`, `*cryptex*`, `*.ipsw`, `*.dmg`, `*.aea`, `*.img`, `*.simg`,
  `*.erofs`, `*.im4p`, `*.img4`, `*.im4m`, `*.kc`, `*.trustcache`, `buildmanifest.plist`, `restore.plist`, `*.shsh*`,
  `dyld_shared_cache*`, `*.kext`, `*.dext`.

```sh
tools/oneios-tool/tests/packagepy/compare-golden.sh     # 91 cases against pack.py's frozen output (made-up stages)
runtime/package/test.sh                                 # 19 tests of the whole runtime/package.sh (needs the NDK)
cargo test -p oneios-package                            # 6 tests: every rule and its message
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

`pack.rs` uses `oneios-common`'s `pyjson` (the manifest's layout), `pyrepr` (the values in the messages) and `pytar`
(tarfile's ustar block: it was `oneios-image/src/tar.rs`, moved so that both crates use one). Checked on Linux
only; nothing Linux-only is used (`std::os::unix` for modes and links, as macOS has them). On macOS
`runtime/package.sh` takes `package archive` instead of GNU tar: that path was compared with `pack.py archive` here
(the 4 archive cases), not run on a Mac.

## image and remotezip: the IPSW, the AEA header and the archive (group 3, 2026-11-13)

The six subcommands that `oneios-tool image` still handed to `scripts/image/imagetool.py` are its own now, and
`scripts/fetch/remotezip.py` is `oneios-tool remotezip`. **No script calls `imagetool.py` any more**:
`scripts/image/build.sh`, `verify.sh`, `kernel-inputs.sh`, `seed-apfs-macos.sh` and `scripts/fetch/fetch.sh` run
the CLI (they find or build the binary, `scripts/lib/oneios-tool.sh`; `IMAGETOOL_IMPL` is gone). **`imagetool.py`
is left in place, unused, on purpose: the owner decides when it goes**; `remotezip.py` (which it imports) and
`scripts/image/tests/` (its own tests) stay with it.

| Command | Was | Module (`crates/oneios-image/src`) | Same output |
|---|---|---|---|
| `remotezip ls\|cat\|get\|head SOURCE ...` | `scripts/fetch/remotezip.py` | `remotezip.rs` | yes: 9 cases on a stand-in IPSW; 3 unit tests (the list and the fetch, a resumed fetch, a chunk marked done that is not: `test_image.py`'s `RemoteZipTest`) |
| `image check-ipsw PROFILE [--source S]` | `imagetool.py check-ipsw` | `ipsw.rs` | yes: 7 cases; **the real IPSW over HTTP** (12.7 GB, a ZIP64 directory): the same 16 lines |
| `image fetch PROFILE INDIR [-j N] [--only ROLES] [--source S]` | `imagetool.py fetch` | `ipsw.rs` | yes: 6 cases (`inputs.json` and every fetched file the same); four small members of the real IPSW over HTTP: the same output, files and `inputs.json` |
| `image record-plain INDIR ROLE PLAIN` | `imagetool.py record-plain` | `ipsw.rs` | yes: 1 case |
| `image aea-info FILE` | `imagetool.py aea-info` | `aea.rs` | yes: 2 cases |
| `image pack SETDIR OUT.tar.zst` | `imagetool.py pack` | `archive.rs` | yes: the archive of a stand-in set is the Python tool's byte for byte (and its `.sha256`) |
| `image verify SET.tar.zst` | `imagetool.py verify` of an archive | `archive.rs` | yes: the good archive, and 8 refusals with imagetool.py's message each |

```sh
tools/oneios-tool/tests/imagepy/compare-python.sh     # 25 cases, both tools side by side on one stand-in IPSW
cargo test -p oneios-image                            # 33 tests; archive_tests.rs runs imagetool.py beside the port
python3 tools/oneios-tool/tests/image/parity.py       # the nine older subcommands: 138 cases
scripts/image/tests/run.sh                            # imagetool.py's own tests (23), build.sh among them
```

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

**Not ported in this round** (group 3's rest):
`scripts/ios/{ios-sign,macho-check,make-tbds}.py` (the iOS-side signing, the
import check and the stub maker, called by `scripts/setup/ios-toolchain.sh` and `userland/*/build.sh`). They are
unchanged and still Python.
