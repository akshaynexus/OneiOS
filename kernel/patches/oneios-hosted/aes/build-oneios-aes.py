#!/usr/bin/env python3
"""The hosted kernel's software AES archive, built between the two kernel passes from the compile records of the first:

  build/oneios-aes/liboneiosaes.a   OneiOSSoftwareAES.cpp (this folder: oneios_software_aes_start), UniXNU's
                                    SoftwareAES NativeAES.cpp with Droid-VM's software hardware keys
                                    (UniXNU-droidvm package/SoftwareAESKeys.inc, staged as its devices build does),
                                    and the generated bindings of its parent class

Why: AppleImage4's start waits for an IOAESAccelerator, and with it IOResources' matching, the BSD disk nodes and
the root volume (OneiOSSoftwareAES.cpp). NativeAES subclasses the original IOAESAccelerator of the kernel
collection's com.apple.iokit.IOCryptoAcceleratorFamily.

The bindings come from IOAESAccelerator-24A446-v54.json here (kernel/prelink/classes.py's output: the class's metaclass,
size, vtable slots, constructor and destructors as read from the iPhone18,2 kernelcache; addresses only). For each
function of the parent that NativeAES's code or vtable names, a stub in kernel text:
      <name>:  bti c ; b <name>.original        with <name>.original an absolute symbol at the function's address
                                                in the collection
which XnuPrelink's emitter retargets when it places the kernel (PC-relative, so right at any slide). The metaclass
is data: an absolute symbol, plus its address and the instance size as numbers for the check at start.
Which slot is which method is OriginalAESABI.h's order (SoftwareAES), the same as in Droid-VM's
SoftwareAES-24A446.S for the iPad: IOService's 168 slots, of which the class overrides 0, 1 (destructors),
7 (getMetaClass), 86 (start), 111 (getWorkLoop) and 117 (callPlatformFunction), then its own 13. The script
refuses a class whose table does not have that shape.

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES and started from oneios_board_late_start (0008-software-aes.patch).
kernel/build-hosted.sh copies this script into the build's device repository and adds it to the board's archives.
Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository. No Apple file is read.
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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-aes: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/aes'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
SOFTWARE_AES = REPO / 'external/Droid-VM-UniXNU/drivers/SoftwareAES'
KEYS = REPO / 'external/UniXNU-droidvm/package/SoftwareAESKeys.inc'
CLASSES = HERE / 'IOAESAccelerator-24A446-v54.json'    # kernel/prelink/classes.py's record of the class (a copy of that one class)
REQUIRED = ['_oneios_software_aes_start', '__GLOBAL__sub_I_NativeAES.cpp', '__ZN9NativeAES10gMetaClassE',
            '_oneios_aes_parent_meta_link', '__ZN16IOAESAcceleratorC2EPK11OSMetaClass']
CLASS = 'IOAESAccelerator'
# The class's own code in the kernelcache, relative to its complete destructor (slot 0), as in every 24A446 build of
# the family seen (iPad17,3 and iPhone18,2): the protected constructor 0x78 before it, the base destructor 8 before.
CONSTRUCTOR_BEFORE, BASE_DESTRUCTOR_BEFORE = 0x78, 8
SLOTS = {  # vtable slot -> symbol
    0: '__ZN16IOAESAcceleratorD1Ev',
    1: '__ZN16IOAESAcceleratorD0Ev',
    7: '__ZNK16IOAESAccelerator12getMetaClassEv',
    86: '__ZN16IOAESAccelerator5startEP9IOService',
    111: '__ZNK16IOAESAccelerator11getWorkLoopEv',
    117: '__ZN16IOAESAccelerator20callPlatformFunctionEPK8OSSymbolbPvS3_S3_S3_',
    168: '__ZN16IOAESAccelerator16getAlgorithmNameEv',
    169: '__ZN16IOAESAccelerator10performAESEPvS0_P4taskyP7IOAESIV14IOAESOperationP12IOAESKeyDataPFvS0_iES0_',
    170: '__ZN16IOAESAccelerator10performAESEP18IOMemoryDescriptorS1_yP7IOAESIV14IOAESOperationP12IOAESKeyDatayyPFvPviES8_',
    171: '__ZN16IOAESAccelerator14isKeySupportedEj',
    172: '__ZN16IOAESAccelerator19getSupportedOptionsEv',
    173: '__ZN16IOAESAccelerator17performAESQuantumEP23IOAESAcceleratorCommand',
    174: '__ZN16IOAESAccelerator18completeAESQuantumEP23IOAESAcceleratorCommand',
    175: '__ZN16IOAESAccelerator19performImmediateAESEP23IOAESAcceleratorCommand',
    176: '__ZN16IOAESAccelerator18disableHardwareKeyEj',
    177: '__ZN16IOAESAccelerator17createSpecialKeysEv',
    178: '__ZN16IOAESAccelerator12_queueLockedEP23IOAESAcceleratorCommand',
    179: '__ZN16IOAESAccelerator13_startCommandEv',
    180: '__ZN16IOAESAccelerator12_completeAESEP23IOAESAcceleratorCommand',
}


def fail(text):
    sys.exit('build-oneios-aes: ' + text)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def bindings():
    """The assembly of the parent's bindings, from the class reader's record of this kernelcache."""
    record = json.loads(CLASSES.read_text())
    cls = record['classes'].get(CLASS) or fail(f'{CLASSES.name} has no {CLASS}')
    if cls.get('bundle') != 'com.apple.iokit.IOCryptoAcceleratorFamily' or int(cls['size'], 16) != 0x120 or cls['slots'] != 181:
        fail(f'{CLASS} is not the class OriginalAESABI.h describes (0x120 bytes, 181 slots, IOCryptoAcceleratorFamily)')
    slide = int(cls['meta_class_linked'], 16) - int(cls['meta_class'], 16)
    table = {entry['slot']: int(entry['original'], 16) for entry in cls['slot_table']}
    family = {slot for slot, address in table.items() if abs(address - table[0]) < 0x10000}
    if family != set(SLOTS):
        fail(f'{CLASS} overrides or adds slots {sorted(family)}, not {sorted(SLOTS)}: the method order must be reviewed')
    if table[0] != int(cls['destructor_complete'], 16) or table[1] != int(cls['destructor_deleting'], 16) or \
            table[7] != int(cls['get_meta_class'], 16):
        fail(f'{CLASS}: slots 0, 1 and 7 are not its destructors and getMetaClass')
    constructor = table[0] - CONSTRUCTOR_BEFORE
    if constructor not in {int(entry['original'], 16) for entry in cls['stores_vtable']}:
        fail(f'{CLASS}: no constructor stores the vtable at {constructor:#x}')
    functions = {'__ZN16IOAESAcceleratorC2EPK11OSMetaClass': constructor,
                 '__ZN16IOAESAcceleratorD2Ev': table[0] - BASE_DESTRUCTOR_BEFORE,
                 **{name: table[slot] for slot, name in SLOTS.items()}}
    meta = int(cls['meta_class_linked'], 16)
    lines = [f'// Generated by build-oneios-aes.py from {CLASSES.name} (kernelcache {record["kernelcache_sha256"][:16]}...): see there.',
             '.globl __ZN16IOAESAccelerator10gMetaClassE',
             f'.set __ZN16IOAESAccelerator10gMetaClassE, {meta:#x}',
             '.section __DATA,__const', '.p2align 3',
             '.globl _oneios_aes_parent_meta_link', f'_oneios_aes_parent_meta_link: .quad {meta:#x}',
             '.globl _oneios_aes_parent_size', f'_oneios_aes_parent_size: .long {int(cls["size"], 16):#x}',
             '.text']
    # The targets are absolute symbols of a second object: in one file the assembler would resolve the branch itself
    # ("fixup value out of range"); across objects it leaves a branch relocation for the linker.
    targets = [lines[0]]
    for name, original in functions.items():
        lines += [f'.globl {name}', '.p2align 2', f'{name}:', ' bti c', f' b {name}.original']
        targets += [f'.globl {name}.original', f'.set {name}.original, {original + slide:#x}']
    return '\n'.join(lines) + '\n', '\n'.join(targets) + '\n'


def staged_native_aes(out):
    """NativeAES.cpp with the software hardware keys, as Droid-VM's build-droidvm-devices.py stages it."""
    text = (SOFTWARE_AES / 'NativeAES.cpp').read_text()
    for anchor, replacement in (
            ('#include <kern/task.h>\n', '#include <kern/task.h>\n' + KEYS.read_text()),
            ('  bool isKeySupported(unsigned selector) override { return selector == 0; }\n',
             '  bool isKeySupported(unsigned selector) override { return selector == 0 || droidvmHardwareKey(selector); }\n'),
            ('  if (key->selector || (operation != IOAESEncrypt && operation != IOAESDecrypt)) {\n',
             '  IOAESKeyData droidvmKey;\n  key = droidvmKeyFor(key, droidvmKey);\n'
             '  if (key->selector || (operation != IOAESEncrypt && operation != IOAESDecrypt)) {\n')):
        if text.count(anchor) != 1:
            fail('NativeAES.cpp no longer has exactly one ' + anchor.strip())
        text = text.replace(anchor, replacement)
    staged = out / 'NativeAES.cpp'       # the same file name: the static initializer is named after it
    staged.write_text(text)
    return staged


def main():
    for path in (REPO / 'drivers/build.py', SOFTWARE_AES / 'NativeAES.cpp', SOFTWARE_AES / 'OriginalAESABI.h', KEYS,
                 RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', HERE / 'OneiOSSoftwareAES.cpp'):
        if not path.is_file():
            fail(f'{path} missing (external/Droid-VM-UniXNU: git submodule update --init drivers/SoftwareAES; '
                 'scripts/setup/get-references.sh UniXNU-droidvm)')
    if not CLASSES.is_file():
        fail(f'{CLASSES} missing: kernel/prelink/classes.py makes it from the kernelcache')
    drivers = drivers_build()
    out = TREE / 'build/oneios-aes'
    out.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    objects = []
    for source in (HERE / 'OneiOSSoftwareAES.cpp', staged_native_aes(out)):
        obj = out / (source.stem + '.o')
        command = drivers.command(record, source, obj) + ['-I', str(SOFTWARE_AES)]
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-6000:])
            fail(f'{source.name} did not compile')
        objects.append(obj)
    # NativeAES's static initializer registers its class: it must run inside the board's module load, not with the
    # kernel's own initializers. Out of __mod_init_func, and global so OneiOSSoftwareAES.cpp can call it.
    native = out / 'NativeAES.o'
    late = out / 'NativeAES.late.o'
    subprocess.run([shutil.which('llvm-objcopy') or 'objcopy', '--remove-section=__DATA,__mod_init_func',
                    '--globalize-symbol=__GLOBAL__sub_I_NativeAES.cpp', str(native), str(late)], check=True)
    objects[objects.index(native)] = late
    stubs, targets = bindings()
    arguments = record['arguments']
    compiler = next(a for a in arguments if 'clang' in Path(a).name)
    triple = arguments[arguments.index('-target') + 1] if '-target' in arguments else 'arm64e-apple-ios27.0'
    for stem, text in (('IOAESAccelerator-bindings', stubs), ('IOAESAccelerator-originals', targets)):
        source = out / (stem + '.S')
        source.write_text(text)
        subprocess.run([compiler, '-target', triple, '-c', str(source), '-o', str(source.with_suffix('.o'))], check=True,
                       cwd=record['directory'])
        objects.append(source.with_suffix('.o'))
    archive = out / 'liboneiosaes.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True, text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    sources = [HERE / 'OneiOSSoftwareAES.cpp', SOFTWARE_AES / 'NativeAES.cpp', SOFTWARE_AES / 'OriginalAESABI.h', KEYS, CLASSES]
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'universe': 'iokit', 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in sources]}, indent=2) + '\n')
    print(f'build-oneios-aes: {archive} ({len(objects)} objects, {CLASS} bound at its 24A446 iPhone18,2 addresses)')


if __name__ == '__main__':
    main()
