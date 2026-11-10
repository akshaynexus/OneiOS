# iOS 27 userland for OneiOS

Owner: userland. Code: `userland/`. Date: 2026-11-11.

**State: run on the real root and booted on the PC; nothing has run on the phone.** `scripts/image/build.sh` with
the hook builds the image set of 24A446 from the real images, and the hosted kernel boots that set under
qemu-aarch64 to launchd and about 120 processes. In that boot launchd applies the daemon policy (section 10 has
the numbers and how to check a run). The guest helpers are not built (no iOS SDK here), so nothing of
`/private/var/oneios/bin` runs. The tests use a generated fake root. Marks:
[F] taken from a source that is named, [I] inferred, [U] unknown until a boot.

What this part does: it takes Apple's iOS 27.0.1 system for iPhone18,2 (24A446) as it is and prepares
everything around it so that launchd, backboardd and SpringBoard come up under OneiOS's kernel on the
Galaxy S24 Ultra: which daemons run, the preferences of the first boot, the iPhone identity and display the
booter must present, where our own programs and the Metal plugin go, and how they are trusted.

## 1. How to run it

The image build runs it (`scripts/image/build.sh` calls `userland/image-hook`, hook API 1 of
`docs/image-format.md` section 6). By hand:

```sh
python3 -I userland/tweak apply --root OS_TREE --data DATA_TREE --out OUT   # --layout sealed
python3 -I userland/tweak daemons                                           # the daemon policy as text
python3 -I userland/tweak cdhash FILE...                                    # CDHash of signed Mach-O files
python3 -I userland/tweak boot-check --data DATA_VOLUME [--root OS_TREE]    # after a boot: section 10
python3 -I userland/tests/test_roottweak.py && python3 -I userland/tests/test_helpers.py
```

(`userland/tweak` is the package's command; `python3 -I -m roottweak` does not find the package, because `-I`
leaves the current folder out of the module path.)

Python 3.11 or later, standard library only. Output in `OUT` (the hook puts it in the image set's
`overlay/userland/`):

| File | For | Content |
|---|---|---|
| `boot-contract.json` | booter, kernel | device tree identity, display, launch policy, mounts, kernel features (section 6) |
| `owner-trust.json` | kernel build | CDHashes of our own binaries for the owner trust cache |
| `tweaks.manifest.json` | image build | every path changed, with the owner and mode the guest needs |
| `report.txt`, `report.json` | people | one line per item: `applied`, `kept`, `not-found`, `off`, `contract` |

The build is not root, so owners are recorded, not set; the hook writes them to `$ONEIOS_OWNERS`.
A second run over the same trees changes nothing. A preference file that is not a property list stops the
run instead of being written over. A root that is not iOS 27.0.1 is refused (`--any-version`).

## 2. Layouts

| | `sealed` (default, what image-build ships) | `rw` |
|---|---|---|
| OS volume | Apple's image, unchanged, hash-checked. Nothing is written to it | a root we write ourselves |
| Model | UniXNU: verified root, original trust caches | iOSPlus10 and vphone: edited launchd cache |
| Tweaks land in | the Data seed (`/private/var`), `overlay/`, the boot contract | the root as well |
| Needs from the kernel | the launch policy and mounts of section 6 | `launchd_unsecure_cache=1` |

`rw` exists because it is the only way to change what a sealed root cannot (section 4, last column). It is
tested on the fake root and not used by the image build.

## 3. The tweaks

Sources: **UniXNU** = https://github.com/UniXNU/UniXNU, **droidvm** = https://github.com/Droid-VM/UniXNU-droidvm,
**ownervisor** = https://github.com/Droid-VM/ownervisor, **iOSPlus10** = https://github.com/akshaynexus/iOSPlus10
(with ios-sweet's tools it vendors), **vphone** = vphone-android (`android/vphone-native`, `android/create/installer`)
and vphone-cli 2.14.2's `Research/Guest` notes, which were measured on iOS 27.0/27.0.1 guests.

| Tweak | What it does | From |
|---|---|---|
| `version` | Reads the build from `SystemVersion.plist`; refuses another iOS version | - |
| `launchd-jobs` | Disables the jobs of the daemon policy (section 4): `db/com.apple.xpc.launchd/disabled.plist` = `{label: true}`, root:wheel 0644, with the empty `disabled.migrated` beside it. Labels the root does not have are reported, not written (the real root has all 142). launchd reads the file once per boot, after the boot task `finish-demo-restore`, and only when uid 0 owns it and neither group nor others can write it (read in `/sbin/launchd` of 24A446; section 10). `rw`: also takes them out of `System/Library/xpc/launchd.plist` | vphone-cli `GuestServiceProfile.swift`, `service_trimming.md`; droidvm `package/DroidVMLaunchPolicy.c`, `scripts/seed-data-disable-jobs.py`; UniXNU `scripts/check-launchd-watchdog-policy.py` (launchd honours the file only when root owns it) |
| DisableAllowed | iOS 27 marks some jobs `DisableAllowed = false` in `com.apple.jetsamproperties.V54.plist`; launchd runs them although disabled. `sealed`: the affected labels are named in the report and the contract. `rw`: the mark is removed | vphone-cli `service_trimming.md` "iOS 27 and DisableAllowed" (`patch-jetsam-disable-allowed`) |
| `launchd-crash-policy` | `rw` only: `_PanicOnCrash` renamed, `RecoverOnCrash` removed for backboardd, SpringBoard, configd, runningboardd, so a crash loop does not restart the guest during bring-up | iOSPlus10 `scripts/laurel/ios26-root-spec.py`, `docs/laurel_sprout/ios26-backboardd.md` |
| `host-services` | Places `/private/var/oneios`: `bin/` (helpers), `services.d/*.conf`, `enabled/`, `identity.expect`. `sealed`: the kernel starts `bin/oneios-init`. `rw`: a launchd job `com.oneios.init` | ownervisor (the manager outside launchd), droidvm `add-ownervisor-data.py`; job keys from iOSPlus10 `scripts/net/root-add-ssh.py` |
| `metal-plugin` | Places a built plugin bundle with `<Name>.clients` (who gets the device) and `<Name>.publish`. `sealed`: `/private/var/oneios/addons/Extensions`, shown over `/System/Library/Extensions` by a kernel bind mount. `rw`: in `/System/Library/Extensions` | UniXNU `scripts/add-gpu-plugin-data.py`, `patches/xnu-12377/zzy-native-gpu-addon-mount.patch`; droidvm `build-droidvm-data-seed.py` (client list); iOSPlus10 `scripts/laurel/root-add-metal-ios26.sh` |
| `app-cryptex` | `sealed`: nothing copied; the contract asks the kernel to mount the image set's `app.img` at `/private/preboot/Cryptexes/App`. With `--appos DIR` the files are carried on Data as droidvm does | droidvm `scripts/add-app-cryptex-data.py`, UniXNU `zzyb-native-cryptex-os-view.patch` |
| `staged-apps` | The removable system apps stay in `/private/var/staged_system_apps`; the kernel shows them in `/Applications`. `rw`: moved there | droidvm `ipados27-bringup.patch` (owner-staged-apps); iOSPlus10 `scripts/ios16/root-finish.sh` |
| `preferences` | Section 3.1 | see there |
| `data-layout` | `containers/{Bundle,Data/System,Shared/SystemGroup,Temp}`, `keybags` (0700, empty), `mobile/tmp`, `tmp` (1777), `run`; logd's stores `db/{diagnostics,uuidtext,diagnostics/timesync}` with `.migrated`, owner 272. Removes a MobileGestalt cache carried in the seed | UniXNU `scripts/make-data-seed.py`, `prepare-owner-data-fixture.py`; ios-sweet `mkiosroot.sh`, `sweet-root-tweaks.py`; iOSPlus10 notes (stale cache) |
| `keybag` | Off: `keybags/` stays empty, which is what UniXNU's key-store stand-in expects (keybagd then logs "No system keybag loaded"). `--plain-keybag` writes ios-sweet's plain `systembag.kb` for a kernel that reads one | UniXNU `scripts/probe-keybag-bypass.py`; ios-sweet `mkiosroot.sh` |
| `feature-flags` | `rw` only: ExtensionKit `enforce_xpc_cache_code_signing` off, or installd and lsd refuse every app extension once the extension cache's signature no longer holds | iOSPlus10 `scripts/ios16/root-tweaks-extra.py` |
| `identity` | Writes the device tree identity and the display into the contract; checks the root has jetsam properties for the target type | UniXNU `docs/guide/mobilegestalt.md`; droidvm `ipados27/scripts/make-droidvm-adt.py` |
| `kernel-features` | `sealed`: lists what the kernel must do, by UniXNU's switch names | droidvm `package/DroidVMBootArgs27.c`, `ipados27/board.toml` |

### 3.1 Preferences (`userland/config/prefs.toml`)

Written as files before the first boot (mobile's 501:501 0600). `--no-GROUP` leaves a group out.

| Group | Domain: keys | Why | From |
|---|---|---|---|
| setup | `com.apple.purplebuddy`: SetupDone, SetupFinishedAllSteps, SetupVersion 11, daemonizeNeedsToRun false; `.notbackedup`: the seven mini-buddy flags false; `com.apple.managedconfiguration.notbackedup` | Lock screen instead of Setup Assistant | UniXNU `scripts/seed-data-skip-setup.py`; vphone-cli `setup_assistant_skip.md` (the three keys alone give the home screen on 27.0) |
| migration | `com.apple.migration`: LastSystemVersion = build, DMUserDataDisposition, DMLastMigrationResults | No data migration at each boot | UniXNU; iOSPlus10 `ios26-root-spec.py` |
| watchdogs | `com.apple.springboard`: BKNoWatchdogs, FBLaunchWatchdogExceptions, FBLaunchWatchdogScale 4; `com.apple.frontboard`: the scale | Slow first launches are not killed | UniXNU; vphone `prefs.rs` (scale 4); iOSPlus10 (scale 3, and why not 20) |
| awake | `com.apple.springboard`: SBDisableAutoDim, SBDontDimOrLockOnAC, SBDisableIdleSleep, SBMinimumLockscreenIdleTime | The screen stays on while input may not work yet. `--no-awake` once it does | UniXNU `--no-auto-dim`; vphone-cli `lock_screen_idle_timer.md` (27.0: the lock screen goes dark after 6 s) |
| coreanimation | CAEnableDeepFramebuffer, CAEnableFramebufferCompression, CAEnableIOSurfaceCompression false in `com.apple.backboardd` and `.GlobalPreferences` of mobile and root and in the any-user file; CAEnableMedina false in `com.apple.coreanimation` | The render server otherwise picks a 10-bit compressed framebuffer the host cannot show; with Medina on, SpringBoard aborted on the camera-indicator layer | UniXNU `add-gpu-plugin-data.py`; vphone `prefs.rs`; droidvm `build-droidvm-data-seed.py` |
| visuals | `com.apple.Accessibility`: ReduceMotionEnabled, EnhancedBackgroundContrastEnabled | Only with `--lighter-visuals`, for CPU rendering | vphone `prefs.rs` |

Not ported, with the reason: droidvm's `BKForceMirroredOrientation=portrait` (for an iPad display that does not
rotate its scanout); iOSPlus10's jetsam floors, Bold Text, InterfaceNamer timeout, status-bar provider,
CA_HEARTBEAT_RATE, MediaToolbox recorder and activation data ark (each works around its own kernel's missing
piece, or edits a file of the OS volume; the first boot's log decides which return); the Mi A3 shared-cache
lowering and PAC stripping (the S24 Ultra has PAC: board note of 2026-11-10).

## 4. Daemons: keep, idle, replace, disable (`userland/config/daemons.toml`)

The file holds every label with its reason; `roottweak daemons` prints it. Summary:

| Class | Count | What | Where it was measured |
|---|---|---|---|
| keep | 55 | Never disabled: the boot chain (SpringBoard, backboardd, runningboardd, lsd, containermanagerd, cfprefsd, notifyd, configd, powerd, securityd, AMFI, keybagd, mobileactivationd, logd, cryptexd, ...), installs and debugging, and the jobs that make things worse when off: sleepd, CommCenter and its helpers, cloudd, nanoregistryd, siriactionsd, passd, financed, generativeexperiencesd, bluetoothd | vphone-cli `neverDisable`, iPhone guest, iOS 27.0; vphone-android `services.rs`; droidvm (bluetoothd) |
| idle | 11 | Run without their hardware and must rest: CommCenter (no baseband node), bluetoothd (virtual transport), cameracaptured, locationd, keybagd (key-store stand-in), biometrickitd, thermalmonitord, wifid, audiomxd | reasons per label in the file |
| replace | 5 | Disabled; the host does the job: watchdogd (no watchdog device; the switch has its dead-man timer), the four OTA jobs (updates are a new image set) | UniXNU `docs/guide/skip-setup.md`; droidvm |
| disable: base | 129 | Siri and intelligence, Find My, Wallet/NFC/SE, Watch, Home, Maps background, iCloud and iMessage sync, ads, telemetry, accessory firmware | vphone-cli `base27`: -29 % memory, -26 % idle CPU, -60 % idle writes, no feature lost |
| disable: extra, signin, hardware | 4 + 2 + 2 | momentsd, coreduetd, modelcatalogd, mstreamd; the sign-in prompts; aned (no Neural Engine), fairplayd.H2 (no FairPlayIOKit kext: its services hang lockdownd and the apps behind it) | vphone-android; droidvm `DroidVMBootArgs27.c`; iOSPlus10 |
| off by default | store 2, fewest 8, bringup-26 9 | Each costs a feature or is unverified on 27; `--group NAME` | vphone; iOSPlus10 iOS 26 |

142 labels are disabled by default. One open disagreement: droidvm disables **CommCenter** on the (Wi-Fi) iPad,
vphone keeps it on the iPhone because SpringBoard reconnects to it every second without it. It is kept. If
it crash-loops without a baseband, `--disable com.apple.CommCenter` (with its two helpers) is the fallback.

On a sealed root, a disabled job marked `DisableAllowed = false` may still run (launchd has the line
`Ignoring disabled state because DisableAllowed=false`). The iPhone's `V54` file marks the same five as
vphone's iPad guest of 24A446: analyticsd, OTATaskingAgent, diagnosticextensionsd, mobile.softwareupdated and
symptomsd-diag [F, the report of the real root]. In the PC boot of 2026-11-11 none of the five was started in
the 7 minutes after launchd came up, launchd logged no such line, and backboardd's lookup of
`com.apple.analyticsd` found no service; whether one of them starts later is [U].

## 5. iPadOS to iOS: what differs from UniXNU and droidvm

Everything they publish at 27.0.1 is iPad17,3 (J820AP). Checked against the iPhone18,2 device tree of the
owner's IPSW (`out/kernel-inputs/24A446/devicetree.bin`, read locally, never committed):

| Piece | iPad (droidvm) | iPhone18,2 here |
|---|---|---|
| model, target-type, compatible | iPad17,3, J820, J820AP | iPhone18,2, V54, `V54AP iPhone18,2 AppleARM` [F] |
| chip-id, board-id | 0x8142, 12 | 0x8150, 0x0E [F] |
| `/product` idiom, subtype, scale | pad, 2732, 2 | phone, 2868, 3 [F] |
| Display | 2048x2732 @2x landscape handling | 1320x2868 @3x portrait, 440x956 points. The host's 1440x3120 panel has the same shape within 0.3 %, so the host scales |
| Scale fixup | two instructions of the iPad cache so a virtual display is not forced to an iPad mini layout | **not done**: QuartzCore gives a virtual device's mode @2x; @3x needs the same method re-found in the iPhone cache, or a display mode that carries a preferred scale of 3 |
| Buttons | hold, voldown, volup, home-button-type 2 | the same three; the tree also has `ringeren` (Action button), no host key for it |
| Baseband | none (Wi-Fi iPad) | the tree has `/baseband` (`baseband,n41`): the booter must leave it out |
| `/filesystems/fstab` | not in their trees either (UniXNU's boots log the same APFS line) | the tree has it; the booter must leave it out (section 10) |
| Telephony daemons | CommCenter disabled | kept and idled (section 4) |
| fairplayd | `fairplayd.A2` | `fairplayd.H2` |
| Setup panes | iPad set | the same flags cover SIM and Face ID panes [I] |
| Hardware daemons | - | nfcd, seld, seserviced, nearbyd (UWB), softposreaderd, aned disabled; biometrickitd, cameracaptured idle |
| Trackpad, Pencil, pointer fixes, `BKForceMirroredOrientation`, iPad wallpaper overlay, windowed apps | present | dropped: none applies to a phone. The kernel's wallpaper bind mounts name iPad files and must go |
| Metal clients | `*` on 27 | the named list (`WidgetRenderer_Default`, `*Poster*`, `iconservicesagent`, `SBRendererService`); backboardd always has it |
| Addon visible-check | `AGXMetalG17G.bundle` | the iPhone's AGX bundle name [U] |
| Dynamic Island, always-on | - | follow the identity (`island-notch-location`, `oled-display` are in `/product`) [U] |

## 6. Boot contract: what the booter and kernel must provide

`boot-contract.json` (schema 1). The booter builds the device tree from `adt`; the kernel implements
`launch_policy`, `addon_mounts` and `kernel_features`.

- `adt.root`, `adt.chosen`, `adt.product`, `adt.buttons`: values to set. The rest of `/product` is copied
  from the IPSW's v54ap tree, less `name`, `AAPL,phandle`, `exclaves-enabled`, `has-exclaves`,
  `artwork-display-gamut`, and with `APPLE2` and `MTL2,2` taken out of `graphics-featureset-fallbacks`
  (droidvm `make-droidvm-adt.py`: otherwise the app switcher's cards are blank).
- `adt.required`: properties that need a value or the guest stalls: `unique-chip-id`, the three MAC
  addresses (MobileGestalt waits 5 s per answer without them), `manifest-properties` (cryptexd), a `wlan`
  node, `/bluetooth` transport-encoding 10, `/defaults/vmm-present`.
- `adt.absent`: `baseband`, and `filesystems/fstab` (section 10: with Apple's node the boot stops in
  mount-phase-2).
- `display`: the framebuffer to present, and `scale_note` (the open @3x question).
- `launch_policy.disable_jobs`: the labels. `launch_policy.override`: where they are in force from. The seed
  carries `db/com.apple.xpc.launchd/disabled.plist` and the kernel's unpack of the seed sets its owner (root)
  and mode; the kernel's launch policy writes nothing itself (`kernel/patches/oneios-hosted/0004-README.md`,
  step 5). The report's line "the kernel writes the same override" is from before that was decided; its wording
  stays until the seed changes for another reason, because the report is a file of the seed and a seed with
  another hash makes every Data store start over.
- `launch_policy.service_manager`: start `/private/var/oneios/bin/oneios-init` once and give its task
  launchd's bootstrap port (droidvm `spawn_service_manager`, `droidvm_fix_ownervisor_bootstrap`).
- `launch_policy.force_enabled`: disabled jobs that will run anyway.
- `addon_mounts`: read-only bind mounts after Data is mounted (the Metal plugin over `/System/Library/Extensions`).
- `app_cryptex`, `staged_apps`: the two views of section 3.
- `kernel_features`: launch policy, service manager, no launch constraints for owner binaries, owner
  trust module, data-protection class in an xattr, the key-store stand-in (no passcode, wrap/unwrap),
  the software AES service, MobileGestalt cache drop, text fixups.

## 7. Guest helpers (`userland/helpers`)

| Helper | Job |
|---|---|
| `oneios-init` | Our service manager outside launchd. Reads `services.d/NAME.conf` (`arg=`, `env=`, `uid=`, `keepalive=`, `throttle=`, `bootstrap=`), starts what `enabled/` names, restarts with a throttle, logs to `log/`, writes `state/status`. Ignores a service file that others can write |
| `oneios-bootstat` | `ONEIOS-GUEST stage=backboardd-up|springboard-process|springboard-up ...` on the console and in `state/boot` |
| `oneios-mg` | Asks MobileGestalt and compares with `identity.expect` (ProductType iPhone18,2, DeviceClass iPhone, main-screen 1320x2868 @3x, subtype 2868) |
| `oneios-seed` | First start: unpacks `data.seed` (pax tar) into the new Data volume as root, with owners, modes, links and extended attributes; marks it with the seed's SHA-256; refuses `..`, absolute paths, links out of the volume, devices and hard links |

Built by `helpers/build.sh` for `arm64e-apple-ios27.0`, signed ad hoc with 4 KiB signature pages: Xcode on
macOS, or stock clang with theos' ld64, ldid and an iPhoneOS SDK on Linux (iOSPlus10's
`userland/ssh/build.sh` toolchain). **Not built for iOS yet**: no SDK on this machine. arm64e because iOS 27
kills arm64 platform binaries at exec (ownervisor `build.py`). A debug shell (dropbear) is not built here;
`helpers/README.md` says where to take one (`external/ownervisor`, `external/applebox`, droidvm
`ipados27/scripts/add-ownervisor-data.py`).

### 7.1 The Metal plug-in

Placed by this part, not built by it. What to build (references agent, `docs/references.md`; fetch with
`scripts/setup/get-references.sh`): `external/NativeMetal` at b0d6a07, metal2vulkan 624bab3 with
`external/Droid-VM-UniXNU/patches/metal2vulkan/0001..0038`, Mesa c823a83e with `patches/mesa/*` (Turnip's
darwin backend), by the kernel base's `scripts/build-turnip-darwin.py`, `build-adreno-metal-bridge.py` and
`build-adreno-metal-plugin.py`; pins in `scripts/data/gpu-metal-vulkan-inputs.json`. It needs a Mac with
Xcode 27.1. That stack passes MetalCheck 41/41 on an SM8650 / Adreno 750 phone under iPadOS 27.0.1 [F, references].
Stage the result as `out/userland/NativeAdrenoMetal.bundle`; the hook picks it up, and the plug-in and Turnip
CDHashes go to `owner-trust.json`.

**It must not be staged as upstream builds it.** Read in `external/NativeMetal/NativeAdrenoMetalPlugin.mm`:
the plug-in writes QuartzCore's options at fixed addresses of the iPad17,3 24A446 shared cache, taken
relative to `CARenderServerStart` (0x1846dce88), with no check of which cache is loaded. In the iPhone's
cache the same writes land on other memory of backboardd. So `roottweak` places a bundle only if it holds a
file `oneios-target` with `iPhone18,2 24A446`, which whoever re-finds the addresses for the iPhone cache adds
before signing; otherwise the line is `off` and the guest renders on the CPU (`--metal-plugin-unchecked`
overrides). Also read there: `<Name>.publish` and `<Name>.primelist` are looked up beside the bundle, which
is where they are placed; `<Name>.clients` is read by the kernel's GPU driver, not the plug-in; and the
plug-in's debug switches are fixed paths under `/private/var/ngpu/debug`, not under `/private/var/oneios`.
GPU tests once it runs: droidvm `package/metalcheck`, `gfxprobe`, `gfxbench`.

## 8. What launchd, backboardd and SpringBoard need

| Stage | Needs | Who |
|---|---|---|
| launchd starts | The OS and SystemOS volumes mounted as Apple's root; the dyld cache; AMFI with Apple's three trust caches. Boot tasks with RequireSuccess (fsck, mount phases, data protection, launchd_cache_loader) must succeed: nothing here touches them | kernel, booter |
| Data volume | First start: `newfs_apfs -e -x -v Data` on the store, mount at `/private/var`, the seed unpacked **in the kernel** with its owners and modes, then launchd (`0004-README.md`; booted). The user-space unpacker `oneios-seed` is not used; the hook still places it in `overlay/userland/bin` when the helpers are built. No `/filesystems/fstab` in the tree and no Overprovision volume: section 10 | userland (the seed); kernel (the rest) |
| launchd settles | `disabled.plist` in force (booted: section 10); `unique-chip-id`, MAC addresses, `manifest-properties` in the tree; software AES (or configd waits 5 minutes); no watchdogd | kernel, booter; this part's list |
| keybagd, securityd | Key-store stand-in: no passcode, wrap/unwrap for the keychain; empty `keybags/` | kernel; `data-layout` |
| backboardd | The original IOMobileFramebuffer with a host display behind it; a Digitizer/Touch Screen HID service; the CA preferences of 3.1; backlight properties (iOS 26.5's CoreBrightness crashed on missing `backlight-calibration-parameters`; [U] on 27) | kernel, booter; `preferences` |
| SpringBoard | Identity = iPhone (`oneios-mg` checks it); Setup skipped; migration recorded; watchdogs relaxed; CommCenter and sleepd answering; PosterBoard's store (data-protection class); staged apps and a fresh LaunchServices database; @3x | all; `preferences`, `launchd-jobs` |
| Home screen with GPU | Metal plugin placed and trusted; clients list; kernel accelerator publishing `MetalPluginName` | plugin build (not started), kernel |

## 9. What is still missing for SpringBoard

1. Done: the run on the real root (all 142 labels of the default policy are in the iPhone's cache) and a PC boot with it (section 10).
2. iOS builds of the helpers (an SDK on the build machine), and their CDHashes in the kernel's owner trust module. Until then the kernel's line `no /private/var/oneios/bin/oneios-init in this seed: no service manager` stays. It is about our own helpers (boot stages on the console, the MobileGestalt check, a debug shell), not about the daemon policy.
3. Done by the kernel: the first-start Data path (`0004`), with the seed unpacked in the kernel.
4. The @3x scale: no fixup for the iPhone's QuartzCore yet, and the phone path of `-[CADisplayMode preferredScale]` has not been read (it needs `systemos.img`, not fetched here). Without it the guest may come up @2x (660x1434 points) and lay out wrongly.
5. Kernel features of section 6 for this profile (release-kernel, booter).
6. CommCenter without a baseband: in the PC boot launchd refused `com.apple.CommCenter.plist` (`_LimitLoadToDeviceTree baseband`: "cannot be loaded on this hardware"), loaded `com.apple.CommCenter-noncellular.plist` under the same label and CommCenter ran with no exit in the 5 minutes the log covers. Longer runs and SpringBoard's use of it: [U].
7. The Metal plug-in: nothing built (needs a Mac with Xcode 27.1), and upstream's holds the iPad cache's QuartzCore addresses, which must be re-found in the iPhone's cache first (section 7.1). Until then CPU rendering.
8. Camera, cellular, Face ID, NFC, haptics: idled or disabled, not provided.


## 10. The PC boots of 2026-11-11: what the image and the device tree answer

Two boots under qemu-aarch64 (`docs/building.md` section 5; `out/kernel/24A446/kernel.kc` as it was at 03:28,
sha256 `8ba32d67...`; the image set of 24A446 with this hook's seed). Their logs are in `out/q8/` on the build machine
(`boot1.log`, `boot1-launchd.log`, `boot2-fstab.log`); nothing of this ran on the phone.

### How to read what user space did

launchd writes its own log to the Data volume, `/private/var/log/com.apple.xpc.launchd/launchd.log`: every job it
spawns, every job it refuses and why, and every exit with its reason (`exited due to SIGSEGV`, `OS_REASON_DYLD`,
...), which the console does not show. After a run has stopped:

```sh
apfs-fuse -o ro,vol=2 DATA_DIR/data.img MNT        # the Data volume is the third of its container (index 2)
less MNT/root/log/com.apple.xpc.launchd/launchd.log
python3 -I userland/tweak boot-check --data MNT/root --root OS_MNT/root   # OS_MNT: apfs-fuse -o ro out/image/24A446/os.img OS_MNT
fusermount3 -u MNT
```

`boot-check` exits 1 when a disabled job was started (other than one marked `DisableAllowed = false`), when the
override would not be read (owner, mode) or when there is no launchd.log.

### The daemon policy: in force, as data

No binary and no kernel hook is needed for it. launchd of 24A446 (read in its code, then booted):

- reads `/private/var/db/com.apple.xpc.launchd/disabled.plist` once per boot, after the boot task
  `finish-demo-restore` and before `sysstatuscheck`; with `disabled.migrated` beside it, it does not look for the
  older `/private/var/db/launchd.db/*/overrides.plist`;
- takes the file only when `st_uid == 0` and `st_mode & 022 == 0`; otherwise it goes on with no override and
  logs nothing. The seed has it as root:wheel 0644 and the Data volume of the boot shows the same.

Boot 1, `boot-check` on the Data volume it left (stopped 8 minutes after launchd started; launchd.log covers the first 5):

```
override: 142 jobs disabled; root owns it, mode 0644: launchd reads it
launchd.log: 70 jobs spawned, 54 pending at load
cache: 27 of the disabled jobs want to run at load, 0 of them pending or spawned; of the 93 other such jobs, 46
lookups of disabled jobs that found no service: com.apple.analyticsd (asked by backboardd[55])
no disabled job was started: the policy is in force
```

The cache has 725 jobs. The 27 are those of the 142 with `RunAtLoad` or `KeepAlive` (assistantd,
identityservicesd, imagent, watchdogd, routined, the software update jobs, ...); the other 115 start on demand
and were not asked for. The process count of a boot does not drop against the earlier runs, because those runs
had the same seed: the kernel's line "no service manager" was read as "no daemon policy", which it is not.
launchd refused 18 more jobs by itself ("cannot be loaded on this hardware", "on current os variant").

### `/filesystems/fstab`: the tree must not have it

What reads it in 24A446 (each read in its code):

| Reader | With the node | Without it (what the tree gives now) |
|---|---|---|
| APFS kext, `DT_get_fstab_entries` (behind `sysctl vfs.generic.apfs.edt_fstab`, which is what `getfsent` of libsystem_info reads) | takes the container that holds a System-role volume as the boot container and looks up each entry's `vol.fs_role` in it; a role that is not there fails the whole list (read, not booted), except Data (0x40), whose entry is blanked (booted) | logs `failed to get filesystem entry at: /filesystems/fstab` once per call; `getfsent` returns nothing |
| `/sbin/fsck` (boot task fsck) | `os_env_type` 1: runs `fsck_apfs` on the OS container (15 s under qemu, passed) | `failed to get filesystem info`, `not booting main or diagnostic OS. Skipping fsck on OS container` |
| `/sbin/mount -P 1`, `-P 2` (mount-phase-1, -2) | looks for a Data-role volume in the boot container; with `os_env_type` 1 and phase 2 a missing one is `mount: missing data volume`, exit 66 | `data volume missing, but not required in env: 0`; no entries, nothing mounted; phase 2 goes on to `apfs_boot_util` |
| `apfs_boot_util 2` | only for the exclaves storage path, which a system without exclaves skips | the same |

Apple's node describes one container with the System, Preboot, xART, Data, Baseband-Data, Update and Hardware
volumes. OneiOS has one container per image and a Data store of its own, and the kernel has mounted all of them
before launchd (`0004-README.md`), so there is nothing for the mount phases to mount.

Boot 2 was the test: the same set and Data store, the tree with `/filesystems/fstab` = `os_env_type 1`, Apple's
`system-vol` and `data-vol` entries (a private xnuhost that copies a sidecar's whole subtree; not committed).
Result: fsck ran `fsck_apfs`; mount-phase-1 logged `failed to lookup data volume`, `not required in env: 1` and
`DT_get_fstab_entries:12873: failed to get volume for role: 64`; mount-phase-2 logged `mount: missing data
volume` and the kernel stopped with `panic: mount[8] exited ... exit status 66`.

So: `adt.absent` names `filesystems/fstab` (device profile, boot contract), the booter leaves it out as it does
now, and the one kext line per mount phase is the expected output of this layout, as on UniXNU's boards. Not
tried: a node with entries but no `os_env_type` (it would only silence the line).

### Overprovisioning: nothing to supply

`apfs_boot_util 2` always does it on iOS (`APFSOverProvModel()` in APFS.framework is the constant 2; only a
recovery boot skips it). It takes the container of `/` by `statfs`, which here is the SystemOS image (the union's
top layer: "Found container size of 6157238272 bytes"), reserves 1 % of it (at most 1 GiB) and looks there for a
volume with role 0x2c0, else creates one named `Overprovision`. The image is read-only and hash-checked, so the
creation fails with `e00002c1` and the task's own code goes on ("Ignoring..."). Neither the image set nor the
tree can answer it: the volume would have to be in Apple's image. Nothing is lost: the reserve keeps free blocks
for the NAND of a real iPhone; here the writable store is a sparse file on One UI's file system.

### `tzinit: Unable to locate data; resetting to system partition`

The normal first boot of a volume with no downloaded time zone asset: tzinit finds no `/var/db/timezone/tz_latest`
and points `/var/db/timezone/zoneinfo` at `/usr/share/zoneinfo.default` (the link is on the Data volume after
boot 1). Nothing to supply.

### Seen in passing, for their owners

- The Data volume made by `newfs_apfs -e -x -v Data` has no APFS role (`apfsutil`: "No specific role"), not the
  Data role (0x40). Nothing in these boots asked for it; newfs_apfs has `-R role` and the string "Volume role 0x%x
  is not allowed for an unencrypted volume" (not tried).
- xnuhost's sidecar tree merge (`<kernel>.adt`) copies a node's properties and its children's, not deeper levels.
- launchd.log of boot 1 names the exits the console does not: parsecd, runningboardd and powerexceptionsd with
  SIGSEGV, wcd and SpringBoard (twice in the log's 5 minutes) with SIGTRAP, powerexceptionsd once with the dyld
  assertion `!sMemoryManagerInitialized`, `iomfb_fdr_loader` exit 1.

