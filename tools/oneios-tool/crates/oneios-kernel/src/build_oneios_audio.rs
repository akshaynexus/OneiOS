//! `oneios-tool kernel gen audio` (was kernel/patches/oneios-hosted/audio/build-oneios-audio.py).
// SPDX-License-Identifier: BSD-2-Clause-Patent
//!
//! The hosted kernel's speaker and microphone: build/oneios-audio/liboneiosaudio.a, built between the two kernel
//! passes from the compile records of the first.
//!
//!   DroidVMVirtioSnd.c   UniXNU-droidvm's virtio-snd transport (external/UniXNU-droidvm/package), unchanged, compiled
//!                        as its iPadOS 27 board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1: its rings and buffers
//!                        come from one region allocated at start): xnuhost's /arm-io/virtio-snd
//!   DroidVMAudio.cpp     UniXNU-droidvm's two IOAudio2Devices (VirtualAudio's "Speaker" and "Mic"), staged with boot
//!                        arguments and a third device, "Actuator" (staged/audio.rs); its static initializer is taken
//!                        out of __mod_init_func and made global: droidvm_audio_attach runs it inside its own module
//!                        load, after the kernel's
//!   OneiOSAudio.c        oneios_audio_start (oneios_drivers_start calls it, drivers/bridge/OneiOSBridge.c): starts the
//!                        transport, says what attached, reports the counters
//!
//! Boot arguments: droidvm-snd=0 (no transport: the devices play into silence), droidvm-audio=0 (no devices),
//! oneios-audio-out-uid=UID and oneios-audio-in-uid=UID (the devices' UIDs, default Speaker and Mic; "none" leaves the
//! microphone out), oneios-audio-latency=FRAMES (the speaker's "output latency", default 6480: 135 ms, what the host
//! path measured on the phone, so iOS keeps pictures in step with the sound), oneios-audio-report=0,
//! oneios-audio-selftest=SECONDS (a quiet 1 kHz tone from the kernel, OneiOSAudio.c), oneios-actuator=0 (no Actuator
//! device), oneios-actuator-channels=1|2 (default 1).
//!
//! The Actuator device: VirtualAudio makes VirtualAudioDevice_Actuator (Device_Actuator_Aspen, iOS 27's VirtualAudio.plugin)
//! on a physical device with UID "Actuator"; Core Haptics, UIFeedbackGenerator and the keyboard play their haptics on it
//! as audio once MobileGestalt says closed-loop haptics (IODeviceTree:/product/haptics, drivers/haptics). Its engine runs
//! like the speaker's, but each period goes to drivers/haptics (oneios_haptic_actuator_play: the motor through the host's
//! haptics bridge), not to virtio-snd. Published only when drivers/haptics says the host has an actuator; the decision is
//! told to drivers/haptics (oneios_haptic_actuator_published), which publishes closed-loop only with the device. Its
//! output latency is the engine's own (one period), not the speaker's host path: the motor path is a bridge message.
//!
//! Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES; kernel/build-hosted.sh adds it to the board's archives of the build's
//! device repository. Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device repository. No Apple
//! binary is read.
//!
//! One line of objects.json is not the script's: the script listed itself (its copy in the build tree) as the
//! fifth source, so that a change of the edit list changed the record. Here that entry is the staged
//! DroidVMAudio.cpp, which is the upstream source with the edit list applied.

use crate::gen::{self, fail, Compile, Ctx};
use crate::staged::{audio::AUDIO, OUTPUT_LATENCY};
use anyhow::Result;
use oneios_common::pyjson::Value;
use std::path::{Path, PathBuf};

const REQUIRED: [&str; 4] = ["_oneios_audio_start", "_droidvm_snd_start", "_droidvm_audio_attach", "__GLOBAL__sub_I_DroidVMAudio.cpp"];

/// DroidVMAudio.cpp with the UIDs and the speaker's output latency from boot arguments, and the Actuator device.
pub fn staged_text(text: String) -> Result<String> {
    gen::replace_once(text, AUDIO, |anchor| {
        format!("build-oneios-audio: DroidVMAudio.cpp no longer has exactly one `{}`", gen::first(anchor.trim(), 90))
    })
}

/// The same file name: the static initializer droidvm_audio_attach calls is named after it.
fn stage_audio(devices: &Path, out: &Path) -> Result<PathBuf> {
    let staged = out.join("DroidVMAudio.cpp");
    gen::write_text(&staged, &staged_text(gen::read_text(devices)?)?)?;
    Ok(staged)
}

pub fn main() -> Result<()> {
    let mut ctx = Ctx::new("build-oneios-audio", "run through the kernel build")?;
    if let Some(records) = gen::env("ONEIOS_AUDIO_RECORDS") {
        ctx.records = gen::pypath(&records);
    }
    let here = ctx.repo.join("kernel/patches/oneios-hosted/audio");
    let package = ctx.repo.join("external/UniXNU-droidvm/package");
    let (transport, devices, start) = (package.join("DroidVMVirtioSnd.c"), package.join("DroidVMAudio.cpp"), here.join("OneiOSAudio.c"));
    gen::need_files(&[ctx.repo.join("drivers/sources.txt"), transport.clone(), devices.clone(), start.clone(), package.join("DroidVMDmaRegion.h"),
                      ctx.records.join("iokit/DEVELOPMENT/IOPlatformExpert.cpo.json"), ctx.records.join("osfmk/DEVELOPMENT/startup.o.json")],
                    |path| format!("build-oneios-audio: {} missing (scripts/setup/get-references.sh UniXNU-droidvm; kernel/fetch.sh)", path.display()))?;
    let out = gen::env("ONEIOS_AUDIO_OUT").map(|o| gen::pypath(&o)).unwrap_or_else(|| ctx.tree.join("build/oneios-audio"));
    gen::mkdirs(&out)?;

    let compile = Compile { ctx: &ctx, shown: 8000 };
    let include = |extra: &[&str]| -> Vec<String> {
        ["-I".to_string(), gen::s(&package)].into_iter().chain(extra.iter().map(|e| e.to_string())).collect()
    };
    let mach = ctx.record("mach")?;
    let transport_object = compile.object(&mach, &transport, &out.join("DroidVMVirtioSnd.o"), &include(&["-DDROIDVM_DEVICE_DYNAMIC=1"]))?;
    let start_object = compile.object(&mach, &start, &out.join("OneiOSAudio.o"), &include(&[]))?;
    let staged = stage_audio(&devices, &out)?;
    let devices_object = compile.object(&ctx.record("iokit")?, &staged, &out.join("DroidVMAudio.o"), &include(&[]))?;
    let late = devices_object.with_extension("late.o");
    gen::late_object(&gen::which("llvm-objcopy", "objcopy"), &devices_object, &late, &["__DATA,__mod_init_func"],
                     "__GLOBAL__sub_I_DroidVMAudio.cpp")?;
    let objects = [transport_object, start_object, late.clone()];
    let archive = out.join("liboneiosaudio.a");
    gen::archive(&archive, &objects)?;
    gen::require_definitions(&ctx, &archive, &REQUIRED)?;
    let sections = gen::capture(&[gen::which("llvm-objdump", "objdump"), "-h".into(), gen::s(&late)], None, None)?.stdout;
    if sections.contains("__mod_init_func") {
        fail!("build-oneios-audio: DroidVMAudio.late.o still has a __mod_init_func section: its classes would register twice");
    }
    // The staged source in the script's place (the module comment).
    let mut list = Value::list();
    for path in [transport, devices, package.join("DroidVMDmaRegion.h"), start, staged] {
        list.push(gen::source_entry(ctx.relative(&path).unwrap_or_else(|_| gen::s(&path)), None, &path)?);
    }
    let mut listing = Value::dict();
    listing.set("sources", list);
    gen::write_json(&out.join("objects.json"), &listing, 2)?;
    println!("build-oneios-audio: {} ({} objects: the virtio-snd transport, the Speaker, Mic and Actuator IOAudio2Devices, \
              the start; output latency {OUTPUT_LATENCY} frames)", archive.display(), objects.len());
    Ok(())
}
