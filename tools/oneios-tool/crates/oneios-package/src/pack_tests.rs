//! pack.rs's tests: every refusal rule with its message, what is accepted, and the order two refusals come in.
//! tests/packagepy/compare-golden.sh has the same rules against pack.py's frozen output.

use super::*;
use std::path::PathBuf;

/// A stage with these files, in a scratch folder of its own (numbered: two tests that both name theirs "fileset" ran
/// at once in one folder, and one saw the other's files, 2026-11-25).
fn stage(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("oneios-package-test.{}.{n}.{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, bytes) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    root
}

/// The refusal of `check` for a stage with these files; "" when it is accepted.
fn refusal(name: &str, files: &[(&str, &[u8])]) -> String {
    let root = stage(name, files);
    let result = check(&root);
    let _ = std::fs::remove_dir_all(&root);
    match result {
        Ok(_) => String::new(),
        Err(Stop::Refused(message)) => message,
        Err(Stop::Failed(message)) => panic!("{message}"),
    }
}

/// A Mach-O header: magic, cputype, cpusubtype, filetype, and 16 more bytes.
fn macho(magic: [u8; 4], cputype: u32, filetype: u32) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    for word in [cputype, 2, filetype, 0, 0, 0, 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

const LE64: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];

/// The good record of a kernel, with `change` applied to its text.
fn record(kernel: &[u8], change: impl Fn(String) -> String) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let sum = oneios_common::hex(&Sha256::digest(kernel));
    change(format!(
        "{{\"builder\": \"oneios-kernel\", \"build_id\": \"test-1\", \"sha256\": \"{sum}\", \"prelinked\": false, \"kernelcache_kexts_added\": false, \"apple_binary_inputs\": [], \"restricted_sources\": [], \"xnu\": \"xnu-0\"}}"
    ))
    .into_bytes()
}

#[test]
fn every_firmware_name_is_refused() {
    let names = [
        ("kernelcache.release.iphone18", "kernelcache*"),
        ("a-cryptex1", "*cryptex*"),
        ("r.ipsw", "*.ipsw"),
        ("d.dmg", "*.dmg"),
        ("o.dmg.aea", "*.aea"),
        ("o.img", "*.img"),
        ("d.simg", "*.simg"),
        ("s.erofs", "*.erofs"),
        ("k.im4p", "*.im4p"),
        ("b.img4", "*.img4"),
        ("a.im4m", "*.im4m"),
        ("k.kc", "*.kc"),
        ("s.trustcache", "*.trustcache"),
        ("BuildManifest.plist", "buildmanifest.plist"),
        ("Restore.plist", "restore.plist"),
        ("a.shsh2", "*.shsh*"),
        ("dyld_shared_cache_arm64e", "dyld_shared_cache*"),
        ("I.kext", "*.kext"),
        ("S.dext", "*.dext"),
        ("UPPER.IPSW", "*.ipsw"),
    ];
    // every pattern of the list has a name here
    for pattern in APPLE_NAMES {
        assert!(names.iter().any(|(_, p)| *p == pattern), "{pattern}");
    }
    for (name, pattern) in names {
        let path = format!("deep/{name}");
        assert_eq!(refusal("names", &[(&path, b"x")]), format!("{path}: the name matches {pattern} (Apple firmware)"));
    }
    // names that only look like the patterns, and folders named like firmware
    assert_eq!(refusal("plain", &[("buildmanifest.plist.txt", b"x"), ("my.kextension", b"x"), ("x.imgs", b"x"), ("a.kc/x", b"x"), ("I.kext/plain", b"x")]), "");
}

#[test]
fn every_apple_container_is_refused_by_its_content() {
    let mut udif = vec![b'd'; 300];
    udif.extend_from_slice(b"koly");
    udif.extend(vec![b'e'; 508]);
    let apfs_at_32 = [&[b'0'; 32][..], b"NXSB"].concat();
    let cases: [(&[u8], &str); 11] = [
        (b"\x30\x83\x12\x34\x56\x16\x04IM4P\x16\x04krnl", "IMG4 container"),
        (b"\x30\x82\x01\x00\x16\x04IMG4", "IMG4 container"),
        (b"\x30\x82\x01\x00\x16\x04IM4M", "IMG4 container"),
        (b"\x30\x82\x01\x00\x16\x04IM4R", "IMG4 container"),
        (b"AEA1\0\0\0\0", "AEA archive"),
        (&udif, "disk image (UDIF)"),
        (b"koly", "disk image (UDIF)"),
        (b"dyld_v1  arm64e", "dyld cache or APFS container"),
        (b"NXSB....", "dyld cache or APFS container"),
        (&apfs_at_32, "dyld cache or APFS container"),
        // the Image4 tag after the first 24 bytes: not looked at
        (b"\x30\x82\x01\x00\x16\x04IM4X-----------------IM4P", ""),
    ];
    for (bytes, kind) in cases {
        let want = if kind.is_empty() { String::new() } else { format!("blob: {kind}") };
        assert_eq!(refusal("content", &[("blob", bytes)]), want);
    }
    // the UDIF letters one byte off, a binary plist, IMG4 letters without the DER byte, an ELF program, nothing
    udif.pop();
    assert_eq!(refusal("accepted", &[("blob", &udif), ("list", b"bplist00"), ("letters", b"IMG4 without the DER byte"), ("elf", b"\x7fELF"), ("empty", b"")]), "");
}

#[test]
fn every_macho_but_our_kernel_is_refused() {
    for magic in MACHO_MAGICS.iter().filter(|m| **m != LE64) {
        assert_eq!(refusal("magic", &[("bin/tool", &macho(*magic, 0x0100_000c, 2))]), "bin/tool: a Mach-O that is not a 64-bit little-endian image");
    }
    let kernel = macho(LE64, 0x0100_000c, 2);
    let fileset = macho(LE64, 0x0100_000c, 12);
    let good = |bytes: &[u8]| record(bytes, |text| text);
    assert_eq!(refusal("cut", &[("k", &LE64)]), "k: a Mach-O magic with no header behind it");
    assert_eq!(refusal("fileset", &[("k", &fileset), ("k.build.json", &good(&fileset))]), "k: a kernel collection (MH_FILESET) holds Apple's kexts; it belongs to the image set");
    for other in [macho(LE64, 0x0100_000c, 6), macho(LE64, 0x0100_0007, 2)] {
        assert_eq!(refusal("other", &[("k", &other), ("k.build.json", &good(&other))]), "k: a Mach-O that is not an arm64 kernel executable");
    }
    assert_eq!(refusal("no-record", &[("k", &kernel)]), "k: a Mach-O without a build record (k.build.json)");
    assert!(refusal("no-json", &[("k", &kernel), ("k.build.json", b"not json")]).starts_with("k: unreadable build record: "));
    assert_eq!(refusal("list", &[("k", &kernel), ("k.build.json", b"[]")]), "k: unreadable build record: not a JSON object");
    let changed: [(&str, &str, &str); 12] = [
        ("\"oneios-kernel\"", "\"someone\"", "k: the build record is not from kernel/ (builder 'someone')"),
        ("\"builder\": \"oneios-kernel\", ", "", "k: the build record is not from kernel/ (builder None)"),
        ("\"sha256\": \"", "\"sha256\": \"0", "k: the build record is for another file (SHA-256 differs)"),
        ("\"kernelcache_kexts_added\": false", "\"kernelcache_kexts_added\": true", "k: the build record does not say \"no kernelcache kexts, not prelinked\""),
        ("\"kernelcache_kexts_added\": false, ", "", "k: the build record does not say \"no kernelcache kexts, not prelinked\""),
        ("\"kernelcache_kexts_added\": false", "\"kernelcache_kexts_added\": 0", "k: the build record does not say \"no kernelcache kexts, not prelinked\""),
        ("\"prelinked\": false", "\"prelinked\": true", "k: the build record does not say \"no kernelcache kexts, not prelinked\""),
        ("\"apple_binary_inputs\": []", "\"apple_binary_inputs\": [\"a.kext\", \"b\"]", "k: the build record lists Apple binary inputs: ['a.kext', 'b']"),
        ("\"apple_binary_inputs\": [], ", "", "k: the build record lists Apple binary inputs: None"),
        ("\"restricted_sources\": []", "\"restricted_sources\": [\"corecrypto-2023\"]", "k: the kernel links sources that may not be redistributed: ['corecrypto-2023']"),
        ("\"build_id\": \"test-1\", ", "", "k: the build record has no build_id"),
        ("\"build_id\": \"test-1\"", "\"build_id\": \"\"", "k: the build record has no build_id"),
    ];
    for (from, to, message) in changed {
        let bad = record(&kernel, |text| {
            assert!(text.contains(from), "{from}");
            text.replacen(from, to, 1)
        });
        assert_eq!(refusal("record", &[("k", &kernel), ("k.build.json", &bad)]), message);
    }
    assert_eq!(
        refusal("two", &[("a/k", &kernel), ("a/k.build.json", &good(&kernel)), ("b/k", &kernel), ("b/k.build.json", &good(&kernel))]),
        "b/k: a second kernel in one package"
    );
    assert_eq!(refusal("own", &[("kernel/xnu.macho", &kernel), ("kernel/xnu.macho.build.json", &good(&kernel))]), "");
}

#[test]
fn paths_links_and_special_files_are_refused() {
    for (path, refused_at, segment) in [
        ("notes,v2.txt", "notes,v2.txt", "notes,v2.txt"),
        ("two words", "two words", "two words"),
        (".hidden", ".hidden", ".hidden"),
        ("dir/-dash", "dir/-dash", "-dash"),
        ("bad dir/fine", "bad dir", "bad dir"),
    ] {
        assert_eq!(refusal("path", &[(path, b"x")]), format!("{refused_at}: path segment '{segment}' is outside [A-Za-z0-9_][A-Za-z0-9._+-]*"));
    }
    assert_eq!(refusal("allowed", &[("ok/a+b_c-d.e", b"x")]), "");
    let root = stage("link", &[("a", b"x")]);
    std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
    assert!(matches!(check(&root), Err(Stop::Refused(m)) if m == "link: not a regular file or directory"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_stages_files_come_before_its_folders() {
    assert_eq!(refusal("order1", &[("z.dmg", b"x"), ("a/kernelcache", b"x")]), "z.dmg: the name matches *.dmg (Apple firmware)");
    assert_eq!(refusal("order2", &[("a/z.dmg", b"x"), ("b/a.ipsw", b"x")]), "a/z.dmg: the name matches *.dmg (Apple firmware)");
    // a path the unpacker rejects is refused before any name or content is looked at
    assert_eq!(refusal("order3", &[("a.dmg", b"x"), ("zz/bad,name", b"x")]), "zz/bad,name: path segment 'bad,name' is outside [A-Za-z0-9_][A-Za-z0-9._+-]*");
}

#[test]
fn the_manifest_needs_an_executable_xnuhost() {
    let root = stage("manifest", &[("bin/xnuhost", b"#!/bin/sh\n"), ("NOTICE", b"n")]);
    assert!(matches!(manifest(&root, "0.1.0", "00", "0", "0.4.80", "0"), Err(Stop::Refused(m)) if m == "bin/xnuhost is missing or not executable"));
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o700)).unwrap();
    manifest(&root, "0.1.0", "00", "1", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    let files = document.get("files").and_then(Value::as_list).unwrap();
    let listed: Vec<(&str, &str)> = files.iter().map(|f| (f.get("path").and_then(Value::as_str).unwrap(), f.get("mode").and_then(Value::as_str).unwrap())).collect();
    assert_eq!(listed, [("NOTICE", "0644"), ("bin/xnuhost", "0755")]);
    assert_eq!(document.get("git").and_then(|g| g.get("dirty")), Some(&Value::Bool(true)));
    assert!(document.get("kernel").is_some_and(Value::is_null));
    let _ = std::fs::remove_dir_all(&root);
}

/// A Metal bundle as userland/metal/build.sh leaves it under lib/guest/, with its record, and the capture plug-in:
/// the files, and the CDHashes of the bundle's two Mach-Os.
fn guest_files(target: &str, record_target: &str) -> (Vec<(String, Vec<u8>)>, [String; 2]) {
    use oneios_testkit::fixture::signed_macho;
    let (plugin, plugin_hash) = signed_macho("org.oneios.test.NativeAdrenoMetal", MH_BUNDLE, true, b"plug-in");
    let (turnip, turnip_hash) = signed_macho("org.oneios.test.turnip", MH_DYLIB, true, b"turnip");
    let (camera, _) = signed_macho("com.oneios.H9ISP", MH_DYLIB, true, b"camera");
    let record = format!(
        "{{\"bundle\": \"NativeAdrenoMetal.bundle\", \"target\": \"{record_target}\", \"files\": {{\"NativeAdrenoMetal\": {{\"cdhash\": \"{plugin_hash}\"}}, \"libvulkan_freedreno.dylib\": {{\"cdhash\": \"{turnip_hash}\"}}}}, \"cdhashes\": [\"{plugin_hash}\", \"{turnip_hash}\"]}}"
    );
    let files = vec![
        ("bin/xnuhost".to_string(), b"#!/bin/sh\n".to_vec()),
        (format!("{METAL_BUNDLE}/Info.plist"), b"<plist/>".to_vec()),
        (format!("{METAL_BUNDLE}/NativeAdrenoMetal"), plugin),
        (format!("{METAL_BUNDLE}/libvulkan_freedreno.dylib"), turnip),
        (format!("{METAL_BUNDLE}/oneios-target"), format!("{target}\n").into_bytes()),
        (METAL_RECORD.to_string(), record.into_bytes()),
        (format!("{CAMERA_DIR}/H9ISP.mediacapture"), camera),
    ];
    (files, [plugin_hash, turnip_hash])
}

fn refusal_of(name: &str, files: &[(String, Vec<u8>)]) -> String {
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    refusal(name, &borrowed)
}

#[test]
fn our_guest_plugins_are_taken_and_named_in_the_manifest() {
    let (files, hashes) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    assert_eq!(refusal_of("guest-ok", &files), "");
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let root = stage("guest-manifest", &borrowed);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    let metal = document.get("metal").unwrap();
    assert_eq!(metal.get("bundle").and_then(Value::as_str), Some(METAL_BUNDLE));
    assert_eq!(metal.get("record").and_then(Value::as_str), Some(METAL_RECORD));
    assert_eq!(metal.get("target").and_then(Value::as_str), Some("iPhone18,2 24A446"));
    let mut want = hashes.to_vec();
    want.sort();
    let listed: Vec<String> = metal.get("cdhashes").and_then(Value::as_list).unwrap().iter().map(str_value).collect();
    assert_eq!(listed, want);
    assert_eq!(document.get("camera").and_then(|c| c.get("path")).and_then(Value::as_str), Some(CAMERA_DIR));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn guest_code_that_is_not_ours_is_refused() {
    use oneios_testkit::fixture::signed_macho;
    let library = |id: &str, filetype: u32, adhoc: bool| signed_macho(id, filetype, adhoc, b"code").0;
    let one = |name: &str, bytes: Vec<u8>| refusal_of(name, &[("lib/guest/x/lib.dylib".to_string(), bytes)]);
    assert_eq!(one("apple", library("com.apple.test", MH_DYLIB, false)), "lib/guest/x/lib.dylib: not signed ad hoc (com.apple.test): our builds sign ad hoc, Apple's code never is");
    assert_eq!(one("apple-adhoc", library("com.apple.test", MH_DYLIB, true)), "lib/guest/x/lib.dylib: signed as 'com.apple.test', not as our own code");
    assert_eq!(one("execute", library("org.oneios.tool", MH_EXECUTE, true)), "lib/guest/x/lib.dylib: a Mach-O under lib/guest/ that is not an arm64 library or bundle (filetype 2)");
    assert_eq!(one("fileset", library("org.oneios.kc", MH_FILESET, true)), "lib/guest/x/lib.dylib: a kernel collection (MH_FILESET) holds Apple's kexts; it belongs to the image set");
    assert!(one("unsigned", macho(LE64, 0x0100_000c, MH_DYLIB)).starts_with("lib/guest/x/lib.dylib: a guest Mach-O without a readable signature ("));
    // outside lib/guest/ a library is refused as before: only the kernel is taken there
    assert_eq!(refusal_of("elsewhere", &[("lib/x.dylib".to_string(), library("org.oneios.x", MH_DYLIB, true))]), "lib/x.dylib: a Mach-O that is not an arm64 kernel executable");
}

#[test]
fn a_metal_bundle_must_match_its_record() {
    // a CDHash the record does not list (the bundle changed after its build)
    let (mut files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    files[3].1 = oneios_testkit::fixture::signed_macho("org.oneios.test.turnip", MH_DYLIB, true, b"rebuilt").0;
    assert!(refusal_of("changed", &files).starts_with(&format!("{METAL_BUNDLE}/libvulkan_freedreno.dylib: CDHash ")));
    // no record
    let (files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    let without: Vec<_> = files.iter().filter(|(p, _)| p != METAL_RECORD).cloned().collect();
    assert_eq!(refusal_of("no-record", &without), format!("{METAL_BUNDLE}/NativeAdrenoMetal: a Mach-O in a bundle without its build record ({METAL_RECORD})"));
    // oneios-target and the record name different targets
    let (files, _) = guest_files("iPhone18,2 24A446", "iPhone17,1 23A341");
    assert_eq!(refusal_of("target", &files), format!("{METAL_BUNDLE}: oneios-target says 'iPhone18,2 24A446', its record {METAL_RECORD} 'iPhone17,1 23A341'"));
    // no oneios-target
    let (files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    let without: Vec<_> = files.iter().filter(|(p, _)| !p.ends_with("oneios-target")).cloned().collect();
    assert_eq!(refusal_of("no-target", &without), format!("{METAL_BUNDLE}: not a whole Metal bundle (NativeAdrenoMetal and oneios-target)"));
    // the capture plug-in alone
    let (files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    let camera_only: Vec<_> = files.iter().filter(|(p, _)| !p.starts_with("lib/guest/NativeAdrenoMetal")).cloned().collect();
    assert_eq!(refusal_of("camera-only", &camera_only), format!("{CAMERA_DIR}/H9ISP.mediacapture: the capture plug-in without the Metal bundle, whose target says which iOS both are built for"));
}

#[test]
fn a_second_target_is_listed_beside_the_default() {
    let (mut files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    // the second target as runtime/package.sh's METAL_MORE_DIRS places it: lib/guest/24B156/
    let more: Vec<(String, Vec<u8>)> = guest_files("iPhone18,2 24B156", "iPhone18,2 24B156")
        .0
        .into_iter()
        .filter(|(p, _)| p.starts_with("lib/guest/"))
        .map(|(p, b)| (p.replacen("lib/guest/", "lib/guest/24B156/", 1), b))
        .collect();
    files.extend(more.clone());
    assert_eq!(refusal_of("two-ok", &files), "");
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let root = stage("two-manifest", &borrowed);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    // an older reader's keys: the default's, as before
    assert_eq!(document.get("metal").and_then(|m| m.get("target")).and_then(Value::as_str), Some("iPhone18,2 24A446"));
    assert_eq!(document.get("metal").and_then(|m| m.get("bundle")).and_then(Value::as_str), Some(METAL_BUNDLE));
    assert_eq!(document.get("camera").and_then(|c| c.get("path")).and_then(Value::as_str), Some(CAMERA_DIR));
    // the lists: the default first, then lib/guest/24B156/
    let metals = document.get("metals").and_then(Value::as_list).unwrap();
    let t: Vec<(&str, &str)> = metals.iter().map(|m| (m.get("target").and_then(Value::as_str).unwrap(), m.get("bundle").and_then(Value::as_str).unwrap())).collect();
    assert_eq!(t, [("iPhone18,2 24A446", METAL_BUNDLE), ("iPhone18,2 24B156", "lib/guest/24B156/NativeAdrenoMetal.bundle")]);
    assert_eq!(metals[1].get("record").and_then(Value::as_str), Some("lib/guest/24B156/NativeAdrenoMetal.build.json"));
    let cameras = document.get("cameras").and_then(Value::as_list).unwrap();
    let c: Vec<(&str, &str)> = cameras.iter().map(|c| (c.get("target").and_then(Value::as_str).unwrap(), c.get("path").and_then(Value::as_str).unwrap())).collect();
    assert_eq!(c, [("iPhone18,2 24A446", CAMERA_DIR), ("iPhone18,2 24B156", "lib/guest/24B156/camera")]);
    let _ = std::fs::remove_dir_all(&root);

    // refused: a folder that is not its bundle's build; two bundles for one target; another target without the default
    let (mut wrong, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    wrong.extend(more.iter().map(|(p, b)| (p.replacen("lib/guest/24B156/", "lib/guest/24B157/", 1), b.clone())));
    assert_eq!(refusal_of("two-folder", &wrong), "lib/guest/24B157/NativeAdrenoMetal.bundle: oneios-target says 'iPhone18,2 24B156', its folder names build 24B157");
    let (mut twice, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    twice.extend(guest_files("iPhone18,2 24A446", "iPhone18,2 24A446").0.into_iter().filter(|(p, _)| p.starts_with("lib/guest/")).map(|(p, b)| (p.replacen("lib/guest/", "lib/guest/24A446/", 1), b)));
    assert_eq!(refusal_of("two-same", &twice), "lib/guest/24A446/NativeAdrenoMetal.bundle: a second Metal bundle for 'iPhone18,2 24A446'");
    let mut alone: Vec<(String, Vec<u8>)> = vec![("bin/xnuhost".to_string(), b"#!/bin/sh\n".to_vec())];
    alone.extend(more.clone());
    assert_eq!(refusal_of("two-alone", &alone), format!("lib/guest/24B156/: another target's plug-ins without the default's ({METAL_BUNDLE}), which runtime.json \"metal\" names"));
}

#[test]
fn one_target_lists_itself() {
    let (files, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let root = stage("one-list", &borrowed);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    assert_eq!(document.get("metals").and_then(Value::as_list).map(|l| l.len()), Some(1));
    assert_eq!(document.get("metals").and_then(Value::as_list).unwrap()[0], *document.get("metal").unwrap());
    assert_eq!(document.get("cameras").and_then(Value::as_list).map(|l| l.len()), Some(1));
    let _ = std::fs::remove_dir_all(&root);
}

/// iOS 27.2.1's kernel beside 27.0.1's (kernel/24B156/xnu.macho): "kernels" lists both with their builds, "kernel"
/// stays the default's; a kernel in a folder of another build than its record's, a second one of one build, or one
/// without the default kernel is refused.
#[test]
fn another_builds_kernel_is_listed_in_kernels() {
    let kernel = macho(LE64, 0x0100_000c, 2);
    let with = |profile: &str| record(&kernel, |t| t.replace("\"xnu\": \"xnu-0\"", &format!("\"xnu\": \"xnu-0\", \"profile\": \"{profile}\"")));
    let (a, b) = (with("ios-27.0.1-24A446"), with("ios-27.2.1-24B156"));
    let files: Vec<(&str, &[u8])> = vec![("bin/xnuhost", b"#!/bin/sh\n"), ("kernel/xnu.macho", &kernel), ("kernel/xnu.macho.build.json", &a),
                                         ("kernel/24B156/xnu.macho", &kernel), ("kernel/24B156/xnu.macho.build.json", &b)];
    let root = stage("kernels", &files);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let at = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    assert_eq!(at(document.get("kernel").unwrap(), "path"), "kernel/xnu.macho");
    let kernels = document.get("kernels").and_then(Value::as_list).unwrap();
    let listed: Vec<(String, String, String)> = kernels.iter().map(|k| (at(k, "build"), at(k, "path"), at(k, "record"))).collect();
    assert_eq!(listed, [("24A446".into(), "kernel/xnu.macho".into(), "kernel/xnu.macho.build.json".into()),
                        ("24B156".into(), "kernel/24B156/xnu.macho".into(), "kernel/24B156/xnu.macho.build.json".into())]);
    assert_eq!(refusal("kernels-folder", &[("kernel/xnu.macho", &kernel), ("kernel/xnu.macho.build.json", &a), ("kernel/24B157/xnu.macho", &kernel), ("kernel/24B157/xnu.macho.build.json", &b)]),
               "kernel/24B157/xnu.macho: its record's profile is 'ios-27.2.1-24B156', its folder names build 24B157");
    assert_eq!(refusal("kernels-alone", &[("kernel/24B156/xnu.macho", &kernel), ("kernel/24B156/xnu.macho.build.json", &b)]),
               "kernel/24B156/xnu.macho: another build's kernel without the default one (kernel/xnu.macho), which runtime.json \"kernel\" names");
    assert_eq!(refusal("kernels-same", &[("kernel/xnu.macho", &kernel), ("kernel/xnu.macho.build.json", &b), ("kernel/24B156/xnu.macho", &kernel), ("kernel/24B156/xnu.macho.build.json", &b)]),
               "kernel/: two kernels for ios-27.2.1-24B156");
}

/// The Galaxy A56's GPU plug-ins (lib/guest/OneiOSRadeonMetal.bundle and OneiOSRadeonGL.bundle, each with its record)
/// beside the Adreno one: taken, and named in runtime.json "metal_a56" with every CDHash; "metal" stays the Adreno's.
fn a56_files(metal_target: &str, gl_target: &str, gl_record_hash: Option<&str>) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    use oneios_testkit::fixture::signed_macho;
    let (plugin, plugin_hash) = signed_macho("OneiOSRadeonMetal", MH_BUNDLE, true, b"radeon metal");
    let (air, air_hash) = signed_macho("libOneiOSRadeonAir.dylib", MH_DYLIB, true, b"radeon air");
    let (gl, gl_hash) = signed_macho("OneiOSRadeonGL", MH_BUNDLE, true, b"radeon gl");
    let metal_record = format!(
        "{{\"bundle\": \"OneiOSRadeonMetal.bundle\", \"target\": \"{metal_target}\", \"files\": {{\"OneiOSRadeonMetal\": {{\"cdhash\": \"{plugin_hash}\"}}, \"libOneiOSRadeonAir.dylib\": {{\"cdhash\": \"{air_hash}\"}}}}}}"
    );
    let gl_record = format!(
        "{{\"bundle\": \"OneiOSRadeonGL.bundle\", \"target\": \"{gl_target}\", \"files\": {{\"OneiOSRadeonGL\": {{\"cdhash\": \"{}\"}}}}}}",
        gl_record_hash.unwrap_or(&gl_hash)
    );
    let mut files = vec![
        ("bin/xnuhost".to_string(), b"#!/bin/sh\n".to_vec()),
        (format!("{A56_METAL_BUNDLE}/Info.plist"), b"<plist/>".to_vec()),
        (format!("{A56_METAL_BUNDLE}/OneiOSRadeonMetal"), plugin),
        (format!("{A56_METAL_BUNDLE}/libOneiOSRadeonAir.dylib"), air),
        (format!("{A56_METAL_BUNDLE}/oneios-target"), format!("{metal_target}\n").into_bytes()),
        (A56_METAL_RECORD.to_string(), metal_record.into_bytes()),
        (format!("{A56_GL_BUNDLE}/Info.plist"), b"<plist/>".to_vec()),
        (format!("{A56_GL_BUNDLE}/OneiOSRadeonGL"), gl),
        (format!("{A56_GL_BUNDLE}/oneios-target"), format!("{gl_target}\n").into_bytes()),
        (A56_GL_RECORD.to_string(), gl_record.into_bytes()),
    ];
    let (adreno, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    files.extend(adreno.into_iter().filter(|(p, _)| p != "bin/xnuhost"));
    let mut hashes = vec![plugin_hash, air_hash, gl_hash];
    hashes.sort();
    (files, hashes)
}

#[test]
fn the_a56_gpu_plugins_are_taken_beside_the_adreno_one() {
    let (files, hashes) = a56_files("iPhone10,3 20F66", "iPhone10,3 20F66", None);
    assert_eq!(refusal_of("a56-ok", &files), "");
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let root = stage("a56-manifest", &borrowed);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    let document = json::load(root.join(MANIFEST)).unwrap();
    assert_eq!(document.get("metal").and_then(|m| m.get("target")).and_then(Value::as_str), Some("iPhone18,2 24A446"));
    let a56 = document.get("metal_a56").unwrap();
    assert_eq!(a56.get("target").and_then(Value::as_str), Some("iPhone10,3 20F66"));
    assert_eq!(a56.get("metal").and_then(|m| m.get("bundle")).and_then(Value::as_str), Some(A56_METAL_BUNDLE));
    assert_eq!(a56.get("gl").and_then(|m| m.get("record")).and_then(Value::as_str), Some(A56_GL_RECORD));
    let listed: Vec<String> = a56.get("cdhashes").and_then(Value::as_list).unwrap().iter().map(str_value).collect();
    assert_eq!(listed, hashes);
    let _ = std::fs::remove_dir_all(&root);
    // without the A56's bundles: "metal_a56" is null (every package before 2026-11-25)
    let (adreno, _) = guest_files("iPhone18,2 24A446", "iPhone18,2 24A446");
    let borrowed: Vec<(&str, &[u8])> = adreno.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let root = stage("a56-none", &borrowed);
    std::fs::set_permissions(root.join("bin/xnuhost"), std::fs::Permissions::from_mode(0o755)).unwrap();
    manifest(&root, "0.1.0", "00", "0", "0.4.80", "0").unwrap();
    assert!(json::load(root.join(MANIFEST)).unwrap().get("metal_a56").is_some_and(Value::is_null));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a56_gpu_plugins_that_do_not_agree_are_refused() {
    // the OpenGL ES bundle for another image than the Metal plug-in
    let (files, _) = a56_files("iPhone10,3 20F66", "iPhone18,2 24A446", None);
    assert_eq!(refusal_of("a56-targets", &files), format!("{A56_GL_BUNDLE}: for 'iPhone18,2 24A446', the Metal plug-in for 'iPhone10,3 20F66'"));
    // a Mach-O whose CDHash is not its record's
    let (files, _) = a56_files("iPhone10,3 20F66", "iPhone10,3 20F66", Some("0000000000000000000000000000000000000000"));
    assert!(refusal_of("a56-cdhash", &files).starts_with(&format!("{A56_GL_BUNDLE}/OneiOSRadeonGL: CDHash ")));
    // the OpenGL ES bundle alone
    let (files, _) = a56_files("iPhone10,3 20F66", "iPhone10,3 20F66", None);
    let gl_only: Vec<(String, Vec<u8>)> = files.into_iter().filter(|(p, _)| !p.starts_with(A56_METAL_BUNDLE) && p != A56_METAL_RECORD).collect();
    assert_eq!(refusal_of("a56-gl-alone", &gl_only), format!("{A56_GL_BUNDLE}: the A56's OpenGL ES without its Metal plug-in ({A56_METAL_BUNDLE})"));
}
