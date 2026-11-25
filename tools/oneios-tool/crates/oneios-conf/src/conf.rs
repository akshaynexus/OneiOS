//! An xnuhost.conf read as xnuhost reads it (runtime/xnuhost/src/config.rs `Config::file`, `add_boot_args`), and
//! changed one switch at a time with every other line kept.
//!
//! xnuhost's rules, which the checks follow: a line is trimmed of C blanks; an empty line or one that starts with `#`
//! is skipped; any other line must be `KEY=VALUE` with a key xnuhost reads and a value it takes, else the whole file
//! is refused and the session does not start (`xnuhost: FILE:N: bad setting KEY`). A key given twice: the last line
//! wins (text-fixup and the disks add one each). `boot-args-add=`'s words are added to the boot arguments, each
//! unless `boot-args=` names it already; of a word given twice in it the first is used.

use crate::catalog::{Catalog, Known};
use oneios_common::json::Value;
use oneios_switches::{check, Place, C_SPACE};

/// xnuhost's limit of boot-args-add= (config.rs `text(value, 512)`).
pub const BOOT_ARGS_ADD_MAX: usize = 511;

/// One finding, at a line (1-based; 0: the file as a whole).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub line: usize,
    pub message: String,
}

/// The file, read.
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    /// the keys xnuhost reads (but boot-args-add) and their values, in the order first given (the last value)
    pub set: Vec<(String, String)>,
    /// the words of the boot-args-add= line xnuhost uses (its last): name and value ("" for a word without '=')
    pub boot_args_add: Vec<(String, String)>,
    /// keys xnuhost does not read, and lines that are not KEY=VALUE (each is an error too)
    pub unknown: Vec<String>,
    pub errors: Vec<Note>,
    pub warnings: Vec<Note>,
}

/// A content line's key and value as xnuhost sees them; None for a blank line or a comment, Some((line, None)) for a
/// line without '='.
fn key_value(raw: &str) -> Option<(&str, Option<(&str, &str)>)> {
    let line = raw.trim_matches(C_SPACE);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    Some((line, line.split_once('=')))
}

/// The lines of a text as xnuhost numbers them (split at '\n'; a last '\n' ends the last line).
fn lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') || text.is_empty() {
        lines.pop();
    }
    lines
}

fn word_name(word: &str) -> &str {
    word.split('=').next().unwrap_or(word)
}

/// Whether the boot arguments name `name` (config.rs `boot_arg_named`).
fn names(args: &str, name: &str) -> bool {
    args.split_whitespace().any(|w| {
        w.strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
    })
}

/// Read a file's text against the catalog.
pub fn parse(cat: &Catalog, text: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut add: Option<(usize, String)> = None;
    let mut boot_args = String::new();
    for (index, raw) in lines(text).iter().enumerate() {
        let n = index + 1;
        let Some((line, kv)) = key_value(raw) else {
            continue;
        };
        let Some((key, value)) = kv else {
            p.errors.push(Note {
                line: n,
                message: format!("{line:?} is not KEY=VALUE: xnuhost refuses the file"),
            });
            p.unknown.push(line.to_string());
            continue;
        };
        let (rule, many) = match cat.known(key) {
            Known::Switch(s) if s.rule.place == Place::BootArg => {
                p.errors.push(Note {
                    line: n,
                    message: format!(
                        "{key} is a kernel switch, a word of boot-args-add= (`conf set` puts it there); as a key xnuhost refuses the file"
                    ),
                });
                p.unknown.push(key.to_string());
                continue;
            }
            Known::Switch(s) => (Some(&s.rule), false),
            Known::Key(k) => (Some(&k.rule), k.many),
            Known::Prefix => (None, false),
            Known::None => {
                p.errors.push(Note {
                    line: n,
                    message: format!(
                        "{key} is not a key of xnuhost.conf: xnuhost refuses the file"
                    ),
                });
                p.unknown.push(key.to_string());
                continue;
            }
        };
        if let Some(rule) = rule {
            if let Err(takes) = check(rule, value) {
                p.errors.push(Note {
                    line: n,
                    message: format!(
                        "{key}={value}: {key} takes {takes}; xnuhost refuses the file"
                    ),
                });
            }
        }
        if let Some(s) = cat
            .switch(key)
            .filter(|s| s.warning.is_some() && value != s.default)
        {
            p.warnings.push(Note {
                line: n,
                message: format!("{key}={value}: {}", s.warning.as_deref().unwrap_or("")),
            });
        }
        if let Some((_, first)) = seen.iter().find(|(k, _)| k == key).filter(|_| !many) {
            p.warnings.push(Note {
                line: n,
                message: format!(
                    "{key} is given again (first on line {first}): this line's value is used"
                ),
            });
        } else {
            seen.push((key.to_string(), n));
        }
        match key {
            "boot-args-add" => add = Some((n, value.to_string())),
            "boot-args" => boot_args = value.to_string(),
            _ => {}
        }
        if key != "boot-args-add" {
            match p.set.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value.to_string(),
                None => p.set.push((key.to_string(), value.to_string())),
            }
        }
    }
    if let Some((n, value)) = add {
        for word in value.split_whitespace() {
            let name = word_name(word);
            let v = word.split_once('=').map_or("", |(_, v)| v);
            if p.boot_args_add.iter().any(|(k, _)| k == name) {
                p.warnings.push(Note {
                    line: n,
                    message: format!(
                        "boot-args-add: {name} is given again: its first word is used"
                    ),
                });
                continue;
            }
            p.boot_args_add.push((name.to_string(), v.to_string()));
            match cat.switch(name) {
                Some(s) if s.rule.place == Place::BootArg => {
                    if let Err(takes) = check(&s.rule, v) {
                        p.errors.push(Note { line: n, message: format!("boot-args-add: {word}: {name} takes {takes}") });
                    } else if s.warning.is_some() && v != s.default {
                        p.warnings.push(Note { line: n, message: format!("boot-args-add: {word}: {}", s.warning.as_deref().unwrap_or("")) });
                    }
                }
                Some(_) => p.warnings.push(Note {
                    line: n,
                    message: format!("boot-args-add: {name} is a key of xnuhost.conf, not a boot argument: give it its own line"),
                }),
                None => {}
            }
            if names(&boot_args, name) {
                p.warnings.push(Note {
                    line: n,
                    message: format!("boot-args names {name}: boot-args-add's {word} is not used"),
                });
            }
        }
    }
    p
}

impl Parsed {
    pub fn notes_json(notes: &[Note]) -> Value {
        Value::List(
            notes
                .iter()
                .map(|e| {
                    let mut v = Value::dict();
                    v.set("line", e.line);
                    v.set("message", e.message.as_str());
                    v
                })
                .collect(),
        )
    }

    pub fn pairs_json(pairs: &[(String, String)]) -> Value {
        let mut v = Value::dict();
        for (k, val) in pairs {
            v.set(k.as_str(), val.as_str());
        }
        v
    }

    pub fn unknown_json(&self) -> Value {
        Value::List(
            self.unknown
                .iter()
                .map(|u| Value::str(u.as_str()))
                .collect(),
        )
    }
}

/// One change: a switch (or another key) to a value, or back to its default (None: its line or word removed).
pub type Change = (String, Option<String>);

/// The text with the changes made; every other line as it was. A conf switch is its own `key=value` line (the last
/// of its lines kept in its place, the others removed; else added at the end); a boot-arg switch is a word of the
/// last boot-args-add= line (one line; the other words first in their order, then the switches' words in the
/// catalog's order; the line added at the end when there is none, removed when no word is left). Err for a name that
/// is not a switch or a key, a value the catalog refuses, or a boot-args-add= longer than xnuhost takes.
pub fn edit(cat: &Catalog, text: &str, changes: &[Change]) -> Result<String, String> {
    let mut out: Vec<String> = lines(text).iter().map(|l| l.to_string()).collect();
    for (name, value) in changes {
        let place = match cat.known(name) {
            Known::Switch(s) => {
                if let Some(v) = value {
                    check(&s.rule, v)
                        .map_err(|takes| format!("{name}={v}: {name} takes {takes}"))?;
                }
                s.rule.place
            }
            Known::Key(k) if !k.many && name != "boot-args-add" => {
                if let Some(v) = value {
                    check(&k.rule, v)
                        .map_err(|takes| format!("{name}={v}: {name} takes {takes}"))?;
                }
                Place::Conf
            }
            Known::Key(_) => {
                return Err(format!(
                    "{name}: not changed by `conf set` (edit the whole file: `conf write`)"
                ))
            }
            Known::Prefix | Known::None => {
                return Err(format!(
                    "{name}: not a switch of the catalog (`oneios-tool switches`)"
                ))
            }
        };
        if let Some(v) = value.as_deref().filter(|v| v.contains(['\n', '\r'])) {
            return Err(format!("{name}: a value of one line ({v:?})"));
        }
        match place {
            Place::Conf => set_line(&mut out, name, value.as_deref()),
            Place::BootArg => set_word(cat, &mut out, name, value.as_deref())?,
        }
    }
    let mut text = out.join("\n");
    if !out.is_empty() {
        text.push('\n');
    }
    Ok(text)
}

/// The key of a raw line, when it is a KEY=VALUE line.
fn line_key(raw: &str) -> Option<&str> {
    match key_value(raw) {
        Some((_, Some((key, _)))) => Some(key),
        _ => None,
    }
}

/// A raw line's '\r' (a file written on Windows) is kept on the line that replaces it.
fn like(raw: &str, line: String) -> String {
    if raw.ends_with('\r') {
        line + "\r"
    } else {
        line
    }
}

fn set_line(out: &mut Vec<String>, name: &str, value: Option<&str>) {
    let at: Vec<usize> = (0..out.len())
        .filter(|&i| line_key(&out[i]) == Some(name))
        .collect();
    let keep = match (value, at.last()) {
        (Some(v), Some(&last)) => {
            out[last] = like(&out[last], format!("{name}={v}"));
            Some(last)
        }
        (Some(v), None) => {
            out.push(format!("{name}={v}"));
            None
        }
        (None, _) => None,
    };
    for &i in at.iter().rev().filter(|&&i| Some(i) != keep) {
        out.remove(i);
    }
}

fn set_word(
    cat: &Catalog,
    out: &mut Vec<String>,
    name: &str,
    value: Option<&str>,
) -> Result<(), String> {
    let at = (0..out.len())
        .rev()
        .find(|&i| line_key(&out[i]) == Some("boot-args-add"));
    let old = at.map_or(String::new(), |i| {
        out[i]
            .trim_matches(C_SPACE)
            .split_once('=')
            .map_or("", |(_, v)| v)
            .to_string()
    });
    let mut words: Vec<String> = old
        .split_whitespace()
        .filter(|w| word_name(w) != name)
        .map(str::to_string)
        .collect();
    if let Some(v) = value {
        words.push(format!("{name}={v}"));
    }
    // the other words in their order, then the switches' in the catalog's (a stable line, whatever was set first)
    let mut switches: Vec<String> = Vec::new();
    words.retain(|w| {
        let is_switch = cat.order(word_name(w)).is_some();
        if is_switch && !switches.iter().any(|s| word_name(s) == word_name(w)) {
            switches.push(w.clone());
        }
        !is_switch
    });
    switches.sort_by_key(|w| cat.order(word_name(w)));
    words.extend(switches);
    let line = words.join(" ");
    if line.len() > BOOT_ARGS_ADD_MAX {
        return Err(format!(
            "boot-args-add= would be {} bytes; xnuhost takes {BOOT_ARGS_ADD_MAX} at most",
            line.len()
        ));
    }
    match (at, line.is_empty()) {
        (Some(i), true) => {
            out.remove(i);
        }
        (Some(i), false) => out[i] = like(&out[i], format!("boot-args-add={line}")),
        (None, true) => {}
        (None, false) => out.push(format!("boot-args-add={line}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalog {
        Catalog::builtin()
    }

    const SAMPLE: &str = "# the S24 Ultra\npen=auto\n\n  encoder=auto  \nboot-args-add=-v oneios-pool-cut=0 serial=3\n# gpu-down=bottom\ngpu-down=governor\n";

    #[test]
    fn reads_as_xnuhost_does() {
        let p = parse(&cat(), SAMPLE);
        assert!(
            p.errors.is_empty() && p.unknown.is_empty() && p.warnings.is_empty(),
            "{p:?}"
        );
        assert_eq!(
            p.set,
            [
                ("pen", "auto"),
                ("encoder", "auto"),
                ("gpu-down", "governor")
            ]
            .map(|(a, b)| (a.into(), b.into()))
        );
        assert_eq!(
            p.boot_args_add,
            [("-v", ""), ("oneios-pool-cut", "0"), ("serial", "3")]
                .map(|(a, b)| (a.into(), b.into()))
        );
        let bad = "gpu-down=top\nnot a setting\npenn=auto\noneios-pool-cut=0\nboot-args-add=oneios-pool-cut=2 oneios-motion=7\nwifi=off\nwifi=auto\n";
        let p = parse(&cat(), bad);
        let lines: Vec<usize> = p.errors.iter().map(|e| e.line).collect();
        assert_eq!(lines, [1, 2, 3, 4, 5], "{:?}", p.errors);
        assert!(
            p.errors[0].message.contains("governor, bottom"),
            "{}",
            p.errors[0].message
        );
        assert_eq!(p.unknown, ["not a setting", "penn", "oneios-pool-cut"]);
        assert!(
            p.warnings
                .iter()
                .any(|w| w.line == 5 && w.message.contains("AppleSPU")),
            "{:?}",
            p.warnings
        );
        assert!(
            p.warnings
                .iter()
                .any(|w| w.line == 7 && w.message.contains("given again")),
            "{:?}",
            p.warnings
        );
        assert_eq!(p.set.iter().find(|(k, _)| k == "wifi").unwrap().1, "auto");
        // boot-args names the word: boot-args-add's is not used
        let p = parse(
            &cat(),
            "boot-args=-v oneios-audio-rt=1\nboot-args-add=oneios-audio-rt=0\n",
        );
        assert!(
            p.warnings.iter().any(|w| w.message.contains("not used")),
            "{:?}",
            p.warnings
        );
        // a missing file reads as an empty one
        let p = parse(&cat(), "");
        assert!(p.set.is_empty() && p.errors.is_empty());
    }

    #[test]
    fn set_keeps_every_other_line() {
        let c = cat();
        let ch = |n: &str, v: Option<&str>| (n.to_string(), v.map(str::to_string));
        let t = edit(
            &c,
            SAMPLE,
            &[
                ch("gpu-down", Some("bottom")),
                ch("display-present", Some("window")),
            ],
        )
        .unwrap();
        assert_eq!(
            t,
            "# the S24 Ultra\npen=auto\n\n  encoder=auto  \nboot-args-add=-v oneios-pool-cut=0 serial=3\n# gpu-down=bottom\ngpu-down=bottom\ndisplay-present=window\n"
        );
        // boot-arg switches: one line, the others' words first, the switches in the catalog's order
        let t = edit(
            &c,
            SAMPLE,
            &[
                ch("oneios-m2v-shader-fix", Some("0")),
                ch("oneios-wifi-rssi", Some("0")),
                ch("oneios-pool-cut", Some("1")),
            ],
        )
        .unwrap();
        assert!(t.contains("\nboot-args-add=-v serial=3 oneios-wifi-rssi=0 oneios-pool-cut=1 oneios-m2v-shader-fix=0\n"), "{t}");
        assert_eq!(parse(&c, &t).boot_args_add.len(), 5);
        // the same switches set in another order: the same line
        let u = edit(
            &c,
            SAMPLE,
            &[
                ch("oneios-pool-cut", Some("1")),
                ch("oneios-wifi-rssi", Some("0")),
                ch("oneios-m2v-shader-fix", Some("0")),
            ],
        )
        .unwrap();
        assert_eq!(t, u);
        // unset: back to the default (the word or the line gone); unset of the last word removes the line
        let t = edit(
            &c,
            &t,
            &[
                ch("oneios-pool-cut", None),
                ch("oneios-wifi-rssi", None),
                ch("gpu-down", None),
            ],
        )
        .unwrap();
        assert!(
            t.contains("\nboot-args-add=-v serial=3 oneios-m2v-shader-fix=0\n")
                && !t.contains("\ngpu-down="),
            "{t}"
        );
        assert!(t.contains("# gpu-down=bottom\n"), "comments kept: {t}");
        let t = edit(
            &c,
            "boot-args-add=oneios-codec-irq=0\n",
            &[ch("oneios-codec-irq", None)],
        )
        .unwrap();
        assert_eq!(t, "");
        // into an empty file, a file without a last newline, duplicates of a key, Windows lines
        assert_eq!(edit(&c, "", &[ch("astc", Some("0"))]).unwrap(), "astc=0\n");
        assert_eq!(
            edit(&c, "pen=auto", &[ch("astc", Some("0"))]).unwrap(),
            "pen=auto\nastc=0\n"
        );
        assert_eq!(
            edit(&c, "astc=1\npen=auto\nastc=0\n", &[ch("astc", Some("1"))]).unwrap(),
            "pen=auto\nastc=1\n"
        );
        assert_eq!(
            edit(&c, "astc=0\r\npen=auto\r\n", &[ch("astc", Some("1"))]).unwrap(),
            "astc=1\r\npen=auto\r\n"
        );
        // a key that is not a switch may be set too (pen=), boot-args-add itself and the repeatable ones not
        assert_eq!(
            edit(&c, "", &[ch("pen", Some("off"))]).unwrap(),
            "pen=off\n"
        );
        assert!(edit(&c, "", &[ch("boot-args-add", Some("-v"))]).is_err());
        assert!(edit(&c, "", &[ch("text-fixup", Some("a,0,00,00"))]).is_err());
    }

    #[test]
    fn set_refuses() {
        let c = cat();
        let ch = |n: &str, v: &str| (n.to_string(), Some(v.to_string()));
        for (n, v) in [
            ("gpu-down", "top"),
            ("oneios-pool-cut", "yes"),
            ("sensors-hz", "1"),
            ("no-such", "1"),
            ("oneios-motion", "16"),
            ("astc", "1\nx=1"),
        ] {
            assert!(edit(&c, SAMPLE, &[ch(n, v)]).is_err(), "{n}={v}");
        }
        assert!(
            edit(
                &c,
                SAMPLE,
                &[ch("gpu-down", "bottom"), ch("gpu-down", "top")]
            )
            .is_err(),
            "all or nothing"
        );
        // boot-args-add= stays within xnuhost's 511 bytes
        let long = format!("boot-args-add=x={}\n", "a".repeat(495));
        assert!(edit(&c, &long, &[ch("oneios-pool-cut", "0")]).is_err());
        assert!(edit(&c, &long, &[ch("astc", "0")]).is_ok());
    }
}
