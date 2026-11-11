#!/usr/bin/env python3
"""Builds OneiOS's kernel-side drivers (this folder) into one archive for the kernel link.

Each source is compiled with the command the XNU build recorded for a source of the same universe
(the build leaves one JSON record per object under build/xnu/ios-obj/<CONFIG>/), with the source and
output replaced and the board's warning options, the way UniXNU-droidvm's
ipados27/scripts/build-droidvm-devices.py builds its device sources:

  bridge/OneiOSBridge.c                 Mach universe   (record osfmk/DEVELOPMENT/startup.o.json)
  haptics/*.c, bluetooth/*.c            BSD universe    (record bsd/DEVELOPMENT/kern_control.o.json)
  haptics/*.cpp, bluetooth/*.cpp        IOKit universe  (record iokit/DEVELOPMENT/IOPlatformExpert.cpo.json)
  camera/oneios_camera.c (Mach), camera/oneios_camera_dev.c (BSD)

  drivers/build.py [--records DIR] [--out DIR] [--check]

--records  default: the newest build under out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/*
--out      default: out/drivers  ->  liboneiosdrivers.a, objects.json (path and SHA-256 of each source)
--check    compile only (no archive): the syntax and type check of a change, a few seconds

The archive enters the kernel through CONFIG_XNU_EXTRA_LD_ARCHIVES (kernel/README.md, "Linking a
board archive"); oneios_drivers_start() is its one entry point. No Apple file is read or written.
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SOURCES = [
    ('mach', 'bridge/OneiOSBridge.c'),
    ('bsd', 'haptics/oneios_haptic.c'),
    ('iokit', 'haptics/OneiOSVibrator.cpp'),
    ('bsd', 'bluetooth/oneios_bt.c'),
    ('bsd', 'bluetooth/oneios_bt_h4.c'),
    ('bsd', 'bluetooth/oneios_bt_ios.c'),
    ('bsd', 'bluetooth/oneios_bt_ctl.c'),
    ('bsd', 'bluetooth/oneios_bt_dev.c'),
    ('iokit', 'bluetooth/OneiOSBluetooth.cpp'),
    ('iokit', 'brightness/OneiOSBacklight.cpp'),
    ('iokit', 'battery/OneiOSBattery.cpp'),
    ('mach', 'camera/oneios_camera.c'),
    ('bsd', 'camera/oneios_camera_dev.c'),
]
RECORDS = {'mach': 'osfmk/DEVELOPMENT/startup.o.json', 'bsd': 'bsd/DEVELOPMENT/kern_control.o.json',
           'iokit': 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json'}


def find_records():
    found = sorted(ROOT.glob('out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/*/osfmk/DEVELOPMENT/startup.o.json'),
                   key=lambda p: p.stat().st_mtime)
    return found[-1].parents[2] if found else None


def record(records, name):
    return json.loads((records / name).read_text().strip().rstrip(','))


def command(rec, src, dst):
    """The recorded compile command with another source and output, and the board's warnings."""
    cmd, args, i = [], rec['arguments'], 0
    while i < len(args):
        value = args[i]
        if value == rec['file']:
            cmd.append(str(src))
        elif value in ('-o', '-MF', '-MT', '-MQ', '-MJ'):
            if value == '-o':
                cmd += ['-o', str(dst)]
            i += 1
        elif value in ('-MD', '-MMD', '-MP'):
            pass
        elif value == '-Xclang' and i + 1 < len(args) and args[i + 1].startswith('-W'):
            i += 1
        elif not value.startswith('-W') or value.startswith('-Wa,'):
            cmd.append(value)
        i += 1
    cmd += ['-Wall', '-Wextra', '-Werror', '-Wno-unused-parameter']
    if Path(src).suffix != '.c':
        cmd += ['-Wno-c99-designator', '-Wno-reorder-init-list', '-Wno-deprecated-enum-enum-conversion',
                '-Wno-unnecessary-virtual-specifier', '-Wno-macro-redefined']
    cmd += ['-I', str(HERE / 'bridge'), '-I', str(HERE / 'bluetooth'), '-I', str(HERE / 'camera')]
    return cmd


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--records', type=Path)
    parser.add_argument('--out', type=Path, default=ROOT / 'out/drivers')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    records = args.records or find_records()
    if not records or not (records / RECORDS['mach']).is_file():
        sys.exit('drivers: no XNU build records (build the kernel first: scripts/bg.sh kernel kernel/build-linux.sh), or pass --records')
    out = args.out
    (out / 'objects').mkdir(parents=True, exist_ok=True)
    objects, listing = [], []
    for universe, name in SOURCES:
        rec = record(records, RECORDS[universe])
        src = HERE / name
        dst = out / 'objects' / (Path(name).name + '.o')
        result = subprocess.run(command(rec, src, dst), cwd=rec['directory'], capture_output=True, text=True, timeout=600)
        if result.returncode:
            sys.stderr.write(result.stderr)
            sys.exit(f'drivers: {name} did not compile')
        objects.append(dst)
        listing.append({'path': f'drivers/{name}', 'universe': universe, 'sha256': hashlib.sha256(src.read_bytes()).hexdigest()})
        print(f'drivers: {name}')
    if args.check:
        print(f'drivers: {len(objects)} sources compile against {records}')
        return
    archive = out / 'liboneiosdrivers.a'
    archive.unlink(missing_ok=True)
    ar = shutil.which('llvm-ar') or shutil.which('ar')
    subprocess.run([ar, 'rcs', str(archive), *map(str, objects)], check=True)
    (out / 'objects.json').write_text(json.dumps({'records': str(records), 'sources': listing}, indent=2) + '\n')
    print(f'drivers: {archive} ({archive.stat().st_size} bytes)')


if __name__ == '__main__':
    main()
