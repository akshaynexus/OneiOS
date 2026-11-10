#!/usr/bin/env python3
"""Which kexts of the iPhone18,2 kernelcache go into OneiOS's kernel.kc, and why each one that does not.

usage: roots.py KERNELCACHE OUT.json [PROFILE.toml]

Rule (kernel/prelink/README.md): every kext of the collection is a root, except a kext all of whose IOKit
personalities attach to hardware that xnuhost does not present (an Apple SoC block, I2C/SPI/SPMI part, RTBuddy or
AFK coprocessor endpoint, SEP endpoint, PCIe, USB, Thunderbolt or accessory transport, AOP/SPU sensor hub). Kexts with
no personality (families, libraries, KPIs) and kexts with any personality on a software provider (IOResources, the
storage and HID stacks, disk images, APFS) stay. The overrides below are decided by hand and carry their reason.
A left-out kext that a kept kext links against comes back as a dependency, for its code only: XnuPrelink drops the
personalities of every non-root (fileset_emit.rebase_collection).
"""
import json
import plistlib
import struct
import sys

# Provider classes that only exist on Apple hardware absent under xnuhost.
HARDWARE_PROVIDERS = {
    'AppleARMIODevice', 'AppleARMIICDevice', 'AppleARMIISDevice', 'AppleARMSPIDevice', 'AppleARMSPMIDevice',
    'AppleARMNORFlashDevice', 'AppleARMPWMDevice', 'AppleA7IOPNub', 'AppleSEPIntelIOPNub', 'AppleSEPDeviceService',
    'AppleSEPManager', 'RTBuddy', 'RTBuddyService', 'RTBuddyEndpointService', 'AFKEndpointInterface',
    'AFKMailboxEndpointInterface', 'AFKTightbeamEndpoint', 'AppleAOP2KextTightbeamEndpoint', 'AppleSPUAppInterface',
    'AppleSPUAppDriver', 'AppleSPUFastpathInterface', 'AppleSPUHIDInterface', 'AppleSPUInterface',
    'AppleAOPAudioDeviceNode', 'AppleAOPAudioDeviceProvider', 'IOPAudioNode', 'ApplePMGRNub', 'AppleSMC',
    'AppleSMCInterface', 'AppleDialogSPMIPMU', 'AppleDockChannelDevice', 'AppleSmartIODMANub', 'AppleNANDConfigAccess',
    'AppleBaseband', 'AppleBasebandPCIMHIInterface', 'AppleC26DeviceStream', 'AppleCSI2CController',
    'AppleCallbackPowerSourceProvider', 'AppleDisplayCrossbar', 'AppleHIDTransportInterface', 'AppleHPMBusDevice',
    'AppleHPMDevice', 'AppleHPMDeviceHAL', 'AppleTMP103', 'AppleAuthCPRelayInterface', 'AppleOnboardSerialSync',
    'AppleUVDMEndpoint', 'AppleEmbeddedUSBDevice', 'AppleT8101USB20XHCILightningPort', 'AppleUSBADCAudioNub',
    'AppleUSBDeviceAudioNub', 'AppleUSBNCM11Control', 'AppleUSBNCMControl', 'IOPCIDevice', 'IOThunderboltPort',
    'IOThunderboltLocalNode', 'IOThunderboltXDomainService', 'IOUSBHostDevice', 'IOUSBHostInterface', 'IOUSBDevice',
    'IOUSBInterface', 'IOUSBDeviceController', 'IOUSBDeviceInterface', 'IOUSBMassStorageDriverNub',
    'IOUSBMassStorageInterfaceNub', 'com_apple_driver_AppleUSBCardReaderInterfaceNub', 'IOSCSIPeripheralDeviceNub',
    'IOSCSILogicalUnitNub', 'IOAccessoryBulkDataEndpoint', 'IOAccessoryDock9Pin', 'IOAccessoryIDBusBulkDataEndpoint',
    'IOAccessoryIDBusTransport', 'IOAccessoryManager', 'IOAccessoryManagerAID', 'IOAccessoryManagerHSAID',
    'IOAccessoryManagerOrion', 'IOPortTransportAIDBus', 'IOPortTransportComponentCC', 'IOMikeyBusDevice',
    'IOMikeyBusFunctionGroup', 'IODPController', 'IODPDevice', 'IOAVDevice', 'IOAVAudioInterface',
    'IOAVSliceVideoInterface', 'DCPAVAudioInterfaceProxy', 'DCPAVCECInterfaceProxy', 'IODARTMapperNub',
    'IONVMeEffaceableDevice', 'PSVR2SenseDevice', 'IOSerialStreamSync',
}
# A name-matched IOService or IOPlatformDevice personality attaches to a device-tree node. xnuhost's ADT
# (runtime/loader/src/adt.c) presents these of the names Apple's kexts match; any other node is absent.
PRESENTED_NODE_NAMES = {'defaults'}

# The roots of the OS profile ([kernel.prelink] roots of PROFILE.toml: the 31 Droid-VM's iPadOS 27.0.1 guest runs with)
# are kept whatever their personalities, as there. Left out by hand although the rule would keep them:
FORCE_OUT = {
    'com.apple.driver.FairPlayIOKit': 'not hardware: its code is obfuscated and reaches kernel functions through computed '
                                      'tables, so an unpaired target cannot be named from the call site (its start '
                                      'panics on the stub at 0xfffffe0009be7240). UniXNU keeps it out of its roots as '
                                      'well; userland disables fairplayd.H2 (docs/agents.md)',
    'com.apple.kec.pthread': 'the kernel links libpthread\'s kernel part (kernel/README.md); a second pthread '
                             'registration from the original kext is refused by pthread_kext_register',
    'com.apple.driver.AppleSEPKeyStore': 'the keystore runs on the SEP, which xnuhost does not present '
                                         '(matched on IOResources, it waits for AppleSEPManager)',
    'com.apple.driver.AppleSEPCredentialManager': 'a SEP endpoint client (matched on IOResources); no SEP',
    'com.apple.security.AKSAnalytics': 'analytics of the SEP keystore (AppleSEPKeyStore); no SEP',
    'com.apple.driver.AppleT8150ANEHAL': 'the T8150 Neural Engine\'s HAL (matched on IOResources); no ANE',
}


def personalities_hardware_only(entry):
    found = entry.get('IOKitPersonalities') or {}
    if not found:
        return False, 'no personality'
    for personality in found.values():
        provider = personality.get('IOProviderClass', '')
        if provider in HARDWARE_PROVIDERS:
            continue
        names = personality.get('IONameMatch', personality.get('compatible'))
        if provider in ('IOService', 'IOPlatformDevice') and names is not None:
            names = [names] if isinstance(names, str) else list(names)
            if not set(names) & PRESENTED_NODE_NAMES:
                continue
        return False, f'a personality on {provider}'
    return True, 'every personality attaches to absent hardware'


def describe(entry):
    out = []
    for personality in (entry.get('IOKitPersonalities') or {}).values():
        provider = personality.get('IOProviderClass', '?')
        names = personality.get('IONameMatch', personality.get('compatible'))
        if names is not None:
            names = [names] if isinstance(names, str) else list(names)
            out.append(f'{provider}[{",".join(names[:3])}{",..." if len(names) > 3 else ""}]')
        else:
            out.append(provider)
    return sorted(set(out))


def main():
    cache_path, out_path = sys.argv[1:3]
    roots_profile = []
    if len(sys.argv) > 3:
        import tomllib
        roots_profile = tomllib.load(open(sys.argv[3], 'rb'))['kernel']['prelink']['roots']
    data = open(cache_path, 'rb').read()
    ncmds, = struct.unpack_from('<I', data, 16)
    off, info = 32, None
    for _ in range(ncmds):
        cmd, length = struct.unpack_from('<II', data, off)
        if cmd == 0x19 and data[off + 8:off + 24].rstrip(b'\0') == b'__PRELINK_INFO':
            fileoff, filesize = struct.unpack_from('<QQ', data, off + 40)
            info = plistlib.loads(data[fileoff:fileoff + filesize].split(b'\0', 1)[0])
        off += length
    entries = info['_PrelinkInfoDictionary']
    by_id = {e['CFBundleIdentifier']: e for e in entries}
    roots, out = [], {}
    for e in entries:
        ident = e['CFBundleIdentifier']
        if not e.get('_PrelinkExecutableLoadAddr'):
            continue        # codeless entries (KPIs, personality-only bundles) come with what links them
        if ident in roots_profile:
            roots.append(ident); continue
        if ident in FORCE_OUT:
            out[ident] = {'reason': FORCE_OUT[ident], 'personalities': describe(e)}; continue
        hardware, why = personalities_hardware_only(e)
        if hardware:
            out[ident] = {'reason': why, 'personalities': describe(e)}
        else:
            roots.append(ident)
    missing = sorted(set(roots_profile) - set(by_id))
    closure, pending = set(), list(roots)
    while pending:
        ident = pending.pop()
        if ident in closure:
            continue
        closure.add(ident)
        pending.extend((by_id[ident].get('OSBundleLibraries') or {}).keys())
    back = sorted(i for i in closure if i in out)
    for i in back:
        out[i]['returns_as_dependency_of'] = sorted(r for r in roots if i in (by_id[r].get('OSBundleLibraries') or {}))
    record = {
        'kernelcache_entries': len(entries),
        'kernelcache_executables': sum(1 for e in entries if e.get('_PrelinkExecutableLoadAddr')),
        'profile_roots_missing': missing,
        'roots': sorted(roots),
        'left_out': dict(sorted(out.items())),
        'left_out_but_linked': back,
        'closure': sorted(closure),
    }
    json.dump(record, open(out_path, 'w'), indent=1)
    print(f'roots: {len(roots)} roots, {len(out)} left out ({len(back)} of them return as dependencies), '
          f'{len(closure)} in the closure; profile roots missing: {missing or "none"}')


if __name__ == '__main__':
    main()
