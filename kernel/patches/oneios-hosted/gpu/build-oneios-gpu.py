#!/usr/bin/env python3
"""The hosted kernel's GPU archive, built between the two kernel passes from the compile records of the first:

  build/oneios-gpu/liboneiosgpu.a    OneiOSGpu.cpp (this folder: oneios_gpu3d_start) and Droid-VM's virtio-gpu 3D
                                     kernel driver, unchanged: UniXNU-droidvm package/DroidVMVirtioGpu.cpp and
                                     DroidVMVirtioGpu3D.cpp, with NativeAdreno's DroidVMVirtgpuABI.h

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES and started from oneios_board_late_start (0005-gpu-3d.patch).
kernel/build-hosted.sh copies this script into the build's device repository and adds it to the board's archives;
the VM kernel (build-linux.sh) does not build it.

The NativeAdreno pieces Droid-VM builds beside the driver (the accelerator's user client and IOKit surface, execbuf,
memory, the PM4 encoder, and package/DroidVMGpu.cpp which publishes them) are compiled here too, into
build/oneios-gpu/adreno/, with the edits Droid-VM's own build makes to two of them (its
ipados27/scripts/build-droidvm-gpu.py, run here up to its accelerator step). They are a compile check only and are
NOT in the archive: they bind to the staged AGXAccelerator, which Droid-VM makes from reviewed inputs of one
kernelcache (iPad17,3) and which does not exist for iPhone18,2 yet, and to XNU patches of Droid-VM's board
(OSMetaClassAdoptSuperclass, the sandbox grant, native_pmap_user_mappings). adreno/status.json says what compiled.

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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-gpu: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/gpu'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
PACKAGE = REPO / 'external/UniXNU-droidvm/package'
ADRENO = REPO / 'external/NativeAdreno'
SOURCES = [HERE / 'OneiOSGpu.cpp', PACKAGE / 'DroidVMVirtioGpu.cpp', PACKAGE / 'DroidVMVirtioGpu3D.cpp']
REQUIRED = ['_oneios_gpu3d_start', '_droidvm_virtgpu_call', '_droidvm_virtgpu_close', '__ZN16DroidVMVirtioGpu5startEv']


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def compile_one(drivers, record, source, obj, extra):
    command = drivers.command(record, source, obj) + extra
    result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
    obj.with_suffix('.log').write_text(result.stderr)
    return result


def adreno_check(out):
    """Droid-VM's build of the NativeAdreno face, up to (not including) the staged accelerator: a compile check."""
    script = REPO / 'external/UniXNU-droidvm/ipados27/scripts/build-droidvm-gpu.py'
    text = script.read_text()
    cut = "print(subprocess.run(['python3', ROOT / 'scripts/build-adreno740-accelerator.py']"
    if text.count(cut) != 1:
        return {'compiled': False, 'why': 'build-droidvm-gpu.py no longer calls the accelerator build where expected'}
    # Its layout: ROOT/drivers/Adreno740, ROOT/kits/{ArmSmmu4K,AdrenoPM4,droidvm}, ROOT/.cache/xnu-work, ROOT/build/xnu.
    root = out / 'root'
    shutil.rmtree(root, ignore_errors=True)
    (root / 'drivers').mkdir(parents=True)
    (root / 'kits').mkdir()
    (root / '.cache').mkdir()
    (root / 'build').mkdir()
    (root / 'scripts').mkdir()
    links = {'drivers/Adreno740': ADRENO, 'kits/ArmSmmu4K': REPO / 'external/ArmSmmu4K', 'kits/AdrenoPM4': REPO / 'external/AdrenoPM4',
             'kits/droidvm': PACKAGE, '.cache/xnu-work': TREE / '.cache/xnu-work', 'build/xnu': TREE / 'build/xnu'}
    for name, target in links.items():
        if not target.exists() or (target.is_dir() and not any(target.iterdir())):
            return {'compiled': False, 'why': f'{target} is not checked out (scripts/setup/get-references.sh)'}
        (root / name).symlink_to(target.resolve())
    staged = root / 'scripts/build-droidvm-gpu.py'
    staged.write_text(text[:text.index(cut)])
    result = subprocess.run([sys.executable, '-I', str(staged)], capture_output=True, text=True, timeout=1800)
    objects = sorted(str(p.relative_to(out)) for p in (root / 'build/droidvm-gpu').glob('*.o'))
    status = {'compiled': result.returncode == 0, 'objects': objects, 'linked': False}
    if result.returncode:
        status['why'] = (result.stderr or result.stdout).strip()[-2000:]
    return status


def main():
    for path in (REPO / 'drivers/build.py', ADRENO / 'DroidVMVirtgpuABI.h', RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', *SOURCES):
        if not path.is_file():
            sys.exit(f'build-oneios-gpu: {path} missing (scripts/setup/get-references.sh UniXNU-droidvm NativeAdreno)')
    drivers = drivers_build()
    out = TREE / 'build/oneios-gpu'
    out.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    objects = []
    for source in SOURCES:
        obj = out / (source.stem + '.o')
        # DYNAMIC: the transport's queues and command buffers (about 9 MB) are allocated at start, not kernel bss: with
        # them in __DATA the kernel no longer fits the collection's stock layout (kernel/prelink, the board's Q7).
        result = compile_one(drivers, record, source, obj, ['-I', str(PACKAGE), '-I', str(ADRENO), '-DDROIDVM_VIRTIO_GPU_DYNAMIC=1'])
        if result.returncode:
            sys.stderr.write(result.stderr[-6000:])
            sys.exit(f'build-oneios-gpu: {source.name} did not compile')
        objects.append(obj)
    archive = out / 'liboneiosgpu.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True, text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        sys.exit('build-oneios-gpu: definitions absent: ' + ', '.join(missing))
    adreno = out / 'adreno'
    adreno.mkdir(exist_ok=True)
    status = adreno_check(adreno) if os.environ.get('ONEIOS_GPU_ADRENO_CHECK', '1') != '0' else {'compiled': False, 'why': 'skipped'}
    (adreno / 'status.json').write_text(json.dumps(status, indent=2) + '\n')
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'universe': 'iokit', 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in SOURCES]}, indent=2) + '\n')
    print(f'build-oneios-gpu: {archive} ({len(objects)} objects); NativeAdreno pieces: '
          + ('compiled, not linked' if status['compiled'] else 'not compiled: ' + status.get('why', '?').splitlines()[-1][:200]))


if __name__ == '__main__':
    main()
