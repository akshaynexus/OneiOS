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

use oneios_kernel::{build_oneios_aes, build_oneios_audio, build_oneios_codec, build_oneios_display, build_oneios_gpu, build_oneios_hid,
                    build_oneios_keystore, build_oneios_net, build_record, drivers_build};
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
    assert_eq!(build_oneios_gpu::staged_3d_text(made_up("gpu3d")).unwrap(), expected("gpu3d"));   // no Python original
    assert!(build_oneios_gpu::staged_3d_text(made_up("gpu3d").repeat(2)).is_err());
    assert_eq!(build_oneios_gpu::staged_accelerator_text(made_up("accelerator")).unwrap(), expected("accelerator"));   // no Python original
    assert!(build_oneios_gpu::staged_accelerator_text(made_up("accelerator").repeat(2)).is_err());
    assert_eq!(build_oneios_display::staged_text(made_up("display")).unwrap(), expected("display"));
    assert_eq!(build_oneios_display::staged_scanout(made_up("display_scanout")).unwrap(), expected("display_scanout"));
}

#[test]
fn the_codec_edit_writes_its_frozen_text_and_fits_the_pinned_source() {
    // hevc-guest, 2026-11-15 (no Python original): the edit list of `kernel gen codec` (staged/codec.rs) on a made-up
    // source against its frozen output, and on the real DroidVMVTHost.cpp where external/ has it.
    assert_eq!(build_oneios_codec::staged_text(golden("synthetic/codec.in")).unwrap(), golden("synthetic/codec.out"));
    assert!(build_oneios_codec::staged_text(golden("synthetic/codec.out")).is_err());   // applied twice: the anchor is gone
    // What the archive must define: the start the bridge calls, the transport, the registration and its initializer.
    assert_eq!(build_oneios_codec::REQUIRED, ["_oneios_codec_start", "_droidvm_media_start", "_droidvm_vt_paravirt_attach",
                                              "__GLOBAL__sub_I_DroidVMVTParavirt.cpp"]);
    let source = repo().join("external/UniXNU-droidvm/package/DroidVMVTHost.cpp");
    if !source.exists() {
        eprintln!("external/UniXNU-droidvm is not here: the codec's edit was checked on the synthetic source only");
        return;
    }
    let staged = build_oneios_codec::staged_text(std::fs::read_to_string(&source).unwrap()).unwrap();
    // The hvcC's twelve profile, tier and level bytes: no plain copy of the stream's bytes is left.
    assert!(!staged.contains("memcpy(out + at, s->sps + 3, 12)"));
    assert!(staged.contains("if (zeros >= 2 && byte == 3) { zeros = 0; continue; }"));
    assert!(staged.contains("if (taken < 12) return 0;"));
    // iOS 27's tile session (2026-11-22): every type of it has a case, ecti is answered as ecfr, the tile messages
    // replace the frame's, and jcap has an answer of its own; a small HEVC session is coded larger and cropped back.
    for case in ["case cc(\"ests\"):", "case cc(\"epti\"):", "case cc(\"etil\"):", "case cc(\"ecfr\"): case cc(\"ecti\"):",
                 "case cc(\"jcap\"):"] {
        assert_eq!(staged.matches(case).count(), 1, "{case}");
    }
    for text in ["#include \"OneiOSCodecPicture.h\"", "cc(\"TEsa\")", "s->tiles ? cc(\"TEcd\") : cc(\"Efds\")", "s->tiles ? cc(\"TEem\") : cc(\"Eemt\")",
                 "s->tiles ? cc(\",til\") : cc(\",frm\")", "h.type = s->finishType ? s->finishType : cc(\"ecfr\");",
                 "bool cropSps(Session *s)", "oneios_hevc_sps_crop(s->spsIn, s->spsInSize, s->width, s->height, s->sps, sizeof s->sps)",
                 "oneios_yuv_copy_area(to, toBytes, w, h, surface, surfaceSampleBytes(view.format), planes,",
                 "if (crop && !cropSps(s)) failed(102, int32_t(n));", "f.width = s->codedWidth;", "constexpr uint32_t MinCodedSize = 96;",
                 // 10-bit pictures (2026-11-23): x420 and xf20 taken, P010 to the device (NV12 when it refuses), the tile
                 // session's device opened at its first tile, a tile session's SPS always cropped, the pictures counted
                 "if (format == cc(\"x420\") || format == cc(\"xf20\")) return 2;", "f.pixelformat = bytes == 2 ? cc(\"010P\") : cc(\"21VN\");",
                 "if (const int error = openForPicture(s, surfaces[0], view)) return error;",
                 "(s->tiles || s->codedWidth != s->width || s->codedHeight != s->height)",
                 "oneios_codec_pictures[path] = oneios_codec_pictures[path] + 1;"] {
        assert!(staged.contains(text), "{text}");
    }
    // copyArea before feedFrame, cropSps before handleCoded: each defined before its caller
    let at = |text: &str| staged.find(text).unwrap_or_else(|| panic!("{text}"));
    assert!(at("bool copyArea(") < at("bool feedFrame(Session *s)"));
    assert!(at("bool cropSps(Session *s)") < at("void handleCoded("));
    assert!(at("bool encodeSurface(") < at("int queueEncodeFrame("));
    assert!(at("uint32_t surfaceSampleBytes(") < at("bool feedFrame(Session *s)"));
    assert!(at("int openForPicture(") < at("int queueEncodeTile(") && at("int openForPicture(") < at("int queueEncodeFrame("));
    assert!(!staged.contains("view.format != cc(\"420v\") && view.format != cc(\"420f\")"));   // the 8-bit-only check is gone
    // The decoder (2026-11-24, edit 6): dcap and dcan answered once each, VP9 and AV1 listed and started, the surface's
    // kernel mapping kept, the counters; each helper before its first caller.
    for case in ["case cc(\"dcap\"):", "case cc(\"dcan\"): {"] {
        assert_eq!(staged.matches(case).count(), 1, "{case}");
    }
    for text in ["{\"vp09\", \"com.oneios.videodecoder.virtio-media.vp9\"", "{\"av01\", \"com.oneios.videodecoder.virtio-media.av1\"",
                 "const int list = p->array(entries, count);", "f.pixelformat = decoderFourcc(d.codec);",
                 "if (find(d.extensions, d.extensionsSize, cc(\"vpcC\"), record)) {", "w.plist(cc(\",cda\"), *p, capabilities);",
                 "s->mappedSurfaces[s->mappedCount++] = {to.id, to.base, memory, map, surface};", "else refreshCapture(s);",
                 "countDecoded(s, frame, bytesused != 0);", "volatile uint32_t oneios_decode_frames[4], oneios_decode_dropped[4];"] {
        assert!(staged.contains(text), "{text}");
    }
    assert!(!staged.contains("decoderDevice() == ~0u ? 0 : 2"));
    assert!(at("uint32_t decoderCodecs()") < at("void emitFrame(") && at("int codecIndex(") < at("void copyFrame("));
    assert!(at("void countDecoded(") < at("void emitFrame(") && at("void refreshCapture(") < at("void workerMain("));
    assert!(at("bool streamHeaders(") < at("int startSession(") && at("int32_t acceptFormat(") < at("int droidvm_vthost_message("));
}

#[test]
fn the_transport_edit_fits_the_pinned_source() {
    // fix/codec-irq, 2026-11-25 (edit 7, staged/codec_transport.rs): DroidVMVirtioMedia.c's event queue on the device's
    // interrupt. On the real file where external/ has it; applied twice it fails (each anchor is gone).
    let source = repo().join("external/UniXNU-droidvm/package/DroidVMVirtioMedia.c");
    if !source.exists() {
        eprintln!("external/UniXNU-droidvm is not here: the transport's edit was not checked");
        return;
    }
    let staged = build_oneios_codec::staged_transport(std::fs::read_to_string(&source).unwrap()).unwrap();
    assert!(build_oneios_codec::staged_transport(staged.clone()).is_err());
    for text in ["#include \"OneiOSCodecWait.h\"", "static void media_interrupt(void *context) {",
                 "static bool reap_events(struct media_device *d) {", "if (sink) sink(context, copy, sizeof copy);",
                 "attach = armcpu_spi_attach(d->interrupt_id, false, media_interrupt, d, NULL, NULL);",
                 "if (d->irq) *(volatile uint16_t *)(media_rings[device][MEDIA_EVENTQ] + 1024) = 0;",
                 "oneios_media_wait_ms(polled, fast), NSEC_PER_MSEC);",
                 "oneios_media_count_wake(oneios_media_counts, woke == THREAD_TIMED_OUT, any);",
                 "PE_parse_boot_argn(\"oneios-codec-irq\", &media_irq_enabled, sizeof(media_irq_enabled));",
                 "SecureDTGetProperty(node, \"droidvm,intid\", &value, &size) == kSuccess ||"] {
        assert_eq!(staged.matches(text).count(), 1, "{text}");
    }
    // the pin's poll is gone: no IOSleep in the event worker, no reap inside a command's wait (it holds the lock)
    assert!(!staged.contains("IOSleep(fast ? 1 : 50);") && !staged.contains("        reap_events(d);\n        IOSleep(1);"));
    // both queues start NO_INTERRUPT; only the event queue's flag is cleared, once the SPI is attached
    let at = |text: &str| staged.find(text).unwrap_or_else(|| panic!("{text}"));
    assert!(at("= 1;   // VIRTQ_AVAIL_F_NO_INTERRUPT: we poll") < at("if (d->irq) *(volatile uint16_t *)"));
    assert!(at("static bool media_pending;") < at("static void media_interrupt(") && at("static void media_interrupt(") < at("static const char *bring_up("));
}

#[test]
fn the_display_edits_fit_the_pinned_sources() {
    // The edit lists against the real upstream files, where external/ has them (a checkout without the references
    // skips this; the kernel build applies the same lists and stops on a missing anchor).
    let package = repo().join("external/UniXNU-droidvm/package");
    let (probe, scanout) = (package.join("LegacyDisplayProbe.cpp"), package.join("DroidVMVirtioGpuDisplay.cpp"));
    if !probe.exists() || !scanout.exists() {
        eprintln!("external/UniXNU-droidvm is not here: the display's edits were checked on the synthetic sources only");
        return;
    }
    let staged = build_oneios_display::staged_text(std::fs::read_to_string(&probe).unwrap()).unwrap();
    assert!(staged.contains("oneios_virtio_gpu_offer_format(display,memory,stride,width,height,format);"));
    let staged = build_oneios_display::staged_scanout(std::fs::read_to_string(&scanout).unwrap()).unwrap();
    assert_eq!(staged.matches("scanoutFormat(r)").count(), 3);                  // CREATE_2D and the two SET_SCANOUT_BLOBs
    assert!(!staged.contains("r.width, r.height, 2, 0,") && staged.contains("format == 2 ? OneiOSHalf : 0};"));
    // fix/s24-fps-heat-camera: the slowest refresh rate reported, in NativeMobileDisplay's display (where external/ has it).
    let legacy = repo().join("external/NativeMobileDisplay/NativeLegacyDisplay.cpp");
    if legacy.exists() {
        let staged = build_oneios_display::staged_legacy(std::fs::read_to_string(&legacy).unwrap()).unwrap();
        assert_eq!(staged.matches("sheng-vsync-min").count(), 1);
        assert!(staged.contains("words[2]=longest;words[3]=longestTime;"));
    }
}

#[test]
fn the_pen_edits_fit_the_pinned_source() {
    // spen, 2026-11-15: the pen digitizer's edits (staged/hid_pen.rs) apply after the phone's (staged/hid.rs) to the real
    // NativeHidInput.cpp, where external/ has it; the kernel build applies the same lists and stops on a missing anchor.
    let source = repo().join("external/NativeHidInput/NativeHidInput.cpp");
    if !source.exists() {
        eprintln!("external/NativeHidInput is not here: the pen's edits were not checked");
        return;
    }
    let staged = build_oneios_hid::staged_hid_pen(build_oneios_hid::staged_hid(std::fs::read_to_string(&source).unwrap()).unwrap()).unwrap();
    // Report 4 in a Digitizer / Stylus application collection; the sink set and cleared only for the pen device.
    assert!(staged.contains("static const unsigned char penDescriptor[]={\n    0x05,0x0d,0x09,0x20,0xa1,0x01, 0x85,0x04,"));
    assert_eq!(staged.matches("native_pen_set_sink(").count(), 3);               // the declaration, start, stop
    assert!(staged.contains("if(penOnly)native_pen_set_sink(&NativeHidInput::penNotify,this);"));
    // backboardd's pencil digitizer: SPI, Built-In, the hover range and the maximum pressure on the device.
    assert!(staged.contains("touchOnly||penOnly?\"SPI\":\"Virtual\""));
    assert!(staged.contains("setProperty(\"MaxHoverHeight\",1ULL,32);setProperty(\"AccurateMaxDigitizerPressureValue\",500ULL,32);"));
    // The stylus event is completed by the wrapper (setFixedValue, slot 26) and is not marked as a finger.
    assert!(staged.contains("enum : uint16_t {kDivSetFixed=0x6e23};"));
    assert!(staged.contains("==kStylus) {orient(child);return false;}"));
    assert!(staged.contains("(touchEnabled || touchOnly || penOnly)){wrappedDriver=driver;edge::install(driver);}"));
    // The report: 11 bytes, bit 0 Tip, 1 Barrel, 2 In Range, 3 Eraser from the sink's flags (1 range, 2 tip, 4 barrel, 8 eraser).
    assert!(staged.contains("unsigned char report[11]={4,bits,"));
    assert!(staged.contains("((flags&2)?1:0)|((flags&4)?2:0)|((flags&1)?4:0)|((flags&8)?8:0)"));
    assert!(staged.ends_with("if(device)device->setPenOnly();return device;}\n"));
    // Applied twice, the anchors are gone: the generator stops.
    assert!(build_oneios_hid::staged_hid_pen(staged).is_err());
}

#[test]
fn the_motion_edits_fit_the_pinned_sources() {
    // sensors-als, 2026-11-25: the motion sensors' edits (staged/hid_motion.rs) on the real DroidVMVirtioInput.c (after the
    // phone's own edits), DroidVMMotionHid.inc and NativeHidInput.cpp (after hid.rs and hid_pen.rs), where external/ has them.
    let package = repo().join("external/UniXNU-droidvm/package");
    let native = repo().join("external/NativeHidInput/NativeHidInput.cpp");
    if !package.join("DroidVMMotionHid.inc").exists() || !native.exists() {
        eprintln!("external/UniXNU-droidvm or external/NativeHidInput is not here: the motion edits were not checked");
        return;
    }
    let read = |path: PathBuf| std::fs::read_to_string(path).unwrap();
    // The transport: the magnetometer's axes kept, published inside the frame's seqlock, each frame counted and the devices woken.
    let transport = build_oneios_hid::staged_motion_transport(
        build_oneios_hid::staged_transport(read(package.join("DroidVMVirtioInput.c")), "", "").unwrap()).unwrap();
    assert!(transport.contains("e->code >= 0x10 && e->code <= 0x12) {\n        oneios_motion_mag_raw[e->code - 0x10] = e->value;"));
    let publish = transport.find("static void motion_publish(").unwrap();
    let body = &transport[publish..publish + transport[publish..].find("\n}\n").unwrap()];
    let (copied, closed, kicked) = (body.find("oneios_motion_mag[i] = oneios_motion_mag_raw[i]").unwrap(),
                                    body.rfind("droidvm_motion.seq = droidvm_motion.seq + 1;").unwrap(), body.find("oneios_motion_kick();").unwrap());
    assert!(copied < closed && closed < kicked, "the magnetometer is published inside the seqlock, the devices woken after it");
    assert!(build_oneios_hid::staged_motion_transport(transport).is_err());   // applied twice: the anchors are gone
    // The motion devices: a unit of its own, the compass, ticks driven by the frames (a 1 s guard), the reports counted.
    let motion = build_oneios_hid::staged_motion_hid(read(package.join("DroidVMMotionHid.inc"))).unwrap();
    assert!(motion.starts_with("// SPDX-License-Identifier: BSD-2-Clause-Patent\n") && motion.contains("#include <IOKit/hid/IOHIDDevice.h>"));
    assert!(motion.contains("DROIDVM_MOTION_ORIENTATION, DROIDVM_MOTION_COMPASS };"));
    assert!(motion.contains("static const unsigned char compass[] = DROIDVM_MOTION_VECTOR(0x0a, 0x83, 0x85);"));
    assert!(motion.contains("? (int64_t)raw[i] * 1000 / 16"));
    assert!(motion.contains("        source->setTimeoutMS(1000);\n") && !motion.contains("        source->setTimeoutMS(10);\n"));
    assert!(motion.contains("if (kind < 4) oneios_motion_timers[kind] = timer;"));
    assert!(motion.contains("oneios_motion_reports[self->kind] = oneios_motion_reports[self->kind] + 1;"));
    assert!(motion.trim_end().ends_with("if (timer) timer->setTimeoutUS(1);\n    }\n}"));
    assert!(build_oneios_hid::staged_motion_hid(motion).is_err());
    // NativeHidInput's slot table exported for the light sensor's wrapper, inside its pointer-authentication branch.
    let hid = build_oneios_hid::staged_hid_slots(
        build_oneios_hid::staged_hid_pen(build_oneios_hid::staged_hid(read(native)).unwrap()).unwrap()).unwrap();
    let export = hid.find("extern \"C\" const uint16_t *oneios_hid_driver_slots(unsigned *count)").unwrap();
    assert!(hid[..export].rfind("#if __has_feature(ptrauth_calls)").unwrap() > hid[..export].rfind("#else").unwrap_or(0));
    assert_eq!(hid.matches("oneios_hid_driver_slots(unsigned").count(), 1);
    // What the archive must define for the new parts.
    for name in ["_oneios_motion_kick", "__GLOBAL__sub_I_DroidVMMotionHid.cpp", "__GLOBAL__sub_I_DroidVMSensorProcessor.cpp",
                 "_droidvm_motion_register", "_oneios_hid_driver_slots", "_oneios_als_spu_wrap", "_oneios_als_spu_wrapped"] {
        assert!(build_oneios_hid::REQUIRED.contains(&name), "{name}");
    }
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
fn the_build_record_is_the_frozen_one() {
    use oneios_common::json::{self, Value};
    let root = std::env::temp_dir().join(format!("oneios-kernel-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let work = made_up_build(&root);
    let out = root.join("out/kernel");
    build_record::finish(&root, "linux", &work, "oneios_test_defconfig", build_record::Place::Out(&out)).unwrap();
    let target = out.join("current").canonicalize().unwrap();
    // the machine's part (the compiler's and the linker's versions) and the checkouts' commits are not frozen
    let record = json::load(target.join("xnu.macho.build.json")).unwrap();
    let Value::Dict(items) = record else { panic!("the record is no object") };
    let kept = Value::Dict(items.into_iter().filter(|(key, _)| key != "host" && key != "sources").collect());
    assert_eq!(json::dumps(&kept, Some(2), false) + "\n", golden("synthetic/build-record.json"));
    assert_eq!(std::fs::read_to_string(target.join("series.txt")).unwrap(), golden("synthetic/build-record.series.txt"));
    assert_eq!(std::fs::read(target.join("xnu.macho")).unwrap(), build_record::fake_kernel());
    let _ = std::fs::remove_dir_all(&root);
}

/// The hosted kernel (kernel/build-hosted.sh): the record of the final file, next to it; the work tree's source
/// corecrypto archive defines symbols the kernel does not hold (ONEIOS_CRYPTO=original), so it is releasable.
#[test]
fn the_hosted_record_is_of_the_final_file() {
    use oneios_common::json::{self, Value};
    use sha2::Digest;
    let root = std::env::temp_dir().join(format!("oneios-kernel-hosted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let work = made_up_build(&root);
    let archive = work.join("tree/build/corecrypto-2023/libcorecrypto_kernel_noasm-namespaced.a");
    std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
    std::fs::write(&archive, build_record::fake_archive(&["_cc_clear"])).unwrap();
    // the shipped file is not the link's output: another kernel (here, one with a symbol table)
    let folder = root.join("out/kernel/current-hosted-t");
    std::fs::create_dir_all(&folder).unwrap();
    let shipped = folder.join("xnu.macho");
    std::fs::write(&shipped, build_record::fake_kernel_with(&["_kernel_bootstrap"])).unwrap();
    let steps = ["hosted-fixup".to_string()];
    build_record::finish(&root, "linux", &work, "oneios_hosted_defconfig", build_record::Place::Hosted { kernel: &shipped, post_link: &steps }).unwrap();
    let record = json::load(folder.join("xnu.macho.build.json")).unwrap();
    let bytes = std::fs::read(&shipped).unwrap();
    assert_eq!(record.get("bytes").and_then(Value::as_int), Some(bytes.len() as i128));
    assert_eq!(record.get("sha256").and_then(Value::as_str), Some(oneios_common::hex(&sha2::Sha256::digest(&bytes)).as_str()));
    assert_ne!(bytes, build_record::fake_kernel());
    assert_eq!(record.get("releasable"), Some(&Value::Bool(true)));
    assert_eq!(record.get("defconfig").and_then(Value::as_str), Some("oneios_hosted_defconfig"));
    assert_eq!(record.get("profile").and_then(Value::as_str), Some("ios-test"));
    assert_eq!(std::fs::read_to_string(folder.join("series.txt")).unwrap(), golden("synthetic/build-record.series.txt"));
    // nothing below out/kernel/builds, no current link: the VM kernel's place is not touched
    assert!(!root.join("out/kernel/builds").exists() && !root.join("out/kernel/current").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_gpu_edits_fit_the_pinned_sources() {
    // a56-gpu, 2026-11-24: the three edit lists of `kernel gen gpu` (staged/gpu.rs) on the real DroidVMGpu.cpp,
    // DroidVMVirtioGpu3D.cpp and NativeAdrenoAccelerator.cpp where external/ has them: each anchor there exactly once, and
    // the Adreno accelerator's names now asked of OneiOSAdreno.cpp, nowhere written as literals.
    let package = repo().join("external/UniXNU-droidvm/package");
    let accelerator = repo().join("external/NativeAdreno/NativeAdrenoAccelerator.cpp");
    if !package.join("DroidVMGpu.cpp").exists() || !accelerator.exists() {
        eprintln!("external/UniXNU-droidvm or external/NativeAdreno is not here: the gpu edits were checked on the synthetic sources only");
        return;
    }
    let gpu = build_oneios_gpu::staged_text(std::fs::read_to_string(package.join("DroidVMGpu.cpp")).unwrap()).unwrap();
    assert!(gpu.contains("accelerator->setProperty(\"MetalPluginName\", oneios_gpu_metal_plugin());"));
    assert!(!gpu.contains("\"NativeAdrenoMetal\""));
    // the GL stage (2026-11-24): OpenGL ES's bundle asked of OneiOSAdreno.cpp too (OneiOSRadeonGL with oneios-gpu-amd-gl=1)
    assert!(gpu.contains("accelerator->setProperty(\"IOGLESBundleName\", oneios_gpu_gles_bundle());"));
    assert!(!gpu.contains("\"AppleMetalGLRenderer\""));
    let gpu3d = build_oneios_gpu::staged_3d_text(std::fs::read_to_string(package.join("DroidVMVirtioGpu3D.cpp")).unwrap()).unwrap();
    assert_eq!(gpu3d.matches("oneios_gpu_context_made(context, in.capset_id)").count(), 1);
    let staged = build_oneios_gpu::staged_accelerator_text(std::fs::read_to_string(&accelerator).unwrap()).unwrap();
    // its declaration, the three copyProperty and serializeProperties' loop (one rule for both reads since the GL stage)
    assert_eq!(staged.matches("oneios_gpu_discovery_hidden(").count(), 5);
    assert!(staged.contains("if(oneios_gpu_open_refused(type))return kIOReturnUnsupported;"));
}
