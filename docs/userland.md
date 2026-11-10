# iOS 27 userland for OneiOS

Owner: userland. Code: `userland/`. Date: 2026-11-10.

**State: written and tested on the build machine only.** No iOS 27 root has been run through it (the OS
images are not fetched on this machine) and nothing has booted. The tests use a generated fake root; `scripts/image/build.sh` with the hook was run end to end on stand-in inputs only. Marks:
[F] taken from a source that is named, [I] inferred, [U] unknown until a boot.

What this part does: it takes Apple's iOS 27.0.1 system for iPhone18,2 (24A446) as it is and prepares
everything around it so that launchd, backboardd and SpringBoard come up under OneiOS's kernel on the
Galaxy S24 Ultra: which daemons run, the preferences of the first boot, the iPhone identity and display the
booter must present, where our own programs and the Metal plugin go, and how they are trusted.

## 1. How to run it

The image build runs it (`scripts/image/build.sh` calls `userland/image-hook`, hook API 1 of
`docs/image-format.md` section 6). By hand:

```sh
python3 -I -m roottweak apply --root OS_TREE --data DATA_TREE --out OUT   # from userland/; --layout sealed
python3 -I -m roottweak daemons                                           # the daemon policy as text
python3 -I -m roottweak cdhash FILE...                                    # CDHash of signed Mach-O files
python3 -I userland/tests/test_roottweak.py && python3 -I userland/tests/test_helpers.py
```

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
| `launchd-jobs` | Disables the jobs of the daemon policy (section 4): `db/com.apple.xpc.launchd/disabled.plist` = `{label: true}`, root:wheel 0644, with the empty `disabled.migrated` beside it. Labels the root does not have are reported, not written. `rw`: also takes them out of `System/Library/xpc/launchd.plist` | vphone-cli `GuestServiceProfile.swift`, `service_trimming.md`; droidvm `package/DroidVMLaunchPolicy.c`, `scripts/seed-data-disable-jobs.py`; UniXNU `scripts/check-launchd-watchdog-policy.py` (launchd honours the file only when root owns it) |
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

On a sealed root, a disabled job marked `DisableAllowed = false` still runs. On vphone's iPad guest of
24A446 those were analyticsd, OTATaskingAgent, diagnosticextensionsd, mobile.softwareupdated and
symptomsd-diag; whether the iPhone's `V54` file has the same marks is [U] until the hook runs on the real
root (the report names them).

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
- `adt.absent`: `baseband`.
- `display`: the framebuffer to present, and `scale_note` (the open @3x question).
- `launch_policy.disable_jobs`: the labels; the kernel writes `disabled.plist` as root before launchd
  starts, as droidvm does, because the first-start unpack of the seed is the only thing that sets owners.
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
| Data volume | First start: `newfs_apfs -e -x` on the store, mount at `/private/var`, `oneios-seed`, then launchd. Split (board, 2026-11-10): userland supplies the unpacker, which the hook puts in `overlay/userland/bin/oneios-seed` with its CDHash in `owner-trust.json`; booter and kernel make the store, format and mount it, hand the unpacker and the seed to the guest and run it before launchd. Not confirmed by them yet | userland (unpack); booter + kernel (the rest) |
| launchd settles | `disabled.plist` in force; `unique-chip-id`, MAC addresses, `manifest-properties` in the tree; software AES (or configd waits 5 minutes); no watchdogd | kernel, booter; this part's list |
| keybagd, securityd | Key-store stand-in: no passcode, wrap/unwrap for the keychain; empty `keybags/` | kernel; `data-layout` |
| backboardd | The original IOMobileFramebuffer with a host display behind it; a Digitizer/Touch Screen HID service; the CA preferences of 3.1; backlight properties (iOS 26.5's CoreBrightness crashed on missing `backlight-calibration-parameters`; [U] on 27) | kernel, booter; `preferences` |
| SpringBoard | Identity = iPhone (`oneios-mg` checks it); Setup skipped; migration recorded; watchdogs relaxed; CommCenter and sleepd answering; PosterBoard's store (data-protection class); staged apps and a fresh LaunchServices database; @3x | all; `preferences`, `launchd-jobs` |
| Home screen with GPU | Metal plugin placed and trusted; clients list; kernel accelerator publishing `MetalPluginName` | plugin build (not started), kernel |

## 9. What is still missing for SpringBoard

1. A run on the real root. Needs the OS images (10.6 GiB fetch). The first run's report tells which policy labels the iPhone root lacks.
2. iOS builds of the helpers (an SDK on the build machine), and their CDHashes in the kernel's owner trust module.
3. The first-start Data path: the unpacker is written and host-tested; the store, `newfs_apfs`, the mount and the call are booter's and the kernel's and not written.
4. The @3x scale: no fixup for the iPhone's QuartzCore yet, and the phone path of `-[CADisplayMode preferredScale]` has not been read (it needs `systemos.img`, not fetched here). Without it the guest may come up @2x (660x1434 points) and lay out wrongly.
5. Kernel features of section 6 for this profile (release-kernel, booter).
6. CommCenter without a baseband: kept by the iPhone evidence, unproven on this kernel.
7. The Metal plug-in: nothing built (needs a Mac with Xcode 27.1), and upstream's holds the iPad cache's QuartzCore addresses, which must be re-found in the iPhone's cache first (section 7.1). Until then CPU rendering.
8. Camera, cellular, Face ID, NFC, haptics: idled or disabled, not provided.
