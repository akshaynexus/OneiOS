//! pack.rs's tests: every refusal rule with its message, what is accepted, and the order two refusals come in.
//! tests/packagepy/compare-golden.sh has the same rules against pack.py's frozen output.

use super::*;
use std::path::PathBuf;

/// A stage with these files, in a scratch folder of its own.
fn stage(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("oneios-package-test.{}.{name}", std::process::id()));
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
    let document = pyjson::load(root.join(MANIFEST)).unwrap();
    let files = document.get("files").and_then(Value::as_list).unwrap();
    let listed: Vec<(&str, &str)> = files.iter().map(|f| (f.get("path").and_then(Value::as_str).unwrap(), f.get("mode").and_then(Value::as_str).unwrap())).collect();
    assert_eq!(listed, [("NOTICE", "0644"), ("bin/xnuhost", "0755")]);
    assert_eq!(document.get("git").and_then(|g| g.get("dirty")), Some(&Value::Bool(true)));
    assert!(document.get("kernel").is_some_and(Value::is_null));
    let _ = std::fs::remove_dir_all(&root);
}
