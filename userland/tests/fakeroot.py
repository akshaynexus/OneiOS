"""A tiny stand-in for an extracted iOS 27 root, made from nothing: no file of Apple's is read or copied.

It has only the shape the tweaks look at: the version plist, a compiled launchd cache with a few jobs (label
names are the real ones, the job contents are invented), the board's jetsam properties with the iOS 27
`DisableAllowed` marks, the directories the tweaks write into, and `/var -> private/var`.
"""
import hashlib
import os
import plistlib
import struct

BUILD = '24A999'  # not a real build number
TARGET = 'V54'

# label -> extra job keys. A mix of jobs the profile keeps, idles, disables and has never heard of.
JOBS = {
    'com.apple.SpringBoard': {'_PanicOnCrash': True},
    'com.apple.backboardd': {'_PanicOnCrash': True, 'EnvironmentVariables': {'CA_EXAMPLE': '1'}},
    'com.apple.runningboardd': {},
    'com.apple.CommCenter': {},
    'com.apple.CommCenterMobileHelper': {},
    'com.apple.CommCenterRootHelper': {},
    'com.apple.sleepd': {},
    'com.apple.cloudd': {},
    'com.apple.locationd': {},
    'com.apple.mobileactivationd': {},
    'com.apple.watchdogd': {},
    'com.apple.analyticsd': {},
    'com.apple.OTATaskingAgent': {},
    'com.apple.mobile.softwareupdated': {},
    'com.apple.nfcd': {},
    'com.apple.seld': {},
    'com.apple.assistantd': {},
    'com.apple.momentsd': {},
    'com.apple.coreduetd': {},
    'com.apple.generativeexperiencesd': {},
    'com.apple.biometrickitd': {},
    'com.apple.nearbyd': {},
    'com.apple.example.unknownd': {},
}
FORCED = ['com.apple.analyticsd', 'com.apple.OTATaskingAgent', 'com.apple.mobile.softwareupdated',
          'com.apple.mobileactivationd', 'com.apple.logd']


def _write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f:
        f.write(data)


def make(top, jobs=None, with_jetsam=True):
    """Makes the OS volume in `top`. The Data volume is `top/private/var` (one tree, as on an edited root)."""
    jobs = JOBS if jobs is None else jobs
    for directory in ('System/Library/LaunchDaemons', 'System/Library/Extensions', 'System/Library/xpc',
                      'System/Library/CoreServices', 'sbin', 'usr/libexec', 'private/var/mobile/Library/Preferences',
                      'private/var/root', 'private/var/db', 'private/preboot'):
        os.makedirs(os.path.join(top, directory), exist_ok=True)
    os.symlink('private/var', os.path.join(top, 'var'))
    _write(os.path.join(top, 'System/Library/CoreServices/SystemVersion.plist'), plistlib.dumps({
        'ProductBuildVersion': BUILD, 'ProductName': 'iPhone OS', 'ProductVersion': '27.0.1'}))
    cache = {'LaunchDaemons': {}, 'Boot': {'/System/Library/xpc/boot.plist': {'Label': 'com.apple.example.boot'}}}
    for label, extra in jobs.items():
        job = {'Label': label, 'ProgramArguments': [f'/usr/libexec/{label.rsplit(".", 1)[-1]}'], **extra}
        cache['LaunchDaemons'][f'/System/Library/LaunchDaemons/{label}.plist'] = job
        _write(os.path.join(top, f'System/Library/LaunchDaemons/{label}.plist'), plistlib.dumps(job))
    _write(os.path.join(top, 'System/Library/xpc/launchd.plist'), plistlib.dumps(cache, fmt=plistlib.FMT_BINARY))
    _write(os.path.join(top, 'sbin/launchd'), b'not a real launchd\n')
    if with_jetsam:
        override = {label: {'DisableAllowed': False, 'ActiveSoftMemoryLimit': 10} for label in FORCED}
        for label in ('com.apple.backboardd', 'com.apple.SpringBoard', 'com.apple.runningboardd', 'com.apple.configd'):
            override[label] = {'RecoverOnCrash': True, 'ActiveSoftMemoryLimit': 100}
        jetsam = {'Version4': {'Daemon': {'Override': override, 'Default': {}},
                               'XPCService': {'Override': {'com.apple.example.xpc': {'DisableAllowed': False}}}}}
        _write(os.path.join(top, f'System/Library/LaunchDaemons/com.apple.jetsamproperties.{TARGET}.plist'),
               plistlib.dumps(jetsam, fmt=plistlib.FMT_BINARY))
    return top


def signed_macho(identifier='com.oneios.test', arm64e=True, page_shift=12, adhoc=True, payload=b'code'):
    """A minimal arm64 Mach-O with an embedded signature: a header, LC_BUILD_VERSION (iOS 27.0),
    LC_CODE_SIGNATURE and one SHA-256 code directory. Enough for `roottweak.macho`; it cannot run."""
    ident = identifier.encode() + b'\0'
    hash_offset = 44 + len(ident)
    code_hash = hashlib.sha256(payload).digest()
    directory_length = 8 + 44 - 8 + len(ident) + len(code_hash)
    fields = struct.pack('>IIIIIIIBBBBI',
                         0x20400,               # version
                         0x2 if adhoc else 0,   # flags
                         hash_offset,           # hashOffset
                         44,                    # identOffset
                         0,                     # nSpecialSlots
                         1,                     # nCodeSlots
                         len(payload),          # codeLimit
                         32, 2, 0, page_shift,  # hashSize, hashType (SHA-256), platform, pageSize
                         0)                     # spare2
    directory = struct.pack('>II', 0xFADE0C02, 8 + len(fields) + len(ident) + len(code_hash)) + fields + ident + code_hash
    assert len(directory) == directory_length
    superblob = struct.pack('>IIIII', 0xFADE0CC0, 20 + len(directory), 1, 0, 20) + directory
    build = struct.pack('<IIIIII', 0x32, 24, 2, 27 << 16, 27 << 16, 0)
    signature_command_size = 16
    commands_size = len(build) + signature_command_size
    signature_offset = 32 + commands_size + len(payload)
    signature_command = struct.pack('<IIII', 0x1D, signature_command_size, signature_offset, len(superblob))
    subtype = (0x80000000 | 2) if arm64e else 0
    header = struct.pack('<IIIIIIII', 0xFEEDFACF, 0x0100000C, subtype, 2, 2, commands_size, 0, 0)
    data = header + build + signature_command + payload + superblob
    return data, hashlib.sha256(directory).digest()[:20].hex()
