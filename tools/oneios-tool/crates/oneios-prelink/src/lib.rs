//! oneios-tool prelink: the steps of kernel/prelink/build.sh in Rust, one subcommand per Python step, each with
//! the step's own arguments and output files so that it can be swapped and compared alone
//! (tools/oneios-tool/README.md "prelink"; tests/prelink/compare-python.sh).
//!
//! Ported: `roots` (roots.py), `hosted` (hosted.py). Not ported yet: native-input, merge-reviewed, the pairing
//! (XnuPrelink's fileset_match.py), align, kclasses, structors, vtables, the emitter (emit.py over XnuPrelink's
//! fileset_emit, fileset_layout, fileset_segmented), audit, record. kernel/prelink/build.sh runs the Python steps.

mod hosted;
mod roots;

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
}

#[derive(clap::Parser)]
#[command(
    name = "oneios-tool prelink",
    args_override_self = true,
    after_help = "Not ported yet (kernel/prelink/build.sh runs the Python steps): native-input, merge-reviewed, match, align, \
                  kclasses, structors, vtables, emit, audit, record."
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
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("prelink {step}: {e:#}");
            1
        }
    }
}
