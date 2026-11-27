// SPDX-License-Identifier: MIT
//! main.c's host bridges (runtime/bridges): CPU placement and cpufreq, haptics, Bluetooth, brightness, the battery,
//! thermal steps, the network (the phone's Wi-Fi, a tap, the stand-in radio), sound, the camera, the video encoder.
//! Each one whose Linux side is missing says so and is left out; nothing else changes.

use std::sync::atomic::Ordering::{AcqRel, Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use onebridges::audio::{AudioBackend, AudioHelper, NullAudio};
use onebridges::battery::{self, Battery};
use onebridges::brightness::{self, Bright};
use onebridges::bt::{hci, hci_hidl, hci_sim};
use onebridges::cpu::{self as bcpu, CpuPlan, Cpufreq, Topology};
use onebridges::devices::{self, bridge_dev, virtio_net, virtio_snd, BridgeConfig};
use onebridges::haptics::{self, Haptic};
use onebridges::hw::{Backend, Bridge};
use onebridges::net::{Net, NetConfig};
use onebridges::sensors::{Hub, Kind as SensorKind, LightFn, Motion, MotionFn};
use onebridges::thermal::{self, Thermal};
use onecamera::link::Camera;
use onecamera::proto::{OC_ROLE_FRONT, OC_ROLE_TELE5, OC_ROLE_WIDE};
use oneencoder::backend::Backend as EncoderBackend;
use oneencoder::decoder::{DecodeBackend, PoolMem, StubDecodeBackend};
use xh_core::{loge, logi};
use xnuloader::machine::{self as xm, Cluster, DeviceKind, Mac, Machine};

use super::board::{add_boot_arg, Slots};
use crate::config::{self as cfg, Bluetooth, CameraVm, Config, CpuPin, Encoder, Haptics, Wifi};
use crate::{cpu, task};

/// The media pool carved out of the RAM for the video encoder (0: none).
#[derive(Clone, Copy)]
pub struct MediaPool {
    pub pa: u64,
    pub bytes: u64,
}

// ---- CPU placement (the hooks cpu.rs calls: plain fns, so the plan is the process's) -----------------------------

/// The CPU plan as made: the thermal steps narrow `plan` within `base`.
struct Placement {
    plan: CpuPlan,
    base: CpuPlan,
    host: Option<Topology>,
    /// cpu-pin: a vCPU and the iOS thread it runs on one core of its mask
    pin: bool,
    cpufreq: Option<Arc<Cpufreq>>,
}

static PLACEMENT: OnceLock<Placement> = OnceLock::new();

/// cpu-pin (default on): vCPU `vcpu`'s one host CPU, the rank-th CPU of its plan's mask among the vCPUs of its kind (so
/// the vCPUs of a kind take different CPUs while there are enough). The iOS thread a vCPU runs goes on the same CPU:
/// the hand-off between them is then a switch on one core, not a wake-up of another (docs/booter.md 2.6a: 3 us for a
/// system call there and back against 30 us across cores). Otherwise, and whenever a thermal step leaves fewer CPUs
/// than vCPUs of the kind (pin_mask), the plan's whole mask.
fn place_mask(p: &Placement, vcpu: usize) -> u64 {
    let mask = p.plan.v[vcpu].affinity(); // the thermal step may narrow it
    if !p.pin || mask == 0 {
        return mask;
    }
    p.plan.pin_mask(&p.base, vcpu)
}

fn place(tid: i32, vcpu: usize) {
    let Some(p) = PLACEMENT.get().filter(|p| vcpu < p.plan.nvcpu()) else { return };
    let mask = place_mask(p, vcpu);
    if let Err(e) = bcpu::set_affinity(tid, mask) {
        if tid == 0 {
            loge!("cpu: vCPU {} on host CPUs {:#x}: {}", vcpu, mask, xh_core::sys::error_text(&e));
        }
    }
}

fn vcpu_tid(vcpu: usize) -> i32 {
    if vcpu < cpu::ncpu() {
        cpu::vcpu(vcpu).tid.load(Relaxed)
    } else {
        0
    }
}

/// The guest's request for a vCPU as a clamp (cpu.rs set_uclamp): what its CLPC stand-in asks for it (DroidVMCpufreq's
/// SET_PERF, Cpufreq::requested). thread_run gives the iOS thread it releases cpu::release_floor of it, which adds the
/// thread's class and the GPU path's animation clamp.
fn vcpu_uclamp(vcpu: usize) -> u32 {
    let Some(p) = PLACEMENT.get() else { return 0 };
    match p.cpufreq.as_ref().filter(|_| vcpu < p.plan.nvcpu()) {
        Some(f) => p.plan.util_for(vcpu, f.requested(vcpu)).min(1024),
        None => 0,
    }
}

/// The guest changed its request for `vcpu` (DroidVMCpufreq writes its own window, on that vCPU's thread): the vCPU's
/// own clamp follows at once, for the kernel's work up to the next release (which sets the released thread's floor).
/// Before 2026-11-23 the device set the vCPU thread's clamp itself, and drm2kgsl's boost overwrote it (and back).
fn cpufreq_apply(vcpu: usize, util: u32) {
    if let Some(v) = cpu::current().filter(|v| v.index() == vcpu) {
        cpu::own_uclamp(v, util);
    }
}

/// The thermal bridge's step (cap_for): the vCPUs and the iOS threads they run keep to fewer, slower cores while the
/// phone is hot. Affinities of xnuhost's own threads and its tasks' only: they end with the session, nothing outside
/// the process is written, so a SIGKILL leaves nothing to restore.
fn thermal_step(state: &thermal::State) {
    let Some(p) = PLACEMENT.get() else { return };
    let Some(host) = &p.host else { return };
    p.plan.cap(&p.base, host, state.cap);
    task::PLACE_EPOCH.fetch_add(1, AcqRel);
    for i in 0..p.plan.nvcpu() {
        let tid = vcpu_tid(i);
        if tid > 0 {
            place(tid, i);
        }
    }
    let last = p.plan.v.last().map_or(0, |v| v.affinity());
    logi!(
        "thermal: step {} ({}): P vCPUs on host CPUs {:#x}{}",
        state.cap,
        thermal::level_name(state.level),
        last,
        if state.cap != 0 { "" } else { " (all of their kind)" }
    );
}

/// The host's cores as read at start (named files only, runtime/bridges/cpu), what each vCPU is described as to iOS,
/// and the cpuset xnuhost runs in: a cpuset of fewer CPUs (Android's background or system-background) would keep the
/// vCPUs off the X4 and the A720s whatever cpu-pin asks, so its CPUs are said here (host-overhead, 2026-11-15).
fn core_name(part: u32) -> &'static str {
    match part {
        0xd80 => "Cortex-A520",
        0xd81 => "Cortex-A720",
        0xd82 => "Cortex-X4",
        0xd46 => "Cortex-A510",
        0xd4d => "Cortex-A715",
        0xd4e => "Cortex-X3",
        0 => "unknown",
        _ => "an Arm core",
    }
}

fn log_cpu_layout(host: &Topology, plan: &CpuPlan) {
    for (c, h) in host.cpu.iter().enumerate().filter(|(_, h)| h.present) {
        let capped = h.scaling_max_khz != 0 && h.scaling_max_khz < h.max_khz;
        logi!(
            "cpu: host CPU {}: {} (part {:#x}), {}, frequency domain of CPU {}, capacity {}, {}-{} kHz{}{}{}",
            c,
            core_name(h.part),
            h.part,
            h.kind.letter(),
            h.policy,
            h.capacity,
            h.min_khz,
            h.max_khz,
            if h.boost_khz != 0 { " (top as a boost frequency)" } else { "" },
            if capped { "; capped now: scaling_max_freq " } else { "" },
            if capped { h.scaling_max_khz.to_string() } else { String::new() }
        );
    }
    for (i, v) in plan.v.iter().enumerate() {
        logi!(
            "cpu: vCPU {} ({}) is described to iOS as host CPU {}: {}-{} kHz, {} table entries, capacity {}",
            i,
            v.kind.letter(),
            v.host_cpu,
            v.min_khz,
            v.max_khz,
            v.freq_khz.len(),
            v.capacity
        );
    }
    let boost = xh_core::fs::read_line("/sys/devices/system/cpu/cpufreq/boost").unwrap_or_default();
    let cpuset = xh_core::fs::read_line("/proc/self/cpuset").unwrap_or_default();
    let allowed = xh_core::fs::key_line("/proc/self/status", "Cpus_allowed_list:").unwrap_or_default();
    logi!(
        "cpu: xnuhost's cpuset {}, CPUs allowed {}; cpufreq boost {}",
        if cpuset.is_empty() { "(none)" } else { &cpuset },
        if allowed.is_empty() { "?" } else { &allowed },
        if boost.is_empty() { "(no switch)" } else { &boost }
    );
    log_governors(host);
}

/// Each frequency domain's governor and the tunables that set how fast it climbs and falls (2026-11-23): with One UI's
/// power HAL gone the governor alone picks the clocks, and nothing said what it runs with. Read only, named files only:
/// `policyN/scaling_governor`, then `policyN/<governor>/<name>` for the names below that exist (WALT's and schedutil's),
/// and WALT's `sched_boost`. Nothing is written.
fn log_governors(host: &Topology) {
    const TUNABLES: [&str; 10] = [
        "rate_limit_us",
        "up_rate_limit_us",
        "down_rate_limit_us",
        "hispeed_load",
        "hispeed_freq",
        "target_loads",
        "pl",
        "boost",
        "rtg_boost_freq",
        "adaptive_high_freq",
    ];
    let mut seen = 0u64;
    for h in host.cpu.iter().filter(|h| h.present) {
        let n = h.policy;
        if n >= 64 || seen >> n & 1 != 0 {
            continue;
        }
        seen |= 1 << n;
        let dir = format!("/sys/devices/system/cpu/cpufreq/policy{n}");
        let gov = xh_core::fs::read_line(format!("{dir}/scaling_governor")).unwrap_or_default();
        let mut line = format!("cpu: policy{n}: governor {}", if gov.is_empty() { "?" } else { &gov });
        let name_ok = !gov.is_empty() && gov.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        let mut first = true;
        for t in TUNABLES.iter().filter(|_| name_ok) {
            if let Some(v) = xh_core::fs::read_line(format!("{dir}/{gov}/{t}")) {
                line += &format!("{}{t} {}", if first { "; " } else { ", " }, v.trim());
                first = false;
            }
        }
        logi!("{line}");
    }
    if let Some(v) = xh_core::fs::read_line("/proc/sys/walt/sched_boost") {
        logi!("cpu: WALT sched_boost {}", v.trim());
    }
}

/// The CPU plan: E vCPUs first, then P, each placed on its kind of host core; the cpufreq device; the uclamp hook.
fn plan_cpus(config: &Config, machine: &mut Machine) -> bool {
    let n = config.ncpu as usize;
    let Some((host, plan)) = Topology::read("/sys").and_then(|h| CpuPlan::make(&h, n, false).map(|p| (h, p))) else {
        logi!("cpu: no host topology under /sys: vCPUs are not placed");
        return false;
    };
    for (i, v) in plan.v.iter().enumerate().take(n) {
        machine.cpu_cluster[i] = if v.kind.letter() == 'E' { Cluster::E } else { Cluster::P };
        cpu::vcpu(i).kind.store(v.kind.letter() as u8, Relaxed);
        // E vCPUs first, then P
    }
    // uclamp-rule=all (docs/switches.md): the device sets the vCPU thread's clamp itself, as before 03d85ac
    let cpufreq = Arc::new(if cpu::UCLAMP_ALL.load(Relaxed) {
        Cpufreq::new(&plan, Some(Box::new(vcpu_tid)))
    } else {
        Cpufreq::with_apply(&plan, Box::new(cpufreq_apply))
    });
    let size = n as u64 * bcpu::CPUFREQ_WINDOW;
    let cpufreq = match xh_core::mmio::bus().add(xm::BOARD_CPUFREQ, size, "cpufreq", cpufreq.clone()) {
        Ok(()) => {
            machine.cpufreq_base = xm::BOARD_CPUFREQ;
            machine.cpufreq_size = size;
            Some(cpufreq)
        }
        Err(_) => None,
    };
    let pin = config.cpu_pin != CpuPin::Off;
    let placement = Placement { base: plan.clone(), plan, host: Some(host), pin, cpufreq };
    let p = PLACEMENT.get_or_init(|| placement);
    cpu::set_place(Some(place));
    cpu::set_uclamp(Some(vcpu_uclamp));
    let host = p.host.as_ref().expect("the host's topology");
    logi!(
        "cpu: {} E + {} P vCPUs on the host's {} E + {} P cores{}",
        p.plan.n_e,
        p.plan.n_p,
        host.n_e,
        host.n_p,
        if pin {
            "; each vCPU and the iOS thread it runs on one core (cpu-pin=1)"
        } else {
            " (cpu-pin=0: a vCPU on any core of its kind)"
        }
    );
    if pin {
        for i in 0..n {
            logi!("cpu{}: host CPU mask {:#x}", i, place_mask(p, i));
        }
    }
    log_cpu_layout(host, &p.plan);
    true
}

/// A bench run (cpu-pin=2): vCPU i and the iOS thread it runs on host CPU i, as cpu-pin does on the phone.
fn pin_by_index(config: &Config) {
    let mut plan = CpuPlan::default();
    for i in 0..config.ncpu as usize {
        let v = bcpu::Vcpu::default();
        v.affinity.store(1 << (i % 64), Relaxed);
        plan.v.push(v);
    }
    // the masks are single CPUs already: no pinning within them
    let placement = Placement { base: plan.clone(), plan, host: None, pin: false, cpufreq: None };
    let _ = PLACEMENT.set(placement);
    cpu::set_place(Some(place));
    logi!("cpu: cpu-pin=2: vCPU i and the iOS thread it runs on host CPU i");
}

// ---- the bridges --------------------------------------------------------------------------------------------------

/// What the session keeps of the bridges, to close them at the stop.
#[derive(Default)]
pub struct Bridges {
    pub haptic: Option<Arc<Haptic>>,
    pub camera: Option<Arc<Camera>>,
    pub brightness: Option<Arc<Bright>>,
    pub battery: Option<Arc<Battery>>,
    pub thermal: Option<Arc<Thermal>>,
    /// the sensor hub's streams (closed first at the stop)
    pub sensors: Option<Arc<Hub>>,
    /// a tap and oneios-net.sh's NAT are up (undone at the stop)
    pub net_up: bool,
    /// wlan0 is the guest's en0 (runtime/bridges/wifi)
    pub wifi_up: bool,
    runtime: String,
    /// the haptics line's counters at its last line, and when
    haptic_shown: Mutex<Option<(u64, onebridges::haptics::HapticStats)>>,
}

/// The brightness bridge, for the input thread's power key.
static BRIGHT: OnceLock<Arc<Bright>> = OnceLock::new();

/// A power-key press from the evdev reader: the touch IC woken (iOS turns its display on or off next).
fn power_key_pressed() {
    if let Some(b) = BRIGHT.get() {
        b.key();
    }
}

/// bluetooth=sim: the guest's /bluetooth node as with the phone's controller (H4, transport 1), with a made-up address
/// that the stand-in controller reports too.
fn bt_sim_on(m: &mut Machine) {
    m.bt_transport = 1;
    m.bt_speed = 3_000_000;
    m.bt_mac = m.mac(Mac::Bluetooth);
    hci_sim::set_address(m.bt_mac);
    logi!("bluetooth: on: a stand-in controller (bluetooth=sim), address {}; no radio, no HAL", mac_text(&m.bt_mac));
}

fn mac_text(a: &[u8; 6]) -> String {
    a.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// sh <runtime>/bin/oneios-net.sh VERB: the tap's address, forwarding and NAT (up), undone (down).
fn net_script(runtime: &str, verb: &str) -> bool {
    if runtime.is_empty() {
        return false;
    }
    let script = format!("{runtime}/bin/oneios-net.sh");
    let status = std::process::Command::new("sh").arg(&script).arg(verb).status();
    let code = status.map_or(-1, |s| s.into_raw_status_code());
    if code != 0 {
        loge!("network: `oneios-net.sh {}` ended with status {}: the guest has no route out", verb, code);
    }
    code == 0
}

trait RawStatus {
    fn into_raw_status_code(self) -> i32;
}
impl RawStatus for std::process::ExitStatus {
    /// system(3)'s status: the wait status as it is.
    fn into_raw_status_code(self) -> i32 {
        use std::os::unix::process::ExitStatusExt;
        self.into_raw()
    }
}

/// An executable in the runtime's bin/.
fn runtime_bin(runtime: &str, name: &str) -> Option<String> {
    if runtime.is_empty() {
        return None;
    }
    let path = format!("{runtime}/bin/{name}");
    let c = std::ffi::CString::new(path.as_str()).ok()?;
    // SAFETY: access with a NUL-terminated path.
    (unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0).then_some(path)
}

/// The speaker's output latency for the guest (DroidVMAudio.cpp as staged publishes the boot argument
/// oneios-audio-latency=FRAMES as the device's "output latency"): what the helper measures on its stream before the
/// guest boots (the guest's speaker hands each period on as it starts, so nothing is added for the guest), unless the
/// boot arguments already say it. ONEIOS_AUDIO_PROBE=0: no probe, the kernel's default.
fn audio_latency_arg(config: &mut Config, audio: &dyn AudioBackend) {
    const KEY: &str = "oneios-audio-latency=";
    if let Some(at) = config.boot_args.find(KEY) {
        let rest = &config.boot_args[at + KEY.len()..];
        let frames = xh_core::parse::leading_u64(rest, 10).map_or(0, |(v, _)| v as u32);
        audio.told(frames);
        logi!(
            "audio: output latency {} frames ({} ms): the boot arguments' oneios-audio-latency, not measured",
            frames,
            frames / 48
        );
        return;
    }
    if std::env::var("ONEIOS_AUDIO_PROBE").is_ok_and(|v| v == "0") {
        logi!("audio: ONEIOS_AUDIO_PROBE=0: the kernel's default output latency");
        return;
    }
    // 3 s: on a dead MMAP stream the helper gives up as soon as its writer has counted 600 ms of timeouts (about a
    // second), and its fallback opens a mixer track without waiting on that track (the probe's 1.5 s are only spent on
    // a stream that reads but gives no timestamp). The old 2 s left too little room for the fallback's open.
    let frames = match audio.probe_latency(3000) {
        Ok(f) if f > 0 => f,
        Ok(_) => {
            logi!(
                "audio: speaker latency not measured ({}): the kernel's default output latency",
                xh_core::sys::strerror(libc::EIO)
            );
            return;
        }
        Err(e) => {
            logi!(
                "audio: speaker latency not measured ({}): the kernel's default output latency",
                xh_core::sys::strerror(e)
            );
            return;
        }
    };
    if add_boot_arg(&mut config.boot_args, &format!("{KEY}{frames}")).is_err() {
        loge!(
            "audio: no room in the boot arguments for oneios-audio-latency={}: the kernel's default output latency",
            frames
        );
        return;
    }
    audio.told(frames);
    logi!(
        "audio: speaker latency {} frames ({} ms) measured by the helper: oneios-audio-latency={}",
        frames,
        frames / 48,
        frames
    );
}

/// The guest codec archive (kernel/patches/oneios-hosted/codec) publishes iOS's paravirtualized VideoToolbox driver
/// with this boot argument (DroidVMVTParavirt.cpp); droidvm-media (default on) brings up the transport.
fn encoder_boot_argument(config: &mut Config) {
    if !config.boot_args.contains("droidvm-vt-paravirt=") {
        let _ = add_boot_arg(&mut config.boot_args, "droidvm-vt-paravirt=1");
    }
}

/// The encoder device over a backend, in the media pool, decoding with what `decoder` makes over the pool's host
/// mapping (2026-11-24: the helper's MediaCodec in a session, the stand-in on a PC boot; none: an encoder only).
fn encoder_device(
    slots: &mut Slots,
    machine: &mut Machine,
    pool: MediaPool,
    backend: Arc<dyn EncoderBackend>,
    decoder: impl FnOnce(PoolMem) -> Option<Arc<dyn DecodeBackend>>,
) -> bool {
    let host = crate::mem::ram(pool.pa, pool.bytes).map_or(std::ptr::null_mut(), |p| p.as_ptr());
    // SAFETY: the media pool is guest RAM mapped for the process's life (mem::map), pool.bytes long at `host`.
    let decoder = unsafe { PoolMem::new(host, pool.bytes) }.and_then(decoder);
    let (base, spi) = slots.next();
    // SAFETY: as above.
    let dev = unsafe { oneencoder::media_device::create(base, spi, Some(backend), decoder, host, pool.pa, pool.bytes) };
    if dev.is_none() {
        return false;
    }
    machine.media_pool_base = pool.pa;
    machine.media_pool_size = pool.bytes;
    slots.add(DeviceKind::Media);
    true
}

/// A bridge device with its config; adds its slot.
fn bridge_device(slots: &mut Slots, config: BridgeConfig) -> bool {
    let (base, spi) = slots.next();
    let ok = bridge_dev::create(base, spi, config).is_some();
    if ok {
        slots.add(DeviceKind::Bridge);
    }
    ok
}

/// The phone's Wi-Fi or a tap or the stand-in radio as the guest's en0.
fn net_device(slots: &mut Slots, net: &NetConfig, open: onebridges::net::NetOpen) -> bool {
    let (base, spi) = slots.next();
    let ok = virtio_net::create(base, spi, net, Some(open)).is_some();
    if ok {
        slots.add(DeviceKind::Net);
    }
    ok
}

impl Bridges {
    /// main.c's bridge part of boot(): every bridge the config asks for, its device in the next slot.
    pub fn start(
        config: &mut Config,
        machine: &mut Machine,
        slots: &mut Slots,
        pool: MediaPool,
        pen_ic: bool,
        sensors: Option<(Arc<Hub>, Option<crate::virtio_input::InputDev>)>,
    ) -> Bridges {
        let mut b = Bridges { runtime: config.runtime.clone(), ..Bridges::default() };
        let planned = config.bridges && plan_cpus(config, machine);
        if !planned && config.cpu_pin == CpuPin::Always {
            pin_by_index(config);
        }
        let encoder_sim = config.encoder == Encoder::Sim;
        if config.bridges {
            b.start_session_bridges(config, machine, slots, pool, pen_ic, encoder_sim, sensors);
        }
        // The video encoder with no phone: encoder=sim, or encoder=auto with the bridges off (a PC boot: no MediaCodec
        // there, and the Android helper cannot run). The stand-in encoder runs inside xnuhost behind the same device
        // and answers each frame with a real HEVC or H.264 stream that carries the frame's own pixels uncompressed
        // (I_PCM): iOS's paravirtualized encoder, CMPhoto's HEIC writer and whoever reads the file back all run as on
        // the phone. Never in a session on the phone (the bridges on) unless encoder=sim asks for it.
        if pool.bytes != 0 && (encoder_sim || !config.bridges) {
            let stub: Arc<dyn EncoderBackend> = Arc::new(oneencoder::backend_stub::StubBackend);
            let standin = |pool| Some(Arc::new(StubDecodeBackend::new(pool)) as Arc<dyn DecodeBackend>);
            if !slots.full() && encoder_device(slots, machine, pool, stub, standin) {
                encoder_boot_argument(config);
                logi!(
                    "encoder: the stand-in encoder ({}): no MediaCodec; every frame is coded as it is (I_PCM), no compression",
                    if encoder_sim { "encoder=sim" } else { "encoder=auto with the bridges off: no phone" }
                );
            } else {
                logi!("encoder: off: no device for the stand-in encoder");
            }
        }
        // wifi=sim: a made-up radio (runtime/bridges/wifi's sim) for a boot with no phone, with the bridges on or off:
        // the guest's Wi-Fi driver and wifid get two networks to scan and join; no frame goes anywhere.
        if config.wifi == Wifi::Sim && !slots.full() {
            let net = NetConfig { guest_mac: machine.mac(Mac::Wifi), ..NetConfig::default() };
            if let Some(open) = onebridges::wifi::sim::prepare(net.guest_mac) {
                net_device(slots, &net, open);
            }
        }
        // bluetooth=sim and/or haptics=sim with the bridges off (a PC boot): a bridge device that serves only the
        // stand-ins.
        let (bt_sim, haptic_sim) = (config.bluetooth == Bluetooth::Sim, config.haptics == Haptics::Sim);
        if !config.bridges && (bt_sim || haptic_sim) && !slots.full() {
            if haptic_sim {
                b.haptic = Haptic::open_sim().map(Arc::new);
            }
            let factory: Option<devices::HciFactory> =
                bt_sim.then(|| Box::new(hci_sim::open_sim) as devices::HciFactory);
            let bridge = BridgeConfig { haptic: b.haptic.clone(), hci_factory: factory, ..BridgeConfig::default() };
            if bridge_device(slots, bridge) {
                if bt_sim {
                    bt_sim_on(machine);
                }
            } else {
                logi!(
                    "{}: off: no bridge device for the stand-in's messages",
                    if bt_sim { "bluetooth" } else { "haptics" }
                );
            }
        }
        b
    }

    // (one call, from start_bridges above: the parts it starts are its arguments)
    #[allow(clippy::too_many_arguments)]
    fn start_session_bridges(
        &mut self,
        config: &mut Config,
        machine: &mut Machine,
        slots: &mut Slots,
        pool: MediaPool,
        pen_ic: bool,
        encoder_sim: bool,
        sensors: Option<(Arc<Hub>, Option<crate::virtio_input::InputDev>)>,
    ) {
        // Bluetooth: the phone's controller through its HAL (hidl), or none (virtual). auto: hidl when the HAL is
        // registered; the HAL itself is opened only when bluetoothd connects.
        let sim = config.bluetooth == Bluetooth::Sim;
        let mut hidl = config.bluetooth == Bluetooth::Hidl;
        let mut hal = 0;
        if config.bluetooth == Bluetooth::Auto {
            hal = hci_hidl::probe();
            hidl = hal > 0;
        }
        // Haptics: the phone's force-feedback device (auto), none (off), or the stand-in actuator (sim).
        // haptics-native-impacts (config.rs checked treble, no fallback, and put oneios_native_impacts=1 in the boot
        // arguments): the bridge asks the HAL for its presets and offers OB_HAPTIC_IMPACT only when TICK, CLICK and
        // HEAVY_CLICK are there.
        haptics::set_native_impacts(config.haptics_native_impacts);
        if config.haptics_native_impacts == Some(true) {
            logi!("haptics: native impacts asked for (haptics-native-impacts=1: the vibrator HAL's presets, guest oneios_native_impacts=1)");
        }
        self.haptic = match config.haptics {
            Haptics::Sim => Haptic::open_sim(),
            Haptics::Off => None,
            Haptics::Auto => Haptic::open(None),
        }
        .map(Arc::new);
        if config.haptics == Haptics::Off {
            logi!("haptics: off (haptics=off): iOS gets no actuator");
        }
        let mut bright = brightness::Options::from_env();
        if !config.touch_sleep {
            bright.no_touch_off = true; // touch-sleep=0 (ONEIOS_TSP_OFF=0)
        }
        if pen_ic {
            bright.pen_ic = true; // the host's pen given to the guest (ONEIOS_PEN_IC=1)
        }
        // The light: the sensor hub's front sensor when it has one (sensors=auto|ssc); with sensors=ssc and none, no light
        // at all (the rear one gives 0 lux on the S24); else the rear evdev sensor as before.
        let hub_light = sensors.as_ref().is_some_and(|(h, _)| h.has(SensorKind::Light));
        if hub_light {
            let n = sensors.as_ref().map_or(0, |(h, _)| h.found().lights);
            bright.als_external =
                Some(format!("the sensor hub's ambient_light (SSC, on change; {n} light sensor(s) there)"));
        } else if config.sensors == cfg::Sensors::Ssc {
            bright.no_als = true;
            logi!("brightness: no light sensor: sensors=ssc and the sensor hub has no ambient_light (the rear sensor is not used)");
        }
        self.brightness = Bright::open(&bright).map(Arc::new);
        // The hub's streams: motion frames to the guest's motion device, light readings to the brightness bridge.
        if let Some((hub, motion_dev)) = sensors {
            let motion: Option<MotionFn> =
                motion_dev.map(|dev| Arc::new(move |m: &Motion| crate::motion::send(&dev, m)) as MotionFn);
            let light: Option<LightFn> = self
                .brightness
                .clone()
                .filter(|_| hub_light)
                .map(|b| Arc::new(move |mlx: u32| b.als_feed(mlx)) as LightFn);
            if hub.start(motion, light) {
                logi!(
                    "sensors: streaming: {} (paused while iOS's display is off)",
                    hub.found().sensors.iter().map(|(k, ..)| k.name()).collect::<Vec<_>>().join(", ")
                );
                self.sensors = Some(hub);
            }
        }
        if let Some(br) = &self.brightness {
            let _ = BRIGHT.set(br.clone());
            crate::input::set_power_key(Some(power_key_pressed)); // the touch IC is woken at each press
        }
        self.battery = Battery::open(&battery::Options::from_env()).map(Arc::new);
        let mut topts = thermal::Options::from_env();
        if !config.thermal_cap {
            topts.cap_off = true;
        }
        self.thermal = Thermal::open(&topts).map(Arc::new);
        if let Some(t) = &self.thermal {
            t.on_cap(Some(Arc::new(thermal_step)));
        }
        let factory: Option<devices::HciFactory> = if hidl {
            Some(Box::new(hci_hidl::open_hidl))
        } else if sim {
            Some(Box::new(hci_sim::open_sim))
        } else {
            None
        };
        let bridge = BridgeConfig {
            haptic: self.haptic.clone(),
            hci_factory: factory,
            bt_address_path: None,
            brightness: self.brightness.clone(),
            battery: self.battery.clone(),
            thermal: self.thermal.clone(),
        };
        let have_bridge = bridge_device(slots, bridge);
        if sim && have_bridge {
            bt_sim_on(machine);
        } else if sim {
            logi!("bluetooth: off: no bridge device for the stand-in controller's packets");
        } else if hidl && have_bridge {
            // The phone's controller, H4 over the bridge. bluetoothd's encodings (24A446, read from the binary): 1 H4,
            // 2 H4DS, 3 H4BC, 10 virtual. Plain H4 is the one UniXNU carries to this bluetoothd over the same Skywalk
            // UART pipe (drivers/bluetooth/oneios_bt_sk.c); the boot argument bt-transport-override=N tries another.
            machine.bt_transport = 1;
            machine.bt_speed = 3_000_000;
            let address = hci::bt_address("/mnt/vendor/efs/bluetooth/bt_addr");
            machine.bt_mac = address.unwrap_or_else(|| machine.mac(Mac::Bluetooth));
            logi!(
                "bluetooth: on: the phone's controller through its HAL{}, address {}{}; the HAL is opened when bluetoothd connects",
                match hal {
                    11 => " 1.1",
                    10 => " 1.0",
                    hci_hidl::PROBE_AIDL => " (AIDL)",
                    _ => "",
                },
                mac_text(&machine.bt_mac),
                if address.is_some() { "" } else { " (made up: /mnt/vendor/efs/bluetooth/bt_addr not read)" }
            );
        } else if hidl {
            logi!("bluetooth: off: no bridge device for the controller's packets");
        } else {
            logi!(
                "bluetooth: off ({}): iOS gets no controller{}",
                config.bluetooth,
                if config.bluetooth == Bluetooth::Virtual {
                    "; bluetooth=hidl in xnuhost.conf (or --bluetooth hidl) for the phone's"
                } else {
                    ""
                }
            );
        }

        // The network: the phone's Wi-Fi. wlan0 is iOS's for the session (docs/hardware.md section 3): en0 has wlan0's
        // address, frames go between them as they are, scans and joins go to the vendor's Wi-Fi stack; no NAT.
        // wifi=tap: the old route (a tap and oneios-net.sh's NAT), for a bench.
        let mut net = NetConfig { guest_mac: machine.mac(Mac::Wifi), ..NetConfig::default() }; // en0 (the tap's)
        match config.wifi {
            Wifi::Tap => {
                if net_device(slots, &net, Net::tap_opener()) {
                    self.net_up = net_script(&config.runtime, "up"); // address, forwarding and NAT; undone at stop
                }
            }
            Wifi::Cell => {
                // The phone's uplink as a made-up, already-joined network with no 802.11 (wifi::cell): iOS's
                // configd leases an address from the bridge and its frames go to the tap and oneios-net.sh's NAT,
                // so data works over mobile data when there is no Wi-Fi.
                let open = onebridges::wifi::cell::prepare(net.guest_mac);
                if net_device(slots, &net, open) {
                    self.net_up = net_script(&config.runtime, "up");
                }
            }
            Wifi::Auto => {
                let session = Some(config.session.as_str()).filter(|s| !s.is_empty());
                if let Some((open, mac)) = onebridges::wifi::wlan::prepare("wlan0", session) {
                    net.guest_mac = mac; // the Wi-Fi's is wlan0's
                    if net_device(slots, &net, open) {
                        self.wifi_up = true;
                    } else {
                        onebridges::wifi::wlan::close();
                    }
                }
            }
            Wifi::Off => logi!("wifi: off (wifi=off): iOS gets no network device"),
            Wifi::Sim => {}
        }

        // Sound: the helper's stream (bin/oneios-audio), else silence.
        let audio: Arc<dyn AudioBackend> =
            match runtime_path(&config.runtime, "oneios-audio").and_then(|p| AudioHelper::spawn(&p)) {
                Some(h) => Arc::new(h),
                None => Arc::new(NullAudio),
            };
        let (base, spi) = slots.next();
        if virtio_snd::create(base, spi, audio.clone()).is_some() {
            slots.add(DeviceKind::Snd);
            audio_latency_arg(config, audio.as_ref());
        }

        // The camera: a helper process (Android's camera libraries are shared libraries) that opens a camera only when
        // the guest asks. ONEIOS_CAMERA=0 leaves it out; ONEIOS_CAMERA_BACKEND picks its back end (auto).
        let camera_off = std::env::var("ONEIOS_CAMERA").is_ok_and(|v| v == "0");
        if !slots.full() && !camera_off {
            if let Some(helper) = runtime_bin(&config.runtime, "oneios-camera") {
                let backend = std::env::var("ONEIOS_CAMERA_BACKEND").ok();
                self.camera = Camera::helper(&helper, backend.as_deref()).map(Arc::new);
                if let Some(cam) = &self.camera {
                    let (base, spi) = slots.next();
                    if onecamera::virtio_camera::create(base, spi, Some(cam.clone())).is_some() {
                        slots.add(DeviceKind::Camera);
                    }
                }
            }
        }

        // The video encoder (encoder=auto): the "droidvm,virtio-media" device over Android's MediaCodec, for iOS's
        // paravirtualized VideoToolbox encoder (HEIC still images, then video). Off by default until a phone session
        // proves it; both samsung and treble use MediaCodec, so one backend serves both. Its V4L2 buffers live in the
        // media pool carved above. (encoder=sim: the stand-in, in start.)
        if pool.bytes != 0 && !encoder_sim {
            match runtime_bin(&config.runtime, "oneios-encoder").filter(|_| !slots.full()) {
                Some(helper) => {
                    onebridges::hw::select(Bridge::Encoder, Backend::Unset);
                    let helper = oneencoder::backend_helper::HelperBackend::new(&helper);
                    // the decoder's pictures go straight into the pool: the helper maps it from the RAM's memfd
                    let m = crate::mem::state();
                    if m.fd >= 0 && pool.pa >= m.fd_phys {
                        helper.set_pool(m.fd, pool.pa - m.fd_phys, pool.bytes);
                    }
                    let decoder = Some(helper.clone() as Arc<dyn DecodeBackend>);
                    let backend: Arc<dyn EncoderBackend> = helper;
                    if encoder_device(slots, machine, pool, backend, |_| decoder) {
                        encoder_boot_argument(config);
                    }
                }
                None => logi!(
                    "encoder: off: {}",
                    if slots.full() {
                        "no device slot left"
                    } else {
                        "no bin/oneios-encoder in the runtime (the MediaCodec helper)"
                    }
                ),
            }
        }
    }

    /// The cameras iOS is told about (the loader's /arm-io/isp and /product/camera): the roles the helper lists.
    /// MobileGestalt answers still-camera and the facing ones once and keeps them in its cache, so they must be in the
    /// tree from the first boot: the list is waited for here, asleep (the phone, 2026-11-22: it came 170 ms after the
    /// helper's start, 115 ms after the tree had been placed), at most 3 s. No list, or no camera in it: no nodes.
    pub fn describe_cameras(&self, config: &mut Config, machine: &mut Machine) {
        let Some(camera) = &self.camera else { return };
        let list = camera.list(onecamera::proto::OC_MAX_CAMERAS as usize, Duration::from_millis(3000));
        for c in &list {
            if c.role == OC_ROLE_FRONT {
                machine.cameras |= xm::CAMERA_FRONT;
            } else if (OC_ROLE_WIDE..=OC_ROLE_TELE5).contains(&c.role) {
                machine.cameras |= xm::CAMERA_REAR;
            }
        }
        let rear = machine.cameras & xm::CAMERA_REAR != 0;
        let front = machine.cameras & xm::CAMERA_FRONT != 0;
        logi!(
            "camera: iOS is told of {}{}{} ({} cameras from the helper)",
            if rear { "a rear camera" } else { "" },
            if rear && front { " and " } else { "" },
            if front {
                "a front camera"
            } else if machine.cameras != 0 {
                ""
            } else {
                "no camera"
            },
            list.len()
        );
        if machine.cameras != 0 {
            Self::camera_identity(config, machine);
        }
    }

    /// With the cameras (main.c, 2026-11-23): 24A446's CMCapture in a VM (FigCaptureIsRunningInVirtualization:
    /// kern.hv_vmm_present) takes the "external camera replaces built-in" path: -[BWFigCaptureDevice
    /// initWithFigCaptureDevice:deviceID:] drops every Back, BackTelephoto and BackSuperWide stream, so no back source
    /// opens and the Camera app stays black (the phone, 2026-11-23: "unable to find a stream with port type PortTypeBack",
    /// err -12780). The kernel's per-process owner-hide-vmm tells only cameracaptured (every caller of the check is in
    /// it) that it is not in one; bluetoothd and the others keep the VM.
    fn camera_identity(config: &mut Config, machine: &mut Machine) {
        let hide = config.camera_vm == CameraVm::Hide;
        let added = if hide {
            cfg::boot_args_hide_vmm_add(&mut config.boot_args, cfg::BOOT_ARGS_SIZE, "cameracaptured")
        } else {
            Ok(false)
        };
        match added {
            Err(_) => loge!(
                "camera: no room in the boot arguments for owner-hide-vmm=cameracaptured: CMCapture drops the back cameras in a VM"
            ),
            Ok(added) if hide => logi!(
                "camera: cameracaptured is not told of the VM (owner-hide-vmm{}): CMCapture keeps the back cameras only outside one",
                if added { " gets it" } else { " has it" }
            ),
            Ok(_) => logi!("camera: camera-vm=show: cameracaptured sees the VM, and CMCapture drops the back cameras"),
        }
        // /product unique-model (camera-model=): MobileGestalt's HWModelDescriptionForCamera, which both CMCapture's
        // folder and AVGestalt's table go by. The default is in no AVGestalt table (VirtualAudio starts; the kernel,
        // hosted 0088, shows CMCapture.framework/<name less AP> as a link to V54); V54AP stops all audio
        // (docs/hardware.md 7).
        machine.unique_model = Some(config.camera_model.clone());
        let v54 = config.camera_model == "V54AP";
        logi!(
            "camera: unique-model {} (camera-model=): CMCapture's tables {}, AVGestalt {}",
            config.camera_model,
            if v54 { "in V54" } else { "through the kernel's link to V54" },
            if v54 {
                "answers V54's (its webcam subport stops VirtualAudio)"
            } else {
                "has no entry (VirtualAudio builds no webcam subport)"
            }
        );
    }

    /// Every 60 s: the haptics' counters when they moved (docs/booter.md "Health lines"). Before, they were one line at
    /// the stop only, so a session that ended by a kill or a panic said nothing of what iOS asked of the motor.
    pub fn haptic_tick(&self) {
        let Some(h) = &self.haptic else { return };
        let now = super::mono_s();
        let mut shown = self.haptic_shown.lock().unwrap_or_else(|e| e.into_inner());
        let Some((last, was)) = shown.as_ref() else {
            *shown = Some((now, haptics::HapticStats::default()));
            return;
        };
        if now - last < 60 {
            return;
        }
        let s = h.stats();
        if s != *was {
            logi!(
                "haptics: {} s: +{} requests (+{} vibrate, +{} stop, +{} impacts), +{} played, +{} failed",
                now - last,
                (s.vib + s.stop + s.impacts).wrapping_sub(was.vib + was.stop + was.impacts),
                s.vib.wrapping_sub(was.vib),
                s.stop.wrapping_sub(was.stop),
                s.impacts.wrapping_sub(was.impacts),
                s.played.wrapping_sub(was.played),
                s.failed.wrapping_sub(was.failed)
            );
        }
        *shown = Some((now, s));
    }

    /// The stop: each bridge closed, the phone's state as it was.
    pub fn close(&mut self) {
        // The bridge device holds Arcs of the bridges for the process's life and the exit runs no destructor: what
        // main.c's br_*_close did is done here, explicitly.
        if let Some(s) = self.sensors.take() {
            s.close(); // every stream stopped on the hub, its sockets closed
        }
        if let Some(h) = self.haptic.take() {
            h.shutdown(); // the counters' line, then the motor stopped
        }
        if let Some(c) = self.camera.take() {
            c.close(); // the helper closes the camera and exits
        }
        if let Some(b) = self.brightness.take() {
            b.shutdown(); // One UI's backlight level again, the touch and pen ICs awake, the light sensor as it was
        }
        self.battery = None; // read only: the worker stops
        self.thermal = None; // read only; the step was affinities of our own threads
        if self.wifi_up {
            onebridges::wifi::wlan::close(); // the network left, the supplicant and wlan0 as One UI left them
        }
        if self.net_up {
            net_script(&self.runtime, "down");
        }
    }
}

/// `<runtime>/bin/<name>` (existence is the helper's to say).
fn runtime_path(runtime: &str, name: &str) -> Option<String> {
    (!runtime.is_empty()).then(|| format!("{runtime}/bin/{name}"))
}

/// Whether a bridge's request for the guest to shut down could be sent (the return's graceful path).
pub fn request_shutdown() -> bool {
    devices::request_shutdown()
}
