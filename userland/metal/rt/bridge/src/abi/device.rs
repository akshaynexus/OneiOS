// SPDX-License-Identifier: MIT
//! The device: Turnip opened with no loader (crate::driver), rt's device on it, the bridge's index grants, its retire gate
//! and small-buffer pools; the device answers the frontend installs as methods; the counts of its telemetry rows.

use super::{copy_out, e, get, guard, release, boxed};
use crate::indices::Indices;
use metal2vulkan_runtime as rt;
use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// The order in which command buffers were made, and those not yet retired. Whatever a released object leaves (a texture or
/// sampler index, a small buffer's range, a heap's range) may be held by any command buffer made before its release, so it is
/// handed out again only once every such command buffer retired (rt's "Set 1 and index grants": the consumer keeps a released
/// ID back until then; the same holds for the bridge's own ranges of blocks).
#[derive(Default)]
pub struct RetireGate {
    next: AtomicU64,
    inner: Mutex<GateInner>,
}

#[derive(Default)]
struct GateInner {
    open: BTreeSet<u64>,
    waiting: Vec<(u64, Box<dyn FnOnce() + Send>)>,
}

impl RetireGate {
    /// A new command buffer's number, open until `close`.
    pub fn open(&self) -> u64 {
        let seq = self.next.fetch_add(1, Relaxed);
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).open.insert(seq);
        seq
    }

    /// The command buffer retired (or was released never committed): runs what waited only for it and those before it.
    pub fn close(&self, seq: u64) {
        let ready = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.open.remove(&seq);
            Self::take_ready(&mut inner)
        };
        for f in ready {
            f();
        }
    }

    /// Runs `f` once every command buffer made so far has retired: at once where none is open.
    pub fn after_retire(&self, f: impl FnOnce() + Send + 'static) {
        let stamp = self.next.load(Relaxed);
        let run_now = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if inner.open.first().is_none_or(|&first| first >= stamp) {
                true
            } else {
                inner.waiting.push((stamp, Box::new(f)));
                return;
            }
        };
        if run_now {
            f();
        }
    }

    fn take_ready(inner: &mut GateInner) -> Vec<Box<dyn FnOnce() + Send>> {
        let low = inner.open.first().copied().unwrap_or(u64::MAX);
        let mut ready = Vec::new();
        let mut i = 0;
        while i < inner.waiting.len() {
            if inner.waiting[i].0 <= low {
                ready.push(inner.waiting.swap_remove(i).1);
            } else {
                i += 1;
            }
        }
        ready
    }

    /// Releases waiting for retirement.
    pub fn waiting(&self) -> usize {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).waiting.len()
    }
}

pub struct NmbDevice {
    pub(crate) device: rt::Device,
    pub(crate) indices: Arc<Indices>,
    pub(crate) gate: Arc<RetireGate>,
    pub(crate) pools: Arc<super::memory::SmallPools>,
    pub(crate) name: String,
    pub(crate) limits: ash::vk::PhysicalDeviceLimits,
    pub(crate) instance: rt::Instance,
    pub(crate) physical: rt::PhysicalDevice,
}

static PANIC_NOTE: std::sync::atomic::AtomicPtr<c_void> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Sends each panic's message, place and thread to `note` before the panic unwinds, then runs the hook installed before (rt's
/// threads catch no panic; on arm64e an unwind through them failed a pointer authentication, so the text is noted first).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_set_panic_note(note: Option<extern "C" fn(*const c_char)>) {
    PANIC_NOTE.store(note.map_or(std::ptr::null_mut(), |f| f as *mut c_void), std::sync::atomic::Ordering::Release);
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let note = PANIC_NOTE.load(std::sync::atomic::Ordering::Acquire);
            if !note.is_null() {
                let thread = std::thread::current();
                let text = format!("rust panic on {}: {info}", thread.name().unwrap_or("?")).replace('\0', " ");
                if let Ok(text) = std::ffi::CString::new(text) {
                    // SAFETY: `note` was stored from an `extern "C" fn(*const c_char)`.
                    let note: extern "C" fn(*const c_char) = unsafe { std::mem::transmute(note) };
                    note(text.as_ptr());
                }
            }
            earlier(info);
        }));
    });
}

/// Opens the driver at `loader` (Turnip's dylib: its own instance entry, no Vulkan loader) and rt's device on its first device
/// above rt's floor, with its pipeline cache in `cache`.
pub fn open(loader: &str, cache: Option<&str>) -> Result<NmbDevice, String> {
    let driver = crate::driver::open(loader)?;
    let instance = driver.instance().map_err(e("instance"))?;
    let devices = instance.physical_devices().map_err(e("devices"))?;
    let mut refused = Vec::new();
    for physical in devices {
        if let Err(below) = physical.check() {
            refused.push(format!("{}: {below}", crate::driver::device_name(&instance, &physical)));
            continue;
        }
        let name = crate::driver::device_name(&instance, &physical);
        let mut options = rt::DeviceOptions::default();
        options.pipeline_cache_dir = cache.map(Into::into);
        // rt 0005, off unless METAL2VULKAN_DONT_CARE_STORES=1 (boot word oneios-dont-care=1, an A/B of the speed only): rt
        // fixes a store as the rendering begins, and an encoder that ends its rendering early (a memory barrier, an
        // attachment read's feedback move, a copy of a texture over a buffer: RenderCommandEncoder::interrupt) begins it
        // again with LOAD. Turnip leaves a DONT_CARE store's tiles unwritten, so the draws before such a split were lost
        // (read in the code 2026-11-28 as the cause of v0.1.40's garbage band in page transitions). Stored, a split keeps them.
        options.dont_care_stores =
            name.starts_with("Turnip") && std::env::var("METAL2VULKAN_DONT_CARE_STORES").is_ok_and(|v| v == "1");
        // rt 0006: the queues' global priority the plug-in asks for (high in the render server, so the compositor's frames do
        // not wait behind an app's), through whichever of the two extensions the device lists.
        options.queue_global_priority = match std::env::var("METAL2VULKAN_QUEUE_PRIORITY").as_deref() {
            Ok("low") => Some(ash::vk::QueueGlobalPriorityKHR::LOW),
            Ok("medium") => Some(ash::vk::QueueGlobalPriorityKHR::MEDIUM),
            Ok("high") => Some(ash::vk::QueueGlobalPriorityKHR::HIGH),
            Ok("realtime") => Some(ash::vk::QueueGlobalPriorityKHR::REALTIME),
            _ => None,
        };
        if options.queue_global_priority.is_some() {
            options.extensions.push(rt::DeviceExtension::new(c"VK_KHR_global_priority"));
            options.extensions.push(rt::DeviceExtension::new(c"VK_EXT_global_priority"));
        }
        // A driver may refuse the priority (VK_ERROR_NOT_PERMITTED_KHR: ANV without CAP_SYS_NICE): the device is made
        // at the default one then, as Vulkan asks of an application.
        let device = match rt::Device::new(&physical, options.clone()) {
            Err(why) if options.queue_global_priority.is_some() && format!("{why}").contains("NOT_PERMITTED") => {
                options.queue_global_priority = None;
                rt::Device::new(&physical, options).map_err(e("device"))?
            }
            made => made.map_err(e("device"))?,
        };
        let indices = Arc::new(Indices::grant(&device).map_err(e("index grants"))?);
        let (_, raw) = instance.raw();
        // SAFETY: the instance listed the device.
        let limits = unsafe { raw.get_physical_device_properties(physical.raw()) }.limits;
        return Ok(NmbDevice {
            device,
            indices,
            gate: Arc::default(),
            pools: Arc::default(),
            name,
            limits,
            instance,
            physical,
        });
    }
    Err(format!("no device above metal2vulkan-rt's floor: {}", refused.join("; ")))
}

/// # Safety
/// `loader` and `cache` are C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_device_open(loader: *const c_char, cache: *const c_char) -> *mut NmbDevice {
    guard(std::ptr::null_mut(), || {
        if loader.is_null() || cache.is_null() {
            return Err("loader and cache are required".into());
        }
        // SAFETY: the caller's contract.
        let (loader, cache) = unsafe { (CStr::from_ptr(loader), CStr::from_ptr(cache)) };
        let device = boxed(open(&loader.to_string_lossy(), Some(&cache.to_string_lossy()))?);
        // The process's device, for the plug-in's rows that name none (m2v_memory_census).
        CURRENT.store(device, std::sync::atomic::Ordering::Release);
        Ok(device)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_release(device: *mut NmbDevice) {
    let _ = CURRENT.compare_exchange(device, std::ptr::null_mut(), std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
    release(device)
}

/// The device nmb_device_open made last in this process (the frontend makes one).
static CURRENT: std::sync::atomic::AtomicPtr<NmbDevice> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Render encoders ended (abi::command), for the census's pass words.
pub(crate) static RENDER_ENCODERS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_name(device: *const NmbDevice, out: *mut c_char, size: usize) -> usize {
    guard(0, || Ok(copy_out(&get(device)?.name, out, size)))
}

/// Metal's maxBufferLength: rt's own bound and the device's.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_max_buffer_length(device: *const NmbDevice) -> u64 {
    guard(0, || {
        let d = get(device)?;
        Ok(rt::MAX_BUFFER_LENGTH.min(d.limits.max_storage_buffer_range as u64).max(256 << 20).min(rt::MAX_BUFFER_LENGTH))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_max_threads_per_threadgroup(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        if out.is_null() {
            return Err("null size".into());
        }
        let l = &get(device)?.limits;
        let s = l.max_compute_work_group_size;
        // SAFETY: the header's three words.
        unsafe { std::slice::from_raw_parts_mut(out, 3) }.copy_from_slice(&[s[0].min(1024) as u64, s[1].min(1024) as u64, s[2].min(1024) as u64]);
        Ok(true)
    })
}

/// The translator source the runtime was built from (pin and patches), which keys the shader priming (PrimeShaders.h).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_runtime_build() -> *const c_char {
    concat!(env!("NMB_RUNTIME_ID"), "\0").as_ptr().cast()
}

static SCANOUT_CONVERT_AIR: &[u8] = include_bytes!(env!("NMB_SCANOUT_AIR"));
static FUNCTION_CONSTANT_AIR: &[u8] = include_bytes!(env!("NMB_FC_AIR"));
pub(crate) static FIXTURE_AIR: &[u8] = include_bytes!(env!("NMB_FIXTURE_AIR"));

fn air(bytes: &'static [u8], length: *mut usize) -> *const c_void {
    // SAFETY: the caller's contract (a writable length or null).
    if let Some(length) = unsafe { length.as_mut() } {
        *length = bytes.len();
    }
    bytes.as_ptr() as *const c_void
}

/// # Safety
/// `length` is writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_scanout_convert_air(length: *mut usize) -> *const c_void {
    air(SCANOUT_CONVERT_AIR, length)
}

/// # Safety
/// `length` is writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_fixture_air(length: *mut usize) -> *const c_void {
    air(FIXTURE_AIR, length)
}

/// # Safety
/// `length` is writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_constant_air(length: *mut usize) -> *const c_void {
    air(FUNCTION_CONSTANT_AIR, length)
}

fn fill(out: *mut u64, values: &[u64]) -> bool {
    if out.is_null() {
        return false;
    }
    // SAFETY: the header's `values.len()` words.
    unsafe { std::slice::from_raw_parts_mut(out, values.len()) }.copy_from_slice(values);
    true
}

/// Pipeline makings (count, total us, longest us, waited us, failed), messages (info, warning+), shader cache hits, misses,
/// writes: from rt's metrics where it counts them, else 0.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_work_stats(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        let m = get(device)?.device.counters().metrics();
        Ok(fill(out, &[m.pipelines_created, m.pipeline_creation_nanos / 1000, 0, 0, 0, 0, 0, m.translations_loaded, 0, 0]))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_shader_cache_detail(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        let m = get(device)?.device.counters().metrics();
        let mut words = [0u64; 24];
        words[0] = m.translations_loaded;
        Ok(fill(out, &words))
    })
}

#[repr(C)]
#[derive(Default)]
pub struct NmbShaderCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub rejected: u64,
    pub read_failures: u64,
    pub writes: u64,
    pub write_failures: u64,
    pub evictions: u64,
    pub driver_loads: u64,
    pub driver_hits: u64,
    pub driver_misses: u64,
    pub driver_saves: u64,
    pub driver_truncated: u64,
    pub driver_mismatched: u64,
    pub driver_unchanged: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_shader_cache_stats(device: *const NmbDevice, out: *mut NmbShaderCacheStats) -> bool {
    guard(false, || {
        let m = get(device)?.device.counters().metrics();
        let out = super::get_mut(out)?;
        *out = NmbShaderCacheStats { hits: m.translations_loaded, ..Default::default() };
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_texture_counts(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        let g = get(device)?.device.counters().gauges();
        Ok(fill(out, &[0, 0, 0, g.shadows, 0, 0, 0, 0]))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_pass_counts(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        get(device)?;
        Ok(fill(out, &[0; 10]))
    })
}

/// The MEM row: pooled bytes live and placed, empty kept, dedicated, host imports; allocated so far; pooled and dedicated blocks;
/// blocks made and released. From rt's gauges: its allocations of both classes, the placed blocks and textures, the idle bytes,
/// the textures' own allocations, the imports; the blocks live.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_memory_counts(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        let d = get(device)?;
        let g = d.device.counters().gauges();
        let m = d.device.counters().metrics();
        let allocations = g.shared_allocation_bytes + g.private_allocation_bytes;
        let placed = g.shared_placed_bytes + g.private_placed_bytes + g.texture_block_bytes;
        let idle = g.shared_idle_bytes + g.private_idle_bytes;
        Ok(fill(out, &[allocations, placed, idle, g.texture_own_bytes, g.import_bytes, allocations + g.texture_own_bytes + g.import_bytes,
                       g.blocks, 0, m.consumer_allocations, m.allocations_freed]))
    })
}

/// The old runtime's stage ring (bridge patch 0022) has no counterpart in rt: no records.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_stage_ring_read(from: u64, _out: *mut c_void, _max: usize, next: *mut u64) -> usize {
    // SAFETY: the caller's contract (writable or null).
    if let Some(next) = unsafe { next.as_mut() } {
        *next = from;
    }
    0
}

/// rt frees idle allocations itself after 250 ms (its README, "Memory blocks and buffers"); the bridge gives back its empty small
/// buffer slabs. Returns how many went.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_release_kept_memory(device: *const NmbDevice) -> usize {
    guard(0, || Ok(get(device)?.pools.trim()))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_allocation_sites(_out: *mut u64, _max: usize) -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_no_copy_remaps(out: *mut u64) -> bool {
    fill(out, &[0, 0])
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_scheduler_counts(device: *const NmbDevice, out: *mut u64) -> bool {
    guard(false, || {
        let d = get(device)?;
        let m = d.device.counters().metrics();
        let mut words = [0u64; 16];
        words[0] = m.submissions;
        words[1] = d.gate.waiting() as u64;
        Ok(fill(out, &words))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_trim_memory(device: *const NmbDevice, _idle_ms: u32) -> u32 {
    guard(0, || Ok(get(device)?.pools.trim() as u32))
}

/// The old runtime's hold-back (Droid-VM 0020) has no counterpart: rt keeps one 16 MiB reserve per class and frees the rest
/// after 250 ms idle. The bridge gives back its empty slabs.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_hold_back_memory(device: *const NmbDevice, on: bool) -> usize {
    guard(0, || Ok(if on { get(device)?.pools.trim() } else { 0 }))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_last_warning(out: *mut c_char, size: usize) -> usize {
    guard(0, || Ok(copy_out("", out, size)))
}

// ---- device answers ----

/// Apple's private counts (NSUInteger), as the old runtime (624bab3 + its 51 patches) answered them on Turnip's Adreno 750
/// (dumped on the laptop over the noop DRM shim, 2026-11-24; userland/metal/rt/README.md); `None`: no answer of the device's
/// (the selector is not installed). The limits the Vulkan device states are read from it.
const PRIVATE_COUNTS: &[(&str, Option<u64>)] = &[
    ("gpuCoreCount", None),
    ("maxVertexAmplificationCount", None),
    ("deviceLinearTextureAlignmentBytes", Some(16)),
    ("linearTextureAlignmentBytes", Some(16)),
    ("iosurfaceTextureAlignmentBytes", Some(16)),
    ("iosurfaceReadOnlyTextureAlignmentBytes", Some(16)),
    ("maxVertexBuffers", Some(31)),
    ("maxFragmentBuffers", Some(31)),
    ("maxComputeBuffers", Some(31)),
    ("maxTileBuffers", Some(31)),
    ("maxVertexTextures", Some(128)),
    ("maxFragmentTextures", Some(128)),
    ("maxComputeTextures", Some(128)),
    ("maxTileTextures", Some(128)),
    ("maxVertexSamplers", Some(16)),
    ("maxFragmentSamplers", Some(16)),
    ("maxComputeSamplers", Some(16)),
    ("maxTileSamplers", Some(16)),
    ("maxComputeLocalMemorySizes", Some(31)),
    ("maxComputeThreadgroupMemory", None),
    ("maxComputeThreadgroupMemoryAlignmentBytes", Some(16)),
    ("maxPredicatedNestingDepth", Some(4294967295)),
    ("maxRasterizationRateLayerCount", Some(2)),
    ("maxTextureWidth2D", Some(0)),
    ("maxTextureHeight2D", Some(0)),
    ("maxTextureLayers", Some(0)),
    ("maxTotalComputeThreadsPerThreadgroup", Some(0)),
    ("maxViewportCount", Some(0)),
];

/// Apple's private flags (BOOL), as above.
const PRIVATE_FLAGS: &[(&str, Option<bool>)] = &[
    ("isFramebufferReadSupported", Some(true)),
    ("isLargeMRTSupported", None),
    ("isRGB10A2GammaSupported", Some(true)),
    ("supportsBufferWithAddressRanges", Some(false)),
    ("supportsBufferWithIOSurface", Some(false)),
    ("supportsCommandBufferJump", Some(true)),
    ("supportsComputeCompressedTextureWrite", None),
    ("supportsConditionalLoadStore", None),
    ("supportsDynamicAttributeStride", Some(true)),
    ("supportsExtendedXR10Formats", Some(true)),
    ("supportsFloat16BCubicFiltering", Some(true)),
    ("supportsHeapWithAddressRanges", Some(false)),
    ("supportsImageBlocks", Some(false)),
    ("supportsMemoryOrderAtomics", Some(true)),
    ("supportsNativeHardwareFP16", Some(true)),
    ("supportsNonSquareTileShaders", Some(false)),
    ("supportsProgrammableSamplePositions", Some(true)),
    ("supportsResourceDetachBacking", Some(false)),
    ("supportsSharedTextureHandles", Some(true)),
    ("supportsSIMDReduction", Some(true)),
    ("supportsSIMDShuffleAndFill", Some(true)),
    ("supportsTexture2DMultisampleArray", Some(true)),
    ("supportsTileShaders", Some(false)),
];

/// The public queries, with their kind (NMB_ANSWER_*).
const PUBLIC_ANSWERS: &[(&str, u32)] = &[
    ("maxThreadgroupMemoryLength", 0),
    ("argumentBuffersSupport", 0),
    ("readWriteTextureSupport", 0),
    ("maxColorAttachments", 0),
    ("minConstantBufferAlignmentBytes", 0),
    ("maxArgumentBufferSamplerCount", 0),
    ("deviceLinearReadOnlyTextureAlignmentBytes", 0),
    ("areRasterOrderGroupsSupported", 1),
    ("supportsRasterOrderGroups", 1),
    ("isDepth24Stencil8PixelFormatSupported", 1),
    ("areProgrammableSamplePositionsSupported", 1),
    ("supports32BitFloatFiltering", 1),
    ("supportsMemorylessRenderTargets", 1),
    ("supportsSamplerReductionMode", 1),
    ("supportsYCBCRFormats12", 1),
    ("supportsYCBCRPackedFormats12", 1),
    ("supportsSIMDGroup", 1),
    ("minimumLinearTextureAlignmentForPixelFormat:", 2),
    ("supportsTextureSampleCount:", 3),
    ("supportsVertexAmplificationCount:", 3),
    ("supportsRasterizationRateMapWithLayerCount:", 3),
    ("supportsFamily:", 3),
    ("supportsPrimitiveType:", 3),
    ("supportsTextureWriteRoundingMode:", 3),
];

/// The GPU families `supportsFamily:` answers true: the old bridge's TARGET_FAMILIES (Apple1, Apple2, Apple3, Common1, Common2),
/// which the OS takes its only working path through (Core Image's Metal context needs Apple2; QuartzCore splits frames where
/// Apple3 is false). The old runtime's rows met no family on the Adreno 750 by themselves.
const TARGET_FAMILIES: [i64; 5] = [1001, 1002, 1003, 3001, 3002];

fn answer_selectors() -> &'static [(std::ffi::CString, u32)] {
    static ALL: std::sync::OnceLock<Vec<(std::ffi::CString, u32)>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let counts = PRIVATE_COUNTS.iter().map(|(s, _)| (*s, 0));
        let flags = PRIVATE_FLAGS.iter().map(|(s, _)| (*s, 1));
        counts
            .chain(flags)
            .chain(PUBLIC_ANSWERS.iter().copied())
            .map(|(s, kind)| (std::ffi::CString::new(s).expect("selector"), kind))
            .collect()
    })
}

/// # Safety
/// `kind` is writable or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_device_answer_selector(index: usize, kind: *mut u32) -> *const c_char {
    let Some((selector, k)) = answer_selectors().get(index) else { return std::ptr::null() };
    // SAFETY: the caller's contract.
    if let Some(kind) = unsafe { kind.as_mut() } {
        *kind = *k;
    }
    selector.as_ptr()
}

fn format_has(d: &NmbDevice, format: ash::vk::Format, feature: ash::vk::FormatFeatureFlags) -> bool {
    let (_, raw) = d.instance.raw();
    // SAFETY: the instance listed the device.
    let p = unsafe { raw.get_physical_device_format_properties(d.physical.raw(), format) };
    p.optimal_tiling_features.contains(feature)
}

fn device_answer(d: &NmbDevice, selector: &str, arg: u64) -> Result<u64, String> {
    let l = &d.limits;
    let none = || format!("the device has no answer for {selector}");
    if let Some((_, value)) = PRIVATE_COUNTS.iter().find(|(s, _)| *s == selector) {
        return Ok(match selector {
            "maxTextureWidth2D" | "maxTextureHeight2D" => l.max_image_dimension2_d as u64,
            "maxTextureLayers" => l.max_image_array_layers as u64,
            "maxTotalComputeThreadsPerThreadgroup" => l.max_compute_work_group_invocations.min(1024) as u64,
            "maxViewportCount" => l.max_viewports as u64,
            _ => value.ok_or_else(none)?,
        });
    }
    if let Some((_, value)) = PRIVATE_FLAGS.iter().find(|(s, _)| *s == selector) {
        return value.map(u64::from).ok_or_else(none);
    }
    use ash::vk::{Format, FormatFeatureFlags as F};
    Ok(match selector {
        "maxThreadgroupMemoryLength" => l.max_compute_shared_memory_size as u64,
        "argumentBuffersSupport" => d.device.argument_buffers_support().as_raw() as u64,
        "readWriteTextureSupport" => d.device.read_write_texture_support().as_raw() as u64,
        "maxColorAttachments" => (rt::MAX_COLOR_ATTACHMENTS as u64).min(l.max_color_attachments.min(l.max_fragment_output_attachments) as u64),
        // rt binds a buffer at any offset a scalar's alignment allows.
        "minConstantBufferAlignmentBytes" => 4,
        // Metal's 2048 of a tier 2 device, held to rt's sampler indices (1..3999).
        "maxArgumentBufferSamplerCount" => 2048,
        "deviceLinearReadOnlyTextureAlignmentBytes" => 16,
        "areRasterOrderGroupsSupported" | "supportsRasterOrderGroups" => {
            d.device.extensions().iter().any(|x| x.as_c_str() == ash::ext::fragment_shader_interlock::NAME) as u64
        }
        "isDepth24Stencil8PixelFormatSupported" => format_has(d, Format::D24_UNORM_S8_UINT, F::DEPTH_STENCIL_ATTACHMENT) as u64,
        "areProgrammableSamplePositionsSupported" => 1,
        "supports32BitFloatFiltering" => format_has(d, Format::R32G32B32A32_SFLOAT, F::SAMPLED_IMAGE_FILTER_LINEAR) as u64,
        // A Memoryless texture is a Private one here (super::Options::storage_mode).
        "supportsMemorylessRenderTargets" => 1,
        // The old runtime answered false on the Adreno 750.
        "supportsSamplerReductionMode" => 0,
        // No YCbCr two-plane formats through this bridge yet (abi::texture).
        "supportsYCBCRFormats12" | "supportsYCBCRPackedFormats12" => 0,
        "supportsSIMDGroup" => 1,
        "minimumLinearTextureAlignmentForPixelFormat:" => {
            d.device.minimum_linear_texture_alignment(super::pixel_format(arg, "pixel format")?).map_err(e("linear texture alignment"))?
        }
        "supportsTextureSampleCount:" => d.device.supports_texture_sample_count(arg.min(64) as u32) as u64,
        // As the old runtime answered: 1, 2 and 4 views, rate maps of 1 and 2 layers, every primitive type and rounding mode.
        "supportsVertexAmplificationCount:" => matches!(arg, 1 | 2 | 4) as u64,
        "supportsRasterizationRateMapWithLayerCount:" => matches!(arg, 1 | 2) as u64,
        "supportsFamily:" => TARGET_FAMILIES.contains(&(arg as i64)) as u64,
        "supportsPrimitiveType:" => (arg <= 4) as u64,
        "supportsTextureWriteRoundingMode:" => (arg <= 2) as u64,
        _ => return Err(format!("{selector} is not a device query of the bridge")),
    })
}

/// # Safety
/// `selector` is a C string; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_device_answer(device: *const NmbDevice, selector: *const c_char, arg: u64, out: *mut u64) -> bool {
    guard(false, || {
        if selector.is_null() || out.is_null() {
            return Err("null selector or answer".into());
        }
        // SAFETY: the caller's contract.
        let selector = unsafe { CStr::from_ptr(selector) }.to_str().map_err(e("selector"))?;
        let answer = device_answer(get(device)?, selector, arg)?;
        // SAFETY: as above.
        unsafe { *out = answer };
        Ok(true)
    })
}

/// `vendorName`: the Vulkan device's PCI vendor, named where known.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_vendor_name(device: *const NmbDevice, out: *mut c_char, size: usize) -> usize {
    guard(0, || {
        let d = get(device)?;
        let (_, raw) = d.instance.raw();
        // SAFETY: the instance listed the device.
        let id = unsafe { raw.get_physical_device_properties(d.physical.raw()) }.vendor_id;
        let name = match id {
            0x5143 => "Qualcomm".to_string(),
            0x106b => "Apple".to_string(),
            0x10de => "NVIDIA".to_string(),
            0x1002 => "AMD".to_string(),
            0x8086 => "Intel".to_string(),
            0x13b5 => "ARM".to_string(),
            _ => format!("0x{id:04x}"),
        };
        Ok(copy_out(&name, out, size))
    })
}

/// No family rows in rt: the waived families are the five `supportsFamily:` answers true, with no rows to name.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_family_waivers(device: *const NmbDevice, out: *mut c_char, size: usize) -> usize {
    guard(0, || {
        get(device)?;
        Ok(copy_out("Apple1 0:\nApple2 0:\nApple3 0:\nCommon1 0:\nCommon2 0:\n", out, size))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_family_blockers(device: *const NmbDevice, family: i64, out: *mut c_char, size: usize) -> usize {
    guard(0, || {
        get(device)?;
        let text = if TARGET_FAMILIES.contains(&family) { String::new() } else { format!("{family}: metal2vulkan-rt states no family rows\n") };
        Ok(copy_out(&text, out, size))
    })
}

/// `requiredLinearTextureBytesPerRowForDescriptor:` (Apple SPI): a row of `width` texels of `format` rounded up to its linear
/// alignment; 0 for a format no buffer can back.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_linear_bytes_per_row(device: *const NmbDevice, format: u64, width: u64) -> u64 {
    guard(0, || {
        let d = get(device)?;
        let pf = super::pixel_format(format, "pixel format")?;
        let alignment = d.device.minimum_linear_texture_alignment(pf).map_err(e("linear texture"))?;
        let info = d.device.pixel_format_info(format);
        // flags 0x400: a format of blocks; words[0]: the bytes of a texel.
        let texel = if info.flags & 0x400 != 0 { 0 } else { info.words[0] };
        if texel == 0 {
            return Err(format!("pixel format {format} has no texel size"));
        }
        Ok((width * texel).div_ceil(alignment) * alignment)
    })
}

/// Bytes a buffer of `length` takes and its alignment, of a heap.
pub(crate) fn heap_buffer(d: &NmbDevice, length: u64, options: u64) -> rt::SizeAndAlign {
    d.device.heap_buffer_size_and_align(length, options)
}

// Every handle class below names the device it was made on by a raw pointer it never frees: the frontend releases a device
// only after its objects (its one device lives as long as the process).
pub(crate) type DeviceRef = *const NmbDevice;

pub(crate) fn device_of<'a>(d: DeviceRef) -> &'a NmbDevice {
    // SAFETY: see DeviceRef.
    unsafe { &*d }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send + Sync>() {}
    is_send::<RetireGate>();
}

// The old patched runtime's telemetry the plug-in imports weakly (a static link needs them defined): the command pool row
// (m2v-spare-command-pools.patch), the placement sites (Droid-VM 0028) and the pool report (Droid-VM 0031). rt has none of
// these structures; each answers nothing, so the rows they feed stay silent.

#[unsafe(no_mangle)]
pub extern "C" fn m2v_command_pool_census(_out: *mut u64, _count: usize) -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn m2v_placement_sites(_out: *mut u8, _size: usize) -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn m2v_pool_report(_out: *mut u8, _size: usize) -> usize {
    0
}

/// The GPU memory row's census (userland/metal/CommandPoolRow.inc, the GM and RP lines), in its 16 words, from rt's gauges:
/// [0] 16 MiB units holding a block, [1] the bytes of rt's allocations, [2] the bytes placed in them (blocks and textures),
/// [3] idle allocations (counted as units of 16 MiB), [4] their bytes, [5] 0, [6] textures' allocations of their own,
/// [7], [8] 0 (rt keeps no spare image blocks: an idle allocation goes after 250 ms), [9] imports, [10] their bytes, [11] 0,
/// [12] render encoders ended, [13] their render passes begun (one each and one per split), [14] splits, [15] splits at a
/// memory barrier (rt 0008). Returns 16 once a device is open, else 0.
///
/// # Safety
/// `out` holds `count` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn m2v_memory_census(out: *mut u64, count: usize) -> usize {
    let device = CURRENT.load(std::sync::atomic::Ordering::Acquire);
    if device.is_null() || out.is_null() || count < 16 {
        return 0;
    }
    guard(0, || {
        let d = get(device)?;
        let g = d.device.counters().gauges();
        let m = d.device.counters().metrics();
        let idle = g.shared_idle_bytes + g.private_idle_bytes;
        let encoders = RENDER_ENCODERS.load(Relaxed);
        let words = [
            g.private_units,
            g.shared_allocation_bytes + g.private_allocation_bytes,
            g.shared_placed_bytes + g.private_placed_bytes + g.texture_block_bytes,
            idle.div_ceil(16 << 20),
            idle,
            0,
            g.texture_own_bytes,
            0,
            0,
            g.imports,
            g.import_bytes,
            0,
            encoders,
            encoders + m.render_pass_splits,
            m.render_pass_splits,
            m.render_pass_barrier_splits,
        ];
        // SAFETY: the caller's 16 words or more.
        unsafe { std::slice::from_raw_parts_mut(out, 16) }.copy_from_slice(&words);
        Ok(16)
    })
}
