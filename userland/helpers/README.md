# Guest helpers

Small programs of ours that run inside the iOS guest. Source only; built by `build.sh` with an iOS SDK
(Xcode on macOS, or stock clang with theos' ld64, ldid and SDK on Linux: the toolchain of iOSPlus10's
`userland/ssh/build.sh`). The Linux build has not been run: this machine has no iOS SDK.

| File | What | Host test |
|---|---|---|
| `oneios-init.c` | Starts and supervises our services outside launchd (the role of Droid-VM/ownervisor): `services.d/NAME.conf`, `enabled/NAME`, logs, a status file. Waits for launchd's bootstrap port when the kernel started it | `tests/test_helpers.py` (cc build) |
| `oneios-bootstat.c` | Prints `ONEIOS-GUEST stage=...` on the console as backboardd and SpringBoard come up, then lock and screen changes | the same |
| `oneios-mg.c` | Asks MobileGestalt for the identity and checks it against `identity.expect` (iPhone, @3x) | none: needs CoreFoundation |

`roottweak apply --helpers DIR` installs every signed Mach-O of DIR in `/private/var/oneios/bin` and lists its
CDHash in `owner-trust.json`. The guest runs a binary only if the kernel's owner trust cache has that hash.

## A debug shell

Not built here. Two sources that fit, both signed for the same trust path:

- [Droid-VM/ownervisor](https://github.com/Droid-VM/ownervisor) `build.py` with
  `DROIDVM_IOS_TARGET=arm64e-apple-ios27.0` (macOS): dropbear 2022.83, zsh, applebox, debugserver.
- iOSPlus10 `userland/ssh/build.sh` (Linux or macOS): dropbear 2026.94, dash, toybox; change its target to arm64e.

Copy `dropbear` (and the shell it logs into) into the helpers output directory before `roottweak apply`:
`services.d/dropbear.conf` starts it on port 2222 (`-R`: host keys made at first use) when the binary is there.
Keys go in `/private/var/root/.ssh/authorized_keys`, which the image build may add to the Data tree.
