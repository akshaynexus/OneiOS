//! oneios-tool prelink (docs/building.md "Rust tools").

/// Run `oneios-tool prelink ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let _ = args;
    eprintln!("oneios-tool prelink: not built yet");
    2
}
