// SPDX-License-Identifier: MIT
//! Textures: rt's textures at the bridge's indices (given back through the retire gate), over buffers and surface planes, in
//! heaps, and views. Memoryless storage is a Private texture (rt builds none: Gap::MemorylessTexture). What rt keeps no
//! getter of (the options word, rotation, framebufferOnly, isDrawable, the purgeable state) the handle keeps.

use super::device::{device_of, DeviceRef, NmbDevice};
use super::memory::{purgeable, NmbBuffer, NmbHeap};
use super::{boxed, e, get, get_mut, guard, raw, release, words, Options};
use metal2vulkan_runtime as rt;
use rt::{PixelFormat, TextureSwizzle, TextureSwizzleChannels, TextureType, TextureUsage};
use std::ffi::c_void;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

#[repr(C)]
pub struct NmbTextureDescriptor {
    pub kind: u64,
    pub format: u64,
    pub width: u64,
    pub height: u64,
    pub depth: u64,
    pub levels: u64,
    pub samples: u64,
    pub array_length: u64,
    pub usage: u64,
    pub options: u64,
    pub rotation: u64,
    pub framebuffer_only: u64,
    pub is_drawable: u64,
}

pub struct NmbTexture {
    pub(crate) texture: rt::Texture,
    pub(crate) device: DeviceRef,
    index: Option<u32>,
    options: u64,
    rotation: u64,
    framebuffer_only: bool,
    is_drawable: bool,
    purgeable: AtomicU64,
    /// A heap texture's heap and range, given back through the gate (automatic heaps).
    heap: Option<(Arc<super::memory::HeapInner>, Option<(u64, u64)>)>,
}

impl Drop for NmbTexture {
    fn drop(&mut self) {
        let d = device_of(self.device);
        if let Some(index) = self.index.take() {
            let indices = d.indices.clone();
            d.gate.after_retire(move || indices.give(rt::IndexClass::Texture, index));
        }
        if let Some((heap, Some(range))) = self.heap.take() {
            d.gate.after_retire(move || heap.free(range));
        }
    }
}

fn u32_of(v: u64, what: &str) -> Result<u32, String> {
    u32::try_from(v).map_err(|_| format!("{what} {v} is past 32 bits"))
}

fn descriptor(d: *const NmbTextureDescriptor) -> Result<(rt::TextureDescriptor, &'static NmbTextureDescriptor), String> {
    let d: &'static NmbTextureDescriptor = get(d)?;
    let mut out = rt::TextureDescriptor::new(raw::<TextureType>(d.kind, "texture type")?, raw::<PixelFormat>(d.format, "pixel format")?,
                                             u32_of(d.width, "width")?, u32_of(d.height.max(1), "height")?);
    out.depth = u32_of(d.depth.max(1), "depth")?;
    out.mipmap_level_count = u32_of(d.levels.max(1), "levels")?;
    out.sample_count = u32_of(d.samples.max(1), "samples")?;
    out.array_length = u32_of(d.array_length.max(1), "array length")?;
    out.usage = raw::<TextureUsage>(d.usage, "texture usage")?;
    out.storage_mode = Options(d.options).storage_mode();
    Ok((out, d))
}

/// Whether a shader may reach a texture of `usage`: a read or write, a render target (a fragment function reads it through its
/// attachment), or Unknown (0), which Metal takes as every usage.
fn reached(usage: TextureUsage) -> bool {
    usage.as_raw() == 0 || usage.intersects(TextureUsage::SHADER_READ | TextureUsage::SHADER_WRITE | TextureUsage::RENDER_TARGET)
}

impl NmbTexture {
    fn new(texture: rt::Texture, d: &NmbDevice, index: Option<u32>, source: &NmbTextureDescriptor) -> NmbTexture {
        NmbTexture {
            texture,
            device: d as *const NmbDevice,
            index,
            options: source.options,
            rotation: source.rotation,
            framebuffer_only: source.framebuffer_only != 0,
            is_drawable: source.is_drawable != 0,
            purgeable: AtomicU64::new(2),
            heap: None,
        }
    }
}

/// Makes a texture at a new index where a shader may reach it; the index goes back where the make fails.
fn at_index(d: &NmbDevice, usage: TextureUsage, make: impl FnOnce(Option<u32>) -> Result<rt::Texture, rt::Error>)
            -> Result<(rt::Texture, Option<u32>), String> {
    let index = if reached(usage) { Some(d.indices.take(rt::IndexClass::Texture).ok_or("no texture index is free")?) } else { None };
    match make(index) {
        Ok(t) => Ok((t, index)),
        Err(why) => {
            if let Some(i) = index {
                d.indices.give(rt::IndexClass::Texture, i);
            }
            Err(format!("{why}"))
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new(device: *mut NmbDevice, descriptor_: *const NmbTextureDescriptor) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || {
        let d = get(device)?;
        let (desc, source) = descriptor(descriptor_)?;
        let (t, index) = at_index(d, desc.usage, |i| d.device.new_texture(&desc, i)).map_err(|w| format!("texture: {w}"))?;
        Ok(boxed(NmbTexture::new(t, d, index, source)))
    })
}

fn over_buffer(buffer: *mut NmbBuffer, descriptor_: *const NmbTextureDescriptor, offset: u64, bytes_per_row: u64)
               -> Result<*mut NmbTexture, String> {
    let b = get(buffer)?;
    let d = device_of(b.device);
    let (desc, source) = descriptor(descriptor_)?;
    let (t, index) = at_index(d, desc.usage, |i| b.buffer.new_texture(&desc, i, offset, bytes_per_row))
        .map_err(|w| format!("texture over a buffer: {w}"))?;
    Ok(boxed(NmbTexture::new(t, d, index, source)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new_on_buffer(buffer: *mut NmbBuffer, descriptor_: *const NmbTextureDescriptor, offset: u64,
                                            bytes_per_row: u64, _iosurface: bool) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || over_buffer(buffer, descriptor_, offset, bytes_per_row))
}

/// `newTextureWithDescriptor:iosurface:plane:`: the asked plane's bytes as a texture over the surface's buffer. A two-plane
/// texture of a YCbCr format (one texture reading both planes) is not made here: rt has no such texture over a buffer.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new_on_surface(buffer: *mut NmbBuffer, descriptor_: *const NmbTextureDescriptor, offset: u64,
                                             bytes_per_row: u64, _pixel_format: u32, _plane: u64, _width: u64, _height: u64,
                                             _chroma_offset: u64, chroma_bytes_per_row: u64) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || {
        if chroma_bytes_per_row != 0 {
            return Err("a two-plane YCbCr texture over a surface is not made by the rt bridge".into());
        }
        over_buffer(buffer, descriptor_, offset, bytes_per_row)
    })
}

/// `newLinearTextureWithDescriptor:offset:bytesPerRow:bytesPerImage:` (Apple SPI): as over a buffer, of one image.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new_linear(buffer: *mut NmbBuffer, descriptor_: *const NmbTextureDescriptor, offset: u64, bytes_per_row: u64,
                                         _bytes_per_image: u64) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || over_buffer(buffer, descriptor_, offset, bytes_per_row))
}

fn view(texture: *mut NmbTexture, format: u64, kind: Option<u64>, levels: *const u64, slices: *const u64, swizzle: Option<u32>)
        -> Result<*mut NmbTexture, String> {
    let t = get(texture)?;
    let d = device_of(t.device);
    let parent = t.texture.descriptor();
    let format = raw::<PixelFormat>(format, "pixel format")?;
    let (kind, l, s) = match kind {
        None => {
            let slices = match parent.texture_type {
                TextureType::Cube | TextureType::CubeArray => parent.array_length * 6,
                _ => parent.array_length,
            };
            (parent.texture_type, 0..parent.mipmap_level_count, 0..slices)
        }
        Some(kind) => {
            let (l, s) = (words(levels, 2, "levels")?, words(slices, 2, "slices")?);
            let l = u32_of(l[0], "level")?..u32_of(l[0].checked_add(l[1]).ok_or("levels overflow")?, "level")?;
            let s = u32_of(s[0], "slice")?..u32_of(s[0].checked_add(s[1]).ok_or("slices overflow")?, "slice")?;
            (raw::<TextureType>(kind, "texture type")?, l, s)
        }
    };
    let (v, index) = at_index(d, parent.usage, |i| match swizzle {
        None => t.texture.new_texture_view(format, kind, l, s, i),
        Some(w) => {
            let channel = |shift: u32| raw::<TextureSwizzle>(((w >> shift) & 0xff) as u64, "texture swizzle").expect("a swizzle");
            let channels = TextureSwizzleChannels { red: channel(0), green: channel(8), blue: channel(16), alpha: channel(24) };
            t.texture.new_texture_view_with_swizzle(format, kind, l, s, channels, i)
        }
    })
    .map_err(|w| format!("texture view: {w}"))?;
    let mut out = NmbTexture::new(v, d, index, &NmbTextureDescriptor {
        kind: 0, format: 0, width: 0, height: 0, depth: 0, levels: 0, samples: 0, array_length: 0, usage: 0,
        options: t.options, rotation: t.rotation, framebuffer_only: t.framebuffer_only as u64, is_drawable: 0,
    });
    out.framebuffer_only = t.framebuffer_only;
    Ok(boxed(out))
}

/// A view of `texture`: of the whole texture as `format` where `whole`, else of `levels` and `slices` (start, count) as `format`
/// and `kind`.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new_view(texture: *mut NmbTexture, format: u64, kind: u64, levels: *const u64, slices: *const u64, whole: bool)
                                       -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || view(texture, format, (!whole).then_some(kind), levels, slices, None))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_new_view_swizzle(texture: *mut NmbTexture, format: u64, kind: u64, levels: *const u64, slices: *const u64,
                                               swizzle: u32) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || {
        for shift in [0, 8, 16, 24] {
            raw::<TextureSwizzle>(((swizzle >> shift) & 0xff) as u64, "texture swizzle")?;
        }
        view(texture, format, Some(kind), levels, slices, Some(swizzle))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_set_purgeable_state(texture: *const NmbTexture, state: u64) -> u64 {
    guard(0, || Ok(purgeable(&get(texture)?.purgeable, state)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_release(texture: *mut NmbTexture) {
    release(texture)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_describe(texture: *const NmbTexture, out: *mut NmbTextureDescriptor) {
    guard((), || {
        let t = get(texture)?;
        let d = t.texture.descriptor();
        let out = get_mut(out)?;
        *out = NmbTextureDescriptor {
            kind: d.texture_type.as_raw() as u64,
            format: d.pixel_format.as_raw() as u64,
            width: d.width as u64,
            height: d.height as u64,
            depth: d.depth as u64,
            levels: d.mipmap_level_count as u64,
            samples: d.sample_count as u64,
            array_length: d.array_length as u64,
            usage: d.usage.as_raw() as u64,
            options: t.options,
            rotation: t.rotation,
            framebuffer_only: t.framebuffer_only as u64,
            is_drawable: t.is_drawable as u64,
        };
        Ok(())
    })
}

fn region(r: *const u64) -> Result<rt::Region, String> {
    let r = words(r, 6, "region")?;
    Ok(rt::Region {
        origin: [u32_of(r[0], "x")?, u32_of(r[1], "y")?, u32_of(r[2], "z")?],
        size: [u32_of(r[3], "width")?, u32_of(r[4], "height")?, u32_of(r[5], "depth")?],
    })
}

/// # Safety
/// `bytes` holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_texture_replace(texture: *mut NmbTexture, region_words: *const u64, level: u64, slice: u64,
                                             bytes: *const c_void, bytes_per_row: u64, bytes_per_image: u64, length: u64) -> bool {
    guard(false, || {
        if bytes.is_null() {
            return Err("null bytes".into());
        }
        // SAFETY: the caller's contract.
        let host = unsafe { rt::HostBytes::from_raw(bytes as *const u8, length as usize) };
        get(texture)?
            .texture
            .replace_region(region(region_words)?, u32_of(level, "level")?, u32_of(slice, "slice")?, host, bytes_per_row, bytes_per_image)
            .map_err(e("replace region"))?;
        Ok(true)
    })
}

/// # Safety
/// `bytes` holds `length` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_texture_get_bytes(texture: *const NmbTexture, bytes: *mut c_void, bytes_per_row: u64, bytes_per_image: u64,
                                               length: u64, region_words: *const u64, level: u64, slice: u64) -> bool {
    guard(false, || {
        if bytes.is_null() {
            return Err("null bytes".into());
        }
        // SAFETY: the caller's contract.
        let out = unsafe { std::slice::from_raw_parts_mut(bytes as *mut u8, length as usize) };
        get(texture)?
            .texture
            .get_bytes(out, bytes_per_row, bytes_per_image, region(region_words)?, u32_of(level, "level")?, u32_of(slice, "slice")?)
            .map_err(e("get bytes"))?;
        Ok(true)
    })
}

/// A texture in the heap, placed as `nmb_heap_new_buffer` places a buffer (`u64::MAX`: the heap places it).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_heap_new_texture(heap: *const NmbHeap, descriptor_: *const NmbTextureDescriptor, offset: u64) -> *mut NmbTexture {
    guard(std::ptr::null_mut(), || {
        let h = get(heap)?;
        let d = device_of(h.device);
        let (mut desc, source) = descriptor(descriptor_)?;
        desc.storage_mode = Options(h.options).storage_mode();
        let (at, placed) = if offset == u64::MAX {
            let sa = d.device.heap_texture_size_and_align(&desc).map_err(e("heap texture size"))?;
            let size = sa.size.div_ceil(sa.align.max(1)) * sa.align.max(1);
            let at = h.inner.place(size, sa.align).ok_or("the heap has no room")?;
            (at, Some((at, size)))
        } else {
            (offset, None)
        };
        match at_index(d, desc.usage, |i| d.device.new_heap_texture(&desc, i, h.inner.range(), at)) {
            Ok((t, index)) => {
                let mut out = NmbTexture::new(t, d, index, source);
                out.options = h.options;
                out.heap = Some((h.inner.clone(), placed));
                Ok(boxed(out))
            }
            Err(why) => {
                if let Some(range) = placed {
                    h.inner.free(range);
                }
                Err(format!("heap texture: {why}"))
            }
        }
    })
}

/// `heapTextureSizeAndAlignWithDescriptor:`: the size rounded up to a multiple of the alignment, as Metal's answers are (a heap
/// sized as the sum of its textures' answers then holds them; NativeMetal 0017).
#[unsafe(no_mangle)]
pub extern "C" fn nmb_device_heap_texture_size_and_align(device: *const NmbDevice, descriptor_: *const NmbTextureDescriptor, out: *mut u64) -> bool {
    guard(false, || {
        let (desc, _) = descriptor(descriptor_)?;
        let sa = get(device)?.device.heap_texture_size_and_align(&desc).map_err(e("heap texture size"))?;
        let align = sa.align.max(1);
        let out = words(out as *const u64, 2, "answer")?.as_ptr() as *mut u64;
        // SAFETY: the header's two words.
        unsafe { (*out, *out.add(1)) = (sa.size.div_ceil(align) * align, align) };
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_texture_resource_id(texture: *const NmbTexture) -> u64 {
    guard(0, || Ok(get(texture)?.texture.gpu_resource_id().to_bits()))
}
