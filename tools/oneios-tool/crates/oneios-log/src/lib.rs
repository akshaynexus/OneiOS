//! oneios-tool log: read an xnuhost session log (`/data/local/tmp/oneios/session/xnuhost.log`, the formats of
//! `runtime/xnuhost/src/log.c` and of the hosted kernel's report lines).

pub mod diff;
pub mod parse;
pub mod reports;
pub mod stacks;
pub mod table;
pub mod timeline;

use anyhow::{anyhow, bail, Result};
use oneios_sym::Locate;
use std::io::Write;
use std::path::{Path, PathBuf};
use table::Format;

const USAGE: &str = "\
usage: oneios-tool log stacks LOG [--proc NAME] [--thread 0xT] [--slide HEX] [--no-sym] [--summary]
                                  [--dsc DIR] [--index DIR]
       oneios-tool log timeline LOG
       oneios-tool log diff LOG_A LOG_B [--words N] [--all]
       oneios-tool log gpu LOG [--json | --csv]
       oneios-tool log cpu LOG [--json | --csv]
       oneios-tool log display LOG [--json | --csv]
       oneios-tool log perf LOG [--json | --csv]

LOG is an xnuhost session log (xnuhost.log). A line cut by another writer is split into its records; a cut
record gives the fields it still has.
  stacks    the user backtraces (oneios-ubt samples, sheng-exc exceptions) per thread over time, named in the dyld
            shared cache through `oneios-tool sym`'s index (the slide is found per boot); the threads that never
            change are listed at the end. --no-sym prints the addresses only.
  timeline  the milestones of every boot with time and line: start, kernel, launchd, frames, gestures and whether
            the guest drew after them, crashes, the end
  diff      the kinds of lines one log has and the other does not, with counts and first lines
  gpu, cpu, display, perf   tables over time from the periodic report lines (--json, --csv for machines)";

/// `oneios-tool log`'s subcommands (clap). Every subcommand takes the same options, as before clap: one that a
/// subcommand has no use for is read and ignored.
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// The user backtraces per thread over time, named through `oneios-tool sym`'s index
    Stacks(Opts),
    /// The milestones of every boot with time and line
    Timeline(Opts),
    /// The kinds of lines one log has and the other does not (two logs)
    Diff(Opts),
    /// The GPU report lines as tables over time
    Gpu(Opts),
    /// The CPU report lines as tables over time
    Cpu(Opts),
    /// The display report lines as tables over time
    Display(Opts),
    /// The performance report lines as tables over time
    Perf(Opts),
}

#[derive(clap::Args, Debug)]
pub struct Opts {
    /// The xnuhost session log (two for diff)
    #[arg(value_name = "LOG")]
    paths: Vec<PathBuf>,
    /// stacks: only this process
    #[arg(long = "proc", value_name = "NAME")]
    proc_name: Option<String>,
    /// stacks: only this thread (hex)
    #[arg(long, value_name = "0xT")]
    thread: Option<String>,
    /// stacks: the slide (hex) instead of the one found per boot
    #[arg(long, value_name = "HEX")]
    slide: Option<String>,
    /// diff: how many words of a line make its kind
    #[arg(long, value_name = "N", default_value_t = 6)]
    words: usize,
    /// stacks: the folder that holds dyld_shared_cache_arm64e
    #[arg(long, value_name = "DIR")]
    dsc: Option<PathBuf>,
    /// stacks: the folder of `oneios-tool sym`'s index
    #[arg(long, value_name = "DIR")]
    index: Option<PathBuf>,
    /// stacks: print the addresses only
    #[arg(long = "no-sym")]
    no_sym: bool,
    /// stacks: only the summary
    #[arg(long)]
    summary: bool,
    /// diff: every kind, not only the first ones
    #[arg(long)]
    all: bool,
    /// The tables as JSON
    #[arg(long, overrides_with = "csv")]
    json: bool,
    /// The tables as CSV
    #[arg(long, overrides_with = "json")]
    csv: bool,
}

#[derive(clap::Parser)]
#[command(name = "oneios-tool log", args_override_self = true, after_help = USAGE)]
struct Standalone {
    #[command(subcommand)]
    command: Command,
}

/// Run `oneios-tool log ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    match <Standalone as clap::Parser>::try_parse_from(std::iter::once("log".to_string()).chain(args.iter().cloned())) {
        Ok(parsed) => main(parsed.command),
        Err(e) => {
            let _ = e.print();
            refused(&e)
        }
    }
}

/// The exit status of a command line clap refused: 2 for no subcommand, 1 for anything else wrong, 0 for --help.
pub fn refused(e: &clap::Error) -> i32 {
    use clap::error::ErrorKind::*;
    match e.kind() {
        DisplayHelp | DisplayVersion => 0,
        DisplayHelpOnMissingArgumentOrSubcommand | MissingSubcommand => 2,
        _ => 1,
    }
}

/// Run a parsed command (oneios-tool's main parses it); the exit status.
pub fn main(command: Command) -> i32 {
    match run_command(command, Locate::from_env(), &mut std::io::stderr()) {
        Ok(text) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            // A closed pipe (`| head`) is not an error.
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
            0
        }
        Err(e) => {
            eprintln!("oneios-tool log: {e:#}");
            1
        }
    }
}

struct Args {
    paths: Vec<PathBuf>,
    proc_name: Option<String>,
    thread: Option<u64>,
    slide: Option<u64>,
    no_sym: bool,
    summary: bool,
    words: usize,
    all: bool,
    format: Format,
}

fn options(o: Opts, loc: &mut Locate) -> Result<Args> {
    let hex = |text: &str| {
        u64::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16).map_err(|_| anyhow!("not a hex number: {text:?}"))
    };
    if o.dsc.is_some() {
        loc.dsc = o.dsc;
    }
    if o.index.is_some() {
        loc.index_parent = o.index;
    }
    Ok(Args {
        paths: o.paths,
        proc_name: o.proc_name,
        thread: o.thread.as_deref().map(hex).transpose()?,
        slide: o.slide.as_deref().map(hex).transpose()?,
        no_sym: o.no_sym,
        summary: o.summary,
        words: o.words,
        all: o.all,
        format: if o.csv { Format::Csv } else if o.json { Format::Json } else { Format::Text },
    })
}

/// The command's whole output for a command line (the tests call this).
pub fn run_inner(args: &[String], loc: Locate, err: &mut dyn Write) -> Result<String> {
    let parsed = <Standalone as clap::Parser>::try_parse_from(std::iter::once("log".to_string()).chain(args.iter().cloned()))
        .map_err(|e| anyhow!("{}", e.to_string().trim_end()))?;
    run_command(parsed.command, loc, err)
}

/// The command's whole output.
fn run_command(command: Command, mut loc: Locate, err: &mut dyn Write) -> Result<String> {
    let (command, opts) = match command {
        Command::Stacks(o) => ("stacks", o),
        Command::Timeline(o) => ("timeline", o),
        Command::Diff(o) => ("diff", o),
        Command::Gpu(o) => ("gpu", o),
        Command::Cpu(o) => ("cpu", o),
        Command::Display(o) => ("display", o),
        Command::Perf(o) => ("perf", o),
    };
    let a = options(opts, &mut loc)?;
    let wanted = if command == "diff" { 2 } else { 1 };
    if a.paths.len() != wanted {
        bail!("{command} takes {wanted} log file(s), {} given\n{USAGE}", a.paths.len());
    }
    let load = |path: &Path| parse::load(path);
    match command {
        "stacks" => {
            let log = load(&a.paths[0])?;
            let index = if a.no_sym {
                None
            } else {
                match oneios_sym::open_index(&loc, err) {
                    Ok(index) => Some(index),
                    Err(e) => {
                        let _ = writeln!(err, "oneios-tool log: addresses are not named: {e:#}");
                        None
                    }
                }
            };
            let o = stacks::Options { proc_name: a.proc_name, thread: a.thread, slide: a.slide, summary_only: a.summary };
            Ok(stacks::report(&log, index.as_ref(), &o))
        }
        "timeline" => Ok(timeline::report(&load(&a.paths[0])?)),
        "diff" => {
            let (first, second) = rayon::join(|| load(&a.paths[0]), || load(&a.paths[1]));
            Ok(diff::report(&first?, &second?, a.words, a.all))
        }
        "gpu" | "cpu" | "display" | "perf" => {
            let log = load(&a.paths[0])?;
            let (tables, notes) = match command {
                "gpu" => reports::gpu(&log),
                "cpu" => reports::cpu(&log),
                "display" => reports::display(&log),
                _ => reports::perf(&log),
            };
            Ok(table::render(&tables, &notes, a.format))
        }
        other => bail!("unknown command {other:?}\n{USAGE}"),
    }
}
