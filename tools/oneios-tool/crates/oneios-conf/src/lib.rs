//! `oneios-tool switches` and `oneios-tool conf`: xnuhost's switch catalog, and an image's
//! `overlay/userland/xnuhost.conf` read, checked and changed, for the OneOS app's Switches screen
//! (tools/oneios-tool/README.md, "switches" and "conf"). The app parses no conf file and keeps no list of its own.
//!
//! Exit status: 0 done; 1 refused (with `--json` a JSON `{"error": "..."}` on standard output, the file untouched);
//! 2 a wrong command line. No network, no Android API; nothing runs at start but the catalog's few hundred rows.

pub mod catalog;
pub mod conf;
pub mod file;

use catalog::Catalog;
use clap::{Args, Subcommand};
use conf::{Change, Parsed};
use oneios_common::json::{dumps, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

/// `switches`: the catalog.
#[derive(Args, Debug)]
pub struct SwitchesCli {
    /// JSON: {"version": 1, "switches": [...], "keys": [...], "key_prefixes": [...]}
    #[arg(long)]
    json: bool,
    /// the catalog of the runtime in DIR (its lib/switches.json) instead of this tool's own
    #[arg(long, value_name = "DIR")]
    runtime: Option<PathBuf>,
}

/// The options every `conf` command takes.
#[derive(Args, Debug)]
pub struct Common {
    /// print JSON (`conf show`'s, or {"error": "..."})
    #[arg(long)]
    json: bool,
    /// check with the catalog of the runtime in DIR (its lib/switches.json) instead of this tool's own
    #[arg(long, value_name = "DIR")]
    runtime: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// The file as xnuhost reads it: its text, its keys, boot-args-add's words, what xnuhost would refuse
    Show {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// The file's errors (xnuhost would refuse the file) and unknown keys; exit 1 with an error
    Check {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Set switches: NAME=VALUE... (a boot-arg switch goes into boot-args-add=); all or none
    Set {
        file: PathBuf,
        #[arg(required = true, value_name = "NAME=VALUE")]
        pairs: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Return switches to their defaults (their lines or words removed)
    Unset {
        file: PathBuf,
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Write the whole new text from standard input, when it has no errors (--force: even then)
    Write {
        file: PathBuf,
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Put FILE.bak back (and the text it replaces becomes FILE.bak)
    Restore {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
}

fn print(v: &Value) {
    println!("{}", dumps(v, None, false));
}

/// A refusal: exit 1, the file untouched.
struct Refused {
    message: String,
    /// more fields for the JSON (errors, unknown, warnings)
    more: Option<Value>,
}

impl Refused {
    fn new(message: impl Into<String>) -> Refused {
        Refused {
            message: message.into(),
            more: None,
        }
    }
}

fn refuse(json: bool, r: Refused) -> i32 {
    if json {
        let mut v = Value::dict();
        v.set("error", r.message.as_str());
        if let Some(Value::Dict(items)) = r.more {
            for (k, val) in items {
                v.set(k, val);
            }
        }
        print(&v);
    } else {
        eprintln!("oneios-tool: {}", r.message);
        if let Some(errors) = r
            .more
            .as_ref()
            .and_then(|m| m.get("errors"))
            .and_then(Value::as_list)
        {
            for e in errors {
                eprintln!(
                    "  line {}: {}",
                    e.get("line").and_then(Value::as_int).unwrap_or(0),
                    e.get("message").and_then(Value::as_str).unwrap_or("")
                );
            }
        }
    }
    1
}

pub fn switches_main(cli: SwitchesCli) -> i32 {
    let cat = match Catalog::load(cli.runtime.as_deref()) {
        Ok(c) => c,
        Err(e) => return refuse(cli.json, Refused::new(e)),
    };
    if cli.json {
        print(&cat.json);
        return 0;
    }
    for s in cat
        .json
        .get("switches")
        .and_then(Value::as_list)
        .unwrap_or(&[])
    {
        let f = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let warn = if s.get("warning").is_some_and(|w| !w.is_null()) {
            "  (warning)"
        } else {
            ""
        };
        println!(
            "{:<24} {:<8} {:<10} {:<9} {}{warn}",
            f("name"),
            f("where"),
            f("default"),
            f("group"),
            f("title")
        );
    }
    0
}

/// The text of FILE ("" when there is none) and whether it was there.
fn read_text(path: &Path) -> Result<(String, bool), Refused> {
    match file::read(path) {
        Ok(Some(b)) => Ok((String::from_utf8_lossy(&b).into_owned(), true)),
        Ok(None) => Ok((String::new(), false)),
        Err(e) => Err(Refused::new(format!("{}: {e}", path.display()))),
    }
}

/// `conf show`'s JSON.
fn show_json(path: &Path, text: &str, exists: bool, p: &Parsed) -> Value {
    let mut v = Value::dict();
    v.set("path", path.to_string_lossy().as_ref());
    v.set("text", text);
    v.set("set", Parsed::pairs_json(&p.set));
    v.set("boot_args_add", Parsed::pairs_json(&p.boot_args_add));
    v.set("unknown", p.unknown_json());
    v.set("errors", Parsed::notes_json(&p.errors));
    v.set("warnings", Parsed::notes_json(&p.warnings));
    v.set("exists", exists);
    v.set("backup", file::backup_path(path).exists());
    v
}

fn findings_json(p: &Parsed) -> Value {
    let mut v = Value::dict();
    v.set("errors", Parsed::notes_json(&p.errors));
    v.set("unknown", p.unknown_json());
    v.set("warnings", Parsed::notes_json(&p.warnings));
    v
}

fn show_text(p: &Parsed) {
    for (k, v) in &p.set {
        println!("{k}={v}");
    }
    if !p.boot_args_add.is_empty() {
        let words: Vec<String> = p
            .boot_args_add
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        println!("boot-args-add: {}", words.join(" "));
    }
    for e in &p.errors {
        println!("error: line {}: {}", e.line, e.message);
    }
    for w in &p.warnings {
        println!("warning: line {}: {}", w.line, w.message);
    }
}

/// After a change: the new file's `show` (with --json), or nothing.
fn done(path: &Path, cat: &Catalog, json: bool) -> Result<i32, Refused> {
    if json {
        let (text, exists) = read_text(path)?;
        print(&show_json(path, &text, exists, &conf::parse(cat, &text)));
    }
    Ok(0)
}

fn write_file(path: &Path, text: &str) -> Result<(), Refused> {
    file::replace(path, text.as_bytes(), true)
        .map_err(|e| Refused::new(format!("{}: {e} (the file is as it was)", path.display())))
}

/// The file's text for a change: it must be UTF-8 (a change keeps every other byte as it is).
fn text_for_change(path: &Path) -> Result<String, Refused> {
    match file::read(path) {
        Ok(Some(b)) => String::from_utf8(b).map_err(|_| {
            Refused::new(format!(
                "{}: not UTF-8 text; edit it whole (`conf write`)",
                path.display()
            ))
        }),
        Ok(None) => Ok(String::new()),
        Err(e) => Err(Refused::new(format!("{}: {e}", path.display()))),
    }
}

fn run(command: Command) -> Result<i32, (bool, Refused)> {
    let (json, runtime) = match &command {
        Command::Show { common, .. }
        | Command::Check { common, .. }
        | Command::Set { common, .. }
        | Command::Unset { common, .. }
        | Command::Write { common, .. }
        | Command::Restore { common, .. } => (common.json, common.runtime.clone()),
    };
    let err = |r: Refused| (json, r);
    let cat = Catalog::load(runtime.as_deref()).map_err(|e| err(Refused::new(e)))?;
    match command {
        Command::Show { file, .. } => {
            let (text, exists) = read_text(&file).map_err(err)?;
            let p = conf::parse(&cat, &text);
            if json {
                print(&show_json(&file, &text, exists, &p));
            } else {
                show_text(&p);
            }
            Ok(0)
        }
        Command::Check { file, .. } => {
            let (text, _) = read_text(&file).map_err(err)?;
            let p = conf::parse(&cat, &text);
            if json {
                print(&findings_json(&p));
            } else {
                for e in &p.errors {
                    eprintln!("{}:{}: {}", file.display(), e.line, e.message);
                }
                for w in &p.warnings {
                    eprintln!("{}:{}: warning: {}", file.display(), w.line, w.message);
                }
            }
            Ok(if p.errors.is_empty() { 0 } else { 1 })
        }
        Command::Set { file, pairs, .. } => {
            let mut changes: Vec<Change> = Vec::new();
            for pair in &pairs {
                let Some((name, value)) = pair.split_once('=') else {
                    eprintln!("oneios-tool conf set: {pair:?} is not NAME=VALUE");
                    return Ok(2);
                };
                changes.push((name.to_string(), Some(value.to_string())));
            }
            change(&cat, &file, &changes, json).map_err(err)
        }
        Command::Unset { file, names, .. } => {
            let changes: Vec<Change> = names.iter().map(|n| (n.clone(), None)).collect();
            change(&cat, &file, &changes, json).map_err(err)
        }
        Command::Write { file, force, .. } => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|e| err(Refused::new(format!("standard input: {e}"))))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| err(Refused::new("standard input is not UTF-8 text")))?;
            let p = conf::parse(&cat, &text);
            if !p.errors.is_empty() && !force {
                return Err(err(Refused {
                    message: format!(
                        "the text has {} error{}: xnuhost would refuse the file; nothing written (--force writes it anyway)",
                        p.errors.len(),
                        if p.errors.len() == 1 { "" } else { "s" }
                    ),
                    more: Some(findings_json(&p)),
                }));
            }
            write_file(&file, &text).map_err(err)?;
            done(&file, &cat, json).map_err(err)
        }
        Command::Restore { file, .. } => {
            let bak = file::backup_path(&file);
            if !bak.exists() {
                return Err(err(Refused::new(format!("{}: no backup", bak.display()))));
            }
            file::restore(&file)
                .map_err(|e| err(Refused::new(format!("{}: {e}", file.display()))))?;
            done(&file, &cat, json).map_err(err)
        }
    }
}

fn change(cat: &Catalog, path: &Path, changes: &[Change], json: bool) -> Result<i32, Refused> {
    let text = text_for_change(path)?;
    let new = conf::edit(cat, &text, changes).map_err(Refused::new)?;
    if new != text {
        write_file(path, &new)?;
    }
    done(path, cat, json)
}

pub fn main(command: Command) -> i32 {
    match run(command) {
        Ok(code) => code,
        Err((json, r)) => refuse(json, r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn change_writes_only_a_change() {
        let d = std::env::temp_dir().join(format!("oneios-conf-lib-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let f = d.join("xnuhost.conf");
        let cat = Catalog::builtin();
        // a missing file: show is an empty file; unset of a default writes nothing
        assert_eq!(
            change(&cat, &f, &[("astc".into(), None)], false).ok(),
            Some(0)
        );
        assert!(!f.exists());
        assert_eq!(
            change(&cat, &f, &[("astc".into(), Some("0".into()))], false).ok(),
            Some(0)
        );
        assert_eq!(fs::read_to_string(&f).unwrap(), "astc=0\n");
        // a refusal leaves it as it was
        assert!(change(&cat, &f, &[("astc".into(), Some("2".into()))], false).is_err());
        assert_eq!(fs::read_to_string(&f).unwrap(), "astc=0\n");
        let p = conf::parse(&cat, "astc=0\n");
        let v = show_json(&f, "astc=0\n", true, &p);
        assert_eq!(
            v.get("set")
                .and_then(|s| s.get("astc"))
                .and_then(Value::as_str),
            Some("0")
        );
        assert_eq!(v.get("backup").and_then(Value::as_bool), Some(false));
        let _ = fs::remove_dir_all(&d);
    }
}
