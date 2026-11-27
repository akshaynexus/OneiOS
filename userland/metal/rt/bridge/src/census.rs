// SPDX-License-Identifier: MIT
//! A library's census on a device: every kernel made into a compute pipeline state, and every
//! vertex function with its library's own fragment (`<name>_vert_<x>` with `<name>_frag_<x>`, or
//! `_vert` with `_frag`) made into a render pipeline state of one BGRA8Unorm attachment, through
//! the runtime, so the driver compiles each. The plug-in's self-test runs it over QuartzCore's
//! library (nmb_selftest's qc_ counts); `m2v-rt-check --census` over any metallibs on the laptop.

use metal2vulkan::library::FunctionStage;
use metal2vulkan_runtime as rt;
use std::time::{Duration, Instant};

#[derive(Default, Debug)]
pub struct Census {
    pub functions: u32,
    pub kernels_ok: u32,
    pub kernels_failed: u32,
    pub pairs_ok: u32,
    pub pairs_failed: u32,
    /// Tile functions, not made: a tile pipeline needs its pass's attachments.
    pub tiles: u32,
    /// Wall time of the makes (translation, lowering and the driver's compile).
    pub time: Duration,
    /// Each failure: the function (or pair) and the runtime's reason, first line.
    pub failures: Vec<(String, String)>,
}

fn first_line(e: impl std::fmt::Display) -> String {
    let text = format!("{e}");
    text.lines().next().unwrap_or("").chars().take(240).collect()
}

/// The census of the metallib (or AIR) `bytes` on `device`.
pub fn census(device: &rt::Device, indices: &crate::indices::Indices, bytes: &[u8]) -> Result<Census, String> {
    let library = rt::Library::parse(bytes).map_err(|e| format!("library: {}", first_line(e)))?;
    let mut out = Census::default();
    let start = Instant::now();
    let mut vertices = Vec::new();
    let mut fragments = Vec::new();
    let infos: Vec<(String, FunctionStage)> = library.functions().map(|f| (f.name.to_string(), f.stage)).collect();
    out.functions = infos.len() as u32;
    for (name, stage) in infos {
        match stage {
            FunctionStage::Kernel => {
                let made = library.function(&name).map_err(|e| first_line(e)).and_then(|function| {
                    let descriptor = rt::ComputePipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(function)));
                    device.new_compute_pipeline_state(&descriptor, |s| indices.sampler_ids(s)).map(|_| ()).map_err(first_line)
                });
                match made {
                    Ok(()) => out.kernels_ok += 1,
                    Err(why) if why.contains("TileAttachments") => out.tiles += 1,
                    Err(why) => {
                        out.kernels_failed += 1;
                        out.failures.push((name, why));
                    }
                }
            }
            FunctionStage::Vertex => vertices.push(name),
            FunctionStage::Fragment => fragments.push(name),
            _ => {}
        }
    }
    for vname in &vertices {
        let Some(fname) = [vname.replace("_vert_", "_frag_"), vname.replacen("_vert", "_frag", 1)]
            .into_iter()
            .find(|candidate| fragments.iter().any(|n| n == candidate))
        else {
            continue;
        };
        let made = (|| -> Result<(), String> {
            let v = library.function(vname).map_err(first_line)?;
            let f = library.function(&fname).map_err(first_line)?;
            let mut descriptor = rt::RenderPipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(v)));
            descriptor.fragment = Some(rt::StageSource::Function(rt::FunctionDescriptor::new(f)));
            descriptor.color_attachments[0] = Some(rt::ColorAttachmentDescriptor::new(rt::PixelFormat::BGRA8Unorm));
            device.new_render_pipeline_state(&descriptor, |s| indices.sampler_ids(s)).map(|_| ()).map_err(first_line)
        })();
        match made {
            Ok(()) => out.pairs_ok += 1,
            Err(why) => {
                out.pairs_failed += 1;
                out.failures.push((format!("{vname}+{fname}"), why));
            }
        }
    }
    out.time = start.elapsed();
    Ok(out)
}

/// One vertex/fragment pair of the library `bytes`, made with each color format of `formats` as
/// its only attachment (index 0): the runtime's whole answer for each (not cut to a line), so a
/// refusal a client's crash report truncates (`... at metada`) is read whole. A pair from two
/// different shader families (QuartzCore's `downsample_blur_vert_lph` with `alpha_threshold_lph`,
/// issue #81) is one the name-matched census never makes.
pub fn pair(
    device: &rt::Device,
    indices: &crate::indices::Indices,
    bytes: &[u8],
    vertex: &str,
    fragment: &str,
    formats: &[rt::PixelFormat],
) -> Result<Vec<(rt::PixelFormat, &'static str, Result<(), String>)>, String> {
    use metal2vulkan::metal::{BlendFactor, ColorWriteMask};
    let library = rt::Library::parse(bytes).map_err(|e| format!("library: {}", first_line(e)))?;
    let mut out = Vec::new();
    // Each format plain, with the premultiplied "over" blend Core Animation draws layers with, and with a mask of
    // alpha alone and of the colour alone (the refusals of a blend or a mask differ by format: A8Unorm's alpha in red,
    // the extended-range forms).
    let variants: [(&'static str, bool, ColorWriteMask); 4] = [
        ("plain", false, ColorWriteMask::ALL),
        ("over", true, ColorWriteMask::ALL),
        ("mask-a", false, ColorWriteMask::ALPHA),
        ("mask-rgb", false, ColorWriteMask::RED | ColorWriteMask::GREEN | ColorWriteMask::BLUE),
    ];
    for &format in formats {
        for &(variant, blend, mask) in &variants {
            let made = (|| -> Result<(), String> {
                let v = library.function(vertex).map_err(|e| format!("{e}"))?;
                let f = library.function(fragment).map_err(|e| format!("{e}"))?;
                let layout = stage_in_layout(&v)?;
                let mut descriptor = rt::RenderPipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(v)));
                descriptor.vertex_descriptor = layout;
                descriptor.fragment = Some(rt::StageSource::Function(rt::FunctionDescriptor::new(f)));
                let mut color = rt::ColorAttachmentDescriptor::new(format);
                color.write_mask = mask;
                if blend {
                    color.blending_enabled = true;
                    color.destination_rgb_blend_factor = BlendFactor::OneMinusSourceAlpha;
                    color.destination_alpha_blend_factor = BlendFactor::OneMinusSourceAlpha;
                }
                descriptor.color_attachments[0] = Some(color);
                device.new_render_pipeline_state(&descriptor, |s| indices.sampler_ids(s)).map(|_| ()).map_err(|e| format!("{e}"))
            })();
            out.push((format, variant, made));
        }
    }
    Ok(out)
}

/// The vertex descriptor a client gives a `[[stage_in]]` vertex function: each of its attributes
/// at the vertex format of its own type, packed one after another in buffer 30 (a buffer index
/// QuartzCore's functions bind nothing at). None for a function without attributes.
fn stage_in_layout(function: &metal2vulkan::library::Function) -> Result<Option<rt::VertexDescriptor>, String> {
    use metal2vulkan::metal::VertexFormat;
    use metal2vulkan::pipeline::{VertexAttributeDescriptor, VertexBufferLayoutDescriptor};
    const BUFFER: u32 = 30;
    let properties = function.properties(&metal2vulkan::Budget::standard()).map_err(|e| format!("properties: {e}"))?;
    let Some(attributes) = properties.vertex_attributes() else { return Ok(None) };
    let mut descriptor = rt::VertexDescriptor::new();
    let mut offset = 0u32;
    for a in attributes {
        let ty = a.attribute_type().name().trim_start_matches("MTLDataType");
        let format = VertexFormat::from_name(&format!("MTLVertexFormat{ty}"))
            .ok_or_else(|| format!("attribute {} ({}): no vertex format for {ty}", a.attribute_index(), a.name()))?;
        // 4 bytes a lane, at most 4 lanes: enough room for every float, half, int and short type
        let lanes = ty.chars().last().and_then(|c| c.to_digit(10)).unwrap_or(1);
        descriptor.attributes.insert(a.attribute_index(), VertexAttributeDescriptor::new(format, offset, BUFFER));
        offset += 4 * lanes;
    }
    descriptor.layouts.insert(BUFFER, VertexBufferLayoutDescriptor::new(Some(offset.max(4))));
    Ok(Some(descriptor))
}
