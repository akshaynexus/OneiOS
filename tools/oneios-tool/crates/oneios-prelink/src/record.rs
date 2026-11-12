//! `prelink record`: the build record of kernel.kc (kernel.kc.build.json): inputs, tools, what went in, what was
//! left out and why. A port of kernel/prelink/record.py: the same file byte for byte and the same line on stdout.
//! `--repo` names the checkout (record.py takes its own place in it): the reviewed file's path is written
//! relative to it, and its HEAD is the record's "oneios".

use crate::image::{cstr_at, hex, u32_at, u64_at};
use crate::records::truthy;
use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use oneios_common::sha256_hex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    work: String,
    #[arg(long)]
    kernel: String,
    #[arg(long)]
    kernelcache: String,
    #[arg(long)]
    reviewed: String,
    #[arg(long)]
    xnuprelink: String,
    #[arg(long)]
    output: String,
    #[arg(long)]
    reference: Option<String>,
    /// The OneiOS checkout (default: the git checkout the current folder is in)
    #[arg(long)]
    repo: Option<String>,
}

fn sha(path: &str) -> Result<String> {
    Ok(sha256_hex(&oneios_common::Map::open(path)?))
}

fn git(path: &Path, command: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(path).args(command).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn resolve(path: &str) -> Result<PathBuf> {
    std::fs::canonicalize(path).with_context(|| path.to_string())
}

fn key<'v>(value: &'v Value, name: &str) -> Result<&'v Value> {
    value.get(name).ok_or_else(|| anyhow!("a record of the build has no \"{name}\""))
}

/// The distinct inner keys of a dict of dicts.
fn inner_keys(value: &Value) -> usize {
    value.as_dict().unwrap_or(&[]).iter().flat_map(|(_, targets)| targets.as_dict().unwrap_or(&[]).iter().map(|(k, _)| k.as_str())).collect::<BTreeSet<_>>().len()
}

pub fn run(args: Args) -> Result<()> {
    let work = Path::new(&args.work);
    let load = |name: &str| pyjson::load(work.join(name));
    let (select, hosted, prelink, pairing) = (load("select.json")?, load("hosted.json")?, load("kernel.kc.prelink.json")?, load("match.map.json")?);
    let reviewed = pyjson::load(&args.reviewed)?;
    let image = oneios_common::Map::open(work.join("kernel.kc"))?;
    let mut entries = Vec::new();
    let mut off = 32usize;
    for _ in 0..u32_at(&image, 16) {
        let (cmd, length) = (u32_at(&image, off), u32_at(&image, off + 4) as usize);
        if cmd == 0x8000_0035 {
            let mut row = Value::dict();
            row.set("name", cstr_at(&image, off + u32_at(&image, off + 24) as usize)?);
            row.set("address", hex(u64_at(&image, off + 8)));
            row.set("file_offset", hex(u64_at(&image, off + 16)));
            entries.push(row);
        }
        off += length;
    }
    let root = match &args.repo {
        Some(repo) => resolve(repo)?,
        None => PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"]).ok_or_else(|| anyhow!("not inside a git checkout: give --repo"))?),
    };
    let (stubbed, missing) = (inner_keys(key(&prelink, "stubbed")?), inner_keys(key(&prelink, "missing")?));
    let file = |path: &str, what: Option<&str>| -> Result<Value> {
        let mut row = Value::dict();
        row.set("path", resolve(path)?.to_string_lossy().into_owned());
        row.set("sha256", sha(path)?);
        if let Some(what) = what {
            row.set("what", what);
        }
        Ok(row)
    };
    let mut record = Value::dict();
    record.set("output", "kernel.kc");
    record.set("format", "MH_FILESET");
    record.set("bytes", Value::Int(image.len() as i128));
    record.set("sha256", sha256_hex(&image));
    record.set("releasable", false);
    record.set("why_not_releasable", "contains the kexts of Apple's kernelcache (and the kernel links UniXNU's source corecrypto)");
    let mut inputs = Value::dict();
    inputs.set("kernel", file(&args.kernel, None)?);
    inputs.set("kernelcache", file(&args.kernelcache, Some("iPhone18,2 iOS 27.0.1 (24A446) kernelcache.release.v54, decoded"))?);
    inputs.set("reference_kernel", args.reference.as_ref().map(|r| file(r, None)).transpose()?);
    let mut row = Value::dict();
    // PATH relative to the repository when it lies in it, else as given
    let real = resolve(&args.reviewed)?;
    row.set("path", real.strip_prefix(&root).unwrap_or(&real).to_string_lossy().into_owned());
    row.set("sha256", sha(&args.reviewed)?);
    inputs.set("reviewed", row);
    record.set("inputs", inputs);
    let mut tools = Value::dict();
    tools.set("oneios", git(&root, &["rev-parse", "HEAD"]));
    tools.set("xnuprelink", git(Path::new(&args.xnuprelink), &["rev-parse", "HEAD"]));
    tools.set("matcher_version", pairing.get("matcher_version").cloned().unwrap_or(Value::Null));
    tools.set("matcher_signature", pairing.get("matcher_signature").cloned().unwrap_or(Value::Null));
    record.set("tools", tools);
    record.set("native_input", key(&load("native-input.json")?, "weak_binds_resolved")?.clone());
    let left_out = key(&select, "left_out")?.as_dict().ok_or_else(|| anyhow!("select.json: left_out is not a dictionary"))?;
    let closure: BTreeSet<&str> = key(&select, "closure")?.as_list().unwrap_or(&[]).iter().filter_map(Value::as_str).collect();
    let mut kexts = Value::dict();
    kexts.set("in_kernelcache", key(&select, "kernelcache_executables")?.clone());
    kexts.set("roots", key(&select, "roots")?.clone());
    kexts.set("dependencies_without_personalities", key(&prelink, "personalities_dropped")?.clone());
    kexts.set("closure", key(&select, "closure")?.clone());
    let mut reasons = Value::dict();
    for (name, entry) in left_out {
        let reason = key(entry, "reason")?.as_str().unwrap_or("");
        reasons.set(name, format!("{reason}{}", if entry.get("returns_as_dependency_of").is_none() { "" } else { "; its code returns as a dependency, without its personalities" }));
    }
    kexts.set("left_out", reasons);
    let absent: Vec<String> = left_out.iter().map(|(k, _)| k.as_str()).collect::<BTreeSet<_>>().difference(&closure).map(|k| k.to_string()).collect();
    let absent_count = absent.len();
    kexts.set("left_out_and_absent", absent);
    record.set("kexts", kexts);
    let count = entries.len();
    record.set("fileset_entries", Value::List(entries));
    let mut host = Value::dict();
    for name in ["convention", "tpidr_el1_rewrites", "mpidr_el1_rewrites", "kernel_pointer_rewrites", "kext_code_bytes_scanned", "privileged_instructions_total", "privileged_instructions_by_kext", "x18_checked", "x18_instructions_by_kext"] {
        host.set(name, key(&hosted, name)?.clone());
    }
    record.set("hosted", host);
    let applied = key(&prelink, "reviewed_applied")?;
    let mut amfi = Value::dict();
    amfi.set("early_security_init", key(&prelink, "early_security_init")?.clone());
    amfi.set("page_shift_site", key(applied, "sites")?.clone());
    amfi.set("launch_constraints", "kernel side: zzyc-owner-skip-launch-constraints.patch (boot argument sheng-no-lc=1) and /chosen/amfi-only-platform-code; nothing in the collection is changed for them");
    record.set("amfi", amfi);
    let functions = reviewed.get("functions").and_then(Value::as_list).unwrap_or(&[]);
    let ported = functions.iter().filter(|r| truthy(r.get("ported"))).count();
    let mut pairs = Value::dict();
    pairs.set("statistics", pairing.get("stats").cloned().unwrap_or(Value::Null));
    pairs.set("reviewed_applied", Value::Dict(applied.as_dict().unwrap_or(&[]).iter().map(|(k, v)| (k.clone(), Value::Int(v.as_list().map_or(0, <[Value]>::len) as i128))).collect()));
    pairs.set("reviewed_hand", Value::Int((functions.len() - ported) as i128));
    pairs.set("reviewed_ported", Value::Int(ported as i128));
    pairs.set("original_functions_bound_to_the_panic_stub", Value::Int(stubbed as i128));
    pairs.set("original_data_left_at_collection_addresses", Value::Int(missing as i128));
    pairs.set("detail", "kernel.kc.prelink.json (stubbed, missing: per kext)");
    record.set("pairing", pairs);
    record.set("mapping_complete", key(&prelink, "mapping_complete")?.clone());
    record.set("runtime_qualified", false);
    std::fs::write(&args.output, pyjson::dumps(&record, Some(1), false) + "\n").with_context(|| args.output.clone())?;
    println!(
        "record: {count} fileset entries ({} roots, {} in the closure, {absent_count} kexts absent), {stubbed} original functions on the stub, {missing} data targets unpaired",
        key(&select, "roots")?.as_list().map_or(0, <[Value]>::len),
        closure_len(&select)
    );
    Ok(())
}

fn closure_len(select: &Value) -> usize {
    select.get("closure").and_then(Value::as_list).map_or(0, <[Value]>::len)
}
