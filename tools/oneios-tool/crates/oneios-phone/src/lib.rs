//! oneios-tool phone: the phone on adb (docs/building.md section 6). Status, switch, return, stage, sums, the
//! Data store's copy. The rules every command keeps are in `adb.rs`.
//!
//! **Not run on the phone yet** (no phone on the box it was written on): tested against a stand-in phone,
//! `tests/phone/fake-adb.sh`, which runs the fork's own root script under its test root.

pub mod adb;
pub mod app;
pub mod phone;
pub mod sparse;

use adb::Adb;

#[derive(clap::Args)]
pub struct Cli {
    /// The phone's adb serial (or ANDROID_SERIAL); needed only when several are attached
    #[arg(long, short = 's', global = true)]
    serial: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Root, the installed app, the session, and what `oneios_root.sh status` says (read only)
    Status,
    /// Start a session: `oneios_root.sh switch <the app's versionCode> 1`
    Switch {
        /// Instead of the installed app's versionCode
        #[arg(long)]
        version_code: Option<u64>,
    },
    /// End the session (session/return.request) and wait for One UI's restore line
    Return {
        /// Seconds to wait for the restore line
        #[arg(long, default_value_t = 180)]
        timeout: u64,
        /// The line written into return.request (the return logs it)
        #[arg(long, default_value = "oneios-tool: return")]
        note: String,
    },
    /// Stage files as the app does (scripts/phone/stage.sh): runtime DIR, runtime-asis DIR, image DIR
    Stage {
        /// runtime, runtime-asis or image
        #[arg(value_parser = ["runtime", "runtime-asis", "image"])]
        kind: String,
        /// An unpacked runtime package, or files of an image set at their paths
        dir: String,
    },
    /// The sha256 of the staged kernel.kc, its hash list, manifest.json, data.seed's line, xnuhost.conf and the runtime
    Sums,
    /// Copy the Data store (data.img) from the phone, zero runs as holes; no session may be open
    PullData {
        out: String,
        /// Then `ioslog boots` and `ioslog checks` on the copy
        #[arg(long)]
        ioslog: bool,
    },
    /// stdin to OUT with zero runs as holes (no phone needed)
    SparseWrite {
        out: String,
        /// stdin is gzip
        #[arg(long)]
        gunzip: bool,
        /// Fail unless this many bytes came
        #[arg(long)]
        expect_size: Option<u64>,
    },
}

/// Run a parsed command; the exit status.
pub fn main(cli: Cli) -> i32 {
    let serial = cli.serial.as_deref();
    let result = match cli.command {
        Command::SparseWrite { out, gunzip, expect_size } => phone::sparse_write(&out, gunzip, expect_size),
        Command::Status => Adb::connect(serial).and_then(|adb| phone::status(&adb)),
        Command::Switch { version_code } => Adb::connect(serial).and_then(|adb| phone::switch(&adb, version_code).map(|_| ())),
        Command::Return { timeout, note } => Adb::connect(serial).and_then(|adb| phone::do_return(&adb, &note, timeout)),
        Command::Stage { kind, dir } => Adb::connect(serial).and_then(|adb| phone::stage(&adb, &kind, &dir)),
        Command::Sums => Adb::connect(serial).and_then(|adb| phone::sums_text(&adb).map(|text| print!("{text}"))),
        Command::PullData { out, ioslog } => Adb::connect(serial).and_then(|adb| phone::pull_data(&adb, &out, ioslog)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("oneios-tool phone: {e:#}");
            1
        }
    }
}

#[derive(clap::Args)]
pub struct AppCli {
    /// The OneOS checkout (else ONEOS_DIR, the current folder, OneOS beside the OneiOS checkout)
    #[arg(long, global = true)]
    oneos: Option<String>,
    /// The phone's adb serial (or ANDROID_SERIAL)
    #[arg(long, short = 's', global = true)]
    serial: Option<String>,
    #[command(subcommand)]
    command: AppCommand,
}

#[derive(clap::Subcommand)]
pub enum AppCommand {
    /// versionName, versionCode, applicationId, the commit, and the installed app's version and certificate
    Version {
        /// Do not ask a phone
        #[arg(long)]
        no_phone: bool,
    },
    /// cd app-android && ./gradlew test assembleFullDebug; then the APK's path, size, sha256 and version
    Build {
        /// Without the unit tests
        #[arg(long)]
        no_test: bool,
    },
    /// The build's prerequisites: JDK 21, an Android SDK with platform 37, the GitHub Packages login
    Doctor,
    /// adb install -r of the built APK, of APK, or of the CI prerelease oneios-VERSION; refuses another certificate
    Install {
        /// An APK file (default: the checkout's build output)
        apk: Option<String>,
        /// The CI prerelease oneios-VERSION, downloaded with gh
        #[arg(long)]
        release: Option<String>,
        /// The GitHub repository of --release
        #[arg(long, default_value = "akshaynexus/OneOS")]
        repo: String,
        /// Where --release puts the download
        #[arg(long, default_value = "out/app")]
        dir: String,
    },
    /// The fork's oneios_switch.sh, oneios_return.sh and oneios_root.sh into the stage, with their sha256
    StageScripts,
}

/// Run a parsed `app` command; the exit status.
pub fn app_main(cli: AppCli) -> i32 {
    let (oneos, serial) = (cli.oneos.as_deref(), cli.serial.as_deref());
    let fork = || app::find_fork(oneos);
    let result = match cli.command {
        AppCommand::Version { no_phone } => fork().and_then(|f| app::version(&f, serial, no_phone)),
        AppCommand::Build { no_test } => fork().and_then(|f| app::build(&f, no_test)),
        AppCommand::Doctor => fork().and_then(|f| app::doctor(&f)),
        AppCommand::Install { apk, release, repo, dir } => {
            // the checkout is needed only for its build output
            let f = if apk.is_none() && release.is_none() { fork().map(Some) } else { Ok(None) };
            f.and_then(|f| app::install(f.as_ref(), serial, apk.as_deref(), release.as_deref(), &repo, std::path::Path::new(&dir)))
        }
        AppCommand::StageScripts => fork().and_then(|f| app::stage_scripts(&f, serial)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("oneios-tool app: {e:#}");
            1
        }
    }
}
