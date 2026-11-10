"""Tests of roottweak on a generated fake root (tests/fakeroot.py). No file of Apple's is used.

    python3 -I userland/tests/test_roottweak.py        (from the repository's top)
"""
import hashlib
import json
import os
import plistlib
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import fakeroot  # noqa: E402
from roottweak import cli, config, launchd, macho  # noqa: E402
from roottweak.tree import Tree, TreeError  # noqa: E402


REPORT = 'oneios/roottweak-report.txt'  # says what this run did, so it differs between a first and a second run


def digest(top):
    """Every path of a tree with its type, link target or content hash; the run's own report left out."""
    out = {}
    for current, dirs, files in os.walk(top):
        for name in dirs + files:
            full = os.path.join(current, name)
            rel = os.path.relpath(full, top)
            if rel.endswith(REPORT):
                continue
            if os.path.islink(full):
                out[rel] = 'link ' + os.readlink(full)
            elif os.path.isdir(full):
                out[rel] = 'dir'
            else:
                with open(full, 'rb') as f:
                    out[rel] = hashlib.sha256(f.read()).hexdigest()
    return out


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.top = self.tmp.name
        self.root = fakeroot.make(os.path.join(self.top, 'root'))
        self.data = os.path.join(self.top, 'data')
        self.out = os.path.join(self.top, 'out')
        os.mkdir(self.data)

    def tearDown(self):
        self.tmp.cleanup()

    def helpers(self, names=('oneios-init', 'oneios-bootstat', 'oneios-mg', 'dropbear'), **kind):
        directory = os.path.join(self.top, 'helpers')
        os.makedirs(directory, exist_ok=True)
        hashes = {}
        for name in names:
            data, cdhash = fakeroot.signed_macho('com.oneios.' + name, **kind)
            with open(os.path.join(directory, name), 'wb') as f:
                f.write(data)
            os.chmod(os.path.join(directory, name), 0o755)
            hashes[name] = cdhash
        return directory, hashes

    def apply(self, *args, layout='sealed', expect=0):
        argv = ['apply', '--root', self.root, '--out', self.out, '--layout', layout]
        if layout == 'sealed':
            argv += ['--data', self.data]
        stdout, stderr = sys.stdout, sys.stderr
        sys.stdout = sys.stderr = open(os.path.join(self.top, 'log'), 'w')
        try:
            status = cli.main(argv + list(args))
        finally:
            sys.stdout.close()
            sys.stdout, sys.stderr = stdout, stderr
        with open(os.path.join(self.top, 'log')) as f:
            self.log = f.read()
        self.assertEqual(status, expect, self.log)
        return status

    def read(self, name):
        with open(os.path.join(self.out, name)) as f:
            return json.load(f)

    def plist(self, base, path):
        with open(os.path.join(base, path), 'rb') as f:
            return plistlib.load(f)

    def states(self):
        return {r['item']: r['state'] for r in self.read('report.json')['results']}

    def entry(self, volume, path):
        found = [e for e in self.read('tweaks.manifest.json')['entries'] if e['volume'] == volume and e['path'] == path]
        self.assertEqual(len(found), 1, f'{volume}:{path}')
        return found[0]


class Policy(unittest.TestCase):
    def test_the_policy_does_not_contradict_itself(self):
        d = config.daemons()
        chosen = d.selected()
        self.assertFalse(set(chosen) & d.keep)
        self.assertFalse(set(chosen) & set(d.idle))
        # vphone's base list of 134, less generativeexperiencesd (kept) and the four OTA jobs (replace)
        self.assertEqual(len(d.groups['base']['labels']), 129)
        for label in ('com.apple.SpringBoard', 'com.apple.backboardd', 'com.apple.CommCenter', 'com.apple.sleepd',
                      'com.apple.generativeexperiencesd', 'com.apple.bluetoothd', 'com.apple.locationd'):
            self.assertIn(label, d.keep)
        for label in ('com.apple.watchdogd', 'com.apple.nfcd', 'com.apple.seld', 'com.apple.momentsd', 'com.apple.fairplayd.H2',
                      'com.apple.identityservicesd', 'com.apple.accessoryupdaterd', 'com.apple.aned'):
            self.assertIn(label, chosen)
        # what costs a feature is off unless asked for
        for label in ('com.apple.apsd', 'com.apple.chronod', 'com.apple.appstored'):
            self.assertNotIn(label, chosen)
        self.assertIn('com.apple.apsd', d.selected(groups=['fewest']))
        self.assertNotIn('com.apple.nfcd', d.selected(without=['base']))
        self.assertNotIn('com.apple.weatherd', d.selected(keep=['com.apple.weatherd']))
        with self.assertRaises(config.ConfigError):
            d.selected(groups=['nonesuch'])

    def test_a_policy_that_disables_what_it_keeps_is_refused(self):
        raw = {'keep': {'labels': ['a']}, 'replace': {'labels': {}}, 'group': {'g': {'labels': ['a'], 'source': 's'}}}
        with self.assertRaises(config.ConfigError):
            config.Daemons(raw)

    def test_the_device_is_an_iphone_at_3x_in_portrait(self):
        d = config.device()
        self.assertEqual((d['device']['product'], d['device']['device_class']), ('iPhone18,2', 'iPhone'))
        self.assertEqual((d['display']['scale'], d['display']['orientation']), (3, 'portrait'))
        self.assertEqual([d['display']['width'] // 3, d['display']['height'] // 3], d['display']['points'])
        self.assertEqual(d['adt']['product']['artwork-device-idiom'], 'phone')
        self.assertIn('baseband', d['adt']['absent'])
        self.assertEqual(d['mobilegestalt']['expect']['DeviceClass'], 'iPhone')

    def test_every_preference_names_its_source(self):
        prefs = config.prefs()
        self.assertTrue(all(p['source'] for p in prefs))
        groups = {p['group'] for p in prefs}
        self.assertEqual(groups, {'setup', 'migration', 'watchdogs', 'awake', 'coreanimation', 'visuals'})


class Sealed(Case):
    def test_nothing_is_written_to_the_os_volume(self):
        before = digest(self.root)
        helpers, hashes = self.helpers()
        self.apply('--helpers', helpers)
        self.assertEqual(digest(self.root), before)
        self.assertFalse([e for e in self.read('tweaks.manifest.json')['entries'] if e['volume'] == 'root'])

    def test_disabled_jobs_go_to_the_data_volume_as_roots_file(self):
        self.apply()
        disabled = self.plist(self.data, 'db/com.apple.xpc.launchd/disabled.plist')
        self.assertEqual(set(disabled), {
            'com.apple.watchdogd', 'com.apple.mobile.softwareupdated', 'com.apple.OTATaskingAgent', 'com.apple.analyticsd',
            'com.apple.nfcd', 'com.apple.seld', 'com.apple.assistantd', 'com.apple.nearbyd', 'com.apple.momentsd',
            'com.apple.coreduetd'})
        self.assertTrue(all(v is True for v in disabled.values()))
        # launchd ignores the file unless it is root's, 0644, with the marker beside it
        entry = self.entry('data', 'db/com.apple.xpc.launchd/disabled.plist')
        self.assertEqual((entry['uid'], entry['gid'], entry['mode']), (0, 0, '0644'))
        self.assertTrue(os.path.isfile(os.path.join(self.data, 'db/com.apple.xpc.launchd/disabled.migrated')))
        contract = self.read('boot-contract.json')
        self.assertEqual(sorted(contract['launch_policy']['disable_jobs']), sorted(disabled))
        # iOS 27's DisableAllowed = false: said, since a sealed root cannot change it
        self.assertEqual(contract['launch_policy']['force_enabled'],
                         ['com.apple.OTATaskingAgent', 'com.apple.analyticsd', 'com.apple.mobile.softwareupdated'])
        # labels the fake root does not have are reported, not written
        missing = [r for r in self.read('report.json')['results'] if r['item'] == 'jobs not in this root'][0]
        self.assertIn('com.apple.weatherd', missing['labels'])
        self.assertNotIn('com.apple.weatherd', disabled)
        # nothing kept is disabled, and a job the policy does not know stays
        for label in ('com.apple.SpringBoard', 'com.apple.CommCenter', 'com.apple.sleepd', 'com.apple.example.unknownd',
                      'com.apple.generativeexperiencesd', 'com.apple.biometrickitd'):
            self.assertNotIn(label, disabled)

    def test_an_override_already_there_stays(self):
        os.makedirs(os.path.join(self.data, 'db/com.apple.xpc.launchd'))
        with open(os.path.join(self.data, 'db/com.apple.xpc.launchd/disabled.plist'), 'wb') as f:
            plistlib.dump({'com.example.theirs': True, 'com.example.on': False}, f)
        self.apply()
        disabled = self.plist(self.data, 'db/com.apple.xpc.launchd/disabled.plist')
        self.assertIs(disabled['com.example.theirs'], True)
        self.assertIs(disabled['com.example.on'], False)
        self.assertIs(disabled['com.apple.nfcd'], True)

    def test_groups_keep_and_disable_by_hand(self):
        self.apply('--no-group', 'base', '--keep', 'com.apple.momentsd', '--disable', 'com.apple.CommCenter',
                   '--disable', 'com.apple.example.unknownd')
        disabled = self.plist(self.data, 'db/com.apple.xpc.launchd/disabled.plist')
        self.assertNotIn('com.apple.nfcd', disabled)
        self.assertNotIn('com.apple.momentsd', disabled)
        self.assertIn('com.apple.CommCenter', disabled)
        self.assertIn('com.apple.example.unknownd', disabled)
        self.assertIn('warning: com.apple.CommCenter', self.states())
        self.apply('--group', 'nonesuch', expect=1)

    def test_preferences_are_seeded_with_owners_and_the_build(self):
        self.apply()
        prefs = 'mobile/Library/Preferences/'
        self.assertEqual(self.plist(self.data, prefs + 'com.apple.purplebuddy.plist'),
                         {'SetupDone': True, 'SetupFinishedAllSteps': True, 'SetupVersion': 11, 'daemonizeNeedsToRun': False})
        migration = self.plist(self.data, prefs + 'com.apple.migration.plist')
        self.assertEqual(migration['LastSystemVersion'], fakeroot.BUILD)
        self.assertEqual(migration['DMLastMigrationResults'], {'buildVersion': fakeroot.BUILD, 'success': True})
        springboard = self.plist(self.data, prefs + 'com.apple.springboard.plist')
        self.assertEqual(springboard['FBLaunchWatchdogScale'], 4.0)
        self.assertIs(springboard['BKNoWatchdogs'], True)
        self.assertEqual(springboard['SBMinimumLockscreenIdleTime'], 2147483647)
        self.assertEqual(springboard['FBLaunchWatchdogExceptions'], {'com.apple.purplebuddy': 100.0})
        for path in (prefs + 'com.apple.backboardd.plist', prefs + '.GlobalPreferences.plist',
                     'root/Library/Preferences/com.apple.backboardd.plist', 'preferences/.GlobalPreferences.plist'):
            self.assertIs(self.plist(self.data, path)['CAEnableDeepFramebuffer'], False, path)
        self.assertIs(self.plist(self.data, prefs + 'com.apple.coreanimation.plist')['CAEnableMedina'], False)
        entry = self.entry('data', prefs + 'com.apple.springboard.plist')
        self.assertEqual((entry['uid'], entry['gid'], entry['mode']), (501, 501, '0600'))
        self.assertEqual(self.entry('data', 'mobile/Library/Preferences')['uid'], 501)
        self.assertEqual(self.entry('data', 'preferences/.GlobalPreferences.plist')['uid'], 0)
        with open(os.path.join(self.data, prefs + 'com.apple.springboard.plist'), 'rb') as f:
            self.assertTrue(f.read().startswith(b'bplist00'))
        # Reduce Motion only when asked
        self.assertFalse(os.path.exists(os.path.join(self.data, prefs + 'com.apple.Accessibility.plist')))
        self.apply('--lighter-visuals', '--no-awake')
        self.assertIs(self.plist(self.data, prefs + 'com.apple.Accessibility.plist')['ReduceMotionEnabled'], True)
        self.assertEqual(self.states()['preferences: awake'], 'off')

    def test_keys_of_somebody_else_stay_and_a_broken_file_is_not_written_over(self):
        path = os.path.join(self.data, 'mobile/Library/Preferences')
        os.makedirs(path)
        with open(os.path.join(path, 'com.apple.springboard.plist'), 'wb') as f:
            plistlib.dump({'SBIconVisibility': True, 'FBLaunchWatchdogScale': 20}, f)
        self.apply()
        springboard = self.plist(self.data, 'mobile/Library/Preferences/com.apple.springboard.plist')
        self.assertIs(springboard['SBIconVisibility'], True)
        self.assertEqual(springboard['FBLaunchWatchdogScale'], 4.0)
        with open(os.path.join(path, 'com.apple.purplebuddy.plist'), 'wb') as f:
            f.write(b'not a plist')
        self.apply(expect=1)
        self.assertIn('not a property list', self.log)
        with open(os.path.join(path, 'com.apple.purplebuddy.plist'), 'rb') as f:
            self.assertEqual(f.read(), b'not a plist')

    def test_a_second_run_changes_nothing(self):
        helpers, _ = self.helpers()
        self.apply('--helpers', helpers)
        def entries():
            return [e for e in self.read('tweaks.manifest.json')['entries'] if e['path'] != REPORT]
        first, manifest = digest(self.data), entries()
        self.apply('--helpers', helpers)
        self.assertEqual(digest(self.data), first)
        self.assertEqual(entries(), manifest)
        states = self.states()
        for item in ('disabled.plist', 'preferences: setup', 'preferences: coreanimation', 'data layout'):
            self.assertEqual(states[item], 'kept', item)

    def test_the_data_layout(self):
        cache = 'containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches'
        os.makedirs(os.path.join(self.data, cache))
        with open(os.path.join(self.data, cache, 'com.apple.MobileGestalt.plist'), 'wb') as f:
            plistlib.dump({'CacheExtra': {'h9jDsbgj7xIVeIQ8S3/X3Q': 'iPad17,3'}}, f)
        self.apply()
        self.assertFalse(os.path.exists(os.path.join(self.data, cache, 'com.apple.MobileGestalt.plist')))
        self.assertEqual(self.states()['MobileGestalt cache'], 'applied')
        self.assertEqual(self.entry('data', 'containers/Bundle')['uid'], 33)
        self.assertEqual(self.entry('data', 'tmp')['mode'], '1777')
        self.assertEqual(self.entry('data', 'keybags')['mode'], '0700')
        logd = self.entry('data', 'db/diagnostics/.migrated')
        self.assertEqual((logd['uid'], logd['mode']), (272, '0644'))
        self.assertEqual(self.entry('data', 'db/diagnostics')['mode'], '0751')
        # no keybag unless asked: UniXNU's key store wants the folder empty
        self.assertEqual(os.listdir(os.path.join(self.data, 'keybags')), [])
        self.apply('--plain-keybag')
        self.assertEqual(self.plist(self.data, 'keybags/systembag.kb'), {'KeyBagKeys': b'SWEET'})
        self.assertEqual(self.entry('data', 'keybags/systembag.kb')['mode'], '0600')

    def test_our_services_and_the_trust_list(self):
        helpers, hashes = self.helpers()
        self.apply('--helpers', helpers)
        base = os.path.join(self.data, 'oneios')
        self.assertEqual(sorted(os.listdir(os.path.join(base, 'enabled'))), ['bootstat', 'dropbear', 'identity'])
        with open(os.path.join(base, 'services.d/dropbear.conf')) as f:
            conf = f.read().splitlines()
        self.assertEqual(conf[:6], ['arg=/private/var/oneios/bin/dropbear', 'arg=-F', 'arg=-E', 'arg=-R', 'arg=-p', 'arg=2222'])
        self.assertIn('keepalive=1', conf)
        with open(os.path.join(base, 'identity.expect')) as f:
            expect = f.read()
        self.assertIn('DeviceClass=iPhone\n', expect)
        self.assertIn('main-screen-scale=3\n', expect)
        self.assertEqual(self.entry('data', 'oneios/bin/oneios-init')['mode'], '0755')
        self.assertEqual(self.entry('data', 'oneios/services.d/dropbear.conf')['mode'], '0644')
        trust = self.read('owner-trust.json')
        self.assertEqual(trust['cdhashes'], sorted(hashes.values()))
        self.assertEqual({f['path'] for f in trust['files']}, {f'/private/var/oneios/bin/{n}' for n in hashes})
        self.assertFalse(any(f['warnings'] for f in trust['files']))
        manager = self.read('boot-contract.json')['launch_policy']['service_manager']
        self.assertEqual((manager['path'], manager['present']), ('/private/var/oneios/bin/oneios-init', True))
        with open(os.path.join(base, 'roottweak-report.txt')) as f:
            self.assertIn('layout sealed', f.read())

    def test_without_helpers_the_services_are_placed_and_nothing_is_enabled(self):
        self.apply()
        base = os.path.join(self.data, 'oneios')
        self.assertEqual(os.listdir(os.path.join(base, 'enabled')), [])
        self.assertTrue(os.path.isfile(os.path.join(base, 'services.d/bootstat.conf')))
        self.assertEqual(self.states()['oneios-init'], 'off')
        self.assertIs(self.read('boot-contract.json')['launch_policy']['service_manager']['present'], False)
        self.assertEqual(self.read('owner-trust.json')['cdhashes'], [])

    def test_a_helper_that_is_not_signed_or_not_arm64e(self):
        directory = os.path.join(self.top, 'bad')
        os.mkdir(directory)
        with open(os.path.join(directory, 'oneios-init'), 'wb') as f:
            f.write(b'\x7fELF a host build')
        self.apply('--helpers', directory, expect=1)
        self.assertIn('not a Mach-O', self.log)
        helpers, _ = self.helpers(arm64e=False, page_shift=14)
        self.apply('--helpers', helpers)
        warnings = self.read('owner-trust.json')['files'][0]['warnings']
        self.assertTrue(any('not arm64e' in w for w in warnings) and any('16384' in w for w in warnings), warnings)

    def test_the_metal_plugin_goes_to_the_addon_view(self):
        bundle = os.path.join(self.top, 'NativeAdrenoMetal.bundle')
        os.makedirs(os.path.join(bundle, '_CodeSignature'))
        with open(os.path.join(bundle, 'Info.plist'), 'wb') as f:
            plistlib.dump({'CFBundleExecutable': 'NativeAdrenoMetal', 'NSPrincipalClass': 'NativeAdrenoMetalDevice'}, f)
        hashes = []
        for name in ('NativeAdrenoMetal', 'libvulkan_freedreno.dylib'):
            data, cdhash = fakeroot.signed_macho('org.example.' + name)
            hashes.append(cdhash)
            with open(os.path.join(bundle, name), 'wb') as f:
                f.write(data)
        with open(os.path.join(bundle, '_CodeSignature/CodeResources'), 'w') as f:
            f.write('resources')
        self.apply('--metal-plugin', bundle)
        base = os.path.join(self.data, 'oneios/addons/Extensions')
        self.assertEqual(sorted(os.listdir(base)), ['NativeAdrenoMetal.bundle', 'NativeAdrenoMetal.clients', 'NativeAdrenoMetal.publish'])
        with open(os.path.join(base, 'NativeAdrenoMetal.clients')) as f:
            self.assertEqual(f.read(), 'WidgetRenderer_Default\n*Poster*\niconservicesagent\nSBRendererService\n')
        self.assertEqual(self.entry('data', 'oneios/addons/Extensions/NativeAdrenoMetal.bundle/NativeAdrenoMetal')['mode'], '0755')
        self.assertEqual(self.entry('data', 'oneios/addons/Extensions/NativeAdrenoMetal.bundle/Info.plist')['mode'], '0644')
        contract = self.read('boot-contract.json')
        self.assertEqual(contract['addon_mounts'][0]['over'], '/System/Library/Extensions')
        self.assertEqual(contract['metal']['principal_class'], 'NativeAdrenoMetalDevice')
        self.assertEqual(self.read('owner-trust.json')['cdhashes'], sorted(hashes))
        self.apply('--metal-plugin', bundle, '--metal-client', '*')
        with open(os.path.join(base, 'NativeAdrenoMetal.clients')) as f:
            self.assertEqual(f.read(), '*\n')
        self.apply('--metal-plugin', self.top, expect=1)

    def test_the_app_cryptex_is_carried_on_the_data_volume(self):
        appos = os.path.join(self.top, 'appos')
        os.makedirs(os.path.join(appos, 'usr/libexec'))
        os.makedirs(os.path.join(appos, '.fseventsd'))
        with open(os.path.join(appos, 'usr/libexec/exampled'), 'wb') as f:
            f.write(b'stand-in')
        os.chmod(os.path.join(appos, 'usr/libexec/exampled'), 0o755)
        os.symlink('libexec/exampled', os.path.join(appos, 'usr/link'))
        self.apply('--appos', appos)
        base = os.path.join(self.data, 'oneios/preboot/Cryptexes')
        self.assertTrue(os.path.isfile(os.path.join(base, 'App/usr/libexec/exampled')))
        self.assertEqual(os.readlink(os.path.join(base, 'App/usr/link')), 'libexec/exampled')
        self.assertFalse(os.path.exists(os.path.join(base, 'App/.fseventsd')))
        self.assertTrue(os.path.isdir(os.path.join(base, 'OS/System')) and os.path.isdir(os.path.join(base, 'OS/usr')))
        self.assertEqual(self.entry('data', 'oneios/preboot/Cryptexes/App/usr/libexec/exampled')['mode'], '0755')
        mounts = self.read('boot-contract.json')['addon_mounts']
        self.assertEqual([m['over'] for m in mounts],
                         ['/private/preboot', '/private/preboot/Cryptexes/OS/System', '/private/preboot/Cryptexes/OS/usr'])

    def test_the_contract_names_the_iphone(self):
        self.apply()
        c = self.read('boot-contract.json')
        self.assertEqual(c['adt']['root'], {'model': 'iPhone18,2', 'target-type': 'V54', 'compatible': 'V54AP'})
        self.assertEqual(c['adt']['chosen']['display-scale'], 3)
        self.assertEqual(c['adt']['product']['artwork-device-idiom'], 'phone')
        self.assertEqual((c['display']['width'], c['display']['height'], c['display']['scale']), (1320, 2868, 3))
        self.assertIn('scale_note', c['display'])
        self.assertIn('chosen/unique-chip-id', c['adt']['required'])
        self.assertEqual((c['layout'], c['build'], c['kernel_args']), ('sealed', fakeroot.BUILD, []))
        self.assertIn('key-store', [f['name'] for f in c['kernel_features']])
        self.assertEqual(self.states()['target-type'], 'kept')

    def test_refusals(self):
        with open(os.path.join(self.root, 'System/Library/CoreServices/SystemVersion.plist'), 'wb') as f:
            plistlib.dump({'ProductBuildVersion': '23F84', 'ProductVersion': '26.5.2', 'ProductName': 'iPhone OS'}, f)
        self.apply(expect=1)
        self.assertIn('--any-version', self.log)
        self.assertEqual(os.listdir(self.data), [])
        self.apply('--any-version')
        self.assertEqual(self.plist(self.data, 'mobile/Library/Preferences/com.apple.migration.plist')['LastSystemVersion'], '23F84')
        # not a root at all
        empty = os.path.join(self.top, 'empty')
        os.mkdir(empty)
        self.root = empty
        self.apply(expect=1)
        self.assertIn('not an iOS root', self.log)


class ReadWrite(Case):
    def cache(self):
        return self.plist(self.root, 'System/Library/xpc/launchd.plist')

    def test_the_launchd_cache_and_the_jetsam_properties_are_edited(self):
        helpers, hashes = self.helpers()
        self.apply('--helpers', helpers, layout='rw')
        have = launchd.jobs(self.cache())
        for label in ('com.apple.nfcd', 'com.apple.watchdogd', 'com.apple.analyticsd', 'com.apple.momentsd'):
            self.assertNotIn(label, have)
        for label in ('com.apple.SpringBoard', 'com.apple.CommCenter', 'com.apple.example.unknownd', 'com.apple.example.boot'):
            self.assertIn(label, have)
        # our manager is a launchd job whose program is on the root file system
        _, key, job = have['com.oneios.init']
        self.assertEqual(key, '/System/Library/LaunchDaemons/com.oneios.init.plist')
        self.assertEqual(job['ProgramArguments'], ['/usr/libexec/oneios-init', '/private/var/oneios'])
        self.assertTrue(job['RunAtLoad'] and job['KeepAlive'] and job['UserName'] == 'root')
        self.assertEqual(self.plist(self.root, 'System/Library/LaunchDaemons/com.oneios.init.plist'), job)
        self.assertEqual(self.entry('root', 'usr/libexec/oneios-init')['mode'], '0755')
        self.assertIn('/usr/libexec/oneios-init', {f['path'] for f in self.read('owner-trust.json')['files']})
        # a crash loop does not restart the guest
        springboard = have['com.apple.SpringBoard'][2]
        self.assertNotIn('_PanicOnCrash', springboard)
        self.assertIs(springboard['_PanicOnCrasX'], True)
        self.assertEqual(have['com.apple.backboardd'][2]['EnvironmentVariables'], {'CA_EXAMPLE': '1'})
        jetsam = self.plist(self.root, 'System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist')
        override = jetsam['Version4']['Daemon']['Override']
        self.assertEqual(override['com.apple.backboardd'], {'ActiveSoftMemoryLimit': 100})
        self.assertEqual(override['com.apple.analyticsd'], {'ActiveSoftMemoryLimit': 10})
        self.assertEqual(launchd.forced(jetsam), set())
        # the Data tree is the root's /private/var; the edited cache needs the boot argument
        self.assertIn('com.apple.nfcd', self.plist(self.root, 'private/var/db/com.apple.xpc.launchd/disabled.plist'))
        self.assertIs(self.plist(self.root, 'private/var/preferences/FeatureFlags/Domain/ExtensionKit.plist')
                      ['enforce_xpc_cache_code_signing']['Enabled'], False)
        contract = self.read('boot-contract.json')
        self.assertEqual([a['arg'] for a in contract['kernel_args']], ['launchd_unsecure_cache=1'])
        self.assertIsNone(contract['launch_policy']['service_manager'])
        self.assertNotIn('kernel_features', contract)

    def test_a_second_run_and_the_kept_key(self):
        self.apply(layout='rw')
        first = digest(self.root)
        self.apply(layout='rw')
        self.assertEqual(digest(self.root), first)
        self.assertEqual(self.states()['launchd cache'], 'kept')

    def test_recover_on_crash_can_stay(self):
        self.apply('--keep-recover-on-crash', layout='rw')
        jetsam = self.plist(self.root, 'System/Library/LaunchDaemons/com.apple.jetsamproperties.V54.plist')
        self.assertIs(jetsam['Version4']['Daemon']['Override']['com.apple.backboardd']['RecoverOnCrash'], True)

    def test_staged_apps_and_the_plugin_go_into_the_root(self):
        os.makedirs(os.path.join(self.root, 'private/var/staged_system_apps/Example.app'))
        os.makedirs(os.path.join(self.root, 'Applications'))
        with open(os.path.join(self.root, 'private/var/staged_system_apps/Example.app/Info.plist'), 'wb') as f:
            plistlib.dump({'CFBundleIdentifier': 'com.example.app'}, f)
        bundle = os.path.join(self.top, 'NativeAdrenoMetal.bundle')
        os.makedirs(bundle)
        with open(os.path.join(bundle, 'Info.plist'), 'wb') as f:
            plistlib.dump({'CFBundleExecutable': 'NativeAdrenoMetal'}, f)
        with open(os.path.join(bundle, 'NativeAdrenoMetal'), 'wb') as f:
            f.write(fakeroot.signed_macho()[0])
        self.apply('--metal-plugin', bundle, layout='rw')
        self.assertTrue(os.path.isfile(os.path.join(self.root, 'Applications/Example.app/Info.plist')))
        self.assertTrue(os.path.isfile(os.path.join(self.root, 'System/Library/Extensions/NativeAdrenoMetal.bundle/NativeAdrenoMetal')))
        self.assertEqual(self.read('boot-contract.json')['addon_mounts'], [])


class Trees(unittest.TestCase):
    def test_links_are_followed_inside_the_tree_only(self):
        with tempfile.TemporaryDirectory() as top:
            outside = os.path.join(top, 'outside')
            tree_top = os.path.join(top, 'tree')
            os.makedirs(os.path.join(tree_top, 'private/var'))
            os.makedirs(outside)
            os.symlink('/private/var', os.path.join(tree_top, 'var'))       # absolute: from the tree's top
            os.symlink('../../outside', os.path.join(tree_top, 'escape'))   # relative, pointing out
            os.symlink(outside, os.path.join(tree_top, 'abs'))               # absolute host path
            tree = Tree(tree_top, 'root')
            tree.write('var/a.txt', b'a')
            self.assertTrue(os.path.isfile(os.path.join(tree_top, 'private/var/a.txt')))
            tree.write('escape/b.txt', b'b')
            tree.write('abs/c.txt', b'c')
            self.assertEqual(os.listdir(outside), [])
            for path in (tree.host('escape/b.txt'), tree.host('abs/c.txt'), tree.host('../../etc/passwd')):
                self.assertTrue(path.startswith(tree_top + os.sep), path)
            with self.assertRaises(TreeError):
                Tree(tree_top, 'root', readonly=True).write('x', b'x')

    def test_merge_keeps_types_apart(self):
        with tempfile.TemporaryDirectory() as top:
            tree = Tree(top, 'data')
            tree.write_plist('p.plist', {'a': 1, 'b': True})
            self.assertEqual(tree.merge_plist('p.plist', {'a': 1, 'b': 1, 'c': 2}), ['b', 'c'])
            self.assertEqual(tree.merge_plist('p.plist', {'a': 1, 'b': 1, 'c': 2}), [])


class MachO(unittest.TestCase):
    def test_the_cdhash_is_the_hash_of_the_code_directory(self):
        data, cdhash = fakeroot.signed_macho('com.example.tool')
        (s,) = macho.describe(data)
        self.assertEqual((s['cdhash'], s['identifier'], s['arch'], s['page_size'], s['adhoc'], s['platform'], s['minos']),
                         (cdhash, 'com.example.tool', 'arm64e', 4096, True, 'ios', '27.0'))
        self.assertEqual(len(cdhash), 40)
        other, other_hash = fakeroot.signed_macho('com.example.tool', payload=b'other code')
        self.assertNotEqual(cdhash, other_hash)

    def test_fat_files_and_files_that_are_not_signed(self):
        import struct
        a, hash_a = fakeroot.signed_macho('a')
        b, hash_b = fakeroot.signed_macho('b', arm64e=False)
        fat = struct.pack('>II', 0xCAFEBABE, 2) + struct.pack('>IIIII', 0x0100000C, 2, 48, len(a), 0) + \
            struct.pack('>IIIII', 0x0100000C, 0, 48 + len(a), len(b), 0) + a + b
        self.assertEqual([(s['cdhash'], s['arch']) for s in macho.describe(fat)], [(hash_a, 'arm64e'), (hash_b, 'arm64')])
        unsigned = struct.pack('<IIIIIIII', 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0)
        with self.assertRaises(macho.MachOError):
            macho.describe(unsigned)
        with self.assertRaises(macho.MachOError):
            macho.describe(a[:len(a) - 10])
        self.assertFalse(macho.is_macho(b'\x7fELF....'))


if __name__ == '__main__':
    unittest.main()
