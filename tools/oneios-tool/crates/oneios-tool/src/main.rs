//! oneios-tool: OneiOS's host tools. `oneios-tool TOOL ARGS...`; each tool parses its own arguments, so its command
//! line stays the one of the Python script it replaces.

const USAGE: &str = "\
usage: oneios-tool TOOL ARGS...
  sym      name user addresses of a boot log in the image's dyld shared cache (scripts/debug/dsc.py)
  log      read an xnuhost session log: stacks, timeline, diff, gpu, cpu, display, perf
  prelink  the kernel collection's pairing and build (kernel/prelink)
  image    the image tool (scripts/image/imagetool.py)
`oneios-tool TOOL --help` prints a tool's own usage.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(tool) = args.first() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let rest = &args[1..];
    let status = match tool.as_str() {
        "sym" => oneios_sym::run(rest),
        "log" => oneios_log::run(rest),
        "prelink" => oneios_prelink::run(rest),
        "image" => oneios_image::run(rest),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            0
        }
        other => {
            eprintln!("oneios-tool: unknown tool {other:?}\n{USAGE}");
            2
        }
    };
    std::process::exit(status);
}
