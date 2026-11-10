#!/usr/bin/env python3
"""The disks of the hosted OneiOS kernel: build/oneios-hosted-storage/liboneioshostedstorage.a.

  IOBlockStorageDevice.cpp   IOStorageFamily's public class, compiled from its source and registered late, below
                             the original class of the kernel collection (as UniXNU-droidvm's
                             ipados27/scripts/build-droidvm-source-storage.py does for its board)
  OneiOSHostedDisk.cpp       every disk xnuhost serves (virtio-blk on virtio-mmio), reads, writes and flush
  OneiOSHostedStorage.cpp    native_source_storage_start: loads the original family and APFS, registers the two
                             classes, adds the personality

Run by UniXNU's driver between the two kernel passes, in the build tree, with UNIXNU_DEVICE set:
kernel/build-hosted.sh copies this script into its copy of the device repository and adds it to board.toml's
[kernel] archives; configs/oneios_hosted_defconfig force-loads the archive (CONFIG_XNU_EXTRA_LD_ARCHIVES).
The sources stay in kernel/patches/oneios-hosted/storage of the repository.
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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-hosted-storage: UNIXNU_DEVICE is not set (run through kernel/build-hosted.sh)'))
REPO = DEVICE.resolve().parents[1]
SOURCES = REPO / 'kernel/patches/oneios-hosted/storage'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
# The pin of the OS profile's IOStorageFamily source (IOStorageFamily-337.100.1), as UniXNU-droidvm's board.
STORAGE_REVISION = '7edb88fbae296fb7c8ce2f64e115e116e566d51c'
LATE = {'IOBlockStorageDevice.cpp', 'OneiOSHostedDisk.cpp'}


def helper(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    storage = TREE / 'refer/IOStorageFamily'
    for path in (storage / 'IOBlockStorageDevice.cpp', SOURCES / 'OneiOSHostedDisk.cpp', REPO / 'drivers/build.py',
                 RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json'):
        if not path.exists():
            sys.exit(f'build-oneios-hosted-storage: {path} missing')
    drivers = helper('oneios_drivers_build', REPO / 'drivers/build.py')
    headers = helper('source_headers', TREE / 'scripts/prepare-storage-headers.py')
    out = TREE / 'build/oneios-hosted-storage'
    out.mkdir(parents=True, exist_ok=True)
    header_report = headers.prepare(storage, STORAGE_REVISION, out / 'headers')

    # The public class under another registered name, with the original class as its parent
    # (native_source_storage_parent, OneiOSHostedStorage.cpp); the C++ class name stays, since arm64e's vtable
    # discriminators are derived from it.
    base = out / 'IOBlockStorageDevice.cpp'
    original = (storage / 'IOBlockStorageDevice.cpp').read_text()
    macro = 'OSDefineMetaClassAndAbstractStructors(IOBlockStorageDevice,IOService)'
    if original.count(macro) != 1:
        sys.exit('build-oneios-hosted-storage: the pinned IOBlockStorageDevice registration macro changed')
    base.write_text(original.replace(macro, '''
extern "C" const OSMetaClass *native_source_storage_parent(void);
OSDefineMetaClassWithInit(IOBlockStorageDevice, IOService, )
IOBlockStorageDevice::MetaClass::MetaClass()
    : OSMetaClass("OneiOSBlockStorageDevice", native_source_storage_parent(), sizeof(IOBlockStorageDevice)) {}
OSDefineAbstractStructors(IOBlockStorageDevice, IOService)
OSDefineOperatorMethods(IOBlockStorageDevice)
'''))

    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    objcopy = shutil.which('llvm-objcopy') or '/opt/homebrew/opt/llvm/bin/llvm-objcopy'
    objects, report = [], []
    for source in (base, SOURCES / 'OneiOSHostedDisk.cpp', SOURCES / 'OneiOSHostedStorage.cpp'):
        target = out / (source.stem + '.o')
        command = drivers.command(record, source, target) + ['-I', str(out / 'headers/include')]
        if source == base:
            command.append('-Wno-error')
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=600)
        (out / (source.stem + '.log')).write_text(result.stdout + result.stderr)
        if result.returncode:
            sys.stderr.write((result.stdout + result.stderr)[-8000:])
            sys.exit(f'build-oneios-hosted-storage: {source.name} did not compile')
        if source.name in LATE:
            # No static initializer at kernel start: OneiOSHostedStorage.cpp runs it once the original family has loaded.
            late = target.with_suffix('.late.o')
            initializer = f'__GLOBAL__sub_I_{source.name}'
            subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func', '--remove-section=__DATA_CONST,__mod_init_func',
                            f'--globalize-symbol={initializer}', str(target), str(late)], check=True)
            nm = subprocess.run([shutil.which('llvm-nm') or 'nm', str(late)], capture_output=True, text=True, check=True).stdout
            sections = subprocess.run([shutil.which('llvm-objdump') or 'objdump', '-h', str(late)], capture_output=True, text=True, check=True).stdout
            if f' T {initializer}' not in nm or '__mod_init_func' in sections or '__init_offsets' in sections:
                sys.exit(f'build-oneios-hosted-storage: {source.name}: the static initializer was not made a late one')
            target = late
        objects.append(target)
        report.append({'path': str(source.relative_to(REPO)) if source.is_relative_to(REPO) else 'refer/IOStorageFamily/IOBlockStorageDevice.cpp (registration replaced)',
                       'universe': 'iokit', 'sha256': hashlib.sha256(source.read_bytes()).hexdigest()})
    archive = out / 'liboneioshostedstorage.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    (out / 'objects.json').write_text(json.dumps({'sources': report, 'headers': header_report}, indent=2, default=str) + '\n')
    print(f'build-oneios-hosted-storage: {archive}')


if __name__ == '__main__':
    main()
