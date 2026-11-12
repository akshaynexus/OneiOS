"""Tests of the guest helpers' logic, built for the host with cc and -DONEIOS_HOST_TEST (they are plain POSIX C; the
guest's Darwin parts, notify, libproc and the bootstrap port, are left out of a host build and are not run here).
Linux or macOS.

    python3 -I userland/tests/test_helpers.py
"""
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HELPERS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'helpers')
CC = shutil.which('cc')
FLAGS = ['-std=c99', '-D_DEFAULT_SOURCE', '-D_POSIX_C_SOURCE=200809L', '-DONEIOS_HOST_TEST', '-O1', '-Wall', '-Wextra', '-Werror']
if sys.platform == 'darwin':
    FLAGS.append('-D_DARWIN_C_SOURCE')       # flock, setgroups, O_NOFOLLOW are BSD's, hidden by _POSIX_C_SOURCE


def wait_for(check, seconds=10):
    end = time.time() + seconds
    while time.time() < end:
        if check():
            return True
        time.sleep(0.05)
    return False


@unittest.skipUnless(CC, 'no C compiler')
class Helpers(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory()
        for name in ('oneios-init', 'oneios-bootstat', 'oneios-seed', 'oneios-thermal'):
            subprocess.run([CC, *FLAGS, os.path.join(HELPERS, name + '.c'), '-o', os.path.join(cls.build.name, name)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.build.cleanup()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.tmp.name, 'oneios')
        for d in ('services.d', 'enabled'):
            os.makedirs(os.path.join(self.root, d))
        self.procs = []

    def tearDown(self):
        for p in self.procs:
            if p.poll() is None:
                p.kill()
            p.wait()
        self.tmp.cleanup()

    def service(self, name, lines, enabled=True, mode=0o644):
        path = os.path.join(self.root, 'services.d', name + '.conf')
        with open(path, 'w') as f:
            f.write('\n'.join(lines) + '\n')
        os.chmod(path, mode)
        if enabled:
            open(os.path.join(self.root, 'enabled', name), 'w').close()

    def init(self, **env):
        p = subprocess.Popen([os.path.join(self.build.name, 'oneios-init'), self.root],
                             env={**os.environ, **env}, stderr=subprocess.DEVNULL)
        self.procs.append(p)
        return p

    def status(self):
        try:
            with open(os.path.join(self.root, 'state/status')) as f:
                return {line.split()[0]: line.split()[1:] for line in f if line.strip()}
        except OSError:
            return {}

    def log(self, name):
        try:
            with open(os.path.join(self.root, 'log', name + '.log')) as f:
                return f.read()
        except OSError:
            return ''

    def test_services_run_once_or_are_kept_alive(self):
        self.service('once', ['# a comment', 'arg=/bin/sh', 'arg=-c', 'arg=echo ran $GREETING', 'env=GREETING=hello', 'bootstrap=0'])
        self.service('again', ['arg=/bin/sh', 'arg=-c', 'arg=echo tick; exit 3', 'keepalive=1', 'throttle=1', 'bootstrap=0'])
        self.service('off', ['arg=/bin/sh', 'arg=-c', 'arg=echo never'], enabled=False)
        p = self.init()
        self.assertTrue(wait_for(lambda: self.log('again').count('tick') >= 2, 15), self.log('oneios-init'))
        self.assertEqual(self.log('once'), 'ran hello\n')
        self.assertEqual(self.log('off'), '')
        status = self.status()
        self.assertEqual(status['once'][1:], ['done', '1', '0'])
        self.assertEqual(status['off'][1], 'disabled')
        self.assertEqual(status['again'][3], '3')
        self.assertGreaterEqual(int(status['again'][2]), 2)
        # at least the throttle apart: two starts in under a second would be a spin
        self.assertLessEqual(self.log('again').count('tick'), 4)
        # a second manager on the same root exits
        second = subprocess.run([os.path.join(self.build.name, 'oneios-init'), self.root], capture_output=True, text=True)
        self.assertEqual(second.returncode, 1)
        self.assertIn('another oneios-init is running', second.stderr)
        p.send_signal(signal.SIGTERM)
        self.assertEqual(p.wait(10), 0)
        self.assertIn('stopping', self.log('oneios-init'))

    def test_a_long_running_service_is_stopped_with_the_manager_and_reloaded_on_hup(self):
        self.service('sleeper', ['arg=/bin/sh', 'arg=-c', 'arg=echo up; exec sleep 600', 'keepalive=1', 'bootstrap=0'])
        p = self.init()
        self.assertTrue(wait_for(lambda: self.status().get('sleeper', ['0', ''])[1] == 'running'))
        pid = int(self.status()['sleeper'][0])
        self.service('late', ['arg=/bin/sh', 'arg=-c', 'arg=echo late', 'bootstrap=0'])
        os.remove(os.path.join(self.root, 'enabled/sleeper'))
        p.send_signal(signal.SIGHUP)
        self.assertTrue(wait_for(lambda: self.log('late') == 'late\n'), self.log('oneios-init'))

        def gone():
            try:
                os.kill(pid, 0)
                return False
            except ProcessLookupError:
                return True
        self.assertTrue(wait_for(gone), 'the disabled service was not stopped')
        self.assertTrue(wait_for(lambda: self.status()['sleeper'][1] == 'disabled'))
        p.send_signal(signal.SIGTERM)
        self.assertEqual(p.wait(10), 0)

    def test_services_wait_for_the_bootstrap_port(self):
        port = os.path.join(self.tmp.name, 'port')
        self.service('needs', ['arg=/bin/sh', 'arg=-c', 'arg=echo with launchd'])
        self.service('early', ['arg=/bin/sh', 'arg=-c', 'arg=echo before launchd', 'bootstrap=0'])
        p = self.init(ONEIOS_FAKE_BOOTSTRAP=port)
        self.assertTrue(wait_for(lambda: self.log('early') == 'before launchd\n'))
        time.sleep(1.2)
        self.assertEqual(self.log('needs'), '')
        self.assertEqual(self.status()['needs'][1], 'waiting')
        open(port, 'w').close()
        self.assertTrue(wait_for(lambda: self.log('needs') == 'with launchd\n', 5))
        p.send_signal(signal.SIGTERM)
        p.wait(10)

    def test_a_service_file_others_can_write_or_that_is_malformed_is_ignored(self):
        self.service('loose', ['arg=/bin/sh', 'arg=-c', 'arg=echo loose', 'bootstrap=0'], mode=0o666)
        self.service('relative', ['arg=sh', 'arg=-c', 'arg=echo relative', 'bootstrap=0'])
        self.service('junk', ['this is not a service'])
        self.service('unknown', ['arg=/bin/true', 'colour=blue'])
        self.service('good', ['arg=/bin/sh', 'arg=-c', 'arg=echo good', 'bootstrap=0'])
        open(os.path.join(self.root, 'services.d', 'not-a-service.txt'), 'w').close()
        p = self.init()
        self.assertTrue(wait_for(lambda: self.log('good') == 'good\n'))
        time.sleep(0.3)
        for name in ('loose', 'relative', 'junk', 'unknown'):
            self.assertEqual(self.log(name), '', name)
            self.assertNotIn(name, self.status())
        self.assertIn('loose: services.d/loose.conf must belong to uid', self.log('oneios-init'))
        p.send_signal(signal.SIGTERM)
        p.wait(10)

    def test_a_program_that_is_not_there(self):
        self.service('missing', ['arg=/nonexistent/program', 'bootstrap=0'])
        p = self.init()
        self.assertTrue(wait_for(lambda: self.status().get('missing', ['', '', '', ''])[3] == '127'))
        self.assertIn('exec /nonexistent/program', self.log('missing'))
        p.send_signal(signal.SIGTERM)
        p.wait(10)

    def bootstat(self, states, polls=3):
        fake = os.path.join(self.tmp.name, 'notify')
        os.makedirs(fake, exist_ok=True)
        for name, value in states.items():
            with open(os.path.join(fake, name), 'w') as f:
                f.write(str(value))
        os.makedirs(os.path.join(self.root, 'state'), exist_ok=True)
        console = os.path.join(self.tmp.name, 'console')
        out = subprocess.run([os.path.join(self.build.name, 'oneios-bootstat'), '-n', str(polls), '-i', '10'],
                             env={'ONEIOS_FAKE_NOTIFY': fake, 'ONEIOS_ROOT': self.root, 'ONEIOS_CONSOLE': console},
                             capture_output=True, text=True, check=True).stdout
        with open(console) as f:
            self.assertEqual(f.read(), out)
        with open(os.path.join(self.root, 'state/boot')) as f:
            last = f.read().split()[0]
        return [line.split()[1].split('=')[1] + ''.join(' ' + w for w in line.split()[3:]) for line in out.splitlines()], last

    def test_bootstat_reports_the_stages_in_order(self):
        self.assertEqual(self.bootstat({}), (['bootstat-up'], 'bootstat-up'))
        self.assertEqual(self.bootstat({'proc.backboardd': 1}), (['bootstat-up', 'backboardd-up'], 'backboardd-up'))
        # finishedstartup left over from an earlier SpringBoard counts only once the process is there
        self.assertEqual(self.bootstat({'com.apple.springboard.finishedstartup': 1})[1], 'backboardd-up')
        stages, last = self.bootstat({'proc.SpringBoard': 1, 'com.apple.springboard.lockstate': 1})
        self.assertEqual(stages, ['bootstat-up', 'backboardd-up', 'springboard-process', 'springboard-up', 'screen locked=1 dark=0'])
        self.assertEqual(last, 'springboard-up')
        usage = subprocess.run([os.path.join(self.build.name, 'oneios-bootstat'), '--bogus'], capture_output=True)
        self.assertEqual(usage.returncode, 2)

    # ---- oneios-thermal ----------------------------------------------------------------------------------------
    def test_thermal_copies_the_hosts_level_and_keeps_it(self):
        fake = os.path.join(self.tmp.name, 'thermal')
        os.makedirs(fake)
        run = lambda polls: subprocess.run([os.path.join(self.build.name, 'oneios-thermal'), '-n', str(polls), '-i', '0'],
                                           env={'ONEIOS_FAKE_THERMAL': fake}, capture_output=True, text=True, check=True).stdout
        def state():
            try:
                with open(os.path.join(fake, 'state')) as f:
                    return f.read().strip()
            except FileNotFoundError:
                return None
        self.assertIn('no-service', run(2))                     # an older kernel: said once, nothing set
        self.assertEqual(run(2).count('no-service'), 1)
        self.assertFalse(os.path.exists(os.path.join(fake, 'state')))
        with open(os.path.join(fake, 'service'), 'w') as f:
            f.write('30 1 4420 7100\n')
        out = run(1)
        self.assertIn('level=30 name=heavy step=1 case=44.2 soc=71.0', out)
        self.assertEqual(state(), '30')
        # someone else (thermalmonitord) sets nominal while the host says heavy: set back, said once
        os.unlink(os.path.join(fake, 'state'))
        p = subprocess.Popen([os.path.join(self.build.name, 'oneios-thermal'), '-n', '40', '-i', '0'], env={'ONEIOS_FAKE_THERMAL': fake},
                             stdout=subprocess.PIPE, text=True)
        self.assertTrue(wait_for(lambda: state() == '30'))
        with open(os.path.join(fake, 'state'), 'w') as f:
            f.write('0\n')
        out = p.communicate(timeout=20)[0]
        self.assertEqual(out.count('overwritten state=0'), 1)
        self.assertEqual(state(), '30')
        # a new level from the host is set and says the old one
        with open(os.path.join(fake, 'service'), 'w') as f:
            f.write('0 0 3500 4000\n')
        os.unlink(os.path.join(fake, 'state'))
        p = subprocess.Popen([os.path.join(self.build.name, 'oneios-thermal'), '-n', '200', '-i', '0'], env={'ONEIOS_FAKE_THERMAL': fake},
                             stdout=subprocess.PIPE, text=True)
        self.assertTrue(wait_for(lambda: state() == '0'))
        with open(os.path.join(fake, 'service'), 'w') as f:
            f.write('40 2 4650 9000\n')
        out = p.communicate(timeout=20)[0]
        self.assertIn('level=40 name=trapping step=2 case=46.5 soc=90.0 was=0', out)
        self.assertEqual(state(), '40')

    # ---- oneios-seed -------------------------------------------------------------------------------------------
    def seed(self, entries, fmt=None):
        """A tar as image-build writes the Data seed: pax, numeric owners, xattrs as LIBARCHIVE.xattr records."""
        import io
        import tarfile
        path = os.path.join(self.tmp.name, 'data.seed')
        with tarfile.open(path, 'w', format=fmt or tarfile.PAX_FORMAT) as tar:
            for name, kind, extra in entries:
                info = tarfile.TarInfo(name)
                info.uid, info.gid, info.mode = extra.get('uid', 0), extra.get('gid', 0), extra.get('mode', 0o644)
                info.uname = info.gname = ''
                info.pax_headers = extra.get('pax', {})
                if kind == 'dir':
                    info.type = tarfile.DIRTYPE
                    tar.addfile(info)
                elif kind == 'link':
                    info.type, info.linkname = tarfile.SYMTYPE, extra['target']
                    tar.addfile(info)
                elif kind == 'hard':
                    info.type, info.linkname = tarfile.LNKTYPE, extra['target']
                    tar.addfile(info)
                else:
                    data = extra.get('data', b'')
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
        return path

    def unpack(self, seed, *args):
        target = os.path.join(self.tmp.name, 'volume')
        os.makedirs(target, exist_ok=True)
        done = subprocess.run([os.path.join(self.build.name, 'oneios-seed'), *args, seed, target], capture_output=True, text=True)
        return done, target

    def test_seed_unpacks_files_folders_links_modes_and_long_names(self):
        long_name = 'containers/' + '/'.join(['a-folder-with-a-long-name'] * 6) + '/file-with-a-name-longer-than-a-ustar-header-holds.plist'
        big = os.urandom(200000)
        entries = [
            ('mobile', 'dir', {'mode': 0o755, 'uid': 501, 'gid': 501}),
            ('mobile/Library', 'dir', {'mode': 0o700}),
            ('mobile/Library/a.plist', 'file', {'data': b'bplist00 stand-in', 'mode': 0o600, 'uid': 501, 'gid': 501}),
            ('mobile/empty', 'file', {'mode': 0o644}),
            ('mobile/big', 'file', {'data': big}),
            ('mobile/tool', 'file', {'data': b'#!', 'mode': 0o4755}),
            ('tmp', 'dir', {'mode': 0o1777}),
            ('link', 'link', {'target': 'mobile/Library'}),
        ]
        parts = long_name.split('/')
        entries += [('/'.join(parts[:i]), 'dir', {'mode': 0o755}) for i in range(1, len(parts))]
        entries.append((long_name, 'file', {'data': b'deep'}))
        seed = self.seed(entries)
        digest = 'ab' * 32
        done, target = self.unpack(seed, '--hash', digest)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn('5 files (200023 bytes), 10 folders, 1 links', done.stdout)
        with open(os.path.join(target, 'mobile/big'), 'rb') as f:
            self.assertEqual(f.read(), big)
        with open(os.path.join(target, long_name), 'rb') as f:
            self.assertEqual(f.read(), b'deep')
        self.assertEqual(os.path.getsize(os.path.join(target, 'mobile/empty')), 0)
        self.assertEqual(os.readlink(os.path.join(target, 'link')), 'mobile/Library')
        mode = lambda p: os.lstat(os.path.join(target, p)).st_mode & 0o7777  # noqa: E731
        self.assertEqual([mode('mobile/Library'), mode('mobile/Library/a.plist'), mode('tmp'), mode('mobile/tool')],
                         [0o700, 0o600, 0o1777, 0o4755])
        with open(os.path.join(target, '.oneios-seed')) as f:
            self.assertEqual(f.read(), digest + '\n')
        # the same seed again: left alone; another seed: refused, nothing touched
        os.remove(os.path.join(target, 'mobile/empty'))
        again, _ = self.unpack(seed, '--hash', digest)
        self.assertEqual((again.returncode, os.path.exists(os.path.join(target, 'mobile/empty'))), (0, False))
        other, _ = self.unpack(seed, '--hash', 'cd' * 32)
        self.assertEqual(other.returncode, 3)
        self.assertEqual(self.unpack(seed, '--hash', 'XYZ')[0].returncode, 2)
        # GNU long names read as well
        import tarfile
        shutil.rmtree(target)
        done, target = self.unpack(self.seed(entries, tarfile.GNU_FORMAT))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertTrue(os.path.isfile(os.path.join(target, long_name)))
        self.assertFalse(os.path.exists(os.path.join(target, '.oneios-seed')))

    def test_seed_sets_extended_attributes(self):
        import base64
        value = bytes(range(256))
        seed = self.seed([('f', 'file', {'data': b'x', 'pax': {'LIBARCHIVE.xattr.user.oneios.test': base64.b64encode(value).decode()}})])
        done, target = self.unpack(seed)
        self.assertEqual(done.returncode, 0, done.stderr)
        try:
            if hasattr(os, 'getxattr'):
                have = os.getxattr(os.path.join(target, 'f'), 'user.oneios.test')
            else:                                # macOS: Python has no getxattr there
                have = bytes.fromhex(subprocess.run(['xattr', '-px', 'user.oneios.test', os.path.join(target, 'f')],
                                                    capture_output=True, text=True, check=True).stdout)
        except (OSError, subprocess.CalledProcessError):
            self.assertIn('1 extended attributes NOT set', done.stdout)  # a file system without user xattrs: said, not hidden
            return
        self.assertEqual(have, value)
        self.assertIn('1 extended attributes', done.stdout)

    def test_seed_refuses_what_could_leave_the_volume(self):
        outside = os.path.join(self.tmp.name, 'outside')
        os.mkdir(outside)
        for entries, words in (
            ([('../escape', 'file', {'data': b'x'})], '..'),
            ([('a/../../escape', 'file', {'data': b'x'})], '..'),
            ([('/abs', 'file', {'data': b'x'})], 'absolute'),
            ([('out', 'link', {'target': outside}), ('out/escape', 'file', {'data': b'x'})], 'through a symbolic link'),
            ([('a', 'file', {'data': b'x'}), ('b', 'hard', {'target': 'a'})], 'not a file, folder or symbolic link'),
        ):
            shutil.rmtree(os.path.join(self.tmp.name, 'volume'), ignore_errors=True)
            done, target = self.unpack(self.seed(entries), '--hash', 'ab' * 32)
            self.assertEqual(done.returncode, 1, entries)
            self.assertIn(words, done.stderr)
            self.assertEqual(os.listdir(outside), [])
            self.assertFalse(os.path.exists(os.path.join(self.tmp.name, 'escape')))
            self.assertFalse(os.path.exists(os.path.join(target, '.oneios-seed')), 'a failed unpack must not be marked done')

    def test_seed_refuses_a_damaged_archive(self):
        seed = self.seed([('a', 'file', {'data': b'x' * 5000}), ('b', 'file', {'data': b'y'})])
        with open(seed, 'rb') as f:
            data = f.read()
        cut = os.path.join(self.tmp.name, 'cut.seed')
        with open(cut, 'wb') as f:
            f.write(data[:3000])
        done, _ = self.unpack(cut)
        self.assertEqual(done.returncode, 1)
        self.assertIn('ends inside an entry', done.stderr)
        with open(cut, 'wb') as f:
            f.write(b'\x01' * 2048)
        done, _ = self.unpack(cut)
        self.assertEqual(done.returncode, 1)
        self.assertTrue(done.stderr.startswith('oneios-seed: a header'), done.stderr)
        with open(cut, 'wb') as f:
            f.write(b'\0' * 4096)
        self.assertEqual(self.unpack(cut)[0].returncode, 1)


if __name__ == '__main__':
    unittest.main()
