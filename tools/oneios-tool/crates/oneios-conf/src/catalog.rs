//! The switch catalog as JSON (`oneios-tool switches --json`, the runtime package's `lib/switches.json`), and read
//! back: from oneios-switches (built in) or from a runtime's `lib/switches.json` (`--runtime DIR`).

use oneios_common::json::Value;
use oneios_switches::{Kind, Place, Rule, RuleKind, KEYS, KEY_PREFIXES, SWITCHES};
use std::path::Path;

/// The JSON's version: a reader refuses another.
pub const VERSION: i128 = 1;

/// A switch, as `conf` uses it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub rule: Rule,
    pub default: String,
    pub warning: Option<String>,
}

/// A key of xnuhost.conf that is not a switch.
#[derive(Clone, Debug)]
pub struct KeyEntry {
    pub name: String,
    pub rule: Rule,
    pub many: bool,
}

/// The catalog: its JSON (as written or as read) and what `conf` checks with.
#[derive(Clone, Debug)]
pub struct Catalog {
    pub json: Value,
    pub switches: Vec<Entry>,
    pub keys: Vec<KeyEntry>,
    pub prefixes: Vec<String>,
}

/// What a key of xnuhost.conf is.
pub enum Known<'a> {
    Switch(&'a Entry),
    Key(&'a KeyEntry),
    Prefix,
    None,
}

impl Catalog {
    /// This tool's own (oneios-switches).
    pub fn builtin() -> Catalog {
        Catalog::from_json(builtin_json()).expect("the built-in catalog reads back")
    }

    /// `DIR/lib/switches.json` of a runtime, else (no DIR) the built-in one.
    pub fn load(runtime: Option<&Path>) -> Result<Catalog, String> {
        let Some(dir) = runtime else {
            return Ok(Catalog::builtin());
        };
        let path = dir.join("lib/switches.json");
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "{}: {e} (a runtime older than the switch catalog has none: leave --runtime out)",
                path.display()
            )
        })?;
        let json =
            oneios_common::json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Catalog::from_json(json).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn from_json(json: Value) -> Result<Catalog, String> {
        if json.get("version").and_then(Value::as_int) != Some(VERSION) {
            return Err(format!("not a switch catalog of version {VERSION}"));
        }
        let list = |name: &str| -> Result<Vec<Value>, String> {
            Ok(json
                .get(name)
                .and_then(Value::as_list)
                .ok_or(format!("no {name} list"))?
                .to_vec())
        };
        let mut switches = Vec::new();
        for s in list("switches")? {
            let name = text(&s, "name")?;
            let place = Place::parse(&text(&s, "where")?).ok_or(format!("{name}: where"))?;
            switches.push(Entry {
                rule: rule_of(&s, place).map_err(|e| format!("{name}: {e}"))?,
                default: text(&s, "default")?,
                warning: s.get("warning").and_then(Value::as_str).map(str::to_string),
                name,
            });
        }
        let mut keys = Vec::new();
        for k in list("keys")? {
            let name = text(&k, "name")?;
            keys.push(KeyEntry {
                rule: rule_of(&k, Place::Conf).map_err(|e| format!("{name}: {e}"))?,
                many: k.get("many").and_then(Value::as_bool).unwrap_or(false),
                name,
            });
        }
        let prefixes = list("key_prefixes")?
            .iter()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect();
        Ok(Catalog {
            json,
            switches,
            keys,
            prefixes,
        })
    }

    pub fn switch(&self, name: &str) -> Option<&Entry> {
        self.switches.iter().find(|s| s.name == name)
    }

    /// The order of a boot-arg switch in the catalog (its words' order in boot-args-add=).
    pub fn order(&self, name: &str) -> Option<usize> {
        self.switches
            .iter()
            .position(|s| s.name == name && s.rule.place == Place::BootArg)
    }

    pub fn known(&self, key: &str) -> Known<'_> {
        if let Some(s) = self.switch(key) {
            return Known::Switch(s);
        }
        if let Some(k) = self.keys.iter().find(|k| k.name == key) {
            return Known::Key(k);
        }
        if self.prefixes.iter().any(|p| key.starts_with(p.as_str())) {
            return Known::Prefix;
        }
        Known::None
    }
}

fn text(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(format!("no {key}"))
}

fn words(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_list).map_or(Vec::new(), |l| {
        l.iter()
            .filter_map(|w| w.as_str().map(str::to_string))
            .collect()
    })
}

/// A rule from its JSON fields (the inverse of [`rule_json`]).
fn rule_of(v: &Value, place: Place) -> Result<Rule, String> {
    let kind = text(v, "kind")?;
    let items = v.get("items").filter(|i| !i.is_null());
    let kind = match kind.as_str() {
        "bool" => RuleKind::Bool,
        "choice" => RuleKind::Choice,
        "int" => RuleKind::Int,
        "size" => RuleKind::Size,
        "text" if items.is_some() => RuleKind::List,
        "text" => RuleKind::Text,
        other => return Err(format!("kind {other}")),
    };
    let num = |key: &str| v.get(key).and_then(Value::as_int).unwrap_or(0);
    Ok(Rule {
        kind,
        place,
        values: words(
            v,
            if kind == RuleKind::List {
                "items"
            } else {
                "values"
            },
        ),
        min: num("min"),
        max: num("max"),
        also: words(v, "also"),
        word: v.get("word").is_some_and(|w| !w.is_null()),
        unique: v.get("unique").and_then(Value::as_bool).unwrap_or(false),
        trim: v.get("trim").and_then(Value::as_bool).unwrap_or(false),
        max_len: v.get("max_len").and_then(Value::as_u64).unwrap_or(0) as usize,
    })
}

fn opt(s: Option<&str>) -> Value {
    s.map_or(Value::Null, Value::str)
}

fn strs(list: &[&str]) -> Value {
    Value::List(list.iter().map(|s| Value::str(*s)).collect())
}

/// A rule's fields: kind, values, min, max, then also, items, unique, trim, max_len (null where they do not apply).
fn rule_json(kind: Kind) -> Vec<(&'static str, Value)> {
    let (name, values, range, items, max_len) = match kind {
        Kind::Bool => ("bool", None, None, None, None),
        Kind::Choice(v) => ("choice", Some(v), None, None, None),
        Kind::Int { min, max } => (
            "int",
            None,
            Some((i128::from(min), i128::from(max))),
            None,
            None,
        ),
        Kind::Size { min, max } => (
            "size",
            None,
            Some((i128::from(min), i128::from(max))),
            None,
            None,
        ),
        Kind::List {
            items,
            unique,
            trim,
        } => ("text", None, None, Some((items, unique, trim)), None),
        Kind::Text { max_len } => ("text", None, None, None, Some(max_len)),
    };
    vec![
        ("kind", Value::str(name)),
        ("values", values.map_or(Value::Null, strs)),
        ("min", range.map_or(Value::Null, |r| Value::Int(r.0))),
        ("max", range.map_or(Value::Null, |r| Value::Int(r.1))),
        ("items", items.map_or(Value::Null, |i| strs(i.0))),
        ("unique", items.map_or(Value::Null, |i| Value::Bool(i.1))),
        ("trim", items.map_or(Value::Null, |i| Value::Bool(i.2))),
        (
            "max_len",
            max_len.map_or(Value::Null, |n| Value::Int(n as i128)),
        ),
    ]
}

/// The built-in catalog's JSON: `{"version": 1, "switches": [...], "keys": [...], "key_prefixes": [...]}`
/// (tools/oneios-tool/README.md, "switches", has every field).
pub fn builtin_json() -> Value {
    let mut out = Value::dict();
    out.set("version", Value::Int(VERSION));
    let mut list = Value::list();
    for s in SWITCHES {
        let rule = rule_json(s.kind);
        let field = |name: &str| {
            rule.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let mut e = Value::dict();
        e.set("name", s.name);
        e.set("where", s.place.as_str());
        for k in ["kind", "values", "min", "max"] {
            e.set(k, field(k));
        }
        e.set("default", s.default);
        e.set("off", opt(s.off));
        e.set("group", s.group);
        e.set("title", s.title);
        e.set("help", s.help);
        e.set("phone", s.phone.as_str());
        e.set("warning", opt(s.warning));
        // every switch is read once, at a session's start: a new session takes it (none needs Update the kernel)
        e.set("restart", "session");
        e.set("also", strs(s.also));
        for k in ["items", "unique", "trim", "max_len"] {
            e.set(k, field(k));
        }
        e.set("word", opt(s.word));
        e.set("part", s.part);
        e.set("auto", opt(s.auto));
        e.set("line", s.line);
        list.push(e);
    }
    out.set("switches", list);
    let mut keys = Value::list();
    for k in KEYS {
        let mut e = Value::dict();
        e.set("name", k.name);
        for (name, v) in rule_json(k.kind) {
            e.set(name, v);
        }
        e.set("also", strs(k.also));
        e.set("many", k.many);
        keys.push(e);
    }
    out.set("keys", keys);
    out.set("key_prefixes", strs(KEY_PREFIXES));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON reads back to the same rules (so a runtime's lib/switches.json checks as the built-in catalog does).
    #[test]
    fn json_round_trip() {
        let c = Catalog::builtin();
        assert_eq!(c.switches.len(), SWITCHES.len());
        for (e, s) in c.switches.iter().zip(SWITCHES) {
            assert_eq!(e.name, s.name);
            assert_eq!(e.rule, s.rule(), "{}", s.name);
            assert_eq!(e.default, s.default);
        }
        for (e, k) in c.keys.iter().zip(KEYS) {
            assert_eq!(
                (e.name.as_str(), &e.rule, e.many),
                (k.name, &k.rule(), k.many)
            );
        }
        let text = oneios_common::json::dumps(&c.json, None, false);
        let again = Catalog::from_json(oneios_common::json::parse(&text).unwrap()).unwrap();
        assert_eq!(oneios_common::json::dumps(&again.json, None, false), text);
        assert!(
            Catalog::from_json(oneios_common::json::parse(r#"{"version": 2}"#).unwrap()).is_err()
        );
    }
}
