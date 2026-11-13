//! What the two lead tools (lead_between.rs, lead_imports.rs; were kernel/prelink/lead/between.py and imports.py)
//! share: the files they read, at the places the scripts had fixed, and llvm-nm's lines of the native kernel.
//!
//!   kernel/prelink/reviewed-24A446-v54.json       the reviewed records
//!   kernel/prelink/lead/lead-reviewed.json        the lead's scratch records (made by --add; none at first)
//!   out/kernel/24A446/work/match.map.json         kernel/prelink/build.sh's output: the matcher's pairs
//!   out/kernel/24A446/kernel.kc.prelink.json      and the emitter's report
//!   out/kernel-inputs/24A446/kernelcache.macho    the original kernelcache
//!   out/kernel/current-hosted/xnu.macho           the native kernel (llvm-nm reads it)

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::pyjson::{self, Value};

pub struct Lead {
    pub reviewed: Value,
    pub lead_path: PathBuf,
    pub lead: Value,
    pub map: Value,
    pub report: Value,
    /// The original kernelcache (each tool maps it itself).
    pub kernelcache: PathBuf,
    pub native: PathBuf,
}

impl Lead {
    pub fn open(repo: Option<&str>) -> Result<Lead> {
        let root = Path::new(repo.unwrap_or("."));
        let kc = root.join("out/kernel/24A446"); // kernel/prelink/build.sh's output
        let lead_path = root.join("kernel/prelink/lead/lead-reviewed.json");
        let lead = if lead_path.exists() {
            pyjson::load(&lead_path)?
        } else {
            let mut none = Value::dict();
            none.set("functions", Value::list());
            none
        };
        Ok(Lead {
            reviewed: pyjson::load(root.join("kernel/prelink/reviewed-24A446-v54.json"))?,
            lead,
            lead_path,
            map: pyjson::load(kc.join("work/match.map.json"))?,
            report: pyjson::load(kc.join("kernel.kc.prelink.json"))?,
            kernelcache: root.join("out/kernel-inputs/24A446/kernelcache.macho"),
            native: root.join("out/kernel/current-hosted/xnu.macho"),
        })
    }

    /// The reviewed and the lead's function records, in that order.
    pub fn reviewed_functions(&self) -> Result<impl Iterator<Item = &Value>> {
        Ok(list(&self.reviewed, "functions")?.iter().chain(list(&self.lead, "functions")?))
    }

    /// `llvm-nm ARGS xnu.macho`: the lines of three words, as (address, type, name).
    pub fn nm(&self, args: &[&str]) -> Result<Vec<(u64, String, String)>> {
        let mut command = Command::new("llvm-nm");
        // llvm-nm: keg-only on a Mac
        let mut path: Vec<PathBuf> = ["/opt/homebrew/opt/llvm/bin", "/usr/local/opt/llvm/bin"].iter().map(PathBuf::from).filter(|d| d.is_dir()).rev().collect();
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        command.env("PATH", std::env::join_paths(path)?).args(args).arg(&self.native);
        let out = command.output().context("llvm-nm")?;
        let mut rows = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let p: Vec<&str> = line.split_whitespace().collect();
            if let [address, kind, name] = p[..] {
                rows.push((u64::from_str_radix(address, 16).with_context(|| format!("llvm-nm: {line}"))?, kind.to_string(), name.to_string()));
            }
        }
        Ok(rows)
    }

    /// Append a record to the lead's functions.
    pub fn add(&mut self, record: Value) {
        self.lead.get_mut("functions").expect("the lead's functions").push(record);
    }

    pub fn save(&self) -> Result<()> {
        std::fs::write(&self.lead_path, pyjson::dumps(&self.lead, Some(1), false) + "\n").with_context(|| self.lead_path.display().to_string())
    }
}

pub fn list<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value.get(key).and_then(Value::as_list).ok_or_else(|| anyhow!("no list \"{key}\" in a record"))
}

pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| anyhow!("no \"{key}\" in a record"))
}

/// A record's "original" address.
pub fn original(value: &Value) -> Result<u64> {
    crate::image::unhex(text(value, "original")?)
}

/// A reviewed record for one function: what both tools append to the lead's list.
pub fn function_record(address: u64, native_symbol: &str, bytes: &[u8], evidence: String) -> Value {
    let hex = crate::image::hex(address);
    let mut check = Value::dict();
    check.set("address", hex.as_str());
    check.set("size", bytes.len());
    check.set("sha256", oneios_common::sha256_hex(bytes));
    let mut record = Value::dict();
    record.set("original", hex);
    record.set("native_symbol", native_symbol);
    record.set("check", check);
    record.set("evidence", evidence);
    record
}

/// The tools' `sys.exit(message)`.
pub fn stop<T>(message: String) -> Result<T> {
    bail!("{message}")
}
