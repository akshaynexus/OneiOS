// SPDX-License-Identifier: MIT
//! Queues, command buffers (each open in the device's retire gate from its making to its completion), events and fences, and
//! the compute, render and blit encoders over rt's. Each command buffer keeps what its encoders bound, so the frontend's waits
//! for a resource know whether it holds it.

use super::device::{device_of, DeviceRef, NmbDevice};
use super::memory::{NmbBuffer, NmbHeap};
use super::shader::{NmbComputePipeline, NmbDepthStencil, NmbRenderPipeline, NmbSampler};
use super::texture::NmbTexture;
use super::{boxed, copy_out, e, get, get_mut, guard, raw, release, words};
use metal2vulkan_runtime as rt;
use rt::{CullMode, IndexType, PrimitiveType, TriangleFillMode, Winding};
use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct NmbQueue(rt::CommandQueue, DeviceRef);

#[unsafe(no_mangle)]
pub extern "C" fn nmb_queue_new(device: *mut NmbDevice) -> *mut NmbQueue {
    guard(std::ptr::null_mut(), || {
        let d = get(device)?;
        Ok(boxed(NmbQueue(d.device.new_command_queue(64).map_err(e("queue"))?, d as *const NmbDevice)))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_queue_release(queue: *mut NmbQueue) {
    release(queue)
}

/// What a command buffer's encoders bound, by handle address; `opaque` once one of them made resources resident it cannot name
/// (useResource, useHeap).
#[derive(Default)]
pub(crate) struct Used {
    handles: HashSet<usize>,
    opaque: bool,
}

pub struct NmbCommandBuffer {
    pub(crate) cb: rt::CommandBuffer,
    device: DeviceRef,
    seq: u64,
    closed: Arc<AtomicBool>,
    committed: AtomicBool,
    used: Arc<Mutex<Used>>,
    /// Set by the completed handler made with the command buffer (rt takes no handler after the commit), which a
    /// bounded wait sleeps on.
    done: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

fn close(device: DeviceRef, seq: u64, closed: &AtomicBool) {
    if !closed.swap(true, Ordering::AcqRel) {
        device_of(device).gate.close(seq);
    }
}

impl Drop for NmbCommandBuffer {
    fn drop(&mut self) {
        if !self.committed.load(Ordering::Acquire) {
            close(self.device, self.seq, &self.closed);
        }
    }
}

struct SendRef(DeviceRef);
// SAFETY: the device outlives every command buffer made on it (abi::device::DeviceRef).
unsafe impl Send for SendRef {}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_new(queue: *mut NmbQueue) -> *mut NmbCommandBuffer {
    guard(std::ptr::null_mut(), || {
        let q = get(queue)?;
        let d = device_of(q.1);
        let seq = d.gate.open();
        let cb = match q.0.command_buffer() {
            Ok(cb) => cb,
            Err(why) => {
                d.gate.close(seq);
                return Err(format!("command buffer: {why}"));
            }
        };
        let closed = Arc::new(AtomicBool::new(false));
        let done: Arc<(Mutex<bool>, std::sync::Condvar)> = Arc::default();
        let (flag, device, signal) = (closed.clone(), SendRef(q.1), done.clone());
        cb.add_completed_handler(move |_| {
            let device = device;
            close(device.0, seq, &flag);
            let (lock, cv) = &*signal;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
            cv.notify_all();
        })
        .map_err(e("completed handler"))?;
        Ok(boxed(NmbCommandBuffer { cb, device: q.1, seq, closed, committed: AtomicBool::new(false), used: Arc::default(), done }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_release(command_buffer: *mut NmbCommandBuffer) {
    release(command_buffer)
}

fn uses(command_buffer: *const NmbCommandBuffer, handle: usize) -> Result<i32, String> {
    let used = get(command_buffer)?.used.lock().unwrap_or_else(|p| p.into_inner());
    Ok(if used.handles.contains(&handle) { 1 } else if used.opaque { -1 } else { 0 })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_uses_buffer(command_buffer: *const NmbCommandBuffer, buffer: *const NmbBuffer) -> i32 {
    guard(-1, || uses(command_buffer, buffer as usize))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_uses_texture(command_buffer: *const NmbCommandBuffer, texture: *const NmbTexture) -> i32 {
    guard(-1, || uses(command_buffer, texture as usize))
}

struct Context(*mut c_void);
// SAFETY: the context is the caller's, handed back on rt's thread as the header states.
unsafe impl Send for Context {}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_on_completed(command_buffer: *mut NmbCommandBuffer, handler: Option<extern "C" fn(*mut c_void)>,
                                                  context: *mut c_void) -> bool {
    guard(false, || {
        let handler = handler.ok_or("null handler")?;
        let context = Context(context);
        get(command_buffer)?
            .cb
            .add_completed_handler(move |_| {
                let context = context;
                handler(context.0)
            })
            .map_err(e("completed handler"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_on_scheduled(command_buffer: *mut NmbCommandBuffer, handler: Option<extern "C" fn(*mut c_void)>,
                                                  context: *mut c_void) -> bool {
    guard(false, || {
        let handler = handler.ok_or("null handler")?;
        let context = Context(context);
        get(command_buffer)?
            .cb
            .add_scheduled_handler(move |_| {
                let context = context;
                handler(context.0)
            })
            .map_err(e("scheduled handler"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_wait_scheduled(command_buffer: *mut NmbCommandBuffer) -> bool {
    guard(false, || {
        get(command_buffer)?.cb.wait_until_scheduled().map_err(e("wait scheduled"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_commit(command_buffer: *mut NmbCommandBuffer) -> bool {
    guard(false, || {
        let c = get(command_buffer)?;
        c.cb.commit().map_err(e("commit"))?;
        c.committed.store(true, Ordering::Release);
        Ok(true)
    })
}

/// 0 completed in time, 1 timed out (0 ms waits without a bound): a bounded wait sleeps on the completion the command
/// buffer's own handler signals.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_wait(command_buffer: *mut NmbCommandBuffer, timeout_ms: u64) -> i32 {
    guard(-1, || {
        let c = get(command_buffer)?;
        if timeout_ms == 0 {
            c.cb.wait_until_completed().map_err(e("wait"))?;
            return Ok(0);
        }
        let (lock, cv) = &*c.done;
        let held = lock.lock().unwrap_or_else(|p| p.into_inner());
        let (held, _) = cv.wait_timeout_while(held, Duration::from_millis(timeout_ms), |finished| !*finished).unwrap_or_else(|p| p.into_inner());
        Ok(if *held { 0 } else { 1 })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_status(command_buffer: *const NmbCommandBuffer) -> u64 {
    guard(5, || {
        use rt::CommandBufferStatus as S;
        Ok(match get(command_buffer)?.cb.status() {
            S::NotEnqueued => 0,
            S::Enqueued => 1,
            S::Committed => 2,
            S::Scheduled => 3,
            S::Completed => 4,
            _ => 5,
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_error(command_buffer: *const NmbCommandBuffer, out: *mut c_char, size: usize) -> usize {
    guard(0, || Ok(get(command_buffer)?.cb.error().map_or(0, |error| copy_out(&format!("{error:?}"), out, size))))
}

/// Metal's MTLCommandBufferError code: a lost device is DeviceRemoved (11); a command buffer that did not run, NotPermitted (7).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_metal_error_code(command_buffer: *const NmbCommandBuffer) -> u64 {
    guard(0, || {
        Ok(match get(command_buffer)?.cb.error() {
            Some(rt::CommandBufferError::DeviceLost { .. }) => 11,
            Some(_) => 7,
            None => 0,
        })
    })
}

/// gpuStartTime / gpuEndTime in seconds of the host clock (rt's are nanoseconds of it).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_gpu_time(command_buffer: *const NmbCommandBuffer, end: bool) -> f64 {
    guard(0.0, || {
        let cb = &get(command_buffer)?.cb;
        Ok(if end { cb.gpu_end_time() } else { cb.gpu_start_time() } as f64 / 1e9)
    })
}

// ---- events and fences ----

pub enum NmbEvent {
    Event(rt::Event),
    Shared(rt::SharedEvent),
}

impl NmbEvent {
    fn event(&self) -> &rt::Event {
        match self {
            NmbEvent::Event(e) => e,
            NmbEvent::Shared(s) => s.as_event(),
        }
    }
}

fn shared<'a>(event: *const NmbEvent) -> Result<&'a rt::SharedEvent, String> {
    match get(event)? {
        NmbEvent::Shared(s) => Ok(s),
        NmbEvent::Event(_) => Err("not a shared event".into()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_event_new(device: *mut NmbDevice, is_shared: bool) -> *mut NmbEvent {
    guard(std::ptr::null_mut(), || {
        let d = &get(device)?.device;
        Ok(boxed(if is_shared {
            NmbEvent::Shared(d.new_shared_event().map_err(e("shared event"))?)
        } else {
            NmbEvent::Event(d.new_event().map_err(e("event"))?)
        }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_event_release(event: *mut NmbEvent) {
    release(event)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_shared_event_value(event: *const NmbEvent) -> u64 {
    guard(0, || Ok(shared(event)?.signaled_value()))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_shared_event_set_value(event: *mut NmbEvent, value: u64) -> bool {
    guard(false, || {
        shared(event)?.set_signaled_value(value);
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_shared_event_wait(event: *mut NmbEvent, value: u64, timeout_ms: u64) -> bool {
    guard(false, || shared(event)?.wait_until_signaled_value(value, Duration::from_millis(timeout_ms)).map_err(e("shared event wait")))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_shared_event_notify(event: *mut NmbEvent, value: u64, handler: Option<extern "C" fn(*mut c_void, u64)>,
                                          context: *mut c_void) -> bool {
    guard(false, || {
        let handler = handler.ok_or("null handler")?;
        let context = Context(context);
        shared(event)?
            .notify_listener(value, move |reached| {
                let context = context;
                handler(context.0, reached)
            })
            .map_err(e("shared event listener"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_signal_event(command_buffer: *mut NmbCommandBuffer, event: *mut NmbEvent, value: u64) -> bool {
    guard(false, || {
        get(command_buffer)?.cb.encode_signal_event(get(event)?.event(), value).map_err(e("signal event"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_command_buffer_wait_event(command_buffer: *mut NmbCommandBuffer, event: *mut NmbEvent, value: u64) -> bool {
    guard(false, || {
        get(command_buffer)?.cb.encode_wait_for_event(get(event)?.event(), value).map_err(e("wait for event"))?;
        Ok(true)
    })
}

pub struct NmbFence(rt::Fence);

#[unsafe(no_mangle)]
pub extern "C" fn nmb_fence_new(device: *mut NmbDevice) -> *mut NmbFence {
    guard(std::ptr::null_mut(), || Ok(boxed(NmbFence(get(device)?.device.new_fence()))))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_fence_release(fence: *mut NmbFence) {
    release(fence)
}

// ---- encoders ----

pub enum Kind {
    Compute(rt::ComputeCommandEncoder),
    Render(rt::RenderCommandEncoder),
    Blit(rt::BlitCommandEncoder),
}

pub struct NmbEncoder {
    kind: Kind,
    used: Arc<Mutex<Used>>,
}

impl NmbEncoder {
    fn note(&self, handle: usize) {
        if handle != 0 {
            self.used.lock().unwrap_or_else(|p| p.into_inner()).handles.insert(handle);
        }
    }
}

fn encoder<'a>(encoder: *mut NmbEncoder) -> Result<&'a mut NmbEncoder, String> {
    get_mut(encoder)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_update_fence(encoder_: *mut NmbEncoder, fence: *mut NmbFence, _stages: u64) -> bool {
    guard(false, || {
        let fence = &get(fence)?.0;
        match &mut encoder(encoder_)?.kind {
            Kind::Compute(c) => c.update_fence(fence),
            Kind::Blit(b) => b.update_fence(fence),
            Kind::Render(r) => r.update_fence(fence),
        }
        .map_err(e("update fence"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_wait_fence(encoder_: *mut NmbEncoder, fence: *mut NmbFence, _stages: u64) -> bool {
    guard(false, || {
        let fence = &get(fence)?.0;
        match &mut encoder(encoder_)?.kind {
            Kind::Compute(c) => c.wait_for_fence(fence),
            Kind::Blit(b) => b.wait_for_fence(fence),
            Kind::Render(r) => r.wait_for_fence(fence),
        }
        .map_err(e("wait for fence"))?;
        Ok(true)
    })
}

fn usage(raw_usage: u64) -> rt::ResourceUsage {
    let mut u = rt::ResourceUsage::READ;
    if raw_usage & 2 != 0 {
        u = rt::ResourceUsage::WRITE;
        if raw_usage & 1 != 0 {
            u = rt::ResourceUsage::READ | rt::ResourceUsage::WRITE;
        }
    }
    if raw_usage & 4 != 0 && raw_usage & 2 == 0 {
        u = rt::ResourceUsage::SAMPLE;
    }
    u
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_use_resource(encoder_: *mut NmbEncoder, buffer: *const NmbBuffer, texture: *const NmbTexture, raw_usage: u64,
                                           _stages: u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        let resource = if !texture.is_null() { rt::Resource::Texture(&get(texture)?.texture) } else { rt::Resource::Buffer(&get(buffer)?.buffer) };
        enc.note(if texture.is_null() { buffer as usize } else { texture as usize });
        let u = usage(raw_usage);
        match &mut enc.kind {
            Kind::Compute(c) => c.use_resource(resource, u),
            Kind::Render(r) => r.use_resource(resource, u),
            Kind::Blit(_) => return Err("useResource on a blit encoder".into()),
        }
        .map_err(e("use resource"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_use_heap(encoder_: *mut NmbEncoder, heap: *const NmbHeap, _stages: u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        let id = get(heap)?.inner.block.id;
        enc.used.lock().unwrap_or_else(|p| p.into_inner()).opaque = true;
        match &mut enc.kind {
            Kind::Compute(c) => c.use_heap(id),
            Kind::Render(r) => r.use_heap(id),
            Kind::Blit(_) => return Err("useHeap on a blit encoder".into()),
        }
        .map_err(e("use heap"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_encoder_new(command_buffer: *mut NmbCommandBuffer) -> *mut NmbEncoder {
    guard(std::ptr::null_mut(), || {
        let c = get(command_buffer)?;
        let kind = Kind::Compute(c.cb.compute_command_encoder(rt::DispatchType::Serial).map_err(e("compute encoder"))?);
        Ok(boxed(NmbEncoder { kind, used: c.used.clone() }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_blit_encoder_new(command_buffer: *mut NmbCommandBuffer) -> *mut NmbEncoder {
    guard(std::ptr::null_mut(), || {
        let c = get(command_buffer)?;
        let kind = Kind::Blit(c.cb.blit_command_encoder().map_err(e("blit encoder"))?);
        Ok(boxed(NmbEncoder { kind, used: c.used.clone() }))
    })
}

#[repr(C)]
pub struct NmbPassAttachment {
    pub texture: *mut NmbTexture,
    pub level: u64,
    pub slice: u64,
    pub depth_plane: u64,
    pub load: u64,
    pub store: u64,
    pub clear: [f64; 4],
    pub resolve_texture: *mut NmbTexture,
    pub resolve_level: u64,
    pub resolve_slice: u64,
    pub resolve_depth_plane: u64,
    pub resolve_filter: u64,
}

#[repr(C)]
pub struct NmbRenderPassDescriptor {
    pub colors: [NmbPassAttachment; 8],
    pub depth: NmbPassAttachment,
    pub stencil: NmbPassAttachment,
    pub width: u64,
    pub height: u64,
    pub array_length: u64,
    pub samples: u64,
    pub visibility: *const NmbBuffer,
}

fn texture_of(t: *mut NmbTexture) -> Result<Option<rt::Texture>, String> {
    if t.is_null() { Ok(None) } else { Ok(Some(get(t)?.texture.clone())) }
}

fn load(v: u64) -> Result<rt::LoadAction, String> {
    Ok(match v {
        0 => rt::LoadAction::DontCare,
        1 => rt::LoadAction::Load,
        2 => rt::LoadAction::Clear,
        _ => return Err(format!("load action {v} is not a Metal value")),
    })
}

fn store(v: u64) -> Result<rt::StoreAction, String> {
    Ok(match v {
        0 => rt::StoreAction::DontCare,
        1 => rt::StoreAction::Store,
        2 => rt::StoreAction::MultisampleResolve,
        3 => rt::StoreAction::StoreAndMultisampleResolve,
        4 => rt::StoreAction::Unknown,
        5 => rt::StoreAction::CustomSampleDepthStore,
        _ => return Err(format!("store action {v} is not a Metal value")),
    })
}

fn u32_of(v: u64, what: &str) -> Result<u32, String> {
    u32::try_from(v).map_err(|_| format!("{what} {v} is past 32 bits"))
}

/// A 3D attachment's depth plane is its slice in rt (the descriptor has no depth plane of its own).
fn slice_of(a: &NmbPassAttachment) -> Result<u32, String> {
    u32_of(a.slice.max(a.depth_plane), "slice")
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_encoder_new(command_buffer: *mut NmbCommandBuffer, descriptor: *const NmbRenderPassDescriptor) -> *mut NmbEncoder {
    guard(std::ptr::null_mut(), || {
        let c = get(command_buffer)?;
        let d = get(descriptor)?;
        let mut pass = rt::RenderPassDescriptor::default();
        let mut noted = Vec::new();
        for (slot, a) in pass.color_attachments.iter_mut().zip(d.colors.iter()) {
            slot.texture = texture_of(a.texture)?;
            slot.resolve_texture = texture_of(a.resolve_texture)?;
            slot.resolve_level = u32_of(a.resolve_level, "level")?;
            slot.resolve_slice = u32_of(a.resolve_slice, "slice")?;
            slot.resolve_depth_plane = u32_of(a.resolve_depth_plane, "depth plane")?;
            slot.level = u32_of(a.level, "level")?;
            slot.slice = slice_of(a)?;
            slot.load_action = load(a.load)?;
            slot.store_action = store(a.store)?;
            slot.clear_color = rt::ClearColor { red: a.clear[0], green: a.clear[1], blue: a.clear[2], alpha: a.clear[3] };
            noted.extend([a.texture as usize, a.resolve_texture as usize]);
        }
        let depth = &mut pass.depth_attachment;
        depth.texture = texture_of(d.depth.texture)?;
        depth.resolve_texture = texture_of(d.depth.resolve_texture)?;
        depth.resolve_level = u32_of(d.depth.resolve_level, "level")?;
        depth.resolve_slice = u32_of(d.depth.resolve_slice, "slice")?;
        depth.resolve_depth_plane = u32_of(d.depth.resolve_depth_plane, "depth plane")?;
        depth.depth_resolve_filter = match d.depth.resolve_filter {
            1 => rt::MultisampleDepthResolveFilter::Min,
            2 => rt::MultisampleDepthResolveFilter::Max,
            _ => rt::MultisampleDepthResolveFilter::Sample0,
        };
        depth.level = u32_of(d.depth.level, "level")?;
        depth.slice = slice_of(&d.depth)?;
        depth.load_action = load(d.depth.load)?;
        depth.store_action = store(d.depth.store)?;
        depth.clear_depth = d.depth.clear[0];
        let stencil = &mut pass.stencil_attachment;
        stencil.texture = texture_of(d.stencil.texture)?;
        stencil.resolve_texture = texture_of(d.stencil.resolve_texture)?;
        stencil.resolve_level = u32_of(d.stencil.resolve_level, "level")?;
        stencil.resolve_slice = u32_of(d.stencil.resolve_slice, "slice")?;
        stencil.resolve_depth_plane = u32_of(d.stencil.resolve_depth_plane, "depth plane")?;
        stencil.stencil_resolve_filter = match d.stencil.resolve_filter {
            1 => rt::MultisampleStencilResolveFilter::DepthResolvedSample,
            _ => rt::MultisampleStencilResolveFilter::Sample0,
        };
        stencil.level = u32_of(d.stencil.level, "level")?;
        stencil.slice = slice_of(&d.stencil)?;
        stencil.load_action = load(d.stencil.load)?;
        stencil.store_action = store(d.stencil.store)?;
        stencil.clear_stencil = d.stencil.clear[0] as u32;
        noted.extend([d.depth.texture as usize, d.stencil.texture as usize]);
        pass.render_target_width = u32_of(d.width, "width")?;
        pass.render_target_height = u32_of(d.height, "height")?;
        pass.render_target_array_length = u32_of(d.array_length, "array length")?;
        pass.default_raster_sample_count = u32_of(d.samples, "samples")?;
        if !d.visibility.is_null() {
            pass.visibility_result_buffer = Some(get(d.visibility)?.buffer.clone());
            noted.push(d.visibility as usize);
        }
        let kind = Kind::Render(c.cb.render_command_encoder(&pass).map_err(e("render encoder"))?);
        let enc = NmbEncoder { kind, used: c.used.clone() };
        for h in noted {
            enc.note(h);
        }
        Ok(boxed(enc))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_end(encoder_: *mut NmbEncoder) -> bool {
    guard(false, || {
        if encoder_.is_null() {
            return Err("null encoder".into());
        }
        // SAFETY: from `boxed`; ending releases it.
        let enc = unsafe { Box::from_raw(encoder_) };
        match enc.kind {
            Kind::Compute(c) => c.end_encoding(),
            Kind::Render(r) => r.end_encoding(),
            Kind::Blit(b) => b.end_encoding(),
        }
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_discard(encoder_: *mut NmbEncoder) {
    release(encoder_)
}

const COMPUTE: u32 = 0;
const VERTEX: u32 = 1;
const FRAGMENT: u32 = 2;

fn bad_stage(stage: u32) -> String {
    format!("stage {stage} does not fit this encoder")
}

fn compute<'a>(encoder_: *mut NmbEncoder) -> Result<&'a mut rt::ComputeCommandEncoder, String> {
    match &mut encoder(encoder_)?.kind {
        Kind::Compute(c) => Ok(c),
        _ => Err("not a compute encoder".into()),
    }
}

fn render<'a>(encoder_: *mut NmbEncoder) -> Result<&'a mut rt::RenderCommandEncoder, String> {
    match &mut encoder(encoder_)?.kind {
        Kind::Render(r) => Ok(r),
        _ => Err("not a render encoder".into()),
    }
}

fn blit<'a>(encoder_: *mut NmbEncoder) -> Result<&'a mut rt::BlitCommandEncoder, String> {
    match &mut encoder(encoder_)?.kind {
        Kind::Blit(b) => Ok(b),
        _ => Err("not a blit encoder".into()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_set_buffer(encoder_: *mut NmbEncoder, stage: u32, buffer: *mut NmbBuffer, offset: u64, index: u32) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(buffer as usize);
        let b = if buffer.is_null() { None } else { Some(&get(buffer)?.buffer) };
        match (&mut enc.kind, stage) {
            (Kind::Compute(c), COMPUTE) => c.set_buffer(b, offset, index),
            (Kind::Render(r), VERTEX) => r.set_vertex_buffer(b, offset, index),
            (Kind::Render(r), FRAGMENT) => r.set_fragment_buffer(b, offset, index),
            _ => return Err(bad_stage(stage)),
        }
        .map_err(e("set buffer"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_set_buffer_offset(encoder_: *mut NmbEncoder, stage: u32, offset: u64, index: u32) -> bool {
    guard(false, || {
        match (&mut encoder(encoder_)?.kind, stage) {
            (Kind::Compute(c), COMPUTE) => c.set_buffer_offset(offset, index),
            (Kind::Render(r), VERTEX) => r.set_vertex_buffer_offset(offset, index),
            (Kind::Render(r), FRAGMENT) => r.set_fragment_buffer_offset(offset, index),
            _ => return Err(bad_stage(stage)),
        }
        .map_err(e("set buffer offset"))?;
        Ok(true)
    })
}

/// # Safety
/// `bytes` holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_encoder_set_bytes(encoder_: *mut NmbEncoder, stage: u32, bytes: *const c_void, length: usize, index: u32) -> bool {
    guard(false, || {
        if bytes.is_null() {
            return Err("null bytes".into());
        }
        // SAFETY: the caller's contract.
        let bytes = unsafe { std::slice::from_raw_parts(bytes as *const u8, length) };
        match (&mut encoder(encoder_)?.kind, stage) {
            (Kind::Compute(c), COMPUTE) => c.set_bytes(bytes, index),
            (Kind::Render(r), VERTEX) => r.set_vertex_bytes(bytes, index),
            (Kind::Render(r), FRAGMENT) => r.set_fragment_bytes(bytes, index),
            _ => return Err(bad_stage(stage)),
        }
        .map_err(e("set bytes"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_set_texture(encoder_: *mut NmbEncoder, stage: u32, texture: *mut NmbTexture, index: u32) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(texture as usize);
        let t = if texture.is_null() { None } else { Some(&get(texture)?.texture) };
        match (&mut enc.kind, stage) {
            (Kind::Compute(c), COMPUTE) => c.set_texture(t, index),
            (Kind::Render(r), VERTEX) => r.set_vertex_texture(t, index),
            (Kind::Render(r), FRAGMENT) => r.set_fragment_texture(t, index),
            _ => return Err(bad_stage(stage)),
        }
        .map_err(e("set texture"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_encoder_set_sampler(encoder_: *mut NmbEncoder, stage: u32, sampler: *mut NmbSampler, index: u32) -> bool {
    guard(false, || {
        let s = if sampler.is_null() { None } else { Some(&get(sampler)?.state) };
        match (&mut encoder(encoder_)?.kind, stage) {
            (Kind::Compute(c), COMPUTE) => c.set_sampler_state(s, index),
            (Kind::Render(r), VERTEX) => r.set_vertex_sampler_state(s, index),
            (Kind::Render(r), FRAGMENT) => r.set_fragment_sampler_state(s, index),
            _ => return Err(bad_stage(stage)),
        }
        .map_err(e("set sampler"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_set_pipeline(encoder_: *mut NmbEncoder, pipeline: *mut NmbComputePipeline) -> bool {
    guard(false, || {
        compute(encoder_)?.set_compute_pipeline_state(&get(pipeline)?.0).map_err(e("compute pipeline"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_set_threadgroup_memory(encoder_: *mut NmbEncoder, length: u32, index: u32) -> bool {
    guard(false, || {
        compute(encoder_)?.set_threadgroup_memory_length(length, index).map_err(e("threadgroup memory"))?;
        Ok(true)
    })
}

fn triple(v: *const u64, what: &str) -> Result<[u32; 3], String> {
    let v = words(v, 3, what)?;
    Ok([u32_of(v[0], what)?, u32_of(v[1], what)?, u32_of(v[2], what)?])
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_dispatch(encoder_: *mut NmbEncoder, size: *const u64, per_group: *const u64, threads: bool) -> bool {
    guard(false, || {
        let (s, g) = (triple(size, "grid")?, triple(per_group, "threadgroup")?);
        let c = compute(encoder_)?;
        if threads { c.dispatch_threads(s, g) } else { c.dispatch_threadgroups(s, g) }.map_err(e("dispatch"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_dispatch_indirect(encoder_: *mut NmbEncoder, buffer: *mut NmbBuffer, offset: u64, per_group: *const u64) -> bool {
    guard(false, || {
        let g = triple(per_group, "threadgroup")?;
        encoder(encoder_)?.note(buffer as usize);
        compute(encoder_)?.dispatch_threadgroups_with_indirect_buffer(&get(buffer)?.buffer, offset, g).map_err(e("indirect dispatch"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_pipeline(encoder_: *mut NmbEncoder, pipeline: *mut NmbRenderPipeline) -> bool {
    guard(false, || {
        render(encoder_)?.set_render_pipeline_state(&get(pipeline)?.0).map_err(e("render pipeline"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_depth_stencil(encoder_: *mut NmbEncoder, state: *mut NmbDepthStencil) -> bool {
    guard(false, || {
        let s = if state.is_null() { None } else { Some(&get(state)?.0) };
        render(encoder_)?.set_depth_stencil_state(s).map_err(e("depth/stencil state"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_viewport(encoder_: *mut NmbEncoder, viewport: *const f64) -> bool {
    guard(false, || {
        let v = words(viewport, 6, "viewport")?;
        render(encoder_)?
            .set_viewport(rt::Viewport { origin_x: v[0], origin_y: v[1], width: v[2], height: v[3], znear: v[4], zfar: v[5] })
            .map_err(e("viewport"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_scissor(encoder_: *mut NmbEncoder, rect: *const u64) -> bool {
    guard(false, || {
        let r = words(rect, 4, "scissor")?;
        render(encoder_)?.set_scissor_rect(rt::ScissorRect { x: r[0], y: r[1], width: r[2], height: r[3] });
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_raster(encoder_: *mut NmbEncoder, cull_mode: u64, winding: u64, fill_mode: u64) -> bool {
    guard(false, || {
        let r = render(encoder_)?;
        r.set_cull_mode(raw::<CullMode>(cull_mode, "cull mode")?);
        r.set_front_facing_winding(raw::<Winding>(winding, "winding")?);
        r.set_triangle_fill_mode(raw::<TriangleFillMode>(fill_mode, "fill mode")?).map_err(e("fill mode"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_stencil_reference(encoder_: *mut NmbEncoder, front: u32, back: u32) -> bool {
    guard(false, || {
        render(encoder_)?.set_stencil_front_back_reference_value(front, back);
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_depth_bias(encoder_: *mut NmbEncoder, bias: f32, slope_scale: f32, clamp: f32) -> bool {
    guard(false, || {
        render(encoder_)?.set_depth_bias(bias, slope_scale, clamp);
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_line_width(encoder_: *mut NmbEncoder, width: f32) -> bool {
    guard(false, || {
        render(encoder_)?.set_line_width(width);
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_visibility_mode(encoder_: *mut NmbEncoder, mode: u64, offset: u64) -> bool {
    guard(false, || {
        let mode = match mode {
            0 => rt::VisibilityResultMode::Disabled,
            1 => rt::VisibilityResultMode::Boolean,
            2 => rt::VisibilityResultMode::Counting,
            _ => return Err(format!("visibility result mode {mode} is not a Metal value")),
        };
        render(encoder_)?.set_visibility_result_mode(mode, offset).map_err(e("visibility result mode"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_blend_color(encoder_: *mut NmbEncoder, color: *const f32) -> bool {
    guard(false, || {
        let c = words(color, 4, "color")?;
        render(encoder_)?.set_blend_color(c[0], c[1], c[2], c[3]);
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_draw(encoder_: *mut NmbEncoder, primitive: u64, start: u32, count: u32, instances: u32, base_instance: u32) -> bool {
    guard(false, || {
        render(encoder_)?
            .draw_primitives(raw::<PrimitiveType>(primitive, "primitive")?, start, count, instances, base_instance)
            .map_err(e("draw"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_draw_indirect(encoder_: *mut NmbEncoder, primitive: u64, index_type: u64, indices: *const NmbBuffer,
                                           index_offset: u64, indirect: *const NmbBuffer, indirect_offset: u64) -> bool {
    guard(false, || {
        let primitive = raw::<PrimitiveType>(primitive, "primitive")?;
        let enc = encoder(encoder_)?;
        enc.note(indirect as usize);
        enc.note(indices as usize);
        let indirect = &get(indirect)?.buffer;
        let r = render(encoder_)?;
        if indices.is_null() {
            r.draw_primitives_indirect(primitive, indirect, indirect_offset)
        } else {
            let ib = rt::IndexBuffer { buffer: &get(indices)?.buffer, offset: index_offset, index_type: raw::<IndexType>(index_type, "index type")? };
            r.draw_indexed_primitives_indirect(primitive, ib, indirect, indirect_offset)
        }
        .map_err(e("indirect draw"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nmb_render_draw_indexed(encoder_: *mut NmbEncoder, primitive: u64, count: u32, index_type: u64, indices: *mut NmbBuffer,
                                          offset: u64, instances: u32, base_vertex: i32, base_instance: u32) -> bool {
    guard(false, || {
        encoder(encoder_)?.note(indices as usize);
        let ib = rt::IndexBuffer { buffer: &get(indices)?.buffer, offset, index_type: raw::<IndexType>(index_type, "index type")? };
        render(encoder_)?
            .draw_indexed_primitives(raw::<PrimitiveType>(primitive, "primitive")?, count, ib, instances, base_vertex, base_instance)
            .map_err(e("indexed draw"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_tessellation_factor_buffer(encoder_: *mut NmbEncoder, buffer: *mut NmbBuffer, offset: u64, instance_stride: u64)
                                                            -> bool {
    guard(false, || {
        encoder(encoder_)?.note(buffer as usize);
        let b = if buffer.is_null() { None } else { Some(&get(buffer)?.buffer) };
        render(encoder_)?.set_tessellation_factor_buffer(b, offset, instance_stride).map_err(e("tessellation factor buffer"))?;
        Ok(true)
    })
}

/// rt takes no tessellation factor scale: 1 is the default, and another is refused.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_set_tessellation_factor_scale(encoder_: *mut NmbEncoder, scale: f32) -> bool {
    guard(false, || {
        render(encoder_)?;
        if scale != 1.0 {
            return Err("metal2vulkan-rt takes no tessellation factor scale".into());
        }
        Ok(true)
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nmb_render_draw_patches(encoder_: *mut NmbEncoder, control_points: u32, patch_start: u32, patch_count: u32,
                                          patch_index: *mut NmbBuffer, patch_index_offset: u64, control_point_index: *mut NmbBuffer,
                                          _control_point_index_offset: u64, instances: u32, base_instance: u32, indirect: *mut NmbBuffer,
                                          indirect_offset: u64) -> bool {
    guard(false, || {
        if !control_point_index.is_null() {
            return Err("metal2vulkan-rt draws no patches with a control point index buffer".into());
        }
        let enc = encoder(encoder_)?;
        enc.note(patch_index as usize);
        enc.note(indirect as usize);
        let pi = if patch_index.is_null() { None } else { Some((&get(patch_index)?.buffer, patch_index_offset)) };
        let r = render(encoder_)?;
        if indirect.is_null() {
            r.draw_patches(control_points, patch_start, patch_count, pi, instances, base_instance)
        } else {
            r.draw_patches_indirect(control_points, pi, &get(indirect)?.buffer, indirect_offset)
        }
        .map_err(e("patch draw"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_blit_copy_buffer(encoder_: *mut NmbEncoder, source: *mut NmbBuffer, source_offset: u64, destination: *mut NmbBuffer,
                                       destination_offset: u64, size: u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(source as usize);
        enc.note(destination as usize);
        blit(encoder_)?
            .copy_from_buffer_to_buffer(&get(source)?.buffer, source_offset, &get(destination)?.buffer, destination_offset, size)
            .map_err(e("buffer copy"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_blit_fill_buffer(encoder_: *mut NmbEncoder, buffer: *mut NmbBuffer, start: u64, end: u64, value: u8) -> bool {
    guard(false, || {
        encoder(encoder_)?.note(buffer as usize);
        blit(encoder_)?.fill_buffer(&get(buffer)?.buffer, start..end, value).map_err(e("fill"))?;
        Ok(true)
    })
}

fn place(slice: u64, level: u64, origin: *const u64) -> Result<rt::TexturePlace, String> {
    Ok(rt::TexturePlace { slice: u32_of(slice, "slice")?, level: u32_of(level, "level")?, origin: triple(origin, "origin")? })
}

fn blit_option(options: u64) -> Result<rt::BlitOption, String> {
    Ok(match options {
        0 => rt::BlitOption::None,
        1 => rt::BlitOption::DepthFromDepthStencil,
        2 => rt::BlitOption::StencilFromDepthStencil,
        _ => return Err(format!("blit options {options:#x}: metal2vulkan-rt takes none of them")),
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nmb_blit_copy_texture(encoder_: *mut NmbEncoder, source: *mut NmbTexture, source_slice: u64, source_level: u64,
                                        source_origin: *const u64, extent: *const u64, destination: *mut NmbTexture,
                                        destination_slice: u64, destination_level: u64, destination_origin: *const u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(source as usize);
        enc.note(destination as usize);
        blit(encoder_)?
            .copy_from_texture(&get(source)?.texture, place(source_slice, source_level, source_origin)?, triple(extent, "size")?,
                               &get(destination)?.texture, place(destination_slice, destination_level, destination_origin)?)
            .map_err(e("texture copy"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nmb_blit_buffer_to_texture(encoder_: *mut NmbEncoder, source: *mut NmbBuffer, offset: u64, bytes_per_row: u64,
                                             bytes_per_image: u64, extent: *const u64, destination: *mut NmbTexture, slice: u64,
                                             level: u64, destination_origin: *const u64, options: u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(source as usize);
        enc.note(destination as usize);
        blit(encoder_)?
            .copy_from_buffer_with_options(&get(source)?.buffer, rt::BufferLayout { offset, bytes_per_row, bytes_per_image },
                                           triple(extent, "size")?, &get(destination)?.texture, place(slice, level, destination_origin)?,
                                           blit_option(options)?)
            .map_err(e("buffer to texture"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nmb_blit_texture_to_buffer(encoder_: *mut NmbEncoder, source: *mut NmbTexture, slice: u64, level: u64,
                                             source_origin: *const u64, extent: *const u64, destination: *mut NmbBuffer, offset: u64,
                                             bytes_per_row: u64, bytes_per_image: u64, options: u64) -> bool {
    guard(false, || {
        let enc = encoder(encoder_)?;
        enc.note(source as usize);
        enc.note(destination as usize);
        blit(encoder_)?
            .copy_from_texture_to_buffer_with_options(&get(source)?.texture, place(slice, level, source_origin)?, triple(extent, "size")?,
                                                      &get(destination)?.buffer, rt::BufferLayout { offset, bytes_per_row, bytes_per_image },
                                                      blit_option(options)?)
            .map_err(e("texture to buffer"))?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_blit_generate_mipmaps(encoder_: *mut NmbEncoder, texture: *mut NmbTexture) -> bool {
    guard(false, || {
        encoder(encoder_)?.note(texture as usize);
        blit(encoder_)?.generate_mipmaps(&get(texture)?.texture).map_err(e("mipmaps"))?;
        Ok(true)
    })
}
