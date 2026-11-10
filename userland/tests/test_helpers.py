"""Tests of the guest helpers' logic, built for the host with cc (they are plain POSIX C; the Darwin parts are
behind __APPLE__ and are not run here).

    python3 -I userland/tests/test_helpers.py
"""
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

HELPERS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'helpers')
CC = shutil.which('cc')
FLAGS = ['-std=c99', '-D_DEFAULT_SOURCE', '-D_POSIX_C_SOURCE=200809L', '-O1', '-Wall', '-Wextra', '-Werror']


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
        for name in ('oneios-init', 'oneios-bootstat'):
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


if __name__ == '__main__':
    unittest.main()
