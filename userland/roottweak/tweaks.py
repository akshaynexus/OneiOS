"""The tweaks. Each takes the context, changes the trees through it and reports one line per item.

Two layouts (docs/userland.md, "Layouts"):

- `sealed`: the OS volume is Apple's image, unchanged and hash-checked (UniXNU's model). Nothing is written
  to it; every change goes to the Data tree (/private/var) or into the boot contract for the kernel and booter.
- `rw`: the OS volume is a root we write ourselves (iOSPlus10's and vphone's model). The launchd cache and
  the jetsam properties are edited in place as well.

States, as iOSPlus10's root audit has them: applied, kept (already so), not-found, off (asked not to, or an
input is missing), contract (left to the kernel or booter, named in boot-contract.json).
"""
import os

from . import launchd, macho
from .tree import MOBILE, ROOT, TreeError

ONEIOS = 'oneios'                       # in the Data tree: /private/var/oneios
ONEIOS_GUEST = '/private/var/oneios'
INIT_LABEL = 'com.oneios.init'
INIT_ROOT_PATH = 'usr/libexec/oneios-init'  # rw layout: the manager on the OS volume
HELPERS = ('oneios-init', 'oneios-bootstat', 'oneios-mg', 'oneios-thermal')  # oneios-seed runs before Data exists: it is not installed here
LOGD = (272, 272)
RECOVER_LABELS = ('com.apple.backboardd', 'com.apple.SpringBoard', 'com.apple.configd', 'com.apple.runningboardd')


class Context:
    def __init__(self, root, data, layout, device, daemons, prefs, options):
        self.root, self.data, self.layout = root, data, layout
        self.device, self.daemons, self.prefs, self.options = device, daemons, prefs, options
        self.results = []
        self.build = None
        self.trusted = []   # our Mach-O files placed in the guest
        self.contract = {
            'kernel_args': [],
            'launch_policy': {'disable_jobs': [], 'service_manager': None, 'force_enabled': []},
            'addon_mounts': [],
            'notes': [],
        }
        self._tweak = None

    @property
    def rw(self):
        return self.layout == 'rw'

    def say(self, state, item, note='', labels=None):
        result = {'tweak': self._tweak, 'state': state, 'item': item, 'note': note}
        if labels is not None:
            # the whole list is in report.json; the line names the first few
            result['labels'] = labels
            shown = ', '.join(labels[:6]) + (f' and {len(labels) - 6} more (report.json)' if len(labels) > 6 else '')
            result['note'] = f'{note}: {shown}' if note else shown
        self.results.append(result)

    def run(self, tweak):
        self._tweak = tweak.__name__.replace('_', '-')
        self.root.tweak = self.data.tweak = self._tweak
        tweak(self)

    def kernel_arg(self, arg, why):
        if arg not in [a['arg'] for a in self.contract['kernel_args']]:
            self.contract['kernel_args'].append({'arg': arg, 'why': why})


# ---- version -------------------------------------------------------------------------------------------------
def version(cx):
    plist = cx.root.read_plist('System/Library/CoreServices/SystemVersion.plist')
    if not plist or not cx.root.exists('sbin/launchd'):
        raise TreeError(f'{cx.root.top}: not an iOS root (no SystemVersion.plist or no sbin/launchd)')
    cx.build = cx.options.get('build') or plist.get('ProductBuildVersion')
    have = plist.get('ProductVersion')
    want = cx.device['device']['version']
    if not cx.build:
        raise TreeError('SystemVersion.plist names no build; give --build')
    if have != want:
        if not cx.options.get('any_version'):
            raise TreeError(f'the root is {plist.get("ProductName")} {have} ({cx.build}); the profile is for {want}. '
                            '--any-version applies it anyway')
        cx.say('applied', 'version', f'root is {have} ({cx.build}), profile is for {want}: applied anyway (--any-version)')
    else:
        cx.say('kept', 'version', f'{plist.get("ProductName")} {have} ({cx.build})')


# ---- launchd jobs --------------------------------------------------------------------------------------------
def _cache(cx):
    cache = cx.root.read_plist(launchd.CACHE)
    if not isinstance(cache, dict):
        raise TreeError(f'{launchd.CACHE}: no launchd job cache in this root')
    return cache


def _jetsam_files(cx):
    return [f'{launchd.DAEMONS_DIR}/{name}' for name in cx.root.listdir(launchd.DAEMONS_DIR)
            if name.startswith('com.apple.jetsamproperties.') and name.endswith('.plist')]


def launchd_jobs(cx):
    """Disables the jobs of the selected groups: by override on the Data volume, and on a root we write
    by taking them out of the cache as well."""
    o = cx.options
    wanted = cx.daemons.selected(o.get('groups', ()), o.get('without_groups', ()), o.get('keep', ()))
    for label in o.get('disable', ()):
        # by hand, also against the keep list: said in the report
        if label in cx.daemons.keep:
            cx.say('applied', f'warning: {label}', 'disabled by --disable although the policy keeps it: ' +
                   cx.daemons.keep_reasons.get(label, 'see daemons.toml [keep]'))
        wanted[label] = 'by hand'
    cache = _cache(cx)
    have = launchd.jobs(cache)
    present = sorted(label for label in wanted if label in have)
    missing = sorted(label for label in wanted if label not in have)
    absent_keep = sorted(label for label in cx.daemons.keep if label not in have)

    path = f'{launchd.OVERRIDES_DIR}/disabled.plist'
    existing = cx.data.read_plist(path, {})
    merged = launchd.overrides(existing, present)
    cx.data.mkdir(launchd.OVERRIDES_DIR, ROOT, 0o755)
    changed = cx.data.write_plist(path, merged, ROOT, 0o644, binary=False)
    # An empty marker selects launchd's current override loader (UniXNU-droidvm DroidVMLaunchPolicy.c).
    if not cx.data.exists(f'{launchd.OVERRIDES_DIR}/disabled.migrated'):
        cx.data.write(f'{launchd.OVERRIDES_DIR}/disabled.migrated', b'', ROOT, 0o644)
    else:
        cx.data.write(f'{launchd.OVERRIDES_DIR}/disabled.migrated', cx.data.read(f'{launchd.OVERRIDES_DIR}/disabled.migrated'), ROOT, 0o644)
    by_group = {}
    for label in present:
        by_group.setdefault(wanted[label], []).append(label)
    summary = ', '.join(f'{group} {len(labels)}' for group, labels in sorted(by_group.items()))
    cx.say('applied' if changed else 'kept', 'disabled.plist',
           f'{len(present)} jobs disabled on the Data volume ({summary}); root:wheel 0644 or launchd ignores it')
    if missing:
        cx.say('not-found', 'jobs not in this root', f'{len(missing)} of the policy\'s labels', missing)
    if absent_keep:
        cx.say('not-found', 'kept jobs not in this root', f'{len(absent_keep)} of the keep list', absent_keep)
    cx.contract['launch_policy']['disable_jobs'] = present

    # iOS 27: DisableAllowed = false loads a job although it is disabled.
    forced = set()
    for file in _jetsam_files(cx):
        forced |= launchd.forced(cx.root.read_plist(file, {}))
    still = sorted(forced & set(present))
    if cx.rw:
        gone = launchd.remove_jobs(cache, present)
        cx.root.write_plist(launchd.CACHE, cache, ROOT, 0o644)
        cx.say('applied' if gone else 'kept', 'launchd cache', f'{len(gone)} jobs taken out of {launchd.CACHE}')
        for file in _jetsam_files(cx):
            jetsam = cx.root.read_plist(file)
            labels = launchd.allow_disable(jetsam)
            if labels:
                cx.root.write_plist(file, jetsam, ROOT, 0o644)
            cx.say('applied' if labels else 'kept', os.path.basename(file),
                   f'DisableAllowed = false removed from {len(labels)} jobs' if labels else 'no DisableAllowed = false')
    elif still:
        cx.contract['launch_policy']['force_enabled'] = still
        cx.say('contract', 'DisableAllowed', f'{len(still)} disabled jobs are marked DisableAllowed = false in the jetsam '
               f'properties and run all the same on a sealed root: {", ".join(still)}')
    else:
        cx.say('kept', 'DisableAllowed', 'none of the disabled jobs is marked DisableAllowed = false')
    if not cx.rw:
        # The line's wording is kept as it was first written: the report is a file of the seed, and a seed with
        # another hash makes every Data store start over. What happens is in the contract below and in
        # docs/userland.md section 6: the seed carries the override and the kernel's unpack sets its owner; the
        # kernel writes nothing itself (kernel/patches/oneios-hosted/0004-README.md, step 5).
        cx.say('contract', 'launch policy', 'the kernel writes the same override before launchd starts '
               '(launch_policy.disable_jobs), for a Data volume whose file owners it cannot trust')
        cx.contract['launch_policy']['override'] = {
            'path': f'/private/var/{path}', 'marker': f'/private/var/{launchd.OVERRIDES_DIR}/disabled.migrated',
            'owner': 'root:wheel', 'mode': '0644', 'carried_by': 'the Data seed; the first start\'s unpack sets the owner',
            'read_by_launchd': 'once per boot, after the boot task finish-demo-restore, only when uid 0 owns the file and '
                               'neither group nor others can write it; a job in it is not spawned and its Mach '
                               'services are not registered (roottweak boot-check shows it from launchd.log)',
        }


def launchd_crash_policy(cx):
    """Bring-up: a crash loop of backboardd or SpringBoard must not restart the guest, or nothing can be read
    from it. Only on a root we write (the keys are in the launchd cache and the jetsam properties)."""
    if not cx.rw:
        cx.contract['notes'].append('sealed root: _PanicOnCrash and RecoverOnCrash stay as Apple set them; three crashes of '
                                    'backboardd, SpringBoard, configd or runningboardd within 60 s restart the guest')
        cx.say('contract', 'crash policy', 'not editable on a sealed root: launchd restarts the guest after 3 crashes in '
               '60 s of a RecoverOnCrash job')
        return
    cache = _cache(cx)
    renamed = 0
    for label, (_, _, job) in sorted(launchd.jobs(cache).items()):
        if '_PanicOnCrash' in job:
            job['_PanicOnCrasX'] = job.pop('_PanicOnCrash')
            renamed += 1
    if renamed:
        cx.root.write_plist(launchd.CACHE, cache, ROOT, 0o644)
    cx.say('applied' if renamed else 'kept', '_PanicOnCrash', f'renamed in {renamed} jobs' if renamed else 'no job has it')
    if cx.options.get('keep_recover_on_crash'):
        cx.say('off', 'RecoverOnCrash', 'kept (--keep-recover-on-crash)')
        return
    total = []
    for file in _jetsam_files(cx):
        jetsam = cx.root.read_plist(file)
        labels = launchd.drop_recover_on_crash(jetsam, RECOVER_LABELS)
        if labels:
            cx.root.write_plist(file, jetsam, ROOT, 0o644)
            total += labels
    cx.say('applied' if total else 'kept', 'RecoverOnCrash',
           f'removed from {", ".join(sorted(set(total)))}' if total else 'no job has it')


# ---- our own services ------------------------------------------------------------------------------------------
def _install_binary(cx, tree, source, path, mode=0o755):
    """Copies one of our Mach-O files into the guest and notes it for the owner trust list."""
    with open(source, 'rb') as f:
        data = f.read()
    if not macho.is_macho(data):
        raise TreeError(f'{source}: not a Mach-O (a host build? the guest needs helpers/build.sh output)')
    try:
        slices = macho.describe(data)
    except macho.MachOError as e:
        raise TreeError(f'{source}: {e}; the guest runs only signed code')
    tree.write(path, data, ROOT, mode)
    guest = ('/private/var/' if tree.volume == 'data' else '/') + path
    for s in slices:
        warnings = []
        if s['arch'] != 'arm64e':
            warnings.append(f'{s["arch"]}, not arm64e: iOS 27 kills arm64 platform binaries at exec (Droid-VM/ownervisor build.py)')
        if s['page_size'] != 4096:
            warnings.append(f'signature pages are {s["page_size"]} bytes, not 4096 (UniXNU signs with --pagesize 4096)')
        if s['platform'] not in (None, 'ios'):
            warnings.append(f'built for {s["platform"]}, not ios')
        cx.trusted.append({'path': guest, 'cdhash': s['cdhash'], 'hash': s['hash'], 'identifier': s['identifier'],
                           'arch': s['arch'], 'page_size': s['page_size'], 'adhoc': s['adhoc'], 'warnings': warnings})
        for w in warnings:
            cx.say('applied', f'warning: {guest}', w)


def _conf(args, **keys):
    lines = [f'arg={a}' for a in args]
    for key, value in keys.items():
        if key == 'env':
            lines += [f'env={k}={v}' for k, v in value.items()]
        else:
            lines.append(f'{key}={int(value)}')
    return ('\n'.join(lines) + '\n').encode()


def host_services(cx):
    """Places oneios-init (the manager of our services), its service files and the helpers."""
    for directory, mode in (('', 0o755), ('bin', 0o755), ('services.d', 0o755), ('enabled', 0o755),
                            ('log', 0o755), ('state', 0o755), ('run', 0o755)):
        cx.data.mkdir(f'{ONEIOS}/{directory}'.rstrip('/'), ROOT, mode)
    expect = ''.join(f'{k}={v}\n' for k, v in sorted(cx.device['mobilegestalt']['expect'].items()))
    cx.data.write(f'{ONEIOS}/identity.expect', expect.encode(), ROOT, 0o644)

    helpers = cx.options.get('helpers')
    have = set()
    if helpers:
        for name in HELPERS:
            source = os.path.join(helpers, name)
            if os.path.isfile(source):
                _install_binary(cx, cx.data, source, f'{ONEIOS}/bin/{name}')
                have.add(name)
        # The debug shell, when the helper build was given one (helpers/README.md): any other file of the folder.
        for name in sorted(os.listdir(helpers)):
            source = os.path.join(helpers, name)
            if name in HELPERS or name == 'oneios-seed' or name.startswith('.') or not os.path.isfile(source):
                continue
            with open(source, 'rb') as f:
                if macho.is_macho(f.read(8)):
                    _install_binary(cx, cx.data, source, f'{ONEIOS}/bin/{name}')
                    have.add(name)
    env = {'ONEIOS_ROOT': ONEIOS_GUEST, 'PATH': f'{ONEIOS_GUEST}/bin:/usr/bin:/bin:/usr/sbin:/sbin'}
    services = {
        # no bootstrap wait: it must report the stages before launchd's services exist
        'bootstat': (_conf([f'{ONEIOS_GUEST}/bin/oneios-bootstat'], keepalive=1, throttle=10, bootstrap=0, env=env), 'oneios-bootstat'),
        'identity': (_conf([f'{ONEIOS_GUEST}/bin/oneios-mg', '--expect', f'{ONEIOS_GUEST}/identity.expect'],
                           keepalive=0, bootstrap=1, env=env), 'oneios-mg'),
        'dropbear': (_conf([f'{ONEIOS_GUEST}/bin/dropbear', '-F', '-E', '-R', '-p', '2222'], keepalive=1, throttle=5,
                           bootstrap=1, env=env), 'dropbear'),
        # the phone's thermal pressure (drivers/thermal) into notify's com.apple.system.thermalpressurelevel; needs notifyd
        'thermal': (_conf([f'{ONEIOS_GUEST}/bin/oneios-thermal'], keepalive=1, throttle=10, bootstrap=1, env=env), 'oneios-thermal'),
    }
    enabled = []
    for name, (conf, binary) in services.items():
        cx.data.write(f'{ONEIOS}/services.d/{name}.conf', conf, ROOT, 0o644)
        if binary in have:
            cx.data.write(f'{ONEIOS}/enabled/{name}', b'', ROOT, 0o644)
            enabled.append(name)
        else:
            cx.data.remove(f'{ONEIOS}/enabled/{name}')
    if 'oneios-init' not in have:
        cx.say('off', 'oneios-init', 'no helpers given (--helpers DIR, the output of userland/helpers/build.sh): the service '
               'files are placed, nothing starts them')
    else:
        cx.say('applied', 'oneios-init', f'in {ONEIOS_GUEST}/bin; services at boot: {", ".join(enabled) or "none"}')
    if 'dropbear' not in have:
        cx.say('off', 'ssh', 'no dropbear in --helpers: no debug shell (helpers/README.md says how to add one)')

    if cx.rw:
        cache = _cache(cx)
        job = {
            'Label': INIT_LABEL,
            'ProgramArguments': ['/' + INIT_ROOT_PATH, ONEIOS_GUEST],
            'RunAtLoad': True, 'KeepAlive': True, 'ThrottleInterval': 10, 'UserName': 'root',
            'EnvironmentVariables': env,
            # as iOSPlus10's iOS 26 debug job (scripts/net/root-add-ssh.py): runs in the minimal boot profile, never
            # exits under memory pressure, says what it does on the console
            'StandardErrorPath': '/dev/console', 'POSIXSpawnType': 'Interactive', 'MinimalBootProfile': True,
            'EnablePressuredExit': False, 'EnableTransactions': False,
        }
        if 'oneios-init' in have:
            # launchd refuses a job whose program is not on the root file system (iOSPlus10 ios26-first-boot.md)
            _install_binary(cx, cx.root, os.path.join(helpers, 'oneios-init'), INIT_ROOT_PATH)
            changed = launchd.add_daemon(cache, INIT_LABEL, job)
            cx.root.write_plist(launchd.CACHE, cache, ROOT, 0o644)
            cx.root.write_plist(f'{launchd.DAEMONS_DIR}/{INIT_LABEL}.plist', job, ROOT, 0o644, binary=False)
            cx.say('applied' if changed else 'kept', 'launchd job', f'{INIT_LABEL} runs /{INIT_ROOT_PATH}')
        cx.kernel_arg('launchd_unsecure_cache=1', 'an edited launchd cache loads only with it: launchd_cache_loader then skips '
                      'the cache\'s signature check (iOSPlus10 ios26-first-boot.md; vphone patches the loader instead)')
    else:
        cx.contract['launch_policy']['service_manager'] = {
            'path': f'{ONEIOS_GUEST}/bin/oneios-init', 'args': [ONEIOS_GUEST], 'uid': 0,
            'present': 'oneios-init' in have,
            'how': 'posix_spawn once from the launch policy hook while launchd is exec\'d, then copy launchd\'s bootstrap '
                   'port into its task (UniXNU-droidvm DroidVMLaunchPolicy.c spawn_service_manager, '
                   'droidvm_fix_ownervisor_bootstrap)',
        }
        cx.say('contract', 'service manager', 'a sealed root has no launchd label for it: the kernel starts '
               f'{ONEIOS_GUEST}/bin/oneios-init (launch_policy.service_manager)')


# ---- Metal plugin ------------------------------------------------------------------------------------------------
def metal_plugin(cx):
    """Places the Metal plugin bundle where Metal looks for it: /System/Library/Extensions/<name>.bundle."""
    source = cx.options.get('metal_plugin')
    if not source:
        cx.say('off', 'metal plugin', 'no --metal-plugin BUNDLE: backboardd composites on the CPU')
        return
    source = source.rstrip('/')
    name = os.path.basename(source)
    info_path = os.path.join(source, 'Info.plist')
    if not name.endswith('.bundle') or not os.path.isfile(info_path):
        raise TreeError(f'{source}: not a .bundle with an Info.plist')
    import plistlib
    with open(info_path, 'rb') as f:
        info = plistlib.load(f)
    executable = info.get('CFBundleExecutable')
    if not executable or not os.path.isfile(os.path.join(source, executable)):
        raise TreeError(f'{source}: Info.plist names no CFBundleExecutable that is in the bundle')
    # The plug-in writes QuartzCore options at fixed addresses of one dyld shared cache (Droid-VM/NativeMetal
    # NativeAdrenoMetalPlugin.mm: the iPad17,3 24A446 cache, relative to CARenderServerStart, with no check of the
    # cache). In another cache those writes land on other memory of backboardd. So a bundle is placed only when it
    # says it was built for this guest: a file `oneios-target` in it holding "<product> <build>".
    want = f'{cx.device["device"]["product"]} {cx.build}'
    marker = os.path.join(source, 'oneios-target')
    have = None
    if os.path.isfile(marker):
        with open(marker) as f:
            have = f.read().strip()
    if have != want and not cx.options.get('metal_plugin_unchecked'):
        cx.say('off', 'metal plugin', f'{name} not placed: it does not say it was built for this guest (its oneios-target is '
               f'{have!r}, wanted {want!r}). The upstream plug-in holds addresses of the iPad\'s shared cache; placed as it is '
               'it would write into backboardd at wrong addresses. --metal-plugin-unchecked places it anyway')
        return
    stem = name[:-len('.bundle')]
    # UniXNU-droidvm build-droidvm-data-seed.py's default; the render server (backboardd) always has the device.
    # "*" gives it to every process, which on the iPad also changed SpringBoard's behaviour (DroidVMBootArgs27.c).
    clients = cx.options.get('metal_clients') or ['WidgetRenderer_Default', '*Poster*', 'iconservicesagent', 'SBRendererService']
    client_text = ('\n'.join(clients) + '\n').encode()
    if cx.rw:
        tree, base, guest = cx.root, 'System/Library/Extensions', '/System/Library/Extensions'
    else:
        tree, base, guest = cx.data, f'{ONEIOS}/addons/Extensions', f'{ONEIOS_GUEST}/addons/Extensions'
    tree.mkdir(f'{base}/{name}', ROOT, 0o755)
    for current, dirs, files in os.walk(source):
        dirs.sort()
        rel = os.path.relpath(current, source)
        for d in dirs:
            tree.mkdir(os.path.normpath(f'{base}/{name}/{rel}/{d}'), ROOT, 0o755)
        for file in sorted(files):
            host = os.path.join(current, file)
            path = os.path.normpath(f'{base}/{name}/{rel}/{file}')
            if os.path.islink(host):
                raise TreeError(f'{host}: a symbolic link in the plugin bundle')
            with open(host, 'rb') as f:
                head = f.read(8)
            if macho.is_macho(head):
                _install_binary(cx, tree, host, path)
            else:
                tree.copy_in(host, path, ROOT, 0o644)
    # The processes that get the plugin's device, one per line; "*" is every process. The kernel's GPU driver
    # reads it for its sandbox grants (UniXNU-droidvm DroidVMGpu.cpp; DroidVMLaunchPolicy.c writes the same file from
    # a boot argument), not the plug-in: the list is in the contract too (metal.clients).
    tree.write(f'{base}/{stem}.clients', client_text, ROOT, 0o644)
    # With this file the plugin gives its device to backboardd's own discovery (UniXNU add-gpu-plugin-data.py --publish).
    tree.write(f'{base}/{stem}.publish', b'1\n', ROOT, 0o644)
    if not cx.rw:
        cx.contract['addon_mounts'].append({
            'source': guest, 'over': '/System/Library/Extensions', 'type': 'bindfs', 'union': True, 'readonly': True,
            'how': 'mounted by the kernel before launchd, files shown as root\'s (UniXNU sheng-gpu-addon=1)'})
    cx.contract['metal'] = {'bundle': f'/System/Library/Extensions/{name}', 'executable': executable,
                            'principal_class': info.get('NSPrincipalClass'), 'clients': clients,
                            'kernel': 'the accelerator service must publish MetalPluginName (the bundle name without '
                                      '.bundle) and MetalPluginClassName, as an IOAccelerator does for Apple\'s driver'}
    cx.say('applied', 'metal plugin', f'{name} in {guest}; clients: {", ".join(clients)}' +
           ('' if cx.rw else '; shown over /System/Library/Extensions by the kernel (addon_mounts)'))


# ---- cryptexes and the removable system apps -----------------------------------------------------------------------
def _copy_tree(cx, tree, source, base):
    """Copies an extracted volume into a tree: files, directories and links, modes from the source."""
    import stat as st
    count = 0
    tree.mkdir(base, ROOT, 0o755)
    for current, dirs, files in os.walk(source):
        dirs.sort()
        rel = os.path.relpath(current, source)
        for name in list(dirs):
            host = os.path.join(current, name)
            path = os.path.normpath(f'{base}/{rel}/{name}')
            if os.path.islink(host):
                tree.symlink(path, os.readlink(host))
                dirs.remove(name)
            elif name in ('.fseventsd', '.Trashes'):
                dirs.remove(name)
            else:
                tree.mkdir(path, ROOT, st.S_IMODE(os.lstat(host).st_mode) or 0o755)
        for name in sorted(files):
            host = os.path.join(current, name)
            path = os.path.normpath(f'{base}/{rel}/{name}')
            if os.path.islink(host):
                tree.symlink(path, os.readlink(host))
            elif os.path.isfile(host):
                with open(host, 'rb') as f:
                    tree.write(path, f.read(), ROOT, st.S_IMODE(os.lstat(host).st_mode))
                count += 1
    return count


def app_cryptex(cx):
    """The App cryptex (Safari's daemons and support files) where iOS looks for it: /private/preboot/Cryptexes/App.
    Its files stay Apple's and pass AMFI through the cryptex's own trust cache."""
    source = cx.options.get('appos')
    if not source and not cx.rw:
        # image-build ships the cryptex as its own read-only image (docs/image-format.md: app.img)
        cx.contract['app_cryptex'] = {'image': 'app.img', 'mount': '/private/preboot/Cryptexes/App', 'readonly': True,
                                      'also': 'the SystemOS volume by its cryptex path: /System over '
                                              '/private/preboot/Cryptexes/OS/System and /usr over .../OS/usr, not as unions '
                                              '(UniXNU zzyb-native-cryptex-os-view.patch)'}
        cx.say('contract', 'App cryptex', 'not copied: the kernel mounts the image set\'s app.img at '
               '/private/preboot/Cryptexes/App (app_cryptex)')
        return
    if not source:
        cx.say('off', 'App cryptex', 'no --appos DIR (the extracted Cryptex1,AppOS volume): Safari\'s daemons are missing')
        return
    if not os.path.isdir(source):
        raise TreeError(f'{source}: not a directory')
    if cx.rw:
        count = _copy_tree(cx, cx.root, source, 'private/preboot/Cryptexes/App')
        cx.say('applied', 'App cryptex', f'{count} files in /private/preboot/Cryptexes/App')
        return
    # UniXNU-droidvm scripts/add-app-cryptex-data.py: carried on the Data volume and shown over the empty /private/preboot
    count = _copy_tree(cx, cx.data, source, f'{ONEIOS}/preboot/Cryptexes/App')
    for point in ('System', 'usr'):
        cx.data.mkdir(f'{ONEIOS}/preboot/Cryptexes/OS/{point}', ROOT, 0o755)
    cx.contract['addon_mounts'] += [
        {'source': f'{ONEIOS_GUEST}/preboot', 'over': '/private/preboot', 'type': 'bindfs', 'union': True, 'readonly': True,
         'how': 'UniXNU owner-app-cryptex=1 (zzyb-native-cryptex-os-view.patch)'},
        {'source': '/System', 'over': '/private/preboot/Cryptexes/OS/System', 'type': 'bindfs', 'union': False, 'readonly': True,
         'how': 'the SystemOS volume by its cryptex path; not a union, or directory reads loop'},
        {'source': '/usr', 'over': '/private/preboot/Cryptexes/OS/usr', 'type': 'bindfs', 'union': False, 'readonly': True,
         'how': 'as /System'},
    ]
    cx.say('applied', 'App cryptex', f'{count} files in {ONEIOS_GUEST}/preboot/Cryptexes/App; shown at /private/preboot by the '
           'kernel (addon_mounts)')


def staged_apps(cx):
    """The removable system apps (Weather, Notes, ...) ship in /private/var/staged_system_apps and are expanded by
    the first boot's migration, which the seed skips. UniXNU mounts that folder over /Applications
    (owner-staged-apps=1); on a root we write they are moved there (iOSPlus10 scripts/ios16/root-finish.sh)."""
    staged = 'private/var/staged_system_apps'
    names = [n for n in cx.root.listdir(staged) if n.endswith('.app')]
    if not cx.rw:
        cx.contract['staged_apps'] = {'source': '/private/var/staged_system_apps (the OS volume\'s, before Data covers /private/var)',
                                      'over': '/Applications', 'how': 'UniXNU owner-staged-apps=1; also drop LaunchServices\' '
                                      'database at each boot, or the home screen shows Settings alone (ipados27-bringup.patch)'}
        cx.say('contract', 'staged apps', f'{len(names)} removable system apps stay in the OS volume; the kernel shows them in '
               '/Applications (staged_apps)')
        return
    moved = 0
    for name in names:
        if cx.root.lexists(f'Applications/{name}'):
            continue
        os.rename(cx.root.host(f'{staged}/{name}'), os.path.join(cx.root.host('Applications'), name))
        cx.root.manifest.record('root', f'Applications/{name}', 'moved', target=f'{staged}/{name}', tweak=cx._tweak)
        cx.root.remove(f'System/Library/AppPlaceholders/{name}') if cx.root.isfile(f'System/Library/AppPlaceholders/{name}') else None
        moved += 1
    cx.say('applied' if moved else 'kept', 'staged apps', f'{moved} moved to /Applications' if moved else 'none staged')


def kernel_features(cx):
    """What the sealed layout asks of the kernel, by the names UniXNU gives the switches."""
    if cx.rw:
        return
    features = [
        ('launch-policy', 'droidvm-disable-jobs / native_board_launch_policy', 'write launch_policy.disable_jobs to disabled.plist as root before each launchd start'),
        ('service-manager', 'owner-service-manager=1', 'start launch_policy.service_manager and hand it launchd\'s bootstrap port'),
        ('no-launch-constraints', 'sheng-no-lc=1 (zzyc-owner-skip-launch-constraints.patch)', 'our binaries are in the owner trust cache but are not platform binaries'),
        ('owner-trust', '/chosen/memory-map/TrustCache with an owner module', 'the CDHashes of owner-trust.json beside Apple\'s three trust caches'),
        ('addon-mounts', 'sheng-gpu-addon=1, owner-app-cryptex=1 (bindfs, BINDM_FORCE_ROOT)', 'the read-only views in addon_mounts, after Data is mounted'),
        ('staged-apps', 'owner-staged-apps=1', 'staged_apps'),
        ('data-protection', 'owner-dataprotect=1', 'a Data volume without content protection: class kept in an xattr; PosterBoard needs it for its wallpaper store'),
        ('key-store', 'sheng-aks=1 sheng-aks-sls=3 offline-key-events=1 (AppleKeyStoreState, KeyStoreWrap.inc)', 'no passcode; wrap and unwrap for the keychain; /private/var/keybags stays empty'),
        ('software-aes', 'SoftwareAES (IOAESAccelerator)', 'AppleImage4 waits for one; without it IOKit never goes quiet and configd waits 5 minutes'),
        ('gestalt-cache-drop', 'native_gestalt_cache_drop', 'remove the MobileGestalt cache at each boot so the identity always comes from the device tree'),
        ('text-fixups', '/chosen/text-fixups (native-text-fixup patch)', 'for -[CADisplayMode preferredScale]: see display.scale_note'),
    ]
    cx.contract['kernel_features'] = [{'name': n, 'unixnu': u, 'what': w} for n, u, w in features]
    cx.contract['display']['scale_note'] = (
        '-[CADisplayMode preferredScale] (QuartzCore, read in 24A446\'s dyld_shared_cache_arm64e.01 at 0x40b1758; the cache is the '
        'one the iPad has) returns the mode\'s own preferred scale when it has one; else, in a virtual device, 2 or the mode\'s '
        'native scale field if that is more; else 2 for a mode taller than 1080 pixels and 1 otherwise. /chosen display-scale is '
        'not read there. So @2x is what it answers unchanged, and for @3x xnuhost adds two text fixups (config.c xh_scale_fixups) '
        'that make both answers 3. Not verified in a running guest: no frame of SpringBoard\'s has been rendered yet.')
    cx.say('contract', 'kernel features', f'{len(features)} things the kernel does for a sealed root (kernel_features)')


# ---- preferences -------------------------------------------------------------------------------------------------
def _fill(value, build):
    if isinstance(value, str):
        return value.replace('@BUILD@', build)
    if isinstance(value, dict):
        return {k: _fill(v, build) for k, v in value.items()}
    return value


def preferences(cx):
    off = set(cx.options.get('no_prefs', ()))
    if not cx.options.get('lighter_visuals'):
        off.add('visuals')
    by_file = {}
    for p in cx.prefs:
        if p['group'] in off:
            continue
        if p['user'] == 'any':
            path, owner, mode, parent = f'preferences/{p["domain"]}.plist', ROOT, 0o644, ROOT
        elif p['user'] == 'root':
            path, owner, mode, parent = f'root/Library/Preferences/{p["domain"]}.plist', ROOT, 0o600, ROOT
        else:
            path, owner, mode, parent = f'mobile/Library/Preferences/{p["domain"]}.plist', MOBILE, 0o600, MOBILE
        entry = by_file.setdefault(path, {'owner': owner, 'mode': mode, 'parent': parent, 'values': {}, 'groups': set()})
        entry['values'].update(_fill(p['values'], cx.build))
        entry['groups'].add(p['group'])
    groups = {}
    for path, entry in sorted(by_file.items()):
        changed = cx.data.merge_plist(path, entry['values'], entry['owner'], entry['mode'], parent_owner=entry['parent'])
        for group in entry['groups']:
            g = groups.setdefault(group, [0, 0])
            g[0] += 1
            g[1] += bool(changed)
    for group in sorted({p['group'] for p in cx.prefs}):
        if group in off:
            cx.say('off', f'preferences: {group}', 'not asked for' if group == 'visuals' else f'--no-{group}')
        else:
            files, changed = groups.get(group, (0, 0))
            cx.say('applied' if changed else 'kept', f'preferences: {group}', f'{files} files')


# ---- Data volume layout ------------------------------------------------------------------------------------------
def data_layout(cx):
    """Directories the system expects on the Data volume before its first boot."""
    made = []

    def need(path, owner, mode):
        if not cx.data.isdir(path):
            made.append(path)
        cx.data.mkdir(path, owner, mode)

    # containers: ios-sweet sweet-root-tweaks.py (owners), UniXNU make-data-seed.py (the same four)
    need('containers/Bundle', (33, 33), 0o755)
    need('containers/Data/System', ROOT, 0o755)
    need('containers/Shared/SystemGroup', ROOT, 0o755)
    need('containers/Temp', ROOT, 0o755)
    # UniXNU make-data-seed.py
    need('keybags', ROOT, 0o700)
    need('mobile/tmp', MOBILE, 0o755)
    need('tmp', ROOT, 0o1777)
    need('run', ROOT, 0o755)
    # logd stores with their markers, or logd fails with "Failed to migrate directory" (ios-sweet mkiosroot.sh)
    for path, mode in (('db/diagnostics', 0o751), ('db/uuidtext', 0o755), ('db/diagnostics/timesync', 0o755)):
        need(path, LOGD, mode)
        cx.data.write(f'{path}/.migrated', b'', LOGD, 0o644)
    cx.say('applied' if made else 'kept', 'data layout', f'made {", ".join(made)}' if made else 'containers, keybags, tmp, logd stores')

    # A MobileGestalt cache from another identity keeps its answers (iOSPlus10 notes 5338-5343): none is carried.
    cache = 'containers/Shared/SystemGroup/systemgroup.com.apple.mobilegestaltcache/Library/Caches/com.apple.MobileGestalt.plist'
    removed = cx.data.remove(cache) if cx.data.lexists(cache) else False
    cx.say('applied' if removed else 'kept', 'MobileGestalt cache', 'a stale cache removed; it is rebuilt from the device tree'
           if removed else 'none in the seed; built from the device tree at the first boot')


def keybag(cx):
    """A plain-plist system keybag for a kernel whose key store reads one (ios-sweet's AKS stand-in with
    /defaults/no-effaceable-storage). Off unless asked: UniXNU's AppleKeyStoreState keeps its state itself."""
    if not cx.options.get('plain_keybag'):
        cx.say('off', 'systembag.kb', 'not written: the key-store stand-in of the kernel owns the keybag (--plain-keybag writes '
               'ios-sweet\'s plain one)')
        return
    body = (b'<?xml version="1.0" encoding="UTF-8"?>\n'
            b'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
            b'<plist version="1.0"><dict><key>KeyBagKeys</key><data>U1dFRVQ=</data></dict></plist>\n')
    cx.data.mkdir('keybags', ROOT, 0o700)
    changed = cx.data.write('keybags/systembag.kb', body, ROOT, 0o600)
    cx.contract['notes'].append('plain keybag written: the device tree needs /defaults/no-effaceable-storage = 1 '
                                '(ios-sweet tools/mkadt.py)')
    cx.say('applied' if changed else 'kept', 'systembag.kb', 'plain property-list keybag, root 0600')


def feature_flags(cx):
    """On a root we write, the extension cache's signature no longer matches; without this flag installd and lsd
    refuse every app extension (iOSPlus10 root-tweaks-extra.py, ios26-root-spec.py). A sealed root keeps Apple's."""
    if not cx.rw:
        cx.say('kept', 'ExtensionKit flag', 'sealed root: the extension cache is Apple\'s and its signature holds')
        return
    flag = {'enforce_xpc_cache_code_signing': {'Enabled': False}}
    changed = cx.data.merge_plist('preferences/FeatureFlags/Domain/ExtensionKit.plist', flag, ROOT, 0o644, binary=False)
    system = 'System/Library/FeatureFlags/Domain/ExtensionKit.plist'
    if cx.root.exists(system):
        changed = cx.root.merge_plist(system, flag, ROOT, 0o644) or changed
    cx.say('applied' if changed else 'kept', 'ExtensionKit flag', 'enforce_xpc_cache_code_signing off')


# ---- identity and display: the booter's part ----------------------------------------------------------------------
def identity(cx):
    d = cx.device
    cx.contract['device'] = dict(d['device'])
    cx.contract['adt'] = {'root': d['adt']['root'], 'chosen': d['adt']['chosen'],
                          'product': d['adt'].get('product', {}), 'buttons': d['adt'].get('buttons', {}),
                          'identity_tree': f'the IPSW\'s DeviceTree for {d["device"]["board"]}: /product is copied from it '
                                           '(see the device profile)',
                          'required': d['adt'].get('required', {}), 'absent': d['adt'].get('absent', {})}
    cx.contract['display'] = dict(d['display'])
    cx.say('contract', 'identity', f'{d["device"]["product"]} ({d["device"]["board"]}), device class {d["device"]["device_class"]}: '
           'from the device tree the booter builds (adt); nothing in the root holds it')
    cx.say('contract', 'display', f'{d["display"]["width"]}x{d["display"]["height"]} @{d["display"]["scale"]}x '
           f'({d["display"]["points"][0]}x{d["display"]["points"][1]} points) {d["display"]["orientation"]}, '
           f'{d["display"]["refresh_hz"]} Hz: the framebuffer the booter presents (display)')
    target = d['device']['target_type']
    jetsam = f'{launchd.DAEMONS_DIR}/com.apple.jetsamproperties.{target}.plist'
    if cx.root.exists(jetsam):
        cx.say('kept', 'target-type', f'{target}: the root has its jetsam properties')
    else:
        names = [n[len('com.apple.jetsamproperties.'):-len('.plist')] for n in cx.root.listdir(launchd.DAEMONS_DIR)
                 if n.startswith('com.apple.jetsamproperties.')]
        cx.say('not-found', 'target-type', f'no jetsam properties for {target} in this root (it has: {", ".join(names) or "none"}): '
               'check target_type in the device profile against the IPSW')


ALL = (version, launchd_jobs, launchd_crash_policy, host_services, metal_plugin, app_cryptex, staged_apps, preferences,
       data_layout, keybag, feature_flags, identity, kernel_features)
