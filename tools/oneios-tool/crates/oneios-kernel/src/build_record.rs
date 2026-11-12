//! `oneios-tool kernel build-record` (was kernel/build-record.py).
//!
//! The build record of a OneiOS XNU build, and its place in out/kernel.
//!
//!   oneios-tool kernel build-record finish --host linux|macos --work DIR --config NAME --out OUT [--repo CHECKOUT]
//!       DIR is UniXNU's work directory of the build (out/kernel/unixnu/oneios/<os profile>/<config>): its
//!       kernel/kernel.macho (the native kernel, --skip-prelink), xnu-inputs.json (source commit and every patch with
//!       its SHA-256) and .config. Writes OUT/builds/<build id>/{xnu.macho, xnu.macho.build.json, series.txt} and
//!       points OUT/current at it. CHECKOUT is this repository (the script found it from its own place; default:
//!       the current directory).
//!   oneios-tool kernel build-record --self-test
//!
//! The record is what runtime/package/pack.py reads before it lets a Mach-O into a release (docs/releases.md):
//! builder, sha256, prelinked, kernelcache_kexts_added, apple_binary_inputs, restricted_sources.
//! The build id names the inputs, not the machine: xnu-<version>-<profile>-<12 hex>, the hex being a hash of the XNU
//! commit, every patch's SHA-256 in order, the configuration, the UniXNU commit and the board archives' sources.

use crate::gen::{self, fail};
use anyhow::{anyhow, Context, Result};
use indexmap::IndexMap;
use object::macho::{MachHeader64, LC_DYLD_CHAINED_FIXUPS, LC_UNIXTHREAD};
use object::read::macho::{MachHeader, Segment};
use object::LittleEndian as LE;
use oneios_common::pyjson::{self, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const BUILDER: &str = "oneios-kernel";
/// Sources that may be linked into a local build but not into a release (their licence does not allow redistribution).
const RESTRICTED: [(&str, &str); 1] =
    [("XNU_SOURCE_CORECRYPTO14", "corecrypto-2023 (github.com/ONLYA/corecrypto-2023: Apple corecrypto, Internal Use License)")];

#[derive(clap::Args)]
pub struct Args {
    /// finish: write the record of a finished build
    #[arg(value_parser = ["finish"])]
    command: Option<String>,
    /// Check the record writer on a made-up kernel
    #[arg(long = "self-test")]
    self_test: bool,
    #[arg(long, value_parser = ["linux", "macos"])]
    host: Option<String>,
    /// UniXNU's work directory of the build
    #[arg(long)]
    work: Option<PathBuf>,
    /// The defconfig's name
    #[arg(long)]
    config: Option<String>,
    /// out/kernel: builds/<build id>/ and current go there
    #[arg(long)]
    out: Option<PathBuf>,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    repo: Option<PathBuf>,
}

/// Type, architecture, segments and entry of a 64-bit Mach-O.
struct Facts {
    filetype: u32,
    cpusubtype: u32,
    segments: Value,
    entry: Value,
    chained_fixups: bool,
}

fn macho_facts(path: &Path) -> Result<Facts> {
    let data = gen::read_bytes(path)?;
    let header = match MachHeader64::<LE>::parse(&*data, 0) {
        Ok(header) => header,
        Err(_) => fail!("{}: not a 64-bit Mach-O", path.display()),
    };
    let broken = |e: object::Error| anyhow!("{}: {e}", path.display());
    let mut facts = Facts { filetype: header.filetype(LE), cpusubtype: header.cpusubtype(LE) & 0xffffff, segments: Value::dict(), entry: Value::Null,
                            chained_fixups: false };
    let mut commands = header.load_commands(LE, &*data, 0).map_err(broken)?;
    while let Some(command) = commands.next().map_err(broken)? {
        if let Some((segment, _)) = command.segment_64().map_err(broken)? {
            let name = String::from_utf8_lossy(segment.name()).into_owned();
            let mut place = Value::dict();
            place.set("vmaddr", format!("{:#x}", segment.vmaddr(LE)));
            place.set("vmsize", format!("{:#x}", segment.vmsize(LE)));
            facts.segments.set(name, place);
        } else if command.cmd() == LC_UNIXTHREAD {
            // flavor, count, then ARM_THREAD_STATE64 (x0-x28, fp, lr, sp, pc)
            let raw = command.raw_data();
            let word = |at: usize, size: usize| raw.get(at..at + size).ok_or_else(|| anyhow!("{}: a short LC_UNIXTHREAD", path.display()));
            if u32::from_le_bytes(word(8, 4)?.try_into().expect("4 bytes")) == 6 {
                facts.entry = Value::str(format!("{:#x}", u64::from_le_bytes(word(16 + 32 * 8, 8)?.try_into().expect("8 bytes"))));
            }
        } else if command.cmd() == LC_DYLD_CHAINED_FIXUPS {
            facts.chained_fixups = true;
        }
    }
    Ok(facts)
}

/// .config as a dict: `CONFIG_X=value` lines, the prefix and the quotes taken off.
fn parse_config(text: &str) -> IndexMap<String, String> {
    let mut result = IndexMap::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once('=') {
                result.insert(key.strip_prefix("CONFIG_").unwrap_or(key).to_string(), value.trim_matches('"').to_string());
            }
        }
    }
    result
}

/// `git -C path rev-parse HEAD`, or null where there is no checkout (or no git).
fn git_head(path: &Path) -> Value {
    match gen::capture(&["git".into(), "-C".into(), gen::s(path), "rev-parse".into(), "HEAD".into()], None, None) {
        Ok(result) if result.code == 0 => Value::str(result.stdout.trim()),
        _ => Value::Null,
    }
}

/// A patch path as a reader finds it: relative to the repository (external/..., kernel/...).
fn relative_patch(root: &Path, path: &Path) -> String {
    let path = gen::resolve(path);
    gen::s(path.strip_prefix(root).unwrap_or(&path))
}

fn list<T: Into<Value>>(items: impl IntoIterator<Item = T>) -> Value {
    let mut value = Value::list();
    items.into_iter().for_each(|item| value.push(item));
    value
}

fn config_dict(config: &[(&String, &String)]) -> Value {
    let mut value = Value::dict();
    config.iter().for_each(|(k, v)| value.set(k.as_str(), v.as_str()));
    value
}

/// The public settings of the configuration (no `_` names), sorted.
fn public_config(config: &IndexMap<String, String>) -> Vec<(&String, &String)> {
    let mut items: Vec<_> = config.iter().filter(|(k, _)| !k.starts_with('_')).collect();
    items.sort();
    items
}

struct Inputs<'a> {
    xnu_commit: &'a Value,
    patches: &'a [Value],
    config: &'a IndexMap<String, String>,
    unixnu_commit: &'a Value,
    unixnu_patches: &'a [Value],
    board_sources: &'a [Value],
}

fn hashes(entries: &[Value]) -> Value {
    list(entries.iter().map(|entry| entry.get("sha256").cloned().unwrap_or(Value::Null)))
}

fn build_id(xnu_version: &str, profile: &str, inputs: &Inputs) -> String {
    let mut named = Value::dict();
    named.set("xnu", inputs.xnu_commit.clone());
    named.set("patches", hashes(inputs.patches));
    named.set("config", list(public_config(inputs.config).into_iter().map(|(k, v)| list([k.as_str(), v.as_str()]))));
    named.set("unixnu", inputs.unixnu_commit.clone());
    named.set("unixnu_patches", hashes(inputs.unixnu_patches));
    named.set("board", hashes(inputs.board_sources));
    let digest = oneios_common::hex(&Sha256::digest(pyjson::dumps(&named, None, true).as_bytes()));
    format!("xnu-{xnu_version}-{profile}-{}", &digest[..12])
}

/// The first line a tool prints for its version (stdout, then stderr), or null where the tool is not installed.
/// (The script ran the tool twice, once for each stream.)
fn tool_version(command: &[&str]) -> Value {
    let command: Vec<String> = command.iter().map(|part| part.to_string()).collect();
    match gen::capture(&command, None, None) {
        Ok(result) => {
            let out = result.stdout + &result.stderr;
            out.trim().lines().next().map(Value::str).unwrap_or(Value::Null)
        }
        Err(_) => Value::Null,
    }
}

struct Build<'a> {
    root: &'a Path,
    kernel: &'a Path,
    inputs: &'a Value,
    config: &'a IndexMap<String, String>,
    profile: &'a str,
    host: &'a str,
    config_name: &'a str,
    unixnu: &'a Path,
    native_record: Option<&'a Value>,
    board_sources: &'a [Value],
}

fn patch_entry(path: String, sha256: Value) -> Value {
    let mut entry = Value::dict();
    entry.set("path", path);
    entry.set("sha256", sha256);
    entry
}

fn make_record(build: &Build) -> Result<Value> {
    let (root, kernel, config) = (build.root, build.kernel, build.config);
    let facts = macho_facts(kernel)?;
    if facts.filetype != 2 {
        fail!("{}: Mach-O type {}, not MH_EXECUTE: this is not the native kernel", kernel.display(), facts.filetype);
    }
    if let Some(native) = build.native_record {
        // kernelcache_kexts_added is not False, or not prelink_skipped
        let skipped = native.get("prelink_skipped").is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
        if native.get("kernelcache_kexts_added") != Some(&Value::Bool(false)) || !skipped {
            fail!("{}: UniXNU's record says it was prelinked; the record is only written for the native kernel", kernel.display());
        }
    }
    let mut patches = Vec::new();
    for patch in gen::key_list(build.inputs, "patches")? {
        patches.push(patch_entry(relative_patch(root, Path::new(gen::key_str(patch, "path")?)), gen::key(patch, "sha256")?.clone()));
    }
    let unixnu_commit = git_head(build.unixnu);
    let mut unixnu_files: Vec<PathBuf> = std::fs::read_dir(root.join("kernel/patches/unixnu")).map(|entries| {
        entries.filter_map(|entry| entry.ok().map(|e| e.path())).filter(|path| path.extension().is_some_and(|e| e == "patch")).collect()
    }).unwrap_or_default();
    unixnu_files.sort();
    let mut unixnu_patches = Vec::new();
    for path in &unixnu_files {
        unixnu_patches.push(patch_entry(relative_patch(root, path), Value::str(gen::sha256(path)?)));
    }
    let xnu_version = config.get("_XNU_VERSION").filter(|v| !v.is_empty()).map(String::as_str).unwrap_or("12377.121.6");
    let restricted: Vec<&str> = RESTRICTED.iter().filter(|(key, _)| config.get(*key).is_some_and(|v| v == "y" || v == "1")).map(|(_, text)| *text).collect();
    let xnu_commit = gen::key(build.inputs, "commit")?;
    let id = build_id(xnu_version, build.profile, &Inputs { xnu_commit, patches: &patches, config, unixnu_commit: &unixnu_commit,
                                                              unixnu_patches: &unixnu_patches, board_sources: build.board_sources });

    let mut macho = Value::dict();
    macho.set("filetype", "MH_EXECUTE");
    macho.set("entry", facts.entry);
    macho.set("chained_fixups", facts.chained_fixups);
    macho.set("segments", facts.segments);
    let mut sources = Value::dict();
    sources.set("unixnu", unixnu_commit.clone());
    sources.set("unixnu_droidvm", git_head(&root.join("external/UniXNU-droidvm")));
    sources.set("xnu_linux_toolchain", if build.host == "linux" { git_head(&root.join("external/xnu-linux-toolchain")) } else { Value::Null });
    sources.set("oneios", git_head(root));
    let mut host = Value::dict();
    host.set("os", build.host);
    host.set("compiler", tool_version(&["clang", "--version"]));
    host.set("linker", if build.host == "linux" { tool_version(&[&gen::s(&root.join("out/toolchain/cctools/bin/ld")), "-v"]) }
                       else { tool_version(&["ld", "-v"]) });

    let mut record = Value::dict();
    record.set("builder", BUILDER);
    record.set("build_id", id);
    record.set("sha256", gen::sha256(kernel)?);
    record.set("bytes", std::fs::metadata(kernel).with_context(|| format!("{}", kernel.display()))?.len());
    record.set("xnu", format!("xnu-{xnu_version}"));
    record.set("xnu_commit", xnu_commit.clone());
    record.set("profile", build.profile);
    record.set("arch", if facts.cpusubtype == 2 { "arm64e" } else { "arm64" });
    record.set("machine_config", "BCM2837");
    record.set("kernel_config", "DEVELOPMENT");
    record.set("defconfig", build.config_name);
    record.set("config", config_dict(&public_config(config)));
    record.set("macho", macho);
    record.set("prelinked", false);
    record.set("kernelcache_kexts_added", false);
    record.set("apple_binary_inputs", Value::list());
    record.set("restricted_sources", list(restricted.iter().copied()));
    record.set("releasable", restricted.is_empty());
    record.set("sources", sources);
    record.set("unixnu_patches", Value::List(unixnu_patches));
    record.set("board_sources", Value::List(build.board_sources.to_vec()));
    record.set("patches", Value::List(patches));
    record.set("host", host);
    record.set("boot_verified", false);
    Ok(record)
}

/// The board's archives (`oneios-tool kernel gen board`): every source that was compiled into them.
fn board_sources(work: &Path) -> Result<Vec<Value>> {
    let mut listings: Vec<PathBuf> = std::fs::read_dir(work.join("tree/build")).map(|entries| {
        entries.filter_map(|entry| entry.ok().map(|e| e.path())).filter(|path| gen::name(path).starts_with("oneios-"))
            .map(|path| path.join("objects.json")).filter(|path| path.exists()).collect()
    }).unwrap_or_default();
    listings.sort();
    let mut sources = Vec::new();
    for listing in listings {
        for source in gen::key_list(&gen::read_json(&listing)?, "sources")? {
            sources.push(patch_entry(gen::key_str(source, "path")?.to_string(), gen::key(source, "sha256")?.clone()));
        }
    }
    Ok(sources)
}

/// `finish`: the record of the build in `work`, written below `out`.
pub fn finish(root: &Path, host: &str, work: &Path, config_name: &str, out: &Path) -> Result<()> {
    let kernel = work.join("kernel/kernel.macho");
    if !kernel.is_file() {
        fail!("{} missing: the kernel step did not finish", kernel.display());
    }
    let inputs = gen::read_json(&work.join("xnu-inputs.json"))?;
    let config = parse_config(&gen::read_text(&work.join(".config"))?);
    let native_path = work.join("kernel/kernel.json");
    let native = if native_path.is_file() { Some(gen::read_json(&native_path)?) } else { None };
    let board = board_sources(work)?;
    let profile = work.parent().map(gen::name).unwrap_or_default();
    let record = make_record(&Build { root, kernel: &kernel, inputs: &inputs, config: &config, profile: &profile, host, config_name,
                                      unixnu: &root.join("external/Droid-VM-UniXNU"), native_record: native.as_ref(), board_sources: &board })?;
    let id = gen::key_str(&record, "build_id")?;
    let target = out.join("builds").join(id);
    gen::mkdirs(&target)?;
    std::fs::write(target.join("xnu.macho"), gen::read_bytes(&kernel)?).with_context(|| format!("{}", target.display()))?;
    gen::write_json(&target.join("xnu.macho.build.json"), &record, 2)?;
    let patches = gen::key_list(&record, "patches")?;
    let series: String = patches.iter().map(|p| Ok(format!("{}  {}\n", gen::key_str(p, "sha256")?, gen::key_str(p, "path")?))).collect::<Result<_>>()?;
    gen::write_text(&target.join("series.txt"), &series)?;
    let current = out.join("current");
    if current.is_symlink() || current.exists() {
        std::fs::remove_file(&current).with_context(|| format!("{}", current.display()))?;
    }
    std::os::unix::fs::symlink(Path::new("builds").join(id), &current).with_context(|| format!("{}", current.display()))?;
    let macho = gen::key(&record, "macho")?;
    println!("kernel: {}", target.join("xnu.macho").display());
    println!("kernel: build id {id}, {}, {} bytes, sha256 {}", gen::key_str(&record, "arch")?, gen::key_int(&record, "bytes")?,
             gen::key_str(&record, "sha256")?);
    println!("kernel: {} board sources linked (drivers/, cpufreq)", board.len());
    let text_at = macho.get("segments").and_then(|s| s.get("__TEXT")).and_then(|t| t.get("vmaddr")).and_then(Value::as_str).unwrap_or("None");
    println!("kernel: entry {}, __TEXT at {text_at}, {} patches", macho.get("entry").and_then(Value::as_str).unwrap_or("None"), patches.len());
    let restricted: Vec<&str> = gen::key_list(&record, "restricted_sources")?.iter().filter_map(Value::as_str).collect();
    if !restricted.is_empty() {
        println!("kernel: NOT releasable (runtime/package.sh refuses it): links {}", restricted.join("; "));
    }
    Ok(())
}

/// A made-up kernel: the header, one LC_SEGMENT_64 (__TEXT) and one LC_UNIXTHREAD with pc.
pub fn fake_kernel() -> Vec<u8> {
    let mut segment = Vec::new();
    segment.extend(0x19u32.to_le_bytes());
    segment.extend(72u32.to_le_bytes());
    segment.extend(b"__TEXT\0\0\0\0\0\0\0\0\0\0");
    for value in [0xfffffff007004000u64, 0x4000, 0, 0x4000] {
        segment.extend(value.to_le_bytes());
    }
    for value in [5u32, 5, 0, 0] {
        segment.extend(value.to_le_bytes());
    }
    let mut thread = Vec::new();
    for value in [0x5u32, 16 + 34 * 8, 6, 68] {
        thread.extend(value.to_le_bytes());
    }
    for value in [0u64; 32].iter().chain([0xfffffff007008000u64].iter()) {
        thread.extend(value.to_le_bytes());
    }
    thread.extend([0u8; 8]);
    let mut kernel = Vec::new();
    for value in [0xfeedfacfu32, 0x0100000c, 2 | (0x80 << 24), 2, 2, (segment.len() + thread.len()) as u32, 0, 0] {
        kernel.extend(value.to_le_bytes());
    }
    kernel.extend(segment);
    kernel.extend(thread);
    kernel
}

/// The checks of the script's --self-test, on files in `directory`.
pub fn self_test(directory: &Path) -> Result<()> {
    let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(anyhow!("build-record: self-test: {what}")) };
    let kernel = directory.join("k");
    std::fs::write(&kernel, fake_kernel())?;
    let patch = directory.join("a.patch");
    std::fs::write(&patch, "x")?;
    let mut inputs = Value::dict();
    inputs.set("commit", "c".repeat(40));
    inputs.set("patches", list([patch_entry(gen::s(&patch), Value::str(gen::sha256(&patch)?))]));
    let config = |pairs: &[(&str, &str)]| -> IndexMap<String, String> { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    let record = |kernel: &Path, config: &IndexMap<String, String>, host: &str, name: &str| {
        make_record(&Build { root: directory, kernel, inputs: &inputs, config, profile: "ios-test", host, config_name: name, unixnu: directory,
                             native_record: None, board_sources: &[] })
    };
    let one = record(&kernel, &config(&[("XNU_ARMCPU", "y")]), "linux", "t_defconfig")?;
    let macho = gen::key(&one, "macho")?;
    check(gen::key_str(&one, "arch")? == "arm64e" && gen::key_str(macho, "entry")? == "0xfffffff007008000", "the architecture and the entry")?;
    check(macho.get("segments").and_then(|s| s.get("__TEXT")).and_then(|t| t.get("vmaddr")).and_then(Value::as_str) == Some("0xfffffff007004000"),
          "__TEXT's address")?;
    check(one.get("releasable") == Some(&Value::Bool(true)) && gen::key_str(&one, "build_id")?.starts_with("xnu-12377.121.6-ios-test-"),
          "a releasable build and its id")?;
    let two = record(&kernel, &config(&[("XNU_ARMCPU", "y"), ("XNU_SOURCE_CORECRYPTO14", "y")]), "linux", "t_defconfig")?;
    check(two.get("releasable") == Some(&Value::Bool(false)) && two.get("build_id") != one.get("build_id"), "a restricted source")?;
    let again = record(&kernel, &config(&[("XNU_ARMCPU", "y")]), "macos", "t_defconfig")?;
    check(again.get("build_id") == one.get("build_id"), "the build id must not depend on the host")?;
    let fileset = directory.join("f");
    let mut header = Vec::new();
    for value in [0xfeedfacfu32, 0x0100000c, 2, 12, 0, 0, 0, 0] {
        header.extend(value.to_le_bytes());
    }
    std::fs::write(&fileset, header)?;
    check(record(&fileset, &config(&[]), "linux", "t").is_err(), "a kernel collection got a record")?;
    Ok(())
}

fn scratch() -> Result<PathBuf> {
    let directory = std::env::temp_dir().join(format!("oneios-build-record-{}", std::process::id()));
    std::fs::create_dir_all(&directory).with_context(|| format!("{}", directory.display()))?;
    Ok(directory)
}

pub fn main(args: Args) -> Result<i32> {
    if args.self_test {
        let directory = scratch()?;
        let result = self_test(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        result?;
        println!("build-record: self-test passed");
        return Ok(0);
    }
    match (args.command.as_deref(), &args.host, &args.work, &args.config, &args.out) {
        (Some("finish"), Some(host), Some(work), Some(config), Some(out)) => {
            let root = gen::resolve(&args.repo.unwrap_or_else(|| PathBuf::from(".")));
            finish(&root, host, &gen::pypath(&gen::s(work)), config, &gen::pypath(&gen::s(out)))?;
            Ok(0)
        }
        _ => {
            eprintln!("oneios-tool kernel build-record: error: finish needs --host, --work, --config and --out");
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scripts_self_test() {
        let directory = super::scratch().unwrap();
        let result = super::self_test(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        result.unwrap();
    }
}
