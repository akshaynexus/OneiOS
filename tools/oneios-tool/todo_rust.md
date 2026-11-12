# oneios-tool: state and what is left (2026-11-12)

The owner stopped the Rust work on 2026-11-12 to continue it later. This file is the hand-over.
Branch: `rustport` (not merged to `main`). Agent: rustport, with two helpers (sym + log, image).

**Rule until further notice: Python stays the default and the only path the builds use.** No build script on
this branch calls the Rust tool. The switches (`PRELINK_IMPL`, `IMAGETOOL_IMPL`) go in only after a byte-identical
comparison of the whole step passes on the real inputs, and even then with `python` as the default until the
parent says otherwise.

## Summary

| Tool | Code | Same output as Python | Left |
|---|---|---|---|
| `sym` | done | yes: 18 of 18 address sets byte-identical to `dsc.py` | docs; index location for the main checkout |
| `log` | done (new tool, no Python original) | not applicable; 67 commands on 4 real logs end with status 0 | tables for more line kinds; docs |
| `image` | done for 8 subcommands | yes: 35 of 35 compared items; the two full builds were not compared to the end | finish the full-build comparison; apply the switch patch; docs |
| `prelink` | **not started** (stub only) | no | everything; plan below |
| `ioslog` | **not started** (the owner's later request) | not applicable | everything; request below |

Nobody ran the comparisons again after the helpers' runs. The numbers below are from the helpers' logs of
2026-11-12. The Mac had other jobs (load average 4 to 25), so the times are not clean.

## Build and unit tests

```sh
cargo build --release --manifest-path tools/oneios-tool/Cargo.toml      # binary: tools/oneios-tool/target/release/oneios-tool
cargo test  --release --manifest-path tools/oneios-tool/Cargo.toml      # unit tests, synthetic data only
```

- Rust 1.94.1. The whole workspace builds (checked at the last commit: 40 s clean).
- `sha2` has the `asm` feature in `oneios-image` (ARMv8 SHA-256 instructions). It needs a C compiler at build time.
- Crates in `Cargo.lock`: anyhow, memmap2, rayon, sha2 (+ sha2-asm), libc, toml (image profile), and their parts.
  `serde` and `serde_json` are declared in the workspace but no crate uses them: `oneios-common::pyjson` writes
  JSON text as Python's `json` module writes it (indent, separators, `ensure_ascii`, float `repr`, key order).
- Unit tests in the tree: `oneios-common` 3, `oneios-sym` 7, `oneios-log` 7, `oneios-image` 29. The sym + log helper
  reported its 14 as passing. The `pyjson` 3 passed. **The image helper was stopped before it reported: the result
  of its 29 unit tests and of `tests/image/parity.py` is not confirmed.** Run `cargo test` first.
- No Apple bytes are in the repository. The tests build their own stand-in caches, images and logs.
- `target/` and `out/` are git-ignored already (`.gitignore` needed no change).

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
2. `docs/building.md` "Rust tools" section (not written; text drafted below under "Docs").
3. Not tested: a cache other than 24A446's; Linux.

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
4. Docs.

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

Not ported, on purpose: `check-ipsw`, `fetch`, `record-plain`, `aea-info`, `im4p`, `pack`, and `verify` of a
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
`KEEP=1`. `FULL_BUILD=1` needs the switch patch applied first (below); without it both builds run the Python tool.

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

Left, in order:
1. Run `cargo test -p oneios-image` and `tests/image/parity.py` and record the result (not confirmed, see above).
2. Apply `tools/oneios-tool/patches/imagetool-impl-switch.patch` (`git apply` from the repository root). It adds
   `IMAGETOOL_IMPL=python|rust` (default `python`) and `ONEIOS_TOOL` to `scripts/image/build.sh` and
   `scripts/image/verify.sh`. It is kept as a patch because this branch's commits touch only `tools/oneios-tool/`.
3. Run `FULL_BUILD=1 tools/oneios-tool/tests/image/compare-python.sh` to the end, and once without `SKIP_SLOW`.
4. Only then land the switch, with `python` as the default.
5. `hashes` and `verify` are limited by reading 16 GiB once (the whole-file SHA-256 is sequential). `tree-copy`
   is limited by the file copies. More speed needs fewer passes over the images (for example hash while copying).
6. Docs: `docs/building.md` "Rust tools", `docs/image-format.md` if the tool is named there.
7. Not tested: Linux (apfs-fuse path of the comparison script).

Known differences from the Python tool, on purpose (listed in `crates/oneios-image/src/lib.rs`):
- Where Python ends with a traceback, the Rust tool prints one line `imagetool: ...` and exits 1.
- A message that quotes Python's TOML or JSON parser quotes the Rust parser instead.
- Names that are not UTF-8 are refused with a message; Python carries them as escapes.
- The usage text names `oneios-tool image`.

## 4. `prelink`: the kernel collection's pairing and build

**Not started.** `crates/oneios-prelink` is a stub that prints "not built yet" and exits 2. No `PRELINK_IMPL`
switch exists in `kernel/prelink/build.sh`.

What to port (sizes in lines of Python):
- XnuPrelink at pin `9b950f2` (`external/XnuPrelink`): `fileset_match.py` 1086, `fileset_emit.py` 804,
  `fileset_segmented.py` 378, `fileset_layout.py` 333, `kc_image.py` 229, `fileset_shape.py` 47,
  `native_prelink.py` 264, `emit_prelink.py` 381.
- `kernel/prelink/`: `emit.py` 216, `align.py` 313, `audit.py` 334, `stubs-check.py` 63, `lead/imports.py` 153;
  and the small steps `build.sh` runs: `native-input.py`, `merge-reviewed.py`, `roots.py`, `hosted.py`,
  `kclasses.py`, `structors.py`, `vtables.py`, `record.py`.

Times today (the parent's numbers): 70 s cold, 19 s warm; `match_images` 20 s, emit 8 s, align 4 s, audit 3 s.

Plan:
1. One Rust subcommand per Python step, with the same arguments and the same output file, so each step can be
   swapped and compared alone: `oneios-tool prelink match|align|kclasses|structors|vtables|emit|audit|stubs-check`.
2. Order by time: `match` (writes `match.map.json`), `emit` (`kernel.kc`, `kernel.kc.prelink.json`), `align`
   (`aligned-*.json`), `audit` (`kernel.kc.binds.json`), then the rest.
3. Output formats to keep byte for byte (`pyjson::dumps(value, indent, sort_keys=false)`):
   `fileset_match.py` `indent=1` with **no** final newline; `fileset_emit.py` report `indent=2` + newline;
   `align.py`, `audit.py --report`, `kclasses.py`, `structors.py`, `vtables.py`, `merge-reviewed.py`, `record.py`,
   `lead/imports.py` `indent=1` + newline; `audit.py --draft` `indent=0` + newline; `hosted.py`, `roots.py`,
   `native-input.py` `indent=1` with no final newline (`json.dump`).
4. The emitter refuses a pairing from another matcher: `build.sh` keys `match.map.json` on the SHA-256 of
   `fileset_match.py`. A Rust matcher must write the same key fields or the cache key must name the Rust tool.
5. Parallelize per image (matching, fixup walks) with rayon; collect in the Python order before writing.
6. `PRELINK_IMPL=rust|python` in `kernel/prelink/build.sh`, default `python`.

Comparison inputs and the expected result:
- Kernel and reference: `out/parent-wt/out/kernel/current-hosted-prelink/xnu.macho` and `reference.macho` (the
  clean build from `c0b8618`), under the main checkout.
- Kernelcache: `out/kernel-inputs/24A446/kernelcache.macho` (78135296 bytes).
- Python outputs of these inputs: `out/parent-wt/out/kernel/24A446/` (`kernel.kc` 94240768 bytes, sha256
  `1e297354...`, `kernel.kc.hashes`, `kernel.kc.prelink.json`, `kernel.kc.binds.json`) and every step's file in
  its `work/` (`match.map.json`, `aligned-native.json`, `aligned-reference.json`, `kclasses.json`,
  `structors.json`, `vtables.json`, `select.json`, `hosted.json`, `reviewed.merged.json`).
- Run the Python build into a scratch `OUT` with `KERNEL=`, `KERNELCACHE=`, `REFERENCE=`, `XNUPRELINK=` set
  (a worktree has no `external/` and no `out/`), then compare each Rust step's file with `cmp`.
- The comparison script to write: `tools/oneios-tool/tests/prelink/compare-python.sh`.

## 5. `ioslog`: iOS's unified log from the phone's Data store (the owner's request, not started)

A subcommand `oneios-tool ioslog`, built on Mandiant's `macos-unifiedlogs` crate (crates.io, Apache-2.0; the
owner approved it; pin it in `Cargo.lock`). The parent did these steps by hand and found the touch fault with
them (`NN >= MAX_PATH_COUNT` in backboardd's TouchEvents). Everything is read-only for the phone.

1. `ioslog pull`: copy `/data/local/tmp/oneios/data/data.img` from the phone with
   `adb exec-out "su -c 'cat ...'"`, **only when no session runs** (`pidof xnuhost` is empty). Check the free
   disk first. Write the file sparsely in the tool itself: skip all-zero 4 MiB blocks with a seek (macOS
   `dd conv=sparse` did not stay sparse). A faster pull to try: read only the data extents on the phone
   (`SEEK_DATA` / `SEEK_HOLE`) or send the stream through gzip.
2. Attach it read-only:
   `hdiutil attach -readonly -noverify -nobrowse -imagekey diskimage-class=CRawDiskImage -nomount`, mount the
   Data volume with `mount -t apfs -o rdonly,nobrowse,noowners`, copy
   `db/diagnostics/{Persist,Special,Signpost,HighVolume,timesync}` and `db/uuidtext/*` into a `.logarchive`
   folder under `out/` (git-ignored), then detach.
3. Parse the archive with the crate (the parent's run: 2.87 M entries from a 285 MB archive) and filter:
   `--boot last|UUID`, `--since` / `--until` (guest UTC), `--proc`, `--subsystem`, `--category`, `--grep REGEX`;
   output as text, CSV or JSONL.
4. `ioslog checks`: print the known problems: `MAX_PATH_COUNT` drops, UISensor mode changes
   (`digitizerEnabled`, `pocketTouchesExpected`), backlight states, crashes and "Too many corpses", launch
   constraint violations, jetsam kills.

Notes: the guest's clock is about a day behind the host's, so its log dates are too. The parent's archive for
tests (read only): `<session scratchpad>/ios.logarchive`. Never touch the phone while a session runs, and never
write to it. Unit tests must not hold Apple's log files. Document it in `docs/building.md` "Rust tools".

## Docs (not written)

`CLAUDE.md` asks for the docs in the same commit. They are not written because the work stopped and this
branch's commits are limited to `tools/oneios-tool/`. When the work continues, add a "Rust tools" section to
`docs/building.md` with the commands above, and name the tools in `docs/image-format.md` and
`kernel/prelink/README.md` when their switches land. Also write the top-level
`tools/oneios-tool/tests/compare-python.sh` that runs the per-tool scripts (`tests/sym`, `tests/image`,
`tests/log/run.sh`, and `tests/prelink` when it exists).

## Notes for the next agent

- The source was not run through rustfmt (no `rustfmt.toml`); some lines are long. Clippy was clean for
  `oneios-sym` and `oneios-log`; it was not run on `oneios-image`.
- The agent's Write and Edit tools turn a backslash-u escape with four hex digits into the real character. Build
  such strings in code (see `esc()` in `crates/oneios-common/src/pyjson.rs`).
- Give each parallel agent its own `CARGO_TARGET_DIR` (`tools/oneios-tool/target/<name>`), or the builds wait on
  each other.
- The scratch outputs of the comparisons are in the worktree's `out/` (`rustport-sym/`, `rustport-log/`,
  `rustport-image/logs/`, `dsc-index/`): git-ignored, they go away with the worktree.
