# iOS 27.0.1 on an iPhone18,2 identity: the kernel profile

What an **iOS** profile needs compared with UniXNU's **iPadOS** 27.0.1 profile, which is the only 27.0.1 profile
UniXNU and Droid-VM publish. Marks: **[F]** read from the linked source, **[I]** inference, **[U]** unknown until
measured. Nothing here has booted.

Sources, at the pins of `kernel/pins.sh`:

- UniXNU upstream `1446f44`: [iPadOS 27.0.1 OS profile](https://github.com/UniXNU/UniXNU/blob/1446f449b6ac98b8a386752f520e402866ddbe99/profiles/os/ipados-27.0.1-24A446/profile.toml),
  [arm64e profile](https://github.com/UniXNU/UniXNU/tree/1446f449b6ac98b8a386752f520e402866ddbe99/profiles/arch/arm64e),
  [xnu-12377 patches](https://github.com/UniXNU/UniXNU/tree/1446f449b6ac98b8a386752f520e402866ddbe99/patches/xnu-12377),
  [MobileGestalt identity](https://github.com/UniXNU/UniXNU/blob/1446f449b6ac98b8a386752f520e402866ddbe99/docs/guide/mobilegestalt.md),
  [target IPSWs](https://github.com/UniXNU/UniXNU/blob/1446f449b6ac98b8a386752f520e402866ddbe99/docs/guide/requirements.md#target-ipsw),
  [kernel build](https://github.com/UniXNU/UniXNU/blob/1446f449b6ac98b8a386752f520e402866ddbe99/docs/guide/build-kernel.md),
  [boot chain](https://github.com/UniXNU/UniXNU/blob/1446f449b6ac98b8a386752f520e402866ddbe99/docs/concepts/boot-chain.md).
- Droid-VM's fork of UniXNU `62d0b11` (branch `wip/droidvm`), the build base:
  [tree](https://github.com/Droid-VM/UniXNU/tree/62d0b11773ec17561fb409ed6e4c5adfa29ff45b).
- Droid-VM's board `f9121cd`: [ipados27/board.toml](https://github.com/Droid-VM/UniXNU-droidvm/blob/f9121cdc629d64cbd148980cdeef1f069c7f0068/ipados27/board.toml),
  [its XNU patches](https://github.com/Droid-VM/UniXNU-droidvm/tree/f9121cdc629d64cbd148980cdeef1f069c7f0068/ipados27/patches/xnu-12377),
  [defconfig](https://github.com/Droid-VM/UniXNU-droidvm/blob/f9121cdc629d64cbd148980cdeef1f069c7f0068/ipados27/configs/droidvm_ipados27_defconfig).
- The IPSW pins: `profiles/ios-27.0.1-iPhone18,2.toml` (image-build), read from Apple's BuildManifest.

The profile itself: `kernel/unixnu/profiles/os/ios-27.0.1-24A446/` (OS layer) and `kernel/device/` (board layer).

## 1. Why the build base is Droid-VM's fork, not upstream

- [F] Upstream's iPadOS 27 OS series does not apply on upstream's arm64e series alone: `zzz-boot-progress.patch`
  expects a hunk (`XNU_SHENG_PMGR`) that upstream's SM8550 SoC series adds (tried 2026-11-10 at `1446f44`).
- [F] Droid-VM's board patches apply on its fork's series (all 24) and not on upstream's (10 of 24 fail).
- [F] That fork plus that board is what brought iPadOS 27.0.1 up to the lock and home screen on SM8650
  ([state table](https://github.com/Droid-VM/UniXNU-droidvm/blob/f9121cdc629d64cbd148980cdeef1f069c7f0068/README.md#state)).

So OneiOS builds from `external/Droid-VM-UniXNU`: its arm64e series (68 patches of `patches/xnu-12377`), the four
Darwin 26 patches, then the board series below. Upstream stays pinned as the reference.

## 2. Identity

| | iPadOS profile [F] | iOS profile |
|---|---|---|
| IPSW | `iPad17,1,iPad17,2,iPad17,3,iPad17,4_27.0.1_24A446_Restore.ipsw` | `iPhone18,2_27.0.1_24A446_Restore.ipsw` [F] |
| Product, board | iPad17,3, `j820ap` | iPhone18,2, `v54ap` [F] |
| Chip, board id | `33090` (0x8142), 12 | `33104` (0x8150), 14 (0x0E) [F] |
| ADT root `model` / `target-type` / `compatible` | `iPad17,3` / `J820` / `J820AP` | `iPhone18,2` / `V54` / `V54AP` |
| `/chosen` `board-id` | 12 | 14 |
| `/chosen` security values | `security-domain` 1, production status 0, security mode 1, `amfi-only-platform-code` 1 | the same [I] |
| `/product` `artwork-device-idiom` | `pad` | `phone` [F: MobileGestalt page] |
| Board option | `ipad_identity = true` in `[payload.adt]` | not set |

- [F] `DeviceClass` compares `hw.product` (the root `model`) by prefix; the identity and the userland image must be
  the same family (MobileGestalt page, "iPadOS and iOS"). An iOS root with a pad identity, or the reverse, is not a
  supported combination.
- [F] UniXNU's guidance is to leave the root `compatible` as the board's own, because an Apple board name can match
  original platform kexts. Its 27.0.1 profile sets `J820AP` all the same, and the iOS profile follows it with
  `V54AP`. [U] whether `V54AP` makes an original iPhone platform kext of the collection match and start a path with
  no hardware behind it; if it does, the ADT's `compatible` becomes the board's (`OneiOS,linuxhost`) and only
  `model`/`target-type` stay Apple's.
- The rest of `/product`, `/buttons` and the display properties are not kernel inputs: they come from
  `DeviceTree.v54ap.im4p` of the IPSW (role `devicetree` in the image profile) and are written by the booter into the
  ADT it builds. Phone values to take from there: `artwork-device-subtype`, `artwork-scale-factor` (3),
  `artwork-display-gamut`, `compatible-device-fallback`, `home-button-type` 2, `button-names` without `menu`.

## 3. Kernelcache pins

The kernel build stops at the native kernel (`--skip-prelink`), so it needs none of these. The **image build** needs
all of them, on the owner's machine, because the kernel collection (`kernel.kc`: our XNU prelinked with the original
kexts) is made there.

| Pin | iPadOS profile [F] | iOS: where it comes from |
|---|---|---|
| IPSW member | `kernelcache.release.j820` [I] | `kernelcache.release.v54`, 23,390,449 bytes, SHA-384 `8180aef0...f86198` of the IM4P as shipped [F: image profile] |
| `kernelcache_sha256` | `38e6d9fa...1780a` | `349c4692...0c57e3`: the decoded file (78,135,296 bytes) that `scripts/image/kernel-inputs.sh` writes (image-build). **Pinned** in the OS profile |
| `early_security_init` | `com.apple.driver.AppleMobileFileIntegrity=0xfffffe00092551ac` | the address of the AMFI wrapper that calls `_initializeAppleMobileFileIntegrity` and returns 0, in the iPhone kernelcache. A different address for certain. **To pin** (UniXNU's audit scripts, `scripts/audit-amfi-pmgr-contract.py` among them) |
| `abi/original-classes.json` | 1 file | the OSMetaClass list of the iPhone kernelcache. **To generate** |
| `fileset-reviewed.json` (Droid-VM board) | reviewed for 24A446 J820 | the reviewed fileset placement for the V54 collection. **To review** |
| `regression_policy` (upstream only) | `scripts/policies/ipados-27.0.1-24A446/prelink-regression.json` | allowed missing and stubbed calls. **To record** after the first prelink |
| `slide` | `-0x1000000` | the same [I] |
| `extra_args` | `--fileset-reference .../mach.release.bcm2837.unstripped.noctf --allow-missing` | the same: the RELEASE build of the same source the emitter matches symbols against |
| `roots` | 31 kext identifiers | the same 31 as a start. [U] which exist under the same identifiers in the iPhone collection; the prelink names any that do not |
| `[root]` volume UUIDs and image digests | OS and SystemOS of the iPad IPSW | of the iPhone images: written by the image build (it has the images) |

- [F] The iPad board adds two iPad-only roots, `AppleTopCaseDriverV2` and `AppleTopCaseHIDEventDriver` (Magic
  Trackpad). They are not in the iOS profile.
- [I] iPhone-only families to look at when the collection is first listed: the baseband and telephony kexts (must
  stay out of the roots: no baseband), the haptics, UWB, NFC and SE drivers (out), the iPhone's display pipe and
  touch (IOMobileGraphicsFamily and IOHIDFamily are already roots).
- The AMFI address, the class list and the reviewed fileset are **derived from Apple's kernelcache**. They are
  addresses and names, not Apple code; UniXNU and Droid-VM publish theirs. They may be committed once produced.

## 4. Display

| | iPadOS board [F] | iOS |
|---|---|---|
| Canvas | 2048x2732 pixels, 1024x1366 points | iPhone18,2's own: 1320x2868 pixels, 440x956 points at @3x [I: the published panel; confirm from the IPSW's device tree] |
| Scale | `offline-legacy-unit-scale=2`; QuartzCore runs any panel taller than 1080 pixels, and every panel of a virtual device, at @2x whatever `display-scale` says | @3x. [U] whether the same QuartzCore rule forces @2x for a phone idiom; the iPad fix-ups (`ffeb241`) are for @1x/@2x and do not carry over |
| Refresh | `droidvm-display-hz=120`, `ca-triple-buffer=1` | the S24 Ultra's panel is 1440x3120 at 1 to 120 Hz: 120 [I] |
| Orientation | landscape and portrait, pointer rendering fixes | portrait only at first; the iPad pointer work (`0c3fa5d`, `b7396ee`, `8d8f023`) is dropped |
| Backend | `GopFramebuffer` under the original `IOMobileFramebufferLegacy` | `HostDisplay` (booter) under the same original class; the boot arguments `offline-mobile-graphics-load=1`, `offline-mobile-graphics-client=1`, `offline-legacy-register=1`, `offline-legacy-phase=4`, `offline-legacy-pm=1`, `offline-legacy-validate=1`, `offline-legacy-transfer=1`, `offline-key-events=1` carry over; `offline-legacy-gop=1` and `offline-legacy-cpu-present=1` are the GOP backend's |

- The 1320x2868 canvas does not have the S24 Ultra's aspect (1440x3120 is 0.4615, the iPhone's 0.4603): scaled by
  1.0879 it is 1436x3120, two black columns each side. Scaling is the display helper's (booter, oneos).
- [U] Dynamic Island, always-on and the corner radius follow the identity's `/product` values; none is a kernel input.

## 5. Which iPadOS fixes carry over

### In the series (`kernel/device/patches/xnu-12377/series`)

| Patch (Droid-VM board unless noted) | What | Why it carries over |
|---|---|---|
| arm64e series, 68 patches (fork) | `BCM2837` machine config with 4 KiB pages, non-Apple CPU, native PAC keys (one key set), 4 KiB user ABI, code-signature hash cache, shared-region slide on 4 KiB, text fix-ups, source machine layer, union mounts, owner root | the base of every arm64e UniXNU kernel |
| `darwin26-syscalls`, `darwin26-kpi`, `darwin26-shared-region`, `zzz-boot-progress` (fork, OS layer) | the Darwin 26 system calls and KPIs on xnu-12377 | iOS and iPadOS 27.0.1 share the Darwin release [I]. [U] a system call the iPhone userland makes and the iPad one does not |
| `ipados27-bringup` | the board-independent bring-up of 24A446 | same userland family and build |
| `droidvm-native4k-once`, `droidvm-dyld-16k-peek-first`, `droidvm-text-fixup-bitmap` | 16 KiB userland on a 4 KiB kernel: signature preparation, dyld fix-up pages, validated-page bitmap | One UI's kernel has 4 KiB pages [U, expected]; the same problem |
| `droidvm-native-pac-user-jop`, `droidvm-native-pac-user-ib` | arm64 (not arm64e) apps run with the user keys off | App Store apps are arm64 |
| `droidvm-timebase-24mhz` | a 24 MHz timebase for userland over the host's counter | iOS userland assumes Apple's 24 MHz; the S24's counter is not 24 MHz [I] |
| `droidvm-pressure-pageable`, `droidvm-free-target-bytes` | memory pressure levels and free targets for a guest with GPU memory | same |
| `zzw-native-sandbox-grant-patterns`, `droidvm-gpu-grant-apps`, `droidvm-gpu-sandbox-first`, `droidvm-surface-gpu-wait` | sandbox grants for the GPU clients, IOSurface lock waits for GPU work | the Metal plugin and the kext families are the same [I] |
| `droidvm-osmetaclass-adopt-superclass` | a source class adopts an original superclass | needed by every host driver under an original family |
| `source-storage-start`, `native-board-launch-policy` | weak hooks: start source-built storage after the IOKit bootstrap; a launch policy before launchd | the hooks `HostBlock` and the userland's daemon policy use |
| `droidvm-amp` | `__ARM_AMP__`: processor sets per cluster type, the Edge scheduler, `hw.nperflevels` = 2 | placement is the host's, but XNU and userland need P and E clusters to report and prefer (`drivers/cpu/README.md`). Every cpu node needs `cluster-type` |
| `zzx-pmap-sync-flush` | write-combined pages written back before reuse | harmless under a host pmap, kept to stay on Droid-VM's tested text |
| `portable-kalloc-type-builtins` (xnu-linux-toolchain) | data-only fallbacks for `__builtin_xnu_type_*` | stock clang; compiled out under Apple clang |
| `oneios-*.patch` (ours) | section 6 | |

### Left out

| Patch | Why |
|---|---|
| `non-apple-vcpu-park`, `non-apple-vcpu-start-flush`, `non-apple-idle-wfi`, `non-apple-sigp-recover` | Gunyah vCPU behaviour (WFE, start-up caches, lost IPIs). xnuhost's vCPUs are Linux threads |
| `droidvm-sched-trace`, `droidvm-early-panic` | the rig's readers through guest RAM. xnuhost has the host hooks |
| Mi A3 (laurel_sprout) series of iOSPlus10 | makes iOS 26 run on ARMv8.0 cores without PAC (instruction emulation, shared-cache rewrite). The S24 Ultra has PAC; not ported (owner's decision, `docs/agents.md`) |
| iOSPlus10 `patches/xnu-12377/0001`-`0004` (NEGRONI machine config and drivers) | the OnePlus 10 Pro's SoC layer on darwin-xnu-build's tree. Same role as UniXNU's `BCM2837` layer, which the Apple kexts are prelinked against |

### iOSPlus10's PAC path (OnePlus 10 Pro, stage 2)

- [F] iOSPlus10 `patches/xnu-12377/0005-negroni-native-pac.patch`: an arm64e kernel (`ARCH_STRING=arm64e`), `HAS_APPLE_PAC`
  when compiled as arm64e, and **one global key set** written at EL1 (`APIA/APIB/APDA/APDB/APGA`), the same for
  the kernel and every process.
- [F] The fork's `native-pac-keys.patch` (arm64e series) is the same model, further along: one key set per boot from
  the counter, `ml_default_jop_pid` returns 1 so the shared-region and dyld pagers sign, `ml_enable_user_jop_key` and
  `ml_disable_user_jop_key` do nothing.
- So the series takes the fork's patch and not 0005: the two edit the same macro (`PAC_INIT_KEY_STATE`). What 0005
  adds on top is nothing; what it lacks is the pager and jop-pid part.
- Under xnuhost the key registers are not writable (EL0): the kernel's ten `msr AP*Key*_EL1` trap, and xnuhost
  ignores them. The keys are the Linux process's own, the same in every task process because all are forks of
  xnuhost (`docs/plan.md` 5.2).

## 6. What OneiOS adds

| Patch | What |
|---|---|
| `oneios-stock-clang-ptrauth.patch` | `__ptrauth(..., "authenticates-null-values")` is Apple clang's: with another compiler the same key and discriminator without the option. An ABI difference for null values of `ucred.cr_label` and `IOCircularDataQueue` against kexts built with Apple clang; none under Apple clang |
| `oneios-no-apple-iig.patch` | without Apple's `iig`: `IOUserClient::CreateActionKernelCompletion` answers unsupported, and the test dext (`TestDextRematch`) is not built, on both hosts |
| `oneios-board-start.patch` | `oneios_board_late_start()` from `bsd_init`: starts the board archives (`drivers/`, cpufreq); weak defaults for them, for `native_source_storage_start` and for the two `__llvm_profile_*_internal` calls of OSKext's PGO export |

Not written yet: the hosted machine layer (`LINUXHOST`, `runtime/xnuhost/include/xnuhost/hostcall.h`). `kernel/README.md`, "Not done".

## 7. Open, in the order they block

1. The kernelcache pins of section 3 that are still open: the AMFI address, the class list, the reviewed fileset.
2. Whether the 31 roots exist in the iPhone collection, and which iPhone-only kexts start without their hardware.
3. An iPhone-only system call or KPI missing from the Darwin 26 patches (first seen as a `nosys` line at launchd).
4. `compatible` = `V54AP` or the board's own.
5. The scale rule of QuartzCore for a phone idiom.
