//! `manifest` (write a set's manifest.json and hash lists) and `verify` of a set folder, with imagetool.py's
//! checks, order and messages. The files are hashed in parallel; what is reported is what Python's one-by-one
//! pass in sorted order reports (its first error).

use crate::apfs;
use crate::hashes::{self, HashList};
use crate::profile::{self, check_set_path, Profile, FORMAT};
use crate::py;
use crate::{crash, err, Fail};
use oneios_common::pyjson::{self, Value};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const SET_NAME: &str = "oneios-image";

/// `set_files(setdir)`: every file below the folder except manifest.json, sorted. As `os.walk` sees them: a link
/// to a folder is neither listed nor followed, a folder that cannot be read is skipped.
pub fn set_files(setdir: &str) -> Result<Vec<String>, Fail> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel_dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(py::join(setdir, &rel_dir)) else { continue };
        for e in entries {
            let Ok(e) = e else { break };
            let name = e.file_name();
            let name = String::from_utf8(name.as_bytes().to_vec())
                .map_err(|e| crash!("{setdir}: the file name {} is not UTF-8", py::repr_str(&String::from_utf8_lossy(e.as_bytes()))))?;
            let rel = py::join(&rel_dir, &name);
            let is_link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            if fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false) {
                if !is_link {
                    stack.push(rel);
                }
            } else if rel != "manifest.json" {
                out.push(rel);
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn is_link(path: &str) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// What the parallel pass found for one file of the set.
struct Facts {
    size: u64,
    sha256: String,
    apfs: Option<Result<apfs::Info, Fail>>,
}

/// One pass of `verify` over a file: its SHA-256 and, with a usable hash list, the SHA-256 of each chunk; None when
/// the pass was ended early.
type Pass = Result<Option<(String, Option<Vec<u8>>)>, Fail>;

/// `make_manifest(profile, setdir, inputs, seed_info, tweaks)`. Returns (files, bytes) for the line it prints.
pub fn make_manifest(
    profile: &Profile,
    setdir: &str,
    inputs: Option<Value>,
    seed_info: Option<Value>,
    tweaks: Option<Value>,
) -> Result<(usize, i128), Fail> {
    let (o, image) = (profile.table("os"), profile.table("image"));
    let empty = Value::dict();
    let names = image.get("files").unwrap_or(&empty).as_dict().unwrap_or(&[]);
    // by_name: {v: k for k, v in names.items()}
    let by_name: HashMap<&str, &str> = names.iter().filter_map(|(k, v)| Some((v.as_str()?, k.as_str()))).collect();
    let chunk_value = profile.chunk();
    let chunk = py::as_int(&chunk_value).unwrap_or(hashes::DEFAULT_CHUNK as i128) as u64;
    let seed = profile.text("image", "seed");
    let kernel = profile.text("image", "kernel");
    let present = set_files(setdir)?;
    for rel in &present {
        check_set_path(rel, "set file")?;
        if is_link(&py::join(setdir, rel)) {
            return Err(err!("{rel}: a link; the set holds regular files only"));
        }
    }
    let listed: Vec<&String> = present.iter().filter(|rel| !rel.ends_with(".hashes")).collect();
    let role_of = |rel: &str| by_name.get(rel).copied();
    let is_volume = |rel: &str| role_of(rel).and_then(profile::role).is_some_and(|r| r.3 == "volume");
    // The first file that is no part of the set stops Python before it reads the files after it.
    let known = |rel: &str| role_of(rel).is_some() || rel == seed || rel == kernel || rel.starts_with("overlay/");
    let stop = listed.iter().position(|rel| !known(rel)).map_or(listed.len(), |i| i + 1);

    let facts: Vec<Result<Facts, Fail>> = listed[..stop]
        .par_iter()
        .map(|rel| {
            let full = py::join(setdir, rel);
            if is_volume(rel) || *rel == kernel {
                let (size, sha256) = hashes::write_hashes(&full, &py::join(setdir, &format!("{rel}.hashes")), chunk)?;
                let apfs = is_volume(rel).then(|| apfs::apfs_info(&full));
                Ok(Facts { size, sha256, apfs })
            } else {
                let size = hashes::file_size(&full)?;
                let apfs = (*rel == seed && seed_format(&seed_info) == Some("apfs-raw")).then(|| apfs::apfs_info(&full));
                Ok(Facts { size, sha256: hashes::sha256_file(&full)?, apfs })
            }
        })
        .collect();

    let mut files: Vec<Value> = Vec::new();
    for (rel, facts) in listed.iter().zip(facts) {
        let rel = rel.as_str();
        let facts = facts?;
        let role = role_of(rel);
        if let Some(role) = role {
            if profile::role(role).is_none() {
                return Err(crash!("[image.files] names {rel} for {}, which is not a role", py::repr_str(role)));
            }
        }
        let mut entry = Value::dict();
        entry.set("path", rel);
        entry.set("size", facts.size);
        entry.set("sha256", facts.sha256.as_str());
        if let Some(role) = role {
            let (_, component, _, kind) = *profile::role(role).unwrap();
            // Python: m["path"] of None is a TypeError
            let m = profile.member_of(role).ok_or(Fail::Usage)?;
            let aea = py::truthy(m.get("aea"));
            entry.set("role", role);
            let mut source = Value::dict();
            source.set("component", component);
            source.set("member", m.get("path").cloned().unwrap_or(Value::Null));
            source.set("decrypted", aea);
            entry.set("source", source);
            if kind == "volume" {
                entry.set("format", "apfs-raw");
                entry.set("read_only", true);
                entry.set("hashes", format!("{rel}.hashes"));
                entry.set("apfs", facts.apfs.expect("a volume's APFS facts")?.to_value());
                let rec = inputs.as_ref().filter(|i| py::truthy(Some(i))).and_then(|i| i.get("members")).and_then(|m| m.get(role));
                let want = rec.and_then(|r| r.get(if aea { "plain_sha256" } else { "sha256" }));
                if py::truthy(want) && want.and_then(Value::as_str) != Some(facts.sha256.as_str()) {
                    return Err(err!("{rel}: differs from the input the fetch recorded ({})", py::str_value(want.unwrap())));
                }
                let pin = m.get(if aea { "plain_sha256" } else { "sha256" });
                if py::truthy(pin) && pin.and_then(Value::as_str) != Some(facts.sha256.as_str()) {
                    return Err(err!("{rel}: SHA-256 {}, the profile pins {}", facts.sha256, py::str_value(pin.unwrap())));
                }
            } else {
                entry.set("format", if role.starts_with("trust-") { "img4-payload" } else { "root-hash" });
            }
        } else if rel == seed {
            entry.set("role", "data-seed");
            match seed_info.as_ref().filter(|s| py::truthy(Some(s))) {
                Some(Value::Dict(info)) => {
                    for (k, v) in info {
                        if k == "role" {
                            return Err(Fail::Usage); // update(role=..., **{"role": ...}): a TypeError
                        }
                        entry.set(k.as_str(), v.clone());
                    }
                }
                Some(_) => return Err(Fail::Usage),
                None => entry.set("format", "tar"),
            }
            match entry.get("format") {
                None => return Err(crash!("the seed information has no \"format\"")),
                Some(Value::Str(f)) if f == "apfs-raw" => {
                    let info = match facts.apfs {
                        Some(info) => info?,
                        None => apfs::apfs_info(&py::join(setdir, rel))?,
                    };
                    entry.set("apfs", info.to_value());
                }
                Some(_) => {}
            }
        } else if rel == kernel {
            entry.set("role", "kernel");
            entry.set("format", "kernel-collection");
            entry.set("read_only", true);
            entry.set("hashes", format!("{rel}.hashes"));
        } else if rel.starts_with("overlay/") {
            entry.set("role", "overlay");
        } else {
            return Err(err!("{rel}: not a file of this profile's set (names: [image.files], [image] seed, kernel, overlay/)"));
        }
        files.push(entry);
    }
    for rel in set_files(setdir)? {
        if rel.ends_with(".hashes") {
            if !files.iter().any(|f| f.get("hashes").and_then(Value::as_str) == Some(rel.as_str())) {
                return Err(err!("{rel}: a hash list without its image"));
            }
            let full = py::join(setdir, &rel);
            let mut entry = Value::dict();
            entry.set("path", rel.as_str());
            entry.set("size", hashes::file_size(&full)?);
            entry.set("sha256", hashes::sha256_file(&full)?);
            entry.set("role", "hashes");
            entry.set("format", "oneios-hash-list-1");
            files.push(entry);
        }
    }
    files.sort_by(|a, b| a.get("path").and_then(Value::as_str).cmp(&b.get("path").and_then(Value::as_str)));
    let have: HashSet<&str> = files.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).collect();
    let mut missing: Vec<String> = names.iter().map(|(_, v)| py::str_value(v)).filter(|n| !have.contains(n.as_str())).collect();
    if !have.contains(seed) {
        missing.push(seed.to_string());
    }
    if !missing.is_empty() {
        missing.sort();
        return Err(err!("the set is missing {}", missing.join(", ")));
    }

    let get = |t: &Value, key: &str| t.get(key).cloned().unwrap_or(Value::Null);
    let (ipsw, data) = (profile.table("ipsw"), profile.table("data"));
    let mut m = Value::dict();
    m.set("format", FORMAT);
    m.set("name", SET_NAME);
    m.set("id", get(image, "id"));
    let device = profile.text("os", "device");
    m.set("os", if device.starts_with("iPhone") { Value::str("iOS") } else { o.get("family").cloned().unwrap_or(Value::str("iOS")) });
    for key in ["version", "build", "device", "board", "arch", "chip_id", "board_id"] {
        m.set(key, get(o, key));
    }
    let mut about = Value::dict();
    about.set("name", get(o, "name"));
    about.set("file", get(&profile.p, "_file"));
    about.set("sha256", get(&profile.p, "_sha256"));
    m.set("profile", about);
    let mut source = Value::dict();
    source.set("ipsw_url", get(ipsw, "url"));
    source.set("ipsw_size", get(ipsw, "size"));
    source.set("ipsw_sha256", get(ipsw, "sha256"));
    m.set("source", source);
    m.set("stage", get(image, "stage"));
    m.set("chunk", chunk_value);
    let mut d = Value::dict();
    d.set("seed", seed);
    d.set("volume_uuid", data.get("volume_uuid").and_then(Value::as_str).unwrap_or("").to_uppercase());
    d.set("volume_name", data.get("volume_name").cloned().unwrap_or(Value::str("Data")));
    d.set("case_sensitive", true);
    d.set("store_gib", data.get("store_gib").cloned().unwrap_or(Value::Int(16)));
    m.set("data", d);
    m.set("kernel", if have.contains(kernel) { Value::str(kernel) } else { Value::Null });
    m.set("tweaks", tweaks.unwrap_or(Value::Null));
    let count = files.len();
    let bytes: i128 = files.iter().map(|f| f.get("size").and_then(py::as_int).unwrap_or(0)).sum();
    m.set("files", Value::List(files));
    let path = py::join(setdir, "manifest.json");
    fs::write(&path, pyjson::dumps(&m, Some(2), false) + "\n").map_err(|e| crash!("{}", py::os_error(&e, &path)))?;
    Ok((count, bytes))
}

fn seed_format(seed_info: &Option<Value>) -> Option<&str> {
    seed_info.as_ref()?.get("format")?.as_str()
}

/// `load_manifest(path)`.
pub fn load_manifest(path: &str) -> Result<Value, Fail> {
    let text = fs::read(path).map_err(|e| err!("{path}: {}", py::os_error(&e, path)))?;
    let text = String::from_utf8(text).map_err(|e| err!("{path}: 'utf-8' codec can't decode the file: {e}"))?;
    let m = pyjson::parse(&text).map_err(|e| err!("{path}: {e}"))?;
    if !matches!(m, Value::Dict(_)) || m.get("name").and_then(Value::as_str) != Some(SET_NAME) {
        return Err(err!("{path}: not a OneiOS image manifest"));
    }
    let format = m.get("format").and_then(py::as_int);
    if format.is_none_or(|f| f < 1) {
        return Err(err!("{path}: no format number"));
    }
    if format.unwrap() > FORMAT {
        return Err(err!("{path}: format {} is newer than this tool reads ({FORMAT})", py::str_value(m.get("format").unwrap())));
    }
    for key in ["version", "build", "device"] {
        if !matches!(m.get(key), Some(Value::Str(_))) {
            return Err(err!("{path}: {key} is missing"));
        }
    }
    let files = match m.get("files") {
        Some(Value::List(files)) if !files.is_empty() => files,
        _ => return Err(err!("{path}: no files")),
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for f in files {
        let rel = match (f, f.get("path"), f.get("size").and_then(py::as_int), f.get("sha256")) {
            (Value::Dict(_), Some(Value::Str(rel)), Some(size), Some(Value::Str(sha))) if size >= 0 && py::is_hex64(sha) => rel.as_str(),
            _ => return Err(err!("{path}: a file entry without path, size or sha256")),
        };
        check_set_path(rel, path)?;
        if rel == "manifest.json" || !seen.insert(rel) {
            return Err(err!("{path}: {rel} listed twice or not allowed"));
        }
    }
    for f in files {
        match f.get("hashes") {
            None => {}
            Some(Value::Str(h)) if seen.contains(h.as_str()) => {}
            Some(Value::List(_)) | Some(Value::Dict(_)) => return Err(Fail::Usage), // unhashable: a TypeError
            Some(other) => {
                return Err(err!(
                    "{path}: {} names the hash list {}, which is not listed",
                    f.get("path").and_then(Value::as_str).unwrap_or(""),
                    py::str_value(other)
                ))
            }
        }
    }
    Ok(m)
}

/// `verify_dir(setdir, quick)`: check a set folder against its manifest. quick: names and sizes only.
/// Returns the manifest.
pub fn verify_dir(setdir: &str, quick: bool) -> Result<Value, Fail> {
    let m = load_manifest(&py::join(setdir, "manifest.json"))?;
    let mut listed: Vec<(&str, &Value)> =
        m.get("files").and_then(Value::as_list).unwrap().iter().map(|f| (f.get("path").and_then(Value::as_str).unwrap(), f)).collect();
    listed.sort_by_key(|(rel, _)| *rel);
    let present = set_files(setdir)?;
    {
        let present: HashSet<&str> = present.iter().map(String::as_str).collect();
        let names: HashSet<&str> = listed.iter().map(|(rel, _)| *rel).collect();
        let missing: Vec<&str> = listed.iter().map(|(rel, _)| *rel).filter(|rel| !present.contains(rel)).collect();
        if !missing.is_empty() {
            return Err(err!("{setdir}: missing {}", missing.join(", ")));
        }
        let mut extra: Vec<&str> = present.iter().copied().filter(|rel| !names.contains(rel)).collect();
        if !extra.is_empty() {
            extra.sort_unstable();
            return Err(err!("{setdir}: not in the manifest: {}", extra.join(", ")));
        }
    }

    // The checks that read no file content, in order; `cheap` is the first one that fails.
    let mut sizes: Vec<u64> = Vec::with_capacity(listed.len());
    let mut cheap: Option<Fail> = None;
    for (rel, f) in &listed {
        let full = py::join(setdir, rel);
        if is_link(&full) || !fs::metadata(&full).map(|m| m.is_file()).unwrap_or(false) {
            cheap = Some(err!("{rel}: not a regular file"));
            break;
        }
        let size = hashes::file_size(&full)?;
        let want = f.get("size").unwrap();
        if py::as_int(want) != Some(size as i128) {
            cheap = Some(err!("{rel}: {size} bytes, the manifest says {}", py::str_value(want)));
            break;
        }
        sizes.push(size);
    }

    // The hash list of each file that names one (a list that cannot be used is reported in its turn, below).
    let hash_list = |f: &Value| -> Option<Result<HashList, Fail>> {
        let name = f.get("hashes")?.as_str()?;
        Some(hashes::read_hashes(&py::join(setdir, name)))
    };
    let mut lists: Vec<Option<Result<HashList, Fail>>> = Vec::new();
    let mut chunks: Vec<Option<Vec<u8>>> = Vec::new();
    if !quick {
        // Every file up to the first cheap failure is read once: its SHA-256 and, with a usable hash list, the
        // SHA-256 of each chunk. Python stops at the first file that is wrong: `wrong` is the earliest one known,
        // and the passes over the files after it end at once (their results are not looked at).
        lists = listed[..sizes.len()].par_iter().map(|(_, f)| hash_list(f)).collect();
        let wrong = AtomicUsize::new(usize::MAX);
        let read: Vec<Pass> = listed[..sizes.len()]
            .par_iter()
            .zip(sizes.par_iter())
            .zip(lists.par_iter())
            .enumerate()
            .map(|(i, (((rel, f), size), list))| {
                let full = py::join(setdir, rel);
                let stop = || wrong.load(Ordering::Relaxed) < i;
                let found = match list {
                    Some(Ok(list)) if list.size == *size => {
                        hashes::digests_until(&full, *size, list.chunk, &stop).map(|d| d.map(|d| (d.sha256, Some(d.chunks))))
                    }
                    _ => hashes::sha256_file_until(&full, &stop).map(|sha256| sha256.map(|sha256| (sha256, None))),
                };
                let good = matches!(&found, Ok(Some((sha256, _))) if f.get("sha256").and_then(Value::as_str) == Some(sha256.as_str()));
                if !good && !matches!(found, Ok(None)) {
                    wrong.fetch_min(i, Ordering::Relaxed);
                }
                found
            })
            .collect();
        for ((rel, f), read) in listed.iter().zip(read) {
            let (sha256, got) = read?.expect("a pass ends early only after an earlier file was found wrong");
            if f.get("sha256").and_then(Value::as_str) != Some(sha256.as_str()) {
                return Err(err!("{rel}: SHA-256 differs from the manifest"));
            }
            chunks.push(got);
        }
    }
    if let Some(e) = cheap {
        return Err(e);
    }

    for (i, (rel, f)) in listed.iter().enumerate() {
        if f.get("hashes").is_none() {
            continue;
        }
        let name = f.get("hashes").and_then(Value::as_str).unwrap();
        let list = if quick { hash_list(f).unwrap()? } else { lists[i].take().unwrap()? };
        let chunk_same = match m.get("chunk") {
            None => true,
            Some(c) => py::eq_int(Some(c), list.chunk as i128),
        };
        if f.get("size").and_then(py::as_int) != Some(list.size as i128) || !chunk_same {
            return Err(err!("{name}: is for {} bytes in {}-byte chunks, not for {rel}", list.size, list.chunk));
        }
        if !quick {
            let full = py::join(setdir, rel);
            let got = match chunks[i].take() {
                Some(got) => got,
                None => hashes::digests(&full, list.size, list.chunk)?.chunks,
            };
            hashes::first_difference(&full, &list, &got)?;
        }
    }
    Ok(m)
}

/// The line `verify` prints for a good set.
pub fn ok_line(m: &Value) -> String {
    let files = m.get("files").and_then(Value::as_list).unwrap_or(&[]);
    let bytes: i128 = files.iter().map(|f| f.get("size").and_then(py::as_int).unwrap_or(0)).sum();
    let id = m.get("id").or_else(|| m.get("build")).map_or(String::new(), py::str_value);
    format!("ok: {id} format {}, {} files, {bytes} bytes", m.get("format").map_or(String::new(), py::str_value), files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{fake_set, Tmp};

    fn load(path: &str) -> Value {
        pyjson::load(path).unwrap()
    }

    fn manifest(tmp: &Tmp, set: &str) -> Result<(usize, i128), Fail> {
        let profile = profile::load_profile(&tmp.path("test.toml")).unwrap();
        let seed_info = pyjson::parse("{\"format\": \"tar\", \"root\": \"/private/var\", \"entries\": 1}").unwrap();
        let tweaks = pyjson::parse("{\"hook\": \"userland/image-hook\"}").unwrap();
        make_manifest(&profile, set, None, Some(seed_info), Some(tweaks))
    }

    #[test]
    fn manifest_of_a_set() {
        let tmp = Tmp::new("manifest");
        let set = fake_set(&tmp, true, true);
        let (count, bytes) = manifest(&tmp, &set).unwrap();
        assert_eq!(count, 16);
        let text = fs::read_to_string(format!("{set}/manifest.json")).unwrap();
        assert!(text.starts_with("{\n  \"format\": 1,\n  \"name\": \"oneios-image\",\n  \"id\": \"ios-0.0.1-0T1-iTest1-1\",\n  \"os\": \"iOS\",\n  \"version\": \"0.0.1\","), "{text}");
        assert!(text.ends_with("    }\n  ]\n}\n"));
        let m = load(&format!("{set}/manifest.json"));
        let keys: Vec<&str> = m.as_dict().unwrap().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["format", "name", "id", "os", "version", "build", "device", "board", "arch", "chip_id", "board_id", "profile", "source", "stage", "chunk", "data", "kernel", "tweaks", "files"]
        );
        let files = m.get("files").unwrap().as_list().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.get("path").unwrap().as_str().unwrap()).collect();
        assert_eq!(paths, set_files(&set).unwrap());
        assert_eq!(bytes, files.iter().map(|f| f.get("size").unwrap().as_int().unwrap()).sum::<i128>());
        for f in files {
            let full = format!("{set}/{}", f.get("path").unwrap().as_str().unwrap());
            assert_eq!(f.get("size").unwrap().as_u64().unwrap(), fs::metadata(&full).unwrap().len());
            assert_eq!(f.get("sha256").unwrap().as_str().unwrap(), oneios_common::sha256_hex(&fs::read(&full).unwrap()));
        }
        let by = |path: &str| files.iter().find(|f| f.get("path").unwrap().as_str() == Some(path)).unwrap();
        let os = pyjson::dumps(by("os.img"), None, false);
        assert!(os.contains("\"role\": \"os\", \"source\": {\"component\": \"OS\", \"member\": \"001-00001-001.dmg.aea\", \"decrypted\": true}, \"format\": \"apfs-raw\", \"read_only\": true, \"hashes\": \"os.img.hashes\", \"apfs\": {\"container_offset\": 0, \"block_size\": 4096, \"block_count\": 40, "), "{os}");
        assert!(os.contains("\"volumes\": [{\"name\": \"FakeOS\", "), "{os}");
        let app = pyjson::dumps(by("app.img"), None, false);
        assert!(app.contains("\"decrypted\": false"), "{app}");
        assert_eq!(
            pyjson::dumps(by("os.img.hashes"), None, false).split(", \"role\"").nth(1).unwrap(),
            ": \"hashes\", \"format\": \"oneios-hash-list-1\"}"
        );
        assert!(pyjson::dumps(by("trust/static.trustcache"), None, false).ends_with("\"format\": \"img4-payload\"}"));
        assert!(pyjson::dumps(by("trust/os.root_hash"), None, false).ends_with("\"format\": \"root-hash\"}"));
        assert!(pyjson::dumps(by("data.seed"), None, false).ends_with("\"role\": \"data-seed\", \"format\": \"tar\", \"root\": \"/private/var\", \"entries\": 1}"));
        assert!(pyjson::dumps(by("kernel.kc"), None, false).ends_with("\"role\": \"kernel\", \"format\": \"kernel-collection\", \"read_only\": true, \"hashes\": \"kernel.kc.hashes\"}"));
        assert!(pyjson::dumps(by("overlay/trust/owner.bin"), None, false).ends_with("\"role\": \"overlay\"}"));
        assert_eq!(m.get("kernel").unwrap().as_str(), Some("kernel.kc"));
        assert_eq!(pyjson::dumps(m.get("tweaks").unwrap(), None, false), "{\"hook\": \"userland/image-hook\"}");
        assert_eq!(
            pyjson::dumps(m.get("data").unwrap(), None, false),
            "{\"seed\": \"data.seed\", \"volume_uuid\": \"11111111-2222-3333-4444-555555555555\", \"volume_name\": \"Data\", \"case_sensitive\": true, \"store_gib\": 16}"
        );
        // the hash lists are the ones `hashes` writes
        hashes::check_hashes(&format!("{set}/os.img"), &format!("{set}/os.img.hashes")).unwrap();
        assert_eq!(hashes::read_hashes(&format!("{set}/kernel.kc.hashes")).unwrap().chunk, 4096);
        let verified = verify_dir(&set, false).unwrap();
        assert_eq!(ok_line(&verified), format!("ok: ios-0.0.1-0T1-iTest1-1 format 1, 16 files, {bytes} bytes"));
        verify_dir(&set, true).unwrap();
        // the same inputs give the same manifest, byte for byte
        manifest(&tmp, &set).unwrap();
        assert_eq!(fs::read_to_string(format!("{set}/manifest.json")).unwrap(), text);
    }

    fn error(r: Result<impl Sized, Fail>) -> String {
        match r {
            Err(Fail::Error(m)) => m,
            Err(Fail::Crash(m)) => format!("crash: {m}"),
            Err(Fail::Usage) => "usage".into(),
            Ok(_) => "passed".into(),
        }
    }

    #[test]
    fn manifest_refuses() {
        let tmp = Tmp::new("manifest-refuses");
        let set = fake_set(&tmp, false, false);
        let profile = profile::load_profile(&tmp.path("test.toml")).unwrap();
        make_manifest(&profile, &set, None, None, None).unwrap();
        let m = load(&format!("{set}/manifest.json"));
        assert!(m.get("kernel").unwrap().is_null() && m.get("tweaks").unwrap().is_null());
        let seed = m.get("files").unwrap().as_list().unwrap().iter().find(|f| f.get("path").unwrap().as_str() == Some("data.seed")).unwrap();
        assert!(pyjson::dumps(seed, None, false).ends_with("\"role\": \"data-seed\", \"format\": \"tar\"}"));
        fs::remove_file(format!("{set}/trust/app.root_hash")).unwrap();
        assert_eq!(error(make_manifest(&profile, &set, None, None, None)), "the set is missing trust/app.root_hash");

        let set = fake_set(&tmp, true, true);
        fs::write(format!("{set}/stray.bin"), b"x").unwrap();
        assert_eq!(
            error(make_manifest(&profile, &set, None, None, None)),
            "stray.bin: not a file of this profile's set (names: [image.files], [image] seed, kernel, overlay/)"
        );
        fs::remove_file(format!("{set}/stray.bin")).unwrap();
        fs::write(format!("{set}/lost.hashes"), b"x").unwrap();
        assert_eq!(error(make_manifest(&profile, &set, None, None, None)), "lost.hashes: a hash list without its image");
        fs::remove_file(format!("{set}/lost.hashes")).unwrap();
        fs::write(format!("{set}/.hidden"), b"x").unwrap();
        assert_eq!(
            error(make_manifest(&profile, &set, None, None, None)),
            "set file: '.hidden' is not a set path (segments of letters, digits and ._+-, no leading dot)"
        );
        fs::remove_file(format!("{set}/.hidden")).unwrap();
        std::os::unix::fs::symlink("os.img", format!("{set}/overlay/link")).unwrap();
        assert_eq!(error(make_manifest(&profile, &set, None, None, None)), "overlay/link: a link; the set holds regular files only");
        fs::remove_file(format!("{set}/overlay/link")).unwrap();
        fs::write(format!("{set}/app.img"), vec![0u8; 8192]).unwrap();
        assert_eq!(error(make_manifest(&profile, &set, None, None, None)), format!("{set}/app.img: no APFS container found: not APFS"));

        // an image that is not the one the fetch recorded, and one the profile does not pin
        let set = fake_set(&tmp, true, true);
        let inputs = pyjson::parse(&format!("{{\"members\": {{\"os\": {{\"plain_sha256\": \"{}\"}}}}}}", "0".repeat(64))).unwrap();
        assert_eq!(
            error(make_manifest(&profile, &set, Some(inputs), None, None)),
            format!("os.img: differs from the input the fetch recorded ({})", "0".repeat(64))
        );
        let pinned = fs::read_to_string(tmp.path("test.toml")).unwrap().replace("crc32 = \"00000003\"", &format!("crc32 = \"00000003\"\nsha256 = \"{}\"", "1".repeat(64)));
        let pinned = profile::load_profile(&tmp.write("pinned.toml", pinned.as_bytes())).unwrap();
        let sha = oneios_common::sha256_hex(&fs::read(format!("{set}/app.img")).unwrap());
        assert_eq!(error(make_manifest(&pinned, &set, None, None, None)), format!("app.img: SHA-256 {sha}, the profile pins {}", "1".repeat(64)));
    }

    #[test]
    fn verify_catches() {
        let tmp = Tmp::new("verify");
        let set = fake_set(&tmp, true, true);
        manifest(&tmp, &set).unwrap();
        verify_dir(&set, false).unwrap();
        let copy = tmp.path("copy");
        let broken = |change: &dyn Fn(&str), quick: bool| -> String {
            let _ = fs::remove_dir_all(&copy);
            crate::fixtures::copy_tree(&set, &copy);
            change(&copy);
            error(verify_dir(&copy, quick))
        };
        fn flip(rel: &'static str, at: usize) -> impl Fn(&str) {
            move |d: &str| {
                let path = format!("{d}/{rel}");
                let mut data = fs::read(&path).unwrap();
                data[at] ^= 1;
                fs::write(&path, data).unwrap();
            }
        }
        fn edit(change: impl Fn(&mut Value)) -> impl Fn(&str) {
            move |d: &str| {
                let path = format!("{d}/manifest.json");
                let mut m = pyjson::load(&path).unwrap();
                change(&mut m);
                fs::write(&path, pyjson::dumps(&m, None, false)).unwrap();
            }
        }
        fn first(m: &mut Value) -> &mut Value {
            let Some(Value::List(files)) = m.get_mut("files") else { panic!() };
            &mut files[0]
        }
        assert_eq!(broken(&flip("os.img", 20000), false), "os.img: SHA-256 differs from the manifest");
        assert_eq!(broken(&flip("trust/static.trustcache", 3), false), "trust/static.trustcache: SHA-256 differs from the manifest");
        assert_eq!(broken(&flip("os.img.hashes", 40), false), "os.img.hashes: SHA-256 differs from the manifest");
        assert_eq!(broken(&flip("os.img", 20000), true), "passed");
        assert_eq!(broken(&|d| fs::remove_file(format!("{d}/systemos.img")).unwrap(), false), format!("{copy}: missing systemos.img"));
        assert_eq!(broken(&|d| fs::write(format!("{d}/extra"), b"").unwrap(), false), format!("{copy}: not in the manifest: extra"));
        let grow = |d: &str| {
            let mut data = fs::read(format!("{d}/app.img")).unwrap();
            data.push(b'x');
            fs::write(format!("{d}/app.img"), data).unwrap();
        };
        assert_eq!(broken(&grow, true), "app.img: 32769 bytes, the manifest says 32768");
        // Python reads the files before app.img in order, so a bad one before it is what it reports
        assert_eq!(broken(&grow, false), "app.img: 32769 bytes, the manifest says 32768");
        let both = |d: &str| {
            grow(d);
            flip("trust/static.trustcache", 3)(d);
            flip("app.img.hashes", 40)(d);
        };
        assert_eq!(broken(&both, false), "app.img: 32769 bytes, the manifest says 32768");
        let earlier = |d: &str| {
            fs::write(format!("{d}/os.img"), [fs::read(format!("{d}/os.img")).unwrap(), vec![0]].concat()).unwrap();
            flip("app.img", 9)(d);
        };
        assert_eq!(broken(&earlier, false), "app.img: SHA-256 differs from the manifest");
        let link = |d: &str| {
            fs::rename(format!("{d}/kernel.kc"), format!("{d}.k")).unwrap();
            std::os::unix::fs::symlink(format!("{d}.k"), format!("{d}/kernel.kc")).unwrap();
        };
        assert_eq!(broken(&link, true), "kernel.kc: not a regular file");
        assert_eq!(broken(&edit(|m: &mut Value|m.set("format", 2)), false), format!("{copy}/manifest.json: format 2 is newer than this tool reads (1)"));
        assert_eq!(broken(&edit(|m: &mut Value|m.set("format", "1")), false), format!("{copy}/manifest.json: no format number"));
        assert_eq!(broken(&edit(|m: &mut Value|m.set("name", "other")), false), format!("{copy}/manifest.json: not a OneiOS image manifest"));
        assert_eq!(
            broken(&edit(|m: &mut Value|first(m).set("path", "../escape")), false),
            format!("{copy}/manifest.json: '../escape' is not a set path (segments of letters, digits and ._+-, no leading dot)")
        );
        let twice = |m: &mut Value| {
            let copy = first(m).clone();
            m.get_mut("files").unwrap().push(copy);
        };
        assert_eq!(broken(&edit(twice), false), format!("{copy}/manifest.json: app.img listed twice or not allowed"));
        assert_eq!(broken(&edit(|m: &mut Value|first(m).set("sha256", "xyz")), false), format!("{copy}/manifest.json: a file entry without path, size or sha256"));
        assert_eq!(broken(&edit(|m: &mut Value|first(m).set("size", -1)), false), format!("{copy}/manifest.json: a file entry without path, size or sha256"));
        assert_eq!(broken(&edit(|m: &mut Value|first(m).set("hashes", "nothing")), false), format!("{copy}/manifest.json: app.img names the hash list nothing, which is not listed"));
        let no_build = |m: &mut Value| {
            let Value::Dict(items) = m else { panic!() };
            items.retain(|(k, _)| k != "build");
        };
        assert_eq!(broken(&edit(no_build), false), format!("{copy}/manifest.json: build is missing"));
        assert_eq!(broken(&edit(|m: &mut Value|m.set("files", Value::list())), false), format!("{copy}/manifest.json: no files"));
        assert_eq!(broken(&edit(|m: &mut Value|m.set("chunk", 8192)), true), "app.img.hashes: is for 32768 bytes in 4096-byte chunks, not for app.img");
        assert_eq!(
            broken(&|d| fs::remove_file(format!("{d}/manifest.json")).unwrap(), false),
            format!("{copy}/manifest.json: [Errno 2] No such file or directory: '{copy}/manifest.json'")
        );
        // a hash list that passes its own SHA-256 but belongs to other content
        let swap = |d: &str| {
            let size = fs::metadata(format!("{d}/os.img")).unwrap().len() as usize;
            fs::write(format!("{d}/x"), crate::fixtures::noise(size, 99)).unwrap();
            hashes::write_hashes(&format!("{d}/x"), &format!("{d}/os.img.hashes"), 4096).unwrap();
            fs::remove_file(format!("{d}/x")).unwrap();
            let sha = oneios_common::sha256_hex(&fs::read(format!("{d}/os.img.hashes")).unwrap());
            let path = format!("{d}/manifest.json");
            let mut m = pyjson::load(&path).unwrap();
            let Some(Value::List(files)) = m.get_mut("files") else { panic!() };
            files.iter_mut().find(|f| f.get("path").unwrap().as_str() == Some("os.img.hashes")).unwrap().set("sha256", sha);
            fs::write(&path, pyjson::dumps(&m, None, false)).unwrap();
        };
        assert_eq!(broken(&swap, false), format!("{copy}/os.img: chunk 0 (offset 0) differs from its hash list"));
        // a hash list for another size, with its own SHA-256 right
        let other_size = |d: &str| {
            fs::write(format!("{d}/x"), crate::fixtures::noise(5000, 1)).unwrap();
            hashes::write_hashes(&format!("{d}/x"), &format!("{d}/app.img.hashes"), 4096).unwrap();
            fs::remove_file(format!("{d}/x")).unwrap();
            let data = fs::read(format!("{d}/app.img.hashes")).unwrap();
            let path = format!("{d}/manifest.json");
            let mut m = pyjson::load(&path).unwrap();
            let Some(Value::List(files)) = m.get_mut("files") else { panic!() };
            let f = files.iter_mut().find(|f| f.get("path").unwrap().as_str() == Some("app.img.hashes")).unwrap();
            f.set("sha256", oneios_common::sha256_hex(&data));
            f.set("size", data.len());
            fs::write(&path, pyjson::dumps(&m, None, false)).unwrap();
        };
        assert_eq!(broken(&other_size, false), "app.img.hashes: is for 5000 bytes in 4096-byte chunks, not for app.img");
        // unknown keys are ignored: a later writer may add them without a new format number
        let path = format!("{set}/manifest.json");
        let mut m = pyjson::load(&path).unwrap();
        m.set("future_key", pyjson::parse("{\"a\": 1}").unwrap());
        fs::write(&path, pyjson::dumps(&m, None, false)).unwrap();
        verify_dir(&set, false).unwrap();
    }
}
