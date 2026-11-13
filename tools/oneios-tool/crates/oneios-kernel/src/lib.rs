//! `oneios-tool kernel`: the kernel build's own steps.
//!
//!   kernel gen NAME          one board archive of the kernel build: what UniXNU's driver runs between the two kernel
//!                            passes (board.toml [kernel] archives), in the build tree, with UNIXNU_DEVICE set.
//!                            NAME: board, storage, aes, keystore, net, hid, audio, gpu, display
//!   kernel build-record      the record of a finished VM kernel build (build-linux.sh, build-macos.sh)
//!   kernel drivers           drivers/ of the repository into liboneiosdrivers.a (--check: compile only)
//!   kernel compile-command   the compile command the archives are built with, for a record
//!   kernel hosted-fixup      the post-link text fixes of the hosted kernel (kernel/build-hosted.sh)
//!   kernel kdk-headers       a headers-only KDKROOT from a Kernel Debug Kit (kernel/build-macos.sh, KDK_HEADERS=1)
//!   kernel iig-fix           the fixes of the Linux toolchain's iig (kernel/linux/unixnu-layout.sh)
//!   kernel series-check      the checks of the series and the profiles that need no sources (kernel/tests/host.sh)
//!   kernel layout hid|net    the record of the original classes a board driver subclasses, from the kernelcache
//!   kernel port-legacy-display   the display's record: Droid-VM's reviewed iPad layout carried over to this kernelcache
//!
//! Each was a Python script (kernel/patches/oneios-hosted/*/build-oneios-*.py, kernel/device/scripts/
//! build-oneios-board.py, kernel/build-record.py, hid/layout.py, net/layout.py, display/port-legacy-display.py); the module of each has the script's name and its description,
//! and writes the same files (tools/oneios-tool/README.md "kernel"). A failure ends as the scripts' did: the
//! message on stderr and status 1.

pub mod build_oneios_aes;
pub mod build_oneios_audio;
pub mod build_oneios_board;
pub mod build_oneios_display;
pub mod build_oneios_gpu;
pub mod build_oneios_hid;
pub mod build_oneios_hosted_storage;
pub mod build_oneios_keystore;
pub mod build_oneios_net;
pub mod build_record;
pub mod class_record;
pub mod drivers_build;
pub mod gen;
pub mod hid_layout;
pub mod hosted_fixup;
pub mod iig_fix;
pub mod kdk_headers;
pub mod layout;
pub mod macho_object;
pub mod net_layout;
pub mod port_legacy_display;
pub mod series_check;
pub mod staged;

use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
pub enum Archive {
    /// drivers/ and Droid-VM's cpufreq (liboneiosdrivers.a, liboneiosboard.a)
    Board,
    /// The disks (liboneioshostedstorage.a)
    Storage,
    /// Software AES under the collection's IOAESAccelerator (liboneiosaes.a)
    Aes,
    /// The key store (liboneioskeystore.a)
    Keystore,
    /// The Wi-Fi, en0 (liboneiosnet.a)
    Net,
    /// Touch, buttons and the light sensor (liboneioshid.a)
    Hid,
    /// The speaker, the microphone and the actuator (liboneiosaudio.a)
    Audio,
    /// virtio-gpu 3D and Metal's kernel side (liboneiosgpu.a)
    Gpu,
    /// The display (libNativeLegacyDisplay.late.a)
    Display,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Family {
    /// IOHIDDevice (IOHIDDevice-24A446-v54.json)
    Hid,
    /// IOEthernetController and IOEthernetInterface (IONetworking-24A446-v54.json)
    Net,
}

#[derive(Subcommand)]
pub enum Command {
    /// One board archive of the kernel build (run by UniXNU's driver in the build tree, UNIXNU_DEVICE set)
    Gen {
        #[arg(value_enum)]
        name: Archive,
    },
    /// The record of a finished kernel build, and its place in out/kernel
    BuildRecord(build_record::Args),
    /// The record of the original classes of the kernelcache that the HID or the Wi-Fi driver subclasses
    Layout {
        #[arg(value_enum)]
        which: Family,
        #[command(flatten)]
        args: layout::Args,
    },
    /// The display's record: Droid-VM's reviewed iPad layout carried over to this kernelcache and checked
    PortLegacyDisplay(port_legacy_display::Args),
    /// drivers/ of the repository into one archive for the kernel link (was drivers/build.py)
    Drivers(drivers_build::Args),
    /// Post-link fixes of the hosted kernel's text, in place: mrs xN, MPIDR_EL1 -> ldr wN, [x18]
    HostedFixup { kernel: PathBuf },
    /// A headers-only KDKROOT for xnu-12377 from a Kernel Debug Kit
    KdkHeaders(kdk_headers::Args),
    /// Fix a copy of xnu-linux-toolchain's iig for xnu-12377's .iig files, in place
    IigFix { path: PathBuf },
    /// Checks of the kernel's series and profiles that need no sources (exit 1 with a line per failed check)
    SeriesCheck {
        /// This repository's checkout (default: the current directory)
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// The compile command the board archives are built with, from a compile record (a JSON list)
    CompileCommand {
        /// A compile record of the kernel build (build/xnu/ios-obj/<CONFIG>/.../*.json)
        record: PathBuf,
        source: PathBuf,
        object: PathBuf,
        /// This repository's checkout (default: the current directory)
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

fn compile_command(record: PathBuf, source: PathBuf, object: PathBuf, repo: Option<PathBuf>) -> anyhow::Result<()> {
    use oneios_common::pyjson::{dumps, Value};
    let name = record.file_name().map(PathBuf::from).unwrap_or_default();
    let rec = drivers_build::record(record.parent().unwrap_or(std::path::Path::new(".")), &name.to_string_lossy())?;
    let here = drivers_build::here(&repo.unwrap_or_else(|| PathBuf::from(".")))?;
    let mut list = Value::list();
    drivers_build::command(&here, &rec, &source, &object).into_iter().for_each(|part| list.push(part));
    println!("{}", dumps(&list, None, false));
    Ok(())
}

pub fn main(command: Command) -> i32 {
    let result = match command {
        Command::Gen { name } => match name {
            Archive::Board => build_oneios_board::main(),
            Archive::Storage => build_oneios_hosted_storage::main(),
            Archive::Aes => build_oneios_aes::main(),
            Archive::Keystore => build_oneios_keystore::main(),
            Archive::Net => build_oneios_net::main(),
            Archive::Hid => build_oneios_hid::main(),
            Archive::Audio => build_oneios_audio::main(),
            Archive::Gpu => build_oneios_gpu::main(),
            Archive::Display => build_oneios_display::main(),
        }.map(|()| 0),
        Command::BuildRecord(args) => build_record::main(args),
        Command::Layout { which: Family::Hid, args } => hid_layout::main(args).map(|()| 0),
        Command::Layout { which: Family::Net, args } => net_layout::main(args).map(|()| 0),
        Command::PortLegacyDisplay(args) => port_legacy_display::main(args).map(|()| 0),
        Command::CompileCommand { record, source, object, repo } => compile_command(record, source, object, repo).map(|()| 0),
        Command::Drivers(args) => drivers_build::main(args).map(|()| 0),
        Command::HostedFixup { kernel } => hosted_fixup::main(&kernel).map(|()| 0),
        Command::KdkHeaders(args) => kdk_headers::main(args).map(|()| 0),
        Command::IigFix { path } => iig_fix::main(&path).map(|()| 0),
        Command::SeriesCheck { repo } => series_check::main(repo.as_deref()),
    };
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}
