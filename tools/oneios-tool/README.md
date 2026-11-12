# oneios-tool: OneiOS's host tools in Rust

One binary, `oneios-tool`, with a subcommand per tool. Each tool that replaces a Python script is compared with that
script before it may replace it; the commands and the results are below. What is left: [todo_rust.md](todo_rust.md).

| Tool | Replaces | State | Same output as the old tool |
|---|---|---|---|
| `sym` | `scripts/debug/dsc.py` (**retired 2026-11-12**: deleted, its output frozen in `tests/sym/golden/`) | done | yes: byte for byte on 18 of 18 address sets (Linux, 2026-11-12; the Mac's run: 18 of 18) |
| `log` | nothing (new) | done | not applicable; 33 commands end with status 0, two runs equal |
| `image` | `scripts/image/imagetool.py` | 9 subcommands in Rust, the rest run the Python tool | yes: 138 stand-in cases; on the real set 35 of 36 items and the two full builds, byte for byte (the 36th: the Linux set has no `kernel.kc`) |
| `ioslog` | the manual steps of `docs/newtodo.md` "How to resume" step 2 (mount, copy, `unifiedlog_iterator`) | done; not run on the phone's Data store yet | yes on 7 archives: CSV byte for byte from a `.logarchive`, from a Data-store image and from a mounted volume |
| `prelink` | the steps of `kernel/prelink/build.sh` | all 12 steps (`PRELINK_IMPL=rust`) | yes: every step's files and stdout byte for byte on two Linux-built kernels, `kernel.kc` `ffbe95f7...` and `cdd98e7e...` |
| `kernel` | the board archive generators of the kernel build (`build-oneios-*.py`, nine), `kernel/build-record.py`, the two `layout.py` and `port-legacy-display.py` | all ported; Python is still what the builds run (`KERNELGEN_IMPL=rust` for the archives) | yes: every file of the nine archives byte for byte (261 files, one line of two `objects.json` aside), the build record, the three kernelcache records, and `kernel/build-hosted.sh` gives the same kernel either way but for the build time it carries (2,074 bytes of time stamps and the UUID; Linux, 2026-11-13) |
| `phone` | `scripts/phone/stage.sh`, the adb lines of `docs/building.md` section 6, the Mac's `sparsewrite.py` | written; **not run on the phone** (none on the box) | `stage` and `sums`: the same output and the same stage as `stage.sh` on a stand-in phone; 64 checks pass (`tests/phone/run.sh`) |
| `app` | the app lines of `docs/building.md` section 7 (new) | written; **the app was not built or installed with it** (the owner's decision for this box) | not applicable; 65 checks pass with a stand-in checkout, gradlew, gh and phone (`tests/app/run.sh`); `app version` runs on the real clone |
| `cycle`, `phone log` | the test cycle of `docs/building.md` section 6 by hand (new) | written; **not run on the phone or with real builds** | not applicable; 90 checks pass with a stand-in checkout and phone (`tests/cycle/run.sh`) |

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
| `flate2` | `phone pull-data`, `phone sparse-write --gunzip`: the gzip stream of the phone's busybox; the zlib kinds of compressed APFS files |
| `zbitmap` 0.1 (MIT, github.com/valentinogrande/zbitmap) | LZBITMAP, the compression of most files of iOS 27's OS volumes (decmpfs kinds 13 and 14). A young crate: its output is checked against apfs-fuse on every compressed file of the two images |
| `apk-info-zip` 1.1 (Apache-2.0, github.com/delvinru/apk-info) | `app`: an APK's signing certificates (schemes v1 to v3.x) |
| `sha2` (`asm`), `rayon`, `memmap2`, `libc`, `anyhow` | hashing (the hash lists are `sha2` and `std::io::Write`), parallel work, file maps, `copy_file_range` (`image`, `sym`, `log`) |

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
# every subcommand: [--volume N|NAME] [--block0] [--apfs own|fuse|auto] [-v]
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

1. `ioslog` on a real `data.img` from the phone (an image written by iOS's own APFS, after a session). The reader's
   paths for it are tested on stand-ins only: `mkapfs.py`'s image (checked against apfs-fuse) and, for images
   written by Apple's APFS, the three below.
2. `ioslog show` against the Mac's `out/session-logs/2026-11-12/ios3-log.csv` (made from `ios3.logarchive`). To
   run it there: `tests/ioslog/compare.sh out/session-logs/2026-11-12/ios3.logarchive`, and
   `oneios-tool ioslog show ios3.logarchive -f csv -o X && cmp X ios3-log.csv` (the same only if `ios3-log.csv` was
   made by `unifiedlog_iterator` of commit `3d5b27a`; version 0.7.0 formats some messages differently).
3. `checks` on a real session's log.

### The APFS reader

Our reader (`crates/oneios-image/src/apfs.rs`, `apfs/files.rs`) is the default everywhere; apfs-fuse is the
independent reader it is checked against, and since the third Linux round also an optional second backend.

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
  with `flate2`, written to `OUT.part` (blocks of 4096 zero bytes are skipped, the length is set at the end) and
  renamed to `OUT` only when as many bytes came as the phone's `stat -c %s` of that file says, before and after.
  `--ioslog` then runs `ioslog boots OUT` and `ioslog checks OUT`.

- `log` reads one named file of the stage and runs beside a session (it only reads). `--file`: `xnuhost`
  (`session/xnuhost.log`, the default), `switch`, `return`, `outcome`, `display`, `watchdog` (the names of
  `oneos/interface.md` section 3), or `logs/xnuhost-<epoch>.log` for an earlier session. `--list` prints each name
  with its size and time (`stat` of the named files, and the earlier sessions as the fork's read-only
  `oneios_root.sh logs` gives them): no folder of the phone is listed.
  - `--tail N`; `--follow` (`tail -n +K -F` on the file): when adb drops, the follow waits (1 to 5 s) and starts
    again after the last whole line it got, so no line is lost or printed twice; a log with fewer lines than were
    read is a new session's and is read from its start. Ctrl-C ends it with status 0.
  - `--save PATH`, `--save-auto` (`out/session-logs/DATE/live-HHMMSS.log`): **every** line read goes to the file,
    whatever the filters print.
  - Filters (a line is printed when it matches any of them; none: every line): `--grep REGEX` (repeatable),
    `--panic` (`panic(`, `xnuhost !!`, `xnuhost E`, `sheng-exc`, `sheng-exit`, `crashed:`, `stopping:`, `exit`),
    `--display` (`oneios-display`, `sheng-vsync`, `ONEIOS BACKLIGHT`, `ONEIOS ALS`, `oneios-swap`), `--gpu`
    (`oneios-gpu*`, `sheng-gpu`, `oneios-adreno`), `--wifi` (`ONEIOS WIFI`, `oneios-wifi`, `sheng-wifi`,
    `oneios-net`), `--hid` (`oneios-hid`, `ONEIOS HAPTIC`, `oneios-haptic`, `ONEIOS VIBRATOR`, `oneios-actuator`),
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
tools/oneios-tool/tests/phone/run.sh          # 64 checks against the stand-in phone; ALL OK
tools/oneios-tool/tests/cycle/run.sh          # phone log, status --watch and cycle: 90 checks; ALL OK
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

## cycle

`oneios-tool cycle [options]`: one test cycle on the phone, eight steps in this order: `build`, `tests`, `stage`,
`switch`, `watch`, `hold`, `return`, `collect`. `docs/building.md` section 6 has the table of what each does.
**Not run on the phone, and not with the real build scripts**: tested with a stand-in checkout whose
`kernel/build-hosted.sh`, `kernel/prelink/build.sh`, `stubs-check.py`, `runtime/package.sh` and
`runtime/host-tests.sh` are stand-ins, and the stand-in phone.

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
  `watch` ran).
- `summary.md` is written by `collect` also for a cycle that failed: the reason, each step's time and result, the
  markers, the panics, the way back.

Result of `tests/cycle/run.sh`, Linux x86_64, 2026-11-12: 90 checks, all ok, three runs. Among them: a whole
cycle (the stand-in kernel built with `XNU_FILESET_REFERENCE=1`, the collection with the reference, the stubs
check, the runtime package unpacked, the host tests, only the differing files staged, the fork's own
`begin`/`receive`/`finish` and `status` under its test root, switch, the nine markers, the hold, the restore
line, the folder); a second cycle that builds and stages nothing; a newer patch that rebuilds; a panic; a boot
that never gets ready; Ctrl-C in the hold with and without `--stay`; the lock; the three build failures; the
three refused switches. What it cannot show: the real builds' times and outputs, the real switch and return,
adb while One UI's framework stops.

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

`oneios-tool kernel`: the kernel build's own steps (crate `crates/oneios-kernel`). Each was a Python script; each
module has its script's name and description, takes what the script took and writes the same files.

| Command | Was (Python) | Module | State |
|---|---|---|---|
| `kernel gen board` | `kernel/device/scripts/build-oneios-board.py` | `build_oneios_board.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen storage` | `kernel/patches/oneios-hosted/storage/build-oneios-hosted-storage.py` | `build_oneios_hosted_storage.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen aes` | `.../aes/build-oneios-aes.py` | `build_oneios_aes.rs`, `class_record.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen keystore` | `.../keystore/build-oneios-keystore.py` | `build_oneios_keystore.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen net` | `.../net/build-oneios-net.py` | `build_oneios_net.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen hid` | `.../hid/build-oneios-hid.py` | `build_oneios_hid.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen audio` | `.../audio/build-oneios-audio.py` | `build_oneios_audio.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen gpu` | `.../gpu/build-oneios-gpu.py` | `build_oneios_gpu.rs`, `class_record.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel gen display` | `.../display/build-oneios-display.py` | `build_oneios_display.rs` | compared; Python is the default, `KERNELGEN_IMPL=rust` runs the command |
| `kernel build-record finish --host linux\|macos --work DIR --config NAME --out OUT [--repo CHECKOUT]`, `kernel build-record --self-test` | `kernel/build-record.py` | `build_record.rs` | compared; the builds still run the script |
| `kernel layout hid\|net [--kernelcache KC] [--prelink-report JSON] [--out FILE] [--repo CHECKOUT]` | `.../hid/layout.py`, `.../net/layout.py` | `hid_layout.rs`, `net_layout.rs`, `layout.rs` | compared; the builds still run the script |
| `kernel port-legacy-display [--kernelcache KC] [--fixture JSON] [--classes JSON] [--out FILE] [--repo CHECKOUT]` | `.../display/port-legacy-display.py` | `port_legacy_display.rs` | compared; the builds still run the script |
| `kernel compile-command RECORD SOURCE OBJECT [--repo CHECKOUT]` | new | `drivers_build.rs` | the check that this crate's copy of `drivers/build.py`'s compile command is the script's |

Shared: `gen.rs` (where a generator runs, Python's file and process calls as the scripts used them, the steps every
script repeated), `drivers_build.rs` (`drivers/build.py`'s `record` and `command`), `macho_object.rs` (a compiled
object's symbols and the signed entries of a vtable), `staged/` (the edit lists of the staged sources).

**`kernel gen NAME`** builds one board archive. UniXNU's driver runs it between the two kernel passes, in the build
tree, with `UNIXNU_DEVICE` set (and `ONEIOS_REPO`, which `kernel/build-hosted.sh` exports): an entry of
`board.toml`'s `[kernel] archives` that is not a `.py` or `.sh` script is run as a command, so the entries read
`"oneios-tool kernel gen aes"` and the build scripts put the binary on `PATH` under that name
(`$KERNEL_OUT/bin/oneios-tool`, a link to `ONEIOS_TOOL`; built first when missing or stale,
`scripts/lib/oneios-tool.sh`). The step's log is `logs/archive-NN-oneios-tool.log` (was
`archive-NN-build-oneios-NAME.log`). A failure ends as the scripts' did: the message on stderr, status 1.

What still runs Python, and why:
- `drivers/build.py` (not one of the retired scripts: it builds `drivers/` on its own, `--check` included).
  `kernel gen board` runs it as the script did, and the generators carry a copy of its two functions (`record`,
  `command`) in `drivers_build.rs`. `tests/kernelgen/compare-python.sh ... compile-command` and the unit test
  compare the copy with the script; a change of the script's rule must be made in both.
- Other projects' own scripts, run as they are: UniXNU's `scripts/prepare-storage-headers.py` (`gen storage`; the
  script imported it and called `prepare`, the command runs it as a program and reads the `headers.json` it writes,
  the same report) and Droid-VM's `build-droidvm-gpu.py`, cut before its accelerator step (`gen gpu`).
- `kernel/prelink/classes.py` (`kernel layout`: the class reader, as the two scripts ran it).
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

`tests/kernelgen/compare-python.sh WORK ENVFILE [NAME...]` (both implementations are in the tree): in the tree of a finished hosted build, each generator runs twice in the same place, the Python
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
tree for both, the box loaded) with the Python generators and with the commands:

| | `xnu.macho` (24,728,008 bytes) | `reference.macho` | Time |
|---|---|---|---|
| Python generators (`ea06d46`) | `5da980a23d26cf14f30272b100533fbf8441c6222211db0f8c610ed41b24d771` | `815ca46e986a5661...` | 1253 s |
| `oneios-tool kernel gen` (`KERNELGEN_IMPL=rust`) | `effea0af3597279d01be685e93bc7a6b50a37d6713fe444754ad8e00737c21a6` | `a51d90d1f28c63ba...` | 1011 s |

**The two SHA-256 are not equal, and no two builds of this kernel are**: the file carries the time of its build.
`tests/kernelgen/kernel-diff.py A B` names the place of every differing byte. Of the 2,074 that differ between the
two kernels, 20 are the time in XNU's version string (`Thu Nov 12 23:53:46 IST 2026` and `Fri Nov 13 00:31:02 IST
2026`, twice in `__TEXT,__const`), 16 are the link's `LC_UUID`, and 2,038 are the values of the symbol table's
`N_OSO` entries (each object file's modification time); **nothing else differs**: every byte of code and data is the
same. The RELEASE reference, which has no board archive in it, differs between the two builds in the same three
ways (76 bytes), so this is the build, not the generators. And what the generators hand to the link is the same:
the ten archive folders of the build with the commands are, file for file, the ones of the Python build (261 files;
the one line of the two `objects.json` aside).

The Linux build in the main checkout (`out/kernel/current-hosted-pc`, `13ed0b70...`) is from an older source and
another tree, so it is no reference for this.

### The goldens

`tests/kernelgen/golden/` is the Python scripts' output, frozen for the time after they are deleted:

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
`UPDATE=1 tests/kernelgen/compare-golden.sh WORK` after a build, in the same commit, and say why.

### The Mac

The commands use nothing of Linux alone (`std`, the `object`, `which` and `wait-timeout` crates; links are made
with `std::os::unix`), and take the same tools the scripts took (`llvm-ar`, `llvm-nm`, `llvm-objcopy`,
`llvm-objdump` from `PATH`, the storage generator's `/opt/homebrew/opt/llvm/bin/llvm-objcopy` when there is none,
`xcrun` for the SDK's `TargetConditionals.h`). **Checked on Linux only**: the comparison, the kernel build with the
commands, `compare-golden.sh`, `cargo test`. Not run on a Mac: the build of this crate, `kernel/build-hosted.sh` with
the commands (Xcode 27.2), `kernel/build-macos.sh` with `kernel build-record`. `compare-python.sh` and `save-env.sh`
are Linux scripts (`/proc`, `bwrap`).
