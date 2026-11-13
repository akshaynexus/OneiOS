//! The generators against the frozen output of the Python scripts they replaced (tests/kernelgen/golden), for
//! everything that needs no kernel build tree:
//!
//!   - the bindings and the header that come from a record in the repository alone (aes, gpu, the display's header),
//!     against the files of the Python build (golden/build);
//!   - every edit list on a made-up source (golden/synthetic/NAME.in), against what the Python script's own staging
//!     function wrote for it (NAME.out);
//!   - drivers/build.py's compile command for a made-up record;
//!   - build-record.py's record of a made-up build.
//!
//! No file of another project and none of Apple's is read. What needs compiled objects (the vtable checks, hid's,
//! net's and the display's bindings) is compared in a build tree: tests/kernelgen/compare-golden.sh.

use oneios_kernel::{build_oneios_aes, build_oneios_audio, build_oneios_display, build_oneios_gpu, build_oneios_hid, build_oneios_keystore,
                    build_oneios_net, build_record, drivers_build};
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..").canonicalize().expect("the repository")
}

fn golden(name: &str) -> String {
    let path = repo().join("tools/oneios-tool/tests/kernelgen/golden").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn hosted(name: &str) -> PathBuf {
    repo().join("kernel/patches/oneios-hosted").join(name)
}

#[test]
fn the_aes_bindings_are_the_python_builds() {
    let (stubs, targets) = build_oneios_aes::bindings(&hosted("aes/IOAESAccelerator-24A446-v54.json")).unwrap();
    assert_eq!(stubs, golden("build/oneios-aes/IOAESAccelerator-bindings.S"));
    assert_eq!(targets, golden("build/oneios-aes/IOAESAccelerator-originals.S"));
}

#[test]
fn the_gpu_bindings_are_the_python_builds() {
    let (stubs, targets, bound) = build_oneios_gpu::bindings(&hosted("gpu/IOAcceleratorES-24A446-v54.json")).unwrap();
    assert_eq!(stubs, golden("build/oneios-gpu/adreno/accelerator/IOAcceleratorES-bindings.S"));
    assert_eq!(targets, golden("build/oneios-gpu/adreno/accelerator/IOAcceleratorES-originals.S"));
    assert_eq!(bound.len(), 3);
}

#[test]
fn the_displays_header_is_the_python_builds() {
    let abi = build_oneios_display::Abi::load(&hosted("display/legacy-display-24A446-v54.json")).unwrap();
    assert_eq!(build_oneios_display::header(&abi).unwrap(), golden("build/native-legacy-display/OriginalLegacyFramebufferABI.h"));
}

#[test]
fn the_discriminator_is_clangs() {
    // ptrauth_string_discriminator("isa"): the constant objc4 writes out (ISA_SIGNING_DISCRIMINATOR). The display's
    // header() checks every reviewed slot name of its record against the slot's diversity with the same function.
    assert_eq!(build_oneios_display::discriminator("isa"), 0x6ae1);
}

#[test]
fn the_probe_sources_are_the_python_builds() {
    assert_eq!(build_oneios_hid::PROBE, golden("build/oneios-hid/OneiOSHidLayoutCheck.cpp"));
    assert_eq!(build_oneios_net::PROBE, golden("build/oneios-net/OneiOSNetCheck.cpp"));
    assert_eq!(build_oneios_net::CRUMBS, golden("build/oneios-net/ionetworking-include/oneios-mbuf-crumbs.h"));
}

#[test]
fn the_edit_lists_write_what_the_python_functions_wrote() {
    let made_up = |name: &str| golden(&format!("synthetic/{name}.in"));
    let expected = |name: &str| golden(&format!("synthetic/{name}.out"));
    assert_eq!(build_oneios_aes::staged_text(made_up("aes"), "// made-up keys\n").unwrap(), expected("aes"));
    assert_eq!(build_oneios_keystore::staged_text(made_up("keystore")).unwrap(), expected("keystore"));
    assert_eq!(build_oneios_audio::staged_text(made_up("audio")).unwrap(), expected("audio"));
    assert_eq!(build_oneios_hid::staged_hid(made_up("hid")).unwrap(), expected("hid"));
    assert_eq!(build_oneios_hid::staged_transport(made_up("hid_transport"), "// made-up side keys\n", "// made-up touch time\n").unwrap(),
               expected("hid_transport"));
    assert_eq!(build_oneios_net::staged_text(made_up("net_transport")).unwrap(), expected("net_transport"));
    assert_eq!(build_oneios_gpu::staged_text(made_up("gpu")).unwrap(), expected("gpu"));
    assert_eq!(build_oneios_display::staged_text(made_up("display")).unwrap(), expected("display"));
}

#[test]
fn the_contact_identifier_is_a_free_one_of_1_to_28_and_the_pointer_gets_29() {
    // hid, 2026-11-12 (a6346c1): backboardd drops a touch whose identifier is 30 or more (MAX_PATH_COUNT)
    let staged = golden("synthetic/hid_transport.out");
    assert!(staged.contains("uint8_t id = 29;\n"));
    assert!(staged.contains("oneios_next_id = c >= 28 ? 1 : (uint8_t)(c + 1);\n"));
    assert!(!staged.contains("% 120"));
}

#[test]
fn an_anchor_that_is_gone_stops_the_generator() {
    let error = build_oneios_keystore::staged_text("namespace {\n".to_string()).unwrap_err().to_string();
    assert_eq!(error, "build-oneios-keystore: NativeKeyStoreEvents.cpp no longer has exactly one default: break;");
    let twice = golden("synthetic/net_transport.in").repeat(2);
    assert!(build_oneios_net::staged_text(twice).unwrap_err().to_string().starts_with("build-oneios-net: DroidVMVirtioNet.c no longer has exactly one 'if ("));
}

#[test]
fn the_compile_command_is_drivers_build_pys() {
    let record = drivers_build::record(&repo().join("tools/oneios-tool/tests/kernelgen/golden/synthetic"), "command.record.json").unwrap();
    for kind in ["c", "cpp"] {
        let command = drivers_build::command(Path::new("/REPO/drivers"), &record, Path::new(&format!("/s/b.{kind}")), Path::new("/o/b.o"));
        let list = format!("[{}]\n", command.iter().map(|part| format!("\"{part}\"")).collect::<Vec<_>>().join(", "));
        assert_eq!(list, golden(&format!("synthetic/command.{kind}.json")));
    }
}

/// The made-up build of the frozen record: a checkout with one patch and one UniXNU patch, a work directory with a
/// made-up kernel, its inputs, its configuration, UniXNU's record and two archives' listings.
fn made_up_build(root: &Path) -> PathBuf {
    let write = |path: PathBuf, data: &[u8]| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    };
    write(root.join("kernel/patches/unixnu/0001-made-up.patch"), b"a made-up UniXNU patch\n");
    write(root.join("kernel/patches/a.patch"), b"x");
    let work = root.join("out/kernel/unixnu/oneios/ios-test/oneios_test");
    write(work.join("kernel/kernel.macho"), &build_record::fake_kernel());
    write(work.join("kernel/kernel.json"), br#"{"kernelcache_kexts_added": false, "prelink_skipped": true}"#);
    let inputs = format!(r#"{{"commit": "{}", "patches": [{{"path": "{}", "sha256": "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"}}]}}"#,
                         "c".repeat(40), root.join("kernel/patches/a.patch").display());
    write(work.join("xnu-inputs.json"), inputs.as_bytes());
    write(work.join(".config"), b"CONFIG_XNU_ARMCPU=y\n# a comment\nCONFIG__XNU_VERSION=\"12377.121.6\"\nCONFIG_NAME=\"a b\"\nCONFIG_XNU_SOURCE_CORECRYPTO14=y\n");
    write(work.join("tree/build/oneios-board/objects.json"), br#"{"sources": [{"path": "external/x.c", "universe": "mach", "sha256": "ab"}]}"#);
    write(work.join("tree/build/oneios-drivers/objects.json"),
          br#"{"records": "r", "sources": [{"path": "drivers/a.c", "universe": "mach", "sha256": "cd"}]}"#);
    work
}

#[test]
fn the_build_record_is_build_record_pys() {
    use oneios_common::pyjson::{self, Value};
    let root = std::env::temp_dir().join(format!("oneios-kernel-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let work = made_up_build(&root);
    let out = root.join("out/kernel");
    build_record::finish(&root, "linux", &work, "oneios_test_defconfig", &out).unwrap();
    let target = out.join("current").canonicalize().unwrap();
    // the machine's part (the compiler's and the linker's versions) and the checkouts' commits are not frozen
    let record = pyjson::load(target.join("xnu.macho.build.json")).unwrap();
    let Value::Dict(items) = record else { panic!("the record is no object") };
    let kept = Value::Dict(items.into_iter().filter(|(key, _)| key != "host" && key != "sources").collect());
    assert_eq!(pyjson::dumps(&kept, Some(2), false) + "\n", golden("synthetic/build-record.json"));
    assert_eq!(std::fs::read_to_string(target.join("series.txt")).unwrap(), golden("synthetic/build-record.series.txt"));
    assert_eq!(std::fs::read(target.join("xnu.macho")).unwrap(), build_record::fake_kernel());
    let _ = std::fs::remove_dir_all(&root);
}
