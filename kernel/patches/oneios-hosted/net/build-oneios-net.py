#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause-Patent
"""The hosted kernel's Wi-Fi, en0: build/oneios-net/liboneiosnet.a, built between the two kernel passes from the
compile records of the first. README.md beside this file; docs/hardware.md section 3.

  OneiOSWifi.cpp            the Wi-Fi controller and interface wifid drives, their station the host's, and
                            oneios_net_start (oneios_drivers_start calls it): subclasses of the collection's own
                            IOEthernetController and IOEthernetInterface (Apple's IONetworkingFamily is a root of the
                            collection and 25 other roots link it: it cannot be replaced by a copy of ours)
  OneiOSWifiLayout.cpp      IONetworkInterface::setInterfaceSubType's store, with the family's member layout
  DroidVMVirtioNet.c        UniXNU-droidvm's virtio-net transport (external/UniXNU-droidvm/package, f9121cd), staged:
                            its receive filter passes multicast too (IPv6 neighbour discovery, mDNS), compiled as its
                            iPadOS 27 board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1)
  bindings                  generated: a stub per original function the objects name, the parents' metaclasses, and
                            the parents' vtables for calls by qualified name (as hid/build-oneios-hid.py makes them)

The classes are compiled against Apple's IONetworkingFamily-188 headers (APSL 2.0,
github.com/apple-oss-distributions/IONetworkingFamily at bb910e2, fetched once into out/sources/IONetworkingFamily)
with __PRIVATE_SPI__. Before anything is built, a probe subclass of each parent is compiled and its vtable compared
with IONetworking-24A446-v54.json (layout.py: what the kernelcache says): the number of slots, every slot's
pointer-authentication discriminator, and whether the slot's function is the family's or the kernel's. On
2026-11-12: 250 of 250 and 221 of 221 slots, and IONetworkMedium's 23, the same. Each original function is reached
through `bti c; b <name>.original`, an absolute symbol at the function's address as linked in kernel.kc that
XnuPrelink's emitter retargets (classes.py, README of kernel/prelink).

  build-oneios-net.py       run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository

No Apple binary is read.
"""
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

TREE = Path.cwd()
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-net: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/net'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
XNU = TREE / '.cache/xnu-work'
PACKAGE = REPO / 'external/UniXNU-droidvm/package'
TRANSPORT = PACKAGE / 'DroidVMVirtioNet.c'
START = HERE / 'OneiOSWifi.cpp'
SUBTYPE = HERE / 'OneiOSWifiLayout.cpp'
LAYOUT = HERE / 'IONetworking-24A446-v54.json'
HID_BUILD = REPO / 'kernel/patches/oneios-hosted/hid/build-oneios-hid.py'
PROTO = REPO / 'drivers/net/oneios_wifi_proto.h'
IONET_TAG, IONET_COMMIT = 'IONetworkingFamily-188', 'bb910e2b458939f6b1f53c0fa6907f258b431ea4'
IONET_REPOSITORY = 'https://github.com/apple-oss-distributions/IONetworkingFamily.git'
IONET_FILES = ['IOEthernetController', 'IOEthernetInterface', 'IOKernelDebugger', 'IOMbufMemoryCursor', 'IONetworkController',
               'IONetworkData', 'IONetworkInterface', 'IONetworkMedium', 'IONetworkStack', 'IONetworkUserClient', 'IOOutputQueue',
               'IOPacketQueue']
REQUIRED = ['_oneios_net_start', '_droidvm_net_start', '_droidvm_net_tx_frame', '_droidvm_net_if_attach', '_droidvm_net_if_input',
            '__GLOBAL__sub_I_OneiOSWifi.cpp', '_oneios_net_set_subtype', '_oneios_net_controller_meta_link',
            '_oneios_net_interface_meta_link']
# The original classes: (name as mangled, the record's key, the probe class and what it must override)
PARENTS = {'IOEthernetController': '20IOEthernetController', 'IOEthernetInterface': '19IOEthernetInterface'}
# The family's classes a mangled name can be qualified by; any other qualifier is the kernel's.
FAMILY = ('IOEthernetController', 'IOEthernetInterface', 'IONetworkController', 'IONetworkInterface', 'IONetworkMedium',
          'IONetworkData', 'IOOutputQueue', 'IOKernelDebugger', 'IONetworkStack', 'IOMbufMemoryCursor', 'IOPacketQueue')


def fail(text):
    sys.exit('build-oneios-net: ' + text)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(cmd, **kw):
    return subprocess.run([str(x) for x in cmd], check=True, capture_output=True, text=True, timeout=900, **kw).stdout


def family():
    source = REPO / 'out/sources/IONetworkingFamily'
    if not (source / '.git').is_dir():
        source.parent.mkdir(parents=True, exist_ok=True)
        run(['git', 'clone', '-q', '--depth', '1', '--branch', IONET_TAG, IONET_REPOSITORY, source])
    if run(['git', '-C', source, 'rev-parse', 'HEAD']).strip() != IONET_COMMIT:
        fail(f'{source}: {IONET_TAG} must be {IONET_COMMIT}')
    if sorted(p.stem for p in source.glob('*.cpp')) != sorted(IONET_FILES):
        fail(f'{IONET_TAG}: not the twelve family sources')
    return source


def sdk_header(record, name):
    """A header of the iphoneos SDK the record compiles against (-isysroot), else Xcode's."""
    args = record['arguments']
    roots = [args[i + 1] for i, a in enumerate(args[:-1]) if a == '-isysroot']
    if not roots and shutil.which('xcrun'):
        roots = [run(['xcrun', '--sdk', 'iphoneos', '--show-sdk-path']).strip()]
    for root in roots:
        path = Path(root) / 'usr/include' / name
        if path.is_file():
            return path
    fail(f'no {name} in the SDK ({roots or "no -isysroot in the record and no xcrun"})')


def includes(out, source, record):
    """<IOKit/network/...> for the family's headers, the SDK's TargetConditionals.h, and the mbuf crumbs header."""
    include = out / 'ionetworking-include'
    (include / 'IOKit/network').mkdir(parents=True, exist_ok=True)
    for link, target in [*((include / 'IOKit/network' / h.name, h) for h in source.glob('*.h')),
                         (include / 'TargetConditionals.h', sdk_header(record, 'TargetConditionals.h'))]:
        link.unlink(missing_ok=True)
        link.symlink_to(target)
    # xnu-12377's sys/mbuf.h has inline helpers that pass an mbuf_t (the KPI's opaque struct __mbuf *) to functions
    # taking struct mbuf *; the family never calls them. Overloads taking mbuf_t, declared first, let them compile
    # (build-droidvm-devices.py's fix).
    (include / 'oneios-mbuf-crumbs.h').write_text(
        '// Generated by build-oneios-net.py: see there.\n#pragma once\n#include <stdint.h>\n#include <sys/kpi_mbuf.h>\n'
        'void m_add_hdr_crumb(mbuf_t, uint64_t, uint64_t);\nvoid m_add_hdr_crumb_chain(mbuf_t, uint64_t, uint64_t);\n')
    return include


def replace_once(text, pairs, what):
    for old, new in pairs:
        if text.count(old) != 1:
            fail(f'{what} no longer has exactly one {old.strip()[:60]!r}')
        text = text.replace(old, new)
    return text


def stage_transport(out):
    """DroidVMVirtioNet.c with multicast passed: on the phone's Wi-Fi the guest needs IPv6's neighbour discovery
    (all-nodes and solicited-node groups) and mDNS; iOS's own stack drops groups it has not joined."""
    text = replace_once(TRANSPORT.read_text(), (
        ('            if (broadcast || ours) {\n',
         '            if (broadcast || ours || (dst[0] & 1)) {   // OneiOS (build-oneios-net.py): multicast too\n'),
    ), 'DroidVMVirtioNet.c')
    staged = out / 'DroidVMVirtioNet.c'
    staged.write_text(text)
    return staged


PROBE = '''// Generated by build-oneios-net.py: one subclass of each original class, whose vtables are compared with the record.
#include <IOKit/network/IOEthernetController.h>
#include <IOKit/network/IOEthernetInterface.h>
class OneiOSNetCheckController : public IOEthernetController {
	OSDeclareDefaultStructors(OneiOSNetCheckController)
public:
	IOReturn getHardwareAddress(IOEthernetAddress *address) override;
};
OSDefineMetaClassAndStructors(OneiOSNetCheckController, IOEthernetController)
IOReturn OneiOSNetCheckController::getHardwareAddress(IOEthernetAddress *) { return kIOReturnUnsupported; }
class OneiOSNetCheckInterface : public IOEthernetInterface {
	OSDeclareDefaultStructors(OneiOSNetCheckInterface)
};
OSDefineMetaClassAndStructors(OneiOSNetCheckInterface, IOEthernetInterface)
'''
PROBES = {'IOEthernetController': '__ZTV24OneiOSNetCheckController', 'IOEthernetInterface': '__ZTV23OneiOSNetCheckInterface'}


def owner(name):
    """The class a mangled member function name is qualified by (__ZN[K]<len><class>...), or None."""
    import re
    match = re.match(r'__ZNK?(\d+)', name)
    if not match:
        return None
    length = int(match.group(1))
    start = match.end()
    return name[start:start + length]


def check_layout(layout, hid, probe_obj):
    """{mangled family function: linked address}, and per parent the probe's vtable; stops when the headers are not
    the record's classes."""
    data = probe_obj.read_bytes()
    address_of, tables = {}, {}
    for parent, symbol in PROBES.items():
        slots = layout['classes'][parent]['slots']
        table = hid.vtable(data, symbol)
        if len(table) != len(slots):
            fail(f'{parent}: the headers give a vtable of {len(table)} slots, the kernelcache\'s has {len(slots)}')
        probe_class = symbol[len('__ZTV'):].lstrip('0123456789')
        for i, ((name, diversity, address, key), slot) in enumerate(zip(table, slots)):
            if diversity != int(slot['diversity'], 16) or not address or key != 0:
                fail(f'{parent} slot {i} ({name}): signed with {diversity:#06x}, the kernelcache\'s slot with {slot["diversity"]}')
            if probe_class in name:
                continue                     # the destructors, getMetaClass, the probe's override
            family = owner(name) in FAMILY
            if family != slot['family']:
                fail(f'{parent} slot {i} ({name}): the kernelcache\'s slot is {"the family" if slot["family"] else "the kernel"}\'s')
            if family and address_of.setdefault(name, int(slot['linked'], 16)) != int(slot['linked'], 16):
                fail(f'{name}: two addresses in the record')
        tables[parent] = table
    return address_of, tables


def bindings(layout, address_of, tables, imports):
    """The assembly of the stubs, the metaclasses, the parents' vtables for qualified calls, and the stubs' targets."""
    functions = {}
    for name in sorted(imports):
        cls = owner(name)
        if name in address_of:
            functions[name] = address_of[name]
        elif cls in PARENTS and name.startswith(f'__ZN{PARENTS[cls]}C2EPK11OSMetaClass'):
            functions[name] = int(layout['classes'][cls]['constructor']['linked'], 16)
        elif cls in PARENTS and name == f'__ZN{PARENTS[cls]}D2Ev':
            functions[name] = int(layout['classes'][cls]['base_destructor']['linked'], 16)
        elif name in (f'__ZN{m}10gMetaClassE' for m in PARENTS.values()) or name in (f'__ZTV{m}' for m in PARENTS.values()):
            continue
        else:
            fail(f'{name}: not a virtual function, constructor or destructor of the original classes (a non-virtual '
                 'function of the family has no slot to bind it by)')
    lines = [f'// Generated by build-oneios-net.py from {LAYOUT.name} (kernelcache {layout["kernelcache_sha256"][:16]}...): see there.']
    for cls, mangled in PARENTS.items():
        c = layout['classes'][cls]
        meta = int(c['meta_class_linked'], 16)
        short = 'controller' if cls == 'IOEthernetController' else 'interface'
        lines += [f'.globl __ZN{mangled}10gMetaClassE', f'.set __ZN{mangled}10gMetaClassE, {meta:#x}',
                  '.section __DATA,__const', '.p2align 3',
                  f'.globl _oneios_net_{short}_meta_link', f'_oneios_net_{short}_meta_link: .quad {meta:#x}',
                  f'.globl _oneios_net_{short}_size', f'_oneios_net_{short}_size: .long {int(c["size"], 16):#x}',
                  # The parent's table for calls by qualified name (super::start and the like): every slot signed for
                  # its place here with the original discriminator, the family's functions through their stubs, the
                  # kernel's by their own names; the probe's own slots (destructors, getMetaClass, the abstract one) 0.
                  '.p2align 3', f'.globl __ZTV{mangled}', f'__ZTV{mangled}:', ' .quad 0', ' .quad 0']
        probe_class = PROBES[cls][len('__ZTV'):].lstrip('0123456789')
        for (name, diversity, _, _), slot in zip(tables[cls], c['slots']):
            if probe_class in name:
                lines.append(' .quad 0')
            else:
                if owner(name) in FAMILY:
                    functions.setdefault(name, int(slot['linked'], 16))
                lines.append(f' .quad {name}@AUTH(ia,{diversity:#x},addr)')
        lines.append('.text')
    targets = [lines[0]]
    for name, address in sorted(functions.items()):
        lines += [f'.globl {name}', '.p2align 2', f'{name}:', ' bti c', f' b {name}.original']
        targets += [f'.globl {name}.original', f'.set {name}.original, {address:#x}']
    return '\n'.join(lines) + '\n', '\n'.join(targets) + '\n', functions


def main():
    for path in (REPO / 'drivers/build.py', HID_BUILD, TRANSPORT, START, SUBTYPE, PROTO, LAYOUT, PACKAGE / 'DroidVMDmaRegion.h',
                 RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', XNU / 'bsd'):
        if not path.exists():
            fail(f'{path} missing (scripts/setup/get-references.sh UniXNU-droidvm; kernel/fetch.sh)')
    drivers = drivers_build()
    spec = importlib.util.spec_from_file_location('oneios_hid_build', HID_BUILD)
    hid = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(hid)          # its Mach-O readers: symbols, vtable
    layout = json.loads(LAYOUT.read_text())
    out = TREE / 'build/oneios-net'
    out.mkdir(parents=True, exist_ok=True)
    iokit = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    source = family()
    include = includes(out, source, iokit)
    # The family's headers as the family itself saw them (__PRIVATE_SPI__ changes IONetworkController's and
    # IONetworkInterface's layout); sys/systm.h first, the bsd source tree last for private headers the export set
    # leaves out (build-droidvm-devices.py's universe).
    lib = ['-DLIBKERN_CONSUMED=', '-DLIBKERN_RETURNS_RETAINED=', '-DLIBKERN_RETURNS_NOT_RETAINED=', '-DLIBKERN_CONSUMES_THIS=']
    universe = [*lib, '-include', 'sys/systm.h', '-include', str(include / 'oneios-mbuf-crumbs.h'), '-I', str(XNU / 'bsd'),
                '-D__PRIVATE_SPI__', '-isystem', str(include), '-Wno-c99-designator', '-Wno-reorder-init-list',
                '-Wno-deprecated-enum-enum-conversion', '-Wno-unnecessary-virtual-specifier', '-Wno-macro-redefined']

    def compile_source(kind, src, obj, extra=()):
        record = drivers.record(RECORDS, drivers.RECORDS[kind])
        command = drivers.command(record, src, obj) + ['-I', str(XNU / 'osfmk'), '-I', str(PACKAGE), '-I', str(REPO / 'drivers/net')]
        command += list(extra)
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-8000:])
            fail(f'{src.name} did not compile')
        return obj

    probe = out / 'OneiOSNetCheck.cpp'
    probe.write_text(PROBE)
    address_of, tables = check_layout(layout, hid, compile_source('iokit', probe, out / 'OneiOSNetCheck.o', universe))
    print(f'build-oneios-net: the IONetworkingFamily-188 headers are the kernelcache\'s classes: '
          + ', '.join(f'{c} {len(t)} slots' for c, t in tables.items()) + f', {len(address_of)} functions of the family placed')

    objcopy = shutil.which('llvm-objcopy') or 'objcopy'
    wifi = compile_source('iokit', START, out / 'OneiOSWifi.o', universe)
    late = wifi.with_suffix('.late.o')
    # The classes' static initializer registers them below the original classes: it runs inside oneios_net_start's
    # module load, not with the kernel's own initializers. Out of __mod_init_func, and global.
    subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func', '--globalize-symbol=__GLOBAL__sub_I_OneiOSWifi.cpp',
                    str(wifi), str(late)], check=True)
    subtype = compile_source('iokit', SUBTYPE, out / 'OneiOSWifiLayout.o', [*universe, '-Wno-keyword-macro'])
    transport = compile_source('mach', stage_transport(out), out / 'DroidVMVirtioNet.o', ['-DDROIDVM_DEVICE_DYNAMIC=1'])
    objects = [late, subtype, transport]
    sections = subprocess.run([shutil.which('llvm-objdump') or 'objdump', '-h', str(late)], capture_output=True, text=True).stdout
    if '__mod_init_func' in sections:
        fail('OneiOSWifi.late.o still has a __mod_init_func section: its classes would register twice')

    # Our vtables keep the originals' slots: the discriminators, and the original function wherever a class does not
    # override it.
    data = late.read_bytes()
    owned = {}
    for name in [n for n, _, kind in hid.symbols(data) if n.startswith('__ZTV') and kind & 0xe == 0xe and '9MetaClassE' not in n]:
        table = hid.vtable(data, name)
        parent = 'IOEthernetInterface' if 'Interface' in name else 'IOEthernetController'
        slots = layout['classes'][parent]['slots']
        if len(table) != len(slots):
            fail(f'{name}: {len(table)} slots, the original {parent} has {len(slots)}')
        for i, (symbol, diversity, address, key) in enumerate(table):
            if diversity != int(slots[i]['diversity'], 16) or not address or key:
                fail(f'{name} slot {i} ({symbol}): not signed as the original slot')
            if owner(symbol) in FAMILY and address_of.get(symbol) != int(slots[i]['linked'], 16):
                fail(f'{name} slot {i} ({symbol}): not the original function of that slot')
        owned[name] = [i for i, (symbol, *_) in enumerate(table) if owner(symbol) not in FAMILY and slots[i]['family']]

    imports = {n for obj in objects for n, _, kind in hid.symbols(obj.read_bytes()) if kind == 1 and owner(n) in FAMILY}
    imports |= {n for n, _, kind in hid.symbols(data) if kind == 1 and n.startswith(tuple(f'__ZTV{m}' for m in PARENTS.values()))}
    for obj in objects:
        if any(n.endswith('9metaClassE') and owner(n) in FAMILY for n, _, kind in hid.symbols(obj.read_bytes()) if kind == 1):
            fail(f'{obj.name} uses a family class\'s metaClass (OSDynamicCast to an original class): not bound')
    stubs, targets, functions = bindings(layout, address_of, tables, imports)
    arguments = iokit['arguments']
    compiler = next(a for a in arguments if 'clang' in Path(a).name)
    triple = arguments[arguments.index('-target') + 1] if '-target' in arguments else 'arm64e-apple-ios27.0'
    for stem, text in (('IONetworking-bindings', stubs), ('IONetworking-originals', targets)):
        src = out / (stem + '.S')
        src.write_text(text)
        subprocess.run([compiler, '-target', triple, '-c', str(src), '-o', str(src.with_suffix('.o'))], check=True, cwd=iokit['directory'])
        objects.append(src.with_suffix('.o'))

    archive = out / 'liboneiosnet.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True,
                             text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    sources = [TRANSPORT, START, SUBTYPE, PROTO, LAYOUT, Path(__file__).resolve()]
    (out / 'objects.json').write_text(json.dumps({
        'ionetworking_headers': {'tag': IONET_TAG, 'commit': IONET_COMMIT},
        'sources': [{'path': str(p.relative_to(REPO)) if p.is_relative_to(REPO) else str(p),
                     'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in sources]}, indent=2) + '\n')
    (out / 'report.json').write_text(json.dumps({'kernelcache_sha256': layout['kernelcache_sha256'],
                                                 'bindings': {n: hex(a) for n, a in functions.items()}, 'owned_slots': owned}, indent=2) + '\n')
    print(f'build-oneios-net: {archive} ({len(objects)} objects, {len(functions)} original functions bound at their 24A446 '
          f'iPhone18,2 addresses; {", ".join(f"{n[5:]} owns {len(s)} of the family\'s slots" for n, s in owned.items())})')


if __name__ == '__main__':
    main()
