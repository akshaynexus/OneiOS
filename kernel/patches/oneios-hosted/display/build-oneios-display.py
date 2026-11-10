#!/usr/bin/env python3
"""The hosted kernel's display archive, built between the two kernel passes from the compile records of the first:

  build/native-legacy-display/libNativeLegacyDisplay.late.a
      NativeLegacyDisplay.cpp, NativeGOPPresenter.cpp   UniXNU's NativeMobileDisplay (external/NativeMobileDisplay),
                                                        unchanged: the owned backend of the original
                                                        IOMobileFramebufferLegacy
      LegacyDisplayProbe.cpp                            Droid-VM's board probe (UniXNU-droidvm package/), staged with
                                                        five lines added (STAGED below)
      DroidVMVirtioGpuDisplay.cpp                       Droid-VM's scanout client of the virtio GPU, unchanged; the
                                                        transport and the 3D half are in liboneiosgpu.a
      OneiOSDisplay.cpp                                 this folder: the kernel's two entries, the parent class under
                                                        the slide, the display's switches, the panel's scale
      the generated bindings of the parent class

This is Droid-VM's ipados27/scripts/build-droidvm-legacy-display.py for the iPhone18,2 kernelcache and xnuhost,
without what that script builds beside the display (key store, HID, devices, the file mailbox, the launch policy).
The class's layout comes from legacy-display-24A446-v54.json here (port-legacy-display.py). As there:

  - OriginalLegacyFramebufferABI.h is generated: the original class with its 379 slots, each declared under a name
    whose pointer-authentication discriminator is the slot's diversity, so that original callers authenticate the
    owned subclass's table entries. The compiled table is checked entry by entry (signed with IA, address diversity
    and that diversity; a slot the backend does not override names the original function or the kernel's own).
  - Every original function is reached through a stub in kernel text.
  - The parent's vtable, which qualified calls of the parent's methods go through, is defined here (assembly()).

Unlike there, a stub is `bti c; b <name>.original` with <name>.original an absolute symbol at the function's linked
address: XnuPrelink's emitter retargets the branch when it places the kernel, so it is right at xnuhost's slide (an
absolute address in a literal, as Droid-VM's stubs have, is not). The parent's metaclass is data: its link address
and the instance size go in as numbers for OneiOSDisplay.cpp, which slides them (as kernel/patches/oneios-hosted/aes).

The name and place of the archive are UniXNU's: with CONFIG_XNU_NATIVE_LEGACY_DISPLAY=y its kernel build links
build/native-legacy-display/libNativeLegacyDisplay.late.a and compares it with build-report.json. kernel/build-hosted.sh
copies this script into the build's device repository, adds it to the board's archives and sets that option.
Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository. No Apple file is read.
"""
import hashlib
import importlib.util
import json
import os
import shutil
import struct
import subprocess
import sys
from pathlib import Path

TREE = Path.cwd()
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-display: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/display'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
DISPLAY = REPO / 'external/NativeMobileDisplay'
PACKAGE = REPO / 'external/UniXNU-droidvm/package'
ADRENO = REPO / 'external/NativeAdreno'
ABI_PATH = HERE / 'legacy-display-24A446-v54.json'
OUT = TREE / 'build/native-legacy-display'
CLASS = 'IOMobileFramebufferLegacy'
META = '__ZN25IOMobileFramebufferLegacy10gMetaClassE'
REQUIRED = ['__ZTV25IOMobileFramebufferLegacy', '_native_legacy_display_probe', '_native_legacy_display_late_probe', '__GLOBAL__sub_I_NativeLegacyDisplay.cpp',
            '__ZN19NativeLegacyDisplay10gMetaClassE', '_oneios_display_parent_meta_link', '_droidvm_virtio_gpu_display_start',
            '__ZN25IOMobileFramebufferLegacyC2EPK11OSMetaClass', '_native_legacy_vsync_notify']
# The probe's lines that change. Each anchor must be there exactly once.
STAGED = (
    ('#include "DroidVMVirtioGpu.h"\n',
     '#include "DroidVMVirtioGpu.h"\n'
     'extern "C" bool oneios_display_parent_bound(void);\nextern "C" void oneios_display_parent_slide(void);\n'
     'extern "C" void oneios_display_client_seen(IOReturn,int,uint32_t,IOUserClient *);\n'
     'extern "C" void oneios_display_frame_seen(unsigned,uint64_t,uint64_t);\nextern "C" void oneios_display_swap_seen(bool,unsigned);\n'),
    # the bindings are for one kernel collection: nothing is registered under another class
    ('    native_legacy_stage=1;void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n',
     '    native_legacy_stage=1;if(!oneios_display_parent_bound()){native_legacy_display_checkpoint();return;}\n'
     '    void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n'),
    # the superclass link is the parent's link address until it is slid (OneiOSDisplay.cpp)
    ('native_legacy_display_static_init();native_gop_presenter_static_init();native_legacy_meta_status=OSMetaClass::postModLoad(handle);',
     'native_legacy_display_static_init();native_gop_presenter_static_init();oneios_display_parent_slide();'
     'native_legacy_meta_status=OSMetaClass::postModLoad(handle);'),
    # the log: who opened the framebuffer, the first swaps, the first frames
    ('    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n',
     '    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n'
     '    oneios_display_client_seen(status,native_legacy_client_pid,native_legacy_client_type,client);\n'),
    ('    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n',
     '    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n    oneios_display_frame_seen(kind,maxx,maxy);\n'),
    ('    if(accepted)native_legacy_swap_stage(0);\n',
     '    if(accepted)native_legacy_swap_stage(0);\n    oneios_display_swap_seen(accepted,reason);\n'),
)
# The probe's entries are OneiOSDisplay.cpp's to wrap, and the probes of other archives it starts are not ours.
PROBE_DEFINES = ['-DNATIVE_LEGACY_PLAIN_NUB=1', '-DPE_parse_boot_argn=oneios_display_boot_arg',
                 '-Dnative_legacy_display_probe=oneios_native_legacy_display_probe',
                 '-Dnative_legacy_display_late_probe=oneios_native_legacy_display_late_probe',
                 *(f'-D{name}=oneios_display_no_probe' for name in
                   ('native_keystore_probe', 'sheng_cpufreq_probe', 'sheng_gpu_power_probe', 'native_touch_probe', 'native_hid_input_probe'))]
# Prototypes of the original slots the owned backend overrides or calls; every other slot is an opaque dispatch target.
PROTOTYPES = {
    'isMainDisplay': 'bool isMainDisplay()', 'getDotPitch': 'IOReturn getDotPitch(uint32_t *)',
    'supportedFrameInfo': 'IOReturn supportedFrameInfo(uint32_t,uint32_t *,char *,uint32_t *)=0',
    'getDisplayDimensions': 'void getDisplayDimensions(uint32_t *,uint32_t *)', 'startHardware': 'bool startHardware()',
    'kickSwapQueue': 'void kickSwapQueue()', 'completeCurrentSwap': 'void completeCurrentSwap()',
    'validateSwap': 'bool validateSwap(const OriginalLegacySwapRequest *)',
    'getRefreshAttributes': 'bool getRefreshAttributes(uint64_t *,bool)',
    'collectCompletionReports': 'IOReturn collectCompletionReports(uint32_t,void *,uint32_t *,uint32_t)=0',
    'applyStateRequest': 'int applyStateRequest(OriginalLegacyStateRequest *,bool)',
    'cancelSwapsAfterTimeout': 'bool cancelSwapsAfterTimeout()',
    'create_default_fb_surface': 'void create_default_fb_surface()'}


def fail(text):
    sys.exit('build-oneios-display: ' + text)


def require(value, why):
    if not value:
        fail(why)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def discriminator(name):
    """clang ptrauth_string_discriminator: SipHash-2-4 with the fixed key, modulo 0xffff, plus one."""
    mask = (1 << 64) - 1

    def rotl(x, b):
        return ((x << b) | (x >> (64 - b))) & mask
    key = bytes([0xb5, 0xd4, 0xc9, 0xeb, 0x79, 0x10, 0x4a, 0x79, 0x6f, 0xec, 0x8b, 0x1b, 0x42, 0x87, 0x81, 0xd4])
    k0, k1 = struct.unpack('<2Q', key)
    v = [0x736f6d6570736575 ^ k0, 0x646f72616e646f6d ^ k1, 0x6c7967656e657261 ^ k0, 0x7465646279746573 ^ k1]

    def rounds(n):
        for _ in range(n):
            v[0] = (v[0] + v[1]) & mask; v[1] = rotl(v[1], 13) ^ v[0]; v[0] = rotl(v[0], 32)
            v[2] = (v[2] + v[3]) & mask; v[3] = rotl(v[3], 16) ^ v[2]
            v[0] = (v[0] + v[3]) & mask; v[3] = rotl(v[3], 21) ^ v[0]
            v[2] = (v[2] + v[1]) & mask; v[1] = rotl(v[1], 17) ^ v[2]; v[2] = rotl(v[2], 32)
    data = name.encode()
    full = len(data) & ~7
    for i in range(0, full, 8):
        m = struct.unpack_from('<Q', data, i)[0]
        v[3] ^= m; rounds(2); v[0] ^= m
    b = len(data) << 56 & mask
    for i, c in enumerate(data[full:]):
        b |= c << (8 * i)
    v[3] ^= b; rounds(2); v[0] ^= b; v[2] ^= 0xff; rounds(4)
    return (v[0] ^ v[1] ^ v[2] ^ v[3]) % 0xffff + 1


def header(abi, slots):
    """OriginalLegacyFramebufferABI.h, as Droid-VM's build-droidvm-legacy-display.py writes it."""
    D, R = abi['display'], abi['request']
    by_slot = {slot: name for name, slot in abi['methods'].items()}
    pure = {i for i, (t, _) in enumerate(slots) if t == slots[214][0]}
    renames = []
    for base, (name, suffix) in abi['names'].items():
        mangled = f'_ZN25{CLASS}{len(name)}{name}{suffix}'
        slot = abi['methods'].get(base) or int(base[len('opaque'):])
        require(discriminator(mangled) == slots[slot][1], f'the reviewed name {name} has not the diversity of slot {slot}')
        renames.append(f'#define {base} {name}\n')
    leaves = ' '.join(f'LEAF({i})' for i in abi['leaves'])
    text = f'''// SPDX-License-Identifier: BSD-2-Clause-Patent
// Opaque ABI storage for the original iOS 27 (24A446, iPhone18,2) class, generated by build-oneios-display.py from
// {ABI_PATH.name} the way Droid-VM's build-droidvm-legacy-display.py does. Each original slot is declared under a name
// whose pointer-authentication discriminator is the original slot's diversity (the names below), so original callers
// authenticate the owned subclass's entries. Only qualified base prototypes may be called from source.
#pragma once
#include <IOKit/IOService.h>
#include <libkern/libkern.h>
extern "C" OSObject *native_legacy_default_surface_create(OSObject *,uint32_t,uint32_t);
struct OriginalLegacySwapRequest;
struct OriginalLegacyStateRequest {{uint32_t skipRequested,requested,skipPowered,powered;uint8_t changed;uint8_t pad[11];}};
static constexpr unsigned kLegacyVsyncClients={D['vsync_clients']};
static constexpr unsigned kRequestRect0={R['rect0']},kRequestRect1={R['rect1']},kRequestMasks={R['masks']},kRequestOptions={R['options']},kRequestDims={R['dims']},kRequestSurfaces={R['surfaces']};
#define NATIVE_LEGACY_LEAVES(LEAF) {leaves}
#define NATIVE_LEGACY_DEFAULT_SURFACE 1
#define NATIVE_LEGACY_CANCEL_SWAPS 1
static constexpr unsigned kLegacyDefaultSurface={D['default_surface']};
{''.join(renames)}class {CLASS} : public IOService {{
 OSDeclareAbstractStructors({CLASS})
 unsigned char originalPrivate[{abi['instance_size']}-sizeof(IOService)];
public:
 bool installDefaultSurface(uint32_t width,uint32_t height) {{
  auto self=reinterpret_cast<unsigned char *>(this);
  OSObject *root=nullptr,*surface=nullptr;
  memcpy(&surface,self+kLegacyDefaultSurface,sizeof(surface));
  if(surface)return true;
  memcpy(&root,self+{D['surface_root']},sizeof(root));
  if(!root)return false;
  surface=native_legacy_default_surface_create(root,width,height);
  if(!surface)return false;
  // Transfer the factory reference to the family; its stop path releases this field.
  memcpy(self+kLegacyDefaultSurface,&surface,sizeof(surface));
  return true;
 }}
 void installBootDimensions(uint32_t width,uint32_t height) {{
  memcpy(reinterpret_cast<unsigned char *>(this)+{D['width']},&width,4);memcpy(reinterpret_cast<unsigned char *>(this)+{D['height']},&height,4);
 }}
 const OriginalLegacySwapRequest *pendingSwapRequest() const {{
  const OriginalLegacySwapRequest *request=nullptr;
  memcpy(&request,reinterpret_cast<const unsigned char *>(this)+{D['head']},sizeof(request));return request;
 }}
 // Tail queue: head {D['head']}, tail pointer {D['tail']}, each request's next {R['next']} and previous-link {R['prev']}; the head moves
 // to the current-swap slot {D['current']} for the original completion body.
 bool activateOnlyPendingSwap(const OriginalLegacySwapRequest *request) {{
  auto self=reinterpret_cast<unsigned char *>(this);
  auto bytes=reinterpret_cast<const unsigned char *>(request);
  const void *current=nullptr,*head=nullptr,*tail=nullptr,*next=nullptr,*previous=nullptr;
  memcpy(&current,self+{D['current']},8);memcpy(&head,self+{D['head']},8);memcpy(&tail,self+{D['tail']},8);
  if(!request || current || head!=request)return false;
  memcpy(&next,bytes+{R['next']},8);memcpy(&previous,bytes+{R['prev']},8);
  if(previous!=self+{D['head']})return false;
  if(next) {{
   const void *link=self+{D['head']};
   memcpy(const_cast<unsigned char *>(reinterpret_cast<const unsigned char *>(next))+{R['prev']},&link,8);
  }} else if(tail!=bytes+{R['next']})return false;
  memcpy(self+{D['current']},&request,8);
  if(next) {{memcpy(self+{D['head']},&next,8);}}
  else {{const void *empty=nullptr,*emptyTail=self+{D['head']};memcpy(self+{D['head']},&empty,8);memcpy(self+{D['tail']},&emptyTail,8);}}
  return true;
 }}
 void free() override;
 bool init(OSDictionary *properties=nullptr) override;
 IOReturn setProperties(OSObject *) override;
 void systemWillShutdown(IOOptionBits) override;
 IOService *probe(IOService *,SInt32 *) override;
 bool start(IOService *) override;
 void stop(IOService *) override;
 IOWorkLoop *getWorkLoop() const override;
 IOReturn newUserClient(task_t,void *,UInt32,IOUserClient **) override;
 IOReturn setPowerState(unsigned long,IOService *) override;
'''
    for i in range(168, len(slots)):
        if i in by_slot:
            text += f' virtual {PROTOTYPES[by_slot[i]]};\n'
        elif i in pure:
            text += f' virtual __attribute__((noreturn)) void opaque{i}()=0;\n'
        else:
            text += f' virtual __attribute__((deprecated("Unclassified ABI: never call from source"))) void opaque{i}();\n'
    text += f'}};\nstatic_assert(sizeof({CLASS})=={abi["instance_size"]},"Actual original instance extent");\n'
    return text, pure


def macho(data):
    """Load commands, segments and sections of an arm64e relocatable object."""
    magic, cpu, _, _, count = struct.unpack_from('<5I', data)
    require(magic == 0xfeedfacf and cpu == 0x100000c, 'NativeLegacyDisplay.o is not an arm64 Mach-O object')
    commands, segments, offset = [], [], 32
    for _ in range(count):
        command, size = struct.unpack_from('<2I', data, offset)
        commands.append((command, offset, size))
        if command == 0x19:
            nsects = struct.unpack_from('<I', data, offset + 64)[0]
            sections = []
            for i in range(nsects):
                at = offset + 72 + 80 * i
                va, length, fileoff = struct.unpack_from('<QQI', data, at + 32)
                sections.append({'va': va, 'size': length, 'fileoff': fileoff, 'command_offset': at})
            segments.append({'sections': sections})
        offset += size
    return {'commands': commands, 'segments': segments}


def bind(obj, abi, slots, pure):
    """Checks the owned class's compiled vtable against the original slots; returns name -> linked address."""
    slide = int(abi['collection_slide'], 16)
    raw = obj.read_bytes()
    image = macho(raw)
    st = next(p for c, p, _ in image['commands'] if c == 2)
    sy, count, strings, _ = struct.unpack_from('<4I', raw, st + 8)
    symbols = []
    for i in range(count):
        ix, kind, _, _, va = struct.unpack_from('<IBBHQ', raw, sy + i * 16)
        symbols.append((raw[strings + ix:raw.index(b'\0', strings + ix)].decode(), va, kind))
    vtable = next(va for name, va, _ in symbols if name == '__ZTV19NativeLegacyDisplay')
    entries = {}
    for segment in image['segments']:
        for sec in segment['sections']:
            if sec['va'] <= vtable < sec['va'] + sec['size']:
                ro, rc = struct.unpack_from('<2I', raw, sec['command_offset'] + 56)
                for i in range(rc):
                    offset, bits = struct.unpack_from('<2I', raw, ro + i * 8)
                    # extern, 8 bytes, ARM64_RELOC_AUTHENTICATED_POINTER
                    if bits >> 27 & 1 and bits >> 25 & 3 == 3 and bits >> 28 == 11:
                        word = struct.unpack_from('<Q', raw, sec['fileoff'] + offset)[0]
                        entries[sec['va'] + offset] = (symbols[bits & 0xffffff][0], word)
    compiled = [entries.get(vtable + 16 + i * 8) for i in range(len(slots))]
    require(all(compiled) and vtable + 16 + len(slots) * 8 not in entries, f'the owned vtable has not exactly {len(slots)} entries')
    for i, (name, word) in enumerate(compiled):
        require(word >> 63 and word >> 48 & 1 and word >> 49 & 3 == 0 and (word >> 32) & 0xffff == slots[i][1],
                f'slot {i} ({name}) is not signed with IA, address diversity and the original diversity {slots[i][1]:#x}')
    family = range(int(abi['family_text'][0], 16), int(abi['family_text'][1], 16))
    bindings = {name: int(va, 16) + slide for name, va in abi['bindings'].items()}
    owned, native = [], []
    for i, (name, _) in enumerate(compiled):
        if name.startswith(('__ZN19NativeLegacyDisplay', '__ZNK19NativeLegacyDisplay')):
            owned.append(i)
        elif '25' + CLASS in name:
            require(i not in pure, f'slot {i} is abstract in the original and the backend does not implement it')
            require(bindings.get(name, slots[i][0] + slide) == slots[i][0] + slide, f'{name} names two original addresses')
            bindings[name] = slots[i][0] + slide
        else:
            require(i < 168 and slots[i][0] not in family, f'slot {i} ({name}) is the kernel\'s here and the family\'s in the original')
            native.append(i)
    require(pure <= set(owned), 'an original abstract slot is not implemented by the owned backend')
    undefined = {n for n, _, k in symbols if k == 1 and CLASS in n}
    require(undefined <= set(bindings), 'original-class imports without a binding: ' + ', '.join(sorted(undefined - set(bindings))))
    return bindings, owned, native, [name for name, _ in compiled]


def assembly(abi, bindings, slots, pure, compiled):
    """The bindings: stubs to the original functions, the parent's metaclass as numbers, and the parent's vtable.

    The vtable is ours, not the original's. The kernel is compiled as a kernel extension is (-fapple-kext): a
    qualified call of a parent's virtual method (IOMobileFramebufferLegacy::start(provider)) is made through the
    parent's vtable, loaded by address. The original table's address would be an absolute symbol, whose value is
    the link address and is wrong under xnuhost's slide; and its entries are signed for their own addresses. So the
    archive defines the symbol itself: the same 379 slots, each a pointer the linker signs for this table (IA, the
    slot's address, the slot's diversity) to a stub that branches to the original function, or to the kernel's own
    method where the family inherits it. The original objects never see this table: their constructor stores the
    original one, and the owned class's own table follows it.
    """
    slide = int(abi['collection_slide'], 16)
    family = range(int(abi['family_text'][0], 16), int(abi['family_text'][1], 16))
    meta = bindings.pop(META)
    bindings.pop('__ZTV25' + CLASS, None)
    bindings = dict(bindings)
    table = ['.section __DATA,__const', '.p2align 3', f'.globl __ZTV25{CLASS}', f'__ZTV25{CLASS}:', ' .quad 0', ' .quad 0']
    for i, (target, diversity) in enumerate(slots):
        if i in pure:
            name = '___cxa_pure_virtual'
        elif target in family:
            name = f'_oneios_display_original_slot{i}'
            bindings[name] = target + slide
        elif '19NativeLegacyDisplay' in compiled[i]:
            name = '___cxa_pure_virtual'          # the kernel's method, overridden by the backend: never called through the parent
        else:
            name = compiled[i]                    # the kernel's own (IOService's), as the family inherits it
        table.append(f' .quad {name}@AUTH(ia,{diversity:#x},addr)')
    lines = [f'// Generated by build-oneios-display.py from {ABI_PATH.name} (kernelcache {abi["kernelcache_sha256"][:16]}...): see there.',
             f'.globl {META}', f'.set {META}, {meta:#x}',
             '.section __DATA,__const', '.p2align 3',
             '.globl _oneios_display_parent_meta_link', f'_oneios_display_parent_meta_link: .quad {meta:#x}',
             '.globl _oneios_display_parent_size', f'_oneios_display_parent_size: .long {int(abi["instance_size"], 16):#x}',
             *table, '.text']
    # The targets are absolute symbols of a second object: in one file the assembler would resolve the branch itself;
    # across objects it leaves a branch relocation, which XnuPrelink's emitter retargets.
    targets = [lines[0]]
    for name, address in sorted(bindings.items()):
        lines += [f'.globl {name}', '.p2align 2', f'{name}:', ' bti c', f' b {name}.original']
        targets += [f'.globl {name}.original', f'.set {name}.original, {address:#x}']
    return '\n'.join(lines) + '\n', '\n'.join(targets) + '\n'


def staged_probe():
    text = (PACKAGE / 'LegacyDisplayProbe.cpp').read_text()
    for anchor, replacement in STAGED:
        if text.count(anchor) != 1:
            fail('LegacyDisplayProbe.cpp no longer has exactly one ' + anchor.strip())
        text = text.replace(anchor, replacement)
    staged = OUT / 'LegacyDisplayProbe.cpp'
    staged.write_text(text)
    return staged


def main():
    sources = [DISPLAY / 'NativeLegacyDisplay.cpp', DISPLAY / 'NativeLegacyRequest.h', DISPLAY / 'NativeGOPPresenter.cpp',
               DISPLAY / 'NativeGOPWire.h', DISPLAY / 'NativeHalfCodes.h', PACKAGE / 'LegacyDisplayProbe.cpp',
               PACKAGE / 'DroidVMVirtioGpuDisplay.cpp', PACKAGE / 'DroidVMVirtioGpu.h', HERE / 'OneiOSDisplay.cpp', ABI_PATH]
    for path in (REPO / 'drivers/build.py', RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', *sources):
        if not path.is_file():
            fail(f'{path} missing (scripts/setup/get-references.sh NativeMobileDisplay UniXNU-droidvm; '
                 f'{ABI_PATH.name} is made by port-legacy-display.py)')
    abi = json.loads(ABI_PATH.read_text())
    slots = [(int(t, 16), int(d, 16)) for t, d in abi['slots']]
    drivers = drivers_build()
    OUT.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    text, pure = header(abi, slots)
    (OUT / 'OriginalLegacyFramebufferABI.h').write_text(text)
    arguments = record['arguments']
    compiler = next(a for a in arguments if 'clang' in Path(a).name)
    triple = arguments[arguments.index('-target') + 1] if '-target' in arguments else 'arm64e-apple-ios27.0'
    objcopy = shutil.which('llvm-objcopy') or 'objcopy'

    def compile_cpp(source, extra):
        obj = OUT / (source.stem + '.o')
        command = drivers.command(record, source, obj) + ['-I', str(TREE / '.cache/xnu-work/osfmk'), *extra]
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-8000:])
            fail(f'{source.name} did not compile')
        return obj

    def late(obj):
        """A class's static initializer registers it: it must run inside the probe's module load, not with the kernel's own."""
        out = obj.with_suffix('.late.o')
        subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func', f'--globalize-symbol=__GLOBAL__sub_I_{obj.stem}.cpp',
                        str(obj), str(out)], check=True)
        return out

    switches = '-DPE_parse_boot_argn=oneios_display_boot_arg'
    native = compile_cpp(DISPLAY / 'NativeLegacyDisplay.cpp', ['-I', str(OUT), '-I', str(DISPLAY), switches])
    bindings, owned, kernel_slots, compiled = bind(native, abi, slots, pure)
    bound = dict(bindings)
    objects = [late(native), late(compile_cpp(DISPLAY / 'NativeGOPPresenter.cpp', ['-I', str(DISPLAY)])),
               compile_cpp(staged_probe(), ['-I', str(PACKAGE), *PROBE_DEFINES]),
               # DYNAMIC as the transport in liboneiosgpu.a: its scratch entries are allocated when the display starts.
               compile_cpp(PACKAGE / 'DroidVMVirtioGpuDisplay.cpp', ['-I', str(PACKAGE), '-I', str(ADRENO), '-DDROIDVM_VIRTIO_GPU_DYNAMIC=1']),
               compile_cpp(HERE / 'OneiOSDisplay.cpp', [])]
    stubs, targets = assembly(abi, bindings, slots, pure, compiled)
    for stem, source_text in ((CLASS + '-bindings', stubs), (CLASS + '-originals', targets)):
        source = OUT / (stem + '.S')
        source.write_text(source_text)
        subprocess.run([compiler, '-target', triple, '-c', str(source), '-o', str(source.with_suffix('.o'))], check=True, cwd=record['directory'])
        objects.append(source.with_suffix('.o'))
    archive = OUT / 'libNativeLegacyDisplay.late.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True, text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()   # noqa: E731
    # UniXNU's kernel build reads this: the archive's hash, and sources by a path under the tree (ours are not there).
    (OUT / 'build-report.json').write_text(json.dumps({
        'archive_sha256': digest(archive), 'source_sha256': {}, 'kernelcache_sha256': abi['kernelcache_sha256'],
        'oneios_sources': {str(p.relative_to(REPO)): digest(p) for p in sources},
        'bindings': {name: f'{address:#x}' for name, address in sorted(bound.items())}, 'virtual_entries': len(slots),
        'owned_entries': owned, 'inherited_native_entries': kernel_slots, 'original_instructions_modified': False}, indent=2) + '\n')
    (OUT / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'universe': 'iokit', 'sha256': digest(p)} for p in sources]}, indent=2) + '\n')
    print(f'build-oneios-display: {archive} ({len(objects)} objects; {CLASS}: {len(owned)} owned slots, {len(kernel_slots)} of the kernel, '
          f'{len(bound)} original bindings at their 24A446 iPhone18,2 addresses)')


if __name__ == '__main__':
    main()
