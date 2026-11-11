#!/usr/bin/env python3
"""The hosted kernel's GPU archive, built between the two kernel passes from the compile records of the first:

  build/oneios-gpu/liboneiosgpu.a    OneiOSGpu.cpp (this folder: oneios_gpu3d_start) and Droid-VM's virtio-gpu 3D
                                     kernel driver, unchanged: UniXNU-droidvm package/DroidVMVirtioGpu.cpp and
                                     DroidVMVirtioGpu3D.cpp, with NativeAdreno's DroidVMVirtgpuABI.h

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES and started from oneios_board_late_start (0005-gpu-3d.patch).
kernel/build-hosted.sh copies this script into the build's device repository and adds it to the board's archives;
the VM kernel (build-linux.sh) does not build it.

Metal's kernel side is in the same archive: the NativeAdreno pieces Droid-VM builds beside the driver (the user
client AGXDeviceUserClient and the IOKit surface, execbuf, memory, the PM4 encoder, and package/DroidVMGpu.cpp which
publishes them), built in build/oneios-gpu/adreno/ by Droid-VM's own ipados27/scripts/build-droidvm-gpu.py run up to
its accelerator step (so with its edits to NativeAdrenoPublish.cpp and NativeAdrenoMemory.cpp), and then what that
step does, for this kernel collection:

  NativeAdrenoAccelerator.late.o   NativeAdreno's staged AGXAccelerator, a subclass of the collection's
                                   IOAcceleratorES (com.apple.iokit.IOGPUFamily); its static initializer is taken
                                   out of __mod_init_func and made global, DroidVMGpu.cpp runs it after the family
                                   has loaded
  IOAcceleratorES-bindings.o       stubs for the parent's protected constructor and base destructor
  IOAcceleratorES-originals.o        `bti c; b <name>.original`, <name>.original absolute at the function's address
                                     in the collection: XnuPrelink's emitter retargets the branch (as ../aes does;
                                     Droid-VM's literal `ldr x16; br x16` is wrong under xnuhost's slide); and the
                                     parent's metaclass as an absolute symbol, with its link address and size as
                                     numbers for OneiOSAdreno.cpp's check and slide
  OneiOSAdreno.o                   this folder: the start, the bound-parent check, the superclass slide, the self-test

The parent's addresses are IOAcceleratorES-24A446-v54.json here (kernel/prelink/classes.py's record of the class in
the iPhone18,2 kernelcache; addresses only). The class must have the shape NativeMetal's OriginalAcceleratorABI.h
declares, which is the iPad's (Droid-VM's scripts/data/ioaccelerator-base-abi-24A446.json): 136 bytes, IOService's
168 slots, only 0, 1 (destructors) and 7 (getMetaClass) its own; the protected constructor 0x78 before the complete
destructor and the base destructor 8 before it (read from the code: both constructors call IOService's with the
caller's metaclass and store the vtable; the base destructor is `bti c; b IOService::~IOService`).
DroidVMGpu.cpp is compiled from a staged copy with four changes (STAGED below): the start condition is
OneiOSAdreno.cpp's, the publish runs on the caller's thread, the bound parent is checked, and the superclass link is
slid before postModLoad.

ONEIOS_GPU_ADRENO=0 in the environment leaves all of this out of the archive (compiled only, as a check);
adreno/status.json says what was compiled and linked.

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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-gpu: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/gpu'
RECORDS = TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837'
PACKAGE = REPO / 'external/UniXNU-droidvm/package'
ADRENO = REPO / 'external/NativeAdreno'
SOURCES = [HERE / 'OneiOSGpu.cpp', PACKAGE / 'DroidVMVirtioGpu.cpp', PACKAGE / 'DroidVMVirtioGpu3D.cpp']
REQUIRED = ['_oneios_gpu3d_start', '_droidvm_virtgpu_call', '_droidvm_virtgpu_close', '__ZN16DroidVMVirtioGpu5startEv']
REQUIRED_ADRENO = ['_oneios_adreno_start', '_sheng_gpu_power_probe', '_native_adreno_publish', '_native_adreno_new_user_client',
                   '_native_adreno_accelerator_allocate', '__GLOBAL__sub_I_NativeAdrenoAccelerator.cpp', '_droidvm_gpu_window_base',
                   '_AdrenoPM4Type7', '__ZN15IOAcceleratorESC2EPK11OSMetaClass', '__ZN15IOAcceleratorESD2Ev',
                   '_oneios_adreno_parent_meta_link', '__ZN14AGXAccelerator10gMetaClassE']
CLASSES = HERE / 'IOAcceleratorES-24A446-v54.json'     # kernel/prelink/classes.py's record of the class (a copy of that one class)
CLASS = 'IOAcceleratorES'
ACCELERATOR_ABI = REPO / 'external/NativeMetal/OriginalAcceleratorABI.h'
CONSTRUCTOR_BEFORE, BASE_DESTRUCTOR_BEFORE = 0x78, 8
# DroidVMGpu.cpp's lines that change. Each anchor must be there exactly once.
STAGED = (
    ('#include "NativeAdrenoMemory.h"\n',
     '#include "NativeAdrenoMemory.h"\n'
     'extern "C" bool oneios_adreno_wanted(void);\nextern "C" bool oneios_adreno_parent_bound(void);\n'
     'extern "C" void oneios_adreno_parent_slide(void);\n'),
    # the start is OneiOSAdreno.cpp's decision (the host offers 3D, or oneios-gpu-metal), not two boot arguments
    ('    if (!bootArg("vgpu-3d", 0) || bootArg("droidvm-gpu-metal", 0) != 1) return;\n',
     '    if (!oneios_adreno_wanted()) return;\n'),
    # the bindings are for one kernel collection: nothing is registered under another class
    ('    if (!parent || parent->getClassSize() != 136) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotFound); return;}\n',
     '    if (!parent || parent->getClassSize() != 136) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotFound); return;}\n'
     '    if (!oneios_adreno_parent_bound()) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotPermitted); return;}\n'),
    # On the caller's thread (bsd_init's, in the board's late start), not on one of its own: registering a class holds
    # OSMetaClass's lock from preModLoad to postModLoad, and postModLoad takes the kext lock. A kext load on another
    # thread holds the kext lock and waits for the class lock: with the display's archive loading its graphics
    # family right after this start, the two threads stopped each other (seen: the boot hung at accelerator stage 2).
    ('    thread_t thread = nullptr;\n    if (kernel_thread_start(publishThread, nullptr, &thread) == KERN_SUCCESS) thread_deallocate(thread);\n',
     '    publishThread(nullptr, THREAD_AWAKENED);\n'),
    # the superclass link is the parent's link address until it is slid (OneiOSAdreno.cpp)
    ('    native_adreno_accelerator_static_init();\n    status = OSMetaClass::postModLoad(handle);\n',
     '    native_adreno_accelerator_static_init();\n    oneios_adreno_parent_slide();\n    status = OSMetaClass::postModLoad(handle);\n'),
    # Without droidvm-gpu-clients the grant names the image's Metal clients too, not backboardd alone: the list roottweak
    # writes to NativeAdrenoMetal.clients. iconservicesagent was refused on the phone and drew the icons on the CPU.
    # droidvm-gpu-clients= (empty) is backboardd alone; a list replaces this one; "*" is every process.
    ('    if (PE_parse_boot_argn("droidvm-gpu-clients", clients, sizeof clients - 1)) {\n',
     '    if (!PE_parse_boot_argn("droidvm-gpu-clients", clients, sizeof clients - 1))\n'
     '        strlcpy(clients, "WidgetRenderer_Default,*Poster*,iconservicesagent,SBRendererService", sizeof clients);\n'
     '    {\n'),
)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def compile_one(drivers, record, source, obj, extra):
    command = drivers.command(record, source, obj) + extra
    result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
    obj.with_suffix('.log').write_text(result.stderr)
    return result


def fail(text):
    sys.exit('build-oneios-gpu: ' + text)


def bindings():
    """The assembly of the parent's bindings, from the class reader's record of this kernelcache."""
    record = json.loads(CLASSES.read_text())
    cls = record['classes'].get(CLASS) or fail(f'{CLASSES.name} has no {CLASS}')
    if cls.get('bundle') != 'com.apple.iokit.IOGPUFamily' or int(cls['size'], 16) != 136 or cls['slots'] != 168:
        fail(f'{CLASS} is not the class OriginalAcceleratorABI.h describes (136 bytes, 168 slots, IOGPUFamily)')
    slide = int(cls['meta_class_linked'], 16) - int(cls['meta_class'], 16)
    table = {entry['slot']: int(entry['original'], 16) for entry in cls['slot_table']}
    own = {slot for slot, address in table.items() if abs(address - table[0]) < 0x10000}
    if own != {0, 1, 7}:
        fail(f'{CLASS} has slots of its own besides the destructors and getMetaClass ({sorted(own)}): it is not a plain IOService')
    if table[0] != int(cls['destructor_complete'], 16) or table[1] != int(cls['destructor_deleting'], 16) or \
            table[7] != int(cls['get_meta_class'], 16):
        fail(f'{CLASS}: slots 0, 1 and 7 are not its destructors and getMetaClass')
    constructor = table[0] - CONSTRUCTOR_BEFORE
    if constructor not in {int(entry['original'], 16) for entry in cls['stores_vtable']}:
        fail(f'{CLASS}: no constructor stores the vtable at {constructor:#x}')
    functions = {'__ZN15IOAcceleratorESC2EPK11OSMetaClass': constructor,
                 '__ZN15IOAcceleratorESD2Ev': table[0] - BASE_DESTRUCTOR_BEFORE}
    meta = int(cls['meta_class_linked'], 16)
    lines = [f'// Generated by build-oneios-gpu.py from {CLASSES.name} (kernelcache {record["kernelcache_sha256"][:16]}...): see there.',
             '.globl __ZN15IOAcceleratorES10gMetaClassE',
             f'.set __ZN15IOAcceleratorES10gMetaClassE, {meta:#x}',
             '.section __DATA,__const', '.p2align 3',
             '.globl _oneios_adreno_parent_meta_link', f'_oneios_adreno_parent_meta_link: .quad {meta:#x}',
             '.globl _oneios_adreno_parent_size', f'_oneios_adreno_parent_size: .long {int(cls["size"], 16):#x}',
             '.text']
    # The targets are absolute symbols of a second object: in one file the assembler would resolve the branch itself.
    targets = [lines[0]]
    for name, original in functions.items():
        lines += [f'.globl {name}', '.p2align 2', f'{name}:', ' bti c', f' b {name}.original']
        targets += [f'.globl {name}.original', f'.set {name}.original, {original + slide:#x}']
    return '\n'.join(lines) + '\n', '\n'.join(targets) + '\n', set(functions) | {'__ZN15IOAcceleratorES10gMetaClassE'}


def accelerator(out, drivers, record):
    """NativeAdreno's staged AGXAccelerator against the collection's IOAcceleratorES, and OneiOSAdreno.cpp."""
    stage = out / 'accelerator'
    shutil.rmtree(stage, ignore_errors=True)
    (stage / 'Adreno740').mkdir(parents=True)
    (stage / 'AdrenoMetal').mkdir()
    # The source includes "../AdrenoMetal/OriginalAcceleratorABI.h": NativeMetal's header, beside the driver as in UniXNU's tree.
    shutil.copyfile(ACCELERATOR_ABI, stage / 'AdrenoMetal/OriginalAcceleratorABI.h')
    source = stage / 'Adreno740/NativeAdrenoAccelerator.cpp'     # the same file name: the static initializer is named after it
    shutil.copyfile(ADRENO / 'NativeAdrenoAccelerator.cpp', source)
    nm, objcopy = shutil.which('llvm-nm') or 'nm', shutil.which('llvm-objcopy') or 'objcopy'
    objects = []
    for path, extra in ((source, ['-Wno-overloaded-virtual']), (HERE / 'OneiOSAdreno.cpp', [])):
        obj = stage / (path.stem + '.o')
        result = compile_one(drivers, record, path, obj, extra + ['-I', str(ADRENO), '-I', str(TREE / '.cache/xnu-work/osfmk')])
        if result.returncode:
            sys.stderr.write(result.stderr[-6000:])
            fail(f'{path.name} did not compile')
        objects.append(obj)
    stubs, targets, bound = bindings()
    undefined = {line.split()[-1] for line in subprocess.run([nm, '-u', str(objects[0])], capture_output=True, text=True, check=True).stdout.splitlines()
                 if 'IOAcceleratorES' in line}
    if undefined - bound or not undefined & bound:
        fail(f'NativeAdrenoAccelerator.cpp imports {sorted(undefined - bound)} of the original base, which are not bound')
    # The static initializer registers the class: it runs when DroidVMGpu.cpp calls it, after IOGPUFamily has loaded.
    late = stage / 'NativeAdrenoAccelerator.late.o'
    subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func',
                    '--globalize-symbol=__GLOBAL__sub_I_NativeAdrenoAccelerator.cpp', str(objects[0]), str(late)], check=True)
    objects[0] = late
    arguments = record['arguments']
    compiler = next(a for a in arguments if 'clang' in Path(a).name)
    triple = arguments[arguments.index('-target') + 1] if '-target' in arguments else 'arm64e-apple-ios27.0'
    for stem, text in (('IOAcceleratorES-bindings', stubs), ('IOAcceleratorES-originals', targets)):
        asm = stage / (stem + '.S')
        asm.write_text(text)
        subprocess.run([compiler, '-target', triple, '-c', str(asm), '-o', str(asm.with_suffix('.o'))], check=True, cwd=record['directory'])
        objects.append(asm.with_suffix('.o'))
    return objects


def adreno_build(out):
    """Droid-VM's build of the NativeAdreno face, up to (not including) its accelerator step."""
    script = REPO / 'external/UniXNU-droidvm/ipados27/scripts/build-droidvm-gpu.py'
    text = script.read_text()
    cut = "print(subprocess.run(['python3', ROOT / 'scripts/build-adreno740-accelerator.py']"
    if text.count(cut) != 1:
        return {'compiled': False, 'why': 'build-droidvm-gpu.py no longer calls the accelerator build where expected'}
    # Its layout: ROOT/drivers/Adreno740, ROOT/kits/{ArmSmmu4K,AdrenoPM4,droidvm}, ROOT/.cache/xnu-work, ROOT/build/xnu.
    root = out / 'root'
    shutil.rmtree(root, ignore_errors=True)
    (root / 'drivers').mkdir(parents=True)
    (root / 'kits/droidvm').mkdir(parents=True)
    (root / '.cache').mkdir()
    (root / 'build').mkdir()
    (root / 'scripts').mkdir()
    links = {'drivers/Adreno740': ADRENO, 'kits/ArmSmmu4K': REPO / 'external/ArmSmmu4K', 'kits/AdrenoPM4': REPO / 'external/AdrenoPM4',
             '.cache/xnu-work': TREE / '.cache/xnu-work', 'build/xnu': TREE / 'build/xnu'}
    for name, target in links.items():
        if not target.exists() or (target.is_dir() and not any(target.iterdir())):
            return {'compiled': False, 'why': f'{target} is not checked out (scripts/setup/get-references.sh)'}
        (root / name).symlink_to(target.resolve())
    # kits/droidvm: the package's files, with DroidVMGpu.cpp from a staged copy (STAGED).
    gpu = (PACKAGE / 'DroidVMGpu.cpp').read_text()
    for anchor, replacement in STAGED:
        if gpu.count(anchor) != 1:
            return {'compiled': False, 'why': 'DroidVMGpu.cpp no longer has exactly one ' + anchor.strip()}
        gpu = gpu.replace(anchor, replacement)
    for path in PACKAGE.iterdir():
        if path.name == 'DroidVMGpu.cpp':
            (root / 'kits/droidvm' / path.name).write_text(gpu)
        else:
            (root / 'kits/droidvm' / path.name).symlink_to(path.resolve())
    staged = root / 'scripts/build-droidvm-gpu.py'
    staged.write_text(text[:text.index(cut)])
    result = subprocess.run([sys.executable, '-I', str(staged)], capture_output=True, text=True, timeout=1800)
    objects = sorted(str(p.relative_to(out)) for p in (root / 'build/droidvm-gpu').glob('*.o')) if result.returncode == 0 else []
    status = {'compiled': result.returncode == 0, 'objects': objects, 'linked': False}
    if result.returncode:
        status['why'] = (result.stderr or result.stdout).strip()[-2000:]
    return status


def main():
    for path in (REPO / 'drivers/build.py', ADRENO / 'DroidVMVirtgpuABI.h', RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', *SOURCES,
                 HERE / 'OneiOSAdreno.cpp', CLASSES):
        if not path.is_file():
            sys.exit(f'build-oneios-gpu: {path} missing (scripts/setup/get-references.sh UniXNU-droidvm NativeAdreno)')
    drivers = drivers_build()
    out = TREE / 'build/oneios-gpu'
    out.mkdir(parents=True, exist_ok=True)
    record = drivers.record(RECORDS, drivers.RECORDS['iokit'])
    # Metal's kernel side first: whether it is linked decides how OneiOSGpu.cpp is compiled.
    link = os.environ.get('ONEIOS_GPU_ADRENO', '1') != '0'
    adreno = out / 'adreno'
    adreno.mkdir(exist_ok=True)
    status = adreno_build(adreno) if link or os.environ.get('ONEIOS_GPU_ADRENO_CHECK', '1') != '0' else {'compiled': False, 'why': 'skipped'}
    linked = []
    if link:
        if not status['compiled']:
            sys.stderr.write(status.get('why', '') + '\n')
            fail('the NativeAdreno pieces did not build (ONEIOS_GPU_ADRENO=0 builds the archive without them)')
        if not ACCELERATOR_ABI.is_file():
            fail(f'{ACCELERATOR_ABI} missing (scripts/setup/get-references.sh NativeMetal)')
        linked = [adreno / name for name in status['objects']] + accelerator(adreno, drivers, record)
        status['linked'] = True
        status['objects'] = sorted(str(p.relative_to(adreno)) for p in linked)
    (adreno / 'status.json').write_text(json.dumps(status, indent=2) + '\n')
    objects = []
    for source in SOURCES:
        obj = out / (source.stem + '.o')
        # DYNAMIC: the transport's queues and command buffers (about 9 MB) are allocated at start, not kernel bss: with
        # them in __DATA the kernel no longer fits the collection's stock layout (kernel/prelink, the board's Q7).
        result = compile_one(drivers, record, source, obj, ['-I', str(PACKAGE), '-I', str(ADRENO), '-DDROIDVM_VIRTIO_GPU_DYNAMIC=1',
                                                             f'-DONEIOS_GPU_ADRENO={int(link)}'])
        if result.returncode:
            sys.stderr.write(result.stderr[-6000:])
            sys.exit(f'build-oneios-gpu: {source.name} did not compile')
        objects.append(obj)
    objects += linked
    archive = out / 'liboneiosgpu.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True, text=True, check=True).stdout
    missing = [name for name in REQUIRED + (REQUIRED_ADRENO if link else []) if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        sys.exit('build-oneios-gpu: definitions absent: ' + ', '.join(missing))
    sources = SOURCES + ([HERE / 'OneiOSAdreno.cpp', CLASSES, ACCELERATOR_ABI, PACKAGE / 'DroidVMGpu.cpp', REPO / 'external/AdrenoPM4/AdrenoPM4.c',
                          *(ADRENO / f'NativeAdreno{name}.cpp' for name in ('Accelerator', 'Publish', 'UserClient', 'Submit', 'Memory', 'Cpu'))]
                         if link else [])
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)), 'universe': 'iokit', 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in sources]}, indent=2) + '\n')
    print(f'build-oneios-gpu: {archive} ({len(objects)} objects); NativeAdreno pieces: '
          + (f'linked ({CLASS} bound at its 24A446 iPhone18,2 addresses)' if link else 'compiled, not linked' if status['compiled']
             else 'not compiled: ' + status.get('why', '?').splitlines()[-1][:200]))


if __name__ == '__main__':
    main()
