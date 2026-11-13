//! What every board archive generator shares: where it runs (the build tree, the device repository, this
//! repository), Python's file and process calls as the scripts used them, and the steps each script repeated
//! (compile with a recorded command, make a late object, archive, check the definitions, assemble the bindings).
//!
//! The generators were Python scripts run by UniXNU's driver between the two kernel passes
//! (kernel/patches/oneios-hosted/*/build-oneios-*.py, kernel/device/scripts/build-oneios-board.py). Each module
//! here is one of them, with the script's name, and writes the files that script wrote, byte for byte
//! (tests/kernelgen). Where a helper copies a Python detail, the comment says which.

use crate::drivers_build::{self, Record};
use anyhow::{anyhow, bail, Context, Result};
use oneios_common::pyjson::{self, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// `sys.exit(text)`: the message on stderr and status 1 (lib.rs prints it).
macro_rules! fail {
    ($($arg:tt)*) => { return Err(anyhow::anyhow!($($arg)*)) };
}
pub(crate) use fail;

/// The places of a generator: `TREE = Path.cwd()`, `DEVICE` from UNIXNU_DEVICE, `REPO` from ONEIOS_REPO or two
/// levels above the device repository's resolved path, the first pass's compile records.
pub struct Ctx {
    pub name: &'static str,
    pub tree: PathBuf,
    pub repo: PathBuf,
    pub repo_from_env: bool,
    pub records: PathBuf,
}

impl Ctx {
    /// `hint` is what the script said beside "UNIXNU_DEVICE is not set".
    pub fn new(name: &'static str, hint: &str) -> Result<Ctx> {
        let tree = std::env::current_dir().context("the current directory")?;
        let device = match env("UNIXNU_DEVICE") {
            Some(d) => pypath(&d),
            None => fail!("{name}: UNIXNU_DEVICE is not set ({hint})"),
        };
        let from_env = env("ONEIOS_REPO");
        let repo = match &from_env {
            Some(r) => pypath(r),
            // DEVICE.resolve().parents[1]
            None => resolve(&device).parent().and_then(Path::parent).map(Path::to_path_buf)
                .ok_or_else(|| anyhow!("{name}: {} has no directory two levels up", device.display()))?,
        };
        let records = tree.join("build/xnu/ios-obj/DEVELOPMENT_ARM64_BCM2837");
        Ok(Ctx { name, tree, repo, repo_from_env: from_env.is_some(), records })
    }

    /// `drivers.record(RECORDS, drivers.RECORDS[universe])`.
    pub fn record(&self, universe: &str) -> Result<Record> {
        drivers_build::record(&self.records, drivers_build::record_name(universe))
    }

    /// `drivers.command(record, source, object)`.
    pub fn command(&self, record: &Record, source: &Path, object: &Path) -> Result<Vec<String>> {
        Ok(drivers_build::command(&drivers_build::here(&self.repo)?, record, source, object))
    }

    /// A path as the scripts wrote it into a report: `str(p.relative_to(REPO))`.
    pub fn relative(&self, path: &Path) -> Result<String> {
        match path.strip_prefix(&self.repo) {
            Ok(rel) => Ok(s(rel)),
            Err(_) => bail!("{}: {} is not under {}", self.name, path.display(), self.repo.display()),
        }
    }
}

/// `os.environ.get(name) or ...`: an empty value counts as unset.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `Path(text)`: pathlib drops repeated and trailing separators and `.` components (and keeps `..`).
pub fn pypath(text: &str) -> PathBuf {
    Path::new(text).components().collect()
}

/// `Path.resolve()` (not strict): the real path, or the path as it is when it does not exist.
pub fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `str(path)`.
pub fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `path.name`.
pub fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `path.stem`.
pub fn stem(path: &Path) -> String {
    path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `Path.read_text()`: UTF-8, and universal newlines (text mode turns `\r\n` and a lone `\r` into `\n`).
pub fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    let text = String::from_utf8(bytes).map_err(|e| anyhow!("{}: not UTF-8 ({e})", path.display()))?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

/// `Path.write_text(text)`.
pub fn write_text(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("{}", path.display()))
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("{}", path.display()))
}

/// `json.loads(path.read_text())`.
pub fn read_json(path: &Path) -> Result<Value> {
    pyjson::parse(&read_text(path)?).map_err(|e| anyhow!("{}: {e}", path.display()))
}

/// `path.write_text(json.dumps(value, indent=indent) + '\n')`.
pub fn write_json(path: &Path, value: &Value, indent: usize) -> Result<()> {
    write_text(path, &(pyjson::dumps(value, Some(indent), false) + "\n"))
}

/// `hashlib.sha256(path.read_bytes()).hexdigest()`.
pub fn sha256(path: &Path) -> Result<String> {
    Ok(oneios_common::hex(&Sha256::digest(read_bytes(path)?)))
}

/// `int(text, 16)` of a record's address or size ("0x..." or bare hex).
pub fn hex(text: &str) -> Result<u64> {
    let t = text.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16).map_err(|_| anyhow!("not a hexadecimal number: {text:?}"))
}

/// `value[key]` of a record; a missing key ends the run as Python's KeyError did.
pub fn key<'a>(value: &'a Value, k: &str) -> Result<&'a Value> {
    value.get(k).ok_or_else(|| anyhow!("KeyError: {k:?}"))
}
pub fn key_str<'a>(value: &'a Value, k: &str) -> Result<&'a str> {
    key(value, k)?.as_str().ok_or_else(|| anyhow!("{k:?}: a string expected"))
}
pub fn key_hex(value: &Value, k: &str) -> Result<u64> {
    hex(key_str(value, k)?)
}
pub fn key_int(value: &Value, k: &str) -> Result<i128> {
    key(value, k)?.as_int().ok_or_else(|| anyhow!("{k:?}: a number expected"))
}
pub fn key_list<'a>(value: &'a Value, k: &str) -> Result<&'a [Value]> {
    key(value, k)?.as_list().ok_or_else(|| anyhow!("{k:?}: a list expected"))
}

/// `text[-count:]`: the last `count` characters.
pub fn tail(text: &str, count: usize) -> &str {
    let chars = text.chars().count();
    if chars <= count {
        return text;
    }
    let (at, _) = text.char_indices().nth(chars - count).expect("counted above");
    &text[at..]
}

/// `text[:count]`: the first `count` characters.
pub fn first(text: &str, count: usize) -> &str {
    text.char_indices().nth(count).map_or(text, |(at, _)| &text[..at])
}

/// `text.replace(anchor, replacement)` for every pair, each anchor exactly once in the text (`text.count`).
/// `gone(anchor)` is the message when it is not.
pub fn replace_once(text: String, pairs: &[(&str, &str)], gone: impl Fn(&str) -> String) -> Result<String> {
    let mut text = text;
    for (anchor, replacement) in pairs {
        if text.matches(anchor).count() != 1 {
            fail!("{}", gone(anchor));
        }
        text = text.replace(anchor, replacement);
    }
    Ok(text)
}

/// `shutil.which(tool) or fallback`.
pub fn which(tool: &str, fallback: &str) -> String {
    which::which(tool).map(|p| s(&p)).unwrap_or_else(|_| fallback.to_string())
}

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=seconds)`.
pub fn capture(command: &[String], cwd: Option<&Path>, seconds: Option<u64>) -> Result<Output> {
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().with_context(|| format!("{}: cannot be started", command[0]))?;
    // Both pipes are read while the program runs: a full pipe would stop it.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err.read_to_end(&mut bytes);
        bytes
    });
    let status = match seconds {
        None => child.wait()?,
        Some(limit) => match child.wait_timeout(Duration::from_secs(limit))? {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{}: timed out after {limit} seconds", command[0]);
            }
        },
    };
    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();
    // A program ended by a signal: Python's returncode is the negative signal number, non-zero either way.
    Ok(Output { code: status.code().unwrap_or(-1), stdout, stderr })
}

/// `subprocess.run(command, capture_output=True, text=True, check=True).stdout`.
pub fn output(command: &[String]) -> Result<String> {
    let result = capture(command, None, None)?;
    if result.code != 0 {
        bail!("{} ended with status {}: {}", command.join(" "), result.code, result.stderr.trim_end());
    }
    Ok(result.stdout)
}

/// `subprocess.run(command, check=True, cwd=cwd)`: the program writes to this one's stdout and stderr.
pub fn run(command: &[String], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let status = cmd.status().with_context(|| format!("{}: cannot be started", command[0]))?;
    if !status.success() {
        bail!("{} ended with {status}", command.join(" "));
    }
    Ok(())
}

/// A generator's compile of one source with the recorded command of its universe plus `extra`: the compiler's
/// stderr goes to `<object>.log`; on a failure its last `shown` characters go to stderr.
pub struct Compile<'a> {
    pub ctx: &'a Ctx,
    pub shown: usize,
}

impl Compile<'_> {
    /// Returns the compiler's result: the callers that go on after a failure (gpu) read it.
    pub fn result(&self, record: &Record, source: &Path, object: &Path, extra: &[String]) -> Result<Output> {
        let mut command = self.ctx.command(record, source, object)?;
        command.extend_from_slice(extra);
        let result = capture(&command, Some(Path::new(&record.directory)), Some(900))?;
        write_text(&object.with_extension("log"), &result.stderr)?;
        Ok(result)
    }

    pub fn object(&self, record: &Record, source: &Path, object: &Path, extra: &[String]) -> Result<PathBuf> {
        let result = self.result(record, source, object, extra)?;
        if result.code != 0 {
            eprint!("{}", tail(&result.stderr, self.shown));
            fail!("{}: {} did not compile", self.ctx.name, name(source));
        }
        Ok(object.to_path_buf())
    }
}

/// A class's static initializer registers it, so it must run inside the board's module load and not with the
/// kernel's own initializers: the object without `__DATA,__mod_init_func` (and the other `sections`), its
/// initializer `symbol` global so the start function can call it.
pub fn late_object(objcopy: &str, object: &Path, late: &Path, sections: &[&str], symbol: &str) -> Result<()> {
    let mut command = vec![objcopy.to_string()];
    command.extend(sections.iter().map(|sec| format!("--remove-section={sec}")));
    command.extend([format!("--globalize-symbol={symbol}"), s(object), s(late)]);
    run(&command, None)
}

/// `archive.unlink(missing_ok=True)`, then `llvm-ar rcs archive objects...` (`ar` where there is no llvm-ar).
pub fn archive(archive: &Path, objects: &[PathBuf]) -> Result<()> {
    match std::fs::remove_file(archive) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => bail!("{}: {e}", archive.display()),
        _ => {}
    }
    let mut command = vec![which("llvm-ar", "ar"), "rcs".to_string(), s(archive)];
    command.extend(objects.iter().map(|o| s(o)));
    run(&command, None)
}

/// The names of `required` the archive does not define (`llvm-nm -g --defined-only`).
pub fn missing_definitions(archive: &Path, required: &[&str]) -> Result<Vec<String>> {
    let defined = output(&[which("llvm-nm", "nm"), "-g".into(), "--defined-only".into(), s(archive)])?;
    Ok(required.iter().filter(|name| !defined.lines().any(|line| line.ends_with(&format!(" {name}"))))
        .map(|name| name.to_string()).collect())
}

pub fn require_definitions(ctx: &Ctx, archive: &Path, required: &[&str]) -> Result<()> {
    let missing = missing_definitions(archive, required)?;
    if !missing.is_empty() {
        fail!("{}: definitions absent: {}", ctx.name, missing.join(", "));
    }
    Ok(())
}

/// The bindings' assembly files (`<stem>.S`), assembled with the record's compiler for its target, as the
/// scripts did: `compiler -target triple -c file.S -o file.o` in the record's directory. Returns the objects.
pub fn assemble(record: &Record, out: &Path, files: &[(&str, &str)]) -> Result<Vec<PathBuf>> {
    let arguments = &record.arguments;
    // next(a for a in arguments if 'clang' in Path(a).name)
    let compiler = arguments.iter().find(|a| name(Path::new(a)).contains("clang"))
        .ok_or_else(|| anyhow!("the compile record names no clang"))?;
    let triple = match arguments.iter().position(|a| a == "-target") {
        Some(at) => arguments.get(at + 1).ok_or_else(|| anyhow!("the compile record's -target has no value"))?.clone(),
        None => "arm64e-apple-ios27.0".to_string(),
    };
    let mut objects = Vec::new();
    for (stem, text) in files {
        let source = out.join(format!("{stem}.S"));
        write_text(&source, text)?;
        let object = source.with_extension("o");
        run(&[compiler.clone(), "-target".into(), triple.clone(), "-c".into(), s(&source), "-o".into(), s(&object)],
            Some(Path::new(&record.directory)))?;
        objects.push(object);
    }
    Ok(objects)
}

/// One entry of objects.json's "sources": the path, the universe when the script gave one, the file's SHA-256.
pub fn source_entry(path: String, universe: Option<&str>, file: &Path) -> Result<Value> {
    let mut entry = Value::dict();
    entry.set("path", path);
    if let Some(universe) = universe {
        entry.set("universe", universe);
    }
    entry.set("sha256", sha256(file)?);
    Ok(entry)
}

/// The "sources" list of objects.json for files below the repository.
pub fn source_list(ctx: &Ctx, universe: Option<&str>, sources: &[PathBuf]) -> Result<Value> {
    let mut list = Value::list();
    for path in sources {
        list.push(source_entry(ctx.relative(path)?, universe, path)?);
    }
    Ok(list)
}

/// objects.json with `{"sources": [{path, universe, sha256}...]}` for files below the repository.
pub fn write_objects(ctx: &Ctx, out: &Path, universe: Option<&str>, sources: &[PathBuf]) -> Result<()> {
    let mut value = Value::dict();
    value.set("sources", source_list(ctx, universe, sources)?);
    write_json(&out.join("objects.json"), &value, 2)
}

/// `path.is_file()` for each; the first that is not ends the run with `missing(path)`.
pub fn need_files(paths: &[PathBuf], missing: impl Fn(&Path) -> String) -> Result<()> {
    for path in paths {
        if !path.is_file() {
            fail!("{}", missing(path));
        }
    }
    Ok(())
}

/// `out.mkdir(parents=True, exist_ok=True)`.
pub fn mkdirs(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("{}", path.display()))
}

/// The bindings: `lines` is the stubs' file, `targets` the second one. The stub of an original function is
/// `bti c; b <name>.original`, with `<name>.original` an absolute symbol of the second object at the function's
/// address in the collection (in one file the assembler would resolve the branch itself; across objects it
/// leaves a branch relocation, which XnuPrelink's emitter retargets when it places the kernel).
pub struct Stubs {
    pub lines: Vec<String>,
    pub targets: Vec<String>,
}

impl Stubs {
    /// `targets = [lines[0]]`: both files start with the "Generated by" line.
    pub fn new(lines: Vec<String>) -> Stubs {
        let targets = vec![lines[0].clone()];
        Stubs { lines, targets }
    }

    pub fn stub(&mut self, name: &str, address: u64) {
        self.lines.extend([format!(".globl {name}"), ".p2align 2".to_string(), format!("{name}:"), " bti c".to_string(),
                           format!(" b {name}.original")]);
        self.targets.extend([format!(".globl {name}.original"), format!(".set {name}.original, {address:#x}")]);
    }

    /// `'\n'.join(lines) + '\n'` of both files.
    pub fn text(&self) -> (String, String) {
        (self.lines.join("\n") + "\n", self.targets.join("\n") + "\n")
    }
}
