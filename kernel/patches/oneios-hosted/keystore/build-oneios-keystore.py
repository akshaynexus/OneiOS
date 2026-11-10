#!/usr/bin/env python3
"""The hosted kernel's key store archive, built between the two kernel passes from the compile records of the first:

  build/oneios-keystore/liboneioskeystore.a   OneiOSKeyStore.cpp (this folder: oneios_keystore_start, the class keys,
                                              wrap and unwrap) and UniXNU's AppleKeyStoreState
                                              (NativeKeyStoreEvents.cpp: the AppleKeyStore service and its user client)

Why: the phone has no Secure Enclave and AppleSEPKeyStore is left out of kernel.kc, so launchd finds no AppleKeyStore
service and MobileKeyBag cannot load a system keybag (README.md here). Not a security boundary.

NativeKeyStoreEvents.cpp is compiled from a staged copy with two lines added, as Droid-VM's
build-droidvm-legacy-display.py does for its KeyStoreWrap.inc: the declaration of oneios_keystore_wrap, and
selectors 10 and 11 (aks_wrap_key, aks_unwrap_key) sent to it. The component's classes subclass IOService and
IOUserClient of the kernel itself, so unlike the software AES no binding to the kernel collection is generated.
Its static initializer is taken out of __mod_init_func and called by oneios_keystore_start inside a module load.

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES and started from oneios_board_late_start (0022-keystore.patch).
kernel/build-hosted.sh copies this script into the build's device repository and adds it to the board's archives.
Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository. No Apple file is read.
"""
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

TREE = Path.cwd()
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-keystore: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/keystore'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
COMPONENT = REPO / 'external/Droid-VM-UniXNU/drivers/AppleKeyStoreState/NativeKeyStoreEvents.cpp'
REQUIRED = ['_oneios_keystore_start', '_oneios_keystore_wrap', '__GLOBAL__sub_I_NativeKeyStoreEvents.cpp',
            '_native_keyevents_allocate', '_native_aks_encode_state', '__ZN13AppleKeyStore10gMetaClassE']
EDITS = (
    ('namespace {\n',
     'extern "C" IOReturn oneios_keystore_wrap(uint32_t selector, IOExternalMethodArguments *args);\nnamespace {\n'),
    ('        default: break;\n',
     '        case 10: case 11: rc = oneios_keystore_wrap(selector, args); break;\n        default: break;\n'),
)


def fail(text):
    sys.exit('build-oneios-keystore: ' + text)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def staged_component(out):
    text = COMPONENT.read_text()
    for anchor, replacement in EDITS:
        if text.count(anchor) != 1:
            fail('NativeKeyStoreEvents.cpp no longer has exactly one ' + anchor.strip())
        text = text.replace(anchor, replacement)
    staged = out / 'NativeKeyStoreEvents.cpp'    # the same file name: the static initializer is named after it
    staged.write_text(text)
    return staged


def main():
    for path in (REPO / 'drivers/build.py', COMPONENT, RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json',
                 HERE / 'OneiOSKeyStore.cpp'):
        if not path.is_file():
            fail(f'{path} missing (external/Droid-VM-UniXNU: git submodule update --init drivers/AppleKeyStoreState)')
    drivers = drivers_build()
    out = TREE / 'build/oneios-keystore'
    out.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    objects = []
    for source in (HERE / 'OneiOSKeyStore.cpp', staged_component(out)):
        obj = out / (source.stem + '.o')
        result = subprocess.run(drivers.command(record, source, obj), cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-6000:])
            fail(f'{source.name} did not compile')
        objects.append(obj)
    # The component's static initializer registers its two classes: it must run inside the board's module load, not
    # with the kernel's own initializers. Out of __mod_init_func, and global so OneiOSKeyStore.cpp can call it.
    native = out / 'NativeKeyStoreEvents.o'
    late = out / 'NativeKeyStoreEvents.late.o'
    subprocess.run([shutil.which('llvm-objcopy') or 'objcopy', '--remove-section=__DATA,__mod_init_func',
                    '--globalize-symbol=__GLOBAL__sub_I_NativeKeyStoreEvents.cpp', str(native), str(late)], check=True)
    objects[objects.index(native)] = late
    archive = out / 'liboneioskeystore.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True, text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    sources = [HERE / 'OneiOSKeyStore.cpp', COMPONENT]
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'universe': 'iokit', 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in sources]}, indent=2) + '\n')
    print(f'build-oneios-keystore: {archive} ({len(objects)} objects)')


if __name__ == '__main__':
    main()
