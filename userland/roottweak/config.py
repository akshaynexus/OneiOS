"""Reads userland/config: the device, the daemon policy and the preference seeds."""
import os
import tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
CONFIG = os.path.join(os.path.dirname(HERE), 'config')


class ConfigError(Exception):
    pass


def _load(name, directory):
    with open(os.path.join(directory or CONFIG, name), 'rb') as f:
        return tomllib.load(f)


def device(directory=None, name='device-iphone18,2.toml'):
    d = _load(name, directory)
    for table in ('device', 'display', 'adt', 'mobilegestalt'):
        if table not in d:
            raise ConfigError(f'{name}: no [{table}]')
    if d['device'].get('device_class') != 'iPhone':
        raise ConfigError(f'{name}: OneiOS targets iOS on an iPhone, not {d["device"].get("device_class")}')
    return d


class Daemons:
    def __init__(self, raw):
        self.keep = set(raw['keep']['labels'])
        self.keep_reasons = raw['keep'].get('reasons', {})
        self.idle = dict(raw.get('idle', {}))
        self.replace = dict(raw['replace']['labels'])
        self.replace_default = raw['replace'].get('default', True)
        self.groups = raw.get('group', {})
        self.check()

    def check(self):
        """The policy must not contradict itself: nothing kept or idled is also disabled."""
        problems = []
        seen = {}
        for name, group in self.groups.items():
            if not group.get('source'):
                problems.append(f'group {name} names no source')
            for label in group['labels']:
                if label in seen:
                    problems.append(f'{label} is in groups {seen[label]} and {name}')
                seen[label] = name
        for label in self.replace:
            if label in seen:
                problems.append(f'{label} is replaced and in group {seen[label]}')
            seen[label] = 'replace'
        for label in sorted(seen):
            if label in self.keep:
                problems.append(f'{label} is kept and in {seen[label]}')
            if label in self.idle:
                problems.append(f'{label} is idled and in {seen[label]}')
        if problems:
            raise ConfigError('daemons.toml: ' + '; '.join(problems))

    def selected(self, groups=(), without=(), keep=()):
        """{label: why} to disable: the default groups and `groups`, less `without` groups and `keep` labels."""
        for name in list(groups) + list(without):
            if name != 'replace' and name not in self.groups:
                raise ConfigError(f'no daemon group {name}; there are: replace, {", ".join(sorted(self.groups))}')
        out = {}
        if (self.replace_default or 'replace' in groups) and 'replace' not in without:
            out.update({label: 'replace' for label in self.replace})
        for name, group in self.groups.items():
            if (group.get('default') or name in groups) and name not in without:
                out.update({label: name for label in group['labels']})
        for label in keep:
            out.pop(label, None)
        return out


def daemons(directory=None):
    return Daemons(_load('daemons.toml', directory))


def prefs(directory=None):
    out = _load('prefs.toml', directory)['pref']
    for p in out:
        if p.get('user') not in ('mobile', 'root', 'any') or not p.get('domain') or not p.get('group') or not p.get('source'):
            raise ConfigError(f'prefs.toml: {p.get("domain")}: needs user, domain, group and source')
    return out
