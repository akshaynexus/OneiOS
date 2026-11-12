# oneios-tool: state and what is left (2026-11-12, after the Linux round)

Branch: `rustport`. Commands, crates and every comparison with its result: [README.md](README.md). This file is
the hand-over: what is done, what is open, and the notes behind the decisions.

Two rounds so far, both on 2026-11-12: the Mac round (sym, log, image written and compared there; stopped by the
owner) and the Linux round (the three built and compared again on a Linux x86_64 PC; `ioslog` written; `image im4p`
and the APFS file reader added; `prelink roots` and `hosted` ported).

**Rule until further notice: Python stays the default and the only path the builds use.** The switches that
exist (`IMAGETOOL_IMPL` in `scripts/image/build.sh` and `verify.sh`, default `python`; `kernel-inputs.sh` takes
`oneios-tool image im4p` only when there is no `lzfse` program) change no output. `kernel/prelink/build.sh` has no
switch.

## Summary

| Tool | Code | Same output as the old tool | Left |
|---|---|---|---|
| `sym` | done | yes: 18 of 18 address sets byte-identical to `dsc.py`, on the Mac (real logs) and on Linux (stand-in logs from the real cache) | the index's place for the main checkout; a comparison on real logs on Linux |
| `log` | done (new tool, no Python original) | not applicable; 67 commands on 4 real logs (Mac) and 33 on 2 stand-in logs (Linux) end with status 0 | tables for more line kinds |
| `image` | 9 subcommands, plus `apfs-ls` and `apfs-copy` | yes: 138 stand-in cases; on the real set 35 of 36 items and the two full builds (the 36th: no `kernel.kc` in the Linux set, README) | decmpfs (compressed files) in the APFS reader; `IMAGETOOL_IMPL=rust` as the default is the owner's call |
| `ioslog` | done | yes on 7 archives (CSV byte for byte, from an archive, an image and a mounted volume) | **a run on the phone's real Data store**; the Mac's `ios3-log.csv`; `checks` on a real log; `pull` |
| `prelink` | `roots`, `hosted` (2 of 12 steps) | yes for the two, byte for byte on the real kernelcache | **the other ten steps**, the pairing and the emitter first (section 4) |

## Build and unit tests

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml      # binary: tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml      # unit tests, synthetic data only
```

- Rust 1.88+ (macos-unifiedlogs); built with 1.98.0 on Linux x86_64 (clean build 1 min 20 s with `-j2`) and with
  1.94.1 on the Mac in the first round (before `ioslog`: check the Mac's toolchain is 1.88 or newer).
- `sha2` has the `asm` feature in `oneios-image`. It needs a C compiler at build time.
- Unit tests, Linux, 2026-11-12: 51 pass (`oneios-common` 3, `oneios-image` 29 + 1 `tests/apfs_files.rs`,
  `oneios-ioslog` 4, `oneios-log` 7, `oneios-sym` 7). `tests/image/parity.py`: 138 cases, all the same. This
  settles the first round's open point (the image helper's 29 tests and `parity.py` were never confirmed there).
- `tests/apfs_files.rs` makes its image with `tests/ioslog/mkapfs.py` (python3; skipped with a line on stderr
  where there is none).
- No Apple bytes are in the repository. `target/` and `out/` are git-ignored.
- `serde_json`'s `preserve_order` must stay off in the whole workspace (README "The rule for libraries").

## 0. First: libraries instead of hand-written code (owner, 2026-11-12)

The rule for the next round: **use a maintained library for every format and every generic job. Keep hand-written
code only where no library exists, or where ours is measured to be faster or more efficient** (say the numbers in
a comment). Model: iOSPlus10's `tools/op10tool` (/Users/akshaycm/Documents/GitRepos/iOSPlus10/tools/op10tool):
1,347 lines for 6 tools, one crate, one small file per subcommand, each a thin wrapper around a library
(`im4p.rs` is 30 lines on simple_asn1 + lzfse_rust; `ipsw.rs` 164 on zip + ureq; `img2simg.rs` 172 on
android-sparse), clap derive, and `tests/compare-python.sh` with the Python versions as references. This branch is
9,567 lines for 3 tools and uses 6 crates; serde and serde_json are declared and unused.

Why it grew: the brief asked for byte-identical output to Python, which forced Python's `json.dumps`, `repr`,
`shlex`/`urllib` quoting and `tarfile` bytes to be rewritten by hand. From now on: **compare by meaning, not by
bytes,** wherever a file's bytes are not hashed or signed (parse both JSON files and compare the values; compare
messages by content). Keep byte-identical output only where a hash is checked: the image's `.hashes` files,
`kernel.kc`, and (until Rust is the default) the seed.

Review of this branch (Opus, 2026-11-12), in order. Done in the Linux round: 1 (`ioslog`, with our APFS reader
instead of a mount, and clap, csv and regex), and of 9 the IM4P part (`image im4p` on simple_asn1 + lzfse_rust).
Done in the second Linux round: 3 (clap for every tool, one layout in `crates/oneios-tool/src/main.rs`; README
"The command line"). Not done: 2 and 4 to 8. They change code that is compared byte for byte with Python today; do them when Rust becomes
the default for a tool (then its comparison by meaning replaces the byte comparison):

| # | Area | Now | Use | Saves | Do |
|---|---|---|---|---|---|
| 1 | `ioslog` | not written | `macos-unifiedlogs` 0.8.0 (Apache-2.0, needs Rust 1.88+): all decoding. `LiveSystemProvider::with_roots(<mnt>/db/diagnostics, <mnt>/db/uuidtext)` reads the mounted Data volume directly (no `.logarchive` copy; keep a copy only as `--save`). Filter in the visit callback on cheap fields (boot, pid, process, subsystem, category, time) before `message()`, which formats lazily. | the whole decoder; our wrapper about 400-600 lines | first |
| 2 | JSON | `oneios-common/src/pyjson.rs` 552 lines | serde_json with `preserve_order` and derive; a small `Formatter` only where a JSON file's bytes are hashed | about 400 | **writer done** (third Linux round): `dumps` is serde_json with a `Formatter` for Python's layout. The parser and the value type stay (README "Hand-written, and why") |
| 3 | CLI | each tool parses argv by hand | clap 4 derive, one style for all subcommands | 150-300 | replace |
| 4 | xattrs, file calls | raw `libc` list/get/set xattr, futimens, mkfifo in `seed.rs` | `xattr` 1.x, std / `filetime` | 60-100 of unsafe code | replace |
| 5 | encoders | `oneios-image/src/py.rs` 349 | percent-encoding, base64; drop Python `repr` and `OSError` texts unless a message must match | about 100+ | **base64 and percent-encoding done**; `repr` and the `OSError` texts stay while the comparison is byte for byte |
| 6 | pax tar | `oneios-image/src/tar.rs` 217 | `tar` 0.4 (`Builder`, `append_pax_extensions`) once Rust is the default (accept one new seed hash) | about 150 | **done with no new seed hash**: the block is `tar::Header`, field by field; four details stay ours because the crate's bytes differ from Python's (README "Hand-written, and why" lists the bytes). `Builder` is not used |
| 7 | plist, uuid | hand bits | `plist`, `uuid` | small | replace |
| 8 | dyld cache + Mach-O | `oneios-sym/src/cache.rs` 298 | `object`'s `read::macho::DyldCache` if it reads the `.symbols` local symbols (check first); today 18 of 18 sets match dsc.py | small | only if iOS changes the format |
| 9 | IPSW / IM4P (prelink inputs, fetch) | Python | `zip` + `ureq` for remote reads, `simple_asn1` + `lzfse_rust` for IM4P, as op10tool | - | for prelink and fetch |

Keep (no library, or ours is measured faster):
- `oneios-image/src/apfs.rs` (690): no maintained Rust crate reads iOS's sealed APFS volumes; it gives the real
  owners without root (the other way is a root mount).
- `oneios-image/src/hashes.rs` (382): one read pass feeding the whole-file SHA and the rayon chunk SHAs; the speed
  work (sha2 with `asm` and rayon are libraries already).
- `oneios-sym/src/index.rs` (605): the project's own on-disk index; the reason `sym` answers in milliseconds.
- `oneios-log`: xnuhost's own log format (cut and joined records); add `regex` only for `--grep`.

APFS decision (owner, 2026-11-12): **keep our own APFS reader** (`oneios-image/src/apfs.rs`); no library replaces it.
Checked on 2026-11-12 (source read, not built):
- `apfs` 0.4.1 (MIT, github.com/Dil4rd/dpp, one author, ~8.9k downloads, ~3.3k lines, no `unsafe`): reads uid,
  gid, mode, xattrs and decmpfs over `Read + Seek`, but **no sealed volumes** (its B-tree knows only the fixed-KV
  flag, not the hashed index nodes of iOS's OS/SystemOS images) and **the first volume only**. Usable for an
  unsealed one-volume image (the Data store) on a host without a native APFS mount; test it first.
- `apfs-core` 0.3.0 (SecurityRonin/apfs-forensic): its README says "design skeleton ... parser bodies are stubs".
  Do not use.
- Every other crate named apfs is smaller (`exhume_apfs`, `newtua-apfs` a fork of apfs-core); `apfsds-*` is an
  unrelated network protocol. No mature Rust APFS crate exists.
- `apfs-fuse` (github.com/sgan81/apfs-fuse, C++, GPL-2.0): the standard tool on Linux (read-only, `-o ro,vol=N`;
  the old Linux docs and iOSPlus10's `root-linux.sh` used it); FreeBSD with fusefs; macOS only through macFUSE (a
  system extension; on Apple Silicon it needs lowered startup security: not worth it); no official Windows build.
  It does not check sealed-volume hashes (xnuhost verifies them). GPL: run it as a program, do not link it.
- iOSPlus10 used no APFS library: macOS's own APFS (`hdiutil attach` + mount, `sudo`, root-macos.sh) and apfs-fuse
  on Linux, plus blacktop's `ipsw` for caches and firmware.
So: the image build keeps our reader (sealed volumes, real owners without root, any host). **`ioslog` reads the
Data store with our reader too** (the owner, second round: no mount, the same on Linux and macOS): the reader
got file contents, any volume of the container and the newest checkpoint (`apfs/files.rs`), checked against
apfs-fuse on images written by Apple's APFS and on stand-ins (README). A mounted volume still works as a source.

Layout for the next round: fold the workspace into one crate with one module per subcommand where that keeps it
simple (op10tool's layout), unless a crate boundary pays (e.g. a shared `apfs` module used by two tools).

## 1. `sym`: the backtrace symbolizer (port of `scripts/debug/dsc.py`)

Done. Crate `crates/oneios-sym` (`lib.rs`, `cache.rs`, `index.rs`, `tests/synthetic.rs`).

```sh
export ONEIOS_DSC_DIR=<folder with dyld_shared_cache_arm64e, its subcaches and .symbols>
oneios-tool sym [--slide HEX] [--stdin | -] [--dsc DIR] [--index DIR] [--no-index] ADDR...
oneios-tool sym index [--force] [--dsc DIR] [--index DIR]
```

- The first call builds an index in `out/dsc-index/<cache UUID>/` (`ONEIOS_DSC_INDEX` or `--index` for another
  folder). Four flat files, mapped and searched in place: `meta.bin` 0.6 MiB, `syms.bin` 173 MiB, `names.bin`
  700 MiB, `calls.bin` 195 MiB (one bit per 4-byte word: "this word is a call"). Total 1.07 GiB.
- Index build: 25 to 36 s in five runs, 62 s in one (the Mac was loaded). After that the cache is not needed:
  without `ONEIOS_DSC_DIR` the tool uses the index built last (`out/dsc-index/latest`).
- Library API for other tools: `open_index`, `Index::find_slide`, `Index::name`.

Comparison (byte for byte on stdout, and the exit status):

```sh
ONEIOS_DSC_DIR=<cache folder> ONEIOS_LOG_DIR=<folder with the *.log files> \
  tools/oneios-tool/tests/sym/compare-python.sh
```

| Case | Same as `dsc.py` | `dsc.py` | `oneios-tool sym` |
|---|---|---|---|
| a1-a4: whole `oneios-ubt` backtraces (12-14 addresses), no `--slide` | yes | 0.30-3.16 s | 0.004-0.009 s |
| a5, a6: `sheng-ubt` crash backtraces (6 addresses), no `--slide` | yes | 0.44-0.71 s | 0.004-0.005 s |
| b: the same six sets with `--slide` | yes | 0.03-0.29 s | 0.003-0.004 s |
| c1, c2: addresses outside the cache | yes | 0.03 s | 0.003-0.006 s |
| d1, d2: 2 addresses and 1 address ("NOT reliable") | yes | 0.06-0.26 s | 0.003-0.013 s |
| e1: 3479 addresses with `--slide` | yes | 1.87 s | 0.100 s |
| e2: the same 3479 addresses, no `--slide` | yes | 52.9 s | 0.167 s |

Left, in order:
1. Decide where the index lives for the main checkout. The built one is in the worktree
   (`out/dsc-index/77bc8c6393da3e9bbb9c0be2af0c0e17/`, 1.0 GiB) and goes away with the worktree. Move it to the
   main checkout's `out/dsc-index/` (and its `latest`), or let the first call build it again.
2. Not tested: a cache other than 24A446's.

Linux round (2026-11-12): built and compared on Linux x86_64. 18 of 18 sets byte-identical to `dsc.py` (README
"sym" has the table). The box has no log from the phone: the sets come from stand-in logs that
`tests/sym/make-log.py` writes from the real cache (real return addresses, random frames). The cache was read
through apfs-fuse (`.symbols` is kept compressed in the image and decompressed on every read): index build 26.4 s,
`dsc.py` 4 to 649 s per set, `sym` 0.002 to 0.85 s, and 14.4 s for 4000 addresses with `--slide` (0.1 s on the Mac:
with the cache at hand `sym` reads it, and here every read goes through FUSE; not looked into further).

Known faults and limits (all are `dsc.py`'s behaviour, ported as it is):
- With a short backtrace several slides tie and the lowest wins. For the 6-frame locationd crash of
  `combo1` and `prev`, both tools print slide `0xdefc000` (PhotosUICore names); the real slide is `0x135f4000`.
  Both print "NOT reliable". `log stacks` settles this tie another way (below).
- Only `__TEXT` is searched; there is no symbol end check (a wrong slide gives names with large offsets).
- An address that is not a multiple of 4 is not in the bitmap: with the cache at hand the tool reads the cache
  for it; without the cache it does not count, and stderr says so.
- `sym` takes `--slide` in any position; `dsc.py` only as the first argument.

## 2. `log`: the xnuhost session log analyzer (new)

Done for the subcommands below. Crate `crates/oneios-log` (`parse.rs`, `stacks.rs`, `timeline.rs`, `diff.rs`,
`reports.rs`, `table.rs`, `tests/synthetic.rs`).

```sh
oneios-tool log stacks LOG [--proc NAME] [--thread 0xT] [--slide HEX] [--no-sym] [--summary] [--dsc DIR] [--index DIR]
oneios-tool log timeline LOG
oneios-tool log diff LOG_A LOG_B [--words N] [--all]
oneios-tool log gpu|cpu|display|perf LOG [--json | --csv]
```

- Joined lines are split where an `oneios-NAME: `, `ONEIOS NAME: ` or `sheng-NAME` record starts in the middle of
  a line. Lost first letters (`neios-ubt:`) are put back. A cut record's last address is dropped.
- `stacks`: every `oneios-ubt` sample per thread, named through the `sym` index, the slide found per boot, runs
  of equal stacks collapsed, and at the end the threads that never changed.
- `timeline`: xnuhost start, kernel start, BSD root, launchd, guest ready, display frames and swaps, gestures
  with the guest's next frame, crashes (`sheng-exc`), `stopping:` and `exit N`.
- `diff`: kinds of lines (source + first 6 words, numbers normalized) in one log and not the other, with counts
  and first line numbers; the boot arguments that differ.
- `gpu|cpu|display|perf`: one row per report line; cut fields show `-`.

Real-data test (no Python original to compare with):

```sh
ONEIOS_LOGS="combo1-xnuhost.log diag-xnuhost.log c2-xnuhost.log prev.log" tools/oneios-tool/tests/log/run.sh
```

Result of the helper's run: 67 commands on the four logs, all status 0, no panic, two runs equal, JSON output
loads in Python. 4 to 9 ms per command; `stacks` on `diag` and `c2` 0.14 to 0.23 s. Checked by hand: the slides
(`0xedb4000` for diag, `0x124d8000` for c2) equal the ones found by hand; all 1238 distinct frame lines of the c2
`stacks` output equal `dsc.py`'s lines; 10 table rows equal the raw lines.

Left, in order:
1. Tables for `oneios-gpu-sizes`, `-open`, `-text`, `-plugin`, `oneios-hid`, `ONEIOS WIFI` / `BT` / `AUDIO` /
   `BACKLIGHT`, `oneios-keystore`. Today they show only as kinds in `diff`. `oneios-iobusy` is only in `timeline`.
2. `timeline` does not name the lock screen: no log line says it. It lists the display's frame and swap lines.
3. `--json` / `--csv` for `stacks`, `timeline` and `diff`.

Linux round (2026-11-12): `tests/log/run.sh` on the two stand-in logs: 33 commands, all status 0, two runs equal.
Those logs hold thread lines and one crash only, so the tables and `timeline` read almost nothing there; `stacks`
found both slides and named every frame. A run on real session logs on Linux is open (none on the box).

Known faults and limits:
- The tie-break of `stacks` for short backtraces is an inference, not a fact from the sources: it takes the slide
  that puts the cache's first writable mapping inside the kernel's `triggered unnest of range A->B of DYLD shared
  region` range. It agrees with the three slides known from the real logs.
- Each real log holds one boot. Several boots in one log are tested only with synthetic text.
- The `W` swap records are a ring: between reports a swap count can be "N or more".
- A thread's "same stack" test compares addresses only.

## 3. `image`: the image tool (port of `scripts/image/imagetool.py`)

Done for `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`, `manifest`, and `verify` of a
set folder. Crate `crates/oneios-image` (`apfs.rs`, `hashes.rs`, `seed.rs`, `tar.rs`, `manifest.rs`, `profile.rs`,
`py.rs`, `fixtures.rs` for tests).

Since the Linux round also `im4p`, and `apfs-ls` and `apfs-copy` (`im4p.rs`, `apfs/files.rs`, `apfs_tools.rs`).

Not ported, on purpose: `check-ipsw`, `fetch`, `record-plain`, `aea-info`, `pack`, and `verify` of a
`.tar.zst`. They are network or one-off helpers. `oneios-tool image X` starts
`python3 -I scripts/image/imagetool.py X ...` for them (`IMAGETOOL_PY` names the script).

```sh
oneios-tool image hashes IMAGE OUT [--chunk BYTES]
oneios-tool image apfs-info IMAGE
oneios-tool image apfs-owners IMAGE [DIR]
oneios-tool image tree-copy SRC DST META [--owners-from IMAGE --owners-path DIR]
oneios-tool image seed-pack TREE META OUT --epoch N [--owners FILE]
oneios-tool image manifest PROFILE SETDIR [--inputs FILE] [--seed-info FILE] [--tweaks FILE]
oneios-tool image verify SET [--quick]
oneios-tool image profile PROFILE [--shell | --members]
```

Comparison on the real image set (stdout, stderr, exit status and every written file, with `cmp`):

```sh
SKIP_SLOW=1 tools/oneios-tool/tests/image/compare-python.sh     # without apfs-owners of the whole OS volume
tools/oneios-tool/tests/image/compare-python.sh                 # everything, about 6 minutes
FULL_BUILD=1 tools/oneios-tool/tests/image/compare-python.sh    # also scripts/image/build.sh --no-pack with each tool
tools/oneios-tool/tests/image/parity.py -v                      # both tools on generated stand-in files, no Apple files
```

Env: `ONEIOS_MAIN` (the checkout whose `out/` and `inputs/` hold the set), `TOOL` (the binary), `OS_ROOT`, `WORK`,
`KEEP=1`. `FULL_BUILD=1` builds the set twice, with `IMAGETOOL_IMPL=python` and `rust`, and compares every file.

Result of the helper's last run (`SKIP_SLOW=1 FULL_BUILD=1`, one run each, the Mac loaded):

| Item | Same | Python | Rust |
|---|---|---|---|
| `profile` (JSON, `--shell`, `--members`) | yes (3) | 0.04 s | 0.00-0.01 s |
| `hashes os.img` (10.4 GB) | yes | 9.10 s | 5.80 s |
| `hashes systemos.img` (6.2 GB) | yes | 6.46 s | 2.80 s |
| `hashes app.img`, `kernel.kc`, `app.img --chunk 4096` | yes (3) | 0.07-0.14 s | 0.01-0.05 s |
| `apfs-info` of os, systemos, app, and of a file that is not APFS | yes (4) | 0.06-0.10 s | 0.00 s |
| `apfs-owners os.img private/var` | yes | 3.95 s | 0.04 s |
| `apfs-owners` of systemos, app, and of a folder that is not there | yes (3) | 0.15-0.79 s | 0.01-0.04 s |
| `apfs-owners os.img` (the whole volume) | **not run** (`SKIP_SLOW=1`) | about 2 min | - |
| `tree-copy private/var` (meta and tree), and into a folder that is not empty | yes (2) | 10.13 s | 3.16 s |
| `seed-pack` (copied tree; changed tree with owner rules; a bad owner rule) | yes (3) | 1.41-1.54 s | 0.56-0.98 s |
| `manifest` (also against the set's own `manifest.json`; a set with a stray file) | yes (3) | 14.49 s | 5.98 s |
| `verify`, `verify --quick`, and 9 sets with one thing wrong each | yes (11) | 16.55 s | 5.96 s |
| the full `build.sh --no-pack` with each tool | **not finished** | 40.1 s | 22.1 s |

35 items compared, 35 the same. The two full builds both ended with status 0 and the same 22 file names and 34
output lines; the byte comparison of the two sets was stopped part way (the stop order came then).

Best of three runs (the helper's timing script, same loaded Mac):

| Command | Python best | Rust best | Faster |
|---|---|---|---|
| `hashes os.img` | 12.96 s | 6.92 s | 1.9x |
| `hashes systemos.img` | 7.94 s | 3.64 s | 2.2x |
| `apfs-owners os.img private/var` | 6.17 s | 0.08 s | 80x |
| `tree-copy private/var` | 10.35 s | 8.20 s | 1.3x |
| `seed-pack` | 2.72 s | 1.95 s | 1.4x |
| `manifest` | 21.48 s | 7.83 s | 2.7x |
| `verify` | 25.44 s | 8.81 s | 2.9x |
| the build's four (tree-copy, seed-pack, manifest, verify) | 59.99 s | 26.78 s | 2.2x |

Linux round (2026-11-12), which closes the first round's open points 1 to 4 and 6:
- `cargo test`: the 29 unit tests pass; `tests/image/parity.py`: 138 cases, all the same.
- The switch patch is applied (`IMAGETOOL_IMPL=python|rust`, default `python`, and `ONEIOS_TOOL`, in
  `scripts/image/build.sh` and `scripts/image/verify.sh`); the patch file is gone.
- `FULL_BUILD=1 BUILD_ARGS=--no-hook tests/image/compare-python.sh`, nothing skipped, on a set built on the Linux
  PC from the fetched IPSW (no kernel, no hook): 35 items the same, the whole-volume `apfs-owners` (Python 385 s,
  Rust 2.7 s) and the two full builds (every file of the two sets) among them. One item differs as documented:
  `apfs-info kernel.kc` on a set that has no `kernel.kc` (Python's traceback against one line). README "image"
  has the table.
- Added: `im4p` in Rust (simple_asn1 + lzfse_rust; the kernelcache decodes to the pinned SHA-256), and the
  commands only this tool has, `apfs-ls` and `apfs-copy` (the APFS file reader `apfs/files.rs`, which `ioslog`
  uses; checked against apfs-fuse, README "ioslog").

Left, in order:
1. A comparison on a set with a kernel and with the userland hook (this box had neither `kernel.kc` nor the built
   plug-ins): `FULL_BUILD=1 tools/oneios-tool/tests/image/compare-python.sh` where the set is built for the phone.
2. `im4p`: the Rust decoder and the `lzfse` program side by side on one host (the result is checked against the
   pinned hash, which the `lzfse` program made on the Mac).
3. `IMAGETOOL_IMPL=rust` as the default: the owner's decision.
4. `hashes` and `verify` are limited by reading 16 GiB once (the whole-file SHA-256 is sequential). `tree-copy`
   is limited by the file copies. More speed needs fewer passes over the images (for example hash while copying).
5. `seed-pack`: **measured and rewritten** (third Linux round; README "seed-pack: where the time goes"). The
   14.9 s against 12.0 s was the disk under other jobs. Warm, to tmpfs: Python 1.85 s, Rust 0.44 s (before:
   0.54 s), `cp` of the finished file 0.35 s; on this box's disk every tool is at `cp`'s 2.4-3.0 s. Open: the
   same on the Mac, and `ONEIOS_SEED_SINK=map` there (0.28 s on tmpfs here, not the default).
7. Not tested: Linux (apfs-fuse path of the comparison script).

Known differences from the Python tool, on purpose (listed in `crates/oneios-image/src/lib.rs`):
- Where Python ends with a traceback, the Rust tool prints one line `imagetool: ...` and exits 1.
- A message that quotes Python's TOML or JSON parser quotes the Rust parser instead.
- Names that are not UTF-8 are refused with a message; Python carries them as escapes.
- The usage text names `oneios-tool image`.

## 4. `prelink`: the kernel collection's pairing and build

**Two of twelve steps are ported.** `crates/oneios-prelink`: `roots.rs` (`roots.py`), `hosted.rs` (`hosted.py`).
Each takes the Python step's arguments and writes the same files; `tests/prelink/compare-python.sh` runs both on the
real kernelcache and compares stdout, the exit status and every file with `cmp`: all the same (README "prelink":
`select.json`, `hosted.json`, `kernelcache.hosted.macho`; Python 1.1 s and 5.3 s, Rust 0.05 s and 1.7 s).
`kernel/prelink/build.sh` has no `PRELINK_IMPL` switch and runs Python.

Why only two: the Linux PC had no hosted kernel (`xnu.macho`, `reference.macho`) during the round, and every other
step reads it. These two read the kernelcache alone, which `scripts/image/kernel-inputs.sh` now makes without the
`lzfse` program. The comparison inputs for the rest, once a kernel is built
(`HOSTED_OUT=NAME XNU_FILESET_REFERENCE=1 kernel/build-hosted.sh`): run the Python build into a scratch `OUT` with
`KERNEL=`, `KERNELCACHE=`, `REFERENCE=`, `XNUPRELINK=` set, keep its `work/`, and compare each Rust step's file.

What is left, in the order to do it (lines of Python; XnuPrelink at pin `9b950f2`, `external/XnuPrelink`):

| # | Step | Python | Reads | Notes |
|---|---|---|---|---|
| 1 | the image reader | `kc_image.py` 229, `fileset_shape.py` 47 | - | `Image` (segments, fileset entries, symbols, function starts, chained fixups), `scan`, `checked_shape`. Every later step uses it. Can be checked on the kernelcache alone (dump the fixups and each function's digest from both and compare) |
| 2 | `native-input` | `native-input.py` 108 | the kernel | writes `native.macho`, `native-input.json` |
| 3 | `merge-reviewed` | `merge-reviewed.py` 57 | the reviewed records, `llvm-nm` | |
| 4 | `match` | `fileset_match.py` 1086 | kernelcache, kernel, reference | 20 s of the build's 70 s. Writes `match.map.json` (`indent=1`, no final line end). Dictionaries and sets are iterated in many places: keep Python's insertion order (a `Vec` plus an index, as `hosted.rs`'s `Counter`), and check every `next(iter(set))` is on a set of one. `build.sh` keys the cached map on the SHA-256 of `fileset_match.py`; a Rust matcher needs its own key |
| 5 | `align`, `kclasses`, `structors`, `vtables` | 313, 139, 191, 112 | the map, the kernel | `align.py` uses `random` with a seed: port the generator or keep the step in Python |
| 6 | `emit` | `emit.py` 216 over `fileset_emit.py` 804, `fileset_layout.py` 333, `fileset_segmented.py` 378 | everything | 8 s. `kernel.kc` is hashed: byte for byte (its sha256 and `kernel.kc.prelink.json`) |
| 7 | `audit`, `record`, `stubs-check` | 334, 118, 63 | `kernel.kc` | |

JSON formats to keep byte for byte (`pyjson::dumps(value, indent, sort_keys=false)`): `fileset_match.py`
`indent=1` with no final newline; `fileset_emit.py` report `indent=2` + newline; `align.py`, `audit.py --report`,
`kclasses.py`, `structors.py`, `vtables.py`, `merge-reviewed.py`, `record.py`, `lead/imports.py` `indent=1` +
newline; `audit.py --draft` `indent=0` + newline; `hosted.py`, `roots.py`, `native-input.py` `indent=1` with no
final newline (`json.dump`). Times today (the parent's numbers on the Mac): 70 s cold, 19 s warm; `match_images`
20 s, emit 8 s, align 4 s, audit 3 s. Parallelize per image with rayon; collect in Python's order before writing.

`roots.rs` copies `roots.py`'s rule and its two lists. Until Python is retired a change to one must be made to the
other; the comparison shows a drift.

## 5. `ioslog`: iOS's unified log from the phone's Data store

**Written and compared; not run on the phone's Data store.** `crates/oneios-ioslog` (`lib.rs`: command line,
filters, outputs, boots; `source.rs`: the files of an image, an archive or a mounted volume as the decoder's
provider; `checks.rs`). README "ioslog" has the commands, the comparison and its table.

Decisions:
- macos-unifiedlogs 0.8.0 from GitHub at commit `3d5b27a` (pinned in `Cargo.lock`), not 0.7.0 from crates.io: 0.8.0
  has the provider that loads string files by UUID and the lazy `message()`, which the filters use. When 0.8.0 is
  on crates.io, name the version instead of the commit.
- The Data store is read with our APFS reader, at its newest checkpoint; no `.logarchive` copy is made unless
  `ioslog save` is asked for.
- CSV and JSON lines are `unifiedlog_iterator`'s so that the two can be compared with `cmp`.

Open, in order:
1. **Run it on a real `data.img`** (copy the store after a session, `docs/newtodo.md` "How to resume" step 2; then
   `oneios-tool ioslog boots data.img`). What a real store may show that the stand-ins do not: an APFS written by
   iOS's own kernel after many transactions (several levels of object map and file-system tree, snapshots, files
   with many extents). `image apfs-ls data.img db/diagnostics --volume Data --newest` and
   `tests/image/apfs-read.sh data.img 2` (Linux, against apfs-fuse, with `NEWEST=1`) check the reader alone.
2. On the Mac: `tests/ioslog/compare.sh out/session-logs/2026-11-12/ios3.logarchive`, and `ioslog show` of it
   against `ios3-log.csv` (the same only if that file was made by `unifiedlog_iterator` at commit `3d5b27a`).
3. `checks`: its patterns are the texts the notes quote; run it on a real log and correct them.
4. `ioslog pull`: copy `data.img` from the phone (`adb exec-out "su -c 'cat ...'"`, only when `pidof xnuhost` is
   empty, free disk checked, written sparsely in the tool: skip all-zero 4 MiB blocks with a seek). Not written:
   no phone on the Linux PC. Until then `docs/newtodo.md`'s adb line.
5. An `Info.plist` in the saved archive, if Apple's `log show --archive` is wanted on it.
6. The APFS reader does not read compressed files (decmpfs). Log files are not compressed; the OS volumes' files
   mostly are (2,910 of 3,347 in the SystemOS and AppOS images, the shared cache's `.symbols` among them), so
   `image apfs-copy` cannot replace a mount for `sym` yet. `flate2` and `lzfse_rust` would do it; LZVN needs a look.

## 6. `phone` and `app`: the adb workflow and the fork's app (third Linux round)

Written; **nothing of it ran on the real phone, and the app was not built or installed with it**. Crate
`crates/oneios-phone` (`adb.rs`: the one phone and the rules; `phone.rs`; `sparse.rs`; `app.rs`). README "phone" and
"app" have the commands and what the stand-in tests show (`tests/phone/run.sh` 64 checks, `tests/app/run.sh` 65).

Open, in order:
1. **The first run on the phone** (the Mac has it): `phone status`, then `phone sums` against
   `scripts/phone/stage.sh sums` (the same text), a `stage image` of one small file, `switch`, `return`,
   `pull-data --ioslog`. Things only the phone can show: the real `su`'s answers, whether `busybox setsid` lets
   adb go before the script prints `@@OK switch` (the tool then reads `switch.log`'s new `epoch` line), adb while
   One UI's framework stops and starts, the time `pull-data` takes on the 64 GiB store (6 min with the Python
   script). Then `scripts/phone/stage.sh` can go.
2. **`app` on the Mac**: `app doctor`, `app build`, `app install` (a real APK's v2/v3 signing block through
   `apk-info-zip`; compare `app version`'s certificate with `apksigner verify --print-certs`), `app install
   --release`, `app stage-scripts` against the app's own staging (the three SHA-256 values must be the ones the
   installed app pins).
3. `phone status`'s `session:` line is this tool's own test (stage.sh's: the pid file and /proc); the fork's
   `@@RESULT session` line beside it is the root script's (its control folder). They can differ for a moment
   around a switch.

## Docs

Written in the Linux round: [README.md](README.md) (the tool's own: commands, crates, comparisons), `docs/building.md`
section 10 "Rust tools" and section 1 (`kernel-inputs.sh` without `lzfse`), `docs/image-format.md` (the kernel
inputs' tools), `CLAUDE.md`'s table. Also written: `tests/compare-python.sh`, which runs every tool's comparison whose inputs are named. Not written:
`kernel/prelink/README.md` (no prelink switch yet).

## Notes for the next agent

- The source was not run through rustfmt (no `rustfmt.toml`); some lines are long. Clippy was clean for
  `oneios-sym` and `oneios-log`; it was not run on `oneios-image`.
- The agent's Write and Edit tools turn a backslash-u escape with four hex digits into the real character. Build
  such strings in code (see `esc()` in `crates/oneios-common/src/pyjson.rs`).
- Give each parallel agent its own `CARGO_TARGET_DIR` (`tools/oneios-tool/target/<name>`), or the builds wait on
  each other.
- The scratch outputs of the comparisons are in the worktree's `out/` (`rustport-sym/`, `rustport-log/`,
  `rustport-image/`, `rustport-ioslog/`, `rustport-prelink/`, `rustport-apfs*/`, `dsc-index/`): git-ignored, they
  go away with the worktree.
- Linux box notes (2026-11-12): no IPv6 route (`scripts/fetch/remotezip.py` was fixed in the main checkout to move
  on after 5 s); no `lzfse` program (`kernel-inputs.sh` uses `oneios-tool image im4p`); the dyld shared cache is
  read through apfs-fuse (`apfs-fuse -o ro inputs/24A446/043-70037-715.dmg out/dsc`), where `.symbols` is
  decompressed on every read, so `dsc.py` and the index build are several times slower than on the Mac.
- Mandiant's test archives (`tests/ioslog/compare.sh`'s inputs) are 2.4 GB unpacked: BUILDING.md of
  github.com/mandiant/macos-unifiedlogs has the download. They are other people's Mac logs: keep them in `out/`.
