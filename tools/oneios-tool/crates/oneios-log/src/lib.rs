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

/// Run `oneios-tool log ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return if args.is_empty() { 2 } else { 0 };
    }
    match run_inner(args, Locate::from_env(), &mut std::io::stderr()) {
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

fn parse_args(args: &[String], loc: &mut Locate) -> Result<Args> {
    let mut a = Args { paths: Vec::new(), proc_name: None, thread: None, slide: None, no_sym: false, summary: false, words: 6, all: false, format: Format::Text };
    let hex = |text: &str| {
        u64::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16).map_err(|_| anyhow!("not a hex number: {text:?}"))
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut value = || -> Result<&String> {
            i += 1;
            args.get(i).ok_or_else(|| anyhow!("{arg} needs a value"))
        };
        match arg {
            "--proc" => a.proc_name = Some(value()?.clone()),
            "--thread" => a.thread = Some(hex(value()?)?),
            "--slide" => a.slide = Some(hex(value()?)?),
            "--words" => a.words = value()?.parse().map_err(|_| anyhow!("--words needs a number"))?,
            "--dsc" => loc.dsc = Some(PathBuf::from(value()?)),
            "--index" => loc.index_parent = Some(PathBuf::from(value()?)),
            "--no-sym" => a.no_sym = true,
            "--summary" => a.summary = true,
            "--all" => a.all = true,
            "--json" => a.format = Format::Json,
            "--csv" => a.format = Format::Csv,
            other if other.starts_with("--") => bail!("unknown option {other}"),
            other => a.paths.push(PathBuf::from(other)),
        }
        i += 1;
    }
    Ok(a)
}

/// The command's whole output (the tests call this).
pub fn run_inner(args: &[String], mut loc: Locate, err: &mut dyn Write) -> Result<String> {
    let command = args[0].as_str();
    let a = parse_args(&args[1..], &mut loc)?;
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
