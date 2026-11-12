#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause-Patent
"""The hosted kernel's speaker and microphone: build/oneios-audio/liboneiosaudio.a, built between the two kernel
passes from the compile records of the first.

  DroidVMVirtioSnd.c   UniXNU-droidvm's virtio-snd transport (external/UniXNU-droidvm/package), unchanged, compiled
                       as its iPadOS 27 board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1: its rings and buffers
                       come from one region allocated at start): xnuhost's /arm-io/virtio-snd
  DroidVMAudio.cpp     UniXNU-droidvm's two IOAudio2Devices (VirtualAudio's "Speaker" and "Mic"), staged with three
                       boot arguments (stage_audio below); its static initializer is taken out of __mod_init_func and
                       made global: droidvm_audio_attach runs it inside its own module load, after the kernel's
  OneiOSAudio.c        oneios_audio_start (oneios_drivers_start calls it, drivers/bridge/OneiOSBridge.c): starts the
                       transport, says what attached, reports the counters

Boot arguments: droidvm-snd=0 (no transport: the devices play into silence), droidvm-audio=0 (no devices),
oneios-audio-out-uid=UID and oneios-audio-in-uid=UID (the devices' UIDs, default Speaker and Mic; "none" leaves the
microphone out), oneios-audio-latency=FRAMES (the speaker's "output latency", default 6480: 135 ms, what the host
path measured on the phone, so iOS keeps pictures in step with the sound), oneios-audio-report=0.

Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES; kernel/build-hosted.sh copies this script into the build's device
repository and adds it to the board's archives. Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the
device repository. No Apple binary is read.
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
DEVICE = Path(os.environ.get('UNIXNU_DEVICE') or sys.exit('build-oneios-audio: UNIXNU_DEVICE is not set (run through the kernel build)'))
REPO = Path(os.environ.get('ONEIOS_REPO') or DEVICE.resolve().parents[1])
HERE = REPO / 'kernel/patches/oneios-hosted/audio'
PACKAGE = REPO / 'external/UniXNU-droidvm/package'
RECORDS = Path(os.environ.get('ONEIOS_AUDIO_RECORDS') or TREE / 'build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837')
TRANSPORT, DEVICES, START = PACKAGE / 'DroidVMVirtioSnd.c', PACKAGE / 'DroidVMAudio.cpp', HERE / 'OneiOSAudio.c'
REQUIRED = ['_oneios_audio_start', '_droidvm_snd_start', '_droidvm_audio_attach', '__GLOBAL__sub_I_DroidVMAudio.cpp']
# The output latency the speaker device publishes (frames at 48 kHz): the host path's, measured on the phone
# (oneios-audio's report "latency about 130-137 ms": deep-buffer track 128-136 ms + the guest's period and the
# virtio hop, docs/hardware.md section 6).
OUTPUT_LATENCY = 6480


def fail(text):
    sys.exit('build-oneios-audio: ' + text)


def drivers_build():
    spec = importlib.util.spec_from_file_location('oneios_drivers_build', REPO / 'drivers/build.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def replace_once(text, pairs, what):
    for anchor, replacement in pairs:
        if text.count(anchor) != 1:
            fail(f'{what} no longer has exactly one `{anchor.strip()[:90]}`')
        text = text.replace(anchor, replacement)
    return text


def stage_audio(out):
    """DroidVMAudio.cpp with the UIDs and the speaker's output latency from boot arguments. The same file name: the
    static initializer droidvm_audio_attach calls is named after it."""
    text = replace_once(DEVICES.read_text(), (
        ('extern kmod_info_t g_kernel_kmod_info;\n',
         'extern kmod_info_t g_kernel_kmod_info;\n'
         '// OneiOS (build-oneios-audio.py): the UIDs and the speaker\'s output latency, from boot arguments at attach.\n'
         '#include <pexpert/pexpert.h>\n'
         f'static uint32_t oneios_output_latency = {OUTPUT_LATENCY};\n'
         'static char oneios_out_uid[32] = "Speaker", oneios_in_uid[32] = "Mic";\n'),
        ('    setNumber(e, "output latency", Latency);\n',
         '    setNumber(e, "output latency", oneios_output_latency);   // OneiOS: the host path\'s latency\n'),
        ('    bool speaker = publishDevice(resources, false, "Speaker", "droidvm-audio-out");\n'
         '    bool mic = publishDevice(resources, true, "Mic", "droidvm-audio-in");\n',
         '    // OneiOS: oneios-audio-out-uid, oneios-audio-in-uid (none: no microphone), oneios-audio-latency.\n'
         '    PE_parse_boot_arg_str("oneios-audio-out-uid", oneios_out_uid, sizeof(oneios_out_uid));\n'
         '    PE_parse_boot_arg_str("oneios-audio-in-uid", oneios_in_uid, sizeof(oneios_in_uid));\n'
         '    PE_parse_boot_argn("oneios-audio-latency", &oneios_output_latency, sizeof(oneios_output_latency));\n'
         '    bool speaker = publishDevice(resources, false, oneios_out_uid, "droidvm-audio-out");\n'
         '    bool mic = strcmp(oneios_in_uid, "none") != 0 && publishDevice(resources, true, oneios_in_uid, "droidvm-audio-in");\n'),
        ('    IOLog("DroidVMAudio: published Speaker=%d Mic=%d as IOAudio2Devices\\n", speaker, mic);\n',
         '    IOLog("DroidVMAudio: published Speaker=%d Mic=%d as IOAudio2Devices (UIDs %s, %s; output latency %u frames)\\n", speaker, mic,\n'
         '          oneios_out_uid, oneios_in_uid, oneios_output_latency);\n'),
    ), 'DroidVMAudio.cpp')
    staged = out / 'DroidVMAudio.cpp'
    staged.write_text(text)
    return staged


def main():
    for path in (REPO / 'drivers/build.py', TRANSPORT, DEVICES, START, PACKAGE / 'DroidVMDmaRegion.h',
                 RECORDS / 'iokit/DEVELOPMENT/IOPlatformExpert.cpo.json', RECORDS / 'osfmk/DEVELOPMENT/startup.o.json'):
        if not path.is_file():
            fail(f'{path} missing (scripts/setup/get-references.sh UniXNU-droidvm; kernel/fetch.sh)')
    drivers = drivers_build()
    out = Path(os.environ.get('ONEIOS_AUDIO_OUT') or TREE / 'build/oneios-audio')
    out.mkdir(parents=True, exist_ok=True)

    def compile_source(universe, source, obj, extra=()):
        record = drivers.record(RECORDS, drivers.RECORDS[universe])
        command = drivers.command(record, source, obj) + ['-I', str(PACKAGE)] + list(extra)
        result = subprocess.run(command, cwd=record['directory'], capture_output=True, text=True, timeout=900)
        obj.with_suffix('.log').write_text(result.stderr)
        if result.returncode:
            sys.stderr.write(result.stderr[-8000:])
            fail(f'{source.name} did not compile')
        return obj

    transport = compile_source('mach', TRANSPORT, out / 'DroidVMVirtioSnd.o', ('-DDROIDVM_DEVICE_DYNAMIC=1',))
    start = compile_source('mach', START, out / 'OneiOSAudio.o')
    devices = compile_source('iokit', stage_audio(out), out / 'DroidVMAudio.o')
    late = devices.with_suffix('.late.o')
    objcopy = shutil.which('llvm-objcopy') or 'objcopy'
    subprocess.run([objcopy, '--remove-section=__DATA,__mod_init_func', '--globalize-symbol=__GLOBAL__sub_I_DroidVMAudio.cpp',
                    str(devices), str(late)], check=True)
    objects = [transport, start, late]
    archive = out / 'liboneiosaudio.a'
    archive.unlink(missing_ok=True)
    subprocess.run([shutil.which('llvm-ar') or 'ar', 'rcs', str(archive), *map(str, objects)], check=True)
    defined = subprocess.run([shutil.which('llvm-nm') or 'nm', '-g', '--defined-only', str(archive)], capture_output=True,
                             text=True, check=True).stdout
    missing = [name for name in REQUIRED if not any(line.endswith(' ' + name) for line in defined.splitlines())]
    if missing:
        fail('definitions absent: ' + ', '.join(missing))
    sections = subprocess.run([shutil.which('llvm-objdump') or 'objdump', '-h', str(late)], capture_output=True, text=True).stdout
    if '__mod_init_func' in sections:
        fail('DroidVMAudio.late.o still has a __mod_init_func section: its classes would register twice')
    sources = [TRANSPORT, DEVICES, PACKAGE / 'DroidVMDmaRegion.h', START, Path(__file__).resolve()]
    (out / 'objects.json').write_text(json.dumps({'sources': [
        {'path': str(p.relative_to(REPO)) if p.is_relative_to(REPO) else str(p), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
        for p in sources]}, indent=2) + '\n')
    print(f'build-oneios-audio: {archive} ({len(objects)} objects: the virtio-snd transport, the Speaker and Mic IOAudio2Devices, '
          f'the start; output latency {OUTPUT_LATENCY} frames)')


if __name__ == '__main__':
    main()
