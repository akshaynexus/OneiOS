// SPDX-License-Identifier: MIT
//! nmb_selftest (NativeMetalBridge.h): real Metal-level work through the bridge's device, checked on the CPU: a compute kernel,
//! a render pass, a render into host pages imported without a copy (the IOSurface path), and the census of the target's own
//! QuartzCore library. Each step reports a marker before it starts, so a hang names the step. `m2v-rt-check --selftest` runs
//! the same on the laptop (ANV), with an empty QuartzCore step or `--qc FILE`.

use crate::abi::device::{NmbDevice, FIXTURE_AIR};
use metal2vulkan_runtime as rt;
use std::ffi::{c_char, CStr};
use std::ptr::NonNull;

#[repr(C)]
pub struct NmbSelftest {
    pub stage: u32,
    pub compute_bad: u32,
    pub render_bad: u32,
    pub import_bad: u32,
    pub qc_functions: u32,
    pub qc_kernels_ok: u32,
    pub qc_kernels_failed: u32,
    pub qc_pairs_ok: u32,
    pub qc_pairs_failed: u32,
    pub reserved: u32,
    pub message: [u8; 96],
}

pub type Mark = Option<unsafe extern "C" fn(u32)>;

struct Run<'a> {
    out: &'a mut NmbSelftest,
    mark: Mark,
}

impl Run<'_> {
    fn step(&mut self, marker: u32) {
        if let Some(mark) = self.mark {
            // SAFETY: the caller's callback, valid for the call.
            unsafe { mark(marker) };
        }
    }
    fn fail(&mut self, marker: u32, why: impl std::fmt::Display) -> u32 {
        self.out.stage = marker;
        let text = format!("{why}");
        let n = text.len().min(self.out.message.len() - 1);
        self.out.message[..n].copy_from_slice(&text.as_bytes()[..n]);
        self.out.message[n] = 0;
        marker
    }
}

fn s(e: impl std::fmt::Display) -> String {
    format!("{e}")
}

fn finish(cb: &rt::CommandBuffer) -> Result<(), String> {
    cb.commit().map_err(s)?;
    cb.wait_until_completed().map_err(s)?;
    match cb.status() {
        rt::CommandBufferStatus::Completed => Ok(()),
        status => Err(format!("{status:?} {:?}", cb.error())),
    }
}

fn shared(d: &NmbDevice, bytes: &[u8]) -> Result<(rt::Block, rt::Buffer), String> {
    let len = bytes.len().max(4) as u64;
    let block = d.device.take_block(rt::MemoryClass::Shared, len).map_err(s)?;
    let buffer = d.device.new_buffer(block.id, 0, len, Some(0)).map_err(s)?;
    let p = buffer.contents().ok_or("no contents")?;
    // SAFETY: the buffer holds `len` >= bytes.len() bytes and no GPU work uses it.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.as_ptr(), bytes.len()) };
    Ok((block, buffer))
}

fn compute(d: &NmbDevice, library: &rt::Library) -> Result<u32, String> {
    const COUNT: usize = 1000;
    let function = library.function("scale_and_offset").map_err(s)?;
    let descriptor = rt::ComputePipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(function)));
    let state = crate::abi::shader::make_compute(d, &descriptor)?;
    let input: Vec<u8> = (0..COUNT).flat_map(|i| (i as f32).to_le_bytes()).collect();
    let (_ib, input) = shared(d, &input)?;
    let (_ob, output) = shared(d, &vec![0; COUNT * 4])?;
    let (_sb, scale) = shared(d, &2.0f32.to_le_bytes())?;
    let queue = d.device.new_command_queue(8).map_err(s)?;
    let cb = queue.command_buffer().map_err(s)?;
    let mut encoder = cb.compute_command_encoder(rt::DispatchType::Serial).map_err(s)?;
    encoder.set_compute_pipeline_state(&state).map_err(s)?;
    encoder.set_buffer(Some(&input), 0, 0).map_err(s)?;
    encoder.set_buffer(Some(&output), 0, 1).map_err(s)?;
    encoder.set_buffer(Some(&scale), 0, 2).map_err(s)?;
    encoder.dispatch_threads([COUNT as u32, 1, 1], [64, 1, 1]).map_err(s)?;
    encoder.end_encoding();
    finish(&cb)?;
    // SAFETY: COUNT floats, the work completed.
    let values = unsafe { std::slice::from_raw_parts(output.contents().ok_or("no contents")?.cast::<f32>().as_ptr(), COUNT) };
    Ok(values.iter().enumerate().filter(|(i, v)| **v != 3.0 * *i as f32).count() as u32)
}

/// Draws the fixture's top-left-quarter quad into `target` (cleared blue); with `readback`, copies it into that buffer.
fn draw_quad(d: &NmbDevice, library: &rt::Library, target: &rt::Texture, readback: Option<(&rt::Buffer, u64, u32)>) -> Result<(), String> {
    let vertex = library.function("colored_vertex").map_err(s)?;
    let fragment = library.function("colored_fragment").map_err(s)?;
    let mut descriptor = rt::RenderPipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(vertex)));
    descriptor.fragment = Some(rt::StageSource::Function(rt::FunctionDescriptor::new(fragment)));
    descriptor.color_attachments[0] = Some(rt::ColorAttachmentDescriptor::new(rt::PixelFormat::RGBA8Unorm));
    let state = crate::abi::shader::make_render(d, &descriptor)?;
    let positions: [[f32; 2]; 6] = [[-1.0, 1.0], [0.0, 1.0], [-1.0, 0.0], [0.0, 1.0], [0.0, 0.0], [-1.0, 0.0]];
    let positions: Vec<u8> = positions.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    let colors: Vec<u8> = (0..6).flat_map(|_| [1.0f32, 0.2, 0.0, 1.0]).flat_map(|v| v.to_le_bytes()).collect();
    let (_pb, positions) = shared(d, &positions)?;
    let (_cb, colors) = shared(d, &colors)?;
    let mut pass = rt::RenderPassDescriptor::default();
    let color = &mut pass.color_attachments[0];
    color.texture = Some(target.clone());
    color.load_action = rt::LoadAction::Clear;
    color.store_action = rt::StoreAction::Store;
    color.clear_color = rt::ClearColor { red: 0.0, green: 0.0, blue: 1.0, alpha: 1.0 };
    let queue = d.device.new_command_queue(8).map_err(s)?;
    let cb = queue.command_buffer().map_err(s)?;
    let mut encoder = cb.render_command_encoder(&pass).map_err(s)?;
    encoder.set_render_pipeline_state(&state).map_err(s)?;
    encoder.set_vertex_buffer(Some(&positions), 0, 0).map_err(s)?;
    encoder.set_vertex_buffer(Some(&colors), 0, 1).map_err(s)?;
    encoder.draw_primitives(rt::PrimitiveType::Triangle, 0, 6, 1, 0).map_err(s)?;
    encoder.end_encoding();
    if let Some((buffer, row, side)) = readback {
        let mut blit = cb.blit_command_encoder().map_err(s)?;
        let from = rt::TexturePlace { slice: 0, level: 0, origin: [0, 0, 0] };
        let layout = rt::BufferLayout { offset: 0, bytes_per_row: row, bytes_per_image: row * side as u64 };
        blit.copy_from_texture_to_buffer(target, from, [side, side, 1], buffer, layout).map_err(s)?;
        blit.end_encoding();
    }
    finish(&cb)
}

/// Texels off the expected picture: the top-left quarter (255, 51, 0, 255), the rest cleared blue.
fn wrong_texels(bytes: &[u8], row: usize, side: usize) -> u32 {
    let mut bad = 0;
    for y in 0..side {
        for x in 0..side {
            let at = y * row + x * 4;
            let inside = x < side / 2 && y < side / 2;
            let expected: [u8; 4] = if inside { [255, 51, 0, 255] } else { [0, 0, 255, 255] };
            if bytes[at..at + 4] != expected {
                bad += 1;
            }
        }
    }
    bad
}

fn render(d: &NmbDevice, library: &rt::Library) -> Result<u32, String> {
    const SIDE: u32 = 8;
    let mut desc = rt::TextureDescriptor::new(rt::TextureType::Type2D, rt::PixelFormat::RGBA8Unorm, SIDE, SIDE);
    desc.usage = rt::TextureUsage::RENDER_TARGET;
    desc.storage_mode = rt::StorageMode::Private;
    let index = d.indices.take(rt::IndexClass::Texture);
    let target = d.device.new_texture(&desc, index).map_err(s)?;
    let (_rb, readback) = shared(d, &vec![0; (SIDE * 4 * SIDE) as usize])?;
    draw_quad(d, library, &target, Some((&readback, (SIDE * 4) as u64, SIDE)))?;
    // SAFETY: SIDE rows of SIDE * 4 bytes, no longer written.
    let bytes = unsafe { std::slice::from_raw_parts(readback.contents().ok_or("no contents")?.as_ptr(), (SIDE * 4 * SIDE) as usize) };
    Ok(wrong_texels(bytes, (SIDE * 4) as usize, SIDE as usize))
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    /// libkern (libSystem): clean and invalidate the data cache over a range, from user space.
    fn sys_dcache_flush(start: *mut std::ffi::c_void, len: usize);
}

fn flush(_pages: *mut libc::c_void, _bytes: usize) {
    #[cfg(target_vendor = "apple")]
    // SAFETY: the range is the caller's mapping.
    unsafe {
        sys_dcache_flush(_pages, _bytes)
    };
}

/// The IOSurface path without an IOSurface: host pages imported without a copy, a render target over them.
fn import(d: &NmbDevice, library: &rt::Library) -> Result<u32, String> {
    const SIDE: u32 = 64;
    const ROW: u64 = 256;
    let bytes = (ROW * SIDE as u64) as usize;
    // An import's address and length are multiples of the device's minImportedHostPointerAlignment: 64 KiB covers every
    // driver seen (4 KiB on Turnip, 4 KiB on ANV).
    let len = bytes.div_ceil(65536) * 65536;
    // SAFETY: an anonymous private mapping, unmapped by the import's release once rt freed it.
    let pages = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
    if pages == libc::MAP_FAILED {
        return Err("mmap".into());
    }
    // SAFETY: just mapped.
    unsafe { std::ptr::write_bytes(pages as *mut u8, 0x5a, len) };
    let addr = pages as usize;
    let release: Box<dyn FnOnce() + Send> = Box::new(move || {
        // SAFETY: rt freed its import; nothing else uses the mapping.
        unsafe { libc::munmap(addr as *mut libc::c_void, len) };
    });
    // SAFETY: the mapping is readable and writable and stays mapped until the release runs.
    let (block, _) = unsafe { d.device.import_host_memory(NonNull::new(pages as *mut u8).ok_or("null")?, len as u64, Some(release)) }
        .map_err(|e| format!("import: {e}"))?;
    let buffer = d.device.new_buffer(block.id, 0, len as u64, None).map_err(s)?;
    let mut desc = rt::TextureDescriptor::new(rt::TextureType::Type2D, rt::PixelFormat::RGBA8Unorm, SIDE, SIDE);
    desc.usage = rt::TextureUsage::RENDER_TARGET | rt::TextureUsage::SHADER_READ;
    let index = d.indices.take(rt::IndexClass::Texture);
    let target = buffer.new_texture(&desc, index, 0, ROW).map_err(|e| format!("texture over pages: {e}"))?;
    flush(pages, bytes);
    draw_quad(d, library, &target, None)?;
    flush(pages, bytes);
    // SAFETY: the pages hold `bytes` bytes and no command buffer writes them now.
    let view = unsafe { std::slice::from_raw_parts(pages as *const u8, bytes) };
    Ok(wrong_texels(view, ROW as usize, SIDE as usize))
}

pub fn run_all(run_: &mut NmbSelftest, mark: Mark, d: &NmbDevice, qc: &[u8]) -> u32 {
    let mut run = Run { out: run_, mark };
    run.step(0x6502);
    let library = match rt::Library::from_air(FIXTURE_AIR) {
        Ok(l) => l,
        Err(why) => return run.fail(0x6502, why),
    };
    run.step(0x6503);
    match compute(d, &library) {
        Ok(bad) => run.out.compute_bad = bad,
        Err(why) => return run.fail(0x6503, why),
    }
    run.step(0x6504);
    match render(d, &library) {
        Ok(bad) => run.out.render_bad = bad,
        Err(why) => return run.fail(0x6504, why),
    }
    run.step(0x6505);
    match import(d, &library) {
        Ok(bad) => run.out.import_bad = bad,
        Err(why) => return run.fail(0x6505, why),
    }
    if !qc.is_empty() {
        run.step(0x6506);
        match crate::census::census(&d.device, &d.indices, qc) {
            Ok(c) => {
                run.out.qc_functions = c.functions;
                run.out.qc_kernels_ok = c.kernels_ok;
                run.out.qc_kernels_failed = c.kernels_failed;
                run.out.qc_pairs_ok = c.pairs_ok;
                run.out.qc_pairs_failed = c.pairs_failed;
            }
            Err(why) => return run.fail(0x6506, why),
        }
    }
    run.step(0x650f);
    0
}

/// # Safety
/// `loader` and `cache` are C strings; `qc` points at `qc_len` bytes (or is null with 0); `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_selftest(loader: *const c_char, cache: *const c_char, qc: *const u8, qc_len: usize, mark: Mark,
                                      out: *mut NmbSelftest) -> u32 {
    if loader.is_null() || cache.is_null() || out.is_null() {
        return 0x65ff;
    }
    // SAFETY: the caller's contract.
    let out = unsafe { &mut *out };
    // SAFETY: as above.
    let (loader, cache) = unsafe { (CStr::from_ptr(loader), CStr::from_ptr(cache)) };
    let qc: &[u8] = if qc.is_null() || qc_len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(qc, qc_len) } };
    // SAFETY: plain integers and bytes.
    *out = unsafe { std::mem::zeroed() };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(mark) = mark {
            // SAFETY: the caller's callback.
            unsafe { mark(0x6501) };
        }
        match crate::abi::device::open(&loader.to_string_lossy(), Some(&cache.to_string_lossy())) {
            Ok(d) => run_all(out, mark, &d, qc),
            Err(why) => Run { out, mark }.fail(0x6501, why),
        }
    }))
    .unwrap_or(0x65fe)
}
