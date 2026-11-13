//! imagetool.py's IPSW commands: `check-ipsw` (the profile against the IPSW's directory and BuildManifest),
//! `fetch` (the profile's members into inputs/<build>/, with their hashes in inputs.json) and `record-plain`
//! (after an AEA was decrypted: the plain image's size and SHA-256 in inputs.json). The IPSW is read member by
//! member (remotezip.rs); the BuildManifest is a property list (the `plist` crate).

use std::collections::HashMap;

use oneios_common::pyjson::{self, Value};
use sha2::{Digest, Sha256, Sha384};

use crate::profile::{self, Profile};
use crate::remotezip::{Info, RemoteZip};
use crate::{crash, err, py, Fail};

/// remotezip's `Error` is not imagetool's: there it ended the tool with a traceback (`Crash`: the message, status 1).
fn zip<T>(result: crate::remotezip::Result<T>) -> Result<T, Fail> {
    result.map_err(|e| crash!("{e}"))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A value of the BuildManifest as Python prints it (`None` when it is not there).
fn shown(value: Option<&plist::Value>) -> String {
    match value {
        Some(plist::Value::String(s)) => s.clone(),
        Some(other) => format!("{other:?}"),
        None => "None".to_string(),
    }
}

fn dict<'a>(value: &'a plist::Value, key: &str) -> Option<&'a plist::Value> {
    value.as_dictionary().and_then(|d| d.get(key))
}

/// `item[key]`, where the script ended with a KeyError.
fn need<'a>(value: &'a plist::Value, key: &str) -> Result<&'a plist::Value, Fail> {
    dict(value, key).ok_or_else(|| crash!("the BuildManifest has a build identity without {key}"))
}

fn hex_number(value: &plist::Value) -> Option<i128> {
    value.as_string().and_then(|s| i128::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok())
}

/// The Erase build identity of the profile's board in the BuildManifest.
fn build_identity<'a>(manifest: &'a plist::Value, profile: &Profile) -> Result<&'a plist::Value, Fail> {
    let o = profile.table("os");
    let (version, build, device, board) = (text(o, "version"), text(o, "build"), text(o, "device"), text(o, "board"));
    let types: Vec<&str> = dict(manifest, "SupportedProductTypes").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(|v| v.as_string()).collect()).unwrap_or_default();
    let (has_version, has_build) = (dict(manifest, "ProductVersion"), dict(manifest, "ProductBuildVersion"));
    if has_version.and_then(|v| v.as_string()) != Some(version) || has_build.and_then(|v| v.as_string()) != Some(build) || !types.contains(&device) {
        return Err(err!(
            "this IPSW is {} ({}) for {}; the profile wants {version} ({build}) for {device}",
            shown(has_version),
            shown(has_build),
            types.join(", ")
        ));
    }
    let variant = profile.table("ipsw").get("variant").filter(|v| !v.is_null());
    let identities = need(manifest, "BuildIdentities")?.as_array().ok_or_else(|| crash!("the BuildManifest's BuildIdentities are no list"))?;
    for item in identities {
        let info = need(item, "Info")?;
        let class = need(info, "DeviceClass")?.as_string().unwrap_or("");
        let erase = need(info, "RestoreBehavior")?.as_string() == Some("Erase");
        let chip = hex_number(need(item, "ApChipID")?) == o.get("chip_id").and_then(py::as_int);
        let board_id = hex_number(need(item, "ApBoardID")?) == o.get("board_id").and_then(py::as_int);
        let same_variant = variant.is_none_or(|want| dict(info, "Variant").and_then(|v| v.as_string()) == want.as_str());
        if class.to_lowercase() == board.to_lowercase() && erase && chip && board_id && same_variant {
            return Ok(item);
        }
    }
    Err(err!("no Erase build identity for {board} in this IPSW"))
}

/// The component's entry of the identity's manifest.
fn component<'a>(identity: &'a plist::Value, name: &str) -> Option<&'a plist::Value> {
    dict(identity, "Manifest").and_then(|m| dict(m, name))
}

/// Compare the profile's members with the ZIP directory (name -> entry) and the BuildManifest. Returns problems.
fn check_members(profile: &Profile, infos: &HashMap<&str, &Info>, identity: Option<&plist::Value>) -> Vec<String> {
    let mut bad = Vec::new();
    for m in profile.members() {
        let (role, path) = (text(m, "role"), text(m, "path"));
        let Some(info) = infos.get(path) else {
            bad.push(format!("{role}: {path} is not in the IPSW"));
            continue;
        };
        if Some(info.file_size as i128) != m.get("size").and_then(py::as_int) {
            bad.push(format!("{role}: {path} is {} bytes, the profile says {}", info.file_size, m.get("size").map_or(String::new(), py::str_value)));
        }
        if format!("{:08x}", info.crc) != text(m, "crc32") {
            bad.push(format!("{role}: {path} has CRC-32 {:08x}, the profile says {}", info.crc, text(m, "crc32")));
        }
        if let (Some(identity), Some(name)) = (identity, profile::role(role).and_then(|r| r.1)) {
            let listed = component(identity, name).and_then(|c| dict(c, "Info")).and_then(|i| dict(i, "Path"));
            if listed.and_then(|p| p.as_string()) != Some(path) {
                bad.push(format!("{role}: BuildManifest {name} is {}, the profile says {path}", shown(listed)));
            }
        }
    }
    bad
}

/// The archive, its directory by name and its BuildManifest.
struct Ipsw {
    zip: RemoteZip,
    manifest: plist::Value,
}

impl Ipsw {
    fn open(spec: &str) -> Result<Ipsw, Fail> {
        let zip = zip(RemoteZip::open(spec))?;
        Ok(Ipsw { zip, manifest: plist::Value::String(String::new()) })
    }

    fn read_manifest(&mut self, profile: &Profile) -> Result<(), Fail> {
        let member = profile.member_of("buildmanifest").ok_or_else(|| crash!("the profile has no buildmanifest member"))?;
        let data = zip(self.zip.read(text(member, "path")))?;
        self.manifest = plist::Value::from_reader(std::io::Cursor::new(data)).map_err(|e| crash!("the BuildManifest is no property list: {e}"))?;
        Ok(())
    }

    fn infos(&self) -> HashMap<&str, &Info> {
        self.zip.infos().iter().map(|i| (i.filename.as_str(), i)).collect()
    }
}

/// `--source SPEC`, else the profile's URL.
fn source<'a>(profile: &'a Profile, given: Option<&'a str>) -> &'a str {
    given.unwrap_or_else(|| profile.text("ipsw", "url"))
}

/// `check-ipsw PROFILE [--source URL|FILE]`.
pub fn cmd_check_ipsw(profile_path: &str, given: Option<&str>) -> Result<(), Fail> {
    let profile = profile::load_profile(profile_path)?;
    let spec = source(&profile, given);
    let mut ipsw = Ipsw::open(spec)?;
    let size = ipsw.zip.source.size;
    let pinned = profile.table("ipsw").get("size");
    if Some(size as i128) != pinned.and_then(py::as_int) {
        return Err(err!("the IPSW is {size} bytes, the profile says {}", pinned.map_or(String::new(), py::str_value)));
    }
    ipsw.read_manifest(&profile)?;
    let infos = ipsw.infos();
    let bad = check_members(&profile, &infos, Some(build_identity(&ipsw.manifest, &profile)?));
    let mut total = 0u64;
    for m in profile.members() {
        if let Some(info) = infos.get(text(m, "path")) {
            total += info.compress_size;
            println!("{:>13} {:>13} {:08x} {:<16} {}", info.file_size, info.compress_size, info.crc, text(m, "role"), text(m, "path"));
        }
    }
    println!("{} members, {total} bytes to download ({:.2} GiB) of {size}", profile.members().len(), total as f64 / (1u64 << 30) as f64);
    if !bad.is_empty() {
        return Err(err!("the profile does not match the IPSW:\n  {}", bad.join("\n  ")));
    }
    println!("ok: {} matches {spec}", profile.text("os", "name"));
    Ok(())
}

/// The SHA-256 and the SHA-384 of a file, in one pass.
fn digest_file(path: &str) -> Result<(String, String), Fail> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| crash!("{}", py::os_error(&e, path)))?;
    let (mut a, mut b, mut block) = (Sha256::new(), Sha384::new(), vec![0u8; 1 << 22]);
    loop {
        let n = file.read(&mut block).map_err(|e| crash!("{}", py::os_error(&e, path)))?;
        if n == 0 {
            return Ok((oneios_common::hex(&a.finalize()), oneios_common::hex(&b.finalize())));
        }
        a.update(&block[..n]);
        b.update(&block[..n]);
    }
}

/// inputs.json, written beside itself and renamed: a reader never sees half of it.
fn write_record(path: &str, record: &Value) -> Result<(), Fail> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, pyjson::dumps(record, Some(2), false) + "\n").map_err(|e| crash!("{}", py::os_error(&e, &tmp)))?;
    std::fs::rename(&tmp, path).map_err(|e| crash!("{}", py::os_error(&e, path)))
}

/// The record of this build's earlier fetch, or a new one.
fn load_record(path: &str, profile: &Profile) -> Result<Value, Fail> {
    let build = profile.table("os").get("build").cloned().unwrap_or(Value::Null);
    let mut record = Value::dict();
    record.set("build", build.clone());
    record.set("profile", profile.p.get("_file").cloned().unwrap_or(Value::Null));
    record.set("members", Value::dict());
    if std::path::Path::new(path).is_file() {
        let old = pyjson::load(path).map_err(|e| crash!("{path}: {e}"))?;
        if old.get("build") == Some(&build) {
            record.set("members", old.get("members").cloned().unwrap_or_else(Value::dict));
        }
    }
    Ok(record)
}

/// `fetch PROFILE INDIR [-j N] [--only ROLE,...] [--source URL|FILE]`.
pub fn cmd_fetch(profile_path: &str, indir: &str, jobs: usize, only: Option<&str>, given: Option<&str>) -> Result<(), Fail> {
    let profile = profile::load_profile(profile_path)?;
    let only: Option<Vec<&str>> = only.map(|list| list.split(',').collect());
    std::fs::create_dir_all(indir).map_err(|e| crash!("{}", py::os_error(&e, indir)))?;
    let record_path = py::join(indir, "inputs.json");
    let mut record = load_record(&record_path, &profile)?;
    let mut ipsw = Ipsw::open(source(&profile, given))?;
    ipsw.read_manifest(&profile)?;
    let identity = build_identity(&ipsw.manifest, &profile)?;
    let bad = check_members(&profile, &ipsw.infos(), Some(identity));
    if !bad.is_empty() {
        return Err(err!("the profile does not match the IPSW:\n  {}", bad.join("\n  ")));
    }
    for m in profile.members() {
        let (role, path) = (text(m, "role"), text(m, "path"));
        if only.as_ref().is_some_and(|roles| !roles.contains(&role)) {
            continue;
        }
        let name = py::basename(path);
        let out = py::join(indir, name);
        if decrypted_is_there(m, &record, indir, &out) {
            println!("{path}: decrypted image present");
            continue;
        }
        zip(ipsw.zip.get(path, &out, jobs, crate::remotezip::CHUNK, &|line| println!("{line}")))?;
        let (sha256, sha384) = digest_file(&out)?;
        for (key, got) in [("sha256", &sha256), ("sha384", &sha384)] {
            if let Some(pin) = m.get(key).filter(|pin| pin.as_str() != Some(got.as_str())) {
                // (the file stays, under another name, for a look at it)
                let _ = std::fs::rename(&out, format!("{out}.bad"));
                return Err(err!("{path}: {key} {got}, the profile pins {} (kept as {name}.bad)", py::str_value(pin)));
            }
        }
        let mut entry = Value::dict();
        entry.set("member", path);
        entry.set("size", m.get("size").cloned().unwrap_or(Value::Null));
        entry.set("crc32", text(m, "crc32"));
        entry.set("sha256", sha256.as_str());
        entry.set("sha384", sha384.as_str());
        let pinned: Vec<&str> = ["sha256", "sha384"].into_iter().filter(|k| m.get(k).is_some()).chain(["size", "crc32"]).collect();
        entry.set("pinned", pinned);
        if let Some(name) = profile::role(role).and_then(|r| r.1) {
            let entry_of = component(identity, name).ok_or_else(|| crash!("the build identity has no {name}"))?;
            let digest = dict(entry_of, "Digest").and_then(|d| d.as_data()).map_or(String::new(), oneios_common::hex);
            let same = digest == sha384;
            entry.set("buildmanifest_digest", digest);
            entry.set("buildmanifest_digest_is_file_sha384", same);
        }
        record.get_mut("members").expect("the record's members").set(role, entry);
        write_record(&record_path, &record)?;
    }
    println!("fetched into {indir}; hashes in {record_path}");
    Ok(())
}

/// An AEA whose decrypted image is recorded and still there is not downloaded again.
fn decrypted_is_there(m: &Value, record: &Value, indir: &str, out: &str) -> bool {
    let plain = py::join(indir, &profile::input_name(m));
    let known = record.get("members").and_then(|members| members.get(text(m, "role")));
    let size = std::fs::metadata(&plain).ok().filter(|meta| meta.is_file()).map(|meta| meta.len() as i128);
    py::truthy(m.get("aea"))
        && !std::path::Path::new(out).is_file()
        && size.is_some()
        && known.is_some_and(|k| k.get("member") == m.get("path") && k.get("crc32") == m.get("crc32") && k.get("plain_size").and_then(py::as_int) == size)
}

/// `record-plain INDIR ROLE PLAIN`.
pub fn record_plain(indir: &str, role: &str, plain: &str) -> Result<(), Fail> {
    let record_path = py::join(indir, "inputs.json");
    let mut record = pyjson::load(&record_path).map_err(|e| crash!("{record_path}: {e}"))?;
    let size = std::fs::metadata(plain).map_err(|e| crash!("{}", py::os_error(&e, plain)))?.len();
    let digest = crate::hashes::sha256_file(plain)?;
    let entry = record.get_mut("members").and_then(|members| members.get_mut(role)).ok_or_else(|| crash!("{record_path} has no member {role}"))?;
    entry.set("plain_size", size);
    entry.set("plain_sha256", digest);
    write_record(&record_path, &record)
}
