#!/usr/bin/env python3
"""The OneiOS board's archives, built between the two kernel passes (board.toml [kernel] archives), from the
compile records of the first pass:

  build/oneios-drivers/liboneiosdrivers.a   drivers/ of the OneiOS repository, by its own drivers/build.py
                                            (bridge transport, haptics, Bluetooth; entry oneios_drivers_start)
  build/oneios-board/liboneiosboard.a       UniXNU-droidvm package/DroidVMCpufreq.c, unchanged, compiled as its
                                            board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1; entry
                                            droidvm_cpufreq_attach)

Both are force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES (configs/oneios_defconfig) and started by
oneios_board_late_start (patches/xnu-12377/oneios-board-start.patch). Run by UniXNU's driver in the build tree,
with UNIXNU_DEVICE = kernel/device.
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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-board: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
CPUFREQ = REPO / 'external/UniXNU-droidvm/package/DroidVMCpufreq.c'


def main():
    build = REPO / 'drivers/build.py'
    print(f'oneios-board: drivers/ of {REPO}' + ('' if os.environ.get('ONEIOS_REPO') else ' (found from the device directory: ONEIOS_REPO is not set)'), flush=True)
    for path in (build, CPUFREQ, RECORDS / 'osfmk/DEVELOPMENT/startup.o.json'):
        if not path.is_file():
            sys.exit(f'build-oneios-board: {path} missing')
    # 1. drivers/: its own script, with this tree's records and output place.
    subprocess.run([sys.executable, '-I', str(build), '--records', str(RECORDS), '--out', str(TREE / 'build/oneios-drivers')], check=True)
    # 2. DroidVMCpufreq.c, with drivers/build.py's rewrite of a recorded compile command (one copy of that code).
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', build)
    drivers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(drivers)
    out = TREE / 'build/oneios-board'
    out.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['mach'])
    target = out / 'DroidVMCpufreq.o'
    command = drivers.command(record, CPUFREQ, target) + ['-DDROIDVM_DEVICE_DYNAMIC=1']
    result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=600)
    if result.returncode:
        sys.stderr.write(result.stderr)
        sys.exit('build-oneios-board: DroidVMCpufreq.c did not compile')
    archive = out / 'liboneiosboard.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), str(target)], check=True)
    (out / 'objects.json').write_text(json.dumps({'sources': [{'path': 'external/UniXNU-droidvm/package/DroidVMCpufreq.c', 'universe': 'mach',
                                                              'sha256': hashlib.sha256(CPUFREQ.read_bytes()).hexdigest()}]}, indent=2) + '\n')
    print(f'build-oneios-board: {archive}')


if __name__ == '__main__':
    main()
