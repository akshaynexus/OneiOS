// SPDX-License-Identifier: MIT
//! m2v-rt-check DRIVER [--floor-only]: opens a Vulkan driver the way the iOS bridge does, with no
//! Vulkan loader (the driver's own `vk_icdGetInstanceProcAddr`, else its `vkGetInstanceProcAddr`,
//! as the instance's entry: rt patch 0001), checks each of its devices against metal2vulkan-rt's
//! floor and prints every item a device lacks, then opens the first device that passes and runs
//! one compute dispatch of the runtime's own example kernel (scale_and_offset over 1000 floats),
//! checking each output. A lasting tool: Turnip over Mesa's noop DRM shim on the build laptop
//! answers what the Adreno 750 offers (userland/metal/rt/README.md), ANV or lavapipe prove the
//! direct path runs work. Exit 0 when the dispatch's 1000 outputs are right (or, with
//! --floor-only, when a device passes the floor).

use adreno_metal_bridge_rt::driver;
use metal2vulkan_runtime as rt;
use std::process::ExitCode;

static FIXTURE_AIR: &[u8] = include_bytes!(env!("NMB_FIXTURE_AIR"));

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: m2v-rt-check DRIVER.so|DRIVER.dylib [--floor-only]");
        return ExitCode::from(2);
    };
    let floor_only = args.iter().any(|a| a == "--floor-only");
    if args.iter().any(|a| a == "--selftest") {
        return selftest(path, args.iter().position(|a| a == "--qc").and_then(|i| args.get(i + 1)));
    }
    let opened = match driver::open(path) {
        Ok(opened) => opened,
        Err(why) => {
            eprintln!("m2v-rt-check: {why}");
            return ExitCode::from(1);
        }
    };
    println!("driver {path}: entry {}", opened.entry_name);
    let instance = match opened.instance() {
        Ok(instance) => instance,
        Err(why) => {
            eprintln!("m2v-rt-check: instance: {why}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<String> = instance.extensions().iter().map(|n| n.to_string_lossy().into_owned()).collect();
    println!("instance extensions enabled: {}", names.join(" "));
    let devices = match instance.physical_devices() {
        Ok(devices) => devices,
        Err(why) => {
            eprintln!("m2v-rt-check: devices: {why}");
            return ExitCode::from(1);
        }
    };
    let mut chosen = None;
    for physical in &devices {
        let name = driver::device_name(&instance, physical);
        match physical.check() {
            Ok(caps) => {
                println!("{name}: above the floor");
                println!("  float16 {} storage_read_without_format {} int64 atomics buffer {} shared {} float64 {}",
                    caps.float16, caps.storage_read_without_format, caps.buffer_int64_atomics,
                    caps.shared_int64_atomics, caps.float64);
                println!("  maximal_reconvergence {} subgroup_uniform_control_flow {} texel_offset_operand {} attachment_reads {} feedback_loop {}",
                    caps.maximal_reconvergence, caps.subgroup_uniform_control_flow, caps.texel_offset_operand,
                    caps.attachment_reads, caps.feedback_loop);
                println!("  indirect command buffers (VK_EXT_device_generated_commands): {}",
                    match caps.indirect_commands { Some(l) => format!("{l:?}"), None => "no: refused (IndirectCommandsKind::Extension)".into() });
                let exts: Vec<String> = caps.extensions.iter().map(|e| e.to_string_lossy().into_owned()).collect();
                println!("  device extensions enabled: {}", exts.join(" "));
                for (i, t) in caps.memory.memory_types[..caps.memory.memory_type_count as usize].iter().enumerate() {
                    println!("  memory type {i}: heap {} flags {:#x} (1 device-local, 2 host-visible, 4 coherent, 8 cached)", t.heap_index, t.property_flags.as_raw());
                }
                if chosen.is_none() {
                    chosen = Some(physical.clone());
                }
            }
            Err(below) => println!("{name}: {below}"),
        }
    }
    let Some(physical) = chosen else {
        println!("no device above the floor");
        return ExitCode::from(1);
    };
    if floor_only {
        return ExitCode::SUCCESS;
    }
    if let Some(at) = args.iter().position(|a| a == "--census") {
        return census_all(&physical, &args[at + 1..]);
    }
    match compute(&physical) {
        Ok(0) => {
            println!("compute: 1000 of 1000 outputs right");
            ExitCode::SUCCESS
        }
        Ok(bad) => {
            println!("compute: {bad} of 1000 outputs wrong");
            ExitCode::from(1)
        }
        Err(why) => {
            println!("compute: {why}");
            ExitCode::from(1)
        }
    }
}

fn compute(physical: &rt::PhysicalDevice) -> Result<u32, String> {
    const COUNT: usize = 1000;
    let e = |e: rt::Error| format!("{e}");
    let device = rt::Device::new(physical, rt::DeviceOptions::default()).map_err(e)?;
    let library = rt::Library::from_air(FIXTURE_AIR).map_err(|x| format!("library: {x}"))?;
    let function = library.function("scale_and_offset").map_err(|x| format!("function: {x}"))?;
    let descriptor = rt::ComputePipelineDescriptor::new(rt::StageSource::Function(rt::FunctionDescriptor::new(function)));
    let state = device
        .new_compute_pipeline_state(&descriptor, |samplers| Ok(vec![0; samplers.len()]))
        .map_err(|x| format!("pipeline: {x}"))?;
    let shared = |len: u64| -> Result<(rt::Block, rt::Buffer), String> {
        let block = device.take_block(rt::MemoryClass::Shared, len).map_err(e)?;
        let buffer = device.new_buffer(block.id, 0, len, Some(0)).map_err(e)?;
        Ok((block, buffer))
    };
    let (_ib, input) = shared((COUNT * 4) as u64)?;
    let (_ob, output) = shared((COUNT * 4) as u64)?;
    let (_sb, scale) = shared(4)?;
    // SAFETY: Shared buffers of the lengths written, which no GPU work uses yet.
    unsafe {
        let p = input.contents().ok_or("no contents")?.cast::<f32>().as_ptr();
        for i in 0..COUNT {
            p.add(i).write(i as f32);
        }
        scale.contents().ok_or("no contents")?.cast::<f32>().as_ptr().write(2.0);
    }
    let queue = device.new_command_queue(64).map_err(e)?;
    let command_buffer = queue.command_buffer().map_err(e)?;
    let mut encoder = command_buffer.compute_command_encoder(rt::DispatchType::Serial).map_err(e)?;
    encoder.set_compute_pipeline_state(&state).map_err(e)?;
    encoder.set_buffer(Some(&input), 0, 0).map_err(e)?;
    encoder.set_buffer(Some(&output), 0, 1).map_err(e)?;
    encoder.set_buffer(Some(&scale), 0, 2).map_err(e)?;
    encoder.dispatch_threads([COUNT as u32, 1, 1], [64, 1, 1]).map_err(e)?;
    encoder.end_encoding();
    command_buffer.commit().map_err(e)?;
    command_buffer.wait_until_completed().map_err(e)?;
    if command_buffer.status() != rt::CommandBufferStatus::Completed {
        return Err(format!("{:?} {:?}", command_buffer.status(), command_buffer.error()));
    }
    // SAFETY: the buffer holds COUNT floats and its work completed.
    let values = unsafe { std::slice::from_raw_parts(output.contents().ok_or("no contents")?.cast::<f32>().as_ptr(), COUNT) };
    Ok(values.iter().enumerate().filter(|(i, v)| **v != 3.0 * *i as f32).count() as u32)
}

/// `--census FILE...`: each library's census on one device; every failure listed. Exit 0 when no
/// kernel or pair failed.
fn census_all(physical: &rt::PhysicalDevice, files: &[String]) -> ExitCode {
    let mut options = rt::DeviceOptions::default();
    options.pipeline_cache_dir = std::env::var_os("M2V_CHECK_CACHE").map(Into::into);
    let device = match rt::Device::new(physical, options) {
        Ok(device) => device,
        Err(why) => {
            println!("device: {why}");
            return ExitCode::from(1);
        }
    };
    let indices = match adreno_metal_bridge_rt::indices::Indices::grant(&device) {
        Ok(indices) => indices,
        Err(why) => {
            println!("grants: {why}");
            return ExitCode::from(1);
        }
    };
    let mut failed = 0;
    for file in files {
        let bytes = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(why) => {
                println!("{file}: {why}");
                failed += 1;
                continue;
            }
        };
        match adreno_metal_bridge_rt::census::census(&device, &indices, &bytes) {
            Ok(c) => {
                println!("census {file}: {} functions, kernels {} ok {} failed, pairs {} ok {} failed, tile functions {}, {} ms",
                    c.functions, c.kernels_ok, c.kernels_failed, c.pairs_ok, c.pairs_failed, c.tiles, c.time.as_millis());
                for (name, why) in &c.failures {
                    println!("  failed {name}: {why}");
                }
                failed += c.kernels_failed + c.pairs_failed;
            }
            Err(why) => {
                println!("census {file}: {why}");
                failed += 1;
            }
        }
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// `--selftest [--qc FILE]`: the plug-in's nmb_selftest steps through the bridge's own device open (compute, render, a render
/// into imported host pages, and the census of FILE, QuartzCore's library on the phone). Exit 0 when every step ran and every
/// output and texel was right.
fn selftest(path: &str, qc: Option<&String>) -> ExitCode {
    let qc = qc.map(|f| std::fs::read(f).expect("the --qc file")).unwrap_or_default();
    let device = match adreno_metal_bridge_rt::abi::device::open(path, None) {
        Ok(d) => d,
        Err(why) => {
            println!("selftest: device: {why}");
            return ExitCode::from(1);
        }
    };
    // SAFETY: plain integers and bytes.
    let mut out: adreno_metal_bridge_rt::selftest::NmbSelftest = unsafe { std::mem::zeroed() };
    let t = std::time::Instant::now();
    let code = adreno_metal_bridge_rt::selftest::run_all(&mut out, None, &device, &qc);
    let message = String::from_utf8_lossy(&out.message).trim_end_matches('\0').to_string();
    println!("selftest {:#x} ({} ms): compute_bad {} render_bad {} import_bad {} qc functions {} kernels {} ok {} failed, pairs {} ok {} failed{}",
             code, t.elapsed().as_millis(), out.compute_bad, out.render_bad, out.import_bad, out.qc_functions, out.qc_kernels_ok,
             out.qc_kernels_failed, out.qc_pairs_ok, out.qc_pairs_failed, if message.is_empty() { String::new() } else { format!(": {message}") });
    if code == 0 && out.compute_bad == 0 && out.render_bad == 0 && out.import_bad == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
