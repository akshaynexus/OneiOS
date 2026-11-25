//! oneios-tool: OneiOS's host tools. `oneios-tool TOOL ARGS...`. One clap layout: each tool's crate defines its
//! own arguments (clap derive) and this file puts them under one command, so `oneios-tool --help` and
//! `oneios-tool TOOL --help` list everything.
//!
//! A wrong command line ends as the tool it names always ended: `image` prints its usage and exits 1 (as
//! imagetool.py does), `sym` exits 1 (as dsc.py does), `log` and `prelink` exit 2 for no or an unknown subcommand
//! and 1 for wrong arguments, everything else exits 2.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "oneios-tool", about = "OneiOS's host tools (tools/oneios-tool/README.md)", args_override_self = true)]
struct Top {
    #[command(subcommand)]
    tool: Tool,
}

#[derive(Subcommand)]
enum Tool {
    /// Name user addresses of a boot log in the image's dyld shared cache (scripts/debug/dsc.py); `sym kernel`
    /// names the kernel addresses of a panic in kernel.kc and xnu.macho (`oneios-tool sym kernel --help`)
    Sym(oneios_sym::Cli),
    /// Read an xnuhost session log: stacks, timeline, diff, gpu, cpu, display, perf
    Log {
        #[command(subcommand)]
        command: oneios_log::Command,
    },
    /// The kernel collection's steps (kernel/prelink/build.sh): roots, hosted
    Prelink {
        #[command(subcommand)]
        command: oneios_prelink::Command,
    },
    /// The kernel build's own steps: gen (the board archives), build-record
    Kernel {
        #[command(subcommand)]
        command: oneios_kernel::Command,
    },
    /// The image tool (scripts/image/imagetool.py), and apfs-ls, apfs-copy
    Image {
        #[command(subcommand)]
        command: oneios_image::Command,
    },
    /// iOS's unified log from a copy of the phone's Data store (no mount), a .logarchive or a mounted volume
    Ioslog {
        #[command(subcommand)]
        command: oneios_ioslog::Command,
    },
    /// The phone on adb: status, switch, return, stage, sums, log, pull-data, sparse-write
    Phone(oneios_phone::Cli),
    /// The OneOS fork's app: version, build, doctor, install, stage-scripts
    App(oneios_phone::AppCli),
    /// One test cycle on the phone: build, tests, stage, switch, watch, hold, return, collect
    Cycle(oneios_phone::cycle::Args),
    /// Single members of a ZIP (an IPSW) without downloading it: ls, cat, get, head (scripts/fetch/remotezip.py)
    Remotezip {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Serve the files of one folder on 127.0.0.1 with byte ranges (scripts/serve/serve-image.sh)
    Rangeserve {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The runtime package (runtime/package.sh): check (no Apple file may go in), manifest, archive
    Package {
        #[command(subcommand)]
        command: oneios_package::Command,
    },
    /// One process for the OneOS app, started as root once per flow: a request per line on standard input, JSON
    /// lines back (README "serve": the protocol). --request JSON runs requests from the command line
    Serve(oneios_serve::Cli),
    /// The helpers of the comparison scripts (tools/oneios-tool/tests): kcmap, ksym-make-log, ksym-compare,
    /// kernel-diff, mkstream
    Debug {
        #[command(subcommand)]
        command: oneios_testkit::Command,
    },
    /// A JSON file read or changed from a shell script: get, lines, len, set, check (README "json")
    Json {
        #[command(subcommand)]
        command: oneios_json::Command,
    },
    /// The iOS user-space toolchain's steps: sign, codesign, macho-check, tbds, and the Metal build's (README "ios")
    Ios {
        #[command(subcommand)]
        command: oneios_ios::Command,
    },
    /// xnuhost's switch catalog (docs/switches.md) for the OneOS app's Switches screen; --json, --runtime DIR
    /// (README "switches")
    Switches(oneios_conf::SwitchesCli),
    /// An image's xnuhost.conf: show, check, set, unset, write, restore (README "conf")
    Conf {
        #[command(subcommand)]
        command: oneios_conf::Command,
    },
}

/// `oneios-tool [--jobs N] [--nice N] TOOL ...`: the two options every command that runs alone takes, before the
/// tool's name (`serve` has its own `--jobs` and `--nice`: README "serve"). `--jobs N` (1 to 8) is the number of
/// threads of the work that splits (the hash lists, the APFS copy, the prelink's kexts, the range requests of a
/// fetch that names no `-j`); without it the commands keep the threads they always had (every CPU). `--nice N` (10
/// to 19) puts the process there and out of any realtime class; on Android that is done without the option, at 10.
/// The words are taken off the command line; the exit status of a wrong one is 2.
fn own_options(args: &mut Vec<String>) -> Result<(), String> {
    let mut nice: Option<i32> = cfg!(target_os = "android").then_some(10);
    while let Some(option @ ("--jobs" | "--nice")) = args.get(1).map(String::as_str) {
        let option = option.to_string();
        let value = args.get(2).cloned().ok_or(format!("{option} needs a number"))?;
        match option.as_str() {
            "--jobs" => {
                let n: usize = value.parse().ok().filter(|n| (1..=oneios_common::job::JOBS_MAX).contains(n)).ok_or(format!("--jobs {value}: 1 to {}", oneios_common::job::JOBS_MAX))?;
                oneios_common::job::set_jobs(n);
            }
            _ => nice = Some(value.parse().map_err(|_| format!("--nice {value}: 10 to 19"))?),
        }
        args.drain(1..3);
    }
    // `serve` sets its own, with its own messages
    if args.get(1).map(String::as_str) == Some("serve") {
        return Ok(());
    }
    if let Some(n) = nice {
        // (before the first thread starts: a nice value is a thread's, and a new thread takes its maker's)
        oneios_common::job::set_nice(n).map_err(|e| format!("--nice {n}: {e}"))?;
    }
    if oneios_common::job::jobs_given() {
        rayon::ThreadPoolBuilder::new().num_threads(oneios_common::job::jobs()).build_global().map_err(|e| format!("--jobs: {e}"))?;
    }
    Ok(())
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    if let Err(text) = own_options(&mut args) {
        eprintln!("oneios-tool: {text}");
        std::process::exit(2);
    }
    // `sym kernel` has its own command line (oneios-ksym): `sym`'s words may start with a hyphen (an address may be
    // negative, as in dsc.py), so clap would hand every option after `kernel` over as a word.
    if args.get(1).map(String::as_str) == Some("sym") && args.get(2).map(String::as_str) == Some("kernel") {
        std::process::exit(oneios_ksym::run(&args[3..]));
    }
    // `image tweaks` (the root tweaks, oneios-tweaks) has its own command line, beside imagetool.py's commands
    if args.get(1).map(String::as_str) == Some("image") && args.get(2).map(String::as_str) == Some("tweaks") {
        std::process::exit(oneios_tweaks::run(&args[3..]));
    }
    // `image a56-gpu-place` (oneios-imagebuild a56gpu): the Galaxy A56's GPU plug-ins for a staged iOS 16.5 set, for the
    // app's root script at Update the kernel (docs/a56-gpu.md 6)
    if args.get(1).map(String::as_str) == Some("image") && args.get(2).map(String::as_str) == Some("a56-gpu-place") {
        std::process::exit(oneios_imagebuild::a56gpu::place_cmd(&args[3..]));
    }
    // `image build` (oneios-imagebuild: build.sh's steps in one process) is not one of imagetool.py's commands,
    // whose command line `image` keeps as it was.
    if args.get(1).map(String::as_str) == Some("image") && args.get(2).map(String::as_str) == Some("build") {
        std::process::exit(oneios_imagebuild::main(&args[3..]));
    }
    let status = match Top::try_parse_from(&args) {
        Ok(top) => match top.tool {
            Tool::Sym(cli) => oneios_sym::main(cli),
            Tool::Log { command } => oneios_log::main(command),
            Tool::Prelink { command } => oneios_prelink::main(command),
            Tool::Kernel { command } => oneios_kernel::main(command),
            Tool::Image { command } => oneios_image::main(command),
            Tool::Ioslog { command } => oneios_ioslog::main(command),
            Tool::Phone(cli) => oneios_phone::main(cli),
            Tool::App(cli) => oneios_phone::app_main(cli),
            Tool::Cycle(args) => oneios_phone::cycle::main(args),
            Tool::Remotezip { args } => oneios_image::remotezip::main(&args),
            Tool::Rangeserve { args } => oneios_image::rangeserve::main(&args),
            Tool::Package { command } => oneios_package::main(command),
            Tool::Serve(cli) => oneios_serve::main(cli),
            Tool::Debug { command } => oneios_testkit::main(command),
            Tool::Json { command } => oneios_json::main(command),
            Tool::Ios { command } => oneios_ios::main(command),
            Tool::Switches(cli) => oneios_conf::switches_main(cli),
            Tool::Conf { command } => oneios_conf::main(command),
        },
        Err(e) if !e.use_stderr() => {
            let _ = e.print();
            0
        }
        Err(e) => match args.get(1).map(String::as_str) {
            Some("image") => {
                eprintln!("{}", oneios_image::usage());
                1
            }
            Some("sym") => {
                eprintln!("oneios-tool sym: {}", e.to_string().trim_end());
                1
            }
            Some("log") => {
                let _ = e.print();
                oneios_log::refused(&e)
            }
            Some("prelink") => {
                let _ = e.print();
                oneios_prelink::refused(&e)
            }
            // (README "serve": 3 for a wrong command line of `serve` itself)
            Some("serve") => {
                let _ = e.print();
                3
            }
            _ => {
                let _ = e.print();
                2
            }
        },
    };
    std::process::exit(status);
}
