// SPDX-License-Identifier: MIT
//! main.c's boot(): the kernel read, the RAM's layout (the window, the GPU pool, the media pool, the boot
//! framebuffer), the board's devices (GIC, console, disks, input, the pen, the display and GPU, the bridges), the
//! device tree, the kernel placed in its window. The first vCPU is started by the session after this.

use xh_core::{loge, logi};
use xnuhost_abi::{XH_FEATURE_PAC, XH_FEATURE_TASKS};
use xnuloader::machine::{self as xm, ChipId, DeviceKind, Machine};
use xnuloader::{BootRequest, Kernel, Node};

use super::bridges::Bridges;
use super::memory::MemoryWatch;
use super::{read_file, BootFailed, Session};
use crate::blk::Blk;
use crate::config::{self, Config, Encoder, Gpu, Pen};
use crate::imageset::{DiskIdentity, ImageSet, Verify};
use crate::ramfix::{self, Huge};
use crate::{
    cpu, display, gic, gpu3d, gpu3d_amdgpu, hostcalls, input, input_script, mem, pen, sysreg, task, uart, virtio_blk,
};
use crate::{motion, virtio_gpu, virtio_input};
use onebridges::sensors;

/// The pool behind /arm-io/virtio-gpu drm-host-pool, as Droid-VM's boards size it (--pre-alloc drm-host-mb=8):
/// drm2kgsl keeps its first 2 MiB unused, the rest holds 16 KiB of response buffer per 3D context.
const GPU_POOL_BYTES: u64 = 8 << 20;
/// The kext whose classes have the names of the kernel's motion classes (AppleSPUHIDDevice, AppleSPUHIDDeviceUserClient).
/// v0.1.29 panicked at kernel start with it in the collection (2026-11-25: "Kext com.apple.driver.AppleSPU class
/// AppleSPUHIDDevice is a duplicate"): its AppleSPUGestureDriver matched the motion devices and IOKit loaded it. `prelink
/// roots` leaves it out since (FORCE_OUT, with the kexts that link it); a collection prelinked before still has it.
const SPU_KEXT: &str = "com.apple.driver.AppleSPU";

/// Whether the sensor hub's accelerometer may become the guest's motion device (and `oneios-motion=7|15` a boot argument):
/// only when the collection has no com.apple.driver.AppleSPU member. An MH_EXECUTE kernel has no kexts at all.
fn motion_allowed(kernel: &Kernel) -> bool {
    !kernel.entries.iter().any(|e| e.name == SPU_KEXT)
}

/// The video encoder's media pool (runtime/bridges/encoder): the V4L2 OUTPUT (NV12) and CAPTURE (bitstream) buffers
/// the guest maps. 48 MiB fits 1080p-class widget/screenshot snapshots across the device's session slots; with the 8
/// MiB GPU pool it stays under the hosted kernel's 64 MiB mappable-pool bound (the RAM layout below).
pub const MEDIA_POOL_BYTES: u64 = 48 << 20;

/// Where the kernel was placed: the first vCPU's start.
pub struct Placed {
    pub entry_va: u64,
    pub args_va: u64,
}

/// The board's devices in the order of their windows (the device tree's kinds).
pub struct Slots {
    pub kinds: Vec<DeviceKind>,
}

impl Slots {
    /// The next device's window and interrupt.
    pub fn next(&self) -> (u64, u32) {
        let n = self.kinds.len();
        (xm::BOARD_VIRTIO + n as u64 * xm::BOARD_VIRTIO_SIZE, xm::BOARD_VIRTIO_SPI + n as u32)
    }
    pub fn add(&mut self, kind: DeviceKind) {
        self.kinds.push(kind);
    }
    pub fn full(&self) -> bool {
        self.kinds.len() >= xm::MAX_DEVICES
    }
}

/// A word added to the boot arguments unless they name it already, within boot-args' room. Err: no room.
pub fn add_boot_arg(args: &mut String, word: &str) -> Result<(), config::NoRoom> {
    config::add_boot_args(args, config::BOOT_ARGS_SIZE, word)
}

/// The guest's unique chip ID: made once, kept with the guest's data, so its MAC addresses and everything keyed on the
/// ID stay the same across sessions.
fn chip_id(data: &str) -> Option<ChipId> {
    if data.is_empty() {
        return None;
    }
    let path = format!("{data}/unique-chip-id");
    let unique = match read_file(&path, 64) {
        Some(saved) if saved.len() == 8 => u64::from_le_bytes(saved[..8].try_into().expect("8 bytes")),
        _ => {
            let mut bytes = [0u8; 8];
            // SAFETY: getentropy fills the 8 bytes it is given.
            if unsafe { libc::getentropy(bytes.as_mut_ptr() as *mut libc::c_void, 8) } != 0 {
                return None;
            }
            let unique = u64::from_le_bytes(bytes) & 0x0000_ffff_ffff_ffff;
            let _ = write_private(&path, &unique.to_le_bytes());
            unique
        }
    };
    (unique != 0).then_some(ChipId { unique, chip: 0x8150 })
}

/// A file only this user reads, replaced whole.
fn write_private(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?.write_all(data)
}

/// A sidecar of the kernel collection, DarwinBoot's naming: <kernel>.<suffix>.
fn sidecar(kernel: &str, suffix: &str, limit: usize) -> Option<Vec<u8>> {
    let path = format!("{kernel}.{suffix}");
    let data = read_file(&path, limit)?;
    logi!("sidecar {}: {} bytes", path, data.len());
    Some(data)
}

/// A device tree shipped beside the kernel replaces the generated one node by node at the top level, except the nodes
/// that describe this machine; its /chosen properties are added to ours, the machine's own stay. As main.c did, a
/// node is copied with its properties and its children's properties (not deeper).
fn merge_shipped(tree: &mut Node, other: &Node) {
    const OURS: [&str; 5] = ["cpus", "arm-io", "psci", "pram", "chosen"];
    for child in &other.children {
        let name = String::from_utf8_lossy(child.name()).into_owned();
        if OURS.contains(&name.as_str()) {
            continue;
        }
        tree.remove(&name);
        let slot = tree.add(&name);
        for p in &child.props {
            let _ = slot.set(&p.name, &p.value);
        }
        for sub in &child.children {
            let into = slot.add(&String::from_utf8_lossy(sub.name()));
            for p in &sub.props {
                let _ = into.set(&p.name, &p.value);
            }
        }
    }
    if let Some(theirs) = other.child("chosen") {
        let chosen = tree.add("chosen");
        for p in &theirs.props {
            if chosen.get(&p.name).is_none() {
                let _ = chosen.set(&p.name, &p.value);
            }
        }
    }
}

impl Session {
    // ---- the image set (imageset.rs): which disk is which volume, and the checks on what the guest reads ----------

    fn image_set_open(&mut self, config: &Config) -> Result<(), BootFailed> {
        match ImageSet::load(&config.image) {
            Err(e) => {
                loge!("image set {}: {}", config.image, e);
                Err(BootFailed)
            }
            Ok(None) => {
                logi!(
                    "image set: no manifest.json in {}: its disks are not verified and carry no identity",
                    config.image
                );
                Ok(())
            }
            Ok(Some(set)) => {
                // The seed is unpacked by the kernel (0004: its node carries the hash); no helper disk.
                self.seed_sha256 = set.role("data-seed").map(|s| crate::imageset::hex(&s.sha256)).unwrap_or_default();
                self.image_set = Some(set);
                Ok(())
            }
        }
    }

    /// The identity of disk `i` in its device tree node; a read-only image of the set gets its chunk checks.
    fn image_set_disk(&mut self, config: &Config, i: usize, disk: &Blk) -> Result<(), BootFailed> {
        let Some(set) = &self.image_set else { return Ok(()) };
        let c = &config.disks[i];
        if c.data_store {
            self.disk_identity[i] = DiskIdentity {
                role: "data".into(),
                volume_uuid: set.data_uuid.clone(),
                volume_name: set.data_name.clone(),
                sha256_hex: self.seed_sha256.clone(),
                ..DiskIdentity::default()
            };
            return Ok(());
        }
        let Some(f) = set.find(&c.path) else { return Ok(()) };
        if c.writable {
            return Ok(());
        }
        match f.role.as_str() {
            "os" | "systemos" | "app" => {
                let verify = Verify::open(set, f).map_err(|e| {
                    loge!("disk {}: {}", c.path, e);
                    BootFailed
                })?;
                if disk.bytes() != f.size {
                    loge!("disk {}: {} bytes, manifest.json lists {}", c.path, disk.bytes(), f.size);
                    return Err(BootFailed);
                }
                let digest = *verify.digest();
                disk.set_verify(verify);
                self.disk_identity[i] = DiskIdentity {
                    role: f.role.clone(),
                    verified: Some(digest),
                    volume_uuid: f.volume_uuid.clone(),
                    ..DiskIdentity::default()
                };
            }
            "data-seed" => {
                self.disk_identity[i] = DiskIdentity {
                    role: "seed".into(),
                    bytes: f.size,
                    sha256_hex: self.seed_sha256.clone(),
                    ..DiskIdentity::default()
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// main.c's boot(): everything up to the kernel placed in its window.
    pub fn boot(&mut self, config: &mut Config) -> Result<Placed, BootFailed> {
        let image = read_file(&config.kernel, 1 << 31).ok_or_else(|| {
            loge!(
                "cannot read the kernel collection {}: {}",
                config.kernel,
                xh_core::sys::strerror(xh_core::sys::errno())
            );
            BootFailed
        })?;
        let kernel = Kernel::parse(&image).map_err(|e| {
            loge!("{}: {}", config.kernel, e);
            BootFailed
        })?;
        logi!(
            "kernel: {}, {} {}, link base {:#x}, {} segments, {} members",
            config.kernel,
            if kernel.fileset { "collection" } else { "MH_EXECUTE" },
            if kernel.cpusubtype & 0xff_ffff == 2 { "arm64e" } else { "arm64" },
            kernel.base,
            kernel.segments.len(),
            kernel.entries.len()
        );
        if kernel.cpusubtype & 0xff_ffff == 2 && hostcalls::info().features & XH_FEATURE_PAC == 0 {
            loge!("an arm64e kernel needs pointer authentication with keys shared across fork; this host has none");
            return Err(BootFailed);
        }

        // RAM: the kernel's window, then the GPU's pool (the memory of the renderer's that the guest maps: each 3D
        // context's response buffer), then the video encoder's media pool (its V4L2 OUTPUT/CAPTURE buffers), then the
        // boot framebuffer the kernel's console draws into. The GPU pool and the media pool both lie between the
        // window's end and the framebuffer, which the hosted kernel lets a task map (0024-gpu-pool.patch,
        // hosted_pa_in_gpu_pool, up to 64 MiB): GPU_POOL_BYTES + MEDIA_POOL_BYTES must stay under that.
        let (fb_width, fb_height) = (self.fb.0, self.fb.1);
        let fb_bytes = (fb_width as u64 * fb_height as u64 * 4 + 0x1f_ffff) & !0x1f_ffff;
        // An amdgpu host without KGSL (the Galaxy A56) under gpu=auto whose kernel's gpuaddon has no A56 plug-in is laid
        // out as gpu=2d: no GPU pool, the window and device tree of today's 2D session (gpu3d_amdgpu::early_2d). The
        // addon read here is kept for the GPU's choice and the kernel's sidecar below.
        let node = if config.gpu_node.is_empty() { gpu3d_amdgpu::DEFAULT_NODE } else { config.gpu_node.as_str() };
        let kgsl = std::path::Path::new("/dev/kgsl-3d0").exists();
        let mut addon_read: Option<Option<Vec<u8>>> = None;
        let early_2d = gpu3d_amdgpu::early_2d(
            config.gpu == Gpu::Auto,
            kgsl,
            !kgsl && config.gpu == Gpu::Auto && gpu3d_amdgpu::node_check(&gpu3d_amdgpu::System, node).is_ok(),
            || {
                let addon = sidecar(&config.kernel, "gpuaddon", 64 << 20);
                let found = gpu3d_amdgpu::plugin_in_addon(addon.as_deref(), None);
                addon_read = Some(addon);
                found
            },
        );
        if let Some(why) = &early_2d {
            logi!("gpu: 2D only: {}", why);
        }
        let pool_bytes = if config.gpu != Gpu::TwoD && early_2d.is_none() { GPU_POOL_BYTES } else { 0 };
        let media_bytes = if config.encoder != Encoder::Off { MEDIA_POOL_BYTES } else { 0 };
        let window = config.ram_bytes - fb_bytes - pool_bytes - media_bytes;
        // 2 MiB host pages for the RAM where this kernel allows them (fix-ram-hugepages; ramfix huge_choose): shmem THP
        // by madvise, only with the RAM held fixed (a give-back's pieces would split them).
        let mut huge = ramfix::huge_choose("", config.ram_huge && !config.ram_giveback);
        if mem::map(kernel.base, huge.mode == Huge::Shmem).is_err()
            || mem::io(xm::BOARD_IO_BASE, xm::BOARD_IO_SIZE).is_err()
        {
            return Err(BootFailed);
        }
        let m = mem::state();
        if huge.mode == Huge::Shmem && !m.huge {
            huge.why = "no (madvise(MADV_HUGEPAGE) refused)".into(); // the line above it says why (qemu-user refuses)
        }
        logi!(
            "memory: 2 MiB host pages: {}; shmem_enabled {}, hpage_pmd_size {}, hugetlb pool {} free of {} KiB pages; the memfd's offset 0 is at {:#x}, {} KiB below the window",
            if m.huge { "yes, the window is mapped with MADV_HUGEPAGE (shmem THP)" } else { huge.why.as_str() },
            if huge.shmem_enabled.is_empty() { "?" } else { huge.shmem_enabled.as_str() },
            huge.pmd_size,
            huge.hugetlb_free,
            huge.hugetlb_size >> 10,
            m.fd_phys,
            m.head >> 10
        );
        let phys = m.phys; // physical = where it is mapped (mem.rs)
        let media_pool_pa = phys + window + pool_bytes; // after the GPU pool, before the framebuffer
                                                        // A direct display helper copies frames from the memfd: it maps the file from offset 0, which is m.head bytes
                                                        // below the window (mem.rs, fix-ram-hugepages), so the address of offset 0 is fd_phys, not phys. With phys,
                                                        // every page of a frame was read 16 KiB too low, from another guest page: the screen showed stripes.
        display::set_ram(m.fd, m.fd_phys, m.head + m.size);
        self.boot_fb_pa = phys + window + pool_bytes + media_bytes;
        if mem::kva().is_ok() {
            let m = mem::state();
            hostcalls::update_info(|i| {
                i.kva_base = m.kva;
                i.kva_size = m.kva_size;
            });
        }
        logi!(
            "memory: {} MiB at {:#x}, physical = virtual (slide {:#x}); I/O aperture {:#x}",
            window >> 20,
            phys,
            (m.host as u64).wrapping_sub(kernel.base),
            m.io_host as u64
        );
        // ram-view=SIZE (docs/booter.md 2.2, "The memory view"): iOS is told SIZE of DRAM over this same RAM. XNU
        // cannot manage more pages than the RAM has, and a page above the locked RAM would be the host's to swap. So
        // the RAM stays what it is; the view is boot_args memSizeActual (hw.memsize_physical), the per-process limits
        // of an iPhone18,2 of that size in /defaults (machine.rs), and the compressor's room to hold that much here.
        let view = if config.ram_view > window { config.ram_view } else { 0 };
        if config.ram_view != 0 && view == 0 {
            loge!(
                "memory: ram-view={} MiB is not more than the guest's RAM ({} MiB): off",
                config.ram_view >> 20,
                window >> 20
            );
        }
        let Ok(view_multiplier) = config::view_boot_args(&mut config.boot_args, config::BOOT_ARGS_SIZE, view, window)
        else {
            loge!("no room in the boot arguments for ram-view's compressor setting");
            return Err(BootFailed);
        };
        // The view is meant over locked RAM (the owner's rule: iOS's RAM is never swapped): with ram=auto and the lock
        // that is the most that fits locked (config.ram_lock_fit). A bigger or unlocked RAM under a view is said.
        if view != 0 && (!config.ram_lock || config.ram_lock_fit == 0 || config.ram_bytes > config.ram_lock_fit) {
            loge!(
                "memory: ram-view over a RAM that is not all locked ({} MiB asked, {} MiB fits locked{}): the host may swap iOS's RAM; ram=auto with the lock (the default) is the mode",
                config.ram_bytes >> 20,
                config.ram_lock_fit >> 20,
                if config.ram_lock { "" } else { ", ram-lock off" }
            );
        }
        if view != 0 {
            logi!(
                "memory: ram-view: iOS is told {} MiB of DRAM (hw.memsize_physical, and an iPhone18,2's per-process limits for it) over {} MiB of RAM (hw.memsize); its compressor may hold up to {} x the RAM's pages (kern.compressor_pool_multiplier) in that RAM",
                view >> 20,
                window >> 20,
                view_multiplier
            );
        }
        self.memory = MemoryWatch::default();
        self.memory.ram_fix_start(config);

        // The board: GIC, console, disks, input, display.
        let mut slots = Slots { kinds: Vec::new() };
        gic::init(config.ncpu as usize, xm::BOARD_GIC_DIST, xm::BOARD_GIC_REDIST);
        gic::map();
        uart::init(xm::BOARD_UART);
        self.image_set_open(config)?;
        // iOS 16.5 (20F66, the Galaxy A56's userland): the kernel's 16.5 switches the image set's xnuhost.conf may not
        // name yet (kernel 0103-0105, config::IOS16_BOOT_ARGS), so an image made before them needs no remake.
        if self.image_set.as_ref().is_some_and(|s| s.build == "20F66") {
            match config::ios16_boot_args(&mut config.boot_args, config::BOOT_ARGS_SIZE) {
                Ok(added) if added.is_empty() => {
                    logi!("ios16: image set 20F66: the boot arguments name every iOS 16.5 switch already")
                }
                Ok(added) => logi!("ios16: image set 20F66: added to the boot arguments: {}", added),
                Err(_) => {
                    loge!("ios16: image set 20F66: no room in the boot arguments for the iOS 16.5 switches");
                    return Err(BootFailed);
                }
            }
        }
        // A writable disk's flush on a thread of its own when the guest's driver takes it (virtio_blk.rs).
        logi!(
            "disk: a flush of a writable disk runs {} (blk-late-flush={})",
            if config.blk_late_flush {
                "on the disk's flush thread when the guest's driver asks for that"
            } else {
                "on the vCPU that sent it"
            },
            u8::from(config.blk_late_flush)
        );
        self.disk_identity = vec![DiskIdentity::default(); config.disks.len()];
        for i in 0..config.disks.len() {
            let c = config.disks[i].clone();
            let disk = Blk::open(&c).map_err(|e| {
                loge!("disk {}: {} ({})", c.path, e.what, xh_core::sys::error_text(&e.error));
                BootFailed
            })?;
            self.image_set_disk(config, i, &disk)?;
            let (base, spi) = slots.next();
            virtio_blk::create(base, spi, disk.clone(), config.blk_late_flush).ok_or(BootFailed)?;
            let id = &self.disk_identity[i];
            logi!(
                "disk {}: {}, {} MiB, {}{}{}{}",
                slots.kinds.len(),
                c.path,
                disk.bytes() >> 20,
                if c.data_store {
                    "Data store"
                } else if c.writable {
                    "writable"
                } else {
                    "read-only"
                },
                if id.role.is_empty() { "" } else { ", role " },
                id.role,
                if id.verified.is_some() { ", chunks verified" } else { "" }
            );
            slots.add(if c.data_store { DeviceKind::DataStore } else { DeviceKind::Block });
            self.disks.push(disk);
        }
        let (base, spi) = slots.next();
        let input_dev = virtio_input::create(base, spi);
        slots.add(DeviceKind::Input);
        // The pen (pen.rs, spen 2026-11-15; pen=off by default): a second input device, /arm-io/input-1, when the
        // host has a pen digitizer (pen::lookup: read only, never grabbed) or the input script has pen lines. The
        // guest publishes it as iOS's pencil digitizer with oneios-pen=1, added to its command line unless it has one.
        let script_pen = input_script::load(&config.input_script).map_err(|_| BootFailed)?;
        let mut host_pen: Option<(pen::Device, u32)> = None;
        let mut pen_dev = None;
        if config.pen != Pen::Off {
            if config.input {
                match pen::lookup() {
                    Ok((device, why)) => {
                        let (rotate, how) = pen::rotation(config.pen_rotate.degrees(), &device, fb_width, fb_height);
                        logi!(
                            "pen: {} \"{}\": read only, not grabbed; {}; turned {} degrees ({}){}{}{}",
                            device.path,
                            device.name,
                            pen::describe(&device),
                            rotate,
                            how,
                            if config.pen_mirror { ", mirrored" } else { "" },
                            if why.is_empty() { "" } else { "; " },
                            why
                        );
                        host_pen = Some((device, rotate));
                    }
                    Err(why) => logi!("pen: none found: {}", why),
                }
            }
            if host_pen.is_some() || script_pen {
                // The pen's IC sleeps with One UI's display (its `1,0` at the switch): the brightness bridge wakes it
                // with the touch IC, and sleeps it with iOS's display, only for the host's pen.
                self.pen_ic = host_pen.is_some();
                let (base, spi) = slots.next();
                let dev = virtio_input::create(base, spi).ok_or(BootFailed)?;
                pen::guest_device(&dev);
                slots.add(DeviceKind::Input);
                if !config.boot_args.contains("oneios-pen=")
                    && add_boot_arg(&mut config.boot_args, "oneios-pen=1").is_err()
                {
                    loge!("pen: no room in the boot arguments for oneios-pen=1");
                    return Err(BootFailed);
                }
                logi!(
                    "pen: the guest's pen device is /arm-io/input-1 ({}){}",
                    if host_pen.is_some() { "the host's pen" } else { "the input script's pen" },
                    if config.boot_args.contains("oneios-pen=1") {
                        "; oneios-pen=1"
                    } else {
                        "; the command line's oneios-pen= is kept"
                    }
                );
                pen_dev = Some(dev);
            }
        } else if script_pen {
            logi!("pen: pen=off: the input script's pen lines are left out");
        }
        // The motion sensors and the front light sensor (sensors=auto|ssc; auto by default since 2026-11-25, sensors=off turns it
        // off): the phone's sensor hub (SSC) over QRTR, no Android framework (onebridges::sensors). The hub is asked here,
        // before the devices are counted: its accelerometer decides the guest's motion device (/arm-io/input-N, after
        // the touch screen and the pen) and its light sensor the guest's SPU light sensor (oneios-als-spu=1). The
        // streams start with the bridges; auto with no hub (no QRTR, no service 400, no answer) is the old path.
        let mut motion_dev = None;
        if config.sensors != config::Sensors::Off {
            if !config.bridges {
                logi!(
                    "sensors: off: sensors={} reads the phone's sensor hub, and the bridges are off (no phone)",
                    config.sensors.as_str()
                );
            } else {
                let o = sensors::Options {
                    hz: config.sensors_hz,
                    light_index: config.sensors_light.map(|v| v as usize),
                    ..sensors::Options::default()
                };
                match sensors::Hub::open(&o) {
                    Ok(hub) => {
                        let hub = std::sync::Arc::new(hub);
                        let mut what: Vec<String> = Vec::new();
                        for (kind, suid, desc) in &hub.found().sensors {
                            what.push(format!("{} {suid} {desc}", kind.name()));
                        }
                        let mut guest = Vec::new();
                        // The motion device only with a collection that has no com.apple.driver.AppleSPU (v0.1.29's
                        // panic, phone 2026-11-25 05:05: with oneios-motion the kernel registers AppleSPUHIDDevice, and
                        // AppleSPU, in the collection then, loaded after it with its own class of that name). The light
                        // sensor below is not affected either way.
                        let motion_ok = motion_allowed(&kernel);
                        if hub.has(sensors::Kind::Accel) {
                            if motion_ok {
                                logi!(
                                    "sensors: motion: the kernel's AppleSPUHIDDevice ({SPU_KEXT} is not in {}): the \
                                     accelerometer is the guest's motion device",
                                    config.kernel
                                );
                            } else {
                                logi!(
                                    "sensors: motion: not given to the guest: {} has {SPU_KEXT}, whose AppleSPUHIDDevice \
                                     would clash with the kernel's (v0.1.29's panic); prelink it again. The light sensor only",
                                    config.kernel
                                );
                            }
                        }
                        if motion_ok && hub.has(sensors::Kind::Accel) && !slots.full() {
                            let input_n = slots.kinds.iter().filter(|k| **k == DeviceKind::Input).count();
                            let (base, spi) = slots.next();
                            let dev = virtio_input::create(base, spi).ok_or(BootFailed)?;
                            let mag = hub.has(sensors::Kind::Mag);
                            motion::guest_device(&dev, mag);
                            slots.add(DeviceKind::Input);
                            let mask = if mag { 0xf } else { 0x7 };
                            if !config.boot_args.contains("oneios-motion=")
                                && add_boot_arg(&mut config.boot_args, &format!("oneios-motion={mask}")).is_err()
                            {
                                loge!("sensors: no room in the boot arguments for oneios-motion={mask}");
                                return Err(BootFailed);
                            }
                            guest.push(format!("motion device /arm-io/input-{input_n} at {} Hz", hub.hz()));
                            motion_dev = Some(dev);
                        }
                        if hub.has(sensors::Kind::Light) {
                            if !config.boot_args.contains("oneios-als-spu=")
                                && add_boot_arg(&mut config.boot_args, "oneios-als-spu=1").is_err()
                            {
                                loge!("sensors: no room in the boot arguments for oneios-als-spu=1");
                                return Err(BootFailed);
                            }
                            guest.push("the light for the brightness bridge, on change".into());
                        }
                        let args: Vec<&str> = config
                            .boot_args
                            .split_whitespace()
                            .filter(|w| w.starts_with("oneios-motion=") || w.starts_with("oneios-als-spu="))
                            .collect();
                        let s = hub.found().service;
                        logi!(
                            "sensors: on: ssc (sensors={}): service 400 version {} at node {} port {:#x}; {}; {}; {}",
                            config.sensors.as_str(),
                            s.version(),
                            s.node,
                            s.port,
                            what.join(", "),
                            if guest.is_empty() { "nothing for the guest".to_string() } else { guest.join(", ") },
                            if args.is_empty() { "no guest switch".to_string() } else { args.join(" ") }
                        );
                        self.sensors = Some(hub);
                    }
                    Err(why) if config.sensors == config::Sensors::Ssc => {
                        loge!("sensors: off: sensors=ssc: {why}: no motion device, and no light sensor (the rear one is not used with ssc)");
                    }
                    Err(why) => {
                        logi!("sensors: off: source old (sensors=auto): {why}: no motion device, the rear light sensor as before");
                    }
                }
            }
        }
        // The GPU: the 2D scanout always; with it the 3D half on the phone's Adreno (drm2kgsl on KGSL), when the host
        // has it, or on an amdgpu host without KGSL (the Galaxy A56's Xclipse) the amdgpu renderer when the kernel's
        // gpuaddon carries the A56's Metal plug-in and the renderer's self-test passes (gpu3d_amdgpu::choose,
        // docs/a56-gpu.md 2); then the guest is told with oneios-gpu-amd=1 (K5).
        let mut gpu = None;
        let mut gl_bundle = false; // the gpuaddon carries OneiOSRadeonGL and the amdgpu renderer was picked
        if pool_bytes != 0 {
            // The GPU's level and priorities (gpu3d_kgsl, drm2kgsl patch 0008; docs/gpu.md 2.2b) read these.
            std::env::set_var("ONEIOS_GPU_LEVEL", config.gpu_level.as_str());
            std::env::set_var("ONEIOS_GPU_KGSL_UI_RB", if config.gpu_kgsl_ui_rb { "1" } else { "0" });
            if !config.gpu_priority {
                std::env::set_var("ONEIOS_GPU_PRIORITY", "off");
            }
            let m = mem::state();
            let pool = mem::ram(phys + window, pool_bytes).map_or(std::ptr::null_mut(), |p| p.as_ptr());
            let sys: std::sync::Arc<dyn gpu3d_amdgpu::Dev> = std::sync::Arc::new(gpu3d_amdgpu::System);
            let build = self.image_set.as_ref().map(|s| s.build.clone());
            let kernel = config.kernel.clone();
            let choice = gpu3d_amdgpu::choose(
                config.gpu == Gpu::ThreeD,
                kgsl,
                if kgsl { Err(String::new()) } else { gpu3d_amdgpu::node_check(&*sys, node) },
                || {
                    // the layout's read when it made one (an amdgpu host), else now
                    let addon = addon_read.take().unwrap_or_else(|| sidecar(&kernel, "gpuaddon", 64 << 20));
                    let found = gpu3d_amdgpu::plugin_in_addon(addon.as_deref(), build.as_deref());
                    addon_read = Some(addon);
                    found
                },
                || gpu3d_amdgpu::selftest::run(sys.clone(), node, gpu_log),
            );
            let fail = |why: String| {
                if config.gpu == Gpu::ThreeD {
                    loge!("gpu: no 3D: {}", why);
                    return Err(BootFailed);
                }
                logi!("gpu: 2D only: {}", why);
                Ok(())
            };
            match choice {
                // the memfd from its offset 0 (mem.rs)
                gpu3d_amdgpu::Choice::Kgsl => {
                    match gpu3d::kgsl_open(m.fd, m.fd_phys, m.head + m.size, pool, phys + window, pool_bytes, gpu_log) {
                        Ok(g) => gpu = Some(g),
                        Err(why) if config.gpu == Gpu::ThreeD => {
                            loge!("gpu: no 3D: {}", why);
                            return Err(BootFailed);
                        }
                        Err(why) => logi!("gpu: 2D only: {}", why),
                    }
                }
                gpu3d_amdgpu::Choice::Amdgpu(target) => {
                    let opened = gpu3d::amdgpu_open(
                        node,
                        m.fd,
                        m.fd_phys,
                        m.head + m.size,
                        pool,
                        phys + window,
                        pool_bytes,
                        gpu_log,
                    );
                    match opened {
                        Err(why) => fail(why)?,
                        Ok(_) if add_boot_arg(&mut config.boot_args, "oneios-gpu-amd=1").is_err() => {
                            fail("no room in the boot arguments for oneios-gpu-amd=1".into())?
                        }
                        Ok(g) => {
                            logi!(
                                "gpu: 3D on amdgpu ({}): the gpuaddon's {} plug-in ({}) and the self-test passed; {}",
                                node,
                                gpu3d_amdgpu::PLUGIN,
                                target,
                                if config.boot_args.split(' ').any(|w| w == "oneios-gpu-amd=1") {
                                    "oneios-gpu-amd=1"
                                } else {
                                    "the command line's oneios-gpu-amd= is kept"
                                }
                            );
                            // OpenGL ES for CoreAnimation (docs/a56-gpu.md 4a): when the same gpuaddon carries the
                            // A56's GL bundle, the kernel names it for every process (oneios-gpu-amd-gl=1) and the
                            // render server composites on it; without it, CPU composition as before.
                            let addon = addon_read.as_ref().and_then(|a| a.as_deref());
                            match gpu3d_amdgpu::gl_in_addon(addon, build.as_deref()) {
                                // a kernel that does not read the switch (an A56 kernel before 5966e71ce2aa) would
                                // name no GL driver for SpringBoard, whose CoreImage then needs the fixup: no GL
                                Ok(_) if !gpu3d_amdgpu::kernel_knows_gl(&image) => logi!(
                                    "gpu: OpenGL ES: the kernel does not read oneios-gpu-amd-gl (an A56 kernel before 5966e71ce2aa): Metal alone, CPU composition, the CoreImage fixup kept"
                                ),
                                Ok(gl) if add_boot_arg(&mut config.boot_args, "oneios-gpu-amd-gl=1").is_ok() => {
                                    gl_bundle = true;
                                    logi!(
                                        "gpu: OpenGL ES: the gpuaddon's {} ({}, {} files); {}",
                                        gpu3d_amdgpu::GL_PLUGIN,
                                        gl.target,
                                        gl.files,
                                        if config::amd_gl_on(&config.boot_args) {
                                            "oneios-gpu-amd-gl=1: CoreAnimation composites on it"
                                        } else {
                                            "the command line's oneios-gpu-amd-gl= is kept: no GL, CPU composition"
                                        }
                                    )
                                }
                                Ok(_) => logi!("gpu: OpenGL ES: no room in the boot arguments for oneios-gpu-amd-gl=1"),
                                Err(why) => logi!("gpu: OpenGL ES: {}: Metal alone, CPU composition", why),
                            }
                            gpu = Some(g);
                        }
                    }
                }
                gpu3d_amdgpu::Choice::TwoD(why) | gpu3d_amdgpu::Choice::Refuse(why) => fail(why)?,
            }
        }
        let have_3d = gpu.is_some();
        let (base, spi) = slots.next();
        let Some(input_dev) = input_dev else { return Err(BootFailed) };
        virtio_gpu::create_3d(base, spi, fb_width, fb_height, gpu).ok_or(BootFailed)?;
        slots.add(DeviceKind::Gpu);
        // The hardware backend of each bridge (hw-backend, onebridges::hw): set before any of them opens.
        onebridges::hw::hw_configure(&config.hw);
        if config.input {
            // Touch and the side keys: one lookup for both backends (input.rs: event0..31 by their capabilities, never
            // grabbed, except touch-exclusive=1, which grabs only the native sec_touchscreen: a bench+SHM preview, or
            // session-mode=takeover, where the side keys are still read and never grabbed).
            use onebridges::hw::{Backend, Bridge};
            let (touch, touch_why) = config.hw.choose(Bridge::Touch, Backend::Unset);
            let (keys, keys_why) = config.hw.choose(Bridge::Keys, Backend::Unset);
            logi!(
                "hw: touch: backend {} ({}); keys: backend {} ({}): the same lookup in both, /dev/input/event0..31 by their capabilities, exclusive touch {}",
                touch.name(),
                touch_why,
                keys.name(),
                keys_why,
                config.exclusive_touch_text()
            );
        }
        let host_pen_dev = pen_dev.as_ref().filter(|_| host_pen.is_some());
        if config.input {
            let ipen = host_pen.zip(host_pen_dev).map(|((device, rotate), dev)| input::InputPen {
                dev: dev.clone(),
                device,
                rotate,
                mirror: config.pen_mirror,
            });
            // The bench preview leaves the side keys to the Android that runs under it; every session reads them.
            let read_keys = config.reads_side_keys();
            if input::start(&input_dev, ipen, fb_width, fb_height, config.touch_exclusive, read_keys).is_err() {
                if config.touch_exclusive {
                    loge!(
                        "input: exclusive touchscreen unavailable; refusing to boot guest, Android input stays usable"
                    );
                    return Err(BootFailed);
                }
                loge!("input: could not start; continuing without");
            }
        }
        let script_pen_dev = pen_dev.as_ref().filter(|_| !self.pen_ic);
        input_script::start(&input_dev, script_pen_dev).map_err(|_| BootFailed)?;

        let mut machine = Machine::default();
        if have_3d {
            machine.gpu_pool_base = phys + window;
            machine.gpu_pool_size = pool_bytes;
        }
        machine.chip_id = chip_id(&config.data);

        // The bridges. Each one whose Linux side is missing says so and is left out; nothing else changes.
        self.bridges = Bridges::start(
            config,
            &mut machine,
            &mut slots,
            super::bridges::MediaPool { pa: media_pool_pa, bytes: media_bytes },
            self.pen_ic,
            self.sensors.clone().map(|hub| (hub, motion_dev)),
        );

        // clusters=EEPPPPPP: the cluster types by hand (a bench run with no host topology to plan from).
        for (i, c) in config.clusters.bytes().take(config.ncpu as usize).enumerate() {
            machine.cpu_cluster[i] = if c == b'E' { xm::Cluster::E } else { xm::Cluster::P };
        }

        // The device tree.
        machine.board(config.ncpu, &slots.kinds);
        let info = hostcalls::info();
        machine.timebase_hz = info.timebase_hz as u32;
        machine.dram_base = phys;
        machine.dram_size = window;
        machine.view_size = view;
        machine.model = Some("iPhone18,2".into());
        machine.target_type = Some("V54".into()); // its board: iOS finds com.apple.jetsamproperties.V54.plist by it
                                                  // The cameras iOS is told about (the loader's /arm-io/isp and /product/camera): the roles the helper lists;
                                                  // with them cameracaptured's owner-hide-vmm and /product unique-model (camera-vm=, camera-model=).
        self.bridges.describe_cameras(config, &mut machine);
        // iOS 16.5 (20F66) finds its product by /product product-id (the iPhone X's) and shows the root's serial-number:
        // without them bluetoothd had no chip and crashed every 10 s, and About had no model name and no serial
        // (docs/userland.md, "The iOS 16.5 identity"). iOS 27 reads neither, so its tree is unchanged.
        let build = self.image_set.as_ref().map(|s| s.build.as_str());
        if build == Some("20F66") {
            machine.product_id = Some(config::IOS16_PRODUCT_ID);
            machine.serial_number = Some(config::IOS16_SERIAL.into());
            logi!(
                "ios16: /product product-id {} (the iPhone X's, MobileGestalt's product), serial-number {}",
                config::IOS16_PRODUCT_ID.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                config::IOS16_SERIAL
            );
        }
        machine.display_width = fb_width;
        machine.display_height = fb_height;
        machine.display_scale = config.scale;
        machine.display_hz = self.fb.2;
        // astc= (docs/switches.md): /product graphics-featureset-fallbacks keeps APPLE2 and MTL2,2, so MobileGestalt answers
        // DeviceSupportsASTC and CoreUI and Core Animation hand ASTC images to the GPU.
        machine.astc = config.astc;
        logi!(
            "gpu: astc={}: /product graphics-featureset-fallbacks {}",
            u8::from(config.astc),
            if config.astc {
                "keeps APPLE2 and MTL2,2 (DeviceSupportsASTC)"
            } else {
                "without APPLE2 and MTL2,2 (no ASTC)"
            }
        );
        // Signed text the kernel changes at page-in (/chosen text-fixups): the configured ones, and what the UI scale
        // needs.
        if config.scale_fixup {
            config::scale_fixups(
                self.image_set.as_ref().map(|s| s.build.as_str()),
                config.scale,
                &mut config.text_fixups,
            );
        }
        // iOS 16.5: CoreImage's OpenGL ES fallback answers no context (no GL driver here; docs/gpu.md). Not with the
        // A56's OpenGL ES (its bundle in this boot's gpuaddon and oneios-gpu-amd-gl=1 in the guest's boot arguments,
        // docs/a56-gpu.md 4a): the bundle answers CoreImage's EAGLContext (on radeonsi, or on softpipe without it), and
        // CoreImage draws instead of a blank.
        if config.ios16_fixup && config::ios16_gl_driver(gl_bundle, &config.boot_args) {
            if build == Some("20F66") {
                logi!(
                    "ios16: CoreImage's GL fallback fixup left out: oneios-gpu-amd-gl=1, {} answers its EAGLContext",
                    gpu3d_amdgpu::GL_PLUGIN
                );
            }
        } else if config.ios16_fixup {
            let added = config::ios16_fixups(build, &mut config.text_fixups);
            if added != 0 {
                logi!(
                    "ios16: {} text fixup(s) added: CoreImage's GL fallback answers no context (ios16-fixup=0: off)",
                    added
                );
            }
        }
        if !config.text_fixups.is_empty() {
            machine.text_fixups = Some(config.text_fixups.to_bytes());
            logi!(
                "text fixups: {} for /chosen text-fixups (UI scale @{}x)",
                config.text_fixups.entries().len(),
                config.scale
            );
        }
        machine.host_vector = hostcalls::vector().map_or(0, |v| v as *const _ as u64);
        machine.user_va_limit = task::va_limit();
        machine.host_page_size = info.host_page_size;
        // SAFETY: getentropy fills the seed it is given (256 bytes, its most).
        if unsafe { libc::getentropy(machine.random_seed.as_mut_ptr() as *mut libc::c_void, machine.random_seed.len()) }
            != 0
        {
            loge!("no entropy for the kernel's seed");
            return Err(BootFailed);
        }
        let ramdisk = sidecar(&config.kernel, "ramdisk", 1 << 30);
        let mut trust = sidecar(&config.kernel, "trustcache", 16 << 20);
        // read once: the GPU's choice may have read it already (an amdgpu host)
        let addon = match addon_read.take() {
            Some(addon) => addon,
            None => sidecar(&config.kernel, "gpuaddon", 64 << 20),
        };
        let policies = sidecar(&config.kernel, "vroot", 128 << 20);
        // Apple's trust caches of the set (IM4P) and the owner module, as the sidecar the kernel takes, unless one is
        // shipped.
        if let Some(set) =
            self.image_set.as_ref().filter(|s| trust.is_none() && s.find("trust/static.trustcache").is_some())
        {
            let blob = set.trustcaches().map_err(|e| {
                loge!("trust caches of {}: {}", config.image, e);
                BootFailed
            })?;
            logi!("trust caches: {} modules from the image set, {} bytes", blob[0], blob.len());
            trust = Some(blob);
        }
        // The root: the OS volume, matched by its APFS volume UUID.
        machine.root_volume_uuid = self
            .image_set
            .as_ref()
            .and_then(|s| s.role("os"))
            .map(|os| os.volume_uuid.clone())
            .filter(|u| !u.is_empty());
        machine.ramdisk = ramdisk.is_some();
        // iOS 16 userlands (20F66) look for the ramdisk under RAMDisk1 as well as RAMDisk (port-a56, docs/a56.md). The
        // image set's build names the userland; without a set (a bench run) there is no node.
        machine.ramdisk1 = self.image_set.as_ref().is_some_and(|s| s.build == "20F66");
        machine.trust_cache = trust.is_some();
        machine.gpu_addon = addon.is_some();
        machine.root_policies = policies.is_some();
        if !config.identity.is_empty() {
            let data = read_file(&config.identity, 16 << 20);
            let parsed = data.as_deref().map(Node::parse);
            match parsed {
                Some(Ok(identity)) => machine.identity = Some(identity),
                Some(Err(e)) => {
                    loge!("identity tree {}: {}", config.identity, e);
                    return Err(BootFailed);
                }
                None => {
                    loge!("identity tree {}: {}", config.identity, xh_core::sys::strerror(xh_core::sys::errno()));
                    return Err(BootFailed);
                }
            }
        }
        let mut tree = machine.tree().map_err(|e| {
            loge!("device tree: {}", e);
            BootFailed
        })?;
        if let Some(shipped) = sidecar(&config.kernel, "adt", 16 << 20) {
            let other = Node::parse(&shipped).map_err(|e| {
                loge!("{}.adt: {}", config.kernel, e);
                BootFailed
            })?;
            merge_shipped(&mut tree, &other);
        }
        // What MobileGestalt's cache depends on, as one number for hosted 0084 (/chosen oneios-identity).
        logi!(
            "identity: {:016x} (/chosen oneios-identity: what MobileGestalt reads; hosted 0084 keeps its cache while it stays)",
            xm::identity(&mut tree)
        );
        // Each disk's identity for the kernel's storage (imageset.rs).
        if self.image_set.is_some() {
            let mut blocks = 0;
            for (i, c) in config.disks.iter().enumerate() {
                let name = if c.data_store {
                    "arm-io/data-store".to_owned()
                } else {
                    blocks += 1;
                    format!("arm-io/virtio-mmio-{}", blocks - 1)
                };
                if let Some(node) = tree.find_mut(&name) {
                    crate::imageset::describe(node, &self.disk_identity[i]);
                }
            }
        }

        let m = mem::state();
        let video = [self.boot_fb_pa, 1, fb_width as u64 * 4, fb_width as u64, fb_height as u64, 32];
        let request = BootRequest {
            image: &image,
            kernel: &kernel,
            phys_base: phys,
            virt_base: m.host as u64,
            command_line: &config.boot_args,
            video,
            boot_flags: 0,
            mem_size_actual: view,
            legacy_boot_args: config.legacy_boot_args,
            ramdisk: ramdisk.as_deref(),
            trustcache: trust.as_deref(),
            gpu_addon: addon.as_deref(),
            root_policies: policies.as_deref(),
        };
        // SAFETY: the kernel's window, `window` bytes mapped at m.host (mem::map), is the guest's RAM; no vCPU runs yet
        // and no task maps it, so this is its only user while the kernel is placed.
        let ram = unsafe { std::slice::from_raw_parts_mut(m.host, window as usize) };
        let layout = request.place(ram, &mut tree).map_err(|e| {
            loge!("placing the kernel: {}", e);
            BootFailed
        })?;
        clear_cache(m.host, layout.used as usize);
        logi!(
            "placed: entry {:#x}, boot_args {:#x}, device tree {} bytes, {} MiB of the window used; boot-args \"{}\"",
            layout.entry_va,
            layout.args_va,
            layout.tree_size,
            layout.used >> 20,
            config.boot_args
        );

        sysreg::SETWAY_SKIP.store(config.setway_skip, std::sync::atomic::Ordering::Relaxed);
        logi!(
            "cpu: a trapped dc set/way {} (setway-skip={})",
            if config.setway_skip { "ends the loop of its level at once" } else { "is one trap per line" },
            u8::from(config.setway_skip)
        );
        let tasks = task::available();
        hostcalls::update_info(|i| {
            i.ncpu = config.ncpu;
            i.ram_phys = phys;
            i.ram_virt = m.host as u64;
            i.ram_size = window;
            i.io_phys = m.io_phys;
            i.io_virt = m.io_host as u64;
            i.io_size = m.io_size;
            i.user_va_min = 0x1_0000_0000;
            i.user_va_limit = task::va_limit();
            if tasks {
                i.features |= XH_FEATURE_TASKS;
            }
            i.video = video;
        });

        if cpu::init(config.ncpu as usize).is_err() {
            loge!("cannot set up {} CPUs", config.ncpu);
            return Err(BootFailed);
        }
        if hostcalls::vector().is_none() {
            loge!("this build cannot enter the kernel (not AArch64 Linux)");
            return Err(BootFailed);
        }
        Ok(Placed { entry_va: layout.entry_va, args_va: layout.args_va })
    }
}

/// The GPU path's lines into the log (0 error, 1 info, 2 debug).
fn gpu_log(level: u8, line: &str) {
    xh_core::xh_log!(xh_core::log::Level::from_index(level as i64), "{}", line);
}

/// The instruction cache over what was written into the window (the kernel's code).
fn clear_cache(start: *mut u8, len: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        // Cache line sizes from CTR_EL0; clean to the point of unification, invalidate the instruction cache.
        let ctr: u64;
        // SAFETY: reading CTR_EL0, which EL0 may read on Linux.
        unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
        let dline = 4usize << ((ctr >> 16) & 0xf);
        let iline = 4usize << (ctr & 0xf);
        let (begin, end) = (start as usize, start as usize + len);
        let mut a = begin & !(dline - 1);
        while a < end {
            // SAFETY: a cache maintenance instruction on an address of the window, which is mapped.
            unsafe { std::arch::asm!("dc cvau, {}", in(reg) a, options(nostack)) };
            a += dline;
        }
        // SAFETY: barriers and cache maintenance on mapped addresses.
        unsafe { std::arch::asm!("dsb ish", options(nostack)) };
        let mut a = begin & !(iline - 1);
        while a < end {
            // SAFETY: as above.
            unsafe { std::arch::asm!("ic ivau, {}", in(reg) a, options(nostack)) };
            a += iline;
        }
        // SAFETY: barriers.
        unsafe { std::arch::asm!("dsb ish", "isb", options(nostack)) };
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = (start, len);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_tree_merge() {
        let mut tree = Node::new("device-tree");
        tree.add("chosen").set_str("ours", "1").unwrap();
        tree.add("product").set_str("old", "1").unwrap();
        tree.add("cpus").set_str("mine", "1").unwrap();
        let mut other = Node::new("device-tree");
        other.add("chosen").set_str("ours", "2").unwrap();
        other.add("chosen").set_str("theirs", "3").unwrap();
        let product = other.add("product");
        product.set_str("new", "1").unwrap();
        product.add("child").set_str("p", "1").unwrap();
        product.add("child").add("grandchild").set_str("deep", "1").unwrap();
        other.add("cpus").set_str("theirs", "1").unwrap();
        merge_shipped(&mut tree, &other);
        let p = tree.child("product").unwrap();
        assert!(p.get("new").is_some() && p.get("old").is_none());
        assert!(
            p.child("child").unwrap().get("p").is_some() && p.child("child").unwrap().child("grandchild").is_none()
        );
        assert!(tree.child("cpus").unwrap().get("theirs").is_none()); // the machine's own nodes stay
        let chosen = tree.child("chosen").unwrap();
        assert_eq!(chosen.get("ours").unwrap().str(), Some("1"));
        assert_eq!(chosen.get("theirs").unwrap().str(), Some("3"));
    }

    #[test]
    fn motion_only_without_the_spu_kext() {
        let entry = |name: &str| xnuloader::macho::FilesetEntry { name: name.into(), vmaddr: 0, fileoff: 0 };
        let mut kernel = Kernel { fileset: true, ..Kernel::default() };
        kernel.entries = vec![entry("com.apple.kernel"), entry("com.apple.iokit.IOHIDFamily")];
        assert!(motion_allowed(&kernel));
        kernel.entries.push(entry("com.apple.driver.AppleSPU"));
        assert!(!motion_allowed(&kernel)); // a collection prelinked before 2026-11-25: v0.1.29's panic
        kernel.entries = vec![entry("com.apple.driver.AppleSPURose")]; // the exact identifier only
        assert!(motion_allowed(&kernel));
        assert!(motion_allowed(&Kernel::default())); // MH_EXECUTE: no kexts
    }

    #[test]
    fn slots_follow_the_board() {
        let mut s = Slots { kinds: Vec::new() };
        assert_eq!(s.next(), (xm::BOARD_VIRTIO, xm::BOARD_VIRTIO_SPI));
        s.add(DeviceKind::Block);
        assert_eq!(s.next(), (xm::BOARD_VIRTIO + xm::BOARD_VIRTIO_SIZE, xm::BOARD_VIRTIO_SPI + 1));
        let mut args = "a=1 b".to_owned();
        assert!(add_boot_arg(&mut args, "oneios-pen=1").is_ok() && args == "a=1 b oneios-pen=1");
    }
}
