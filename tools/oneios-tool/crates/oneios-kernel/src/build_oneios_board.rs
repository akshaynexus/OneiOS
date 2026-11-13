//! `oneios-tool kernel gen board` (was kernel/device/scripts/build-oneios-board.py).
//!
//! The OneiOS board's archives, built between the two kernel passes (board.toml [kernel] archives), from the
//! compile records of the first pass:
//!
//!   build/oneios-drivers/liboneiosdrivers.a   drivers/ of the OneiOS repository, by its own drivers/build.py
//!                                             (bridge transport, haptics, Bluetooth; entry oneios_drivers_start)
//!   build/oneios-board/liboneiosboard.a       UniXNU-droidvm package/DroidVMCpufreq.c, unchanged, compiled as its
//!                                             board does (Mach universe, -DDROIDVM_DEVICE_DYNAMIC=1; entry
//!                                             droidvm_cpufreq_attach)
//!
//! Both are force-loaded by CONFIG_XNU_EXTRA_LD_ARCHIVES (configs/oneios_defconfig) and started by
//! oneios_board_late_start (patches/xnu-12377/oneios-board-start.patch). Run by UniXNU's driver in the build tree,
//! with UNIXNU_DEVICE = kernel/device.

use crate::gen::{self, fail, Ctx};
use anyhow::Result;
use oneios_common::pyjson::Value;
use std::path::Path;

pub fn main() -> Result<()> {
    let ctx = Ctx::new("build-oneios-board", "run through the kernel build")?;
    let build = ctx.repo.join("drivers/build.py");
    let cpufreq = ctx.repo.join("external/UniXNU-droidvm/package/DroidVMCpufreq.c");
    println!("oneios-board: drivers/ of {}{}", ctx.repo.display(),
             if ctx.repo_from_env { "" } else { " (found from the device directory: ONEIOS_REPO is not set)" });
    gen::need_files(&[build.clone(), cpufreq.clone(), ctx.records.join("osfmk/DEVELOPMENT/startup.o.json")],
                    |path| format!("build-oneios-board: {} missing", path.display()))?;
    drivers_archive(&ctx, &build)?;
    let archive = cpufreq_archive(&ctx, &cpufreq)?;
    println!("build-oneios-board: {}", archive.display());
    Ok(())
}

/// 1. drivers/: its own script, with this tree's records and output place.
fn drivers_archive(ctx: &Ctx, build: &Path) -> Result<()> {
    gen::run(&["python3".into(), "-I".into(), gen::s(build), "--records".into(), gen::s(&ctx.records), "--out".into(),
               gen::s(&ctx.tree.join("build/oneios-drivers"))], None)
}

/// 2. DroidVMCpufreq.c, with drivers/build.py's rewrite of a recorded compile command.
fn cpufreq_archive(ctx: &Ctx, cpufreq: &Path) -> Result<std::path::PathBuf> {
    let out = ctx.tree.join("build/oneios-board");
    gen::mkdirs(&out)?;
    let record = ctx.record("mach")?;
    let target = out.join("DroidVMCpufreq.o");
    let mut command = ctx.command(&record, cpufreq, &target)?;
    command.push("-DDROIDVM_DEVICE_DYNAMIC=1".into());
    let result = gen::capture(&command, Some(Path::new(&record.directory)), Some(600))?;
    if result.code != 0 {
        eprint!("{}", result.stderr);
        fail!("build-oneios-board: DroidVMCpufreq.c did not compile");
    }
    let archive = out.join("liboneiosboard.a");
    gen::archive(&archive, &[target])?;
    let mut entry = Value::dict();
    entry.set("path", "external/UniXNU-droidvm/package/DroidVMCpufreq.c");
    entry.set("universe", "mach");
    entry.set("sha256", gen::sha256(cpufreq)?);
    let mut sources = Value::list();
    sources.push(entry);
    let mut listing = Value::dict();
    listing.set("sources", sources);
    gen::write_json(&out.join("objects.json"), &listing, 2)?;
    Ok(archive)
}
