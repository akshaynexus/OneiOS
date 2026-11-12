//! oneios-tool image: the image tool, a port of scripts/image/imagetool.py (docs/image-format.md; docs/building.md
//! "Rust tools"). Same subcommands, options, output text, exit status and output files, byte for byte.
//!
//! In Rust: `profile`, `apfs-info`, `apfs-owners`, `hashes`, `tree-copy`, `seed-pack`, `manifest`, `im4p` (LZFSE by
//! the lzfse_rust crate: no `lzfse` program) and `verify` of a set folder. The others (`check-ipsw`, `fetch`,
//! `record-plain`, `aea-info`, `pack`, and `verify` of a `.tar.zst`) are run by the Python tool: this one starts
//! `python3 -I scripts/image/imagetool.py ...` for them. Only here: `apfs-ls`, `apfs-copy` (apfs/files.rs).
//!
//! What differs from Python, on purpose:
//! - where Python ends with a traceback (a file that is not there, a damaged image), this prints one line
//!   `imagetool: ...` and exits 1;
//! - a message that quotes Python's TOML or JSON parser (a profile or a manifest that is not valid TOML or JSON)
//!   quotes this tool's parser instead;
//! - names that are not UTF-8 (file names, xattr names, names in an image's records) are refused with a message;
//!   Python carries them as escapes;
//! - the usage text names `oneios-tool image`.

pub mod apfs;
mod apfs_tools;
#[cfg(test)]
mod fixtures;
mod hashes;
pub mod im4p;
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

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Error(m) | Fail::Crash(m) => f.write_str(m),
            Fail::Usage => f.write_str("wrong arguments"),
        }
    }
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
    oneios-tool image im4p IN OUT                                the payload of an IM4P (LZFSE decoded here: no `lzfse` program)

Only in this tool (imagetool.py has no such command): read a volume without mounting it
    oneios-tool image apfs-ls IMAGE [PATH] [--volume N|NAME] [--newest]      a folder's entries: mode, owner, size, name
    oneios-tool image apfs-copy IMAGE SRC DST [--volume N|NAME] [--newest]   copy a file or a folder out of the image
                                                                 (--newest: the newest checkpoint, for a store copied
                                                                 after use; files kept compressed are decoded)

Run by scripts/image/imagetool.py (python3, found beside this tool's repository or through IMAGETOOL_PY):
    oneios-tool image check-ipsw PROFILE [--source URL|FILE]     compare the profile with the IPSW's directory and BuildManifest
    oneios-tool image fetch PROFILE INDIR [-j N] [--only ROLE,..] [--source URL|FILE]
    oneios-tool image aea-info FILE                              the key-service fields of an AEA header
    oneios-tool image pack SETDIR OUT.tar.zst                    the archive the OneOS fork imports, and OUT.tar.zst.sha256
    oneios-tool image verify SET.tar.zst                         the archive: its .sha256, the tar rules, every file

Formats: docs/image-format.md. The Rust port of scripts/image/imagetool.py: same output, byte for byte.
";

/// Arguments after the ones a command reads. imagetool.py ignores them (it takes its arguments by position and
/// looks for its options by name), so they are taken and ignored here too.
#[derive(clap::Args, Debug)]
pub struct More {
    #[arg(hide = true, allow_hyphen_values = true)]
    extra: Vec<String>,
}

/// The arguments of a command the Python tool runs, as they are.
#[derive(clap::Args, Debug)]
pub struct Raw {
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    args: Vec<String>,
}

/// `oneios-tool image`'s subcommands (clap): imagetool.py's commands with its arguments and options. An option
/// given twice counts once, the first time, as in imagetool.py.
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Check a profile; print it as JSON, shell or a table
    Profile {
        profile: String,
        #[arg(long)]
        shell: bool,
        #[arg(long)]
        members: bool,
        #[command(flatten)]
        more: More,
    },
    /// Container and volume UUIDs of a raw APFS image
    ApfsInfo {
        image: String,
        #[command(flatten)]
        more: More,
    },
    /// The owners, modes and xattr names in an image's records
    ApfsOwners {
        image: String,
        dir: Option<String>,
        #[command(flatten)]
        more: More,
    },
    /// The chunk hash list of an image
    Hashes {
        image: String,
        out: String,
        #[arg(long, value_name = "BYTES")]
        chunk: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Copy a tree, recording owners, modes, times, xattrs
    TreeCopy {
        src: String,
        dst: String,
        meta: String,
        #[arg(long = "owners-from", value_name = "IMAGE")]
        owners_from: Vec<String>,
        #[arg(long = "owners-path", value_name = "DIR")]
        owners_path: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Pack the Data seed (deterministic tar)
    SeedPack {
        tree: String,
        meta: String,
        out: String,
        #[arg(long, value_name = "N")]
        epoch: Vec<String>,
        #[arg(long, value_name = "FILE")]
        owners: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Write manifest.json and the hash lists of a set folder
    Manifest {
        profile: String,
        setdir: String,
        #[arg(long, value_name = "FILE")]
        inputs: Vec<String>,
        #[arg(long = "seed-info", value_name = "FILE")]
        seed_info: Vec<String>,
        #[arg(long, value_name = "FILE")]
        tweaks: Vec<String>,
        #[command(flatten)]
        more: More,
    },
    /// Check a set folder (here) or its .tar.zst (the Python tool)
    Verify {
        set: String,
        #[arg(long)]
        quick: bool,
        #[command(flatten)]
        more: More,
    },
    /// The payload of an IM4P (LZFSE decoded here: no `lzfse` program)
    Im4p {
        input: String,
        out: String,
        #[command(flatten)]
        more: More,
    },
    /// Only in this tool: a folder's entries (mode, owner, size, name), read without a mount
    ApfsLs {
        image: String,
        path: Option<String>,
        #[arg(long, value_name = "N|NAME")]
        volume: Option<String>,
        /// The newest checkpoint, for a store copied after use
        #[arg(long)]
        newest: bool,
        /// The reader: ours, apfs-fuse (Linux), or ours and apfs-fuse when ours refuses
        #[arg(long, value_enum, default_value = "auto")]
        apfs: apfs::fuse::Backend,
    },
    /// Only in this tool: copy a file or a folder out of an image, without a mount
    ApfsCopy {
        image: String,
        src: String,
        dst: String,
        #[arg(long, value_name = "N|NAME")]
        volume: Option<String>,
        /// The newest checkpoint, for a store copied after use
        #[arg(long)]
        newest: bool,
        /// The reader: ours, apfs-fuse (Linux), or ours and apfs-fuse when ours refuses
        #[arg(long, value_enum, default_value = "auto")]
        apfs: apfs::fuse::Backend,
    },
    /// (Python tool) compare the profile with the IPSW's directory and BuildManifest
    CheckIpsw(Raw),
    /// (Python tool) fetch the IPSW's members a profile names
    Fetch(Raw),
    /// (Python tool) record a decrypted image in inputs.json
    RecordPlain(Raw),
    /// (Python tool) the key-service fields of an AEA header
    AeaInfo(Raw),
    /// (Python tool) the archive the OneOS fork imports
    Pack(Raw),
}

#[derive(clap::Parser)]
#[command(name = "oneios-tool image", args_override_self = true)]
struct Standalone {
    #[command(subcommand)]
    command: Command,
}

/// The usage, as a wrong command line prints it (imagetool.py prints its own there).
pub fn usage() -> &'static str {
    USAGE
}

/// Run `oneios-tool image ARGS...`; `args` are the arguments after the tool's name. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    match <Standalone as clap::Parser>::try_parse_from(std::iter::once("image".to_string()).chain(args.iter().cloned())) {
        Ok(parsed) => main(parsed.command),
        Err(e) if !e.use_stderr() => {
            let _ = e.print();
            0
        }
        Err(_) => {
            eprintln!("{USAGE}");
            1
        }
    }
}

/// Run a parsed command (oneios-tool's main parses it); the exit status.
pub fn main(command: Command) -> i32 {
    match dispatch(command) {
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

/// `_opt(args, name)`: the first value of an option.
fn first(values: &[String]) -> Option<&str> {
    values.first().map(String::as_str)
}

fn dispatch(command: Command) -> Result<(), Fail> {
    match command {
        Command::Profile { profile, shell, members, .. } => {
            let mut args = vec![profile];
            if shell {
                args.push("--shell".into());
            }
            if members {
                args.push("--members".into());
            }
            let text = profile::cmd_profile(&args)?;
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes()).and_then(|_| out.flush()).map_err(|e| crash!("stdout: {e}"))
        }
        Command::ApfsInfo { image, .. } => say(&pyjson::dumps(&apfs::apfs_info(&image)?.to_value(), Some(2), false)),
        Command::ApfsOwners { image, dir, .. } => {
            let tree = apfs::ApfsTree::open(&image)?;
            let walked = tree.walk(dir.as_deref().unwrap_or(""))?;
            say(&pyjson::dumps(&apfs::owners_value(walked), None, true))
        }
        Command::Hashes { image, out, chunk, .. } => {
            let chunk = match first(&chunk) {
                None => hashes::DEFAULT_CHUNK,
                Some(text) => py::int(text, 10)
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| crash!("--chunk {}: not a count of bytes", py::repr_str(text)))?,
            };
            let (size, digest) = hashes::write_hashes(&image, &out, chunk)?;
            say(&format!("{digest}  {size}  {image}"))
        }
        Command::TreeCopy { src, dst, meta, owners_from, owners_path, .. } => {
            let owners_from = first(&owners_from).filter(|s| !s.is_empty());
            let owners_path = first(&owners_path).unwrap_or("");
            let (count, skipped) = seed::tree_copy(&src, &dst, &meta, owners_from, owners_path)?;
            let more = if skipped.is_empty() { String::new() } else { format!(", skipped {} special files", skipped.len()) };
            say(&format!("copied {count} entries{more}"))
        }
        Command::SeedPack { tree, meta, out, epoch, owners, .. } => {
            // Python: int(None) is a TypeError, int("x") a ValueError
            let epoch = first(&epoch).ok_or(Fail::Usage)?;
            let epoch = py::int(epoch, 10).ok_or_else(|| crash!("--epoch {}: not a whole number", py::repr_str(epoch)))?;
            let (count, added, changed) = seed::seed_pack(&tree, &meta, &out, epoch, first(&owners))?;
            let mut info = Value::dict();
            info.set("format", "tar");
            info.set("root", "/private/var");
            info.set("entries", count);
            info.set("added", added);
            info.set("changed", changed);
            say(&pyjson::dumps(&info, None, false))
        }
        Command::Manifest { profile, setdir, inputs, seed_info, tweaks, .. } => {
            let profile = profile::load_profile(&profile)?;
            let inputs = load_json(first(&inputs))?;
            let seed_info = load_json(first(&seed_info))?;
            let tweaks = load_json(first(&tweaks))?;
            let (files, bytes) = manifest::make_manifest(&profile, &setdir, inputs, seed_info, tweaks)?;
            say(&format!("manifest.json: {files} files, {bytes} bytes"))
        }
        Command::Verify { set, quick, more } => {
            if std::fs::metadata(&set).map(|m| m.is_dir()).unwrap_or(false) {
                let m = manifest::verify_dir(&set, quick)?;
                return say(&manifest::ok_line(&m));
            }
            let mut argv = vec!["verify".to_string(), set];
            if quick {
                argv.push("--quick".into());
            }
            argv.extend(more.extra);
            python(&argv)
        }
        Command::Im4p { input, out, .. } => say(&im4p::cmd(&input, &out)?),
        Command::ApfsLs { image, path, volume, newest, apfs } => apfs_tools::ls(&image, path.as_deref().unwrap_or(""), volume.as_deref(), newest, apfs),
        Command::ApfsCopy { image, src, dst, volume, newest, apfs } => apfs_tools::copy(&image, &src, &dst, volume.as_deref(), newest, apfs),
        Command::CheckIpsw(raw) => python_command("check-ipsw", raw),
        Command::Fetch(raw) => python_command("fetch", raw),
        Command::RecordPlain(raw) => python_command("record-plain", raw),
        Command::AeaInfo(raw) => python_command("aea-info", raw),
        Command::Pack(raw) => python_command("pack", raw),
    }
}

fn python_command(name: &str, raw: Raw) -> Result<(), Fail> {
    let mut argv = vec![name.to_string()];
    argv.extend(raw.args);
    python(&argv)
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
