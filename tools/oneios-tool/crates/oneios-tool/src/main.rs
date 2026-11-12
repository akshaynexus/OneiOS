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
    /// Name user addresses of a boot log in the image's dyld shared cache (scripts/debug/dsc.py)
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
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let status = match Top::try_parse_from(&args) {
        Ok(top) => match top.tool {
            Tool::Sym(cli) => oneios_sym::main(cli),
            Tool::Log { command } => oneios_log::main(command),
            Tool::Prelink { command } => oneios_prelink::main(command),
            Tool::Image { command } => oneios_image::main(command),
            Tool::Ioslog { command } => oneios_ioslog::main(command),
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
            _ => {
                let _ = e.print();
                2
            }
        },
    };
    std::process::exit(status);
}
