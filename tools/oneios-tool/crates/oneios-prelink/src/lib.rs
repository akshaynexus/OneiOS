//! oneios-tool prelink: the steps of kernel/prelink/build.sh in Rust, one subcommand per Python step, each with
//! the step's own arguments and output files so that it can be swapped and compared alone
//! (tools/oneios-tool/README.md "prelink"; tests/prelink/compare-python.sh).
//!
//! All twelve steps are ported: roots, hosted, native-input, merge-reviewed, match (XnuPrelink's
//! fileset_match.py), align, kclasses, structors, vtables, emit (emit.py over XnuPrelink's fileset_emit,
//! fileset_layout and fileset_segmented), audit, record. XnuPrelink is read at pin 9b950f2: its reviewed fixtures
//! and the matcher signature come from the checkout given with `--xnuprelink`.

mod align;
mod audit;
mod emit;
mod hosted;
mod kclasses;
mod layout;
pub mod image;
mod matcher;
mod merge_reviewed;
mod native_input;
mod plistpy;
mod pyset;
mod record;
mod records;
mod roots;
mod structors;
mod vtables;

/// `oneios-tool prelink`'s steps (clap): each takes the arguments of the Python step it replaces.
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// roots.py: which kexts of the kernelcache go into kernel.kc, and why each one that does not
    Roots {
        kernelcache: String,
        /// OUT.json (select.json)
        out: String,
        /// The kernel profile, for its [kernel.prelink] roots
        profile: Option<String>,
    },
    /// hosted.py: the kexts' thread register rewrite and the list of privileged instructions
    Hosted(hosted::Args),
    /// native-input.py: the kernel's weak-lookup binds resolved to its own definitions
    NativeInput {
        input: String,
        output: String,
        /// REPORT.json
        report: String,
    },
    /// merge-reviewed.py: the other agents' reviewed records merged into the owner's
    MergeReviewed(merge_reviewed::Args),
    /// XnuPrelink's fileset_match.py: pair the original kernel's functions and data with the hosted kernel's
    Match(matcher::Args),
    /// align.py: unpaired kext-facing functions by link order against a reference kernel
    Align(align::Args),
    /// kclasses.py: the kernel's class objects by the names their static initializers register
    Kclasses(kclasses::Args),
    /// structors.py: constructors, destructors, factories and VNOP wrappers
    Structors(structors::Args),
    /// vtables.py: the virtual methods of every named vtable, slot by slot
    Vtables(vtables::Args),
    /// emit.py over XnuPrelink's emitter: write kernel.kc and its report
    Emit(emit::Args),
    /// audit.py: every kext import's binding against the known-good pairs and the stub list (exit 1 on a failure)
    Audit(audit::Args),
    /// record.py: the build record kernel.kc.build.json
    Record(record::Args),
    /// Print the kernelcache SHA-256 a kernel profile pins ([kernel.prelink] kernelcache_sha256)
    Pin { profile: String },
}

#[derive(clap::Parser)]
#[command(
    name = "oneios-tool prelink",
    args_override_self = true,
    after_help = "Each step takes the arguments of the Python step it replaced (kernel/prelink/README.md)."
)]
struct Standalone {
    #[command(subcommand)]
    command: Command,
}

/// The exit status of a command line clap refused: 2 for no step or an unknown one, 1 for a step's wrong
/// arguments, 0 for --help.
pub fn refused(e: &clap::Error) -> i32 {
    use clap::error::ErrorKind::*;
    match e.kind() {
        DisplayHelp | DisplayVersion => 0,
        DisplayHelpOnMissingArgumentOrSubcommand | MissingSubcommand | InvalidSubcommand => 2,
        _ => 1,
    }
}

/// Run `oneios-tool prelink ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    match <Standalone as clap::Parser>::try_parse_from(std::iter::once("prelink".to_string()).chain(args.iter().cloned())) {
        Ok(parsed) => main(parsed.command),
        Err(e) => {
            let _ = e.print();
            refused(&e)
        }
    }
}

/// Run a parsed step (oneios-tool's main parses it); the exit status.
pub fn main(command: Command) -> i32 {
    let (step, result) = match command {
        Command::Roots { kernelcache, out, profile } => ("roots", roots::run(&kernelcache, &out, profile.as_deref())),
        Command::Hosted(args) => ("hosted", hosted::run(args)),
        Command::NativeInput { input, output, report } => ("native-input", native_input::run(&input, &output, &report)),
        Command::MergeReviewed(args) => ("merge-reviewed", merge_reviewed::run(args)),
        Command::Match(args) => ("match", matcher::run(args)),
        Command::Align(args) => ("align", align::run(args)),
        Command::Kclasses(args) => ("kclasses", kclasses::run(args)),
        Command::Structors(args) => ("structors", structors::run(args)),
        Command::Vtables(args) => ("vtables", vtables::run(args)),
        Command::Emit(args) => ("emit", emit::run(args)),
        Command::Audit(args) => match audit::run(args) {
            Ok(true) => return 1,
            other => ("audit", other.map(|_| ())),
        },
        Command::Record(args) => ("record", record::run(args)),
        Command::Pin { profile } => ("pin", pin(&profile)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("prelink {step}: {e:#}");
            1
        }
    }
}

fn pin(profile: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let table: toml::Table = std::fs::read_to_string(profile).with_context(|| profile.to_string())?.parse().with_context(|| profile.to_string())?;
    let pinned = table.get("kernel").and_then(|k| k.get("prelink")).and_then(|p| p.get("kernelcache_sha256")).and_then(|v| v.as_str());
    println!("{}", pinned.ok_or_else(|| anyhow::anyhow!("{profile}: no [kernel.prelink] kernelcache_sha256"))?);
    Ok(())
}
