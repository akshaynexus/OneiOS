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
