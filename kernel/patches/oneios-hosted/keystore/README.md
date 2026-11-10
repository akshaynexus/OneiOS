# kernel/patches/oneios-hosted/keystore: the AppleKeyStore service in software

Owner: q5-keystore (job Q5 of the board's queue of 2026-11-11). With `0022-keystore.patch`.

**This is not a security boundary.** The class keys are derived from a constant in `OneiOSKeyStore.cpp`. Whoever
can read the Data store (`data/data.img`) can unwrap every key this service wrapped, and no file on the Data volume
is encrypted by the guest. The store is protected by what One UI gives the file (the phone's own file-based
encryption of `/data`), nothing more. There is no passcode, no lock and no Secure Enclave. It exists so that iOS's
daemons find the service they expect and the keychain can store items.

## Why

The S24 Ultra has no Secure Enclave, and the collection's `com.apple.driver.AppleSEPKeyStore` (the real
`AppleKeyStore` class, a SEP client) is left out of `kernel.kc` (`kernel/prelink/roots.py`, `FORCE_OUT`). Without a
service of that name, launchd logged `failed to find AppleKeyStore service` and its keybag boot task
`MKB_INIT: FATAL KEYBAG ERROR: failed to load system bag`.

**AppleSEPKeyStore must stay out of the roots.** This service registers a class named `AppleKeyStore`; with the
original in the collection the name is taken, `oneios_keystore_start` says so and starts nothing. Nothing in the
collection's closure links against the original kext (`kernel.kc.prelink.json` "closure"), so nothing else changes
in the prelink.

## What it is

| Piece | From | What |
|---|---|---|
| `AppleKeyStore` (IOService on IOResources) and `AppleKeyStoreUserClient` | UniXNU's **AppleKeyStoreState**, `NativeKeyStoreEvents.cpp`, BSD-2-Clause-Patent, at the kernel base's submodule commit (`external/Droid-VM-UniXNU/drivers/AppleKeyStoreState`, 42ff83e), compiled with two lines added | The matching class MobileKeyBag's event registration looks for, and the answers to libaks's state queries |
| `OneiOSKeyStore.cpp` | here, after Droid-VM's `package/KeyStoreProbe.cpp` and `KeyStoreWrap.inc` | the start, the class keys, wrap and unwrap, the hooks the component calls, the log lines |
| `build-oneios-keystore.py` | here, after `aes/build-oneios-aes.py` | `build/oneios-keystore/liboneioskeystore.a` in the kernel build tree |
| `0022-keystore.patch` | here | `oneios_keystore_start()` called from `oneios_board_late_start` (`osfmk/arm64/oneios_host.c`); `owner-dataprotect` on by default (`bsd/vfs/kpi_vfs.c`) |

Unlike the software AES, the two classes subclass `IOService` and `IOUserClient` of the kernel itself: no binding to
an original class of the collection is generated, and nothing depends on the kernelcache's addresses.

What the guest gets:

- **A system keybag that is always loaded, with no passcode.** `aks_get_system` (selector 14) answers handle 0;
  keybagd needs no `systembag.kb`, and `/private/var/keybags` stays empty (`docs/userland.md`, tweak `keybag`).
  The lock state (selector 7) is "unlocked since boot"; the device state (17, 35) is the DER set libaks parses,
  with `sls` = 3 (MobileKeyBag's "no passcode set", Droid-VM's iPadOS 27 value; boot argument `oneios-aks-sls=N`
  for another). Selectors 0, 4, 5, 9, 12, 13, 26, 27, 55, 101 and 102 succeed with no output.
- **Class keys.** One 256-bit key per protection class (a class is the low five bits of the number libaks passes:
  A to D are 1 to 4, the keychain's classes 6 to 12), made at start as AES-256 of a label and the class number under
  the constant root key. The same on every boot and every phone: keys wrapped on one start unwrap on the next.
- **Wrap and unwrap** (`aks_wrap_key` selector 10, `aks_unwrap_key` selector 11; scalars in: keybag handle and
  class; the key in and out as structures; for 10 the class as the scalar out). AES key wrap with padding
  (RFC 5649) under the class's key, in software through the kernel's `libkern/crypto/aes.h` (corecrypto). Keys of
  1 to 512 bytes; a wrapped key is the key rounded up to 8 bytes plus 8, the size a real one has (40 bytes for a
  32-byte key). Unwrapping with another class, or changed bytes, fails with `0xe00002bc`. The start runs a test
  (wrap, unwrap, wrong class, changed byte, three lengths) and starts nothing if it fails.
- **The Data volume's protection classes** (UniXNU's `owner-dataprotect`, `ipados27-bringup.patch`, already in the
  board series): the Data volume is an unencrypted APFS volume, where APFS keeps no class. The class a caller sets
  on a file or folder is kept in the extended attribute `com.unixnu.dataprotect-class`, and a file without one reads
  as class D. `0022` turns this on without a boot argument (`owner-dataprotect=0` turns it off).
- Every other selector fails with `0xe00002c7`, as UniXNU's does. Each selector is logged once when first called
  and once when it first fails, with the calling process (`oneios-keystore: selector N from NAME: 0x...`), so the
  boot log names what the guest asks for that is not answered.

Boot arguments: `oneios-keystore=0` (no service: the old failure), `oneios-aks-sls=N`, `owner-dataprotect=0`. None is
needed; xnuhost's default boot arguments are unchanged.

Not provided: keybags other than the system one (backup, escrow, per-user), passcode change, the reference-key
operations (`aks_ref_key_*`: selectors that reach the Secure Enclave's own keys, used for attestation and some
`SecKey` items), per-file keys in the kernel (the Data volume is unencrypted, APFS asks for none), any in-kernel
caller of the original kext's exported functions (none is in the collection's closure).

## Build

`kernel/build-hosted.sh` copies `build-oneios-keystore.py` into the build's device repository, adds it to the
board's archives before the GPU's, and adds `-Wl,-force_load,{tree}/build/oneios-keystore/liboneioskeystore.a` to the
hosted defconfig's `CONFIG_XNU_EXTRA_LD_ARCHIVES`. `kernel/lib/stage.sh` checks out the component's submodule. The
component's static initializer is taken out of `__mod_init_func` and run by `oneios_keystore_start` inside a module
load, as for NativeAES. `oneios_keystore_start` is weak in `oneios_host.c`: a kernel linked without the archive
starts none.

## Tested

On the PC only, 2026-11-11. Nothing here has run on the phone.

- The key wrap, on the host once (not a committed test): `wrapKey` and `unwrapKey` as they are in
  `OneiOSKeyStore.cpp`, compiled against OpenSSL's AES, give the bytes of OpenSSL's `aes-256-wrap-pad` for every
  key length from 1 to 512, unwrap them, and refuse a changed byte.
- In the guest (`qemu-aarch64 -cpu cortex-a710`, 8 vCPUs, 4 GiB, `docs/building.md` section 5): the service
  starts, its start test passes, launchd finds it and the keybag boot task ends with `DONE MKB_INIT`. Called and
  answered: 0 and 14 (keybagd), 7 and 35 (powerd), 17 (backboardd), 10 and 11 (securityd wraps and unwraps).
  Called and **not answered**: 107 (keybagd). Not checked: a keychain item read back after a restart, the
  protection-class attribute on a file. The log lines and the boots: `kernel/patches/oneios-hosted/README.md`,
  "0022-keystore".
