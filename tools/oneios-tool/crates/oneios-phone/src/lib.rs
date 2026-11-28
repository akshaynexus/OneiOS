//! oneios-tool phone: the phone on adb (docs/building.md section 6). Status, switch, return, stage, sums, the
//! Data store's copy, and the session's own commands (`session.rs`: the display helper's restart, the touch IC, the
//! panel's level). The rules every command keeps are in `adb.rs`.
//!
//! **Not run on the phone yet** (no phone on the box it was written on): tested against a stand-in phone,
//! `tests/phone/fake-adb.sh`, which runs the fork's own root script under its test root.

pub mod adb;
pub mod app;
pub mod cpu;
pub mod cycle;
pub mod hw;
pub mod host;
pub mod kb;
pub mod local;
pub mod log;
pub mod phone;
pub mod procs;
pub mod pull;
pub mod pulllog;
pub mod qrtr;
pub mod session;
pub mod sparse;
pub mod wifi;

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
    /// Root, the host phone (the table entry xnuhost will pick), the installed app, the session, and what
    /// `oneios_root.sh status` says (read only)
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
    /// Only during a session: end the display helper (control/display.pid: SIGTERM, then SIGKILL) and start it again
    /// with the switch script's own command; waits for its first frame in control/display.log. xnuhost is not touched
    DisplayRestart {
        /// First put this liboneios-display.so into the staged runtime (runtime/bin/), its SHA-256 checked on the phone
        #[arg(long, value_name = "PATH")]
        lib: Option<String>,
        /// First put this oneios-display.dex into the staged runtime (runtime/lib/), its SHA-256 checked on the phone
        #[arg(long, value_name = "PATH")]
        dex: Option<String>,
        /// Seconds to wait for the new helper's `first frame posted`
        #[arg(long, value_name = "SECONDS", default_value_t = 30)]
        timeout: u64,
        /// Seconds between the old helper's SIGTERM and its SIGKILL
        #[arg(long, value_name = "SECONDS", default_value_t = 3)]
        kill_after: u64,
    },
    /// Only during a session: the touch IC's power state (/sys/class/sec/tsp/enabled)
    Touch {
        #[command(subcommand)]
        command: TouchCommand,
    },
    /// Only during a session: the S Pen's IC power state (/sys/class/sec/sec_epen/enabled; docs/hardware.md 7f)
    Pen {
        #[command(subcommand)]
        command: TouchCommand,
    },
    /// Where iOS's CPU work can run, read only: each CPU's core, frequency domain, top and capped clocks, governor and
    /// boost; Android's cpusets; in a session xnuhost's threads (CPU, nice, cpuset, uclamp) and the iOS threads by CPU
    Cpu {
        /// Also record a Perfetto trace (sched, freq, idle) of SECONDS and copy it here
        #[arg(long, value_name = "SECONDS")]
        trace: Option<u64>,
        /// Where the trace goes (default oneios-cpu.pftrace)
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// The hardware as the phone's kernel was given it, read only, the first facts of a new port: the device tree
    /// (/sys/firmware/fdt), /proc's cmdline, cpuinfo, meminfo, iomem, partitions, interrupts, the CPUs' MIDR, the
    /// panel's connector, the partitions' names and the kernel's configuration, into DIR. Named files only; refused
    /// during a session
    Hw {
        /// The folder for the files (fdt.dtb, iomem.txt, ..., hw.txt)
        #[arg(long, value_name = "DIR", default_value = "phone-hw")]
        out: String,
    },
    /// The staged runtime's wifictl as root beside One UI (docs/hardware.md 4a): `p2p` (read only: every radio, Google's
    /// Wonder radio, the verdicts for AirDrop) and `wonder` (a session on Wonder's raw 802.11 path beside wlan0, AWDL's
    /// frames counted, everything closed after it and wlan0's link checked)
    Wifi {
        #[command(subcommand)]
        command: WifiCommand,
    },
    /// Whether a root process beside One UI reaches the sensor hub (SSC) over Linux's QRTR, read only: the runtime's
    /// `sensorctl qrtr` lists the QRTR nodes and services and asks the SNS service (400) one lookup; `--sensors` also
    /// each data type's SUIDs and attributes (runtime/bridges/sensors/README.md). Settles whether sensors=auto can work
    Qrtr {
        /// Also every sensor the hub has (lookups and attributes only, no stream)
        #[arg(long)]
        sensors: bool,
        /// This machine's sensorctl (STAGE/bin/sensorctl of runtime/build.sh), pushed to /data/local/tmp, run and
        /// removed, for a phone whose staged runtime has none
        #[arg(long, value_name = "FILE")]
        binary: Option<String>,
        /// Also write what the phone printed to FILE
        #[arg(long, value_name = "FILE")]
        save: Option<String>,
    },
    /// Every process with its CPU share and RAM, read only, with or without a session: the session's own processes,
    /// the fork's watchers, the kernel's threads, and each Android or vendor process that still runs beside iOS (its
    /// init service, and which bridge needs it). docs/plan-ios-first-session.md
    Procs {
        /// Seconds between the two CPU readings (0: one reading, no CPU figures)
        #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(0..=600))]
        seconds: u64,
        /// The order of the Android and vendor processes
        #[arg(long, value_enum, default_value = "cpu")]
        sort: procs::Sort,
        /// Only the first N Android and vendor processes (0: all)
        #[arg(long, value_name = "N", default_value_t = 0)]
        top: usize,
        /// List every kernel thread, not only the busy ones
        #[arg(long)]
        kernel: bool,
        /// Print what the phone gave, as it came (keep it: --from reads it again)
        #[arg(long)]
        raw: bool,
        /// Read FILE (the output of --raw) and not a phone
        #[arg(long, value_name = "FILE")]
        from: Option<String>,
    },
    /// Only during a session: the panel's level (/sys/class/backlight/panel0-backlight/brightness)
    Backlight {
        #[command(subcommand)]
        command: BacklightCommand,
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
    /// Stage a boot addon (`ios addon`'s: the Metal plug-in, the capture plug-in) as image/kernel.kc.gpuaddon with the
    /// phone's owner-trust.json plus its CDHashes and a manifest for both: the kernel writes it into the Data store at the
    /// next boot (hosted 0031, 0090), no new seed. --remove: no addon at the next boot
    Addon {
        /// The addon (NGPUADD2)
        #[arg(required_unless_present = "remove")]
        file: Option<String>,
        /// Its CDHashes, `CDHASH NAME` lines (default: FILE.cdhashes)
        #[arg(long)]
        cdhashes: Option<String>,
        /// Take the staged addon away (owner-trust.json is left as it is)
        #[arg(long, conflicts_with_all = ["file", "cdhashes"])]
        remove: bool,
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
        /// Then `ioslog boots`, `ioslog checks` and `ioslog health` on the copy (the health report: ioslog-health.txt beside OUT)
        #[arg(long)]
        ioslog: bool,
    },
    /// iOS's own log of one boot without copying the Data store: the runtime's oneios-tool reads the staged store on
    /// the phone (`ioslog show`), only the gzipped text crosses adb, then `ioslog health`, `ioslog keyboard` and
    /// `ioslog haptics` here (OUT_DIR/ioslog.txt, ioslog-health.txt, ioslog-keyboard.txt, ioslog-haptics.txt). Works beside an
    /// open session (its newest checkpoint)
    PullLog {
        out_dir: String,
        /// The boot: last, or the start of a boot UUID (`ioslog boots`)
        #[arg(long, default_value = "last")]
        boot: String,
        /// Keep the gzipped text on the phone (STAGE/logs/pull-log.txt.gz)
        #[arg(long)]
        keep: bool,
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

#[derive(clap::Subcommand)]
pub enum TouchCommand {
    /// Read the node: 1 is on, 0 is off (the IC sleeps after iOS's brightness was 0)
    Status,
    /// Write `2,0` then `2,1` (what One UI's input HAL writes when the panel comes on), then read the node back
    Wake,
}

#[derive(clap::Subcommand)]
pub enum WifiCommand {
    /// `wifictl p2p`: every radio and interface, the Wonder block, the verdicts, Wonder's files (read only), then a summary
    P2p {
        /// The station whose radio the verdicts are for
        #[arg(long, default_value = "wlan0")]
        station: String,
        /// Also write the phone's report to FILE
        #[arg(long, value_name = "FILE")]
        save: Option<String>,
    },
    /// `wifictl wonder`: a monitor interface on Wonder's radio, its frames counted for SECONDS, then all of it closed
    Wonder {
        /// How long frames are counted (1 to 600)
        #[arg(long, default_value_t = 30)]
        seconds: u32,
        /// The AWDL channel: 2437 (6), 5220 (44) or 5745 (149)
        #[arg(long, default_value_t = 2437)]
        freq: u32,
        /// The phone's country (two letters), only when the driver's is not known: wifictl refuses one that differs
        #[arg(long, value_name = "CC")]
        country: Option<String>,
        /// Also write what the phone printed to FILE
        #[arg(long, value_name = "FILE")]
        save: Option<String>,
    },
}

#[derive(clap::Subcommand)]
pub enum BacklightCommand {
    /// The level, max_brightness, and One UI's own value that the return writes back (read only)
    Status,
    /// Write LEVEL (1 to max_brightness: 12750 on the S24 Ultra); the return writes One UI's own value back
    Set { level: u32 },
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
    /// The S Pen: xnuhost's `pen:` lines, the guest's pen device and HID service (docs/hardware.md 7f)
    #[arg(long)]
    pen: bool,
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
    let named: Vec<&str> = [("panic", a.panic), ("display", a.display), ("gpu", a.gpu), ("wifi", a.wifi), ("hid", a.hid), ("mem", a.mem), ("pen", a.pen)].iter().filter(|(_, on)| *on).map(|(name, _)| *name).collect();
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
        Command::DisplayRestart { lib, dex, timeout, kill_after } => {
            Adb::connect(serial).and_then(|adb| session::display_restart(&adb, &session::Restart { lib, dex, timeout, kill_after }))
        }
        Command::Touch { command: TouchCommand::Status } => Adb::connect(serial).and_then(|adb| session::touch_status(&adb)),
        Command::Touch { command: TouchCommand::Wake } => Adb::connect(serial).and_then(|adb| session::touch_wake(&adb)),
        Command::Pen { command: TouchCommand::Status } => Adb::connect(serial).and_then(|adb| session::pen_status(&adb)),
        Command::Pen { command: TouchCommand::Wake } => Adb::connect(serial).and_then(|adb| session::pen_wake(&adb)),
        Command::Cpu { trace, out } => Adb::connect(serial).and_then(|adb| cpu::cpu(&adb, trace, out.as_deref())),
        Command::Hw { out } => Adb::connect(serial).and_then(|adb| hw::hw(&adb, &out)),
        Command::Wifi { command: WifiCommand::P2p { station, save } } => Adb::connect(serial).and_then(|adb| wifi::p2p(&adb, &station, &save)),
        Command::Wifi { command: WifiCommand::Wonder { seconds, freq, country, save } } => {
            Adb::connect(serial).and_then(|adb| wifi::wonder(&adb, &wifi::Wonder { seconds, freq, country, save }))
        }
        Command::Qrtr { sensors, binary, save } => Adb::connect(serial).and_then(|adb| qrtr::qrtr(&adb, sensors, binary.as_deref(), &save)),
        Command::Procs { seconds, sort, top, kernel, raw, from } => {
            let o = procs::Options { seconds, sort, top, kernel, raw, from };
            if o.from.is_some() {
                procs::procs(None, &o)
            } else {
                Adb::connect(serial).and_then(|adb| procs::procs(Some(&adb), &o))
            }
        }
        Command::Backlight { command: BacklightCommand::Status } => Adb::connect(serial).and_then(|adb| session::backlight_status(&adb)),
        Command::Backlight { command: BacklightCommand::Set { level } } => Adb::connect(serial).and_then(|adb| session::backlight_set(&adb, level)),
        Command::Stage { kind, dir } => Adb::connect(serial).and_then(|adb| phone::stage(&adb, &kind, &dir)),
        Command::Sums => Adb::connect(serial).and_then(|adb| phone::sums_text(&adb).map(|text| print!("{text}"))),
        Command::ImageCheck => Adb::connect(serial).and_then(|adb| {
            adb.need_root()?;
            phone::image_check(&adb).map(|n| println!("image: manifest.json and the {n} files it lists agree"))
        }),
        Command::Manifest { dir, out, stage } => Adb::connect(serial).and_then(|adb| phone::manifest_cmd(&adb, &dir, out.as_deref(), stage)),
        Command::Addon { file, cdhashes, remove } => Adb::connect(serial).and_then(|adb| match (remove, file) {
            (true, _) => phone::addon_remove(&adb),
            (false, Some(f)) => phone::addon_cmd(&adb, &f, cdhashes.as_deref()),
            (false, None) => Err(anyhow::anyhow!("phone addon FILE, or phone addon --remove")),
        }),
        Command::PullLog { out_dir, boot, keep } => Adb::connect(serial).and_then(|adb| pulllog::pull_log(&adb, &out_dir, &boot, keep)),
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
    /// The OneOS checkout (else ONEOS_DIR, the current folder, app/ of the OneiOS checkout, OneOS beside it)
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
    /// The checkout's path alone, as every app command and the cycle find it (for scripts)
    Dir,
    /// One iOS build's pairing files (assets/kb/ for 24A446, assets/kb-BUILD/ for another) made from this checkout's
    /// records; --check compares only (exit 1 when they differ)
    Kb {
        /// The Apple build (24A446, 24B156)
        build: String,
        /// Compare, write nothing
        #[arg(long)]
        check: bool,
        /// This OneiOS checkout (default: the current folder)
        #[arg(long, default_value = ".")]
        repo: String,
    },
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
        AppCommand::Dir => fork().map(|f| println!("{}", f.dir.display())),
        AppCommand::Kb { build, check, repo } => fork().and_then(|f| kb::run(std::path::Path::new(&repo), &f.dir, &build, check)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("oneios-tool app: {e:#}");
            1
        }
    }
}
