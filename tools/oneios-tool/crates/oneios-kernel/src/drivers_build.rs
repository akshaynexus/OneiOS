//! `oneios-tool kernel drivers [--records DIR] [--out DIR] [--check] [--repo CHECKOUT]` (was drivers/build.py,
//! retired 2026-11-13): builds OneiOS's kernel-side drivers (the repository's drivers/) into one archive for the
//! kernel link.
//!
//! Each source is compiled with the command the XNU build recorded for a source of the same universe (the build
//! leaves one JSON record per object under build/xnu/ios-obj/<CONFIG>/), with the source and output replaced and
//! the board's warning options, the way UniXNU-droidvm's ipados27/scripts/build-droidvm-devices.py builds its
//! device sources:
//!
//!   mach    record osfmk/DEVELOPMENT/startup.o.json              (bridge/OneiOSBridge.c, camera/oneios_camera.c)
//!   bsd     record bsd/DEVELOPMENT/kern_control.o.json           (haptics/*.c, bluetooth/*.c, camera/oneios_camera_dev.c)
//!   iokit   record iokit/DEVELOPMENT/IOPlatformExpert.cpo.json   (the .cpp sources)
//!
//! The sources and their universes are drivers/sources.txt (the script's SOURCES list: it stays with the drivers,
//! so that a new driver is one line there and no change of this tool).
//!
//! --records  default: the newest build under out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/*
//! --out      default: out/drivers  ->  liboneiosdrivers.a, objects.json (path and SHA-256 of each source)
//! --check    compile only (no archive): the syntax and type check of a change, a few seconds
//!
//! The archive enters the kernel through CONFIG_XNU_EXTRA_LD_ARCHIVES (kernel/README.md, "Linking a board
//! archive"); oneios_drivers_start() is its one entry point. No Apple file is read or written.
//!
//! `record` and `command` are also what the board archive generators use (the scripts imported drivers/build.py
//! as a module); `oneios-tool kernel compile-command` prints `command`'s result for a record.

use crate::gen::{self, fail, read_text, resolve, s};
use anyhow::{anyhow, Context, Result};
use oneios_common::pyjson::{self, Value};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    /// The kernel build's compile records (default: the newest build under out/kernel/unixnu)
    #[arg(long)]
    records: Option<PathBuf>,
    /// Where the objects, the archive and objects.json go (default: out/drivers)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Compile only (no archive)
    #[arg(long)]
    check: bool,
    /// This repository's checkout (default: the current directory)
    #[arg(long)]
    repo: Option<PathBuf>,
}

/// `RECORDS[universe]`: the record whose command a source of that universe is compiled with.
pub fn record_name(universe: &str) -> &'static str {
    match universe {
        "mach" => "osfmk/DEVELOPMENT/startup.o.json",
        "bsd" => "bsd/DEVELOPMENT/kern_control.o.json",
        "iokit" => "iokit/DEVELOPMENT/IOPlatformExpert.cpo.json",
        other => panic!("no compile record for the universe {other}"),
    }
}

/// drivers/sources.txt: `UNIVERSE PATH` per line (the path below drivers/), `#` starts a comment.
pub fn sources(here: &Path) -> Result<Vec<(String, String)>> {
    let list = here.join("sources.txt");
    let mut out = Vec::new();
    for line in read_text(&list)?.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty()) {
        match line.split_whitespace().collect::<Vec<_>>()[..] {
            [universe @ ("mach" | "bsd" | "iokit"), name] => out.push((universe.to_string(), name.to_string())),
            _ => fail!("drivers: {}: not `mach|bsd|iokit PATH`: {line}", list.display()),
        }
    }
    Ok(out)
}

/// The newest kernel build's records under the checkout's out/kernel (by the time of its startup.o.json).
fn find_records(root: &Path) -> Option<PathBuf> {
    let subfolders = |dir: PathBuf| std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path());
    subfolders(root.join("out/kernel/unixnu/oneios"))
        .flat_map(|profile| subfolders(profile.join("oneios/tree/build/xnu/ios-obj")))
        .filter_map(|config| Some((std::fs::metadata(config.join(record_name("mach"))).ok()?.modified().ok()?, config)))
        .max_by_key(|(time, _)| *time)
        .map(|(_, config)| config)
}

/// Compile every source of drivers/ with its universe's record; then (without `check`) the archive and objects.json.
pub fn build(here: &Path, records: &Path, out: &Path, check: bool) -> Result<()> {
    gen::mkdirs(&out.join("objects"))?;
    let (mut objects, mut listing) = (Vec::new(), Value::list());
    for (universe, name) in sources(here)? {
        let rec = record(records, record_name(&universe))?;
        let src = here.join(&name);
        let dst = out.join("objects").join(format!("{}.o", gen::name(Path::new(&name))));
        let result = gen::capture(&command(here, &rec, &src, &dst), Some(Path::new(&rec.directory)), Some(600))?;
        if result.code != 0 {
            eprint!("{}", result.stderr);
            fail!("drivers: {name} did not compile");
        }
        objects.push(dst);
        listing.push(gen::source_entry(format!("drivers/{name}"), Some(&universe), &src)?);
        println!("drivers: {name}");
    }
    if check {
        println!("drivers: {} sources compile against {}", objects.len(), records.display());
        return Ok(());
    }
    let archive = out.join("liboneiosdrivers.a");
    gen::archive(&archive, &objects)?;
    let mut value = Value::dict();
    value.set("records", s(records));
    value.set("sources", listing);
    gen::write_json(&out.join("objects.json"), &value, 2)?;
    println!("drivers: {} ({} bytes)", archive.display(), std::fs::metadata(&archive).with_context(|| s(&archive))?.len());
    Ok(())
}

pub fn main(args: Args) -> Result<()> {
    let here = here(&args.repo.unwrap_or_else(|| PathBuf::from(".")))?;
    let root = here.parent().ok_or_else(|| anyhow!("{} has no directory", here.display()))?;
    // (pathlib drops a trailing separator: the path is printed and recorded as the script did)
    let records = args.records.map(|r| gen::pypath(&s(&r))).or_else(|| find_records(root));
    let Some(records) = records.filter(|r| r.join(record_name("mach")).is_file()) else {
        fail!("drivers: no XNU build records (build the kernel first: scripts/bg.sh kernel kernel/build-linux.sh), or pass --records");
    };
    build(&here, &records, &args.out.map_or_else(|| root.join("out/drivers"), |o| gen::pypath(&s(&o))), args.check)
}

pub struct Record {
    pub directory: String,
    pub file: String,
    pub arguments: Vec<String>,
}

/// `json.loads((records / name).read_text().strip().rstrip(','))`: the build leaves one JSON object per file,
/// with the comma of the compilation database it is later joined into.
pub fn record(records: &Path, name: &str) -> Result<Record> {
    let path = records.join(name);
    parse_record(&read_text(&path)?).map_err(|e| anyhow!("{}: {e}", path.display()))
}

pub fn parse_record(text: &str) -> Result<Record> {
    let value = pyjson::parse(text.trim().trim_end_matches(',')).map_err(|e| anyhow!("{e}"))?;
    let text_of = |k: &str| -> Result<String> {
        Ok(value.get(k).and_then(Value::as_str).ok_or_else(|| anyhow!("the record has no {k}"))?.to_string())
    };
    let arguments = value.get("arguments").and_then(Value::as_list).ok_or_else(|| anyhow!("the record has no arguments"))?
        .iter().map(|a| a.as_str().map(str::to_string).ok_or_else(|| anyhow!("an argument is not a string")))
        .collect::<Result<Vec<_>>>()?;
    Ok(Record { directory: text_of("directory")?, file: text_of("file")?, arguments })
}

/// `HERE` of drivers/build.py: `Path(__file__).resolve().parent`, the real path of the repository's drivers/.
pub fn here(repo: &Path) -> Result<PathBuf> {
    Ok(resolve(&repo.join("drivers")))
}

/// The recorded compile command with another source and output, and the board's warnings.
pub fn command(here: &Path, rec: &Record, src: &Path, dst: &Path) -> Vec<String> {
    let args = &rec.arguments;
    let mut cmd: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let value = args[i].as_str();
        if value == rec.file {
            cmd.push(s(src));
        } else if matches!(value, "-o" | "-MF" | "-MT" | "-MQ" | "-MJ") {
            // the option's value is dropped with it; -o gets the new output
            if value == "-o" {
                cmd.extend(["-o".to_string(), s(dst)]);
            }
            i += 1;
        } else if matches!(value, "-MD" | "-MMD" | "-MP") {
            // no dependency file
        } else if value == "-Xclang" && i + 1 < args.len() && args[i + 1].starts_with("-W") {
            i += 1;
        } else if !value.starts_with("-W") || value.starts_with("-Wa,") {
            // the record's own warning options go; the assembler's options (-Wa,) stay
            cmd.push(value.to_string());
        }
        i += 1;
    }
    cmd.extend(["-Wall", "-Wextra", "-Werror", "-Wno-unused-parameter"].map(String::from));
    // Path(src).suffix != '.c'
    if src.extension().and_then(|e| e.to_str()) != Some("c") {
        cmd.extend(["-Wno-c99-designator", "-Wno-reorder-init-list", "-Wno-deprecated-enum-enum-conversion",
                    "-Wno-unnecessary-virtual-specifier", "-Wno-macro-redefined"].map(String::from));
    }
    for folder in ["bridge", "bluetooth", "camera"] {
        cmd.extend(["-I".to_string(), s(&here.join(folder))]);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_keeps_the_record_and_takes_the_boards_warnings() {
        let rec = parse_record(r#" {"directory": "/t/obj", "file": "/t/src/a.c", "output": "a.o", "arguments":
            ["/usr/bin/clang-23", "-xc", "/t/src/a.c", "-o", "a.o", "-MD", "-MF", "a.d", "-Wall", "-Wa,-x", "-Xclang",
             "-Wno-x", "-Xclang", "-keep", "-DX=1", "-MT", "a.o", "-MP"]},
        "#).unwrap();
        let c = command(Path::new("/r/drivers"), &rec, Path::new("/s/b.c"), Path::new("/o/b.o"));
        assert_eq!(c, ["/usr/bin/clang-23", "-xc", "/s/b.c", "-o", "/o/b.o", "-Wa,-x", "-Xclang", "-keep", "-DX=1", "-Wall",
                       "-Wextra", "-Werror", "-Wno-unused-parameter", "-I", "/r/drivers/bridge", "-I", "/r/drivers/bluetooth",
                       "-I", "/r/drivers/camera"]);
        let cpp = command(Path::new("/r/drivers"), &rec, Path::new("/s/b.cpp"), Path::new("/o/b.o"));
        assert!(cpp.contains(&"-Wno-macro-redefined".to_string()));
    }
}
