"""launchd's job set: which jobs the guest runs, and how a job that must not run is kept from running.

Two places hold it:

- `System/Library/xpc/launchd.plist` on the OS volume: the compiled cache launchd reads its jobs from. Its
  `LaunchDaemons` dictionary is keyed by the path of each job's plist; the on-disk plists under
  `/System/Library/LaunchDaemons` are not read at boot (UniXNU-droidvm `package/DroidVMLaunchPolicy.c`:
  "launchd takes its jobs from the signed compiled cache ... and ignores the on-disk plist's
  ProgramArguments, and a brand-new label is never enumerated"). Edited only on a root we write ourselves.
- `/private/var/db/com.apple.xpc.launchd/disabled.plist` on the Data volume: the override `launchctl disable`
  writes, `{label: true}`. launchd does not load a disabled job. It must belong to root, and the empty file
  `disabled.migrated` beside it selects the current loader (DroidVMLaunchPolicy.c writes both).

iOS 27 adds `DisableAllowed = false` to some jobs in the jetsam properties of the board
(`System/Library/LaunchDaemons/com.apple.jetsamproperties.<target>.plist`, `Version4 > Daemon|XPCService >
Override > <label>`): launchd then loads the job although it is disabled (vphone-cli
`Research/Guest/service_trimming.md`, "iOS 27 and DisableAllowed").
"""

CACHE = 'System/Library/xpc/launchd.plist'
DAEMONS_DIR = 'System/Library/LaunchDaemons'
OVERRIDES_DIR = 'db/com.apple.xpc.launchd'  # in the Data tree (/private/var)


def jobs(cache):
    """{label: (group, key, job)} of every job in a compiled cache dictionary."""
    out = {}
    for group in cache:
        entries = cache[group]
        if not isinstance(entries, dict):
            continue
        for key, job in entries.items():
            if isinstance(job, dict) and isinstance(job.get('Label'), str):
                out.setdefault(job['Label'], (group, key, job))
    return out


def remove_jobs(cache, labels):
    """Takes the jobs with these labels out of the cache. The labels that were there."""
    have = jobs(cache)
    gone = []
    for label in labels:
        if label in have:
            group, key, _ = have[label]
            del cache[group][key]
            gone.append(label)
    return gone


def add_daemon(cache, name, job):
    """Adds or replaces a LaunchDaemon as vphone's `inject_daemon` does. True when the cache changed."""
    daemons = cache.setdefault('LaunchDaemons', {})
    key = f'/{DAEMONS_DIR}/{name}.plist'
    if daemons.get(key) == job:
        return False
    daemons[key] = job
    return True


def strip_keys(cache, keys):
    """Removes these keys from every job. {key: [labels]} of what was removed."""
    removed = {}
    for label, (_, _, job) in sorted(jobs(cache).items()):
        for key in keys:
            if key in job:
                del job[key]
                removed.setdefault(key, []).append(label)
    return removed


def overrides(existing, disable, enable=()):
    """The disabled.plist content: `existing` with `disable` set and `enable` taken out again."""
    out = dict(existing or {})
    for label in enable:
        out.pop(label, None)
    for label in disable:
        out[label] = True
    return out


def _override_tables(jetsam):
    """The `Override` dictionaries of a jetsam properties plist, with where they are."""
    out = []
    for version, body in jetsam.items():
        if not isinstance(body, dict):
            continue
        for kind in ('Daemon', 'XPCService'):
            table = body.get(kind, {}).get('Override') if isinstance(body.get(kind), dict) else None
            if isinstance(table, dict):
                out.append((f'{version}/{kind}', table))
    return out


def forced(jetsam):
    """Labels a jetsam properties plist marks `DisableAllowed = false`."""
    out = set()
    for _, table in _override_tables(jetsam):
        for label, entry in table.items():
            if isinstance(entry, dict) and entry.get('DisableAllowed') is False:
                out.add(label)
    return out


def allow_disable(jetsam):
    """Removes every `DisableAllowed = false`. The labels it was removed from."""
    out = set()
    for _, table in _override_tables(jetsam):
        for label, entry in table.items():
            if isinstance(entry, dict) and entry.get('DisableAllowed') is False:
                del entry['DisableAllowed']
                out.add(label)
    return sorted(out)


def drop_recover_on_crash(jetsam, labels):
    """Removes `RecoverOnCrash` from these labels' entries, so a crash loop of one of them does not restart
    the whole guest during bring-up (iOSPlus10 `docs/laurel_sprout/ios26-root.md` section 3). The labels changed."""
    out = set()
    for _, table in _override_tables(jetsam):
        for label in labels:
            entry = table.get(label)
            if isinstance(entry, dict) and 'RecoverOnCrash' in entry:
                del entry['RecoverOnCrash']
                out.add(label)
    return sorted(out)
