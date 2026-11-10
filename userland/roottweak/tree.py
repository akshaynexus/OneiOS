"""A directory that stands for a volume of the guest, and the record of what was changed in it.

Paths are the guest's, relative to the volume's top (`System/Library/xpc/launchd.plist`). A symbolic link inside
the tree is followed inside the tree: an absolute target (`/private/var`) is taken from the tree's top, never
from the host. A path that would leave the tree is an error.

The build usually does not run as root, so owners cannot be set on the host. Every change is recorded with
the owner and mode the guest needs; the image build applies them when it packs the volume
(`tweaks.manifest.json`). As root, owners are also set on the files.
"""
import hashlib
import os
import plistlib
import stat

ROOT = (0, 0)
MOBILE = (501, 501)
MAX_LINKS = 40


class TreeError(Exception):
    pass


class Manifest:
    """What the tweaks changed: one entry per path and volume, the last change of a path wins."""

    def __init__(self):
        self.entries = {}

    def record(self, volume, path, kind, uid=None, gid=None, mode=None, sha256=None, size=None, target=None, tweak=None):
        entry = {'volume': volume, 'path': path, 'type': kind}
        if uid is not None:
            entry.update(uid=uid, gid=gid)
        if mode is not None:
            entry['mode'] = '%04o' % mode
        if sha256 is not None:
            entry.update(sha256=sha256, size=size)
        if target is not None:
            entry['target'] = target
        if tweak:
            entry['tweak'] = tweak
        self.entries[(volume, path)] = entry

    def sorted(self):
        return [self.entries[key] for key in sorted(self.entries)]


class Tree:
    def __init__(self, top, volume, manifest=None, readonly=False):
        self.top = os.path.realpath(top)
        if not os.path.isdir(self.top):
            raise TreeError(f'{top}: not a directory')
        self.volume = volume
        self.manifest = manifest if manifest is not None else Manifest()
        self.readonly = readonly
        self.tweak = None  # the tweak now running, for the manifest

    # ---- paths -------------------------------------------------------------------------------------------
    def host(self, path, follow_last=True):
        """The host path of a guest path, links followed inside the tree."""
        parts = [p for p in path.split('/') if p not in ('', '.')]
        done = []
        links = 0
        while parts:
            part = parts.pop(0)
            if part == '..':
                if done:
                    done.pop()
                continue
            here = os.path.join(self.top, *done, part)
            if os.path.islink(here) and (parts or follow_last):
                links += 1
                if links > MAX_LINKS:
                    raise TreeError(f'{path}: too many symbolic links')
                target = os.readlink(here)
                if target.startswith('/'):
                    done = []
                parts = [p for p in target.split('/') if p not in ('', '.')] + parts
                continue
            done.append(part)
        return os.path.join(self.top, *done)

    def exists(self, path):
        return os.path.exists(self.host(path))

    def lexists(self, path):
        return os.path.lexists(self.host(path, follow_last=False))

    def isdir(self, path):
        return os.path.isdir(self.host(path))

    def isfile(self, path):
        return os.path.isfile(self.host(path))

    def listdir(self, path):
        host = self.host(path)
        return sorted(os.listdir(host)) if os.path.isdir(host) else []

    def walk_files(self, path):
        """Guest paths of the regular files under `path` (links not followed)."""
        base = self.host(path)
        out = []
        for current, dirs, files in os.walk(base):
            dirs.sort()
            for name in sorted(files):
                full = os.path.join(current, name)
                if os.path.isfile(full) and not os.path.islink(full):
                    out.append(os.path.join(path, os.path.relpath(full, base)))
        return out

    # ---- reading -----------------------------------------------------------------------------------------
    def read(self, path):
        with open(self.host(path), 'rb') as f:
            return f.read()

    def read_plist(self, path, default=None):
        """The property list at `path`; `default` where there is no file. A file that is no plist is an error:
        it is never written over as if it were empty."""
        if not self.exists(path):
            return default
        try:
            return plistlib.loads(self.read(path))
        except Exception as e:
            raise TreeError(f'{self.volume}:{path}: not a property list ({e})')

    # ---- writing -----------------------------------------------------------------------------------------
    def _writable(self, path):
        if self.readonly:
            raise TreeError(f'{self.volume}:{path}: this volume is read-only in this layout')

    def _own(self, host, owner, mode):
        if mode is not None and not os.path.islink(host):
            os.chmod(host, mode)
        if owner is not None and os.geteuid() == 0:
            os.lchown(host, *owner)

    def mkdir(self, path, owner=ROOT, mode=0o755):
        """Makes `path` and its missing parents. Only what is made here is recorded, with `owner` and `mode`;
        a directory that exists keeps what it has unless it is `path` itself."""
        self._writable(path)
        parts = [p for p in path.split('/') if p]
        for i in range(1, len(parts) + 1):
            sub = '/'.join(parts[:i])
            host = self.host(sub)
            last = i == len(parts)
            if not os.path.isdir(host):
                os.makedirs(host)  # a link's target inside the tree may lack parents of its own
                self._own(host, owner, mode if last else 0o755)
                self.manifest.record(self.volume, sub, 'dir', *(owner if last else ROOT), mode if last else 0o755,
                                     tweak=self.tweak)
            elif last:
                self._own(host, owner, mode)
                self.manifest.record(self.volume, sub, 'dir', *owner, mode, tweak=self.tweak)

    def write(self, path, data, owner=ROOT, mode=0o644, parent_owner=None):
        """Writes a file. True when the content changed."""
        self._writable(path)
        parent = os.path.dirname(path)
        if parent and not self.isdir(parent):
            self.mkdir(parent, parent_owner or owner)
        host = self.host(path)
        changed = True
        if os.path.isfile(host) and not os.path.islink(host):
            with open(host, 'rb') as f:
                changed = f.read() != data
        if changed:
            tmp = host + '.oneios-tmp'
            with open(tmp, 'wb') as f:
                f.write(data)
            os.replace(tmp, host)
        self._own(host, owner, mode)
        self.manifest.record(self.volume, path, 'file', *owner, mode, hashlib.sha256(data).hexdigest(), len(data),
                             tweak=self.tweak)
        return changed

    def write_plist(self, path, value, owner=ROOT, mode=0o644, binary=True, parent_owner=None):
        fmt = plistlib.FMT_BINARY if binary else plistlib.FMT_XML
        return self.write(path, plistlib.dumps(value, fmt=fmt, sort_keys=True), owner, mode, parent_owner)

    def merge_plist(self, path, values, owner=ROOT, mode=0o644, binary=True, parent_owner=None):
        """Sets `values` in the dictionary at `path`, keeping its other keys. The keys that changed."""
        have = self.read_plist(path, {})
        if not isinstance(have, dict):
            raise TreeError(f'{self.volume}:{path}: not a dictionary')
        changed = sorted(k for k, v in values.items() if k not in have or have[k] != v or type(have[k]) is not type(v))
        have.update(values)
        self.write_plist(path, have, owner, mode, binary, parent_owner)
        return changed

    def symlink(self, path, target, owner=ROOT):
        self._writable(path)
        parent = os.path.dirname(path)
        if parent and not self.isdir(parent):
            self.mkdir(parent, owner)
        host = self.host(path, follow_last=False)
        if os.path.islink(host) and os.readlink(host) == target:
            changed = False
        else:
            if os.path.lexists(host):
                raise TreeError(f'{self.volume}:{path}: in the way of a link to {target}')
            os.symlink(target, host)
            changed = True
        self._own(host, owner, None)
        self.manifest.record(self.volume, path, 'link', *owner, target=target, tweak=self.tweak)
        return changed

    def copy_in(self, source, path, owner=ROOT, mode=None):
        """Copies a host file or directory to `path`. Modes come from the source unless `mode` is given
        (files only): executable stays executable. Links inside a copied directory are refused."""
        self._writable(path)
        if os.path.islink(source):
            raise TreeError(f'{source}: a symbolic link is not copied')
        if os.path.isdir(source):
            self.mkdir(path, owner, 0o755)
            for name in sorted(os.listdir(source)):
                self.copy_in(os.path.join(source, name), f'{path}/{name}', owner)
            return
        if not os.path.isfile(source):
            raise TreeError(f'{source}: not a regular file')
        with open(source, 'rb') as f:
            data = f.read()
        if mode is None:
            mode = 0o755 if os.stat(source).st_mode & stat.S_IXUSR else 0o644
        self.write(path, data, owner, mode)

    def remove(self, path):
        """Removes a file or link. True when there was one."""
        self._writable(path)
        host = self.host(path, follow_last=False)
        if not os.path.lexists(host):
            return False
        if os.path.isdir(host) and not os.path.islink(host):
            raise TreeError(f'{self.volume}:{path}: a directory is not removed')
        os.remove(host)
        self.manifest.record(self.volume, path, 'removed', tweak=self.tweak)
        return True
