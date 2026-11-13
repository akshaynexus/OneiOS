//! drivers/build.py's two functions the board archive generators used (they imported the script as a module):
//! `record`, a compile record of the first kernel pass, and `command`, the recorded compile command with another
//! source and output and the board's warnings.
//!
//! drivers/build.py itself stays (it builds drivers/ on its own, `--check` included, and
//! build_oneios_board runs it): this is a second copy of the rule. `oneios-tool kernel compile-command` prints
//! this copy's command, and tests/kernelgen/drivers-command.sh compares it with the script's.

use crate::gen::{read_text, resolve, s};
use anyhow::{anyhow, Result};
use oneios_common::pyjson::{self, Value};
use std::path::{Path, PathBuf};

/// `RECORDS[universe]`: the record whose command a source of that universe is compiled with.
pub fn record_name(universe: &str) -> &'static str {
    match universe {
        "mach" => "osfmk/DEVELOPMENT/startup.o.json",
        "bsd" => "bsd/DEVELOPMENT/kern_control.o.json",
        "iokit" => "iokit/DEVELOPMENT/IOPlatformExpert.cpo.json",
        other => panic!("no compile record for the universe {other}"),
    }
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
    let script = resolve(&repo.join("drivers/build.py"));
    script.parent().map(Path::to_path_buf).ok_or_else(|| anyhow!("{} has no directory", script.display()))
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
