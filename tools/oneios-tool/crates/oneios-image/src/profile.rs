//! The build profile (`profiles/*.toml`, docs/image-format.md section 2): `load_profile` with imagetool.py's
//! checks and messages, and `image profile PROFILE [--shell | --members]`.
//!
//! The profile is kept as the value Python's `tomllib` gives (tables in the file's key order), with `_sha256` and
//! `_file` added at the end, so `image profile PROFILE` prints what `json.dumps(p, indent=2)` prints.

use crate::py;
use crate::{crash, err, Fail};
use oneios_common::pyjson::{self, Value};

pub const FORMAT: i128 = 1;
pub const PROFILE_SCHEMA: i128 = 1;

/// role -> (BuildManifest component, needs a name in [image.files], kind), in imagetool.py's order.
pub const ROLES: [(&str, Option<&str>, bool, &str); 13] = [
    ("os", Some("OS"), true, "volume"),
    ("systemos", Some("Cryptex1,SystemOS"), true, "volume"),
    ("app", Some("Cryptex1,AppOS"), true, "volume"),
    ("trust-static", Some("StaticTrustCache"), true, "file"),
    ("trust-system", Some("Cryptex1,SystemTrustCache"), true, "file"),
    ("trust-app", Some("Cryptex1,AppTrustCache"), true, "file"),
    ("roothash-os", Some("SystemVolume"), true, "file"),
    ("roothash-system", Some("Cryptex1,SystemVolume"), true, "file"),
    ("roothash-app", Some("Cryptex1,AppVolume"), true, "file"),
    ("kernelcache", Some("KernelCache"), false, "input"),
    ("devicetree", Some("DeviceTree"), false, "input"),
    ("buildmanifest", None, false, "input"),
    ("restore", None, false, "input"),
];

pub fn role(name: &str) -> Option<&'static (&'static str, Option<&'static str>, bool, &'static str)> {
    ROLES.iter().find(|r| r.0 == name)
}

pub struct Profile {
    pub p: Value,
    /// The file has a date or a time, which Python's json cannot write (`image profile PROFILE` then fails).
    datetime: bool,
}

fn convert(v: toml::Value, datetime: &mut bool) -> Value {
    match v {
        toml::Value::String(s) => Value::Str(s),
        toml::Value::Integer(i) => Value::Int(i as i128),
        toml::Value::Float(f) => Value::Float(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => {
            *datetime = true;
            Value::Str(d.to_string())
        }
        toml::Value::Array(a) => Value::List(a.into_iter().map(|v| convert(v, datetime)).collect()),
        toml::Value::Table(t) => Value::Dict(t.into_iter().map(|(k, v)| (k, convert(v, datetime))).collect()),
    }
}

/// A path segment the OneOS fork accepts: `^[A-Za-z0-9_][A-Za-z0-9._+-]*$`.
fn is_segment(s: &str) -> bool {
    let s = py::before_newline(s).as_bytes();
    match s.first() {
        Some(c) if c.is_ascii_alphanumeric() || *c == b'_' => s[1..].iter().all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(c)),
        _ => false,
    }
}

/// `check_set_path(path, what)`.
pub fn check_set_path(path: &str, what: &str) -> Result<(), Fail> {
    if path.split('/').any(|s| !is_segment(s)) {
        return Err(err!(
            "{what}: {} is not a set path (segments of letters, digits and ._+-, no leading dot)",
            py::repr_str(path)
        ));
    }
    Ok(())
}

/// What `uuid.UUID(text)` accepts: 32 hex digits, with hyphens, braces and a urn:uuid: prefix allowed.
fn is_uuid(text: &str) -> bool {
    let hex = text.replace("urn:", "").replace("uuid:", "");
    let hex = hex.trim_matches(|c| c == '{' || c == '}').replace('-', "");
    hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `load_profile(path)`.
pub fn load_profile(path: &str) -> Result<Profile, Fail> {
    let raw = std::fs::read(path).map_err(|e| err!("{path}: {}", py::os_error(&e, path)))?;
    let text = std::str::from_utf8(&raw).map_err(|e| err!("{path}: 'utf-8' codec can't decode the file: {e}"))?;
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| err!("{path}: {}", e.message()))?;
    let mut datetime = false;
    let mut p = convert(toml::Value::Table(table), &mut datetime);

    // need(table, key, kind): the value, when [table] has it as that kind.
    let need = |table: &str, key: &str, kind: &str| -> Result<&Value, Fail> {
        let missing = || err!("{path}: [{table}] {key} is missing or not a {kind}");
        let v = match p.get(table) {
            None => return Err(missing()),
            Some(Value::Dict(_)) => p.get(table).unwrap().get(key).ok_or_else(missing)?,
            Some(Value::List(_)) => return Err(missing()),
            Some(_) => return Err(Fail::Usage), // Python: `key in 5` is a TypeError
        };
        let ok = match kind {
            "str" => matches!(v, Value::Str(_)),
            _ => py::as_int(v).is_some(),
        };
        if ok { Ok(v) } else { Err(missing()) }
    };

    if !py::eq_int(p.get("schema"), PROFILE_SCHEMA) {
        let got = p.get("schema").map_or("None".into(), py::repr_value);
        return Err(err!("{path}: schema {got}, this tool reads schema {PROFILE_SCHEMA}"));
    }
    for key in ["name", "version", "build", "device", "board", "arch"] {
        need("os", key, "str")?;
    }
    for key in ["chip_id", "board_id"] {
        need("os", key, "int")?;
    }
    need("ipsw", "url", "str")?;
    need("ipsw", "size", "int")?;
    if !py::is_hex64(need("ipsw", "sha256", "str")?.as_str().unwrap()) {
        return Err(err!("{path}: [ipsw] sha256 is not 64 hex digits"));
    }
    let build = p.get("os").unwrap().get("build").unwrap().as_str().unwrap();
    let build = py::before_newline(build);
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(err!("{path}: [os] build has characters other than letters and digits"));
    }
    let members = match p.get("member") {
        Some(Value::List(members)) if !members.is_empty() => members,
        _ => return Err(err!("{path}: no [[member]]")),
    };
    let mut seen: Vec<&str> = Vec::new();
    for m in members {
        if !matches!(m, Value::Dict(_)) {
            return Err(crash!("{path}: a member that is not a table"));
        }
        let name = match m.get("role") {
            Some(Value::Str(name)) if role(name).is_some() => name.as_str(),
            Some(Value::List(_)) | Some(Value::Dict(_)) => return Err(Fail::Usage), // unhashable: a TypeError
            other => {
                let all: Vec<&str> = ROLES.iter().map(|r| r.0).collect();
                return Err(err!(
                    "{path}: member role {} is unknown ({})",
                    other.map_or("None".into(), py::repr_value),
                    all.join(", ")
                ));
            }
        };
        if seen.contains(&name) {
            return Err(err!("{path}: two members with role {name}"));
        }
        seen.push(name);
        for (key, kind) in [("path", "str"), ("size", "int"), ("crc32", "str")] {
            let ok = match (m.get(key), kind) {
                (Some(Value::Str(_)), "str") => true,
                (Some(v), "int") => py::as_int(v).is_some(),
                _ => false,
            };
            if !ok {
                return Err(err!("{path}: member {name}: {key} is missing or not a {kind}"));
            }
        }
        let crc = py::before_newline(m.get("crc32").unwrap().as_str().unwrap());
        if crc.len() != 8 || !py::is_lower_hex(crc) {
            return Err(err!("{path}: member {name}: crc32 is not 8 hex digits"));
        }
        if let Some(component) = role(name).unwrap().1 {
            if m.get("component").is_some_and(|c| c.as_str() != Some(component)) {
                return Err(err!("{path}: member {name}: component must be {component}"));
            }
        }
        let ends = m.get("path").unwrap().as_str().unwrap().ends_with(".aea");
        let aea = match m.get("aea") {
            None => Some(false),
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::Int(i)) if *i == 0 || *i == 1 => Some(*i == 1),
            Some(Value::Float(f)) if *f == 0.0 || *f == 1.0 => Some(*f == 1.0),
            Some(_) => None,
        };
        if aea != Some(ends) {
            return Err(err!("{path}: member {name}: aea must be true exactly when the path ends in .aea"));
        }
        for key in ["sha256", "sha384", "plain_sha256"] {
            match m.get(key) {
                None => {}
                Some(Value::Str(s)) => {
                    if !py::is_lower_hex(s) {
                        return Err(err!("{path}: member {name}: {key} is not hex"));
                    }
                }
                Some(_) => return Err(Fail::Usage), // re.match on a number: a TypeError
            }
        }
    }
    for name in ["os", "systemos", "app", "trust-static", "trust-system", "trust-app", "buildmanifest"] {
        if !seen.contains(&name) {
            return Err(err!("{path}: no member with role {name}"));
        }
    }
    let empty = Value::dict();
    let image = p.get("image").unwrap_or(&empty);
    if !matches!(image, Value::Dict(_)) {
        return Err(crash!("{path}: image is not a table"));
    }
    if !py::eq_int(image.get("format"), FORMAT) {
        let got = image.get("format").map_or("None".into(), py::repr_value);
        return Err(err!("{path}: [image] format {got}, this tool writes format {FORMAT}"));
    }
    for key in ["id", "archive", "stage"] {
        need("image", key, "str")?;
    }
    let files = image.get("files").unwrap_or(&empty);
    let mut names: Vec<&str> = Vec::new();
    // Python walks a set here, in no fixed order; this walks the members in the file's order.
    for name in &seen {
        if role(name).unwrap().2 {
            let Some(Value::Str(file)) = files.get(name) else {
                return Err(err!("{path}: [image.files] has no name for {name}"));
            };
            check_set_path(file, &format!("{path}: [image.files] {name}"))?;
            if names.contains(&file.as_str()) {
                return Err(err!("{path}: [image.files] {file} is used twice"));
            }
            names.push(file);
        }
    }
    for key in ["seed", "kernel"] {
        check_set_path(need("image", key, "str")?.as_str().unwrap(), &format!("{path}: [image] {key}"))?;
    }
    check_set_path(image.get("archive").unwrap().as_str().unwrap(), &format!("{path}: [image] archive"))?;
    let chunk_ok = match image.get("chunk") {
        None => true,
        Some(v) => py::as_int(v).is_some_and(|c| c >= 4096 && c & (c - 1) == 0),
    };
    if !chunk_ok {
        return Err(err!("{path}: [image] chunk must be a power of two of at least 4096"));
    }
    let data = p.get("data").unwrap_or(&empty);
    if !matches!(data, Value::Dict(_)) {
        return Err(crash!("{path}: data is not a table"));
    }
    if !is_uuid(data.get("volume_uuid").and_then(Value::as_str).unwrap_or("")) {
        return Err(err!("{path}: [data] volume_uuid is not a UUID"));
    }
    if data.get("epoch").and_then(py::as_int).is_none() {
        return Err(err!("{path}: [data] epoch (seconds, the time of files the build adds) is missing"));
    }
    p.set("_sha256", oneios_common::sha256_hex(&raw));
    p.set("_file", py::basename(path));
    Ok(Profile { p, datetime })
}

impl Profile {
    pub fn table(&self, name: &str) -> &Value {
        self.p.get(name).expect("load_profile checked the tables")
    }
    pub fn text(&self, table: &str, key: &str) -> &str {
        self.table(table).get(key).and_then(Value::as_str).expect("load_profile checked the strings")
    }
    pub fn members(&self) -> &[Value] {
        self.p.get("member").and_then(Value::as_list).expect("load_profile checked the members")
    }
    /// `member_of(profile, role)`.
    pub fn member_of(&self, role: &str) -> Option<&Value> {
        self.members().iter().find(|m| m.get("role").and_then(Value::as_str) == Some(role))
    }
    /// `image.get("chunk", DEFAULT_CHUNK)`.
    pub fn chunk(&self) -> Value {
        self.table("image").get("chunk").cloned().unwrap_or(Value::Int(crate::hashes::DEFAULT_CHUNK as i128))
    }
}

/// `input_name(m)`: the file name of a member in inputs/<build>/ (AEA members: the decrypted image, without .aea).
fn input_name(m: &Value) -> String {
    let name = py::basename(m.get("path").and_then(Value::as_str).unwrap_or(""));
    if py::truthy(m.get("aea")) {
        let cut = name.char_indices().rev().nth(3).map_or(0, |(i, _)| i);
        name[..cut].to_string()
    } else {
        name.to_string()
    }
}

/// `cmd_profile(args)`: the text it prints.
pub fn cmd_profile(args: &[String]) -> Result<String, Fail> {
    let profile = load_profile(args.first().ok_or(Fail::Usage)?)?;
    let p = &profile.p;
    let mut out = String::new();
    if args.iter().any(|a| a == "--shell") {
        let (o, i, d) = (profile.table("os"), profile.table("image"), profile.table("data"));
        let get = |t: &Value, key: &str| t.get(key).cloned().ok_or_else(|| crash!("the profile has no {key}"));
        let pairs = [
            ("P_NAME", get(o, "name")?),
            ("P_VERSION", get(o, "version")?),
            ("P_BUILD", get(o, "build")?),
            ("P_DEVICE", get(o, "device")?),
            ("P_BOARD", get(o, "board")?),
            ("P_IPSW_URL", get(profile.table("ipsw"), "url")?),
            ("P_SET_ID", get(i, "id")?),
            ("P_ARCHIVE", get(i, "archive")?),
            ("P_STAGE", get(i, "stage")?),
            ("P_SEED", get(i, "seed")?),
            ("P_KERNEL", get(i, "kernel")?),
            ("P_CHUNK", profile.chunk()),
            ("P_DATA_UUID", get(d, "volume_uuid")?),
            ("P_EPOCH", get(d, "epoch")?),
            ("P_DATA_SOURCE", d.get("source").cloned().unwrap_or(Value::str("private/var"))),
            ("P_NEED_GIB", profile.table("ipsw").get("need_gib").cloned().unwrap_or(Value::Int(30))),
        ];
        for (k, v) in pairs {
            out.push_str(&format!("{k}={}\n", py::shell_quote(&py::str_value(&v))));
        }
    } else if args.iter().any(|a| a == "--members") {
        let files = profile.table("image").get("files");
        for m in profile.members() {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("");
            let file = match files.and_then(|f| f.get(role)) {
                None => "-",
                Some(Value::Str(s)) => s,
                Some(_) => return Err(Fail::Usage), // "\t".join of a number: a TypeError
            };
            let fields = [
                role.to_string(),
                m.get("path").and_then(Value::as_str).unwrap_or("").to_string(),
                input_name(m),
                if py::truthy(m.get("aea")) { "1".into() } else { "0".into() },
                m.get("size").map_or(String::new(), py::str_value),
                file.to_string(),
            ];
            out.push_str(&fields.join("\t"));
            out.push('\n');
        }
    } else {
        if profile.datetime {
            return Err(Fail::Usage); // json.dumps of a date: a TypeError
        }
        out.push_str(&pyjson::dumps(p, Some(2), false));
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{profile_text, Tmp};

    #[test]
    fn set_paths() {
        for good in ["os.img", "trust/static.trustcache", "_a/b+c-d.e", "a\n"] {
            check_set_path(good, "x").unwrap();
        }
        for bad in ["", "../os.img", ".hidden", "a//b", "a/", "/a", "a b", "-a", "caf\u{e9}"] {
            match check_set_path(bad, "what") {
                Err(Fail::Error(m)) => assert_eq!(
                    m,
                    format!("what: {} is not a set path (segments of letters, digits and ._+-, no leading dot)", py::repr_str(bad))
                ),
                _ => panic!("{bad:?} passed"),
            }
        }
    }

    #[test]
    fn loads_and_prints() {
        let tmp = Tmp::new("profile");
        let path = tmp.write("test.toml", profile_text().as_bytes());
        let profile = load_profile(&path).unwrap();
        assert_eq!(profile.text("os", "build"), "0T1");
        assert_eq!(profile.p.get("_file").unwrap().as_str(), Some("test.toml"));
        assert_eq!(profile.p.get("_sha256").unwrap().as_str().unwrap(), oneios_common::sha256_hex(profile_text().as_bytes()));
        assert_eq!(input_name(profile.member_of("os").unwrap()), "001-00001-001.dmg");
        assert_eq!(input_name(profile.member_of("app").unwrap()), "001-00003-001.dmg");

        let shell = cmd_profile(&[path.clone(), "--shell".into()]).unwrap();
        assert_eq!(
            shell,
            "P_NAME=ios-0.0.1-0T1-iTest1,1\nP_VERSION=0.0.1\nP_BUILD=0T1\nP_DEVICE=iTest1,1\nP_BOARD=t1ap\n\
             P_IPSW_URL=https://example.invalid/iTest.ipsw\nP_SET_ID=ios-0.0.1-0T1-iTest1-1\nP_ARCHIVE=OneiOS_test.tar.zst\n\
             P_STAGE=/data/local/tmp/oneios/image\nP_SEED=data.seed\nP_KERNEL=kernel.kc\nP_CHUNK=4096\n\
             P_DATA_UUID=11111111-2222-3333-4444-555555555555\nP_EPOCH=1700000000\nP_DATA_SOURCE=private/var\nP_NEED_GIB=0\n"
        );
        let members = cmd_profile(&[path.clone(), "--members".into()]).unwrap();
        let lines: Vec<&str> = members.lines().collect();
        assert_eq!(lines.len(), 13);
        assert_eq!(lines[0], "os\t001-00001-001.dmg.aea\t001-00001-001.dmg\t1\t300044\tos.img");
        assert_eq!(lines[3], "trust-static\tFirmware/001-00001-001.dmg.aea.trustcache\t001-00001-001.dmg.aea.trustcache\t0\t100\ttrust/static.trustcache");
        assert_eq!(lines[12], "restore\tRestore.plist\tRestore.plist\t0\t100\t-");
        let json = cmd_profile(&[path]).unwrap();
        assert!(json.starts_with("{\n  \"schema\": 1,\n  \"os\": {\n    \"name\": \"ios-0.0.1-0T1-iTest1,1\","), "{json}");
        assert!(json.contains("\"chip_id\": 4660,"));
        assert!(json.ends_with(",\n  \"_file\": \"test.toml\"\n}\n"), "{json}");
    }

    #[test]
    fn rejects() {
        let tmp = Tmp::new("profile-rejects");
        let good = profile_text();
        // the profile without one member's block
        let without = |role: &str| {
            let start = good.find(&format!("[[member]]\nrole = \"{role}\"")).unwrap();
            let end = start + 1 + good[start + 1..].find("[[member]]").unwrap();
            format!("{}{}", &good[..start], &good[end..])
        };
        let cases = [
            ("schema", good.replace("schema = 1", "schema = 2"), "schema 2, this tool reads schema 1"),
            ("role", good.replace("role = \"restore\"", "role = \"bogus\""), "member role 'bogus' is unknown (os, systemos, app, trust-static, trust-system, trust-app, roothash-os, roothash-system, roothash-app, kernelcache, devicetree, buildmanifest, restore)"),
            ("crc", good.replace("crc32 = \"0000000b\"", "crc32 = \"000000b\""), "member buildmanifest: crc32 is not 8 hex digits"),
            ("aea", good.replace("aea = true\n[[member]]\nrole = \"systemos\"", "[[member]]\nrole = \"systemos\""), "member os: aea must be true exactly when the path ends in .aea"),
            ("format", good.replace("format = 1", "format = 9"), "[image] format 9, this tool writes format 1"),
            ("name", good.replace("os = \"os.img\"", "os = \"../os.img\""), "[image.files] os: '../os.img' is not a set path (segments of letters, digits and ._+-, no leading dot)"),
            ("twice", good.replace("app = \"app.img\"", "app = \"os.img\""), "[image.files] os.img is used twice"),
            ("uuid", good.replace("11111111-2222-3333-4444-555555555555", "not-a-uuid"), "[data] volume_uuid is not a UUID"),
            ("chunk", good.replace("chunk = 4096", "chunk = 1000"), "[image] chunk must be a power of two of at least 4096"),
            ("two", good.replace("role = \"restore\"", "role = \"os\""), "two members with role os"),
            ("missing", without("trust-app"), "no member with role trust-app"),
            ("epoch", good.replace("epoch = 1700000000", "epoch = \"soon\""), "[data] epoch (seconds, the time of files the build adds) is missing"),
            ("sha", good.replace("sha256 = \"00", "sha256 = \"zz"), "[ipsw] sha256 is not 64 hex digits"),
            ("os", good.replace("board = \"t1ap\"", "board = 5"), "[os] board is missing or not a str"),
            ("size", good.replace("size = 12345\nsha256", "size = \"big\"\nsha256"), "[ipsw] size is missing or not a int"),
            ("stage", good.replace("stage = \"/data/local/tmp/oneios/image\"\n", ""), "[image] stage is missing or not a str"),
            ("no-members", good.split("[[member]]").next().unwrap().to_string() + "[image]\nformat = 1\n", "no [[member]]"),
        ];
        for (name, text, want) in cases {
            assert_ne!(text, good, "{name}");
            let path = tmp.write(&format!("{name}.toml"), text.as_bytes());
            match load_profile(&path) {
                Err(Fail::Error(m)) => assert_eq!(m, format!("{path}: {want}"), "{name}"),
                _ => panic!("{name}: passed"),
            }
        }
        let missing = tmp.path("none.toml");
        match load_profile(&missing) {
            Err(Fail::Error(m)) => assert_eq!(m, format!("{missing}: [Errno 2] No such file or directory: '{missing}'")),
            _ => panic!("a missing profile passed"),
        }
    }
}
