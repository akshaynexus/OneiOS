//! oneios-tool phone: the phone on adb (docs/building.md section 6). Status, switch, return, stage, sums, the
//! Data store's copy. The rules every command keeps are in `adb.rs`.
//!
//! **Not run on the phone yet** (no phone on the box it was written on): tested against a stand-in phone,
//! `tests/phone/fake-adb.sh`, which runs the fork's own root script under its test root.

pub mod adb;
pub mod app;
pub mod cycle;
pub mod log;
pub mod phone;
pub mod pull;
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
    Status {
        /// Again every SECONDS (default 5) until Ctrl-C
        #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "5")]
        watch: Option<u64>,
    },
    /// The session's logs by name: print, filter, follow live, save (runs beside a session: read only)
    Log(LogArgs),
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
    /// Check that the staged manifest.json lists the staged files (image.sha256), as xnuhost will; `switch` runs it first (read only)
    ImageCheck,
    /// The phone's manifest.json with the entries of DIR's files (kernel.kc, kernel.kc.hashes, ... at their paths in the
    /// set) set to theirs; every other entry must list the staged file. Printed, written to --out, or staged with --stage
    Manifest {
        /// Files of the image set at their paths (DIR's own manifest.json is ignored)
        dir: String,
        /// Write the manifest to FILE
        #[arg(long)]
        out: Option<String>,
        /// Stage DIR's files and the manifest together (phone stage image), then check the set
        #[arg(long)]
        stage: bool,
    },
    /// Copy the Data store (data.img) from the phone in chunks, each checked against the phone's sha256, zero runs
    /// as holes; a copy that stopped goes on where it stopped (OUT.part, OUT.part.progress); no session may be open
    PullData {
        out: String,
        /// A chunk's size in MiB: one adb stream each (default 256; a copy that goes on keeps the size it began with)
        #[arg(long, value_name = "MIB")]
        chunk: Option<u64>,
        /// How many times a chunk whose stream broke or whose sum differed is copied again before the command gives up
        #[arg(long, default_value_t = 5)]
        retries: u32,
        /// Seconds before the first retry of a chunk; doubled for each further one, at most 30
        #[arg(long, value_name = "SECONDS", default_value_t = 2)]
        backoff: u64,
        /// End a stream that gave no byte for this many seconds, and try the chunk again (0: never)
        #[arg(long, value_name = "SECONDS", default_value_t = 120)]
        stall: u64,
        /// When the phone's sha256 of a chunk is asked: before the chunk is copied (a chunk of zeros is then not
        /// copied at all), or for every chunk after the last one is copied
        #[arg(long, value_enum, default_value = "chunk")]
        verify: pull::Verify,
        /// Throw away OUT.part and its progress and begin again
        #[arg(long)]
        restart: bool,
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

#[derive(clap::Args)]
pub struct LogArgs {
    /// Which log: xnuhost (default), switch, return, outcome, display, watchdog, usb, or logs/xnuhost-<epoch>.log
    #[arg(long, default_value = "xnuhost")]
    file: String,
    /// The named logs with their sizes, and the earlier sessions' (the fork's `oneios_root.sh logs`)
    #[arg(long)]
    list: bool,
    /// Only the last N lines (with --follow: start N lines before the end)
    #[arg(long)]
    tail: Option<u64>,
    /// Keep reading as the log grows (tail -F), connecting again when adb drops; Ctrl-C ends it
    #[arg(long, short = 'f')]
    follow: bool,
    /// Also write every line read (not only the ones printed) to PATH
    #[arg(long)]
    save: Option<String>,
    /// --save to out/session-logs/DATE/live-HHMMSS.log
    #[arg(long)]
    save_auto: bool,
    /// Print only lines matching REGEX (repeatable; with the named filters: any of them)
    #[arg(long)]
    grep: Vec<String>,
    /// Panics, faults, crashes, the session's end
    #[arg(long)]
    panic: bool,
    /// The boot's stages (kernel, launchd, backboardd, SpringBoard, the first frame), each once, and panics and the end
    #[arg(long)]
    markers: bool,
    #[arg(long)]
    display: bool,
    #[arg(long)]
    gpu: bool,
    #[arg(long)]
    wifi: bool,
    #[arg(long)]
    hid: bool,
    #[arg(long)]
    mem: bool,
    /// Only lines from this second of the log's own clock on (its [seconds.millis] stamps)
    #[arg(long, value_name = "SECONDS")]
    since: Option<f64>,
    /// When the reading ends: `oneios-tool log timeline` of what was read, and `log stacks --summary` when a sym index exists
    #[arg(long)]
    analyze: bool,
}

fn phone_log(serial: Option<&str>, a: LogArgs) -> anyhow::Result<()> {
    let adb = Adb::connect(serial)?;
    if a.list {
        return log::list(&adb);
    }
    let named: Vec<&str> = [("panic", a.panic), ("display", a.display), ("gpu", a.gpu), ("wifi", a.wifi), ("hid", a.hid), ("mem", a.mem)].iter().filter(|(_, on)| *on).map(|(name, _)| *name).collect();
    let filter = log::Filter::new(&a.grep, &named, a.markers, a.since)?;
    let save = match (a.save, a.save_auto || a.analyze) {
        (Some(path), _) => Some(path),
        (None, true) => Some(log::auto_save_path(&cycle::repo_root().unwrap_or_else(|_| std::path::PathBuf::from(".")))),
        (None, false) => None,
    };
    log::log(&adb, &log::Options { file: a.file, tail: a.tail, follow: a.follow, save, analyze: a.analyze }, filter)
}

/// Run a parsed command; the exit status.
pub fn main(cli: Cli) -> i32 {
    let serial = cli.serial.as_deref();
    let result = match cli.command {
        Command::SparseWrite { out, gunzip, expect_size } => phone::sparse_write(&out, gunzip, expect_size),
        Command::Status { watch: None } => Adb::connect(serial).and_then(|adb| phone::status(&adb)),
        Command::Status { watch: Some(seconds) } => Adb::connect(serial).and_then(|adb| {
            log::catch_stop();
            loop {
                let (date, time) = cycle::local_date_time();
                println!("--- {date} {}:{}:{}", &time[..2], &time[2..4], &time[4..]);
                // a look that fails (adb dropping while the framework stops) is said, and the watch goes on
                if let Err(e) = phone::status(&adb) {
                    println!("status: {e:#}");
                }
                if log::sleep_or_stop(std::time::Duration::from_secs(seconds.max(1))) {
                    return Ok(());
                }
            }
        }),
        Command::Log(args) => phone_log(serial, args),
        Command::Switch { version_code } => Adb::connect(serial).and_then(|adb| phone::switch(&adb, version_code).map(|_| ())),
        Command::Return { timeout, note } => Adb::connect(serial).and_then(|adb| phone::do_return(&adb, &note, timeout)),
        Command::Stage { kind, dir } => Adb::connect(serial).and_then(|adb| phone::stage(&adb, &kind, &dir)),
        Command::Sums => Adb::connect(serial).and_then(|adb| phone::sums_text(&adb).map(|text| print!("{text}"))),
        Command::ImageCheck => Adb::connect(serial).and_then(|adb| {
            adb.need_root()?;
            phone::image_check(&adb).map(|n| println!("image: manifest.json and the {n} files it lists agree"))
        }),
        Command::Manifest { dir, out, stage } => Adb::connect(serial).and_then(|adb| phone::manifest_cmd(&adb, &dir, out.as_deref(), stage)),
        Command::PullData { out, chunk, retries, backoff, stall, verify, restart, ioslog } => Adb::connect(serial).and_then(|adb| {
            // Ctrl-C ends the chunk that is being copied and says how to go on; the chunks done stay
            log::catch_stop();
            pull::pull_data(&adb, &out, &pull::Options { chunk, retries, backoff, stall, verify, restart, ioslog })
        }),
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
