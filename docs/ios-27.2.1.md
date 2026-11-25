# iOS 27.2.1 (24B156) beside 27.0.1 (24A446)

The owner's goal (2026-11-25): iOS 27.2.1 (24B156, iPhone18,2, the only build Apple signs since 2026-11-09) as a
build the user can select on the S24 Ultra and every phone the app offers iOS 27 on (not the A56), with 27.0.1
(24A446) the default and first choice until a phone boot of 27.2.1 passes. One runtime release serves both.

## State at the hand-off of 2026-11-25

Branch `ios272/base` (worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272` on the build laptop; `external`,
`inputs`, `out` are links to the main checkout's). Nothing is merged to main. Nothing booted.

Done:
- `profiles/ios-27.2.1-iPhone18,2.toml` (commit `9abcce7`): pins from Apple's ZIP directory and BuildManifest
  (`image check-ipsw`: ok, 2026-11-25), the SystemOS image's `sha256`/`plain_sha256` from a fetch. The OS image's
  are not pinned (not fetched: about 20 GiB of room at once; the laptop has about 12 GiB free).
- `inputs/24B156/` (git-ignored): every small member, and the SystemOS image decrypted
  (`141-10617-235.dmg`, 6.3 GB, the dyld shared cache). `out/kernel-inputs/24B156/kernelcache.macho`
  (78,512,128 bytes, SHA-256 `f6f31ac4...e9a5939`).
- **No newer XNU is needed**: 24A446's kernelcache is `xnu-13432.2.10~2`, 24B156's `xnu-13432.42.3~28`, the same
  branch; we already run xnu-13432 kexts on our xnu-12377 kernel. (The `xnu-13432-repin` branch's "blocked until
  Apple publishes xnu-13432" was wrong for this reason.) The same 301 fileset entries; 91 have another text size;
  the kernel's text is 32 KiB longer.
- **The class check, 8 of 11 the same**: `prelink classes` (one class per run: with all 11 in one run the first,
  IOAESAccelerator, fails "no static initializer with this class name found", a bug to look at) against
  `classes-24A446-v54.json`, comparing size, slot count and every slot's diversity. The same for IOAESAccelerator,
  IOCryptoAccelerator, IOHIDDevice, IOSurface, IOGPUDevice, IOAcceleratorES, IOGPUDeviceUserClient and
  AGXDeviceUserClient. Different: **IOMobileFramebufferLegacy** 0x1dd8 -> 0x1de0 bytes (379 slots both), so the
  display archive's field offsets (`legacy-display-24A446-v54.json` "display"/"request", 6720...7156) must be read
  again in 24B156's code before a 24B156 kernel; **IOGPU** 0x2e8/216 slots -> 0x2f0/217 (nothing of ours subclasses
  it; IOAcceleratorES, which NativeAdreno subclasses, is the same); **AGXAccelerator** 0xcdf8/474 -> 0xce00/475 (left
  out of the collection). The 24B156 records are in the scratchpad only (regenerate with the command below).
- **A 24B156 kernel is a second kernel build**: the board archives (hid, net, display, aes, gpu) compile 24A446's
  original addresses into `xnu.macho` (`<stub>.original` N_ABS, metaclass and vtable constants in
  `*-bindings.S`), so one `xnu.macho` cannot pair with both kernelcaches. The runtime has to carry two kernels
  (`runtime.json` `"kernels"`; the contract given to the app helper is below).
- `kernel/unixnu/profiles/os/ios-27.2.1-24B156/` (a copy of 24A446's with the IPSW and kernelcache pin). Not used
  by any build yet.
- `oneios-tool prelink port` (`tools/oneios-tool/crates/oneios-prelink/src/port.rs`, uncommitted until this note;
  **compiles, never run**): the cross-build matcher that carries the address-keyed records (pairs, stubs, the three
  reviewed files, the layouts) from 24A446 to 24B156 and reads every check again from 24B156's bytes. Rules in the
  file's header (shape, walk, slot, order; inside-function addresses by the same offset or the shapes' LCS).

Helpers started 2026-11-25 (T3 delegate tasks, each on its own branch; they report to the agent that started them):
- app: fork akshaynexus/OneOS, worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272-app`, branch
  `ios272/app`: Make iOS image offers both builds (sizes 10.59 / 10.91 GiB, room 30 / 32 GiB), Update the kernel
  pairs the staged set's build's kb, names follow the staged set.
  **Stopped at the hand-off**, just before its own commit: saved as `38dc4f1` on the fork's `ios272/app` (pushed,
  not reviewed). Its last report: `./gradlew test` 750 tests, 0 failures (80 skipped), the app compiles. Not merged
  into `oneios`; the OneiOS submodule is not bumped.
- userland: worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272-user`, branch `ios272/userland`: per-build
  userland tables (`scale_fixups`, the Metal plug-in's staged edits, the camera plug-in, both targets in the
  package, make-image's target pick), from 24B156's SystemOS image.
  **Stopped at the hand-off**: its uncommitted edits are saved as `430ebe3` on `ios272/userland` (pushed; not
  reviewed, not tested): `userland/metal/hooks/iPhone18,2-24B156.json`, scale fixups, the package for several
  plug-in targets, `oneios-sym` disassembly (`dis.rs`), tweaks. It was next making `Tweak::for_image` pick among
  the runtime's plug-in targets. Run its host tests before using any of it.

The contract both were given: `runtime.json` keeps `"kernel"` (24A446) and adds `"kernels"`, an array of kernel
objects with `"build"` (24B156's at `kernel/24B156/xnu.macho`, profile `ios-27.2.1-24B156`); the app's
`assets/kb/` stays 24A446 and `assets/kb-24B156/` is added (format 2), staged at `<stage>/kb-24B156`; make-image
gets `profile`, `xnu` and `kb` of the chosen build; `serve prelink` takes the staged set's build's kb and kernel.

Left (in order):
1. Run the matcher and look at its counts:
   `tools/oneios-tool/target/release/oneios-tool prelink port --from out/kernel-inputs/24A446/kernelcache.macho
   --to out/kernel-inputs/24B156/kernelcache.macho --map out/kernel/port-24A446-24B156.json --report
   out/kernel/port-24A446-24B156.report.json --record kernel/prelink/pairs-24A446.json=kernel/prelink/pairs-24B156.json
   --record kernel/prelink/stubs-24A446.json=kernel/prelink/stubs-24B156.json --record
   kernel/prelink/reviewed-24A446-v54.json=kernel/prelink/reviewed-24B156-v54.json --record
   kernel/prelink/lead/reviewed-lead-24A446-v54.json=kernel/prelink/lead/reviewed-lead-24B156-v54.json --record
   kernel/prelink/user/reviewed-user-24A446-v54.json=kernel/prelink/user/reviewed-user-24B156-v54.json`
   (check `early_security_init` lands on `0xfffffe000927b430`, the wrapper derived by hand on 2026-11-20).
2. The layouts per build: `prelink classes` / `kernel layout hid|net` / the aes and gpu records against 24B156
   (one class per run; `--prelink-report out/kernel/24A446/kernel.kc.prelink.json` only gives the slide
   0x1f000000000, to be checked again on 24B156's kernel.kc), the display record by `prelink port` plus reading the
   fields again (the 8 bytes moved them); the generators (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_*.rs`)
   read `<Class>-<build>-v54.json` by the build of the OS profile instead of the fixed 24A446 names.
3. `kernel/lib/stage.sh`: `OS_PROFILE=${ONEIOS_OS_PROFILE:-ios-27.0.1-24A446}`, and build-hosted.sh writes it into
   the device copy's `board.toml`; then `HOSTED_OUT=ios272 ONEIOS_OS_PROFILE=ios-27.2.1-24B156 ONEIOS_CRYPTO=original
   kernel/build-hosted.sh` in tmux; then `kernel/prelink/build.sh` with `BUILD=24B156` and the ported records: audit
   0 failures, `prelink stubs-check` 0 bad; review every record the port marked.
4. kb for 24B156 (the fork's `assets/kb-24B156/`), `runtime.json` `"kernels"` (runtime/package.sh), serve's
   make-image default kernel and kb by the profile's build; the second release kernel (`kernel/release-kernel.txt`,
   docs/releases.md); review and merge the two helpers' branches.
5. The Mac's phone boot of 27.2.1 (make the image on the phone: this laptop has no room for the OS image).

No tmux job of this work is running (`tmux ls | grep ios272`: none). Logs: `logs/ios272-*.log` in the worktree.

## After the hand-off (2026-11-25, the implementation sub-agent)

Left 1 and 2 done, on `ios272/base` (kernel/prelink/README.md "Another build" has the commands and counts):
- `prelink port` run: 211,361 functions paired; `early_security_init` -> `0xfffffe000927b430` (as derived by hand);
  pairs 2,703 kept / 9 left out, stubs 100 / 1, reviewed 650 / 2, lead 308 / 0, user 4 / 0, the display record 397 / 0.
  Two port bugs fixed: `linked` addresses (0xfffffff0...) were not carried, and a record whose `slots` are objects
  (the hid layout) stopped the run.
- The class records read again from 24B156 (`classes-24B156-v54.json`, the aes, gpu, hid and net layouts): the same
  as 24A446's but for addresses; the port of the same four records agrees with them address for address.
- The display record: `instance_size` 0x1dd8 -> 0x1de0 (the port had kept 24A446's); `kernel gen display` now
  checks the record against the class record. Every field offset we use is the same.
- The kernel build picks the build: `ONEIOS_OS_PROFILE=ios-27.2.1-24B156 kernel/build-hosted.sh` (written into the
  device copy's `board.toml`; the generators read `<Class>-<build>-v54.json` from it); `BUILD=24B156
  kernel/prelink/build.sh`.

Left 3 and 4 done (2026-11-25, on `ios272/userland`, which has `ios272/base` merged and is the branch to merge):
- **The 24B156 kernel**: `HOSTED_OUT=ios272 ONEIOS_OS_PROFILE=ios-27.2.1-24B156 ONEIOS_CRYPTO=original
  kernel/build-hosted.sh` (457 s): `xnu-12377.121.6-ios-27.2.1-24B156-23df94f24c01`, 24,718,272 bytes, SHA-256
  `3439037c...a9f0`, releasable; every board archive says "bound at its 24B156 iPhone18,2 addresses", the display's
  parent size 0x1de0. The same kernel series as main's pinned 24A446 kernel `5966e71ce2aa`.
- **Its collection**: `BUILD=24B156 kernel/prelink/build.sh` -> `09df2833...` (96,698,368 bytes): audit 0 failures,
  `stubs-check` 666 good, 0 bad, after the 10 left-outs were settled (kernel/prelink/README.md "Another build").
- **The app's pairing files**: `oneios-tool app kb 24B156` wrote the fork's `assets/kb-24B156/`; `prelink build`
  from them alone gives the same `09df2833` (the phone's Update the kernel and Make iOS image path).
- **The runtime carries both**: `runtime/package.sh` `KERNEL_MORE_DIRS` (`kernel/24B156/`, runtime.json
  `"kernels"`) and `METAL_MORE_DIRS` (`lib/guest/24B156/`, `"metals"`, `"cameras"`); `scripts/release/release.sh`
  takes each extra line of `kernel/release-kernel.txt` and builds the Metal plug-in for both targets. `serve
  make-image` picks the plug-in, the kernel and `kb-<build>/` of the image's build; the app names them too.
- **The Metal plug-in for 24B156**: `userland/metal/hooks/iPhone18,2-24B156.json` (the userland helper's, reviewed);
  built (`out/userland-u272/24B156`, the checks PASS); the capture plug-in has no per-build address and is shared.
- **The app** (fork `ios272/app`, merged with `oneios` `99a7b0e`, then `oneios` fast-forwarded to it: `1311a47`, the
  `app` pin here): Make iOS image offers "iOS 27.0.1 (24A446), tested" first and "iOS 27.2.1 (24B156), new: not booted
  yet" second; its `assets/kb-24B156/` from `oneios-tool app kb 24B156`; `./gradlew test --rerun-tasks`: 754 tests, 0
  failures (80 skipped).

Not done: no phone boot (the phone is not on this laptop); the 24B156 kernel is uploaded and pinned (2026-11-25: `kernel/publish.sh --hosted ios272 --upload`, SHA-256
`4d429519…`, the second line of `kernel/release-kernel.txt`; [releases.md](releases.md)); since then rebuilt from main `0f4ad03` as `1ff040fc5c9b` and released in **v0.1.29** with the app 0.5.93, which offers 27.2.1 (v0.1.28 had the kernel but the old app). Next: a phone boot of 27.2.1 (the steps above).
