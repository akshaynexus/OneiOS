// SPDX-License-Identifier: MIT
//! Loader unit tests on generated fixtures (xnuloader::fixture): no Apple files. The cases of tests/test_loader.c,
//! and the C Makefile's `xnuloader-inspect board 4 | xnuloader-inspect adt` check.

use xh_core::bytes::le;
use xnuloader::boot::{self, BootRequest};
use xnuloader::fixture::{self, BASE};
use xnuloader::machine::{self, Cluster, DeviceKind, Mac, Machine, CAMERA_FRONT, CAMERA_MODEL, CAMERA_REAR};
use xnuloader::machine::{
    BOARD_CPUFREQ, BOARD_GIC_DIST, BOARD_GIC_REDIST, BOARD_VIRTIO, BOARD_VIRTIO_SIZE, BOARD_VIRTIO_SPI,
};
use xnuloader::{macho, Kernel, Node};

fn u32_of(n: &Node, name: &str) -> Option<u32> {
    n.get(name)?.u32()
}

fn str_of<'a>(n: &'a Node, name: &str) -> Option<&'a str> {
    n.get(name)?.str()
}

fn words(value: &[u8]) -> Vec<u64> {
    value.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
}

#[test]
fn macho() {
    let mut im = fixture::kernel(BASE, 0, &[], true);
    let k = Kernel::parse(&im).unwrap();
    assert!(k.segments.len() == 2 && k.base == BASE && k.end == BASE + 0x5000 && k.entry == BASE + 0x1000);
    assert!(!k.fileset && k.cpusubtype == 0 && k.segments[0].name == "__TEXT" && k.segments[1].filesize == 0);
    assert_eq!(macho::symbol(&im, 0, "_fixture_symbol"), Some(BASE + 0x1008));
    assert_eq!(macho::symbol(&im, 0, "_absent"), None);
    assert_eq!(k.offset(BASE + 0x1000, 16), Some(0x1000));
    assert_eq!(k.offset(BASE + 0x4000, 16), None); // zero-fill has no file bytes

    // arm64e with capability bits in the subtype's top byte
    le::put_u32(&mut im, 8, 0x8000_0002);
    assert_eq!(Kernel::parse(&im).unwrap().cpusubtype, 2);
    le::put_u32(&mut im, 8, 0);

    // DarwinBoot's rejected variants: (offset, 64-bit, value)
    let size = im.len() as u64;
    let bad: [(usize, bool, u64); 10] = [
        (0, false, 0xfeedface),                // 32-bit magic
        (4, false, 0x01000007),                // x86_64
        (8, false, 3),                         // bad subtype
        (12, false, 6),                        // MH_DYLIB
        (20, false, size),                     // commands past the file
        (32 + 72 + 24, true, BASE + 0x1000),   // __DATA overlaps __TEXT
        (32 + 144 + 272, true, BASE + 0x4000), // entry in zero-fill, not executable
        (32 + 144 + 272, true, BASE + 0x1002), // unaligned entry
        (32 + 40, true, 0x100000),             // segment file offset outside the file
        (32 + 4, false, 71),                   // command length not a multiple of 8
    ];
    for (off, wide, value) in bad {
        let saved: [u8; 8] = im[off..off + 8].try_into().unwrap();
        if wide {
            le::put_u64(&mut im, off, value);
        } else {
            le::put_u32(&mut im, off, value as u32);
        }
        assert!(Kernel::parse(&im).is_err(), "variant at {off:#x} = {value:#x} parses");
        im[off..off + 8].copy_from_slice(&saved);
    }
    assert!(Kernel::parse(&im[..16]).is_err());
    assert!(Kernel::parse(&im).is_ok());
}

#[test]
fn fileset() {
    let mut im = fixture::collection(BASE, 2, &[]);
    let k = Kernel::parse(&im).unwrap_or_else(|e| panic!("fileset: {e}"));
    assert!(k.fileset && k.segments.len() == 4 && k.entries.len() == 2 && k.kernel_header == 0x4000);
    assert!(k.entries[0].name == "com.apple.kernel" && k.entries[1].name == "com.example.kext");
    assert!(k.entries[1].vmaddr == BASE + 0xc000 && k.entry == BASE + 0x5000 && k.base == BASE);
    assert_eq!(macho::symbol(&im, k.kernel_header, "_fixture_symbol"), Some(BASE + 0x5008));

    let entry0 = 32 + 4 * 72 + 288;
    let saved = u64::from_le_bytes(im[entry0 + 8..entry0 + 16].try_into().unwrap());
    le::put_u64(&mut im, entry0 + 8, BASE + 0x8000); // member header not mapped where the entry says
    assert!(Kernel::parse(&im).is_err());
    le::put_u64(&mut im, entry0 + 8, saved);
    im[entry0 + 32 + 15] = b'x'; // no com.apple.kernel
    assert!(Kernel::parse(&im).is_err());
    im[entry0 + 32 + 15] = b'l';
    le::put_u32(&mut im, 0x4000 + 24, 0); // the kernel member lost MH_DYLIB_IN_CACHE
    assert!(Kernel::parse(&im).is_err());
    le::put_u32(&mut im, 0x4000 + 24, 0x8000_0000);
    le::put_u64(&mut im, 0x4000 + 32 + 144 + 272, BASE + 0x5004); // member entry differs
    assert!(Kernel::parse(&im).is_err());
    le::put_u64(&mut im, 0x4000 + 32 + 144 + 272, BASE + 0x5000);
    assert!(Kernel::parse(&im).is_ok());
}

#[test]
fn adt() {
    let mut root = Node::new("device-tree");
    root.set_u32("#address-cells", 2).unwrap();
    root.path("chosen/memory-map").unwrap().set("RAMDisk", [0u8; 16]).unwrap();
    root.path("chosen").unwrap().set_str("osenvironment", "main").unwrap();
    root.path("arm-io/dev").unwrap().set("dma-coherent", b"").unwrap();
    root.path("arm-io/dev").unwrap().set("odd", b"abcde").unwrap(); // padded to 8 in the flat form
    assert!(root.set("a-property-name-that-is-longer-than-31", b"").is_err());
    assert!(root.find("chosen/memory-map").is_some_and(|m| m.get("RAMDisk").is_some()));
    assert!(root.find("chosen/none").is_none());

    let flat = root.flatten();
    let size = flat.len();
    // the layout XNU reads: root = 2 properties ("name" first), 2 children
    assert!(flat[..8] == [2, 0, 0, 0, 2, 0, 0, 0] && &flat[8..13] == b"name\0");
    assert!(flat[40..44] == 12u32.to_le_bytes() && &flat[44..56] == b"device-tree\0");
    assert_eq!(size % 4, 0);

    let mut back = Node::parse(&flat).unwrap();
    assert_eq!(back.flat_size(), size);
    assert_eq!(back.flatten(), flat);
    let p = back.find("arm-io/dev").unwrap().get("odd").unwrap();
    assert_eq!(p.value, b"abcde");
    assert!(back.remove("arm-io").is_some() && back.find("arm-io").is_none() && back.remove("arm-io").is_none());
    assert!(back.unset("#address-cells") && back.get("#address-cells").is_none());

    // malformed flat trees
    assert!(Node::parse(&flat[..size - 4]).is_err());
    assert!(Node::parse(&flat[..4]).is_err());
    let mut bad = flat.clone();
    bad.extend_from_slice(&[0; 8]);
    assert_eq!(Node::parse(&bad).unwrap_err().0, "bytes after the device tree"); // trailing bytes
    let mut bad = flat.clone();
    le::put_u32(&mut bad, 8 + 32, 0x7fff_fff0);
    assert!(Node::parse(&bad).is_err()); // property past the tree
    let mut bad = flat.clone();
    le::put_u32(&mut bad, 4, 0xff_ffff);
    assert!(Node::parse(&bad).is_err()); // child count past the tree
}

fn machine_default() -> Machine {
    use DeviceKind::*;
    let mut m = Machine::default();
    m.cpu_cluster[2] = Cluster::E;
    m.board(4, &[Block, Block, DataStore, Input, Gpu]);
    m.timebase_hz = 19_200_000;
    m.dram_base = 0x8_0000_4000;
    m.dram_size = 0x4000_0000;
    m.trust_cache = true;
    m.model = Some("iPhone18,2".into());
    m.root_volume_uuid = Some("0a1b2c3d-0000-4000-8000-00000000abcd".into());
    m.host_vector = 0x20_0010_0000;
    m.user_va_limit = 0x7e_0000_0000;
    m.host_page_size = 4096;
    m.random_seed = std::array::from_fn(|i| i as u8);
    m
}

#[test]
fn machine_tree() {
    let m = machine_default();
    let t = m.tree().unwrap_or_else(|e| panic!("machine: {e}"));
    // what ArmDTPlatform::configure requires
    for node in ["cpus", "arm-io", "defaults", "options"] {
        assert!(t.find(node).is_some(), "{node}");
    }
    // the board without an identity tree: the target-type iOS names its jetsam properties by
    {
        let mut b = machine_default();
        b.target_type = Some("V54".into());
        let bt = b.tree().unwrap();
        assert!(str_of(&bt, "target-type") == Some("V54") && t.get("target-type").is_none());
    }
    // the iPhone18,2's I/O throttle (SSD periods, its /defaults tier 3 period and window), whatever the disks answer
    let d = t.find("defaults").unwrap();
    let throttle = [
        ("kern.io_throttle_period_tier1", 5),
        ("kern.io_throttle_period_tier2", 15),
        ("kern.io_throttle_period_tier3", 20),
        ("kern.io_throttle_window_tier3", 100),
    ];
    for (name, ms) in throttle {
        assert_eq!(u32_of(d, name), Some(ms), "{name}");
    }
    assert!(d.get("kern.io_throttle_window_tier1").is_none() && d.get("kern.io_throttle_window_tier2").is_none());
    // no ram-view: the iPhone18,2's default task limit, no entitled raise
    assert!(u32_of(d, "kern.max_task_pmem") == Some(3376) && d.get("kern.entitled_max_task_pmem").is_none());
    {
        // ram-view=12G: an iPhone18,2's own limits (24A446's DeviceTree /defaults); 6G: half of each
        let mut v = machine_default();
        v.view_size = 12 << 30;
        let vt = v.tree().unwrap();
        let d = vt.find("defaults").unwrap();
        let names = ["kern.max_task_pmem", "kern.entitled_max_task_pmem", "kern.entitled_dev_max_task_pmem"];
        assert_eq!(names.map(|n| u32_of(d, n)), [Some(3376), Some(6144), Some(6656)]);
        v.view_size = 6 << 30;
        assert_eq!(u32_of(v.tree().unwrap().find("defaults").unwrap(), "kern.max_task_pmem"), Some(1688));
    }
    let chosen = t.find("chosen").unwrap();
    assert!(chosen.get("nvram-bank-size").is_some_and(|p| p.value.len() == 4));
    let p = chosen.get("nvram-proxy-data").unwrap();
    assert!(p.value.len() == 8192 && p.value[0] == 0x70 && &p.value[4..9] == b"nvram");
    assert_eq!(chosen.get("random-seed").unwrap().value.len(), 256);
    assert!(str_of(chosen, "root-matching").unwrap().contains("<string>0A1B2C3D-0000-4000-8000-00000000ABCD</string>"));
    // what ArmCpu reads
    let cpus = t.find("cpus").unwrap();
    assert_eq!(cpus.children.len(), 4);
    assert_eq!(str_of(&cpus.children[0], "state"), Some("running"));
    assert_eq!(str_of(&cpus.children[3], "state"), Some("waiting"));
    assert_eq!(str_of(&cpus.children[2], "cluster-type"), Some("E"));
    assert_eq!(u32_of(&cpus.children[1], "timebase-frequency"), Some(19_200_000));
    let intc = t.find("arm-io/intc").unwrap();
    let r = words(&intc.get("reg").unwrap().value);
    assert!(r.len() == 4 && r[0] == BOARD_GIC_DIST - 0x1000 && r[1] >= 0x10000);
    assert!(r[2] == BOARD_GIC_REDIST - 0x1000 && r[3] == 4 * 0x20000);
    assert_eq!(intc.get("compatible").unwrap().value, b"arm,gic-v3\0");
    assert_eq!(str_of(t.find("psci").unwrap(), "method"), Some("hvc"));
    // the devices, by the names and compatibles their drivers match
    assert_eq!(t.find("arm-io/virtio-mmio-1").unwrap().get("compatible").unwrap().value, b"virtio,mmio\0");
    for node in ["arm-io/data-store", "arm-io/input-0", "arm-io/virtio-gpu"] {
        assert!(t.find(node).is_some(), "{node}");
    }
    assert!(t.find("arm-io/virtio-gpu").unwrap().get("drm-host-pool").is_none()); // no 3D renderer: no pool
    let store = t.find("arm-io/data-store").unwrap();
    assert_eq!(
        words(&store.get("reg").unwrap().value),
        [BOARD_VIRTIO + 2 * BOARD_VIRTIO_SIZE - 0x1000, BOARD_VIRTIO_SIZE]
    );
    assert_eq!(u32_of(store, "droidvm,intid"), Some(BOARD_VIRTIO_SPI + 2));
    let bt = t.find("bluetooth").unwrap();
    assert!(u32_of(bt, "transport-encoding") == Some(10) && bt.get("transport-speed").is_none());
    assert!(t.find("arm-io/cpufreq").is_none());
    // memory-map placeholders and the host node
    let map = t.find("chosen/memory-map").unwrap();
    assert!(map.get("TrustCache").is_some() && map.get("RAMDisk").is_none());
    // RAMDisk1 (port-a56, iOS 16): absent by default, even with a ramdisk
    let ramdisks = |m: &Machine| {
        let t = m.tree().unwrap();
        let map = t.find("chosen/memory-map").unwrap();
        (map.get("RAMDisk").is_some(), map.get("RAMDisk1").is_some())
    };
    let mut r1 = machine_default();
    r1.ramdisk = true;
    assert_eq!(ramdisks(&r1), (true, false));
    r1.ramdisk1 = true;
    assert_eq!(ramdisks(&r1), (true, true));
    r1.ramdisk = false;
    assert_eq!(ramdisks(&r1), (false, false)); // no ramdisk: no node either

    // no identity tree (the phone's image set has none): /product still has the artwork answers CoreUI asks
    // MobileGestalt for (2026-11-22: without them every app probed loose image files on each redraw), as V54's
    let pr = t.find("product").unwrap();
    assert_eq!(str_of(pr, "artwork-device-idiom"), Some("phone"));
    assert_eq!(u32_of(pr, "artwork-device-subtype"), Some(2868));
    assert_eq!(str_of(pr, "artwork-display-gamut"), Some("sRGB")); // the display's
    assert_eq!(str_of(pr, "artwork-dynamic-displaymode"), Some("0"));
    assert_eq!(str_of(pr, "graphics-featureset-class"), Some("APPLE10"));
    assert_eq!(
        str_of(pr, "graphics-featureset-fallbacks"),
        Some("APPLE9:APPLE8:APPLE7:APPLE6:APPLE5:APPLE4:APPLE3:APPLE3v1:APPLE1:GLES2,0") // no APPLE2
    );
    assert_eq!(str_of(pr, "model"), Some("oneios,xnuhost"));
    assert_eq!(
        u64::from_le_bytes(t.find("chosen/xnuhost").unwrap().get("vector").unwrap().value[..].try_into().unwrap()),
        0x20_0010_0000
    );
    // it flattens and parses back
    assert_eq!(Node::parse(&t.flatten()).unwrap(), t);
}

#[test]
fn machine_ios16_product_and_serial() {
    // Without them (iOS 27, the S24 Ultra): no product-id, no product-description, no serial-number, as before.
    let plain = machine_default().tree().unwrap();
    let product = plain.find("product").unwrap();
    assert!(product.get("product-id").is_none() && product.get("product-description").is_none());
    assert!(plain.get("serial-number").is_none());
    // An iOS 16.5 image set's (xnuhost): /product product-id as given, product-description = product-name, and the
    // root's serial-number in IODTPlatformExpert's layout.
    let id: [u8; 20] = std::array::from_fn(|i| 0xa0 + i as u8);
    let mut m = machine_default();
    m.product_id = Some(id);
    m.serial_number = Some("ONEIOS16A56".into());
    let t = m.tree().unwrap();
    let product = t.find("product").unwrap();
    assert_eq!(product.get("product-id").unwrap().value, id);
    assert!(str_of(product, "product-description") == Some("OneiOS guest"));
    assert!(str_of(product, "product-name") == Some("OneiOS guest"));
    let serial = &t.get("serial-number").unwrap().value;
    assert_eq!(serial.len(), 32);
    // XNU's IODTPlatformExpert::createSystemSerialNumberString with no '-': bytes 13..21, then 0..3
    assert!(!serial.contains(&b'-'));
    let shown: Vec<u8> = serial[13..21].iter().chain(&serial[..3]).copied().collect();
    assert_eq!(shown, b"ONEIOS16A56");
    assert!(serial[3..13].iter().chain(&serial[21..]).all(|&b| b == 0));
    // MobileGestalt's cache follows the new answers (hosted 0084): another identity
    assert_ne!(machine::identity_hash(&t), machine::identity_hash(&plain));
    // A serial the layout cannot carry is refused, not shortened
    for bad in ["ONEIOS16A5", "ONEIOS16A567", "oneios16a56", "ONEIOS-6A56", "ONEIOS 6A56"] {
        assert!(machine::serial_number_property(bad).is_none(), "{bad}");
        m.serial_number = Some(bad.into());
        assert!(m.tree().is_err(), "{bad}");
    }
    // An identity tree's own /product wins: no product-id or description of ours is added to it
    let mut i = machine_default();
    let mut idt = Node::new("device-tree");
    idt.set("model", b"iPhone10,3\0").unwrap();
    idt.set("target-type", b"D22\0").unwrap();
    idt.add("product").set("product-name", b"iPhone X\0").unwrap();
    i.identity = Some(idt);
    i.product_id = Some(id);
    let it = i.tree().unwrap();
    assert!(it.find("product").unwrap().get("product-id").is_none());
}

#[test]
fn machine_cameras_and_identity() {
    use DeviceKind::*;
    let t = machine_default().tree().unwrap();
    // the cameras: none without the camera device, whatever m.cameras says; with it /arm-io/isp camera-rear and
    // camera-front (u32 1, no compatible), an empty /product/camera and /product unique-model CAMERA_MODEL ("OneiOSAP":
    // in no AVGestalt table, so VirtualAudio builds no webcam subport); camera-model= gives another (V54AP: the iPhone's)
    assert!(t.find("arm-io/isp").is_none() && t.find("product/camera").is_none());
    assert!(t.find("product").unwrap().get("unique-model").is_none());
    let mut c = machine_default();
    c.cameras = CAMERA_REAR | CAMERA_FRONT;
    let ct = c.tree().unwrap();
    assert!(ct.find("arm-io/isp").is_none() && ct.find("product/camera").is_none()); // no camera device
    c.board(4, &[Block, DataStore, Gpu, Camera]);
    let mut ct = c.tree().unwrap();
    let isp = ct.find("arm-io/isp").unwrap();
    assert!(u32_of(isp, "camera-rear") == Some(1) && u32_of(isp, "camera-front") == Some(1));
    assert!(isp.get("compatible").is_none() && isp.get("device_type").is_none());
    let cam = ct.find("product/camera").unwrap();
    assert!(cam.props.len() == 1 && cam.children.is_empty()); // only its name
    assert!(str_of(ct.find("product").unwrap(), "unique-model") == Some("OneiOSAP") && CAMERA_MODEL == "OneiOSAP");
    // the identity: another number with the cameras; set in /chosen; the same whatever changes per boot
    let with = machine::identity(&mut ct);
    let stored = ct.find("chosen").unwrap().get("oneios-identity").unwrap().u64();
    assert!(with != 0 && stored == Some(with) && with != machine::identity_hash(&t));
    c.dram_size *= 2; // the RAM, the seed, the pools and xnuhost's vector (its ASLR)
    c.view_size = 0x8000_0000;
    c.random_seed[0] ^= 0xff;
    c.host_vector += 0x4000;
    c.gpu_pool_base = 0x9_0000_0000;
    c.gpu_pool_size = 0x1000_0000;
    let mut again = c.tree().unwrap();
    assert_eq!(machine::identity_hash(&again), with);
    // the memory map's placeholders and /chosen oneios-identity itself are not part of it either
    let mut filled = [0u8; 16];
    filled[..3].copy_from_slice(&[1, 2, 3]);
    again.find_mut("chosen/memory-map").unwrap().set("TrustCache", filled).unwrap();
    assert!(machine::identity(&mut again) == with && machine::identity(&mut again) == with);
    c.display_scale = 2; // the display mode is (MobileGestalt's main-screen-scale)
    assert_ne!(machine::identity_hash(&c.tree().unwrap()), with);
    c.display_scale = 0;
    c.cameras = CAMERA_REAR; // the rear one only
    let again = c.tree().unwrap();
    let isp = again.find("arm-io/isp").unwrap();
    assert!(isp.get("camera-rear").is_some() && isp.get("camera-front").is_none());
    assert_ne!(machine::identity_hash(&again), with);
    // camera-model=V54AP (the name before 2026-11-23): that name, and another identity (MobileGestalt's cache is made
    // again once when the setting changes, hosted 0084)
    c.cameras = CAMERA_REAR | CAMERA_FRONT;
    c.unique_model = Some("V54AP".into());
    let again = c.tree().unwrap();
    let q = again.find("product").unwrap().get("unique-model").unwrap();
    assert!(q.str() == Some("V54AP") && q.value.len() == 6 && machine::identity_hash(&again) != with);
    c.unique_model = None;
    assert_eq!(machine::identity_hash(&c.tree().unwrap()), with); // back to the default
}

#[test]
fn machine_corner_radius() {
    // The panel's corners in points, as /product display-corner-radius: two little-endian u32s, the radius and 1
    // (the Galaxy S25: 102 px at the UI scale of 3 is 34). 0 writes nothing (the guest lays out for square corners).
    let mut m = machine_default();
    m.display_scale = 3;
    m.corner_radius = 34;
    let t = m.tree().unwrap();
    let pr = t.find("product").unwrap();
    let v = &pr.get("display-corner-radius").unwrap().value;
    assert_eq!(v.len(), 8);
    assert_eq!(u32::from_le_bytes(v[..4].try_into().unwrap()), 34);
    assert_eq!(u32::from_le_bytes(v[4..].try_into().unwrap()), 1);
    m.corner_radius = 0;
    let t = m.tree().unwrap();
    assert!(t.find("product").unwrap().get("display-corner-radius").is_none());
}

#[test]
fn machine_artwork_subtype() {
    // /product artwork-device-subtype names the artwork and the layout metrics CoreUI, SpringBoard and UIKit look up
    // by it: the panel's own on the S25 (a 1080x2340 panel at @3x is the iPhone 12/13 mini's 360x780 grid, subtype
    // 2340), the iPhone18,2's Pro Max (2868) for any other height; artwork-subtype= overrides both.
    let mut m = machine_default();
    (m.display_width, m.display_height) = (1080, 2340);
    let t = m.tree().unwrap();
    assert_eq!(u32_of(t.find("product").unwrap(), "artwork-device-subtype"), Some(2340));
    (m.display_width, m.display_height) = (1440, 3120);
    let t = m.tree().unwrap();
    assert_eq!(u32_of(t.find("product").unwrap(), "artwork-device-subtype"), Some(2868));
    m.display_height = 2340;
    m.artwork_subtype = 2868;
    let t = m.tree().unwrap();
    assert_eq!(u32_of(t.find("product").unwrap(), "artwork-device-subtype"), Some(2868));
    m.display_height = 3120;
    m.artwork_subtype = 2340;
    let t = m.tree().unwrap();
    assert_eq!(u32_of(t.find("product").unwrap(), "artwork-device-subtype"), Some(2340));
}

#[test]
fn machine_identity_tree() {
    use DeviceKind::*;
    // identity from a donor tree: model, target-type, board first in compatible, /product filtered
    let mut donor = Node::new("device-tree");
    donor.set_str("model", "Fixture1,1").unwrap();
    donor.set_str("target-type", "F1").unwrap();
    donor.set("compatible", b"F1AP\0Fixture1,1\0AppleARM\0").unwrap();
    let product = donor.add("product");
    product.set_str("product-name", "Fixture phone").unwrap();
    product.set_u32("has-exclaves", 1).unwrap();
    product.set_str("graphics-featureset-fallbacks", "MTL1,2:APPLE2:MTL2,2:GLES2,0").unwrap();
    product.add("audio").set_u32("acoustic-id", 77).unwrap();
    product.set_u32("artwork-scale-factor", 3).unwrap();
    product.set_u32("artwork-device-subtype", 2868).unwrap();
    let mut m = machine_default();
    m.identity = Some(donor.clone());
    // the display: the mode, the UI scale and the refresh rate (the scale in /chosen and in /product too)
    (m.display_width, m.display_height, m.display_scale, m.display_hz) = (1440, 3120, 2, 120);
    let t = m.tree().unwrap();
    let ch = t.find("chosen").unwrap();
    assert!(u32_of(ch, "display-scale") == Some(2) && u32_of(ch, "display-rotation") == Some(0));
    let host = t.find("chosen/xnuhost").unwrap();
    let display = ["display-width", "display-height", "display-hz", "display-scale"].map(|n| u32_of(host, n));
    assert_eq!(display, [Some(1440), Some(3120), Some(120), Some(2)]);
    let pr = t.find("product").unwrap();
    assert_eq!(u32_of(pr, "artwork-scale-factor"), Some(2));
    assert_eq!(u32_of(pr, "artwork-device-subtype"), Some(2868)); // the identity's set of artwork stays
    assert_eq!(str_of(&t, "model"), Some("Fixture1,1"));
    assert_eq!(t.get("compatible").unwrap().value, b"F1AP\0oneios,xnuhost\0");
    assert!(pr.get("has-exclaves").is_none() && pr.get("product-name").is_some());
    assert_eq!(str_of(pr, "graphics-featureset-fallbacks"), Some("MTL1,2:GLES2,0"));
    // astc (xnuhost's astc=1, 2026-11-25): APPLE2 and MTL2,2 stay, with an identity and without one
    m.astc = true;
    let with_astc = m.tree().unwrap();
    assert_eq!(
        str_of(with_astc.find("product").unwrap(), "graphics-featureset-fallbacks"),
        Some("MTL1,2:APPLE2:MTL2,2:GLES2,0")
    );
    let mut plain = machine_default();
    plain.astc = true;
    let plain_tree = plain.tree().unwrap();
    assert_eq!(
        str_of(plain_tree.find("product").unwrap(), "graphics-featureset-fallbacks"),
        Some("APPLE9:APPLE8:APPLE7:APPLE6:APPLE5:APPLE4:APPLE3:APPLE3v1:APPLE2:APPLE1:GLES2,0")
    );
    m.astc = false;
    assert!(t.find("product/audio").unwrap().get("acoustic-id").is_some());
    // with an identity and cameras: the identity's own unique-model stays, unless camera-model= names one
    {
        let mut c = m.clone();
        c.board(4, &[Block, DataStore, Camera]);
        c.cameras = CAMERA_FRONT;
        donor.child_mut("product").unwrap().set_str("unique-model", "F1AP").unwrap();
        c.identity = Some(donor.clone());
        let t = c.tree().unwrap();
        assert_eq!(str_of(t.find("product").unwrap(), "unique-model"), Some("F1AP"));
        let isp = t.find("arm-io/isp").unwrap();
        assert!(isp.get("camera-front").is_some() && isp.get("camera-rear").is_none());
        c.unique_model = Some(CAMERA_MODEL.into());
        let t = c.tree().unwrap();
        assert_eq!(str_of(t.find("product").unwrap(), "unique-model"), Some("OneiOSAP"));
    }
    donor.remove("product");
    m.identity = Some(donor);
    assert_eq!(m.tree().unwrap_err().0, "the identity tree has no model, target-type or /product");
}

#[test]
fn machine_bridges_and_refusals() {
    // the bridged Bluetooth controller, the cpufreq windows and the bridge channel
    let mut m = machine_default();
    (m.bt_transport, m.bt_speed, m.bt_mac) = (3, 3_000_000, [0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
    (m.cpufreq_base, m.cpufreq_size) = (BOARD_CPUFREQ, 4 * 0x1000);
    m.devices[4].kind = DeviceKind::Bridge;
    let t = m.tree().unwrap();
    let b = t.find("bluetooth").unwrap();
    assert!(u32_of(b, "transport-encoding") == Some(3) && b.get("local-mac-address").unwrap().value == m.bt_mac);
    assert!(b.get("transport-speed").is_some());
    assert_eq!(t.find("arm-io/bridge").unwrap().get("compatible").unwrap().value, b"oneios,bridge\0");
    assert_eq!(words(&t.find("arm-io/cpufreq").unwrap().get("reg").unwrap().value), [BOARD_CPUFREQ - 0x1000, 0x4000]);
    let (wifi, eth) = (m.mac(Mac::Wifi), m.mac(Mac::Ethernet));
    assert!(
        wifi[0] == 0x02 && wifi != eth && t.find("chosen").unwrap().get("mac-address-wifi0").unwrap().value == wifi
    );

    // refused machines
    let refused = |change: &dyn Fn(&mut Machine)| {
        let mut m = machine_default();
        change(&mut m);
        m.tree().is_err()
    };
    assert!(refused(&|m| m.ncpu = 9));
    assert!(refused(&|m| m.timebase_hz = 0));
    assert!(refused(&|m| m.armio_base = 0));
    assert!(refused(&|m| m.devices[0].base = 0x5000_0000));
    assert!(refused(&|m| m.devices[1].kind = DeviceKind::Gpu)); // two GPUs
                                                                // the 3D renderer's pool: base and size on the GPU's node, as UniXNU-droidvm's DroidVMVirtioGpu3D.cpp reads them
    let mut m = machine_default();
    (m.gpu_pool_base, m.gpu_pool_size) = (0x8_ff80_0000, 8 << 20);
    let gt = m.tree().unwrap();
    assert_eq!(
        words(&gt.find("arm-io/virtio-gpu").unwrap().get("drm-host-pool").unwrap().value),
        [0x8_ff80_0000, 8 << 20]
    );
}

#[test]
fn sidecars() {
    let mut tc = fixture::trustcache(3);
    assert!(boot::trustcaches_valid(&tc));
    le::put_u32(&mut tc, 8, 3);
    assert!(!boot::trustcaches_valid(&tc)); // version
    le::put_u32(&mut tc, 8, 2);
    le::put_u32(&mut tc, 28, 0xffff_ffff);
    assert!(!boot::trustcaches_valid(&tc)); // count
    le::put_u32(&mut tc, 28, 3);
    le::put_u32(&mut tc, 4, 0xffff_fff0);
    assert!(!boot::trustcaches_valid(&tc)); // offset
    le::put_u32(&mut tc, 4, 8);
    tc[32 + 24..32 + 44].fill(0x05);
    assert!(!boot::trustcaches_valid(&tc)); // order
    assert!(!boot::trustcaches_valid(&tc[..4]));

    // packed ramdisk: 16 pages logical, two extents
    let mut rd = vec![0u8; 32 + 2 * 16 + 2 * 4096];
    rd[..8].copy_from_slice(b"DBRDISK1");
    le::put_u32(&mut rd, 8, 1);
    le::put_u32(&mut rd, 12, 4096);
    le::put_u64(&mut rd, 16, 16 * 4096);
    le::put_u64(&mut rd, 24, 2);
    for (at, v) in [(32, 4096), (40, 4096), (48, 8 * 4096), (56, 4096)] {
        le::put_u64(&mut rd, at, v);
    }
    rd[64..64 + 4096].fill(0xaa);
    rd[64 + 4096..].fill(0xbb);
    let l = boot::ramdisk_layout(&rd).unwrap();
    assert!(l.packed && l.expanded == 16 * 4096);
    let mut out = vec![0u8; l.expanded as usize];
    boot::ramdisk_copy(&mut out, &rd, l.packed);
    assert!(out[0] == 0 && out[4096] == 0xaa && out[8191] == 0xaa && out[8192] == 0 && out[8 * 4096] == 0xbb);
    le::put_u64(&mut rd, 48, 4096); // overlapping extents
    assert!(boot::ramdisk_layout(&rd).is_none());
    le::put_u64(&mut rd, 48, 8 * 4096);
    assert!(boot::ramdisk_layout(&rd[..rd.len() - 1]).is_none());
    assert_eq!(boot::ramdisk_layout(b"plain disk"), Some(boot::RamdiskLayout { expanded: 10, packed: false }));
}

#[test]
fn place() {
    const CODE: [u8; 8] = [0x1f, 0x20, 0x03, 0xd5, 0xc0, 0x03, 0x5f, 0xd6]; // nop; ret
    let im = fixture::kernel(BASE, 0, &CODE, false);
    let k = Kernel::parse(&im).unwrap();
    let mut m = machine_default();
    m.ramdisk = true;
    const WINDOW: usize = 64 << 20;
    let mut ram = vec![0u8; WINDOW];
    let tc = fixture::trustcache(2);
    let disk = [0x5au8; 4097];
    let request = BootRequest {
        image: &im,
        kernel: &k,
        phys_base: 0x8_0000_4000,
        virt_base: BASE,
        command_line: "",
        video: [0; 6],
        boot_flags: 0,
        mem_size_actual: 0,
        legacy_boot_args: false,
        ramdisk: Some(&disk),
        trustcache: Some(&tc),
        gpu_addon: None,
        root_policies: None,
    };

    for hosted in [false, true] {
        for legacy in [false, true] {
            let mut tree = m.tree().unwrap();
            let r = BootRequest {
                virt_base: if hosted { 0x20_0000_4000 } else { BASE },
                command_line: "debug=0x8 -v",
                legacy_boot_args: legacy,
                video: [0x9_0000_0000, 1, 4 * 1080, 1080, 2340, 32],
                mem_size_actual: if hosted { 12 << 30 } else { 0 }, // ram-view: memSizeActual only, memSize stays the window
                ..request
            };
            let l = r.place(&mut ram, &mut tree).unwrap_or_else(|e| panic!("place: {e}"));
            assert!(l.slide == r.virt_base.wrapping_sub(BASE) && l.entry_va == r.virt_base + 0x1000);
            assert_eq!(l.entry_pa, r.phys_base + 0x1000);
            assert_eq!(ram[0x1000..0x1008], CODE); // __TEXT copied
            assert!(l.args_pa == r.phys_base + 0x8000 && l.tree_pa == l.args_pa + 0x1000); // 16 KiB after the kernel's end
            let args = &ram[(l.args_pa - r.phys_base) as usize..];
            let f = words(&args[8..40]);
            assert!(args[0] == 2 && args[2] == 2);
            assert_eq!(f, [r.virt_base, r.phys_base, WINDOW as u64, r.phys_base + l.used]);
            let word = |at: usize| le::u64(args, at);
            assert_eq!(word(0x28), 0x9_0000_0000);
            assert_eq!(word(0x60), r.virt_base + (l.tree_pa - r.phys_base));
            assert_eq!(u32::from_le_bytes(args[0x68..0x6c].try_into().unwrap()) as u64, l.tree_size);
            assert_eq!(word(if legacy { 728 } else { 0x478 }), if hosted { 12 << 30 } else { WINDOW as u64 });
            assert_eq!(&args[0x6c..0x6c + 13], b"debug=0x8 -v\0");
            // the tree in the window parses and its memory-map names the copies
            let at = (l.tree_pa - r.phys_base) as usize;
            let placed = Node::parse(&ram[at..at + l.tree_size as usize]).unwrap();
            let map = placed.find("chosen/memory-map").unwrap();
            let pair = words(&map.get("RAMDisk").unwrap().value);
            let disk_at = (pair[0] - r.phys_base) as usize;
            assert!(
                pair[0] == l.ramdisk_pa && pair[1] == 0x2000 && ram[disk_at + 4096] == 0x5a && ram[disk_at + 4097] == 0
            );
            let pair = words(&map.get("TrustCache").unwrap().value);
            let tc_at = (pair[0] - r.phys_base) as usize;
            assert!(pair[0] == l.trustcache_pa && pair[1] == tc.len() as u64 && ram[tc_at..tc_at + tc.len()] == tc[..]);
            assert!(l.used % 0x4000 == 0 && l.used <= WINDOW as u64 - 0x20_0000);
        }
    }

    // RAMDisk1 (port-a56, iOS 16): filled with the same address and size as RAMDisk when the tree names it
    m.ramdisk1 = true;
    let mut tree1 = m.tree().unwrap();
    let r1 = BootRequest { command_line: "debug=0x8 -v", ..request };
    let l1 = r1.place(&mut ram, &mut tree1).unwrap();
    let at = (l1.tree_pa - r1.phys_base) as usize;
    let placed1 = Node::parse(&ram[at..at + l1.tree_size as usize]).unwrap();
    let map = placed1.find("chosen/memory-map").unwrap();
    let (rd, rd1) = (words(&map.get("RAMDisk").unwrap().value), words(&map.get("RAMDisk1").unwrap().value));
    assert!(rd1 == rd && rd1[0] == l1.ramdisk_pa);
    m.ramdisk1 = false;

    // refusals
    let refused = |r: BootRequest, tree: &mut Node, ram: &mut [u8]| r.place(ram, tree).is_err();
    let mut tree = m.tree().unwrap();
    // tree names a trust cache that is not supplied
    assert!(refused(BootRequest { trustcache: None, ..request }, &mut tree, &mut ram));
    // slide not a multiple of 2 MiB
    assert!(refused(BootRequest { virt_base: BASE + 0x4000, ..request }, &mut tree, &mut ram));
    // physical base loses the 2 MiB offset
    assert!(refused(BootRequest { phys_base: 0x8_0000_0000, ..request }, &mut tree, &mut ram));
    // window too small
    assert!(refused(request, &mut tree, &mut ram[..16 << 20]));
    // not ASCII
    assert!(refused(BootRequest { command_line: "caf\u{e9}", ..request }, &mut tree, &mut ram));
    let long = "a".repeat(1099);
    assert!(refused(BootRequest { command_line: &long, ..request }, &mut tree, &mut ram));
    let mut bad_tc = tc.clone();
    bad_tc[8] = 9;
    assert!(refused(BootRequest { trustcache: Some(&bad_tc), ..request }, &mut tree, &mut ram)); // bad trust cache
    let mut stale = [0u8; 16];
    stale[0] = 1;
    tree.find_mut("chosen/memory-map").unwrap().set("RAMDisk", stale).unwrap();
    assert!(refused(request, &mut tree, &mut ram)); // stale memory-map entry
}

// The C Makefile's check: the board's tree for 4 CPUs, flat, read back by `adt`.
#[test]
fn inspect_board_and_adt() {
    let inspect = env!("CARGO_BIN_EXE_xnuloader-inspect");
    let board = std::process::Command::new(inspect).args(["board", "4"]).output().unwrap();
    assert!(board.status.success());
    let tree = Node::parse(&board.stdout).unwrap();
    assert_eq!(tree.find("cpus").unwrap().children.len(), 4);
    let dir = std::env::temp_dir().join(format!("xnuloader-inspect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let flat = dir.join("board.adt");
    std::fs::write(&flat, &board.stdout).unwrap();
    let dump = std::process::Command::new(inspect).arg("adt").arg(&flat).output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(dump.status.success());
    let text = String::from_utf8(dump.stdout).unwrap();
    assert!(text.starts_with("device-tree {\n") && text.contains("    cpu3 {\n") && text.ends_with("}\n"));
    let usage = std::process::Command::new(inspect).output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert_eq!(usage.stderr, b"usage: xnuloader-inspect kernel FILE | adt FILE | board NCPU\n");
}

// mkkernel: a small AArch64 ELF (one PT_LOAD: 16 bytes of code at the entry, more memory than file) becomes a kernel
// the loader takes, its code at the entry, the zero-fill after the text; and the test disk's sectors.
#[test]
fn mkkernel_wraps_an_elf() {
    let entry = BASE + 0x1000;
    let mut elf = vec![0u8; 0x1010];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2; // ELFCLASS64
    elf[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
    le::put_u64(&mut elf, 24, entry);
    le::put_u64(&mut elf, 32, 64); // e_phoff
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    let ph = 64;
    le::put_u32(&mut elf, ph, 1); // PT_LOAD
    le::put_u64(&mut elf, ph + 8, 0x1000);
    le::put_u64(&mut elf, ph + 16, entry);
    le::put_u64(&mut elf, ph + 32, 0x10);
    le::put_u64(&mut elf, ph + 40, 0x5000);
    elf[0x1000..0x1010].copy_from_slice(&[0x1f, 0x20, 0x03, 0xd5].repeat(4)); // nop x4

    let dir = std::env::temp_dir().join(format!("mkkernel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (elf_path, kc, disk) = (dir.join("k.elf"), dir.join("kernel.kc"), dir.join("os.img"));
    std::fs::write(&elf_path, &elf).unwrap();
    let out =
        std::process::Command::new(env!("CARGO_BIN_EXE_mkkernel")).args([&elf_path, &kc, &disk]).output().unwrap();
    let (image, sectors) = (std::fs::read(&kc).unwrap(), std::fs::read(&disk).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "mkkernel: base 0xfffffff007004000, text 0x4000, bss 0x4000, entry 0xfffffff007005000\n"
    );
    let k = Kernel::parse(&image).unwrap();
    assert!(k.base == BASE && k.entry == entry && k.end == BASE + 0x8000 && k.segments.len() == 2);
    assert_eq!(image[0x1000..0x1010], elf[0x1000..0x1010]);
    assert_eq!(sectors.len(), 2048 * 512);
    let s7 = &sectors[7 * 512..8 * 512];
    assert!(&s7[..8] == b"ONEIOSTD" && s7[8..12] == 7u32.to_le_bytes() && s7[12..].iter().all(|b| *b == 7 ^ 0x5a));
}
