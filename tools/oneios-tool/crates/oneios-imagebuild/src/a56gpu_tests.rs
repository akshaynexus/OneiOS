use super::*;
use oneios_testkit::fixture::signed_macho;

const MH_DYLIB: u32 = 6;
const MH_BUNDLE: u32 = 8;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a56gpu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A staged runtime with the A56's two bundles under lib/guest/ and its runtime.json (metal_a56 for `target`, or
/// none), and the CDHashes of the bundles' Mach-Os.
fn runtime(root: &Path, target: Option<&str>) -> Vec<String> {
    let rt = root.join("runtime");
    let m = rt.join("lib/guest/OneiOSRadeonMetal.bundle");
    let g = rt.join("lib/guest/OneiOSRadeonGL.bundle");
    std::fs::create_dir_all(&m).unwrap();
    std::fs::create_dir_all(&g).unwrap();
    let (plugin, a) = signed_macho("OneiOSRadeonMetal", MH_BUNDLE, true, b"metal");
    let (air, b) = signed_macho("libOneiOSRadeonAir.dylib", MH_DYLIB, true, b"air");
    let (gl, c) = signed_macho("OneiOSRadeonGL", MH_BUNDLE, true, b"gl");
    std::fs::write(m.join("OneiOSRadeonMetal"), plugin).unwrap();
    std::fs::write(m.join("libOneiOSRadeonAir.dylib"), air).unwrap();
    std::fs::write(m.join("Info.plist"), b"<plist/>").unwrap();
    std::fs::write(m.join("oneios-target"), b"iPhone10,3 20F66\n").unwrap();
    std::fs::write(g.join("OneiOSRadeonGL"), gl).unwrap();
    std::fs::write(g.join("oneios-target"), b"iPhone10,3 20F66\n").unwrap();
    let entry = match target {
        Some(t) => format!(
            "{{\"target\": \"{t}\", \"metal\": {{\"bundle\": \"lib/guest/OneiOSRadeonMetal.bundle\", \"record\": \"lib/guest/OneiOSRadeonMetal.build.json\"}}, \"gl\": {{\"bundle\": \"lib/guest/OneiOSRadeonGL.bundle\", \"record\": \"lib/guest/OneiOSRadeonGL.build.json\"}}, \"cdhashes\": []}}"
        ),
        None => "null".into(),
    };
    std::fs::write(rt.join("runtime.json"), format!("{{\"format\": 1, \"metal\": null, \"metal_a56\": {entry}}}\n")).unwrap();
    let mut all = vec![a, b, c];
    all.sort();
    all
}

/// A staged image set of `device build` with an owner-trust.json that already lists `known`.
fn set(root: &Path, device: &str, build: &str, known: &str) -> PathBuf {
    let s = root.join("image");
    std::fs::create_dir_all(s.join("overlay/userland")).unwrap();
    let trust = format!("{{\n  \"cdhashes\": [\n    \"{known}\"\n  ],\n  \"files\": [],\n  \"schema\": 1\n}}\n");
    std::fs::write(s.join(TRUST), &trust).unwrap();
    let manifest = format!(
        "{{\"format\": 1, \"build\": \"{build}\", \"device\": \"{device}\", \"files\": [{{\"path\": \"kernel.kc\", \"size\": 1, \"sha256\": \"{}\"}}, {{\"path\": \"{TRUST}\", \"size\": {}, \"sha256\": \"{}\", \"role\": \"overlay\"}}]}}\n",
        "0".repeat(64),
        trust.len(),
        sha256_hex(trust.as_bytes())
    );
    std::fs::write(s.join(MANIFEST), manifest).unwrap();
    s
}

#[test]
fn an_ios16_set_gets_the_addon_its_trust_and_its_manifest() {
    let root = tmp("place");
    let hashes = runtime(&root, Some("iPhone10,3 20F66"));
    let known = hashes[0].clone();
    set(&root, "iPhone10,3", "20F66", &known);
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let args: Vec<String> = ["--stage", root.to_str().unwrap(), "--out", out.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
    assert_eq!(place_cmd(&args), 0);
    // the addon: both bundles under addons/, the Metal one with its .publish marker, the CDHashes all three
    let blob = std::fs::read(out.join(ADDON)).unwrap();
    let names: Vec<String> = oneios_ios::metal::read_gpuaddon(&blob).unwrap().into_iter().map(|f| f.0).collect();
    assert!(names.contains(&"addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal".to_string()), "{names:?}");
    assert!(names.contains(&"addons/OneiOSRadeonGL.bundle/OneiOSRadeonGL".to_string()), "{names:?}");
    let trust = json::load(out.join("owner-trust.json")).unwrap();
    let listed: Vec<String> = trust.get("cdhashes").unwrap().as_list().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(listed, hashes, "every CDHash once, sorted, the one already there kept");
    assert!(trust.get("schema").is_some(), "the other keys kept");
    // manifest.json: owner-trust.json's entry has the new file's size and SHA-256, the rest as it was
    let manifest = json::load(out.join(MANIFEST)).unwrap();
    let text = std::fs::read(out.join("owner-trust.json")).unwrap();
    let entry = manifest.get("files").unwrap().as_list().unwrap().iter().find(|f| f.get("path").and_then(Value::as_str) == Some(TRUST)).unwrap().clone();
    assert_eq!(entry.get("sha256").and_then(Value::as_str), Some(sha256_hex(&text).as_str()));
    assert_eq!(entry.get("size").and_then(Value::as_u64), Some(text.len() as u64));
    assert_eq!(entry.get("role").and_then(Value::as_str), Some("overlay"));
    assert_eq!(manifest.get("build").and_then(Value::as_str), Some("20F66"));
    // the staged set itself is not touched
    let before = std::fs::read_to_string(root.join("image").join(TRUST)).unwrap();
    assert!(before.contains(&known) && !before.contains(&hashes[1]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nothing_is_placed_for_ios27_or_without_the_plugins() {
    // an iOS 27 set (the S24 Ultra's): none, and DIR stays empty
    let root = tmp("ios27");
    runtime(&root, Some("iPhone10,3 20F66"));
    set(&root, "iPhone18,2", "24A446", &"1".repeat(40));
    let plugins = runtime_plugins(&root.join("runtime")).unwrap();
    let r = for_set(&root.join("image"), plugins.as_ref()).unwrap();
    assert_eq!(r.unwrap_err(), "the image is iPhone18,2 24A446, the A56 GPU plug-ins are for iPhone10,3 20F66");
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    let args: Vec<String> = ["--stage", root.to_str().unwrap(), "--out", out.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
    assert_eq!(place_cmd(&args), 0);
    assert!(std::fs::read_dir(&out).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&root);
    // a runtime without them (every runtime before this one): none
    let root = tmp("old-runtime");
    runtime(&root, None);
    set(&root, "iPhone10,3", "20F66", &"1".repeat(40));
    assert_eq!(runtime_plugins(&root.join("runtime")).unwrap(), None);
    let r = for_set(&root.join("image"), None).unwrap();
    assert!(r.unwrap_err().contains("carries no A56 GPU plug-ins"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn faults_are_errors_and_dir_must_be_empty() {
    let root = tmp("faults");
    runtime(&root, Some("iPhone10,3 20F66"));
    let s = set(&root, "iPhone10,3", "20F66", &"1".repeat(40));
    // a manifest that does not list owner-trust.json: the addon's code would not be trusted
    std::fs::write(s.join(MANIFEST), "{\"build\": \"20F66\", \"device\": \"iPhone10,3\", \"files\": []}\n").unwrap();
    let plugins = runtime_plugins(&root.join("runtime")).unwrap();
    assert!(for_set(&s, plugins.as_ref()).unwrap_err().contains("lists no overlay/userland/owner-trust.json"));
    // an out folder that is not empty
    let out = root.join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("x"), b"").unwrap();
    let args: Vec<String> = ["--stage", root.to_str().unwrap(), "--out", out.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
    assert_eq!(place_cmd(&args), 1);
    // runtime.json naming a bundle the runtime does not hold
    std::fs::remove_dir_all(root.join("runtime/lib/guest/OneiOSRadeonGL.bundle")).unwrap();
    assert!(runtime_plugins(&root.join("runtime")).unwrap_err().contains("does not hold"));
    // bad usage
    assert_eq!(place_cmd(&["--nope".to_string()]), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_trust_merge_keeps_every_key_and_each_cdhash_once() {
    let (text, new) = trust_with("{\"cdhashes\": [\"BB\"], \"files\": [1], \"schema\": 1}", &["aa".into(), "bb".into()]).unwrap();
    assert_eq!(new, 1);
    let v = json::parse(&text).unwrap();
    let listed: Vec<&str> = v.get("cdhashes").unwrap().as_list().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(listed, ["aa", "bb"]);
    assert!(v.get("files").is_some() && v.get("schema").is_some());
    assert!(trust_with("{\"files\": []}", &[]).is_err());
}

/// The placed line is the app's interface (oneios_root.sh a56_gpu_place reads its words with `read`): exactly
/// `@@A56GPU placed: kernel.kc.gpuaddon SHA BYTES; owner-trust.json SHA; manifest.json SHA`, seven words after the tag.
#[test]
fn the_placed_line_is_the_apps_seven_words() {
    let p = Placed { addon: b"NGPUADD2".to_vec(), trust: "{}".into(), manifest: "{}".into(), cdhashes: 3, new: 2 };
    let line = placed_line(&p);
    let words: Vec<&str> = line.strip_prefix("@@A56GPU placed: ").unwrap().split_whitespace().collect();
    assert_eq!(words.len(), 7, "{line}");
    assert_eq!((words[0], words[3], words[5]), ("kernel.kc.gpuaddon", "owner-trust.json", "manifest.json"));
    assert_eq!(words[2], "8;");
    for w in [words[1], words[4].trim_end_matches(';'), words[6]] {
        assert!(w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()), "{w}");
    }
    assert_eq!(counts_line(&p), "a56gpu: the addon's 3 CDHashes, 2 of them new in owner-trust.json");
}

/// Issue #87: Make iOS image for the A56 (iOS 16.5, 20F66) stopped at "not in the manifest: kernel.kc.gpuaddon",
/// because the sidecar went into the set before the verify. The make-image order: [`place_listed`] (owner-trust.json
/// and manifest.json), the set's verify, then [`write_sidecar`]. The verify passes on the placed set, the sidecar
/// is the runtime's addon, and it stays outside the manifest (the lists name it).
#[test]
fn make_image_places_the_sidecar_after_the_verify() {
    let root = tmp("make-image");
    let hashes = runtime(&root, Some("iPhone10,3 20F66"));
    let s = root.join("image");
    std::fs::create_dir_all(s.join("overlay/userland")).unwrap();
    let trust = "{\n  \"cdhashes\": [],\n  \"files\": [],\n  \"schema\": 1\n}\n";
    std::fs::write(s.join(TRUST), trust).unwrap();
    let kc = b"a kernel collection".to_vec();
    std::fs::write(s.join("kernel.kc"), &kc).unwrap();
    let manifest = format!(
        "{{\"name\": \"oneios-image\", \"format\": 1, \"version\": \"16.5\", \"build\": \"20F66\", \"device\": \"iPhone10,3\", \"files\": [{{\"path\": \"kernel.kc\", \"size\": {}, \"sha256\": \"{}\"}}, {{\"path\": \"{TRUST}\", \"size\": {}, \"sha256\": \"{}\", \"role\": \"overlay\"}}]}}\n",
        kc.len(),
        sha256_hex(&kc),
        trust.len(),
        sha256_hex(trust.as_bytes())
    );
    std::fs::write(s.join(MANIFEST), manifest).unwrap();
    let set_s = s.to_str().unwrap();
    oneios_image::verify_set(set_s, false).unwrap();

    let plugins = runtime_plugins(&root.join("runtime")).unwrap();
    let placed = place_listed(&s, plugins.as_ref()).unwrap().unwrap();
    assert!(!s.join(ADDON).exists(), "no sidecar before the verify");
    // the verify make-image runs: the new owner-trust.json against the new manifest.json, nothing extra
    oneios_image::verify_set(set_s, false).unwrap();
    let listed: Vec<String> = json::load(s.join(TRUST)).unwrap().get("cdhashes").unwrap().as_list().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(listed, hashes);

    let sha = write_sidecar(&placed, &s).unwrap();
    let blob = std::fs::read(s.join(ADDON)).unwrap();
    assert_eq!(sha, sha256_hex(&blob));
    assert!(blob.starts_with(b"NGPUADD2"));
    assert!(placed_line(&placed).contains(&format!("{ADDON} {sha} {}", blob.len())));
    // the sidecar is not a manifest file: a verify with it in the set refuses it, which is why it comes after
    let refused = oneios_image::verify_set(set_s, false).unwrap_err().to_string();
    assert!(refused.ends_with(&format!("not in the manifest: {ADDON}")), "{refused}");
    let _ = std::fs::remove_dir_all(&root);
}

/// An iOS 27 set (the S24 Ultra's) is left as it was by Make iOS image's placing: no file written.
#[test]
fn make_image_writes_nothing_for_ios27() {
    let root = tmp("make-image-27");
    runtime(&root, Some("iPhone10,3 20F66"));
    let s = set(&root, "iPhone18,2", "24A446", &"1".repeat(40));
    let before = (std::fs::read(s.join(MANIFEST)).unwrap(), std::fs::read(s.join(TRUST)).unwrap());
    let plugins = runtime_plugins(&root.join("runtime")).unwrap();
    assert!(place_listed(&s, plugins.as_ref()).unwrap().is_err());
    assert_eq!(before, (std::fs::read(s.join(MANIFEST)).unwrap(), std::fs::read(s.join(TRUST)).unwrap()));
    assert!(!s.join(ADDON).exists());
    let _ = std::fs::remove_dir_all(&root);
}
