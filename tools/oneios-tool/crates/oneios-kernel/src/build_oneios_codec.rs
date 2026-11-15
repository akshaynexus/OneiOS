//! `oneios-tool kernel gen codec` (no Python original: written in Rust, 2026-11-15).
// SPDX-License-Identifier: BSD-2-Clause-Patent
//!
//! The hosted kernel's video encoder and decoder: build/oneios-codec/liboneioscodec.a, built between the two kernel
//! passes from the compile records of the first. It publishes iOS's own paravirtualized VideoToolbox codecs
//! (AppleVideoToolboxParavirtualizationDriver), so VideoToolbox encodes HEVC and H.264 through the host's encoder
//! and CMPhoto writes HEIC (docs/hardware.md 7g; kernel/patches/oneios-hosted/codec/README.md).
//!
//!   DroidVMVirtioMedia.c    UniXNU-droidvm's virtio-media transport (external/UniXNU-droidvm/package), unchanged,
//!                           compiled as its iPadOS 27 board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1):
//!                           xnuhost's /arm-io/virtio-media-0 and its media-host-pool (runtime/bridges/encoder)
//!   DroidVMVTHost.cpp       UniXNU-droidvm's session logic (the host half of VideoToolbox's paravirtualized decoder
//!                           and encoder, played in the guest's kernel over the virtio-media device), staged with
//!                           one edit (staged/codec.rs): the hvcC's profile, tier and level are taken from the SPS
//!                           without the stream's emulation prevention bytes
//!   DroidVMVTParavirt.cpp   UniXNU-droidvm's AppleVideoToolboxParavirtualizationDriver and its user client,
//!                           unchanged; its static initializer is taken out of __mod_init_func and made global:
//!                           droidvm_vt_paravirt_attach runs it inside its own module load, after the kernel's
//!   OneiOSCodec.c           oneios_codec_start (oneios_drivers_start calls it, drivers/bridge/OneiOSBridge.c, where
//!                           a weak default stands for a kernel without this archive): starts the transport,
//!                           registers the classes, says what attached
//!
//! Boot arguments: oneios-codec=0 (nothing of this archive starts), droidvm-media=0 (no transport),
//! droidvm-vt-paravirt=1 (publish the service; xnuhost adds it with encoder=auto or encoder=sim; without it the
//! classes are registered and root can publish later: sysctl kern.droidvm_vt_paravirt=1), oneios-codec-report=0 (no
//! counters line).
//!
//! Force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES; kernel/build-hosted.sh adds it to the board's archives of the build's
//! device repository (after audio). Run by UniXNU's driver in the build tree, with UNIXNU_DEVICE = the device
//! repository. ONEIOS_CODEC_RECORDS and ONEIOS_CODEC_OUT name another record folder and output folder (a check of
//! the archive against a finished build's tree). No Apple binary is read.

use crate::gen::{self, fail, Compile, Ctx};
use crate::staged::codec::CODEC_HOST;
use anyhow::Result;
use oneios_common::pyjson::Value;
use std::path::{Path, PathBuf};

/// The start, the transport's start, the classes' registration, and the one static initializer it runs.
pub const REQUIRED: [&str; 4] = ["_oneios_codec_start", "_droidvm_media_start", "_droidvm_vt_paravirt_attach",
                                 "__GLOBAL__sub_I_DroidVMVTParavirt.cpp"];
/// What DroidVMVTParavirt.cpp calls in DroidVMVTHost.cpp and DroidVMVTHost.cpp in DroidVMVirtioMedia.c: an archive
/// without one of them is not the three files together.
pub const SESSION: [&str; 4] = ["_droidvm_vthost_available", "_droidvm_vthost_message", "_droidvm_media_ioctl", "_droidvm_media_mmap"];

/// DroidVMVTHost.cpp with the hvcC's profile, tier and level taken without emulation prevention bytes.
pub fn staged_text(text: String) -> Result<String> {
    gen::replace_once(text, CODEC_HOST, |anchor| {
        format!("build-oneios-codec: DroidVMVTHost.cpp no longer has exactly one `{}`", gen::first(anchor.trim(), 90))
    })
}

/// The same file name, in the archive's folder (its own includes come from the package folder: -I).
fn stage_host(host: &Path, out: &Path) -> Result<PathBuf> {
    let staged = out.join("DroidVMVTHost.cpp");
    gen::write_text(&staged, &staged_text(gen::read_text(host)?)?)?;
    Ok(staged)
}

pub fn main() -> Result<()> {
    let mut ctx = Ctx::new("build-oneios-codec", "run through the kernel build")?;
    if let Some(records) = gen::env("ONEIOS_CODEC_RECORDS") {
        ctx.records = gen::pypath(&records);
    }
    let here = ctx.repo.join("kernel/patches/oneios-hosted/codec");
    let package = ctx.repo.join("external/UniXNU-droidvm/package");
    let (transport, host, paravirt, start) = (package.join("DroidVMVirtioMedia.c"), package.join("DroidVMVTHost.cpp"),
                                              package.join("DroidVMVTParavirt.cpp"), here.join("OneiOSCodec.c"));
    let headers = [package.join("DroidVMVTWire.h"), package.join("DroidVMDmaRegion.h")];
    gen::need_files(&[ctx.repo.join("drivers/sources.txt"), transport.clone(), host.clone(), paravirt.clone(), start.clone(),
                      headers[0].clone(), headers[1].clone(),
                      ctx.records.join("iokit/DEVELOPMENT/IOPlatformExpert.cpo.json"), ctx.records.join("osfmk/DEVELOPMENT/startup.o.json")],
                    |path| format!("build-oneios-codec: {} missing (scripts/setup/get-references.sh UniXNU-droidvm; kernel/fetch.sh)", path.display()))?;
    let out = gen::env("ONEIOS_CODEC_OUT").map(|o| gen::pypath(&o)).unwrap_or_else(|| ctx.tree.join("build/oneios-codec"));
    gen::mkdirs(&out)?;

    let compile = Compile { ctx: &ctx, shown: 8000 };
    let include = |extra: &[&str]| -> Vec<String> {
        ["-I".to_string(), gen::s(&package)].into_iter().chain(extra.iter().map(|e| e.to_string())).collect()
    };
    let (mach, iokit) = (ctx.record("mach")?, ctx.record("iokit")?);
    let transport_object = compile.object(&mach, &transport, &out.join("DroidVMVirtioMedia.o"), &include(&["-DDROIDVM_DEVICE_DYNAMIC=1"]))?;
    let start_object = compile.object(&mach, &start, &out.join("OneiOSCodec.o"), &include(&[]))?;
    let staged = stage_host(&host, &out)?;
    let host_object = compile.object(&iokit, &staged, &out.join("DroidVMVTHost.o"), &include(&[]))?;
    let paravirt_object = compile.object(&iokit, &paravirt, &out.join("DroidVMVTParavirt.o"), &include(&[]))?;
    let late = paravirt_object.with_extension("late.o");
    gen::late_object(&gen::which("llvm-objcopy", "objcopy"), &paravirt_object, &late, &["__DATA,__mod_init_func"],
                     "__GLOBAL__sub_I_DroidVMVTParavirt.cpp")?;
    let objects = [transport_object, start_object, host_object.clone(), late.clone()];
    let archive = out.join("liboneioscodec.a");
    gen::archive(&archive, &objects)?;
    gen::require_definitions(&ctx, &archive, &REQUIRED)?;
    gen::require_definitions(&ctx, &archive, &SESSION)?;
    // The classes register in droidvm_vt_paravirt_attach's module load only: a static initializer left in either C++
    // object would run at the kernel's own load (DroidVMVTHost.cpp has no class and must have no initializer).
    for (object, why) in [(&late, "its classes would register twice"), (&host_object, "it has a static initializer the archive does not expect")] {
        let sections = gen::capture(&[gen::which("llvm-objdump", "objdump"), "-h".into(), gen::s(object)], None, None)?.stdout;
        if sections.contains("__mod_init_func") {
            fail!("build-oneios-codec: {} has a __mod_init_func section: {why}", gen::name(object));
        }
    }
    // The staged source last, as the audio archive lists its own (the upstream source with the edit list applied).
    let mut list = Value::list();
    for path in [transport, host, paravirt, headers[0].clone(), headers[1].clone(), start, staged] {
        list.push(gen::source_entry(ctx.relative(&path).unwrap_or_else(|_| gen::s(&path)), None, &path)?);
    }
    let mut listing = Value::dict();
    listing.set("sources", list);
    gen::write_json(&out.join("objects.json"), &listing, 2)?;
    println!("build-oneios-codec: {} ({} objects: the virtio-media transport, the VideoToolbox paravirtualization driver \
              and its sessions, the start)", archive.display(), objects.len());
    Ok(())
}
