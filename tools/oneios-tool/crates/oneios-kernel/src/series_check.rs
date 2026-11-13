//! `oneios-tool kernel series-check [--repo CHECKOUT]` (was kernel/tests/series-check.py, retired 2026-11-13):
//! checks of the kernel's series and profile that need no sources: every entry of the board series is a path into
//! a pinned external repository or a patch of this directory that exists; no patch is listed twice; our own
//! patches are all listed; the OS profile and the board parse and name the iPhone18,2 identity; the defconfig has
//! no unknown shape. Each failed check is a line `series-check: ...` on stderr; exit 1 when there is one.
//! Run by kernel/tests/host.sh.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use toml::{Table, Value};

fn table(path: &Path) -> Result<Table> {
    std::fs::read_to_string(path).with_context(|| path.display().to_string())?.parse().with_context(|| path.display().to_string())
}

/// `table[a][b]...`: a missing key ends the check, as Python's KeyError did.
fn at<'a>(table: &'a Table, keys: &[&str]) -> Result<&'a Value> {
    let mut value = table.get(keys[0]);
    for key in &keys[1..] {
        value = value.and_then(|v| v.get(key));
    }
    value.ok_or_else(|| anyhow!("series-check: no [{}] in a profile", keys.join("][")))
}

fn text<'a>(table: &'a Table, keys: &[&str]) -> Result<&'a str> {
    at(table, keys)?.as_str().ok_or_else(|| anyhow!("series-check: [{}] is not a string", keys.join("][")))
}

/// Python's `repr()` of a value of a profile (a string or a number).
fn repr(value: &Value) -> String {
    match value {
        Value::String(s) => oneios_common::pyrepr::repr_str(s),
        Value::Boolean(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Integer(i) => i.to_string(),
        other => format!("{other:?}"),
    }
}

/// The board series: its entries exist, none twice, and every patch of the folder is listed.
fn series(root: &Path, errors: &mut Vec<String>) -> Result<()> {
    let series = root.join("kernel/device/patches/xnu-12377/series");
    let folder = series.parent().unwrap();
    let external = Regex::new(r"^\.\./\.\./\.\./\.\./external/(UniXNU-droidvm|xnu-linux-toolchain|Droid-VM-UniXNU)/[A-Za-z0-9_./-]+\.patch$").unwrap();
    let content = std::fs::read_to_string(&series).with_context(|| series.display().to_string())?;
    let entries: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if entries.len() != entries.iter().collect::<BTreeSet<_>>().len() {
        errors.push("a patch is listed twice in the board series".to_string());
    }
    let mut own = BTreeSet::new();
    for entry in &entries {
        let path = folder.join(entry);
        if entry.starts_with("../") {
            if !external.is_match(entry) {
                errors.push(format!("series: {entry} is not a patch of a pinned external repository"));
            } else if root.join("external").is_dir() && path.parent().is_some_and(Path::is_dir) && !path.is_file() {
                // (only where the reference is checked out: CI has no external/)
                errors.push(format!("series: {entry} does not exist at the pin"));
            }
        } else {
            own.insert(entry.to_string());
            if !path.is_file() {
                errors.push(format!("series: {entry} missing"));
            }
        }
    }
    let mut patches: Vec<String> = std::fs::read_dir(folder)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    patches.sort();
    for name in patches.iter().filter(|n| n.ends_with(".patch") && !own.contains(*n)) {
        errors.push(format!("{name} is not in the series"));
    }
    Ok(())
}

/// The OS profile against the image profile, the pins and the board.
fn profiles(root: &Path, errors: &mut Vec<String>) -> Result<()> {
    let profile = table(&root.join("kernel/unixnu/profiles/os/ios-27.0.1-24A446/profile.toml"))?;
    if text(&profile, &["os", "arch"])? != "arm64e" || text(&profile, &["os", "xnu_version"])? != "12377.121.6" {
        errors.push("OS profile: not arm64e on xnu-12377.121.6".to_string());
    }
    if at(&profile, &["adt", "root", "string", "model"])?.as_str() != Some("iPhone18,2") || at(&profile, &["ipsw", "product"])?.as_str() != Some("iPhone18,2") {
        errors.push("OS profile: the identity is not iPhone18,2".to_string());
    }
    let image_path = root.join("profiles/ios-27.0.1-iPhone18,2.toml");
    if image_path.is_file() {
        let image = table(&image_path)?;
        for (ours, theirs) in [("url", ["ipsw", "url"]), ("build", ["os", "build"]), ("device_class", ["os", "board"]), ("chip_id", ["os", "chip_id"]), ("board_id", ["os", "board_id"])] {
            let (mine, other) = (at(&profile, &["ipsw", ours])?, at(&image, &theirs)?);
            if mine != other {
                errors.push(format!("OS profile [ipsw] {ours} = {}, the image profile says {}", repr(mine), repr(other)));
            }
        }
    }
    let pins = std::fs::read_to_string(root.join("kernel/pins.sh")).context("kernel/pins.sh")?;
    if !pins.contains(&format!("XNU_COMMIT={}", text(&profile, &["os", "xnu_revision"])?)) {
        errors.push("pins.sh XNU_COMMIT differs from the OS profile".to_string());
    }
    let board = table(&root.join("kernel/device/board.toml"))?;
    if text(&board, &["board", "os_profile"])? != text(&profile, &["os", "name"])? {
        errors.push("board.toml os_profile is not the OS profile".to_string());
    }
    let config = root.join("kernel/device/configs").join(text(&board, &["kernel", "defconfig"])?);
    let shape = Regex::new(r#"^CONFIG_XNU_[A-Z0-9_]+=(y|n|"[^"]*")$"#).unwrap();
    for line in std::fs::read_to_string(&config).with_context(|| config.display().to_string())?.lines() {
        if !line.trim().is_empty() && !line.starts_with('#') && !shape.is_match(line) {
            errors.push(format!("defconfig: bad line {}", repr(&Value::String(line.to_string()))));
        }
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    series(root, &mut errors)?;
    profiles(root, &mut errors)?;
    Ok(errors)
}

pub fn main(repo: Option<&Path>) -> Result<i32> {
    let errors = check(repo.unwrap_or(Path::new(".")))?;
    for error in &errors {
        eprintln!("series-check: {error}");
    }
    Ok(if errors.is_empty() { 0 } else { 1 })
}
