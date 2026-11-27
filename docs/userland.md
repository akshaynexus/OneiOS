# iOS 27 userland for OneiOS

Owner: userland. Code: `userland/`. Date: 2026-11-11.

**State: run on the real root and booted on the PC; nothing has run on the phone.** `scripts/image/build.sh` with
the hook builds the image set of 24A446 from the real images, and the hosted kernel boots that set under
qemu-aarch64 to launchd and about 120 processes. In that boot launchd applies the daemon policy (section 10 has
the numbers and how to check a run). The guest helpers are not built (the iOS toolchain of
`docs/building.md` section 9 exists since 2026-11-11; `helpers/build.sh` does not use it yet), so nothing of
`/private/var/oneios/bin` runs. The Metal plug-in is built with that toolchain and placed (section 7.1); not run. The tests use a generated fake root. Marks:
[F] taken from a source that is named, [I] inferred, [U] unknown until a boot.

What this part does: it takes Apple's iOS 27.0.1 system for iPhone18,2 (24A446) as it is and prepares
everything around it so that launchd, backboardd and SpringBoard come up under OneiOS's kernel on the
Galaxy S24 Ultra: which daemons run, the preferences of the first boot, the iPhone identity and display the
booter must present, where our own programs and the Metal plugin go, and how they are trusted.

## 1. How to run it

The image build runs it (`scripts/image/build.sh` calls the userland hook, hook API 1 of
`docs/image-format.md` section 6). Since 2026-11-21 the tweaks are Rust, `oneios-tool image tweaks` (crate
`tools/oneios-tool/crates/oneios-tweaks`, the port of `userland/roottweak` and `userland/image-hook`; the same seed
and overlay byte for byte: `tools/oneios-tool/README.md` "image tweaks"), and build.sh runs that hook by default;
on the phone the OneOS app runs the same code (`oneios-tool serve`, request `tweaks`). By hand:

```sh
oneios-tool image tweaks apply --root OS_TREE --data DATA_TREE --out OUT   # --layout sealed; --scale 2 for the 2x UI
oneios-tool image tweaks apply --root-image out/image/24A446/os.img --data DATA_TREE --out OUT   # the OS volume without a mount
oneios-tool image tweaks daemons                                           # the daemon policy as text
oneios-tool image tweaks cdhash FILE...                                    # CDHash of signed Mach-O files
oneios-tool image tweaks boot-check --data DATA_VOLUME [--root OS_TREE]    # after a boot: section 10
cargo test --release --manifest-path tools/oneios-tool/Cargo.toml -p oneios-tweaks   # the root tweaks' 38 cases
userland/tests/host.sh                                                     # the guest helpers' C, built for this machine
```

The Python original (`userland/roottweak`, `userland/tweak`, `userland/image-hook` and its tests
`test_roottweak.py`, `fakeroot.py`) was deleted on 2026-11-21 (the owner's decision: no Python of ours in the
repository), after the port gave the same seed and overlay byte for byte; `roottweak` in this document now means
`oneios-tool image tweaks` (its commands and options are roottweak's). The guest helpers' host tests
(`test_helpers.py`, 11 cases) are `userland/tests/host.sh` since the same day: POSIX shell, the same cases (89
checks), CI runs it with the other `*/tests/host.sh`; the seed archives it unpacks are written by `oneios-tool debug
mktar`, the bytes Python's tarfile wrote for the old test. The config
(`userland/config/*.toml`) is read from the checkout by build.sh (`--repo`), and built into the binary for the
phone: rebuild the tool after an edit there.

Output in `OUT` (the hook puts it in the image set's `overlay/userland/`):

| File | For | Content |
|---|---|---|
| `boot-contract.json` | booter, kernel | device tree identity, display, launch policy, mounts, kernel features (section 6) |
| `xnuhost.conf` | xnuhost | `scale=3` or `scale=2`: the UI scale this image was made for (section 5.1) |
| `owner-trust.json` | kernel build | CDHashes of our own binaries for the owner trust cache; `oneios-tool phone addon` adds a boot addon's CDHashes to the staged one (hosted 0090) |
| `tweaks.manifest.json` | image build | every path changed, with the owner and mode the guest needs |
| `report.txt`, `report.json` | people | one line per item: `applied`, `kept`, `not-found`, `off`, `contract` |

The build is not root, so owners are recorded, not set; the hook writes them to `$ONEIOS_OWNERS`.
A second run over the same trees changes nothing. A preference file that is not a property list stops the
run instead of being written over. A root of a build the device profile lists in `[builds]` (`userland/config/device-iphone18,2.toml`:
`"24A446" = "27.0.1"`, `"24B156" = "27.2.1"`, 2026-11-25) with that ProductVersion is taken as the profile's ("kept",
a line that says which); [device] version stays the first one's. Its IPSW device tree for v54ap differs from 24A446's
in four values none of which this profile states [F: `xnuloader-inspect adt` of both]; no 24B156 root has been read
here (its OS volume was not fetched) [U]. Any other root that is not iOS 27.0.1 is refused (`--any-version`), except iOS 16.5
(20F66), the Galaxy A56's userland (docs/a56.md, 2026-11-23): its tweaks apply with a line that says so, and its
`xnuhost.conf` carries the 16.5 boot arguments (the 16.5 shims, `oneios-gpu-metal=0`, and since 2026-11-23
`oneios-mac-exec-skip=mediaserverd,TVRemoteConnectionService`, kernel 0073), the keybagd fixup (only for 20F66's own
`keybagd`, by its SHA-256) and the QuartzCore vsync fixup. Since 2026-11-24 its boot arguments also carry hosted 0103-0105's switches
(`oneios-hid16=1`, configd's `oneios-grant`, `oneios-ls-keep=3`) and `oneios-disable-jobs` with the other A56 fork's idle
jobs (suggestd, spotlightknowledged, corespotlightservice, siriknowledged, tipsd, gamed, followupd, tailspind) beside
0053's three; xnuhost adds the same words to an older 20F66 image ([booter.md](booter.md), "iOS 16.5's kernel switches"). On the 20F66 root of 2026-11-23 the report says 6 launchd jobs
disabled (most of the policy's labels are not in 16.5) and no jetsam properties for V54 (the root has D22's).

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
| DisableAllowed | iOS 27 marks some jobs `DisableAllowed = false` in `com.apple.jetsamproperties.V54.plist`; launchd runs them although disabled. `sealed`: the affected labels are named in the report and the contract, and the hosted kernel answers it (0098, `oneios-disable-allowed`, 2026-11-24): before launchd it renames the mark in a copy of the plist that 0031 shows over `/System/Library/LaunchDaemons`; 24A446 marks 19 jobs, osanalyticshelper among them, which ran at 31-85 % of a CPU although disabled (session of 2026-11-24 01:17). `rw`: the mark is removed | vphone-cli `service_trimming.md` "iOS 27 and DisableAllowed" (`patch-jetsam-disable-allowed`) |
| `launchd-crash-policy` | `rw` only: `_PanicOnCrash` renamed, `RecoverOnCrash` removed for backboardd, SpringBoard, configd, runningboardd, so a crash loop does not restart the guest during bring-up | iOSPlus10 `scripts/laurel/ios26-root-spec.py`, `docs/laurel_sprout/ios26-backboardd.md` |
| `host-services` | Places `/private/var/oneios`: `bin/` (helpers), `services.d/*.conf`, `enabled/`, `identity.expect`. `sealed`: the kernel starts `bin/oneios-init`. `rw`: a launchd job `com.oneios.init` | ownervisor (the manager outside launchd), droidvm `add-ownervisor-data.py`; job keys from iOSPlus10 `scripts/net/root-add-ssh.py` |
| `metal-plugin` | Places a built plugin bundle with `<Name>.clients` (who gets the device) and `<Name>.publish`. `sealed`: `/private/var/oneios/addons/Extensions`, shown over `/System/Library/Extensions` by a kernel bind mount. `rw`: in `/System/Library/Extensions` | UniXNU `scripts/add-gpu-plugin-data.py`, `patches/xnu-12377/zzy-native-gpu-addon-mount.patch`; droidvm `build-droidvm-data-seed.py` (client list); iOSPlus10 `scripts/laurel/root-add-metal-ios26.sh` |
| `camera-plugin` | Places the capture plug-in (`userland/camera/build.sh`, `--camera-plugin DIR`) where CMCapture 27 loads it for the kernel's `AppleH9CamIn`. `sealed`: `/private/var/oneios/addons/MediaCapture/H9ISP.mediacapture`, shown over `/System/Library/MediaCapture` by hosted 0056 (`addon_mounts`). `rw`: in `/System/Library/MediaCapture`. Its CDHash goes to the owner trust list; the contract's `camera` says what the kernel must publish | read in CMCapture of the 24A446 cache (`fcc_probeCameraDriverService`, `FigCaptureGetCaptureDeviceCreateFunction`); HakiOS/iOSPlus10 for the device object |
| `app-cryptex` | `sealed`: nothing copied; the contract asks the kernel to mount the image set's `app.img` at `/private/preboot/Cryptexes/App`. The kernel answers `FSIOC_AUTH_FS` for that mount, so its programs pass AMFI's launch constraints (`0032-app-cryptex-authenticated.patch`). With `--appos DIR` the files are carried on Data as droidvm does | droidvm `scripts/add-app-cryptex-data.py`, UniXNU `zzyb-native-cryptex-os-view.patch` |
| `staged-apps` | The removable system apps stay in `/private/var/staged_system_apps`; the kernel shows them in `/Applications` (hosted `0035`: on unless `owner-staged-apps=0`; before it nothing did, and the phone's Home Screen was empty). `rw`: moved there | droidvm `ipados27-bringup.patch` (owner-staged-apps); iOSPlus10 `scripts/ios16/root-finish.sh` |
| `preferences` | Section 3.1 | see there |
| `data-layout` | `containers/{Bundle,Data/System,Shared/SystemGroup,Temp}`, `keybags` (0700, empty), `mobile/tmp`, `tmp` (1777), `run`, `vm` (root 0755: XNU's compressor swaps to `/private/var/vm/swapfileN` and never makes the folder; without it every swap file failed, 2026-11-12; an older store gets it from the kernel, hosted 0057); logd's stores `db/{diagnostics,uuidtext,diagnostics/timesync}` with `.migrated`, owner 272. Removes a MobileGestalt cache carried in the seed | UniXNU `scripts/make-data-seed.py`, `prepare-owner-data-fixture.py`; ios-sweet `mkiosroot.sh`, `sweet-root-tweaks.py`; iOSPlus10 notes (stale cache) |
| `keybag` | Off: `keybags/` stays empty, which is what the kernel's key store expects (`kernel/patches/oneios-hosted/keystore`: UniXNU's AppleKeyStoreState, system keybag handle 0 always loaded, no passcode; launchd's keybag task then ends with `DONE MKB_INIT`). `--plain-keybag` writes ios-sweet's plain `systembag.kb` for a kernel that reads one | UniXNU `scripts/probe-keybag-bypass.py`; ios-sweet `mkiosroot.sh` |
| `feature-flags` | Both layouts: `CoreLocation/CLAutoCohort` off in the Data volume's override `preferences/FeatureFlags/Domain/CoreLocation.plist` (XML, root 0644; `/Library/Preferences/FeatureFlags` on the guest, read after the root's defaults): with CoreLocation's automatic cohort queues locationd crash-looped, 17 times a session (pc 0 from `_dispatch_lane_serial_drain`; `kernel/patches/oneios-hosted/README.md`, 0048). Cost: locationd's silos keep their own queues. A Data store made from an older seed gets the entry from the kernel (hosted 0048). `rw` only: ExtensionKit `enforce_xpc_cache_code_signing` off, or installd and lsd refuse every app extension once the extension cache's signature no longer holds | vphone-cli `patch-locationd-cohorting` (iOS 27); droidvm `PROGRESS.md` 11-02 (Cohort:0 queue); iOSPlus10 `scripts/ios16/root-tweaks-extra.py` |
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
| dictation | `com.apple.assistant.support`: `Offline Dictation Status` = `{}` | AssistantServices caches the installed offline dictation languages of this key and posts "availability changed" each time it fills the cache; with no value (assistantd, which writes it, is disabled) the cache stays nil and the keyboard dock asks again on each post: a main-thread loop in every app with a keyboard when there is no network (Spotlight 105% and no keyboard, Settings busy 389 s while typing, 2026-11-22). A store from an older seed gets it from the kernel (hosted 0083). `--no-dictation` leaves it out | read in AssistantServices and UIKitCore of 24A446 (`kernel/patches/oneios-hosted/README.md`, 0083) |
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
| keep | 58 | Never disabled: the boot chain (SpringBoard, backboardd, runningboardd, lsd, containermanagerd, cfprefsd, notifyd, configd, powerd, securityd, AMFI, keybagd, mobileactivationd, logd, cryptexd, ...), installs and debugging, and the jobs that make things worse when off: sleepd, CommCenter and its helpers, cloudd, nanoregistryd, siriactionsd, passd, financed, generativeexperiencesd, bluetoothd, navd (2026-11-22: the Maps widget reconnects to it with no back-off), duetexpertd (2026-11-23: PosterBoard's Add New Wallpaper gallery waits on it) | vphone-cli `neverDisable`, iPhone guest, iOS 27.0; vphone-android `services.rs`; droidvm (bluetoothd) |
| idle | 11 | Run without their hardware and must rest: CommCenter (no baseband node), bluetoothd (virtual transport), cameracaptured, locationd, keybagd (key-store stand-in), biometrickitd, thermalmonitord, wifid, audiomxd | reasons per label in the file |
| replace | 5 | Disabled; the host does the job: watchdogd (no watchdog device; the switch has its dead-man timer), the four OTA jobs (updates are a new image set) | UniXNU `docs/guide/skip-setup.md`; droidvm |
| group base (off by default since 2026-11-23) | 110 | Siri and intelligence, Find My, Wallet/NFC/SE, Watch, Home, Maps background, iCloud and iMessage sync, ads, accessory firmware (its 16 analytics and telemetry jobs went to the group telemetry): they run; `--group base` disables them again | vphone-cli `base27`: -29 % memory, -26 % idle CPU, -60 % idle writes, "no feature lost" (not so here: the wallpaper gallery, `kvsd`, Mail's and the widgets' services were missing on the phone, 2026-11-23) |
| groups extra and signin (off by default since 2026-11-23), disable: hardware | 4 + 2, 1 | momentsd, coreduetd, modelcatalogd, mstreamd and the sign-in prompts run; aned (no Neural Engine) stays disabled. fairplayd.H2 runs since 2026-11-23 (the owner: App Store apps need it); droidvm saw its services hang lockdownd and the apps behind it on the iPad without the FairPlayIOKit kext, which this kernel does not carry either: watch its first boot | vphone-android; droidvm `DroidVMBootArgs27.c`; iOSPlus10 |
| disable: loops | 3 | aonsensed and prngseedd (respawn loops, 2026-11-22: section 11); axassetsd: it lists the speech synthesis audio units (SiriAUSP, MacinTalkAUSP, MauiAUSP, KonaSynthesizer), which were started four at a time and killed by SIGKILL 3-4 s later, still in dyld, every 8-10 s; their corpses filled the kernel's limit. Cost: Accessibility's downloadable assets. A Data store from an older seed gets it from the kernel (hosted 0053) | this phone, 2026-11-12 (`kernel/patches/oneios-hosted/README.md`, 0053) |
| disable: telemetry (on by default since 2026-11-23) | 18 | osanalyticshelper (out of keep) and its second job osanalyticshelper.agent (its "agent-init", started by a Trial notification after the network comes up: with only the first disabled it still ran at 15-49% of a CPU, 2026-11-23 20:23; hosted 0094) and the analytics, telemetry and diagnostics uploaders of vphone's base list (analyticsagent, audio, ecosystem, geo, input and wifi analytics, wirelessinsightsd, cloud, hang, USB and SQLite telemetry, diagnosticextensionsd, diagnosticspushd, symptomsd-diag, rtcreportingd, perfpowermetricd). In the session of 2026-11-23 18:24 osanalyticshelper used 55-103 % of a CPU for minutes after the Wi-Fi join (70895 page faults in 10.4 s of its 13.7 s), every guest CPU busy. ReportCrash stays: a crash still gets its `.ips` (OSALog with a writing block does not use the helper); nothing is submitted. A Data store from an older seed gets it from the kernel (hosted 0093, 0053's list; 0092's enable list no longer has them). Both osanalyticshelper jobs, diagnosticextensionsd and symptomsd-diag are `DisableAllowed = false` (row DisableAllowed above): on the sealed root launchd ran them although disabled until hosted 0098 (osanalyticshelper at 31-85 % in the session of 2026-11-24 01:17, with 0093 and 0094 in) | this phone, 2026-11-23 (`kernel/patches/oneios-hosted/README.md`, 0093) |
| off by default | store 2, fewest 8, bringup-26 9 | Each costs a feature or is unverified on 27; `--group NAME` | vphone; iOSPlus10 iOS 26 |

27 labels are disabled by default since the evening of 2026-11-23 (the replace 5, aned, the loops 3, the telemetry 18; 9 that morning, 143 before: the owner asked for every job but the crash loops, then for analytics and crash-report submission off. A Data store from an older seed gets the other 118 enabled by the kernel, hosted 0092 through 0044's list, and the loops and telemetry disabled, hosted 0053 and 0093. Before that: axassetsd added on 2026-11-12, aonsensed and prngseedd on 2026-11-22, group `loops`; navd moved to keep on 2026-11-22: without it GeneralMapsWidget reconnected ~28000 times in one session, 54-103% CPU with geod and launchd beside it, and a store from an older seed gets it enabled by the kernel, hosted 0083 through 0044). analyticsd moved from base to keep on 2026-11-12: without it CoreAnalytics'
clients retry each synchronous send with a new connection (backboardd at 217% after a brightness change, on the
phone); a Data store made from an older seed gets it enabled by the kernel (hosted 0044). One open disagreement: droidvm disables **CommCenter** on the (Wi-Fi) iPad,
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
| `/product` idiom, subtype, scale | pad, 2732, 2 | phone, 2868, and the UI scale (3, or 2) [F] |
| Display | 2048x2732 @2x landscape handling | the host panel's native mode, 1440x3120 portrait at 120 Hz, every pixel rendered by the guest; @3x (480x1040 points) or @2x (720x1560 points). Section 5.1 |
| Scale fixup | two instructions of the iPad cache so a virtual display is not forced to an iPad mini layout | QuartzCore's rule read in this build's cache (it is the iPad's code at the same offset): @2x is its own answer, @3x takes two text fixups xnuhost adds. Not verified in a running guest. Section 5.1 |
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
| `/product` as the guest gets it | - | [F, 2026-11-13] not the profile's `[adt.product]` and not the IPSW tree's: xnuhost copies `/product` only from `<image>/identity.adt`, which no script writes (none in the PC's set; a `kernel.kc.adt` sidecar could carry one, none does), so the guest has `product-name`, `artwork-scale-factor` and `model` only (until 2026-11-22; since then the loader's no-identity branch also writes 24A446 V54's `artwork-device-idiom` phone, `artwork-device-subtype` 2868, `artwork-dynamic-displaymode` 0, `graphics-featureset-class` APPLE10 and its fallbacks without APPLE2, and `artwork-display-gamut` sRGB: "The artwork answers" below) (no `artwork-device-idiom`, `artwork-display-gamut`, `oled-display`, `island-notch-location`, ...). The profile says `artwork-display-gamut "sRGB"`, xnuhost's machine tree `P3` when an identity is given. `--set identity=out/kernel-inputs/24A446/devicetree.bin` is the A/B test (`kernel/patches/oneios-hosted/display/README.md`, "Colour, depth and the panel") |

### 5.1 The screen: native resolution, 120 Hz, UI scale 3 or 2

The guest does not present the iPhone 17 Pro Max's own 1320x2868 frame. It renders the Galaxy S24 Ultra's panel at
its native mode, **1440x3120 at 120 Hz**, and the display helper shows that frame 1:1 (`runtime/display/README.md`).
The UI scale is a setting, at full resolution either way:

| Scale | Points | Against the Pro Max's 440x956 | Physical size of a point (505 ppi) |
|---|---|---|---|
| 3 (default) | 480x1040 | 9 % more points each way | 1/168 inch (the iPhone's: 1/153): everything 9 % smaller; a 44-point target is 6.6 mm (7.3 mm on the iPhone) |
| 2 | 720x1560 | 64 % more points each way | 1/253 inch: everything 61 % of the iPhone's size; a 44-point target is 4.4 mm |

**Setting it.** One value, in every place that states it:

```sh
ONEIOS_SCALE=2 scripts/image/build.sh PROFILE          # the image: roottweak apply --scale 2
xnuhost ... --set scale=2                              # one run, over the image's value (bench)
```

| Place | Written by | Read by |
|---|---|---|
| `boot-contract.json` `display.scale`, `display.points`, `adt.chosen.display-scale`, `adt.product.artwork-scale-factor` | roottweak (`config.device(scale=)`) | people, the booter |
| `identity.expect` on the Data seed: `main-screen-scale`, `main-screen-width`, `main-screen-height` | roottweak | `oneios-mg` in the guest |
| `overlay/userland/xnuhost.conf` `scale=N` | roottweak | xnuhost |
| `/chosen display-scale`, `/product artwork-scale-factor`, `/chosen/xnuhost display-*`, boot argument `offline-legacy-unit-scale=N` | xnuhost (`docs/booter.md` 2.8) | MobileGestalt, the framebuffer driver |
| `/chosen text-fixups` (scale 3, build 24A446 or 24B156) | xnuhost | the kernel, for QuartzCore |

Width and height are not a setting: xnuhost takes them from the panel, and the profile's `[display]` (1440x3120,
120 Hz) is what it finds on this phone. An image built before this change has `main-screen-width=1320` in its
seed's `identity.expect`: rebuild the image set, or `oneios-mg` reports the difference. `out/image/24A446` has
not been rebuilt.

**What iOS 27 reads** (24A446; [F] read in the cache or a reference, [I] inferred, [U] unknown):

- `main-screen-scale`: MobileGestalt answers it from `/chosen display-scale` [F: UniXNU
  `docs/guide/mobilegestalt.md`]. `main-screen-width`/`height`: [I] from the framebuffer's mode
  (MobileGestaltHelper opens the framebuffer in every boot), not from the device tree, which has no such
  property; not confirmed in the cache.
- The render server's scale: `-[CADisplayMode preferredScale]`, QuartzCore, `dyld_shared_cache_arm64e.01` at
  file offset 0x40b1758 [F: disassembled here; the instruction Droid-VM patches for the iPad, `cmp x8, #1080`,
  is at the same offset 0x40b17b8, so the phone and the iPad have one QuartzCore]. In order: the mode's own
  preferred scale if it has one (a real iPhone's panel driver supplies 3); else, in a virtual device, 2, or the
  mode's native-scale field when that is more; else 2 for a mode taller than 1080 pixels and 1 otherwise.
  `display-scale` is not read. This framebuffer's modes carry no preferred scale, so **the unchanged answer is
  2**. For 3, xnuhost adds two fixups (`mov w9, #2` -> `#3` at 0x40b17a8; `mov w8, #1; cinc` -> `mov w8, #3;
  nop` at 0x40b17bc). iOS 27.2.1 (24B156) has the same function, the same 32 instructions, at 0x3f9d6f4 (found by its bytes, one match in .01, and checked by its selector, entry 11 of the same method list): its two fixups are at 0x3f9d744 and 0x3f9d758 with the same bytes (`runtime/xnuhost/src/config.rs` `SCALE_FIXUPS_3X`, per build; a build not there gets none). Which of the two paths this guest takes ([U]: the byte tested is a global set once, taken
  to be "virtual device") does not matter, both are changed.
- The refresh rate: the framebuffer's refresh attributes and `IOMFBDisplayRefresh` (minimum interval 1/120 s,
  maximum 1/24 s), as a ProMotion panel reports them (`kernel/patches/oneios-hosted/display/README.md`). No
  MobileGestalt answer is written for it; [U] whether anything consults one.
- `ArtworkDeviceSubType` stays 2868 and `artwork-device-idiom` phone: the subtype names the Pro Max's set of
  artwork and its layout metrics, and nothing in an iPhone's OS image is made for a 3120 subtype [I].

**The artwork answers (fix-artwork-traits, 2026-11-22).** CoreUI picks each image of an asset catalog (`Assets.car`) by
the device's artwork traits, which it asks MobileGestalt for (`ArtworkTraits`: `ArtworkDeviceIdiom`,
`ArtworkDeviceSubType`, `ArtworkDisplayGamut`, `ArtworkGraphicsFeatureSetClass`, `ArtworkDeviceScaleFactor`), and
MobileGestalt reads them from `/product`. Without an identity tree the guest's `/product` had only `product-name`,
`artwork-scale-factor` and `model`, so the answer was `{ArtworkDeviceScaleFactor = 3}`:

- [F] iOS's log (the 2026-11-22 20:28 session): `CUICatalog: couldn't get 'kMGArtworkDeviceIdiom' from MGCopyAnswer got
  '{ArtworkDeviceScaleFactor = 3;}'` (and the same for `kMGArtworkDisplayGamut`, `kMGArtworkGraphicsFeatureSetClass`),
  117 lines, from SpringBoard, iconservicesagent, Preferences and the apps.
- **Not the cause of the `faccessat` storm** (checked on the phone): Spotlight's 171000-179000 `faccessat` calls each 30 s
  (105% of a core; MobileNotes 74000; WebSheet about 6000 a second), which the stack samples put in UIKit's
  `__UIImageCollectImagePathsForPath`, went on with this fix on a fresh Data store (172846 in one 30 s period), while
  the CoreUI lines went from 117 to 0. The storm, and the keyboard that waits for Spotlight's busy main thread, are
  still open (the next boot names the files with `oneios-sysfail-enoent=1`).

The loader (`runtime/loader/src/machine.rs`, `Machine::tree`; `machine.c`'s `xl_machine_tree` when this was written) now writes, without an identity, the values of 24A446's
V54 device tree (`out/kernel-inputs/24A446/devicetree.bin`, read with `xnuloader-inspect adt`): `artwork-device-idiom`
"phone", `artwork-device-subtype` 2868 (or 2340 on a 1080x2340 panel, the iPhone 12/13 mini 360x780-point grid; `artwork-subtype=N` overrides either; ported from `NoNameBuilder/OneiOS` `2a021be7`), `display-corner-radius` when `corner-radius=N` is non-zero (two little-endian u32s, radius and 1; 34 on `galaxy-s25`, `120df342`), `artwork-dynamic-displaymode` "0", `graphics-featureset-class` "APPLE10",
`graphics-featureset-fallbacks` "APPLE9:APPLE8:APPLE7:APPLE6:APPLE5:APPLE4:APPLE3:APPLE3v1:APPLE1:GLES2,0" (APPLE2
taken out, as with an identity; since 2026-11-25 only with xnuhost's `astc=0`: at the default `astc=1` APPLE2 and MTL2,2 stay, MobileGestalt
answers DeviceSupportsASTC, and SpringBoard's snapshots are kept JPEG or PNG by the Metal plug-in's `SnapshotFormat.inc`, which
is what the blank app-switcher cards below came from; docs/switches.md `astc`), and `artwork-display-gamut` "sRGB" (the device tree has P3; the display helper tags the
frames sRGB). Test: `runtime/loader/tests/test_loader.c` (177 checks, the C loader; its cases are in `runtime/loader/tests/loader.rs` since the Rust port). On the phone (2026-11-22 21:28, a fresh Data store from the seed): 0 `couldn't get 'kMGArtwork...'` lines in iOS's log (117 the session before). MobileGestalt's cache
(`containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches/com.apple.MobileGestalt.plist`,
`CacheVersion` 24A446) pulled at 21:22, before that boot, held `ArtworkTraits = {ArtworkDeviceScaleFactor = 3}` (key
`oPeik/9e8lQWMszEjbPzng`): the answers of the tree of its own boot. **Correction (2026-11-22, enable-camera-app):** an
existing store did not keep an old cache. Until hosted 0084 the kernel removed the cache at every boot (the base's
`native_gestalt_cache_drop`, `bsd/kern/kern_exec.c`), and MobileGestaltHelper made it again from that boot's tree
(below).

**MobileGestalt's cache follows the tree (hosted 0084, 2026-11-22).** MobileGestalt reads the device tree and keeps the
answers (strings in `CacheExtra`, booleans such as `still-camera` in `CacheData`); it builds the file again only for
another OS build. With the cache removed at every boot, each boot ran its first seconds without one: iOS's log of the
phone's boot of 2026-11-22 17:01 has `Could not open .../com.apple.MobileGestalt.plist: No such file or directory` from
144 processes between 17:01:39.6 and 17:01:47.6 (271 in 21 s at 16:30, 48 on the fresh store at 15:59), one line each
process; each then worked out its answers itself [I: IOKit lookups and XPC to MobileGestaltHelper, whose `platform fast
path elided` lines number 189]. Now xnuhost's loader writes `/chosen oneios-identity`, a hash of what MobileGestalt can
read that stays the same from boot to boot (`machine::identity` in `runtime/loader/src/machine.rs`, C: `xl_machine_identity`; [booter.md](booter.md), the loader), and the kernel,
before launchd, compares it with the marker `/private/var/oneios/mobilegestalt-identity` (16 hex digits). When they
differ, or there is no marker (every store from before 0084), it removes the cache and writes the marker: the next
answers are worked out from this tree, so a device-tree change (the camera nodes of `docs/hardware.md` 7) reaches an
existing Data store once. When they are the same the cache stays. One line in `xnuhost.log`: `oneios-mg-cache:
identity 1a2b... (marker none): cache removed (0), marker written (0)`, or `... (marker the same): cache kept`. Boot
argument `oneios-mg-cache=2`: removed at every boot (the old way, the A/B switch); `=0`: never removed. A tree with no
`oneios-identity` (an older xnuhost): removed at every boot, as before. What proves it on the phone: the second boot
after a change has `cache kept` and no `Could not open .../com.apple.MobileGestalt.plist` line in iOS's log. Not run
on the phone yet. A new Data seed carries no cache (the root tweaks remove it) and no marker.

**The iOS 16.5 identity (fix/a56-ios16-faults, 2026-11-24; the Galaxy A56, OneiOS issue 73).** iOS 16.5's
MobileGestalt (20F66, read in `libMobileGestalt.dylib` of its cache on the build laptop) does not take the product
from the root's `model`: `MGGetProductType` and `MGIsDeviceOfType` copy `IODeviceTree:/product product-id` (a 20-byte
value) and look it up in a table of 363 products (20 bytes, flags, the product type). The tree xnuhost made for
iOS 27 has no `product-id` (24A446 does not read it), so on 20F66 the product type was -1:

- bluetoothd (`/usr/sbin/bluetoothd` of 20F66) picks its chip in one function (0x10029aa14) by `getenv("BT_CHIP")`
  (only `ACI BT`) and then dozens of `MGIsDeviceOneOfType` lists of these ids; none matched, it logged "Unknown
  platform", left the chip null, and its caller read `[chip + 0x260]` (`ldr w23, [x0, #0x260]` at 0x100274cf0): `trap
  ... far 0x260 ... pc ...cf0 lr ...cf0` and `Process[N] crashed: bluetoothd` every 10 s, 29 times. Its xnuhost log
  had the lookup: `oneios-iokit: bluetoothd[N] property "product-id" (no plane) of IOPlatformDevice "product" -> not
  found`. The iPhone X's id is in one of the factory's lists (at 0x10029b110, chip 15) and in two other functions' (0x1002dcafc, 0x1002fbe74): with it the factory makes chip 15.
- Settings > General > About had no Model Name. [I] MobileGestalt reads the marketing name from `/product
  product-name` (and `product-description`), but only once it knows the product.
- The Serial Number was N/A: MobileGestalt answers it from `IOPlatformExpertDevice`'s `IOPlatformSerialNumber`, which
  XNU publishes from the root's `serial-number` (`IOPlatformExpert::publishPlatformUUIDAndSerial`). Our tree had
  none, for iOS 27 too.

What xnuhost gives a 20F66 image set now (the runtime alone, no image remake; `runtime/xnuhost/src/config.rs`
`IOS16_PRODUCT_ID`, `IOS16_SERIAL`, the loader's `Machine::product_id` and `serial_number`, docs/booter.md):
`/product product-id` = the iPhone X's (`fd079b298144a7deb4d7c87fef3eb474d8f0e7eb`, from `DeviceTree.d22ap` of the
same restore file, `oneios-tool remotezip get` + `image im4p` + `xnuloader-inspect adt`; entry 36 of the table),
`/product product-description` = `product-name` = "OneiOS guest" (ours), and the root's `serial-number` for the
made-up serial `ONEIOS16A56` (never an Apple serial; 11 characters, laid out as XNU's IODTPlatformExpert reads them:
ArmDTPlatform is one). The root's `model` (iPhone18,2) and `target-type` (V54) stay as they are, so the jetsam
properties and every per-board table follow what they followed before; whether 16.5 wants D22's there too is open.
The tree changes, so `/chosen oneios-identity` changes and MobileGestalt's cache is made again once (hosted 0084).
iOS 27 (any other build): no `product-id`, no `product-description`, no `serial-number`; its tree is the same bytes.
**The proof on the A56** (not booted here): xnuhost's `ios16: /product product-id fd079b29... serial-number
ONEIOS16A56`; `oneios-iokit: bluetoothd[N] property "product-id" ... -> found`; no `trap ... far 0x260` and no
`crashed: bluetoothd`; About shows a Model Name and Serial Number `ONEIOS16A56`. What bluetoothd does after the chip
(its BlueTool chip boot, the transport) is docs/hardware.md section 4.

**Layout risks. None of this has been seen: no frame of SpringBoard's has been rendered at any size.**

Both scales:

- 480x1040 and 720x1560 are not the point size of any iPhone. Apps and SpringBoard that lay out from the
  screen's bounds fill it; code that looks the device up in a table of known sizes (keyboard metrics, the
  status bar and Dynamic Island geometry, SpringBoard's icon grid, wallpaper and Lock Screen layouts keyed by
  the artwork subtype) gets the Pro Max's 2868 entry for a screen that is 40 points wider and 84 taller (at 3),
  or misses.
- Apps built against old SDKs, or without a launch storyboard, run in a compatibility size iOS picks from the
  device classes it knows: letterboxed, at a size [U] for this screen.
- The safe-area insets, corner radius and the island's place are the Pro Max's (from the subtype). The S24
  Ultra has a centred punch-hole camera near the top edge and nearly square corners: the island is drawn around
  where the hole is, roughly; rounded-corner masks cut a little picture off. [I]
- If MobileGestalt's scale and QuartzCore's differ (the fixups not applied at 3, or `--set scale-fixup=0`),
  UIKit lays out for one and the render server draws for the other: a UI that fills a quarter more or less of
  the screen than it should. The first SpringBoard frame at each scale shows which.
- 19 % more pixels than the iPhone frame per frame, and twice the frames: the helper's copy, the CPU-copy
  path of the framebuffer and the GPU's fill rate all scale with it.

Only at 3:

- It depends on the two text fixups, which exist for builds 24A446 and 24B156 only and have not been seen applied. Without
  them QuartzCore runs the display at 2.
- The least risk to layouts: iPhone assets, fonts and metrics are all made for 3; only the point size is new.

Only at 2:

- It is QuartzCore's own answer (no fixup), and the path Droid-VM runs on the iPad.
- No current iPhone is @2x. The OS image of an iPhone18,2 carries @3x phone artwork; @2x lookups fall back to
  other variants or scale them [I]: softer or wrongly sized glyphs and icons are possible, and
  `artwork-scale-factor` 2 with subtype 2868 names a set that does not exist.
- 720 points is wider than any iPhone and nearly an iPad mini's 744. UIKit takes size classes from the idiom
  and the window, so a phone stays compact in portrait [I], but apps that switch layout on width (two columns
  past some width) will; SpringBoard's grid has room for more columns than its phone layouts define ([U] what
  it does with it).
- Touch targets are 4.4 mm; text is 61 % of its size on an iPhone. Dynamic Type or Display Zoom in Settings
  are the guest's own remedies.

Whether 3 is "more correct" than another value: 3 is the only scale an iPhone identity is built for, and
480x1040 is the closest a whole-number scale gets to the Pro Max's 440x956 at this panel's size; a fractional
scale (3.27 would give 440 points) is not something UIKit has. So 3 stays the default.

## 6. Boot contract: what the booter and kernel must provide

`boot-contract.json` (schema 1). The booter builds the device tree from `adt`; the kernel implements
`launch_policy`, `addon_mounts` and `kernel_features`.

- `adt.root`, `adt.chosen`, `adt.product`, `adt.buttons`: values to set. The rest of `/product` is copied
  from the IPSW's v54ap tree, less `name`, `AAPL,phandle`, `exclaves-enabled`, `has-exclaves`,
  `artwork-display-gamut`, and with `APPLE2` and `MTL2,2` taken out of `graphics-featureset-fallbacks`
  (droidvm `make-droidvm-adt.py`: otherwise the app switcher's cards are blank: SplashBoard wrote them as ATX). xnuhost's
  loader keeps both at `astc=1` (2026-11-25), with SpringBoard's snapshot format held to JPEG or PNG instead
  (`userland/metal/SnapshotFormat.inc`, `oneios-snapshot-astc`).
- `adt.required`: properties that need a value or the guest stalls: `unique-chip-id`, the three MAC
  addresses (MobileGestalt waits 5 s per answer without them), `manifest-properties` (cryptexd), a `wlan`
  node, `/bluetooth` transport-encoding 10, `/defaults/vmm-present`.
- `adt.absent`: `baseband`, and `filesystems/fstab` (section 10: with Apple's node the boot stops in
  mount-phase-2).
- `display`: the framebuffer to present (the panel's native mode, the scale and the points it gives), and
  `scale_note` (what QuartzCore does with the scale; section 5.1).
- `launch_policy.disable_jobs`: the labels. `launch_policy.override`: where they are in force from. The seed
  carries `db/com.apple.xpc.launchd/disabled.plist` and the kernel's unpack of the seed sets its owner (root)
  and mode; the kernel's launch policy writes nothing itself (`kernel/patches/oneios-hosted/0004-README.md`,
  step 5). The report's line "the kernel writes the same override" is from before that was decided; its wording
  stays until the seed changes for another reason, because the report is a file of the seed and a seed with
  another hash makes every Data store start over.
- `launch_policy.service_manager`: start `/private/var/oneios/bin/oneios-init` once and give its task
  launchd's bootstrap port (droidvm `spawn_service_manager`, `droidvm_fix_ownervisor_bootstrap`).
- `launch_policy.force_enabled`: disabled jobs that will run anyway.
- `addon_mounts`: read-only bind mounts after Data is mounted (the Metal plugin over `/System/Library/Extensions`:
  `kernel/patches/oneios-hosted/0031-gpu-addon-mount.patch`, on without a boot argument, `docs/gpu.md` 5.1; the
  capture plug-in's folder over `/System/Library/MediaCapture`: `0056-camera-plugin-mount.patch`).
- `app_cryptex`, `staged_apps`: the two views of section 3.
- `kernel_features`: launch policy, service manager, no launch constraints for owner binaries, owner
  trust module, data-protection class in an xattr, the key-store stand-in (no passcode, wrap/unwrap),
  the software AES service, MobileGestalt cache drop, text fixups.

## 7. Guest helpers (`userland/helpers`)

| Helper | Job |
|---|---|
| `oneios-init` | Our service manager outside launchd. Reads `services.d/NAME.conf` (`arg=`, `env=`, `uid=`, `keepalive=`, `throttle=`, `bootstrap=`), starts what `enabled/` names, restarts with a throttle, logs to `log/`, writes `state/status`. Ignores a service file that others can write |
| `oneios-bootstat` | `ONEIOS-GUEST stage=backboardd-up|springboard-process|springboard-up ...` on the console and in `state/boot` |
| `oneios-mg` | Asks MobileGestalt and compares with `identity.expect` (ProductType iPhone18,2, DeviceClass iPhone, main-screen 1440x3120 at the image's scale, subtype 2868) |
| `oneios-thermal` | Service `thermal`: copies the kernel's `OneiOSThermal` `ThermalPressureLevel` (the phone's thermal pressure, `drivers/thermal`) into the notify state `com.apple.system.thermalpressurelevel` every 5 s, and sets it again when thermalmonitord (which has no sensors here) changed it. `ONEIOS-GUEST thermal level=N name=... step=... case=... soc=...` on the console |
| `oneios-seed` | First start: unpacks `data.seed` (pax tar) into the new Data volume as root, with owners, modes, links and extended attributes; marks it with the seed's SHA-256; refuses `..`, absolute paths, links out of the volume, devices and hard links |

Built by `helpers/build.sh` for `arm64e-apple-ios27.0` with the toolchain of `scripts/ios` (`docs/building.md`
section 9: Xcode's on macOS, the cross toolchain on Linux), linked against the guest's iOS 27 stubs and signed by
`oneios-tool ios sign` with 4 KiB pages and the `no-container` entitlement (XML and DER). Built on the Mac on
2026-11-12 (Xcode 27.2): all four pass `macho-check.py` (`oneios-thermal`, added the same day, builds and signs; not checked with `macho-check.py`, not run in a guest). `oneios-bootstat` declares libproc's two calls itself (the
iPhoneOS SDK has no `libproc.h`; 24A446's libSystem exports both). **Not in any image yet:** with helpers in the seed
the kernel starts `oneios-init`, which changes every boot, so the Mac build went to `out/userland/helpers-mac`, which
the hook does not read (it reads `out/userland/helpers`, or `ONEIOS_HELPERS`). arm64e because iOS 27
kills arm64 platform binaries at exec (ownervisor `build.py`). A debug shell (dropbear) is not built here;
`helpers/README.md` says where to take one (`external/ownervisor`, `external/applebox`, droidvm
`ipados27/scripts/add-ownervisor-data.py`).

### 7.1 The Metal plug-in

Built by `userland/metal/build.sh` on macOS (Xcode) or Linux (`docs/gpu.md` section 7 has the pieces, the pins, what
differs from upstream and the sizes and CDHashes of each host's build), placed by this part. The build leaves `out/userland/NativeAdrenoMetal.bundle` (`Info.plist`,
`NativeAdrenoMetal`, `libvulkan_freedreno.dylib`, `oneios-target`) and beside it `NativeAdrenoMetal.build.json`:
the SDK used, the source pins, each binary's size and the CDHash the build signed it with. The hook picks the
bundle up, reads each Mach-O's CDHash from its signature into `owner-trust.json`, and compares them with the build
record: a bundle changed after its build stops the image build. The manifest's `tweaks.info` gets `metal_plugin` (placed,
target, CDHashes, SDK). The same stack passes MetalCheck 41/41 on an SM8650 / Adreno 750 phone under iPadOS 27.0.1
[F, references]; here nothing has loaded it.

**It must not be staged as upstream builds it.** Read in `external/NativeMetal/NativeAdrenoMetalPlugin.mm`:
the plug-in writes QuartzCore's options at fixed addresses of the iPad17,3 24A446 shared cache, taken
relative to `CARenderServerStart` (0x1846dce88), with no check of which cache is loaded. In the iPhone's
cache the same writes land on other memory of backboardd. So `roottweak` places a bundle only if it holds a
file `oneios-target` with `iPhone18,2 24A446`, which whoever re-finds the addresses for the iPhone cache adds
(`userland/metal/build.sh` does, after rewriting the addresses from `userland/metal/hooks/iPhone18,2-24A446.json`;
`docs/gpu.md` section 7 has the table and how each was found); otherwise the line is `off` and the guest renders on the CPU (`--metal-plugin-unchecked`
overrides). Also read there: `<Name>.publish` and `<Name>.primelist` are looked up beside the bundle, which
is where they are placed; `<Name>.clients` is read by the kernel's GPU driver, not the plug-in; and the
plug-in's debug switches are fixed paths under `/private/var/ngpu/debug`, not under `/private/var/oneios`.
GPU tests once it runs: droidvm `package/metalcheck`, `gfxprobe`, `gfxbench`.
Where the bundle comes from: on a PC `--metal-plugin` (`scripts/image/build.sh`: `out/userland/NativeAdrenoMetal.bundle`); on
the phone (Make iOS image, `serve make-image`, since 2026-11-24) the staged runtime's `lib/guest/` copy, with the
capture plug-in, when its `oneios-target` is the image's ([image-format.md](image-format.md), "The Metal plug-in of an
image made on the phone").

**It also carries backboardd's brightness mode** (since 2026-11-12): `build.sh stage` appends
`drivers/brightness/BacklightMode.inc` to the staged `NativeAdrenoMetalPlugin.mm`. In backboardd only, its constructor
gives CoreBrightness's `-[BrightnessSystemInternal initBLControl:]` mode 1 (BLControl) where vmm-present selects 3
(VMBLControl, which never reaches the backlight service). It runs because the plug-in loads on backboardd's main thread
before that thread instantiates BrightnessSystem; a session without the plug-in keeps VMBLControl. `oneios-bl-mode=0`
in the kernel's boot arguments turns it off. Why and what to look for: `drivers/brightness/README.md`.

### 7.2 The Galaxy A56's guest Mesa (`userland/metal-amd`, iOS 16.5)

For the A56 only (20F66, arm64; nothing of it is in an S24 Ultra or iOS 27 image). `userland/metal-amd/build.sh`
builds Mesa's radeonsi (ACO, no LLVM) over the amdgpu virtio native context and a Darwin vdrm backend that speaks to
the guest kernel's accelerator user client (open type 0x4149, `DroidVMVirtgpuABI.h`), into `out/metal-amd/ios`:

- `liboneios_radeonsi.dylib`: exports `oneios_radeonsi_screen_create(why, why_size)` alone (the radeonsi
  `pipe_screen`, K3 of [a56-gpu.md](a56-gpu.md)); install name `@loader_path/`. The A56's Metal plug-in does not load it: its
  `libOneiOSRadeonAir.dylib` compiles the same target's sources in (section 4 of a56-gpu.md), so the bundle carries
  one library with radeonsi in it.
- `oneios-amdgpu-probe`: a lasting command-line tool for a root shell on the A56: the device, the capset's GPU facts,
  a guest BO, a GFX NOP IB and its fence, one `PASS`/`FAIL` line each; `--screen` adds the radeonsi screen and a
  clear read back.

Both are arm64, minos 16.5, signed by `oneios-tool ios sign`, checked against the 20F66 stubs. Every process that
opens the GPU logs `vdrm-darwin: connected: ...` to iOS's unified log, and every screen `oneios-radeonsi: screen
"..."`. **Not placed in any image yet and not run in iOS**: the image hook does not know them; the Metal stage places
the dylib with its bundle. Where they come from, what is stubbed and the host tests: [a56-gpu.md](a56-gpu.md)
section 3; the build: [building.md](building.md) section 9, "The A56's guest Mesa".

### 7.3 The Galaxy A56's Metal plug-in (`userland/metal-amd`, iOS 16.5)

`OneiOSRadeonMetal.bundle`: Jesvi-iOS a13e406's Metal device and AIR Metal layer (MIT; `userland/metal-amd/NOTICE`
lists every file taken and changed) for the A56's 20F66 image, built by `userland/metal-amd/air/build.sh plugin`
([building.md](building.md) section 9). The bundle holds `Info.plist` (`CFBundleExecutable` OneiOSRadeonMetal,
`NSPrincipalClass` OneiOSRadeonMetalDevice), `OneiOSRadeonMetal` (the Objective-C device, arm64, iOS 16.5),
`libOneiOSRadeonAir.dylib` (the AIR frontend, Mesa's Gallium and, from the guest Mesa stage's build, radeonsi; loaded
with `dlopen` from beside the executable) and `oneios-target` (`iPhone10,3 20F66`). Metal finds it through the
accelerator the kernel publishes on an amdgpu host (`MetalPluginName` OneiOSRadeonMetal,
`MetalPluginClassName` OneiOSRadeonMetalDevice: [a56-gpu.md](a56-gpu.md) section 5).

Which processes get a device (the plug-in decides, in `-initWithAcceleratorPort:`; a process it declines gets nil, no
device, and keeps the road it has without one, as with NativeAdrenoMetal): every process but `backboardd`,
`mediaserverd`, `PosterBoard`, `AegirPoster` and `WeatherPoster` (Jesvi-iOS's own list of processes kept off this
Metal: [a56-gpu.md](a56-gpu.md) section 4 says why backboardd is). `OneiOSRadeonMetal.clients` beside the bundle (one
process name a line, `*` for all), when it exists, decides instead. `oneios-radeon-backboardd=1` in the boot
arguments gives backboardd the device (a test of the render server on this layer; not shown to work).
`oneios-metal-profile=N` sets the feature profile (Jesvi's `metal_profile=`; default 1).

Its log: os_log subsystem `com.oneios.radeonmetal` (`oneios-tool phone pull-log`), and for its errors, the device's
start and the first two pipelines a kernel text row in `xnuhost.log`: `oneios-gpu-text: PROCESS[PID] RM ...` (hosted
0039's rows; the accelerator's class must start with `AGX` for the kernel to log them).

**Not placed yet.** The image hook (`oneios-tool image tweaks`) places `NativeAdrenoMetal.bundle` for the S24 Ultra
image; no tool places this bundle in the 20F66 image, and the kernel mounts the addon as for the Adreno bundle (hosted 0031), and since hosted 0108 names the
A56 bundle in its lines (`oneios-gpu-addon: .../OneiOSRadeonMetal.bundle/Info.plist ...: visible`, the dyld lines of
`OneiOSRadeon*` files, the clients file named after the published plug-in; a56-gpu.md section 5). `oneios-tool ios gpuaddon` already keeps its name
(`addons/OneiOSRadeonMetal.bundle/...`, `addons/OneiOSRadeonMetal.publish`).

### 7.4 The Galaxy A56's OpenGL ES (`userland/metal-amd/gl`, iOS 16.5)

`OneiOSRadeonGL.bundle` is NegroniGL of the owner's iOSPlus10 (MIT; `userland/metal-amd/gl/NOTICE` lists every file
taken and changed), ported to 20F66 and to the A56. It is Mesa's GLES frontend over the A56's radeonsi (the same K3
screen as the Metal layer, compiled in), built by the guest Mesa stage's hook `userland/metal-amd/gl/build.sh`
([building.md](building.md) section 9). Its purpose: backboardd's CoreAnimation composites on the GPU through its
OpenGL ES renderer ([a56-gpu.md](a56-gpu.md) section 4a).

The bundle holds:
- `Info.plist` (`CFBundleExecutable` OneiOSRadeonGL, `com.oneios.radeongl`, MinimumOSVersion 16.5);
- `OneiOSRadeonGL`, an MH_BUNDLE, arm64, iOS 16.5. It exports libGFXShared's 129 GLD names, the 18 GLI names
  OpenGLES's `eagl_init` binds, `gliTerminateLibrary` and `oneios_gl_probe`;
- `oneios-target` (`iPhone10,3 20F66`).

How iOS finds it:
1. libGFXShared reads the accelerator's `IOGLESBundleName` (`OneiOSRadeonGL` with the kernel's `oneios-gpu-amd-gl=1`,
   which xnuhost adds when the gpuaddon carries the bundle).
2. It loads `/System/Library/Extensions/OneiOSRadeonGL.bundle/OneiOSRadeonGL` (hosted 0031's union of the gpuaddon's
   `addons/`).
3. The bundle points OpenGLES's GLI table at itself (20F66's addresses, checked before they are written).
4. Every EAGLContext of that process is then Mesa's.

Which processes get it (the bundle's gate, once per process):
- every process, unless `OneiOSRadeonGL.clients` beside the bundle (`*`, `NAME`, `-NAME`) says otherwise, or
  `OneiOSRadeonGL.bundle/off` exists (every process closed);
- the screen is radeonsi. Where radeonsi does not open, softpipe (the CPU) stands in for every process but backboardd
  (CoreImage needs a context, [gpu.md](gpu.md) 5.2), and backboardd keeps CoreAnimation's CPU renderer;
- `oneios-gl-softpipe=1` in the boot arguments gives backboardd softpipe too (a test of the EAGL road), and `=0` gives
  no process softpipe;
- backboardd runs a preflight (a triangle and an IOSurface drawn and read back) before it is handed a context.
A closed gate gives the process nil EAGLContexts, as before the bundle.

Its log:
- os_log subsystem `com.oneios.radeongl` (`oneios-tool phone pull-log`), records `SEV CAT message`;
- in `xnuhost.log`, `oneios-gpu-text: PROCESS[PID] RG ...` rows (hosted 0039) for its errors and the first lines of the
  gate, the screen, the contexts, the drawables and the first frame;
- the rate once at the 600th frame, then every 3600th (`GLI 600 frames, N a second since the first; copies: ...`).

The lasting tool `oneios-gl-probe` (built beside the bundle, `$METAL_AMD_OUT/gl/oneios-gl-probe`) loads the bundle in a
shell and prints one PASS/FAIL line: the screen, a triangle, an IOSurface round trip.

**Placed by** `oneios-tool ios gpuaddon OneiOSRadeonMetal.bundle OUT --gl OneiOSRadeonGL.bundle` (and
`userland/metal-amd/air/build.sh addon` when the GL bundle is built). The image itself is not changed.

## 8. What launchd, backboardd and SpringBoard need

| Stage | Needs | Who |
|---|---|---|
| launchd starts | The OS and SystemOS volumes mounted as Apple's root; the dyld cache; AMFI with Apple's three trust caches. Boot tasks with RequireSuccess (fsck, mount phases, data protection, launchd_cache_loader) must succeed: nothing here touches them | kernel, booter |
| Data volume | First start: `newfs_apfs -e -x -v Data` on the store, mount at `/private/var`, the seed unpacked **in the kernel** with its owners and modes, then launchd (`0004-README.md`; booted). The user-space unpacker `oneios-seed` is not used; the hook still places it in `overlay/userland/bin` when the helpers are built. No `/filesystems/fstab` in the tree and no Overprovision volume: section 10 | userland (the seed); kernel (the rest) |
| launchd settles | `disabled.plist` in force (booted: section 10); `unique-chip-id`, MAC addresses, `manifest-properties` in the tree; software AES (or configd waits 5 minutes); no watchdogd | kernel, booter; this part's list |
| keybagd, securityd | Key-store stand-in: no passcode, wrap/unwrap for the keychain; empty `keybags/`. In the hosted kernel since `0022-keystore.patch` (`kernel/patches/oneios-hosted/keystore/README.md`: class keys from a constant, AES key wrap in software, the protection class of a Data file in an extended attribute; **not a security boundary**) | kernel; `data-layout` |
| backboardd | The original IOMobileFramebuffer with a host display behind it; a Digitizer/Touch Screen HID service; the CA preferences of 3.1; backlight properties (iOS 26.5's CoreBrightness crashed on missing `backlight-calibration-parameters`; [U] on 27) | kernel, booter; `preferences` |
| SpringBoard | Identity = iPhone (`oneios-mg` checks it); Setup skipped; migration recorded; watchdogs relaxed; CommCenter and sleepd answering; PosterBoard's store (data-protection class); staged apps and a LaunchServices database that names them (built at the store's first boot, kept from then on by hosted `0043` when it names every app of `/Applications`; a recovery file `com.apple.LaunchServices.error` that a killed session left beside it is removed before launchd by `0060`, or lsd would throw the kept database away, register every app again and so make iconservicesagent draw every icon again; `oneios-ls-state:` and `oneios-icons-state:` lines say what both stores held at boot); the drawn-icon store `com.apple.IconsCache/<UUID>` kept too (iconservicesagent empties it only when its `__cache_info__` names another build or language, the folder is not `_iconservices`' or has an ACL: read in 24A446's iconservicesagent, `-[ISMutableIconCache initWithURL:]`); @3x | all; `preferences`, `launchd-jobs`; kernel |
| Home screen with GPU | Metal plugin placed and trusted; clients list; kernel accelerator publishing `MetalPluginName` | plugin built and placed (`userland/metal`), not run; kernel |

## 9. What is still missing for SpringBoard

1. Done: the run on the real root (all 142 labels of the default policy are in the iPhone's cache) and a PC boot with it (section 10).
2. iOS builds of the helpers (`helpers/build.sh` moved to the toolchain of `docs/building.md` section 9), and their CDHashes in the kernel's owner trust module. Until then the kernel's line `no /private/var/oneios/bin/oneios-init in this seed: no service manager` stays. It is about our own helpers (boot stages on the console, the MobileGestalt check, a debug shell), not about the daemon policy.
3. Done by the kernel: the first-start Data path (`0004`), with the seed unpacked in the kernel.
4. The scale: read and wired (section 5.1), not yet seen. @2x is QuartzCore's own answer; @3x relies on two text fixups. The first SpringBoard frame decides both, and the layout risks of a 480x1040 or 720x1560 point screen.
5. Kernel features of section 6 for this profile (release-kernel, booter).
6. CommCenter without a baseband: in the PC boot launchd refused `com.apple.CommCenter.plist` (`_LimitLoadToDeviceTree baseband`: "cannot be loaded on this hardware"), loaded `com.apple.CommCenter-noncellular.plist` under the same label and CommCenter ran with no exit in the 5 minutes the log covers. Longer runs and SpringBoard's use of it: [U].
7. The Metal plug-in is built for the iPhone's cache and placed (section 7.1), but no boot has loaded it (`docs/gpu.md` sections 5.1 and 7: the kernel side is linked; a boot needs an image set with the bundle and a kernel that trusts its two CDHashes). Until then CPU rendering.
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
oneios-tool image tweaks boot-check --data MNT/root --root OS_MNT/root   # OS_MNT: apfs-fuse -o ro out/image/24A446/os.img OS_MNT
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


## 11. What else costs performance or reliability (audit of 2026-11-22)

Inputs: the session folder `out/session-logs/2026-11-22/kbd-debug/` (not in git): iOS's log of the boot
`336E41F9DB7E414ABD4233A338BB9398` (262923 entries, 47.6 min once the times are right: the first item below),
`xnuhost.log` (that boot), `xnuhost-s3.log` and `live5.log` (a later session, host time 7205-8340 s), the
MobileGestalt cache `mg/mg.plist`; the iPhone18,2 tree `out/kernel-inputs/24A446/devicetree.bin` (read with
`xnuloader-inspect adt`, not committed); the root's `System/Library/xpc/launchd.plist` (`oneios-tool image apfs-copy`
of `os.img`). The cache's keys are obfuscated: a key is the first 22 characters of base64(MD5("MGCopyAnswer" + name))
(checked: `ArtworkTraits` gives `oPeik/9e8lQWMszEjbPzng`). A process start shows in the session logs as one
`triggered unnest of range ... of DYLD shared region` line.

| Item | Evidence | Cost | Fix |
|---|---|---|---|
| `ioslog` read the phone's times 52 times too slow | The decoder library converts ticks only for the timebase 125/3 (24 MHz); the phone's is 625/12 (19.2 MHz). The kernel's 10 s `oneios-cpu` lines were 0.19 s apart; `health` said the boot lasted 55 s | every gap, rate and "when" read from iOS's log was wrong (counts were right) | **Done**: `oneios-tool ioslog` scales the times itself (`tools/oneios-tool/README.md` "ioslog"); backboardd's own clock and the 10 s lines now agree with the entries' times |
| Background I/O throttled as on a rotating disk | Each session: `spec_open : ... isssd is (0)` for every hosted disk. XNU then uses periods of 40/85/200 ms (tier 1/2/3) in windows of 25/100/500 ms; the iPhone's NAND gets 5/15/25 ms and its `/defaults` sets `kern.io_throttle_period_tier3` 20, `kern.io_throttle_window_tier3` 100 | a utility or background I/O (indexers, `assetsd`, SQLite writers) slept up to 85 or 200 ms each while a foreground I/O was within the window; a lock they hold is held as long [I: not measured] | **Done**: `/defaults` has the iPhone's effective values (`runtime/loader/src/machine.c`, loader test). **Proposed**: the hosted disk publishes `Device Characteristics` = {`Medium Type` = `Solid State`} (`kernel/patches/oneios-hosted/storage/OneiOSHostedDisk.cpp`), so DKIOCISSOLIDSTATE answers 1 for APFS and the other askers too (kernel build needed) |
| `aonsensed` respawn loop | `live5.log`: 21 starts from 7790 s, every 10-20 s. iOS's log: `exited due to exit(0)`, then `Successfully spawned aonsensed ... because event publisher` at once; its job publishes `com.apple.proximity.launch` (deviceaccessd subscribes). wifid and locationd: `AONSense.isAvailable,false` | an xpcproxy, a dyld launch and a MobileGestalt cache load each 10-20 s | **Done**: daemon policy group `loops`; hosted 0053's default list for an existing Data store |
| `prngseedd` (`seputil --daemonize-reseed`) respawn loop | `live5.log`: 16 starts, about every 10 s; `Successfully spawned seputil because xpc event` (activity `com.apple.prngseedd.reseed`, interval 1800 s). No SEP here | the same per start | **Done**: as above |
| HWModelStr `oneios,xnuhost` | cache `/YYygAofPDbhrwToVsXdeA` = `oneios,xnuhost` (also `GGIIDN/ANr8X2WrgS6nBYQ`); audiomxd `Device tuning directory not found: /Library/Audio/Tunings/oneios,xnuhost`, `Cannot find preset file oneios,xnuhost/Sidetone/...`; atc `deviceModel=oneios,xnuhost` | per-board tables are not found (audio tunings, VAD strips) | Not changed: the board name stays out of `compatible` on purpose (`docs/hardware.md`, "Why not the board name"); hosted 0078 links only the haptics folder (since 0086 also the Actuator's VAD strip, `oneios,xnuhost/VAD/d42_actuator_general` -> `AID8022/VAD/actuator_general`: docs/hardware.md section 5). This boot's audiomxd still had no `/Library/Audio/Tunings/oneios,xnuhost`: check that its kernel had 0078 |
| marketing name `OneiOS guest` | cache `marketing-name` (`Z/dqyWS6OZTRy10UcmUAhw`) and `PhysicalHardwareNameString` (`bbtR9jQx50Fv5Af/affNtA`) = `OneiOS guest`; the iPhone tree's `product-name` is `iPhone 17 Pro Max` | Settings' About; apps that read the name | Proposed (the no-identity `/product`, the parent's) |
| `/product` keys of the iPhone the guest lacks | not in xnuhost's tree without an identity: `oled-display` 1, `display-corner-radius` 8 bytes (u32 62, u32 1: the Pro Max's 62-point corners [I]), `island-notch-location` 144, `device-perf-memory-class` 12, `compatible-device-fallback` `iPhone17,2`, `supports-burnin-mitigation` 1, `thin-bezel` 1, `ui-background-quality` 100, `ui-weather-quality` 100, `display-workgroup-interval` 1, `hid-workgroup-interval` 1, `unique-model` `V54AP` (given since 2026-11-22 with the cameras, as "OneiOSAP" since 2026-11-23: with "V54AP" AVGestalt's webcam answer stopped all audio, docs/hardware.md section 7), `sub-product-type` and `fdr-product-type` `iPhone18,2` | per key: corner mask, Dynamic Island, cache sizes by memory class [I] | Proposed (the parent's; values in the report of 2026-11-22). The S24 Ultra's own corner radius is [U]: the display helper can read it from Android (`Display.getRoundedCorner`) |
| DeviceColor, DeviceEnclosureColor `unknown` | cache `mumHZHMLEfAuTkkd28fHlQ`, `JhEU414EIaDvAz8ki5DSqw` = `unknown` (iBoot fills them from syscfg; there is none) | colour-keyed defaults [I] | Proposed, low |
| analyticsd retries triald | 23 failed lookups of `com.apple.triald.namespace-management` by analyticsd; 45 `[TrialStateRelay] No sandbox extensions present, attempting to retry`; 138 `Namespace resolver file not present`; triald is in group `base` | analyticsd 10-147 % in the `oneios-cpu` lines; the retry's share [U] | Proposed: an A/B boot with `oneios-enable-jobs=com.apple.analyticsd,com.apple.triald` |
| MobileMail without maild | 1213 errors `EMRemoteConnection ... will re-attempt on daemon re-launch`, 1211 of them in one minute (15:08); Spotlight 5 failed lookups of `com.apple.mobilemail.services.xpc`; maild is in `base` | each send fails at once (no timeout) | none now |
| passd without nfcd | 136 `Failed to re-register with nfcd`, 119 `Unexpected connectionID`; nfcd in `base`, passd kept | fast failures | none now |
| MobileAsset short-term locks, errno 2 | 351 for `com.apple.linguisticdata`, 60 `AppleDepth.MLModel`, 52 `siri.tts`, 11 `search.queryparser` (kbd, Spotlight, cameracaptured, sirittsd, wifid); the file is `/private/var/MobileAsset/AssetsV2/locks/com.apple.UnifiedAssetFramework/<set>/shared_locks/atomic_instance_latest.locker`, a link mobileassetd makes for a downloaded asset set | each lock fails at once; the keyboard's path: `ioslog keyboard` | Not a seed fix: a link with no asset behind it would be a lie to the clients |
| chronod cannot write HEIC | 116 `CMPhotoCompressionSessionAddImage ... (codec: 'hvc1')`, 105 watchdog kills by chronod | blank widgets, retries | Known: the encoder bridge (`docs/hardware.md` 7g), off by default |
| runningboardd `setGPURole ... nil IOGPU device ref` | 123 | fast failures | the GPU path (`docs/gpu.md`) |
| backboardd `swap_wait ... = 0xe00002d5` | 230 lines, all in the first seconds of the boot | boot only | the display path |
| AttentionAwareness in backboardd | 5002 `notify client` lines in 8 minutes (2967 in one), to HomeGrabberView, AppSuggestionManager and sharingd; SpringBoard 2087 `attention client lost event tag is not a number`; events reached the clients 16-26 s after their own time | backboardd's CPU [I]; the delay looks like a result of backboardd's load, not its cause [I] | Proposed: look at it after the asset catalog fix |
| Wi-Fi scans | `scans asked 20 (started 4, reused 16)` | none | nothing to do: the driver reuses scans |
| cfprefsd `Couldn't open ... No such file or directory` | 4287 in the first 11 minutes of the boot, from 791 a minute down to 16 | first reads of absent plists | nothing to do |
| Most threads on the two E vCPUs (s9, 2026-11-22) | `perf: cpuN:` vCPU 0-1 0% idle, vCPU 2-7 85-99% idle in every period; the render server at 20-32 frames a second with 1.4-5.6 ms of GPU time; iOS's log (`s9b-ioslog`): SpringBoard 90 hangs (214.9 s), chronod 126 watchdog kills of widget extensions (`getAllDescriptors`, timelines) | launch and Control Center lag, blank widgets | Hosted `0085`: the Edge scheduler's E -> P edges opened (kernel/patches/oneios-hosted/README.md, "0085") |
| Fitness's first screen is black for about 80-90 s with no network | SpringBoard `scene content state changed: ready` (a first frame), Fitness `Waiting to show onboarding until initial queries complete`; fitcored's Fitness+ queries through AppleMediaServices fail slowly with no network and no storefront (`queryActiveWorkoutPlan ended in 82.3 s`, `fetchServiceSubscription ended in 93.3 s`, `AMSErrorDomain Code=205`), then `is presenting onboarding: 1`. Its failed lookups (amsengagementd, itunescloudd's in-app messages, fairplayd: groups base and hardware) end at once (`error = 3`) | the app looks dead at first launch | Not a render or policy fault. `0085` takes the CPU part out (fitcored and healthd at default and utility QoS were on the E vCPUs); with Wi-Fi the bag loads can answer (not tested) |
