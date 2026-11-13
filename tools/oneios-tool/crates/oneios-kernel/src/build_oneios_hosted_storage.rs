//! `oneios-tool kernel gen storage` (was kernel/patches/oneios-hosted/storage/build-oneios-hosted-storage.py).
//!
//! The disks of the hosted OneiOS kernel: build/oneios-hosted-storage/liboneioshostedstorage.a.
//!
//!   IOBlockStorageDevice.cpp   IOStorageFamily's public class, compiled from its source and registered late, below
//!                              the original class of the kernel collection (as UniXNU-droidvm's
//!                              ipados27/scripts/build-droidvm-source-storage.py does for its board)
//!   OneiOSHostedDisk.cpp       every disk xnuhost serves (virtio-blk on virtio-mmio), reads, writes and flush
//!   OneiOSHostedStorage.cpp    native_source_storage_start: loads the original family and APFS, registers the two
//!                              classes, adds the personality
//!
//! Run by UniXNU's driver between the two kernel passes, in the build tree, with UNIXNU_DEVICE set:
//! kernel/build-hosted.sh adds it to board.toml's [kernel] archives of its copy of the device repository;
//! configs/oneios_hosted_defconfig force-loads the archive (CONFIG_XNU_EXTRA_LD_ARCHIVES).
//! The sources stay in kernel/patches/oneios-hosted/storage of the repository.

use crate::gen::{self, fail, Ctx};
use anyhow::Result;
use oneios_common::pyjson::Value;
use std::path::{Path, PathBuf};

/// The pin of the OS profile's IOStorageFamily source (IOStorageFamily-337.100.1), as UniXNU-droidvm's board.
const STORAGE_REVISION: &str = "7edb88fbae296fb7c8ce2f64e115e116e566d51c";
const LATE: [&str; 2] = ["IOBlockStorageDevice.cpp", "OneiOSHostedDisk.cpp"];
const MACRO: &str = "OSDefineMetaClassAndAbstractStructors(IOBlockStorageDevice,IOService)";
const REGISTRATION: &str = r#"
extern "C" const OSMetaClass *native_source_storage_parent(void);
OSDefineMetaClassWithInit(IOBlockStorageDevice, IOService, )
IOBlockStorageDevice::MetaClass::MetaClass()
    : OSMetaClass("OneiOSBlockStorageDevice", native_source_storage_parent(), sizeof(IOBlockStorageDevice)) {}
OSDefineAbstractStructors(IOBlockStorageDevice, IOService)
OSDefineOperatorMethods(IOBlockStorageDevice)
"#;

pub fn main() -> Result<()> {
    let ctx = Ctx::new("build-oneios-hosted-storage", "run through kernel/build-hosted.sh")?;
    let sources = ctx.repo.join("kernel/patches/oneios-hosted/storage");
    let storage = ctx.tree.join("refer/IOStorageFamily");
    for path in [storage.join("IOBlockStorageDevice.cpp"), sources.join("OneiOSHostedDisk.cpp"), ctx.repo.join("drivers/build.py"),
                 ctx.records.join("iokit/DEVELOPMENT/IOPlatformExpert.cpo.json")] {
        if !path.exists() {
            fail!("build-oneios-hosted-storage: {} missing", path.display());
        }
    }
    let out = ctx.tree.join("build/oneios-hosted-storage");
    gen::mkdirs(&out)?;
    let header_report = prepare_headers(&ctx, &storage, &out.join("headers"))?;
    let base = staged_base(&storage, &out)?;

    let record = ctx.record("iokit")?;
    let objcopy = gen::which("llvm-objcopy", "/opt/homebrew/opt/llvm/bin/llvm-objcopy");
    let mut objects = Vec::new();
    let mut report = Value::list();
    for source in [base.clone(), sources.join("OneiOSHostedDisk.cpp"), sources.join("OneiOSHostedStorage.cpp")] {
        let mut target = compile(&ctx, &record, &source, &out, source == base)?;
        if LATE.contains(&gen::name(&source).as_str()) {
            target = late(&objcopy, &source, &target)?;
        }
        objects.push(target);
        // `str(source.relative_to(REPO)) if source.is_relative_to(REPO)`: the staged copy too is below the
        // repository when the build tree is (out/ of this checkout); the other text is for a tree elsewhere.
        let path = ctx.relative(&source).unwrap_or_else(|_| "refer/IOStorageFamily/IOBlockStorageDevice.cpp (registration replaced)".into());
        report.push(gen::source_entry(path, Some("iokit"), &source)?);
    }
    let archive = out.join("liboneioshostedstorage.a");
    gen::archive(&archive, &objects)?;
    let mut listing = Value::dict();
    listing.set("sources", report);
    listing.set("headers", header_report);
    gen::write_json(&out.join("objects.json"), &listing, 2)?;
    println!("build-oneios-hosted-storage: {}", archive.display());
    Ok(())
}

/// UniXNU's scripts/prepare-storage-headers.py (the build tree's copy): the family's public headers at the pin
/// and the SDK's TargetConditionals.h into `out`/include, and its report. The Python script imported it and
/// called `prepare`; here it runs as the program it also is, which does the same after resolving the two paths,
/// and writes the report `prepare` returns to `out`/headers.json.
fn prepare_headers(ctx: &Ctx, storage: &Path, out: &Path) -> Result<Value> {
    let script = ctx.tree.join("scripts/prepare-storage-headers.py");
    let result = gen::capture(&["python3".into(), "-I".into(), gen::s(&script), "--source".into(), gen::s(storage), "--revision".into(),
                                STORAGE_REVISION.into(), "--output".into(), gen::s(out)], None, None)?;
    if result.code != 0 {
        eprint!("{}", result.stderr);
        fail!("build-oneios-hosted-storage: {} did not prepare the headers", script.display());
    }
    gen::read_json(&out.join("headers.json"))
}

/// The public class under another registered name, with the original class as its parent
/// (native_source_storage_parent, OneiOSHostedStorage.cpp); the C++ class name stays, since arm64e's vtable
/// discriminators are derived from it.
fn staged_base(storage: &Path, out: &Path) -> Result<PathBuf> {
    let base = out.join("IOBlockStorageDevice.cpp");
    let original = gen::read_text(&storage.join("IOBlockStorageDevice.cpp"))?;
    if original.matches(MACRO).count() != 1 {
        fail!("build-oneios-hosted-storage: the pinned IOBlockStorageDevice registration macro changed");
    }
    gen::write_text(&base, &original.replace(MACRO, REGISTRATION))?;
    Ok(base)
}

fn compile(ctx: &Ctx, record: &crate::drivers_build::Record, source: &Path, out: &Path, is_base: bool) -> Result<PathBuf> {
    let stem = gen::stem(source);
    let target = out.join(format!("{stem}.o"));
    let mut command = ctx.command(record, source, &target)?;
    command.extend(["-I".to_string(), gen::s(&out.join("headers/include"))]);
    if is_base {
        command.push("-Wno-error".into());
    }
    let result = gen::capture(&command, Some(Path::new(&record.directory)), Some(600))?;
    let log = result.stdout + &result.stderr;
    gen::write_text(&out.join(format!("{stem}.log")), &log)?;
    if result.code != 0 {
        eprint!("{}", gen::tail(&log, 8000));
        fail!("build-oneios-hosted-storage: {} did not compile", gen::name(source));
    }
    Ok(target)
}

/// No static initializer at kernel start: OneiOSHostedStorage.cpp runs it once the original family has loaded.
fn late(objcopy: &str, source: &Path, target: &Path) -> Result<PathBuf> {
    let late = target.with_extension("late.o");
    let initializer = format!("__GLOBAL__sub_I_{}", gen::name(source));
    gen::late_object(objcopy, target, &late, &["__DATA,__mod_init_func", "__DATA_CONST,__mod_init_func"], &initializer)?;
    let nm = gen::output(&[gen::which("llvm-nm", "nm"), gen::s(&late)])?;
    let sections = gen::output(&[gen::which("llvm-objdump", "objdump"), "-h".into(), gen::s(&late)])?;
    if !nm.contains(&format!(" T {initializer}")) || sections.contains("__mod_init_func") || sections.contains("__init_offsets") {
        fail!("build-oneios-hosted-storage: {}: the static initializer was not made a late one", gen::name(source));
    }
    Ok(late)
}
