//! `oneios-tool kernel gen board` (was kernel/device/scripts/build-oneios-board.py).
//!
//! The OneiOS board's archives, built between the two kernel passes (board.toml [kernel] archives), from the
//! compile records of the first pass:
//!
//!   build/oneios-drivers/liboneiosdrivers.a   drivers/ of the OneiOS repository, by drivers_build.rs (was drivers/build.py)
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
    let list = ctx.repo.join("drivers/sources.txt");
    let cpufreq = ctx.repo.join("external/UniXNU-droidvm/package/DroidVMCpufreq.c");
    println!("oneios-board: drivers/ of {}{}", ctx.repo.display(),
             if ctx.repo_from_env { "" } else { " (found from the device directory: ONEIOS_REPO is not set)" });
    gen::need_files(&[list, cpufreq.clone(), ctx.records.join("osfmk/DEVELOPMENT/startup.o.json")],
                    |path| format!("build-oneios-board: {} missing", path.display()))?;
    drivers_archive(&ctx)?;
    let archive = cpufreq_archive(&ctx, &cpufreq)?;
    println!("build-oneios-board: {}", archive.display());
    Ok(())
}

/// 1. drivers/: its own list of sources, with this tree's records and output place (the script ran
/// `drivers/build.py --records RECORDS --out TREE/build/oneios-drivers`).
fn drivers_archive(ctx: &Ctx) -> Result<()> {
    crate::drivers_build::build(&crate::drivers_build::here(&ctx.repo)?, &ctx.records, &ctx.tree.join("build/oneios-drivers"), false)
}

/// 2. DroidVMCpufreq.c, with the rewrite of a recorded compile command that drivers/ is built with.
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
