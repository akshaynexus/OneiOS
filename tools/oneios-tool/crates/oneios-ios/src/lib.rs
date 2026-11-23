//! `oneios-tool ios`: the iOS user-space toolchain's own steps (scripts/ios, docs/building.md section 9). They were
//! Python until 2026-11-21 (scripts/ios/ios-sign.py, macho-check.py, make-tbds.py, userland/metal/shim/codesign
//! and userland/metal/build.sh's inline blocks): README "ios" has what each one was compared with.

pub mod check;
pub mod codesign;
pub mod diff;
pub mod macho;
pub mod metal;
pub mod sign;
pub mod tbd;
pub mod tbds;

use clap::Subcommand;
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign iOS Mach-Os ad hoc in place, as `codesign --force --sign - --pagesize 4096` does on a Mac, and print
    /// each one's line: CDHASH ARCH pages=4096 IDENTIFIER FILE (cctools' codesign_allocate makes the room:
    /// IOS_CODESIGN_ALLOCATE, IOS_CCTOOLS or out/ios-toolchain/cctools/bin)
    Sign {
        /// Default: the file's name
        #[arg(long)]
        identifier: Option<String>,
        /// A property list (XML or binary): signed as it is and as DER
        #[arg(long, value_name = "PLIST")]
        entitlements: Option<String>,
        /// Print the records as JSON
        #[arg(long)]
        json: bool,
        #[arg(required = true)]
        files: Vec<String>,
    },
    /// `codesign` for the kernel base's build scripts: --sign (over `sign`), --verify, --display, -d --entitlements :-
    Codesign {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Check that iOS Mach-Os built here will load in the guest: arm64e, iOS, signed with 4 KiB pages, every
    /// library stubbed and every undefined symbol exported by its library (exit 1 when one check fails)
    MachoCheck {
        /// The stubs (default IOS_TBD, else out/ios27-tbd: `ios tbds`)
        #[arg(long, value_name = "DIR")]
        tbd: Option<String>,
        /// Check against the SDK's stubs (scripts/ios/env.sh's IOS_SDK), not the iOS 27 ones
        #[arg(long)]
        sdk: bool,
        /// A dylib that travels with the files (its own symbol table is read)
        #[arg(long, value_name = "FILE")]
        local: Vec<String>,
        #[arg(long, default_value = "arm64e")]
        arch: String,
        #[arg(required = true)]
        files: Vec<String>,
    },
    /// Make .tbd stubs of the guest's own iOS 27 libraries from the dyld shared cache: every image (4696 in
    /// 24A446) or the NAMEs (install names or their last component); stubs that exist are kept
    Tbds {
        /// The cache's folder or its main file. Default out/ios27-cache; when it is not there, the cache's folder is
        /// copied there out of --image first (our APFS reader: no mount)
        #[arg(long, value_name = "DIR")]
        cache: Option<PathBuf>,
        /// Default out/image/24A446/systemos.img
        #[arg(long, value_name = "IMG")]
        image: Option<PathBuf>,
        /// Default out/ios27-tbd
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        names: Vec<String>,
    },
    /// Compare two trees of stubs by what the linker reads (install names, re-exports, exported names)
    TbdDiff {
        a: PathBuf,
        b: PathBuf,
        /// Read only the names A lists as symbols, not those its Objective-C lists stand for
        #[arg(long)]
        a_listed: bool,
        /// How many differences to print
        #[arg(long, default_value_t = 20)]
        show: usize,
    },
    /// userland/metal/build.sh's `stage`: the staged copy ROOT's changes against upstream, each said
    MetalStageEdit {
        root: PathBuf,
        hooks: PathBuf,
        jobs: String,
        /// 1 when the compiler knows -Wunused-but-set-global
        knows: String,
        /// The bridge on metal2vulkan-rt (userland/metal/rt): the plug-in script links the static library
        /// userland/metal/rt/build.sh ios put in build/adreno-metal-bridge/ios27, and does not build the old one
        #[arg(long)]
        rt: bool,
    },
    /// userland/metal/build.sh's `finish`: NativeAdrenoMetal.build.json (the environment is scripts/ios/env.sh's)
    MetalRecord {
        /// This checkout (its external/ has the pinned sources)
        root: PathBuf,
        bundle: PathBuf,
        record: PathBuf,
        hooks: PathBuf,
        target: String,
    },
    /// userland/metal/build.sh's `addon`: the bundle as the kernel.kc.gpuaddon sidecar (NGPUADD2)
    Gpuaddon { bundle: PathBuf, output: PathBuf },
    /// The boot addon (the kernel.kc.gpuaddon sidecar, NGPUADD2) with the Metal plug-in (addons/, hosted 0031), the
    /// capture plug-in (camera/, hosted 0090) or both, and OUTPUT.cdhashes (its Mach-O files' CDHashes, for the
    /// image's owner-trust.json; `phone addon OUTPUT` stages both)
    Addon {
        output: PathBuf,
        /// NativeAdrenoMetal.bundle (userland/metal/build.sh's)
        #[arg(long)]
        metal: Option<PathBuf>,
        /// userland/camera/build.sh's output folder (or its H9ISP.mediacapture)
        #[arg(long)]
        camera: Option<PathBuf>,
    },
}

/// This checkout: IOS_REPO or ONEIOS_REPO, else the folder above the current one (or the tool's binary) that has
/// scripts/ios/env.sh.
pub fn repo() -> PathBuf {
    for k in ["IOS_REPO", "ONEIOS_REPO"] {
        if let Some(v) = std::env::var_os(k).filter(|v| !v.is_empty()) {
            return PathBuf::from(v);
        }
    }
    let up = |start: &Path| start.ancestors().find(|a| a.join("scripts/ios/env.sh").is_file()).map(Path::to_path_buf);
    std::env::current_dir().ok().and_then(|d| up(&d)).or_else(|| std::env::current_exe().ok().and_then(|e| up(&e))).unwrap_or_else(|| PathBuf::from("."))
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn run(command: Command) -> anyhow::Result<i32> {
    let repo = repo();
    match command {
        Command::Sign { identifier, entitlements, json, files } => sign::run(identifier.as_deref(), entitlements.as_deref(), json, &files).map(|()| 0),
        Command::Codesign { args } => codesign::run(&args),
        Command::MachoCheck { tbd, sdk, local, arch, files } => check::run(&repo, &check::Options { tbd, sdk, local, arch, files }),
        Command::Tbds { cache, image, out, names } => tbds::run(
            &repo,
            &tbds::Options {
                cache,
                image: image.unwrap_or_else(|| repo.join("out/image/24A446/systemos.img")),
                out: out.unwrap_or_else(|| repo.join("out/ios27-tbd")),
                names,
            },
        ),
        Command::TbdDiff { a, b, a_listed, show } => diff::run(&a, &b, a_listed, show),
        Command::MetalStageEdit { root, hooks, jobs, knows, rt } => metal::stage_edit(&root, &hooks, &jobs, knows == "1", rt).map(|()| 0),
        Command::MetalRecord { root, bundle, record, hooks, target } => metal::record(&root, &bundle, &record, &hooks, &target).map(|()| 0),
        Command::Gpuaddon { bundle, output } => metal::gpuaddon(&bundle, &output).map(|()| 0),
        Command::Addon { output, metal, camera } => metal::addon(&output, metal.as_deref(), camera.as_deref()).map(|()| 0),
    }
}

pub fn main(command: Command) -> i32 {
    let name = match &command {
        Command::Sign { .. } => "sign",
        Command::Codesign { .. } => "codesign",
        Command::MachoCheck { .. } => "macho-check",
        Command::Tbds { .. } => "tbds",
        Command::TbdDiff { .. } => "tbd-diff",
        Command::MetalStageEdit { .. } | Command::MetalRecord { .. } | Command::Gpuaddon { .. } | Command::Addon { .. } => "metal",
    };
    match run(command) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("oneios-tool ios {name}: {e:#}");
            1
        }
    }
}
