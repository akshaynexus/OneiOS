// SPDX-License-Identifier: BSD-2-Clause-Patent
//! The device tree of the machine xnuhost presents to XNU (machine.c).
//!
//! Ported from UniXNU-droidvm (https://github.com/Droid-VM/UniXNU-droidvm, commit f9121cd,
//! ipados27/scripts/make-droidvm-adt.py), BSD-2-Clause-Patent: the same nodes ArmDTPlatform and ArmCpu consume
//! (/chosen with a CHRP NVRAM proxy, /cpus, /psci, /defaults, /options, /product, /arm-io with the GICv3 and the
//! virtio-mmio windows). Changes: no Python; the hardware values come from xnuhost's own board instead of a crosvm
//! FDT; the identity is an iPhone's, not an iPad's; a /chosen/xnuhost node tells a hosted kernel where the host-call
//! vector is. The comments on single properties are the reasons that script records for them.

use crate::adt::{Node, Prop};
use crate::Error;

/// ArmCpu starts at most eight cores.
pub const MAX_CPUS: usize = 8;
pub const MAX_DEVICES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// "virtio,mmio": read-only image, NativeVirtioBlock
    Block,
    /// "droidvm,data-store": the writable Data store disk
    DataStore,
    /// "droidvm,virtio-input"
    Input,
    /// "droidvm,net"
    Net,
    /// "droidvm,virtio-gpu"
    Gpu,
    /// "droidvm,virtio-snd"
    Snd,
    /// "oneios,bridge": the message channel of runtime/bridges (haptics, Bluetooth HCI)
    Bridge,
    /// "oneios,camera": camera frames from runtime/bridges/camera (drivers/camera)
    Camera,
    /// "droidvm,virtio-media": the video encoder (runtime/bridges/encoder), with a media-host-pool
    Media,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    pub kind: DeviceKind,
    pub base: u64,
    pub size: u64,
}

/// A CPU's cluster type: every cpu node carries one (an AMP kernel panics on a CPU of none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cluster {
    #[default]
    P,
    E,
}

/// The PSCI conduit of /psci.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Psci {
    Hvc,
    Smc,
}

impl Psci {
    pub const fn name(self) -> &'static str {
        match self {
            Psci::Hvc => "hvc",
            Psci::Smc => "smc",
        }
    }
}

/// /chosen unique-chip-id and chip-id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipId {
    pub unique: u64,
    pub chip: u32,
}

/// Which of the guest's stable addresses ([`Machine::mac`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mac {
    Bluetooth = 0,
    Wifi = 1,
    Ethernet = 2,
}

/// a back camera (the bridge's wide, ultra-wide or a telephoto)
pub const CAMERA_REAR: u32 = 1;
/// the front camera
pub const CAMERA_FRONT: u32 = 2;
/// The default /product unique-model with the cameras (`XL_CAMERA_MODEL`): in neither AVGestalt's nor
/// CMCaptureGestalt's table of 41 iPhone models (so AVGestalt answers false, as with no name), and CMCapture's folder
/// "OneiOS" is the hosted kernel's link to V54 (0088).
pub const CAMERA_MODEL: &str = "OneiOSAP";

// The board xnuhost emulates: where its windows are (physical addresses inside /arm-io).
pub const BOARD_ARMIO_BASE: u64 = 0x1000;
pub const BOARD_ARMIO_SIZE: u64 = 0x4000_0000 - BOARD_ARMIO_BASE;
pub const BOARD_UART: u64 = 0x0000_6000;
pub const BOARD_UART_SIZE: u64 = 0x1000;
pub const BOARD_GIC_DIST: u64 = 0x0800_0000;
pub const BOARD_GIC_DIST_SIZE: u64 = 0x10000;
pub const BOARD_GIC_REDIST: u64 = 0x080a_0000;
pub const BOARD_GIC_STRIDE: u64 = 0x20000;
/// + cpu * 0x1000
pub const BOARD_CPUFREQ: u64 = 0x0900_0000;
pub const BOARD_VIRTIO: u64 = 0x0a00_0000;
/// one page per device, so each is its own trapped page
pub const BOARD_VIRTIO_SIZE: u64 = 0x1000;
pub const BOARD_IO_BASE: u64 = BOARD_GIC_DIST;
pub const BOARD_IO_SIZE: u64 = 0x0300_0000;
/// first SPI INTID of the virtio windows, one each
pub const BOARD_VIRTIO_SPI: u32 = 32;
/// not wired by default
pub const BOARD_UART_SPI: u32 = 31;

/// The machine to describe.
#[derive(Debug, Clone)]
pub struct Machine {
    pub ncpu: u32,
    /// MPIDR affinity of each CPU
    pub cpu_reg: [u32; MAX_CPUS],
    pub cpu_cluster: [Cluster; MAX_CPUS],
    /// CNTFRQ_EL0 of the host
    pub timebase_hz: u32,
    /// the kernel's memory window (physical)
    pub dram_base: u64,
    pub dram_size: u64,
    /// xnuhost's ram-view= (0 = off): the DRAM size the device is said to have. /defaults then carries the
    /// per-process memory limits of an iPhone18,2 (its own device tree's kern.*_task_pmem, scaled by view_size / 12
    /// GiB); without it only kern.max_task_pmem, the iPhone's 3376 MB.
    pub view_size: u64,
    /// must not start at 0 (pexpert takes it as the SoC base)
    pub armio_base: u64,
    pub armio_size: u64,
    pub gic_dist: u64,
    pub gic_dist_size: u64,
    pub gic_redist: u64,
    pub gic_redist_size: u64,
    pub gic_redist_stride: u64,
    /// PL011, node "uart0"; size 0 for none
    pub uart_base: u64,
    pub uart_size: u64,
    pub psci_method: Option<Psci>,
    /// at most MAX_DEVICES
    pub devices: Vec<Device>,
    /// /arm-io/cpufreq: one 4 KiB window per CPU; size 0 for none
    pub cpufreq_base: u64,
    pub cpufreq_size: u64,
    /// /arm-io/virtio-gpu drm-host-pool: physical base and size of the memory, outside the kernel's window, in which
    /// the host's 3D renderer keeps what the guest maps (UniXNU-droidvm DroidVMVirtioGpu3D.cpp); size 0 for no 3D.
    pub gpu_pool_base: u64,
    pub gpu_pool_size: u64,
    /// /arm-io/virtio-media media-host-pool: physical base and size of the memory, outside the kernel's window, in
    /// which the video encoder's MMAP'd V4L2 buffers live (runtime/bridges/encoder); the guest maps it as the
    /// codec's OUTPUT/CAPTURE buffers (UniXNU-droidvm DroidVMVirtioMedia.c). Size 0 for no encoder.
    pub media_pool_base: u64,
    pub media_pool_size: u64,
    /// /bluetooth: transport-encoding 10 (the virtual transport, no controller) when bt_transport is 0; else that
    /// encoding (xnuhost: 1, H4, the phone's controller behind the bridge) with bt_speed and bt_mac.
    pub bt_transport: u32,
    pub bt_speed: u32,
    pub bt_mac: [u8; 6],
    /// /pram; size 0 for none
    pub panic_base: u64,
    pub panic_size: u64,
    /// xnu-12377 takes 4 x 64 seed bytes
    pub random_seed: [u8; 256],
    pub chip_id: Option<ChipId>,
    /// /chosen root-matching by APFS volume UUID
    pub root_volume_uuid: Option<String>,
    // memory-map placeholders the loader fills (BootRequest::place)
    pub ramdisk: bool,
    pub packed_ramdisk: bool,
    pub trust_cache: bool,
    pub gpu_addon: bool,
    pub root_policies: bool,
    /// Also publish the ramdisk as RAMDisk1: iOS 16 userlands look for it under that name as well as RAMDisk
    /// (port-a56, docs/a56.md). Off by default: a 27 kernel never sees the node, so its tree is unchanged.
    pub ramdisk1: bool,
    /// The device's own decoded device tree (the IPSW's DeviceTree payload), from which model, target-type, the
    /// board name and /product are taken; None gives a generic machine with model/product names from the strings
    /// below.
    pub identity: Option<Node>,
    /// "iPhone18,2"
    pub model: Option<String>,
    /// the root's target-type without an identity tree ("V54"; None: none)
    pub target_type: Option<String>,
    /// /product artwork-display-gamut, default "P3" (with an identity) or "sRGB"
    pub display_gamut: Option<String>,
    /// /product graphics-featureset-fallbacks keeps APPLE2 and MTL2,2 (false: both dropped, set_featuresets): MobileGestalt
    /// then answers DeviceSupportsASTC, and CoreUI and Core Animation hand ASTC images to the GPU instead of decoding
    /// them on the CPU. xnuhost's astc= (default 1) sets it; false here, the tree of the loader's own tests.
    pub astc: bool,
    /// The guest's display (0 = say nothing): the mode in pixels, the UI scale (pixels per point, 2 or 3: /chosen
    /// display-scale, which MobileGestalt answers main-screen-scale from, and /product artwork-scale-factor) and the
    /// panel's refresh rate. /chosen/xnuhost carries all four for the hosted kernel's display driver.
    pub display_width: u32,
    pub display_height: u32,
    pub display_scale: u32,
    pub display_hz: u32,
    /// The panel's rounded corners in points (0 = say nothing): /product display-corner-radius, two little-endian
    /// u32s, the radius and 1 (24A446's V54 tree has 62 and 1 on the 62-point Pro Max). iOS insets the status bar and
    /// masks the screen by it; without it the guest lays out for square corners and the phone's own corner cut clips
    /// the picture (the Galaxy S25's is 34: its stock ROM's framework overlay says rounded_corner_radius_top and
    /// _bottom = 34 dp, 102 px at the UI scale of 3).
    pub corner_radius: u32,
    /// /product artwork-device-subtype (0 = from the panel): 2340 on a 1080x2340 panel (at @3x that is the iPhone
    /// 12/13 mini's 360x780-point grid), else 2868 (the iPhone18,2's Pro Max set).
    pub artwork_subtype: u32,
    /// The cameras iOS is told about (CAMERA_*; 0 = none), used only with an "oneios,camera" device: /arm-io/isp
    /// camera-rear and camera-front (MobileGestalt's rear-facing-camera, front-facing-camera and still-camera, which
    /// SpringBoard shows Camera.app by), an empty /product/camera (video-camera), and /product unique-model
    /// (MobileGestalt's HWModelDescriptionForCamera: CMCapture reads its per-model tables from
    /// CMCapture.framework/<it less "AP">). docs/hardware.md section 7.
    pub cameras: u32,
    /// /product product-id when there is no identity tree: the 20 bytes iOS 16's MobileGestalt finds its product by
    /// (libMobileGestalt 20F66 copies IODeviceTree:/product product-id and looks it up in its table of 363 products; with
    /// none, MGGetProductType is -1, every MGIsDeviceOneOfType is false and bluetoothd's chip factory finds no chip).
    /// xnuhost gives the iPhone X's for an iOS 16.5 image set (docs/userland.md, "The iOS 16.5 identity"); iOS 27 reads
    /// none. With it, /product product-description is product-name too. None: no property, as before.
    pub product_id: Option<[u8; 20]>,
    /// The root's serial-number, as text: written in the layout XNU's IODTPlatformExpert (ArmDTPlatform's base) turns
    /// into IOPlatformExpertDevice IOPlatformSerialNumber, which MobileGestalt answers SerialNumber from (Settings >
    /// General > About). 11 characters of A-Z and 0-9 ([`serial_number_property`]). None: no property, as before.
    pub serial_number: Option<String>,
    /// /product unique-model with the cameras (xnuhost's camera-model=): None gives an identity tree's own, else
    /// [`CAMERA_MODEL`]. A name that AVFCapture's AVGestalt knows ("V54AP") makes VirtualAudio build a webcam subport
    /// it has no DSP chain for, and audio never starts (2026-11-23); [`CAMERA_MODEL`] is in no such table, and the
    /// hosted kernel (0088) shows CMCapture.framework/OneiOS as a link to V54.
    pub unique_model: Option<String>,
    /// NUL-separated list for /chosen text-fixups (None or empty: no property)
    pub text_fixups: Option<Vec<u8>>,
    /// xnuhost: where a hosted kernel finds the host (0 = not hosted, node omitted): the virtual address of the
    /// xh_vector
    pub host_vector: u64,
    /// physical range of the trapped MMIO aperture
    pub host_io_base: u64,
    pub host_io_size: u64,
    /// first address iOS tasks may not use
    pub user_va_limit: u64,
    pub host_page_size: u32,
}

impl Default for Machine {
    fn default() -> Self {
        Machine {
            ncpu: 0,
            cpu_reg: [0; MAX_CPUS],
            cpu_cluster: [Cluster::P; MAX_CPUS],
            timebase_hz: 0,
            dram_base: 0,
            dram_size: 0,
            view_size: 0,
            armio_base: 0,
            armio_size: 0,
            gic_dist: 0,
            gic_dist_size: 0,
            gic_redist: 0,
            gic_redist_size: 0,
            gic_redist_stride: 0,
            uart_base: 0,
            uart_size: 0,
            psci_method: None,
            devices: Vec::new(),
            cpufreq_base: 0,
            cpufreq_size: 0,
            gpu_pool_base: 0,
            gpu_pool_size: 0,
            media_pool_base: 0,
            media_pool_size: 0,
            bt_transport: 0,
            bt_speed: 0,
            bt_mac: [0; 6],
            panic_base: 0,
            panic_size: 0,
            random_seed: [0; 256],
            chip_id: None,
            root_volume_uuid: None,
            ramdisk: false,
            packed_ramdisk: false,
            trust_cache: false,
            gpu_addon: false,
            root_policies: false,
            ramdisk1: false,
            identity: None,
            model: None,
            target_type: None,
            display_gamut: None,
            astc: false,
            display_width: 0,
            display_height: 0,
            display_scale: 0,
            display_hz: 0,
            corner_radius: 0,
            artwork_subtype: 0,
            cameras: 0,
            unique_model: None,
            product_id: None,
            serial_number: None,
            text_fixups: None,
            host_vector: 0,
            host_io_base: 0,
            host_io_size: 0,
            user_va_limit: 0,
            host_page_size: 0,
        }
    }
}

fn adler32(p: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in p {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

fn chrp_header(at: &mut [u8], name: &str, size: usize) {
    at[..16].fill(0);
    at[0] = 0x70;
    at[2] = (size / 16) as u8;
    at[3] = ((size / 16) >> 8) as u8;
    at[4..4 + name.len()].copy_from_slice(name.as_bytes());
    let mut sum: u32 = at[..16].iter().map(|b| *b as u32).sum();
    while sum > 255 {
        sum = (sum & 255) + (sum >> 8);
    }
    at[1] = sum as u8;
}

/// A fresh volatile CHRP NVRAM bank for IODTNVRAM.
pub fn nvram_proxy() -> [u8; 8192] {
    let mut image = [0u8; 8192];
    chrp_header(&mut image, "nvram", 32);
    chrp_header(&mut image[32..], "common", 16 + 2048);
    let free_at = 32 + 16 + 2048;
    chrp_header(&mut image[free_at..], "free", 8192 - free_at);
    let sum = adler32(&image[20..]);
    image[16..20].copy_from_slice(&sum.to_le_bytes());
    image
}

/// The /product properties of the identity tree a hosted machine does not have: the exclaves of Apple's SoCs, and
/// the panel's gamut, which display_gamut sets.
fn product_dropped(name: &[u8]) -> bool {
    const DROPPED: [&str; 5] = ["name", "AAPL,phandle", "exclaves-enabled", "has-exclaves", "artwork-display-gamut"];
    DROPPED.iter().any(|d| d.as_bytes() == name)
}

/// graphics-featureset-fallbacks (a NUL-terminated, colon-separated list), without APPLE2 and MTL2,2 unless `astc`: with
/// them MobileGestalt answers DeviceSupportsASTC. They were dropped because SplashBoard wrote app snapshots in the AGX
/// GPU's own ASTC layout, which no other GPU decodes; NativeMetal's frontend now answers the layout itself (its
/// DVMLinearTextureLayout, whose watermark refuses data laid out for another GPU), and with ASTC on UniXNU-zizhan
/// measured 46.9 -> 88.6 moving fps (CA uploads ASTC images instead of ImageIO decoding them). xnuhost's astc=.
fn set_featuresets(product: &mut Node, name: &[u8], value: &[u8], astc: bool) {
    const OUT: usize = 512; // the C buffer: an entry that would not fit is left out
    let mut length = value.len();
    while length > 0 && value[length - 1] == 0 {
        length -= 1;
    }
    let mut out = Vec::with_capacity(OUT);
    if length > 0 {
        for item in value[..length].split(|b| *b == b':') {
            let drop = !astc && (item == b"APPLE2" || item == b"MTL2,2");
            if !drop && out.len() + item.len() + 2 < OUT {
                if !out.is_empty() {
                    out.push(b':');
                }
                out.extend_from_slice(item);
            }
        }
    }
    out.push(0);
    product.set(name, out).expect("an identity property name fits");
}

/// Sets a property of a fixed, short name (cannot fail).
trait Put {
    fn put(&mut self, name: &str, value: impl AsRef<[u8]>);
    fn put_u32(&mut self, name: &str, value: u32);
    fn put_u64(&mut self, name: &str, value: u64);
    fn put_str(&mut self, name: &str, value: &str);
}

impl Put for Node {
    fn put(&mut self, name: &str, value: impl AsRef<[u8]>) {
        self.set(name, value).expect("a fixed property name fits");
    }
    fn put_u32(&mut self, name: &str, value: u32) {
        self.put(name, value.to_le_bytes());
    }
    fn put_u64(&mut self, name: &str, value: u64) {
        self.put(name, value.to_le_bytes());
    }
    fn put_str(&mut self, name: &str, value: &str) {
        self.set_str(name, value).expect("a fixed property name fits");
    }
}

/// The root's serial-number for `serial`, 11 characters of A-Z and 0-9 (None for any other text): 32 bytes laid out
/// as XNU's IODTPlatformExpert::createSystemSerialNumberString reads a serial without a hyphen (iokit/Kernel/
/// IOPlatformExpert.cpp): IOPlatformSerialNumber = bytes 13..21 then bytes 0..3. So bytes 0..3 are its last three
/// characters, 13..21 its first eight, the rest zero (as on a Mac; a '-' anywhere would select the CTO layout).
pub fn serial_number_property(serial: &str) -> Option<[u8; 32]> {
    let s = serial.as_bytes();
    if s.len() != 11 || !s.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return None;
    }
    let mut p = [0u8; 32];
    p[..3].copy_from_slice(&s[8..]);
    p[13..21].copy_from_slice(&s[..8]);
    Some(p)
}

fn pair(a: u64, b: u64) -> [u8; 16] {
    let mut v = [0u8; 16];
    v[..8].copy_from_slice(&a.to_le_bytes());
    v[8..].copy_from_slice(&b.to_le_bytes());
    v
}

fn words(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

impl Machine {
    /// Fills the windows for `ncpu` CPUs and the given devices from the BOARD_* layout.
    pub fn board(&mut self, ncpu: u32, kinds: &[DeviceKind]) {
        self.ncpu = ncpu;
        // Affinity0, as crosvm numbers its vCPUs
        for (i, reg) in self.cpu_reg.iter_mut().enumerate().take(ncpu as usize) {
            *reg = i as u32;
        }
        self.armio_base = BOARD_ARMIO_BASE;
        self.armio_size = BOARD_ARMIO_SIZE;
        self.gic_dist = BOARD_GIC_DIST;
        self.gic_dist_size = BOARD_GIC_DIST_SIZE;
        self.gic_redist = BOARD_GIC_REDIST;
        self.gic_redist_stride = BOARD_GIC_STRIDE;
        self.gic_redist_size = BOARD_GIC_STRIDE * u64::from(ncpu.max(1));
        self.uart_base = BOARD_UART;
        self.uart_size = BOARD_UART_SIZE;
        self.psci_method = Some(Psci::Hvc);
        self.devices = kinds
            .iter()
            .take(MAX_DEVICES)
            .enumerate()
            .map(|(i, &kind)| Device {
                kind,
                base: BOARD_VIRTIO + i as u64 * BOARD_VIRTIO_SIZE,
                size: BOARD_VIRTIO_SIZE,
            })
            .collect();
        self.host_io_base = BOARD_IO_BASE;
        self.host_io_size = BOARD_IO_SIZE;
    }

    /// The guest's stable addresses, from its unique chip ID.
    pub fn mac(&self, which: Mac) -> [u8; 6] {
        const SALT: [u8; 3] = [0, 0x5a, 0x69];
        let seed = match self.chip_id {
            Some(c) if c.unique != 0 => c.unique,
            _ => 0x4f6e_6569,
        };
        // 02:d7 locally administered
        [0x02, 0xd7, (seed >> 24) as u8 ^ SALT[which as usize], (seed >> 16) as u8, (seed >> 8) as u8, seed as u8]
    }

    fn check(&self) -> Result<(), Error> {
        if self.serial_number.as_deref().is_some_and(|s| serial_number_property(s).is_none()) {
            return Err(Error("the serial number must be 11 characters of A-Z and 0-9"));
        }
        if self.ncpu == 0 || self.ncpu as usize > MAX_CPUS {
            return Err(Error("1 to 8 CPUs"));
        }
        if self.timebase_hz == 0 {
            return Err(Error("the timebase frequency is missing"));
        }
        if (self.dram_base | self.dram_size) & 0x3fff != 0 {
            return Err(Error("the RAM base and size must be 16 KiB aligned"));
        }
        if self.armio_base == 0 {
            return Err(Error("the arm-io base must not be 0"));
        }
        if self.ncpu > 1 && self.psci_method.is_none() {
            return Err(Error("more than one CPU needs a PSCI conduit"));
        }
        if self.gic_redist_size < self.gic_redist_stride * u64::from(self.ncpu) {
            return Err(Error("the redistributor window is smaller than one frame per CPU"));
        }
        let armio_end = self.armio_base + self.armio_size;
        let outside = |base: u64, size: u64| base < self.armio_base || base + size > armio_end;
        if outside(self.gic_dist, self.gic_dist_size) || outside(self.gic_redist, self.gic_redist_size) {
            return Err(Error("a GIC window lies outside the arm-io window"));
        }
        if self.devices.iter().any(|d| outside(d.base, d.size)) {
            return Err(Error("a device window lies outside the arm-io window"));
        }
        if self.panic_size != 0 && (self.panic_base | self.panic_size) & 0x3fff != 0 {
            return Err(Error("the panic region must be 16 KiB aligned"));
        }
        Ok(())
    }

    /// Builds the tree; fails for a machine that cannot be described.
    pub fn tree(&self) -> Result<Node, Error> {
        self.check()?;
        let id = self.identity.as_ref();
        let id_product = id.and_then(|i| i.child("product"));
        if let Some(i) = id {
            if id_product.is_none() || i.get("model").is_none() || i.get("target-type").is_none() {
                return Err(Error("the identity tree has no model, target-type or /product"));
            }
        }

        let mut root = Node::new("device-tree");
        let mut compatible = Vec::new();
        if let Some(i) = id {
            root.put("model", &i.get("model").unwrap().value);
            root.put("target-type", &i.get("target-type").unwrap().value);
            // The device's board name first: libMobileGestalt reads the first entry as the hardware model, and
            // CoreMotion, the audio server and MediaExperience key their per-board tables on it.
            if let Some(p) = i.get("compatible") {
                let n = xh_core::text::until_nul(&p.value).len();
                if n != 0 && n + 1 < 160 - 32 {
                    compatible.extend_from_slice(&p.value[..n]);
                    compatible.push(0);
                }
            }
        } else {
            root.put_str("model", self.model.as_deref().unwrap_or("oneios-xnuhost"));
            // The board (24A446's DeviceTree: target-type "V54" for the iPhone18,2). iOS names its jetsam properties
            // by it: without it runningboardd logs "Can't find jetsamproperties
            // ('/System/Library/LaunchDaemons/com.apple.jetsamproperties' | '' | 'plist')" (a PC boot, 2026-11-15)
            // and has none of com.apple.jetsamproperties.V54.plist's bands and limits.
            if let Some(t) = &self.target_type {
                root.put_str("target-type", t);
            }
        }
        compatible.extend_from_slice(b"oneios,xnuhost\0");
        root.put("compatible", compatible);
        // The serial number Settings shows (xnuhost: an iOS 16.5 image set's; a made-up value, never an Apple serial).
        if let Some(p) = self.serial_number.as_deref().and_then(serial_number_property) {
            root.put("serial-number", p);
        }
        root.put_u32("#address-cells", 2);
        root.put_u32("#size-cells", 2);

        self.chosen(root.add("chosen"))?;
        let cpus = root.add("cpus");
        for i in 0..self.ncpu as usize {
            let cpu = cpus.add(&format!("cpu{i}"));
            cpu.put_str("state", if i == 0 { "running" } else { "waiting" });
            cpu.put_u32("reg", self.cpu_reg[i]);
            cpu.put_str("device_type", "cpu");
            cpu.put_str("compatible", "arm,armv8");
            cpu.put_u32("timebase-frequency", self.timebase_hz);
            // An AMP kernel panics on a CPU of no cluster type, so every cpu node carries one.
            cpu.put_str("cluster-type", if self.cpu_cluster[i] == Cluster::E { "E" } else { "P" });
        }
        if self.ncpu > 1 {
            let psci = root.add("psci");
            psci.put_str("compatible", "arm,psci-1.0");
            psci.put_str("method", self.psci_method.unwrap().name());
        }
        if self.panic_size != 0 {
            root.add("pram").put("reg", pair(self.panic_base, self.panic_size));
        }
        self.defaults(root.add("defaults"));
        root.add("options");

        // bluetoothd finds its controller by name; transport-encoding 10 is the virtual transport.
        let bt = root.add("bluetooth");
        bt.put_str("device_type", "bluetooth");
        bt.put_u32("transport-encoding", 10);
        let bt_mac = if self.bt_transport != 0 {
            bt.put_u32("transport-encoding", self.bt_transport);
            if self.bt_speed != 0 {
                bt.put_u32("transport-speed", self.bt_speed);
            }
            self.bt_mac
        } else {
            self.mac(Mac::Bluetooth)
        };
        bt.put("local-mac-address", bt_mac);
        // The three addresses the userland's boot contract requires, each stable for one unique chip ID.
        let chosen = root.add("chosen");
        chosen.put("mac-address-bluetooth0", bt_mac);
        chosen.put("mac-address-wifi0", self.mac(Mac::Wifi));
        chosen.put("mac-address-ethernet0", self.mac(Mac::Ethernet));

        let buttons = root.add("buttons");
        buttons.put_str("compatible", "buttons");
        buttons.put_str("device_type", "buttons");
        buttons.put("button-names", b"hold\0voldown\0volup\0");
        buttons.put_u32("home-button-type", 2);

        let cameras = if self.devices.iter().any(|d| d.kind == DeviceKind::Camera) {
            self.cameras & (CAMERA_REAR | CAMERA_FRONT)
        } else {
            0
        };
        self.product(root.add("product"), id_product, cameras);
        self.arm_io(root.add("arm-io"), cameras)?;
        Ok(root)
    }

    fn chosen(&self, chosen: &mut Node) -> Result<(), Error> {
        chosen.put("random-seed", self.random_seed);
        if let Some(c) = self.chip_id {
            chosen.put_u64("unique-chip-id", c.unique);
            chosen.put_u32("chip-id", c.chip);
        }
        chosen.put_str("osenvironment", "main");
        chosen.put_u32("certificate-production-status", 0);
        chosen.put_u32("effective-production-status-ap", 0);
        chosen.put_u32("certificate-security-mode", 1);
        chosen.put_u32("effective-security-mode-sep", 1);
        let any_ramdisk = self.ramdisk || self.packed_ramdisk;
        if any_ramdisk {
            chosen.put_u32("ephemeral-storage", 1);
        }
        chosen.put_u64("dram-base", self.dram_base);
        chosen.put_u64("dram-size", self.dram_size);
        chosen.put_str("crypto-hash-method", "sha2-384");
        if let Some(uuid) = &self.root_volume_uuid {
            let uuid = uuid.split('\0').next().unwrap_or("");
            if uuid.len() > 39 {
                return Err(Error("root volume UUID too long"));
            }
            let upper: String =
                uuid.chars().map(|c| if ('a'..='f').contains(&c) { c.to_ascii_uppercase() } else { c }).collect();
            let matching = format!(
                "<dict><key>IOProviderClass</key><string>IOMedia</string><key>IOPropertyMatch</key>\
                 <dict><key>UUID</key><string>{upper}</string></dict></dict>"
            );
            chosen.put_str("root-matching", &matching);
        }
        chosen.put_u32("amfi-only-platform-code", 1);
        // What iBoot writes: 0 = nothing locks the kernel's text (no CTRR under a host or a hypervisor). xnu-12377
        // asserts that it was given before DTrace's SDT probes ask ml_unsafe_kernel_text (bsd_init -> sdt_init).
        chosen.put_u32("kernel-ctrr-to-be-enabled", 0);
        if self.panic_size != 0 {
            chosen.put_u32("embedded-panic-log-size", self.panic_size as u32);
        }
        if let Some(fixups) = self.text_fixups.as_ref().filter(|f| !f.is_empty()) {
            chosen.put("text-fixups", fixups);
        }
        let image = nvram_proxy();
        chosen.put_u32("nvram-bank-size", image.len() as u32);
        chosen.put_u32("nvram-bank-count", 1);
        chosen.put_u32("nvram-current-bank", 0);
        chosen.put("nvram-proxy-data", image);
        let map = chosen.add("memory-map");
        if any_ramdisk {
            map.put(if self.packed_ramdisk { "PackedRAMDisk" } else { "RAMDisk" }, [0u8; 16]);
        }
        // iOS 16's kernel reads the ramdisk's address from RAMDisk1 (port-a56, docs/a56.md); 27's never sees this
        // node.
        if self.ramdisk1 && any_ramdisk {
            map.put("RAMDisk1", [0u8; 16]);
        }
        for (wanted, name) in
            [(self.trust_cache, "TrustCache"), (self.gpu_addon, "GpuAddon"), (self.root_policies, "RootPolicies")]
        {
            if wanted {
                map.put(name, [0u8; 16]);
            }
        }
        chosen.add("manifest-properties");
        if self.display_scale != 0 {
            // The UI scale: MobileGestalt's main-screen-scale. The display driver publishes the same two words and
            // refuses to start its scale policy on a value that differs (LegacyDisplayProbe.cpp publishOrMatchWord).
            chosen.put_u32("display-scale", self.display_scale);
            chosen.put_u32("display-rotation", 0);
        }
        if self.host_vector != 0 {
            let host = chosen.add("xnuhost");
            host.put_u32("abi", xnuhost_abi::XH_ABI_VERSION);
            host.put_u64("vector", self.host_vector);
            host.put("io-aperture", pair(self.host_io_base, self.host_io_size));
            host.put_u64("user-va-limit", self.user_va_limit);
            host.put_u32("host-page-size", self.host_page_size);
            if self.display_width != 0 && self.display_height != 0 {
                host.put_u32("display-width", self.display_width);
                host.put_u32("display-height", self.display_height);
                host.put_u32("display-scale", self.display_scale);
                host.put_u32("display-hz", self.display_hz);
            }
        }
        Ok(())
    }

    fn defaults(&self, defaults: &mut Node) {
        defaults.put_u32("sleep-disabled", 1);
        // XNU publishes this as kern.hv_vmm_present; 24A446's bluetoothd then skips chip initialisation, and its
        // power init succeeds only with transport 10, its own simulated controller (docs/hardware.md 4, "The virtual
        // transport").
        defaults.put_u32("vmm-present", 1);
        // The vnode limit XNU sizes from memory, (bytes / 64 KiB) + 1024: without it the BCM2837 configuration clips
        // it to 1024 and the vnode jetsam kills every idle process.
        defaults.put_u32("kern.maxvnodes", (self.dram_size / 0x10000 + 1024) as u32);
        // The I/O throttle an iPhone18,2 has (XNU throttle_init_throttle_window/_period read these before any disk
        // opens). The hosted disks answer DKIOCISSOLIDSTATE 0 (`spec_open : ... isssd is (0)`), so XNU took its
        // rotating-disk table: a utility (tier 2) I/O slept 85 ms and a background (tier 3) I/O 200 ms, within 500 ms
        // of any foreground I/O on the same disk, while the iPhone's NAND gets the SSD table (5, 15, 25 ms) and its
        // DeviceTree /defaults (24A446) sets kern.io_throttle_period_tier3 20 and kern.io_throttle_window_tier3 100.
        // These are the iPhone's effective values, whatever the disk answers; the tier 1 and tier 2 windows (25, 100
        // ms) are XNU's on both.
        defaults.put_u32("kern.io_throttle_period_tier1", 5);
        defaults.put_u32("kern.io_throttle_period_tier2", 15);
        defaults.put_u32("kern.io_throttle_period_tier3", 20);
        defaults.put_u32("kern.io_throttle_window_tier3", 100);
        // ram-view= (xnuhost): the per-process memory limits an iPhone18,2 has, from its own device tree (24A446's
        // DeviceTree /defaults: kern.max_task_pmem 3376, kern.entitled_max_task_pmem 6144,
        // kern.entitled_dev_max_task_pmem 6656 MB, for its 12 GiB), scaled by the view. XNU reads them in task_init
        // (max_task_footprint) and memorystatus_init (the entitled raise of apps with
        // com.apple.developer.kernel.increased-memory-limit). A footprint counts the task's compressed pages too
        // (phys_footprint), so these are limits on what the compressor holds as well. Without the view: the
        // iPhone18,2's own default task limit (kern.max_task_pmem 3376 MB; before 2026-11-15 the tree had none, so a
        // task with no limit of its own from launchd or RunningBoard had none at all) and no entitled raise, as
        // before.
        if self.view_size == 0 {
            defaults.put_u32("kern.max_task_pmem", 3376);
        } else {
            const LIMITS: [(&str, u64); 3] = [
                ("kern.max_task_pmem", 3376),
                ("kern.entitled_max_task_pmem", 6144),
                ("kern.entitled_dev_max_task_pmem", 6656),
            ];
            for (name, mb) in LIMITS {
                defaults.put_u32(name, (mb * (self.view_size >> 20) / (12 << 10)) as u32);
            }
        }
    }

    fn product(&self, product: &mut Node, id_product: Option<&Node>, cameras: u32) {
        if let Some(idp) = id_product {
            for p in idp.props.iter().filter(|p| !product_dropped(&p.name)) {
                if p.name == b"graphics-featureset-fallbacks" {
                    set_featuresets(product, &p.name, &p.value, self.astc);
                } else {
                    product.set(&p.name, &p.value).expect("a parsed property name fits");
                }
            }
            product.put_str("artwork-display-gamut", self.display_gamut.as_deref().unwrap_or("P3"));
            let audio = product.add("audio");
            audio.put_u32("wireless-splitter", 1);
            if let Some(acoustic) = idp.child("audio").and_then(|a| a.get("acoustic-id")) {
                audio.put("acoustic-id", &acoustic.value);
            }
        } else {
            product.put_str("product-name", "OneiOS guest");
            // iOS 16's product (xnuhost, an iOS 16.5 image set): MobileGestalt finds it by product-id and reads the
            // marketing name and the description from /product (product-name, product-description); iOS 27 reads no
            // product-id, so its tree stays as it was.
            if let Some(id) = &self.product_id {
                product.put("product-id", id);
                product.put_str("product-description", "OneiOS guest");
            }
            // No identity tree (the phone's image set has none): the iPhone18,2 answers that CoreUI asks
            // MobileGestalt for (kMGArtworkDeviceIdiom, kMGArtworkDisplayGamut, kMGArtworkGraphicsFeatureSetClass, the
            // subtype). Without them every app logged `CUICatalog: couldn't get 'kMGArtworkDeviceIdiom' from
            // MGCopyAnswer got '{ArtworkDeviceScaleFactor = 3}'`, its asset catalogs chose no image, and UIKit probed
            // loose files for each image on every redraw (_UIImageCollectImagePathsForPath: about 6000 faccessat a
            // second in Spotlight, Notes and WebSheet, 105% of a core, keys waiting for the main thread; the phone,
            // 2026-11-22). The values are 24A446's V54 device tree's /product, with this host's choices: the gamut is
            // the display's (sRGB unless display_gamut says so: the helper tags the frames sRGB), and the GPU
            // fallbacks lose APPLE2 and MTL2,2 as with an identity unless astc (set_featuresets).
            product.put_str("artwork-device-idiom", "phone");
            // The subtype names the artwork and the layout metrics: the panel's own (2340 for a 1080x2340 panel, the
            // iPhone 12/13 mini's grid) unless the host says otherwise; 2868 (the iPhone18,2's Pro Max) for every
            // other panel, as before.
            let subtype = if self.artwork_subtype != 0 {
                self.artwork_subtype
            } else if (self.display_width, self.display_height) == (1080, 2340) {
                2340
            } else {
                2868
            };
            product.put_u32("artwork-device-subtype", subtype);
            product.put_str("artwork-display-gamut", self.display_gamut.as_deref().unwrap_or("sRGB"));
            product.put_str("artwork-dynamic-displaymode", "0");
            product.put_str("graphics-featureset-class", "APPLE10");
            set_featuresets(
                product,
                b"graphics-featureset-fallbacks",
                b"APPLE9:APPLE8:APPLE7:APPLE6:APPLE5:APPLE4:APPLE3:APPLE3v1:APPLE2:APPLE1:GLES2,0\0",
                self.astc,
            );
        }
        // The artwork scale follows the UI scale (the identity tree's is the iPhone panel's 3). The artwork subtype is
        // the identity's own, or (no identity) the panel's: 2340 on a 1080x2340 panel (the iPhone 12/13 mini's grid),
        // else 2868 (the iPhone18,2's). docs/userland.md, "The artwork answers".
        if self.display_scale != 0 {
            product.put_u32("artwork-scale-factor", self.display_scale);
        }
        // The panel's corners, in points: two little-endian u32s (the radius, then 1, as the V54 tree has 62 and 1 on
        // the Pro Max). iOS insets the status bar and masks the screen by it; an identity tree's own value stays
        // unless corner-radius= was given (xnuhost's config.corner_radius, the host phone's row).
        if self.corner_radius != 0 {
            let mut v = [0u8; 8];
            v[..4].copy_from_slice(&self.corner_radius.to_le_bytes());
            v[4..].copy_from_slice(&1u32.to_le_bytes());
            product.put("display-corner-radius", v);
        }
        product.put_str("model", "oneios,xnuhost");
        // The cameras (with the camera device only, so a host without one keeps the tree it had). 24A446's
        // libMobileGestalt answers video-camera from /product/camera being there (no property of it is needed;
        // iOSPlus10 adds it empty), and HWModelDescriptionForCamera (= ...ForAnalytics, ...ForAudio, ...ForPowerPerf,
        // ...ForAutomatedTesting) from /product unique-model. CMCapture's FigCaptureGetModelSpecificName takes that
        // answer less "AP" as the folder of its per-model tables (CMCapture.framework/V54/AVCaptureSession.plist,
        // CameraSetup.plist, ...); without it the name was NULL and every built-in source failed (iOS's log,
        // 2026-11-22: Fig assert "modelSpecificAVCaptureSessionPlist" at FigCaptureSourceBackingsProvider.m:2316, err
        // -12780, "Created 0 in-memory built-in backings").
        // The name is not the iPhone18,2's own "V54AP" by default: AVFCapture's AVGestalt (in audiomxd too) finds V54 in
        // its table of 41 iPhone models and answers its webcam key true, VirtualAudio's RoutingSettings_N71 then builds
        // a webcam subport whose DSP list exists only with input processing on (here it is off), its precondition
        // fails, and VirtualAudio never starts: no speaker, no volume, no haptics (iOS's log, v0.1.16, 2026-11-23: "Need
        // valid DSPConfig for webcam recording", VA Init Status: 1). CAMERA_MODEL ("OneiOSAP") is in no such table, so
        // AVGestalt answers as with no name, and the hosted kernel (0088) shows CMCapture.framework/OneiOS as a link to
        // the sealed V54 folder. self.unique_model (xnuhost's camera-model=) wins; without it an identity tree's
        // /product keeps its own.
        if cameras != 0 {
            product.add("camera");
            if self.unique_model.is_some() || product.get("unique-model").is_none() {
                product.put_str("unique-model", self.unique_model.as_deref().unwrap_or(CAMERA_MODEL));
            }
        }
    }

    fn arm_io(&self, io: &mut Node, cameras: u32) -> Result<(), Error> {
        let reg = |n: &mut Node, base: u64, size: u64| n.put("reg", pair(base - self.armio_base, size));
        io.put_str("device_type", "arm-io");
        io.put_u32("#address-cells", 2);
        io.put_u32("#size-cells", 2);
        io.put("ranges", words(&[0, self.armio_base, self.armio_size]));
        let intc = io.add("intc");
        intc.put_str("compatible", "arm,gic-v3");
        intc.put(
            "reg",
            words(&[
                self.gic_dist - self.armio_base,
                self.gic_dist_size,
                self.gic_redist - self.armio_base,
                self.gic_redist_size,
            ]),
        );
        intc.put_u64("redistributor-stride", self.gic_redist_stride);
        // launchd loads wifid only where the tree has a node named wlan; no driver matches it.
        io.add("wlan").put_str("device_type", "wlan");
        // MobileGestalt answers rear-facing-camera from IODeviceTree:/arm-io/isp camera-rear, front-facing-camera from
        // its camera-front, and still-camera (Camera.app's SBAppTags: SpringBoard shows the app only with it) from
        // either. The iPhone18,2's values are 1 (u32). No compatible and no device_type: no driver matches the node
        // (the camera's driver is drivers/camera on /arm-io/camera, and CMCapture finds it by the AppleH9CamIn class,
        // not by this node).
        if cameras != 0 {
            let isp = io.add("isp");
            if cameras & CAMERA_REAR != 0 {
                isp.put_u32("camera-rear", 1);
            }
            if cameras & CAMERA_FRONT != 0 {
                isp.put_u32("camera-front", 1);
            }
        }

        let (mut blocks, mut inputs) = (0, 0);
        for (i, d) in self.devices.iter().enumerate() {
            let (name, compat) = match d.kind {
                DeviceKind::Block => {
                    blocks += 1;
                    (format!("virtio-mmio-{}", blocks - 1), "virtio,mmio")
                }
                DeviceKind::DataStore => ("data-store".into(), "droidvm,data-store"),
                DeviceKind::Input => {
                    inputs += 1;
                    (format!("input-{}", inputs - 1), "droidvm,virtio-input")
                }
                DeviceKind::Net => ("net".into(), "droidvm,net"),
                DeviceKind::Gpu => ("virtio-gpu".into(), "droidvm,virtio-gpu"),
                DeviceKind::Snd => ("virtio-snd".into(), "droidvm,virtio-snd"),
                DeviceKind::Bridge => ("bridge".into(), "oneios,bridge"),
                DeviceKind::Camera => ("camera".into(), "oneios,camera"),
                DeviceKind::Media => ("virtio-media-0".into(), "droidvm,virtio-media"),
            };
            if io.child(&name).is_some() {
                return Err(Error("two devices of a kind that has one node"));
            }
            let n = io.add(&name);
            n.put_str("compatible", compat);
            reg(n, d.base, d.size);
            n.put("dma-coherent", b"");
            // The line of the window on the board's GIC: SPI INTID 32 + its index, under the name UniXNU-droidvm's
            // transports read. Not "interrupts": the tree has no interrupt-controller node (ArmCpu owns the GIC), and
            // IOKit's IODTMapOneInterrupt walks the parents of a node with "interrupts" forever looking for one.
            n.put_u32("droidvm,intid", BOARD_VIRTIO_SPI + i as u32);
            if d.kind == DeviceKind::Gpu && self.gpu_pool_size != 0 {
                n.put("drm-host-pool", pair(self.gpu_pool_base, self.gpu_pool_size));
            }
            if d.kind == DeviceKind::Media && self.media_pool_size != 0 {
                n.put("media-host-pool", pair(self.media_pool_base, self.media_pool_size));
            }
        }
        if self.cpufreq_size != 0 {
            let freq = io.add("cpufreq");
            freq.put_str("compatible", "oneios,cpufreq");
            reg(freq, self.cpufreq_base, self.cpufreq_size);
        }
        // XNU's PL011 console finds the node named "uart0" at soc_base + reg.
        if self.uart_size != 0 {
            let uart = io.add("uart0");
            uart.put_str("compatible", "arm,pl011");
            reg(uart, self.uart_base, self.uart_size);
        }
        Ok(())
    }
}

const FNV_PRIME: u64 = 0x100_0000_01b3;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv(mut h: u64, data: &[u8]) -> u64 {
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// What changes from boot to boot, or with the RAM xnuhost got this time: never part of the identity.
fn boot_to_boot(name: &[u8]) -> bool {
    const NAMES: [&str; 6] =
        ["random-seed", "dram-base", "dram-size", "drm-host-pool", "media-host-pool", "oneios-identity"];
    NAMES.iter().any(|n| n.as_bytes() == name)
}

/// A node's properties (less boot_to_boot and, with `prefix`, only those that start with it) and, with `deep`, its
/// children's, in the tree's order.
fn hash_node(mut h: u64, n: &Node, deep: bool, prefix: Option<&str>) -> u64 {
    let hashed = |p: &&Prop| !boot_to_boot(&p.name) && prefix.is_none_or(|x| p.name.starts_with(x.as_bytes()));
    for p in n.props.iter().filter(hashed) {
        h = fnv(h, &p.name);
        h = fnv(h, &(p.value.len() as u32).to_ne_bytes());
        h = fnv(h, &p.value);
    }
    if deep {
        h = fnv(h, &(n.children.len() as u32).to_ne_bytes());
        for c in &n.children {
            h = hash_node(h, c, true, None);
        }
    }
    h
}

/// The identity MobileGestalt answers from and keeps in its cache, as one number: FNV-1a 64 (names, lengths and
/// values, in the tree's order) of the root's own properties, the /product, /arm-io, /buttons and /bluetooth subtrees,
/// /chosen's own properties and /chosen/xnuhost's display-* ones. Nothing that changes from boot to boot is in it: not
/// random-seed, dram-base, dram-size, the GPU and media pools, /chosen's memory map, xnuhost's vector, /defaults (its
/// limits follow the RAM), nor oneios-identity itself. Call it on the finished tree (after a shipped tree's nodes are
/// merged).
pub fn identity_hash(tree: &Node) -> u64 {
    let mut h = hash_node(FNV_OFFSET, tree, false, None); // the root's own properties
                                                          // Each part with a marker byte first, so that a missing node and an empty one differ.
    for part in ["product", "arm-io", "buttons", "bluetooth"] {
        let n = tree.child(part);
        h = fnv(h, if n.is_some() { b"+" } else { b"-" });
        if let Some(n) = n {
            h = hash_node(h, n, true, None);
        }
    }
    // /chosen's own properties (the chip ID, the MAC addresses, display-scale, the root's volume, ...) and the display
    // mode of /chosen/xnuhost; not its memory map (addresses of this boot) nor the host vector (xnuhost's own ASLR).
    let chosen = tree.child("chosen");
    let host = chosen.and_then(|c| c.child("xnuhost"));
    h = fnv(h, if chosen.is_some() { b"+" } else { b"-" });
    if let Some(c) = chosen {
        h = hash_node(h, c, false, None);
    }
    h = fnv(h, if host.is_some() { b"+" } else { b"-" });
    if let Some(x) = host {
        h = hash_node(h, x, false, Some("display-"));
    }
    h
}

/// [`identity_hash`], also written as /chosen oneios-identity (8 bytes): hosted 0084 removes MobileGestalt's cache
/// only when it changed.
pub fn identity(tree: &mut Node) -> u64 {
    let h = identity_hash(tree);
    tree.add("chosen").put_u64("oneios-identity", h);
    h
}
