"""After a boot: did launchd apply the daemon policy? Read from the Data volume the boot left.

launchd (iOS 27.0.1, `/sbin/launchd`, read in its code) loads the override once per boot, after the boot task
`finish-demo-restore` and before `sysstatuscheck`, so the Data volume is mounted by then:

- the file is `/private/var/db/com.apple.xpc.launchd/disabled.plist`. With `disabled.migrated` beside it the
  older `/private/var/db/launchd.db/*/overrides.plist` files are not looked for;
- it is read only when root owns it and neither the group nor others may write it (`st_uid == 0` and
  `st_mode & 022 == 0`); otherwise launchd goes on with no override and says nothing;
- a job in it is still imported from the cache (its plist warnings appear in the log) but gets no
  `pending spawn` line, is never spawned, and a lookup of its Mach service fails with `error = 3`.

launchd writes what it does to `/private/var/log/com.apple.xpc.launchd/launchd.log`: every spawn as
`(<domain>/<label>) <Notice>: service state: spawning`, every job that wants to run at load while the domain
is still on demand as `pending spawn, domain in on-demand-only mode: <label>`. On the PC the Data volume of a
stopped run is read with apfs-fuse (docs/building.md section 5).
"""
import os
import plistlib
import re
import stat

from . import launchd

OVERRIDE = f'{launchd.OVERRIDES_DIR}/disabled.plist'
MIGRATED = f'{launchd.OVERRIDES_DIR}/disabled.migrated'
LOG = 'log/com.apple.xpc.launchd/launchd.log'

_LABEL = r'(?:system/|user/\d+/|gui/\d+/)?([A-Za-z0-9_.\-]+)'
_SPAWNING = re.compile(r'\(' + _LABEL + r'(?: \[\d+\])?\) <Notice>: service state: spawning')
_PENDING = re.compile(r'pending spawn, domain in on-demand-only mode: (\S+)')
_FORCED = re.compile(r'Ignoring disabled state because DisableAllowed=false: (\S+)')
_LOOKUP = re.compile(r'failed lookup: name = (\S+?), requestor = ([^,]+), error = 3\b')


def accepted(st):
    """Why launchd would not read an override file with this stat result; None when it would."""
    if not stat.S_ISREG(st.st_mode):
        return 'not a regular file'
    if st.st_uid != 0:
        return f'owner is uid {st.st_uid}: launchd reads it only when root owns it'
    if st.st_mode & 0o022:
        return f'mode {stat.S_IMODE(st.st_mode):04o}: launchd reads it only when group and others cannot write it'
    return None


def read_log(text):
    """What a launchd.log says: labels spawned, labels pending at load, labels run although disabled, and the
    Mach service lookups that found no job ({name: requestor})."""
    return {
        'spawned': set(_SPAWNING.findall(text)),
        'pending': set(_PENDING.findall(text)),
        'forced': set(_FORCED.findall(text)),
        'no_such_service': {name: who.strip() for name, who in _LOOKUP.findall(text)},
    }


def at_load(cache):
    """Labels of the compiled cache that run without being asked: RunAtLoad or KeepAlive."""
    return {label for label, (_, _, job) in launchd.jobs(cache).items() if job.get('RunAtLoad') or job.get('KeepAlive')}


def check(data, root=None, ignore_owner=False):
    """Compares the override and launchd's log of a Data tree. `root` (the OS volume's files) adds how many of
    the jobs want to run at load and which are marked DisableAllowed = false.
    Returns (lines, problems): problems is empty when the policy was applied."""
    lines, problems = [], []
    path = os.path.join(data, OVERRIDE)
    try:
        st = os.stat(path)
        with open(path, 'rb') as f:
            disabled = {label for label, off in plistlib.load(f).items() if off is True}
    except (OSError, plistlib.InvalidFileException, ValueError) as e:
        return lines, [f'{OVERRIDE}: {e}']
    why = accepted(st)
    if why and ignore_owner:
        lines.append(f'override: {len(disabled)} jobs disabled; owner and mode not checked here ({why})')
    elif why:
        problems.append(f'{OVERRIDE}: {why}')
    else:
        lines.append(f'override: {len(disabled)} jobs disabled; root owns it, mode {stat.S_IMODE(st.st_mode):04o}: launchd reads it')
    if not os.path.exists(os.path.join(data, MIGRATED)):
        lines.append(f'{MIGRATED} is not there: launchd looks for the older launchd.db overrides first')
    try:
        with open(os.path.join(data, LOG), errors='replace') as f:
            log = read_log(f.read())
    except OSError as e:
        return lines, problems + [f'{LOG}: {e} (launchd writes it once the Data volume is mounted)']
    lines.append(f'launchd.log: {len(log["spawned"])} jobs spawned, {len(log["pending"])} pending at load')

    forced = set()
    if root:
        daemons = os.path.join(root, launchd.DAEMONS_DIR)
        for name in sorted(os.listdir(daemons)) if os.path.isdir(daemons) else ():
            if name.startswith('com.apple.jetsamproperties.') and name.endswith('.plist'):
                with open(os.path.join(daemons, name), 'rb') as f:
                    forced |= launchd.forced(plistlib.load(f))
        with open(os.path.join(root, launchd.CACHE), 'rb') as f:
            wants = at_load(plistlib.load(f))
        ran = log['spawned'] | log['pending']
        lines.append(f'cache: {len(wants & disabled)} of the disabled jobs want to run at load, '
                     f'{len(wants & disabled & ran)} of them pending or spawned; of the {len(wants - disabled)} other '
                     f'such jobs, {len((wants - disabled) & ran)}')
    ran_disabled = sorted((log['spawned'] | log['pending']) & disabled)
    allowed = sorted(label for label in ran_disabled if label in forced or label in log['forced'])
    wrong = sorted(set(ran_disabled) - set(allowed))
    if allowed:
        lines.append(f'run although disabled (DisableAllowed = false): {", ".join(allowed)}')
    refused = sorted(name for name in log['no_such_service'] if name in disabled)
    if refused:
        lines.append('lookups of disabled jobs that found no service: ' +
                     ', '.join(f'{name} (asked by {log["no_such_service"][name]})' for name in refused))
    if wrong:
        problems.append(f'{len(wrong)} disabled jobs were started: {", ".join(wrong)}')
    else:
        lines.append('no disabled job was started: the policy is in force')
    return lines, problems
