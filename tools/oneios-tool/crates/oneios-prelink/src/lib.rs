//! oneios-tool prelink: the steps of kernel/prelink/build.sh in Rust, one subcommand per Python step, each with
//! the step's own arguments and output files so that it can be swapped and compared alone
//! (tools/oneios-tool/README.md "prelink"; tests/prelink/compare-python.sh).
//!
//! Ported: `roots` (roots.py), `hosted` (hosted.py). Not ported yet: native-input, merge-reviewed, the pairing
//! (XnuPrelink's fileset_match.py), align, kclasses, structors, vtables, the emitter (emit.py over XnuPrelink's
//! fileset_emit, fileset_layout, fileset_segmented), audit, record. kernel/prelink/build.sh runs the Python steps.

mod hosted;
mod roots;

const USAGE: &str = "\
usage: oneios-tool prelink STEP ARGS...     (a step of kernel/prelink/build.sh; arguments as the Python step's)
  roots KERNELCACHE OUT.json [PROFILE.toml]                              roots.py: which kexts go into kernel.kc
  hosted KERNELCACHE OUT REPORT.json [--closure select.json] [--x18]     hosted.py: the kexts for the hosted kernel
Not ported yet (kernel/prelink/build.sh runs the Python steps): native-input, merge-reviewed, match, align, kclasses,
structors, vtables, emit, audit, record.";

/// Run `oneios-tool prelink ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let Some(step) = args.first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest = &args[1..];
    let result = match step.as_str() {
        "roots" => roots::run(rest),
        "hosted" => hosted::run(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return 0;
        }
        other => {
            eprintln!("oneios-tool prelink: no step {other:?}\n{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("prelink {step}: {e:#}");
            1
        }
    }
}
