//! oneios-tool log (docs/building.md "Rust tools").

/// Run `oneios-tool log ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    let _ = args;
    eprintln!("oneios-tool log: not built yet");
    2
}
