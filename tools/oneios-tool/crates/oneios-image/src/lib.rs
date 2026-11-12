//! oneios-tool image: the image tool, a port of scripts/image/imagetool.py (docs/image-format.md; docs/building.md
//! "Rust tools"). Same subcommands, options, output text, exit status and output files, byte for byte.
//!
//! In Rust: `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`, `manifest`, and `verify` of a
//! set folder. The others (`check-ipsw`, `fetch`, `record-plain`, `aea-info`, `im4p`, `pack`, and `verify` of a
//! `.tar.zst`) are run by the Python tool: this one starts `python3 -I scripts/image/imagetool.py ...` for them.
//!
//! What differs from Python, on purpose:
//! - where Python ends with a traceback (a file that is not there, a damaged image), this prints one line
//!   `imagetool: ...` and exits 1;
//! - a message that quotes Python's TOML or JSON parser (a profile or a manifest that is not valid TOML or JSON)
//!   quotes this tool's parser instead;
//! - names that are not UTF-8 (file names, xattr names, names in an image's records) are refused with a message;
//!   Python carries them as escapes;
//! - the usage text names `oneios-tool image`.

mod apfs;
#[cfg(test)]
mod fixtures;
mod hashes;
mod manifest;
mod profile;
mod py;
mod seed;
mod tar;

use oneios_common::pyjson::{self, Value};
use std::io::Write;

/// How a command ends when it does not end well.
#[derive(Debug)]
pub enum Fail {
    /// imagetool.py's `Error`: `imagetool: MESSAGE` on stderr, exit 1.
    Error(String),
    /// An IndexError or TypeError in Python's `main` (a missing argument, mostly): the usage on stderr, exit 1.
    Usage,
    /// Where Python ends with a traceback: `imagetool: MESSAGE` on stderr, exit 1.
    Crash(String),
}

macro_rules! err {
    ($($arg:tt)*) => { $crate::Fail::Error(format!($($arg)*)) };
}
macro_rules! crash {
    ($($arg:tt)*) => { $crate::Fail::Crash(format!($($arg)*)) };
}
pub(crate) use {crash, err};

const USAGE: &str = "\
The OneiOS image set: profile, inputs, hash lists, Data seed, manifest, archive.

    oneios-tool image profile PROFILE [--shell | --members]      check a profile; print it as JSON, shell or a table
    oneios-tool image apfs-info IMAGE                            container and volume UUIDs of a raw APFS image
    oneios-tool image hashes IMAGE OUT [--chunk BYTES]           the chunk hash list of an image
    oneios-tool image tree-copy SRC DST META [--owners-from IMAGE --owners-path DIR]
                                                                 copy a tree, recording owners, modes, times, xattrs
                                                                 (owners from IMAGE's APFS records: macOS without root)
    oneios-tool image apfs-owners IMAGE [DIR]                    the owners, modes and xattr names in IMAGE's records
    oneios-tool image seed-pack TREE META OUT --epoch N [--owners FILE]   pack the Data seed (deterministic tar)
    oneios-tool image manifest PROFILE SETDIR [--inputs FILE] [--seed-info FILE] [--tweaks FILE]
    oneios-tool image verify SET [--quick]                       SET: a set folder or its .tar.zst

Run by scripts/image/imagetool.py (python3, found beside this tool's repository or through IMAGETOOL_PY):
    oneios-tool image check-ipsw PROFILE [--source URL|FILE]     compare the profile with the IPSW's directory and BuildManifest
    oneios-tool image fetch PROFILE INDIR [-j N] [--only ROLE,..] [--source URL|FILE]
    oneios-tool image aea-info FILE                              the key-service fields of an AEA header
    oneios-tool image im4p IN OUT                                the payload of an IM4P (LZFSE decoded with `lzfse`)
    oneios-tool image pack SETDIR OUT.tar.zst                    the archive the OneOS fork imports, and OUT.tar.zst.sha256
    oneios-tool image verify SET.tar.zst                         the archive: its .sha256, the tar rules, every file

Formats: docs/image-format.md. The Rust port of scripts/image/imagetool.py: same output, byte for byte.
";

/// The subcommands the Python tool runs.
const PYTHON_ONLY: [&str; 6] = ["check-ipsw", "fetch", "record-plain", "aea-info", "im4p", "pack"];

/// Run `oneios-tool image ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    if matches!(args.first().map(String::as_str), Some("-h" | "--help" | "help")) {
        print!("{USAGE}");
        return 0;
    }
    match main(args) {
        Ok(()) => 0,
        Err(Fail::Error(m)) | Err(Fail::Crash(m)) => {
            eprintln!("imagetool: {m}");
            1
        }
        Err(Fail::Usage) => {
            eprintln!("{USAGE}");
            1
        }
    }
}

/// With ONEIOS_IMAGE_TIMING set: how long each part of a command took, on stderr (`timing: PART 1.234 s`).
pub(crate) struct Laps {
    on: bool,
    last: std::time::Instant,
}

impl Laps {
    pub(crate) fn start() -> Laps {
        Laps { on: std::env::var_os("ONEIOS_IMAGE_TIMING").is_some_and(|v| !v.is_empty()), last: std::time::Instant::now() }
    }
    pub(crate) fn lap(&mut self, part: &str) {
        if self.on {
            eprintln!("timing: {part} {:.3} s", self.last.elapsed().as_secs_f64());
        }
        self.last = std::time::Instant::now();
    }
}

/// `print(text)`.
fn say(text: &str) -> Result<(), Fail> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes()).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush()).map_err(|e| crash!("stdout: {e}"))
}

/// `_opt(args, name, default)`: the argument after the first NAME. A NAME that is last is Python's IndexError.
fn opt<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, Fail> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args.get(i + 1).map(|s| Some(s.as_str())).ok_or(Fail::Usage),
    }
}

/// `_load_json(path)`.
fn load_json(path: Option<&str>) -> Result<Option<Value>, Fail> {
    match path.filter(|p| !p.is_empty()) {
        None => Ok(None),
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| crash!("{}", py::os_error(&e, path)))?;
            pyjson::parse(&text).map(Some).map_err(|e| crash!("{path}: {e}"))
        }
    }
}

fn arg(args: &[String], i: usize) -> Result<&str, Fail> {
    args.get(i).map(String::as_str).ok_or(Fail::Usage)
}

fn main(argv: &[String]) -> Result<(), Fail> {
    // Python: `if len(argv) < 3: sys.exit(__doc__)`, the command and at least one argument.
    if argv.len() < 2 {
        return Err(Fail::Usage);
    }
    let (cmd, args) = (argv[0].as_str(), &argv[1..]);
    match cmd {
        "profile" => {
            let text = profile::cmd_profile(args)?;
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes()).and_then(|_| out.flush()).map_err(|e| crash!("stdout: {e}"))
        }
        "apfs-info" => say(&pyjson::dumps(&apfs::apfs_info(arg(args, 0)?)?.to_value(), Some(2), false)),
        "apfs-owners" => {
            let tree = apfs::ApfsTree::open(arg(args, 0)?)?;
            let walked = tree.walk(args.get(1).map_or("", String::as_str))?;
            say(&pyjson::dumps(&apfs::owners_value(walked), None, true))
        }
        "hashes" => {
            let (image, out) = (arg(args, 0)?, arg(args, 1)?);
            let chunk = match opt(args, "--chunk")? {
                None => hashes::DEFAULT_CHUNK,
                Some(text) => py::int(text, 10)
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| crash!("--chunk {}: not a count of bytes", py::repr_str(text)))?,
            };
            let (size, digest) = hashes::write_hashes(image, out, chunk)?;
            say(&format!("{digest}  {size}  {image}"))
        }
        "tree-copy" => {
            let (src, dst, meta) = (arg(args, 0)?, arg(args, 1)?, arg(args, 2)?);
            let owners_from = opt(args, "--owners-from")?.filter(|s| !s.is_empty());
            let owners_path = opt(args, "--owners-path")?.unwrap_or("");
            let (count, skipped) = seed::tree_copy(src, dst, meta, owners_from, owners_path)?;
            let more = if skipped.is_empty() { String::new() } else { format!(", skipped {} special files", skipped.len()) };
            say(&format!("copied {count} entries{more}"))
        }
        "seed-pack" => {
            let (tree, meta, out) = (arg(args, 0)?, arg(args, 1)?, arg(args, 2)?);
            // Python: int(None) is a TypeError, int("x") a ValueError
            let epoch = opt(args, "--epoch")?.ok_or(Fail::Usage)?;
            let epoch = py::int(epoch, 10).ok_or_else(|| crash!("--epoch {}: not a whole number", py::repr_str(epoch)))?;
            let (count, added, changed) = seed::seed_pack(tree, meta, out, epoch, opt(args, "--owners")?)?;
            let mut info = Value::dict();
            info.set("format", "tar");
            info.set("root", "/private/var");
            info.set("entries", count);
            info.set("added", added);
            info.set("changed", changed);
            say(&pyjson::dumps(&info, None, false))
        }
        "manifest" => {
            let profile = profile::load_profile(arg(args, 0)?)?;
            let setdir = arg(args, 1)?;
            let inputs = load_json(opt(args, "--inputs")?)?;
            let seed_info = load_json(opt(args, "--seed-info")?)?;
            let tweaks = load_json(opt(args, "--tweaks")?)?;
            let (files, bytes) = manifest::make_manifest(&profile, setdir, inputs, seed_info, tweaks)?;
            say(&format!("manifest.json: {files} files, {bytes} bytes"))
        }
        "verify" if std::fs::metadata(arg(args, 0)?).map(|m| m.is_dir()).unwrap_or(false) => {
            let m = manifest::verify_dir(arg(args, 0)?, args.iter().any(|a| a == "--quick"))?;
            say(&manifest::ok_line(&m))
        }
        "verify" => python(argv),
        cmd if PYTHON_ONLY.contains(&cmd) => python(argv),
        _ => Err(Fail::Usage),
    }
}

/// Where scripts/image/imagetool.py is: `IMAGETOOL_PY`, or `scripts/image/imagetool.py` in a folder above this
/// program or above the working directory.
fn python_tool() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("IMAGETOOL_PY").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    let starts = [std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()), std::env::current_dir().ok()];
    for start in starts.into_iter().flatten() {
        for dir in start.ancestors() {
            let tool = dir.join("scripts/image/imagetool.py");
            if tool.is_file() {
                return Some(tool);
            }
        }
    }
    None
}

/// Hand a command this port does not have to the Python tool; its output and exit status become this program's.
fn python(argv: &[String]) -> Result<(), Fail> {
    use std::os::unix::process::CommandExt;
    let _ = std::io::stdout().flush();
    let Some(tool) = python_tool() else {
        eprintln!(
            "oneios-tool image: {} is not ported to Rust and scripts/image/imagetool.py was not found; \
             set IMAGETOOL_PY to its path, or run it: python3 -I scripts/image/imagetool.py {}",
            argv[0],
            argv.join(" ")
        );
        std::process::exit(2);
    };
    let python = std::env::var_os("PYTHON").filter(|p| !p.is_empty()).unwrap_or_else(|| "python3".into());
    let e = std::process::Command::new(&python).arg("-I").arg(&tool).args(argv).exec();
    eprintln!("oneios-tool image: cannot run {} for {}: {e}", python.to_string_lossy(), argv[0]);
    std::process::exit(2);
}
