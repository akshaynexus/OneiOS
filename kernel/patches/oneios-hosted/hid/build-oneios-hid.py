#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause-Patent
"""The hosted kernel's touch screen, buttons and light sensor: build/oneios-hid/liboneioshid.a, built between the
two kernel passes from the compile records of the first.

  NativeHidInput.cpp        UniXNU's ShengHidInput (external/NativeHidInput at 24f1f3f, its arm64e source), staged with
                            the phone's changes (stage_hid below): two HID devices under the collection's original
                            IOHIDDevice, the touch screen (Digitizer/Touch Screen, ten fingers) and the buttons
  DroidVMVirtioInput.c      UniXNU-droidvm's virtio-input transport (external/UniXNU-droidvm/package), staged with the
                            side keys (stage_transport below, OneiOSSideKeys.inc): xnuhost's /arm-io/input-N
  OneiOSHidInput.cpp        oneios_hid_input_start (oneios_drivers_start calls it, drivers/bridge/OneiOSBridge.c):
                            loads the family, registers the classes, publishes the devices, says what attached
  OneiOSALSDevice.cpp       drivers/brightness: the host's light sensor as a HID device (fable-brightness)
  bindings                  generated: one stub per original IOHIDDevice function the objects name

The layout of the original class is OriginalHidDeviceABI.h (this folder); IOHIDDevice-24A446-v54.json is what the
kernelcache says about it (layout.py). Before anything is built the header is checked against the record:

  1. a class that overrides only newReportDescriptor is compiled against the header;
  2. its vtable must have exactly the record's number of slots, every entry signed with the record's
     discriminator for that slot (IA key, address diversity);
  3. an entry names an IOHIDDevice function exactly where the record's slot points into the family's code (the
     destructors and getMetaClass aside, which every class owns);
  4. the header's comment table must list the record's discriminators.

Each original function is reached through a stub in kernel text, `bti c; b <name>.original`, with `<name>.original`
an absolute symbol of a second object at the function's address as linked in kernel.kc; XnuPrelink's emitter
retargets the branch when it places the kernel (PC-relative: right at any slide). The slot of a function is the
one its name fills in the check class's vtable. The metaclass is data: an absolute symbol, with its link address and
the instance size as numbers for the check at start (OneiOSHidInput.cpp).

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES; kernel/build-hosted.sh copies this script into the build's device
repository and adds it to the board's archives. Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the
device repository. No Apple binary is read. After UniXNU-droidvm's ipados27/scripts/build-droidvm-hid-input.py
(BSD-2-Clause-Patent), which does this for the iPad collection.
"""
import hashlib
import importlib.util
import json
import os
import re
import shutil
import struct
import subprocess
import sys
from pathlib import Path

TREE = Path.cwd()
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-hid: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/hid'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
HID_SOURCE = REPO / 'external/NativeHidInput/NativeHidInput.cpp'
TRANSPORT = REPO / 'external/UniXNU-droidvm/package/DroidVMVirtioInput.c'
ALS = REPO / 'drivers/brightness/OneiOSALSDevice.cpp'
LAYOUT = HERE / 'IOHIDDevice-24A446-v54.json'
HEADER = HERE / 'OriginalHidDeviceABI.h'
CLASS = '11IOHIDDevice'
CONSTRUCTOR, DESTRUCTOR = '__ZN11IOHIDDeviceC2EPK11OSMetaClass', '__ZN11IOHIDDeviceD2Ev'
META, VTABLE = '__ZN11IOHIDDevice10gMetaClassE', '__ZTV11IOHIDDevice'
REQUIRED = ['_oneios_hid_input_start', '_droidvm_input_start', '_native_hid_touch_allocate', '_native_hid_input_allocate',
            '_oneios_als_device_start', '__GLOBAL__sub_I_NativeHidInput.cpp', '__GLOBAL__sub_I_OneiOSALSDevice.cpp',
            '_oneios_hid_parent_meta_link', CONSTRUCTOR]
# NativeHidInput's edge band, as a share of each axis: 3 mm of the iPhone18,2's panel (54 of 1320 x 2868 pixels at
# 460 per inch). The S24 Ultra's panel is the same size within 1 % (1440 x 3120 at 505 per inch: 60 pixels), so the
# share holds for the guest's native mode too.
PANEL = (1320, 2868)
EDGE_PIXELS = 54


def fail(text):
    sys.exit('build-oneios-hid: ' + text)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# ---- Mach-O objects: symbols, and the signed entries of a vtable (after build-droidvm-hid-input.py) ----

def sections(data):
    magic, cpu, _, _, count = struct.unpack_from('<5I', data)
    if magic != 0xfeedfacf or cpu != 0x100000c:
        fail('not an arm64 Mach-O object')
    found, symtab, offset = [], None, 32
    for _ in range(count):
        cmd, size = struct.unpack_from('<2I', data, offset)
        if cmd == 0x19:
            for i in range(struct.unpack_from('<I', data, offset + 64)[0]):
                at = offset + 72 + 80 * i
                va, length, fileoff = struct.unpack_from('<QQI', data, at + 32)
                reloff, nreloc = struct.unpack_from('<2I', data, at + 56)
                found.append({'va': va, 'size': length, 'fileoff': fileoff, 'reloff': reloff, 'nreloc': nreloc})
        elif cmd == 2:
            symtab = offset
        offset += size
    return found, symtab


def symbols(data):
    """[(name, value, type byte)] of an object's symbol table; type 1 is undefined external."""
    _, symtab = sections(data)
    symoff, count, stroff, _ = struct.unpack_from('<4I', data, symtab + 8)
    found = []
    for i in range(count):
        index, kind, _, _, value = struct.unpack_from('<IBBHQ', data, symoff + i * 16)
        found.append((data[stroff + index:data.index(b'\0', stroff + index)].decode(), value, kind))
    return found


def vtable(data, name):
    """The entries of vtable `name` from its first slot on, while they are authenticated pointers:
    [(symbol, discriminator, address diversity, key)]."""
    found = symbols(data)
    start = next((value for symbol, value, kind in found if symbol == name and kind & 0xe == 0xe), None)
    if start is None:
        fail(f'{name} is not defined in the object')
    entries = {}
    for section in sections(data)[0]:
        if not section['va'] <= start < section['va'] + section['size']:
            continue
        for i in range(section['nreloc']):
            offset, bits = struct.unpack_from('<2I', data, section['reloff'] + i * 8)
            # extern, 8 bytes, ARM64_RELOC_AUTHENTICATED_POINTER
            if bits >> 27 & 1 and bits >> 25 & 3 == 3 and bits >> 28 == 11:
                word = struct.unpack_from('<Q', data, section['fileoff'] + offset)[0]
                entries[section['va'] + offset] = (found[bits & 0xffffff][0], word >> 32 & 0xffff, word >> 48 & 1, word >> 49 & 3)
    table, at = [], start + 16
    while at in entries:
        table.append(entries[at])
        at += 8
    return table


# ---- The header against the kernelcache's record ----

PROBE = '''// Generated by build-oneios-hid.py: the class whose vtable is compared with the kernelcache's record.
#include "OriginalHidDeviceABI.h"
class OneiOSHidLayoutCheck : public IOHIDDevice {
	OSDeclareDefaultStructors(OneiOSHidLayoutCheck)
public:
	IOReturn newReportDescriptor(IOMemoryDescriptor **descriptor) const override;
};
OSDefineMetaClassAndStructors(OneiOSHidLayoutCheck, IOHIDDevice)
IOReturn OneiOSHidLayoutCheck::newReportDescriptor(IOMemoryDescriptor **) const { return kIOReturnUnsupported; }
'''


def check_header(layout, compile_source, out):
    """Returns {mangled IOHIDDevice function: slot} and the check class's vtable; stops when the header is not the
    record's class."""
    source = out / 'OneiOSHidLayoutCheck.cpp'
    source.write_text(PROBE)
    obj = compile_source('iokit', source, out / 'OneiOSHidLayoutCheck.o')
    table = vtable(obj.read_bytes(), '__ZTV20OneiOSHidLayoutCheck')
    slots = layout['slots']
    if int(layout['size'], 16) != 0xd0 or not layout['abstract']:
        fail(f'{LAYOUT.name}: not the abstract 0xd0-byte class the header asserts')
    if len(table) != len(slots):
        fail(f'the header gives a vtable of {len(table)} slots, the kernelcache\'s has {len(slots)}')
    owned = {0, 1, 7}                           # destructors and getMetaClass: every class's own
    pure = [s['slot'] for s in slots if s['slot'] >= 168 and not s['family']]
    slot_of = {}
    for i, ((name, diversity, address, key), slot) in enumerate(zip(table, slots)):
        if diversity != int(slot['diversity'], 16) or not address or key != 0:
            fail(f'slot {i} ({name}): signed with {diversity:#06x} (address diversity {address}, key {key}), '
                 f'the kernelcache\'s slot with {slot["diversity"]}')
        if i in owned or i in pure:
            if 'OneiOSHidLayoutCheck' not in name:
                fail(f'slot {i} ({name}): the check class\'s own function expected')
        elif (CLASS in name) != slot['family']:
            fail(f'slot {i} ({name}): the kernelcache\'s slot is {"the family\'s" if slot["family"] else "the kernel\'s"}; '
                 'the header declares an override the class does not have, or misses one')
        elif CLASS in name:
            if slot_of.setdefault(name, i) != i:
                fail(f'{name} fills two slots')
    if len(pure) != 1:
        fail(f'one abstract slot expected, the record has {pure}')
    # The comment table of the header: every "<slot> name (0x....)" (and "0, 1 name (0x...., 0x....)") is the record's.
    text = HEADER.read_text()
    listed = {}
    for match in re.finditer(r'(?<![\w.])(\d+)(?:, (\d+))? [A-Za-z][\w/()]*(?:\([\w, ]*\))? \((0x[0-9a-f]{4})(?:, (0x[0-9a-f]{4}))?\)', text):
        listed[int(match.group(1))] = int(match.group(3), 16)
        if match.group(2):
            listed[int(match.group(2))] = int(match.group(4), 16)
    family = {s['slot'] for s in slots if s['family']} | set(pure)
    if set(listed) != family:
        fail(f'the header\'s comment table lists slots {sorted(set(listed) ^ family)} differently from the record')
    wrong = [i for i, d in listed.items() if d != int(slots[i]['diversity'], 16)]
    if wrong:
        fail(f'the header\'s comment table has other discriminators than the record for slots {wrong}')
    return slot_of, table


# ---- The staged sources ----

def replace_once(text, pairs, what):
    for anchor, replacement in pairs:
        if text.count(anchor) != 1:
            fail(f'{what} no longer has exactly one `{anchor.strip()[:90]}`')
        text = text.replace(anchor, replacement)
    return text


def stage_hid(out):
    """NativeHidInput.cpp for the phone. The buttons device's report 2 gets a fifth bit, the ringer switch
    (Telephony / Ring Enable), and takes all five from the transport's button sink (the source keeps the Home bit
    for its own timed press and drops it from the sink's mask); the edge band is 3 mm of the phone's panel, not of
    the iPad's; the products are named for what they are here; the edge wrapper follows a change of event driver."""
    edge_x, edge_y = (f'{EDGE_PIXELS}*65536/{side}' for side in PANEL)
    text = replace_once(HID_SOURCE.read_text(), (
        ('    0x75,0x01,0x95,0x04,0x81,0x02,\n    0x75,0x04,0x95,0x01,0x81,0x03,0xc0\n};',
         '    0x75,0x01,0x95,0x04,0x81,0x02,\n'
         '    // OneiOS (build-oneios-hid.py): bit 4, the ringer switch (Telephony / Ring Enable), then three bits of padding.\n'
         '    0x05,0x0b,0x09,0x2d,0x95,0x01,0x81,0x02,\n'
         '    0x75,0x03,0x95,0x01,0x81,0x03,0xc0\n};'),
        ('        self->buttons=static_cast<uint8_t>(uintptr_t(mask)&0xe);\n',
         '        self->buttons=static_cast<uint8_t>(uintptr_t(mask)&0x1f);   // OneiOS: Home and the ringer switch too\n'),
        ('static const int32_t kEdgeX=48*65536/3048,kEdgeY=48*65536/2032;',
         f'static const int32_t kEdgeX={edge_x},kEdgeY={edge_y};   // OneiOS: {EDGE_PIXELS} px of {PANEL[0]} x {PANEL[1]}\n'
         '// OneiOS: where the last contact began and where the last one lifted, for the log (OneiOSHidInput.cpp).\n'
         'extern "C" {extern volatile int32_t oneios_touch_last[4];}'),
        ('        native_touch_edge_stats[1]=native_touch_edge_stats[1]+1;\n',
         '        native_touch_edge_stats[1]=native_touch_edge_stats[1]+1;\n'
         '        oneios_touch_last[0]=x;oneios_touch_last[1]=y;   // OneiOS\n'),
        ('    if(!touch && (mask&kMaskTouch))fromEdge[index]=false;',
         '    if(!touch && (mask&kMaskTouch)) {   // OneiOS: where the contact lifted, for the log\n'
         '        oneios_touch_last[2]=call<int32_t,uint32_t,IOOptionBits>(child,kGetFixed,kDivGetFixed,kX,0);\n'
         '        oneios_touch_last[3]=call<int32_t,uint32_t,IOOptionBits>(child,kGetFixed,kDivGetFixed,kY,0);\n'
         '    }\n'
         '    if(!touch && (mask&kMaskTouch))fromEdge[index]=false;'),
        ('return OSString::withCString(touchOnly?"Offline touch digitizer":"Offline HID input probe");',
         'return OSString::withCString(touchOnly?"OneiOS touch screen":"OneiOS buttons");'),
        # The event driver can change under a device: the collection tries the DriverKit driver first
        # (AppleUserHIDEventService) and, when its process does not come up, gives the device to the kernel's
        # IOHIDEventDriver two minutes later. The source looks the driver up once and wraps the first one only.
        ('    bool eventDriverFound=false;\n',
         '    bool eventDriverFound=false;\n'
         '    IORegistryEntry *wrappedDriver=nullptr;   // OneiOS: the event driver last handed to edge::install (not retained)\n'),
        ('        if(!eventDriverFound) {\n'
         '            IORegistryEntry *driver=eventDriver(this,4);\n'
         '            eventDriverFound=driver!=nullptr;\n'
         '            if(driver && (touchEnabled || touchOnly))edge::install(driver);\n'
         '        }\n',
         '        IORegistryEntry *driver=eventDriver(this,4);   // OneiOS: each time, the driver can be another one by now\n'
         '        eventDriverFound=driver!=nullptr;\n'
         '        if(driver && driver!=wrappedDriver && (touchEnabled || touchOnly)){wrappedDriver=driver;edge::install(driver);}\n'),
        ('    if(originalDispatch || !driver)return;\n    if(kSlotDiversity[kDispatch]!=kDivDispatch)return;\n',
         '    if(!driver)return;   // OneiOS: a later driver is wrapped too (its table is checked and copied as the first one\'s)\n'
         '    if(kSlotDiversity[kDispatch]!=kDivDispatch)return;\n'),
    ), 'NativeHidInput.cpp')
    staged = out / 'NativeHidInput.cpp'           # the same file name: the static initializer is named after it
    staged.write_text(text)
    return staged


def stage_transport(out):
    """DroidVMVirtioInput.c with the phone's side keys (OneiOSSideKeys.inc): xnuhost's one input device carries the
    touch screen and the keys, and the source reads keys from a keyboard only."""
    text = replace_once(TRANSPORT.read_text(), (
        ('struct droidvm_input_desc { uint64_t address;',
         '// OneiOS (build-oneios-hid.py): a side key\'s event, OneiOSSideKeys.inc at the end of this file.\n'
         'static bool oneios_side_key(unsigned code, int32_t value);\n'
         'struct droidvm_input_desc { uint64_t address;'),
        ('    if (e->type == EV_SYN) return e->code == SYN_REPORT;\n',
         '    if (e->type == EV_SYN) return e->code == SYN_REPORT;\n'
         '    if (e->type == EV_KEY && oneios_side_key(e->code, e->value)) return false;   // OneiOS: any device\'s side keys\n'),
        # A device with the side keys and no touch screen (xnuhost found no panel) is still read.
        ('        input_bit(abs, nabs, ABS_Z))\n        return KIND_MOTION;\n    return KIND_NONE;\n',
         '        input_bit(abs, nabs, ABS_Z))\n        return KIND_MOTION;\n'
         '    if (oneios_has_side_keys(keys, nkeys)) return KIND_KEYBOARD;   // OneiOS\n    return KIND_NONE;\n'),
        ('static bool input_bit(const uint8_t *bits, unsigned size, unsigned bit) {\n',
         'static bool oneios_has_side_keys(const uint8_t *keys, unsigned size);\n'
         'static bool input_bit(const uint8_t *bits, unsigned size, unsigned bit) {\n'),
    ), 'DroidVMVirtioInput.c')
    staged = out / 'DroidVMVirtioInput.c'
    staged.write_text(text + '\n' + (HERE / 'OneiOSSideKeys.inc').read_text())
    return staged


def bindings(layout, slot_of, table, imports):
    """The assembly of the original functions' stubs and of their targets, for the IOHIDDevice names the objects import."""
    slots = layout['slots']
    functions = {}
    for name in sorted(imports):
        if name == CONSTRUCTOR:
            functions[name] = int(layout['constructor']['linked'], 16)
        elif name == DESTRUCTOR:
            functions[name] = int(layout['base_destructor']['linked'], 16)
        elif name in (META, VTABLE):
            continue                         # data: the absolute metaclass, and the table below
        elif name in slot_of:
            functions[name] = int(slots[slot_of[name]]['linked'], 16)
        else:
            fail(f'{name}: not a function of the original class the header declares (its vtable, static data or a '
                 'private function cannot be bound)')
    meta = int(layout['meta_class_linked'], 16)
    lines = [f'// Generated by build-oneios-hid.py from {LAYOUT.name} (kernelcache {layout["kernelcache_sha256"][:16]}...): see there.',
             f'.globl {META}', f'.set {META}, {meta:#x}',
             '.section __DATA,__const', '.p2align 3',
             '.globl _oneios_hid_parent_meta_link', f'_oneios_hid_parent_meta_link: .quad {meta:#x}',
             '.globl _oneios_hid_parent_size', f'_oneios_hid_parent_size: .long {int(layout["size"], 16):#x}',
             '.text']
    # A table under the original vtable's name, for calls by qualified name: in a kernel (-fapple-kext) a call such as
    # IOHIDDevice::start(provider) in a subclass loads the function from the named class's vtable and authenticates
    # it as that slot. The compiler reaches the table through the GOT, and a GOT entry for an absolute symbol would
    # hold the link address (no fixup slides it), so the original table cannot be named; this one has every slot
    # signed for its place here with the original discriminator: the stub of the family's function, or the kernel's
    # own function where the class inherits it. The destructors, getMetaClass and the abstract slot are never called
    # by qualified name: zero. No object's vtable pointer ever points here.
    lines += ['.section __DATA,__const', '.p2align 3', f'.globl {VTABLE}', f'{VTABLE}:', ' .quad 0', ' .quad 0']
    for (name, diversity, _, _), slot in zip(table, slots):
        own = CLASS not in name and 'OneiOSHidLayoutCheck' in name
        lines.append(' .quad 0' if own else f' .quad {name}@AUTH(ia,{diversity:#x},addr)')
    lines.append('.text')
    # The targets are absolute symbols of a second object: in one file the assembler would resolve the branch itself.
    targets = [lines[0]]
    for name, address in functions.items():
        lines += [f'.globl {name}', '.p2align 2', f'{name}:', ' bti c', f' b {name}.original']
        targets += [f'.globl {name}.original', f'.set {name}.original, {address:#x}']
    return '\n'.join(lines) + '\n', '\n'.join(targets) + '\n', functions


def main():
    needed = (REPO / 'drivers/build.py', HID_SOURCE, TRANSPORT, ALS, LAYOUT, HEADER, HERE / 'OneiOSHidInput.cpp',
              HERE / 'OneiOSSideKeys.inc', RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json')
    for path in needed:
        if not path.is_file():
            fail(f'{path} missing (scripts/setup/get-references.sh NativeHidInput; kernel/fetch.sh)')
    if 'NATIVE_HID_SOURCE_API' not in HID_SOURCE.read_text():
        fail(f'{HID_SOURCE} is not ShengHidInput 24f1f3f or later (its arm64e source): remove external/NativeHidInput and '
             'run scripts/setup/get-references.sh NativeHidInput')
    layout = json.loads(LAYOUT.read_text())
    drivers = drivers_build()
    out = TREE / 'build/oneios-hid'
    out.mkdir(parents=True, exist_ok=True)
    includes = ['-I', str(HERE), '-I', str(HERE / 'include'), '-I', str(REPO / 'drivers/brightness')]

    def compile_source(universe, source, obj, extra=()):
        record = drivers.record(RECORDS, drivers.RECORDS[universe])
        command = drivers.command(record, source, obj) + includes + list(extra)
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-8000:])
            fail(f'{source.name} did not compile')
        return obj

    slot_of, table = check_header(layout, compile_source, out)
    print(f'build-oneios-hid: OriginalHidDeviceABI.h is the kernelcache\'s IOHIDDevice: {len(layout["slots"])} slots with its '
          f'discriminators, {len(slot_of)} functions of the family placed, 0xd0 bytes')

    hid = compile_source('iokit', stage_hid(out), out / 'NativeHidInput.o', ('-DNATIVE_HID_SOURCE_API=1',))
    start = compile_source('iokit', HERE / 'OneiOSHidInput.cpp', out / 'OneiOSHidInput.o')
    als = compile_source('iokit', ALS, out / 'OneiOSALSDevice.o')
    # Its queue and event pages come from the kernel map (DROIDVM_INPUT_DYNAMIC), as on the iPadOS 27 board.
    transport = compile_source('mach', stage_transport(out), out / 'DroidVMVirtioInput.o',
                               ('-DDROIDVM_INPUT_DYNAMIC=1', '-mgeneral-regs-only'))
    objcopy = shutil.which('llvm-objcopy') or 'objcopy'
    objects = [start, transport]
    # The classes' static initializers register them below the original class: they run inside the module load of
    # oneios_hid_input_start, not with the kernel's own initializers. Out of __mod_init_func, and global.
    for obj in (hid, als):
        late = obj.with_suffix('.late.o')
        subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func',
                        f'--globalize-symbol=__GLOBAL__sub_I_{obj.stem}.cpp', str(obj), str(late)], check=True)
        objects.append(late)

    # Every function of the family gets a stub: the table for calls by qualified name (bindings) names them all.
    imports = {name for obj in objects for name, _, kind in symbols(obj.read_bytes()) if kind == 1 and CLASS in name} | set(slot_of)
    # The metaclass is an absolute symbol: wherever it is read from (a GOT entry or the class's superClass pointer)
    # the value may be the link address. Its one use is as the superclass each static initializer passes, which
    # OneiOSHidInput.cpp corrects before the classes register; any other use (OSDynamicCast to IOHIDDevice, say)
    # would compare against an address that is wrong when the collection is slid.
    for obj in objects:
        data = obj.read_bytes()
        if any(CLASS + '9metaClassE' in name for name, _, kind in symbols(data) if kind == 1):
            fail(f'{obj.name} uses IOHIDDevice::metaClass (OSDynamicCast to the original class): not bound')
    stubs, targets, functions = bindings(layout, slot_of, table, imports)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    arguments = record['arguments']
    compiler = next(a for a in arguments if 'clang' in Path(a).name)
    triple = arguments[arguments.index('-target') + 1] if '-target' in arguments else 'arm64e-apple-ios27.0'
    for stem, text in (('IOHIDDevice-bindings', stubs), ('IOHIDDevice-originals', targets)):
        source = out / (stem + '.S')
        source.write_text(text)
        subprocess.run([compiler, '-target', triple, '-c', str(source), '-o', str(source.with_suffix('.o'))], check=True,
                       cwd=record['directory'])
        objects.append(source.with_suffix('.o'))

    # Every owned vtable keeps the original's slots: the discriminators, and the original function wherever the
    # class does not override it.
    owned = {}
    for obj in objects[2:4]:
        data = obj.read_bytes()
        for name in [n for n, _, kind in symbols(data) if n.startswith('__ZTV') and kind & 0xe == 0xe and '9MetaClassE' not in n]:
            table = vtable(data, name)
            if len(table) != len(layout['slots']):
                fail(f'{name}: {len(table)} slots, the original has {len(layout["slots"])}')
            for i, (symbol, diversity, address, key) in enumerate(table):
                if diversity != int(layout['slots'][i]['diversity'], 16) or not address or key:
                    fail(f'{name} slot {i} ({symbol}): not signed as the original slot')
                if CLASS in symbol and slot_of.get(symbol) != i:
                    fail(f'{name} slot {i} ({symbol}): not the original function of that slot')
            owned[name] = [i for i, (symbol, *_) in enumerate(table) if name[len('__ZTV'):] in symbol]

    archive = out / 'liboneioshid.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True,
                             text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    sources = [HID_SOURCE, TRANSPORT, HERE / 'OneiOSSideKeys.inc', HERE / 'OneiOSHidInput.cpp', ALS, HEADER, LAYOUT]
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in sources]}, indent=2) + '\n')
    (out / 'report.json').write_text(json.dumps({
        'kernelcache_sha256': layout['kernelcache_sha256'], 'bindings': {n: hex(a) for n, a in functions.items()},
        'owned_slots': owned}, indent=2) + '\n')
    print(f'build-oneios-hid: {archive} ({len(objects)} objects, {len(functions)} original IOHIDDevice functions bound at '
          f'their 24A446 iPhone18,2 addresses; {", ".join(f"{n[5:]} owns {len(s)} slots" for n, s in owned.items())})')


if __name__ == '__main__':
    main()
