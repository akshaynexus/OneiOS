#!/usr/bin/env python3
"""Checks of the kernel's series and profile that need no sources: every entry of the board series is a path into a
pinned external repository or a patch of this directory that exists; no patch is listed twice; our own patches are
all listed; the OS profile and the board parse and name the iPhone18,2 identity; the defconfig has no unknown shape."""
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SERIES = ROOT / 'kernel/device/patches/xnu-12377/series'
EXTERNAL = re.compile(r'^\.\./\.\./\.\./\.\./external/(UniXNU-droidvm|xnu-linux-toolchain|Droid-VM-UniXNU)/[A-Za-z0-9_./-]+\.patch$')
errors = []

entries = [line.strip() for line in SERIES.read_text().splitlines() if line.strip() and not line.lstrip().startswith('#')]
if len(entries) != len(set(entries)):
    errors.append('a patch is listed twice in the board series')
own = set()
for entry in entries:
    if entry.startswith('../'):
        if not EXTERNAL.match(entry):
            errors.append(f'series: {entry} is not a patch of a pinned external repository')
        elif (ROOT / 'external').is_dir() and (SERIES.parent / entry).resolve().parent.is_dir() and not (SERIES.parent / entry).is_file():
            errors.append(f'series: {entry} does not exist at the pin')
    else:
        own.add(entry)
        if not (SERIES.parent / entry).is_file():
            errors.append(f'series: {entry} missing')
for patch in SERIES.parent.glob('*.patch'):
    if patch.name not in own:
        errors.append(f'{patch.name} is not in the series')

profile = tomllib.loads((ROOT / 'kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml').read_text())
image = tomllib.loads((ROOT / 'profiles/ios-27.0.1-iPhone18,2.toml').read_text()) if (ROOT / 'profiles/ios-27.0.1-iPhone18,2.toml').is_file() else None
if profile['os']['arch'] != 'arm64e' or profile['os']['xnu_version'] != '12377.121.6':
    errors.append('OS profile: not arm64e on xnu-12377.121.6')
if profile['adt']['root']['string']['model'] != 'iPhone18,2' or profile['ipsw']['product'] != 'iPhone18,2':
    errors.append('OS profile: the identity is not iPhone18,2')
if image:
    for ours, theirs in (('url', image['ipsw']['url']), ('build', image['os']['build']), ('device_class', image['os']['board']),
                         ('chip_id', image['os']['chip_id']), ('board_id', image['os']['board_id'])):
        if profile['ipsw'][ours] != theirs:
            errors.append(f'OS profile [ipsw] {ours} = {profile["ipsw"][ours]!r}, the image profile says {theirs!r}')
pins = (ROOT / 'kernel/pins.sh').read_text()
if f'XNU_COMMIT={profile["os"]["xnu_revision"]}' not in pins:
    errors.append('pins.sh XNU_COMMIT differs from the OS profile')
board = tomllib.loads((ROOT / 'kernel/device/board.toml').read_text())
if board['board']['os_profile'] != profile['os']['name']:
    errors.append('board.toml os_profile is not the OS profile')
for line in (ROOT / 'kernel/device/configs' / board['kernel']['defconfig']).read_text().splitlines():
    if line.strip() and not line.startswith('#') and not re.match(r'^CONFIG_XNU_[A-Z0-9_]+=(y|n|"[^"]*")$', line):
        errors.append(f'defconfig: bad line {line!r}')
for error in errors:
    print('series-check: ' + error, file=sys.stderr)
sys.exit(1 if errors else 0)
