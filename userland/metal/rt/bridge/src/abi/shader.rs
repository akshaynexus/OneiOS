// SPDX-License-Identifier: MIT
//! Libraries and functions (the translator's), dynamic libraries, compute and render pipeline states (rt's, their samplers
//! numbered at the bridge's indices), their reflection (the translator's, as JSON for the frontend), binary archives (made as
//! rt makes the states; nothing is kept: rt's pipeline cache directory keeps what it compiled), argument encoders (the
//! translator's layouts, written by the bridge through the buffer's mapping: rt has no encoder), samplers and depth/stencil
//! states.

use super::device::{device_of, DeviceRef, NmbDevice};
use super::memory::NmbBuffer;
use super::texture::NmbTexture;
use super::{boxed, copy_out, e, get, get_mut, guard, raw, release};
use metal2vulkan::bind::{ArgumentBufferLayout, ArgumentDescriptor, ArgumentType};
use metal2vulkan::library::{FunctionConstantValue, FunctionConstantValues, Library};
use metal2vulkan::metal::{
    BindingAccess, BlendFactor, BlendOperation, ColorWriteMask, CompareFunction, DataType, PixelFormat, PrimitiveTopologyClass,
    SamplerAddressMode, SamplerBorderColor, SamplerMinMagFilter, SamplerMipFilter, StencilOperation, TessellationControlPointIndexType,
    TessellationFactorFormat, TessellationFactorStepFunction, TessellationPartitionMode, TextureType, VertexFormat, VertexStepFunction,
    Winding,
};
use metal2vulkan::metal::state::{DepthStencilDescriptor, StencilDescriptor};
use metal2vulkan::pipeline::{
    ColorAttachmentDescriptor, ComputePipelineDescriptor, DynamicLibraries, FunctionDescriptor, LinkedFunction, LinkedFunctions,
    RenderPipelineDescriptor, StageSource, TessellationDescriptor, VertexAttributeDescriptor, VertexBufferLayoutDescriptor, VertexDescriptor,
};
use metal2vulkan_runtime as rt;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Arc;

fn budget() -> metal2vulkan::Budget {
    metal2vulkan::Budget::standard()
}

fn name_of<'a>(name: *const c_char) -> Result<&'a str, String> {
    if name.is_null() {
        return Err("null name".into());
    }
    // SAFETY: the caller's contract: a C string.
    unsafe { CStr::from_ptr(name) }.to_str().map_err(e("name"))
}

// ---- libraries and functions ----

pub struct NmbLibrary(pub(crate) Library, Vec<String>);

/// The AIR64 slice of a universal metallib (rt's parse takes either; kept for bare-AIR callers).
/// # Safety
/// `bytes` holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_library_new(device: *mut NmbDevice, bytes: *const c_void, length: usize, air: bool) -> *mut NmbLibrary {
    guard(std::ptr::null_mut(), || {
        get(device)?;
        if bytes.is_null() || length == 0 {
            return Err("no library bytes".into());
        }
        // SAFETY: the caller's contract.
        let bytes = unsafe { std::slice::from_raw_parts(bytes as *const u8, length) };
        let library = if air { Library::from_air(bytes).map_err(e("AIR"))? } else { Library::parse(bytes).map_err(e("metallib"))? };
        let names = library.functions().map(|f| f.name.to_string()).collect();
        Ok(boxed(NmbLibrary(library, names)))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_library_release(library: *mut NmbLibrary) {
    release(library)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_library_function_count(library: *const NmbLibrary) -> u32 {
    guard(0, || Ok(get(library)?.1.len() as u32))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_library_function_name(library: *const NmbLibrary, index: u32, out: *mut c_char, size: usize) -> usize {
    guard(0, || Ok(copy_out(get(library)?.1.get(index as usize).ok_or("function index")?, out, size)))
}

/// A function, its constants (specialized where `specialized`), and the name it links under.
pub struct NmbFunction {
    pub(crate) function: metal2vulkan::library::Function,
    constants: FunctionConstantValues,
    specialized: bool,
    specialized_name: Option<String>,
}

impl NmbFunction {
    fn new(function: metal2vulkan::library::Function, constants: FunctionConstantValues, specialized: bool) -> NmbFunction {
        NmbFunction { function, constants, specialized, specialized_name: None }
    }

    pub(crate) fn descriptor(&self) -> FunctionDescriptor {
        let mut d = FunctionDescriptor::new(self.function.clone());
        d.constants = self.constants.clone();
        d.specialized = self.specialized;
        d
    }

    fn linked(&self) -> LinkedFunction {
        let mut l = LinkedFunction::new(self.function.clone());
        l.constants = self.constants.clone();
        if let Some(name) = &self.specialized_name {
            l = l.with_specialized_name(name.clone());
        }
        l
    }
}

/// # Safety
/// `name` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_new(library: *mut NmbLibrary, name: *const c_char) -> *mut NmbFunction {
    guard(std::ptr::null_mut(), || {
        let function = get(library)?.0.function(name_of(name)?).map_err(e("function"))?;
        Ok(boxed(NmbFunction::new(function, FunctionConstantValues::defaults(), false)))
    })
}

#[repr(C)]
pub struct NmbConstant {
    pub kind: u64,
    pub index: u64,
    pub name: *const c_char,
    pub bytes: *const c_void,
    pub length: u64,
}

/// Packed bytes of a function constant's `MTLDataType` (scalars and vectors), 0 for a type that is not given a value.
fn constant_bytes(raw: u32) -> usize {
    let (first, scalar) = match raw {
        3..=6 => (3, 4),
        16..=19 => (16, 2),
        29..=32 => (29, 4),
        33..=36 => (33, 4),
        37..=40 => (37, 2),
        41..=44 => (41, 2),
        45..=48 => (45, 1),
        49..=52 => (49, 1),
        53..=56 => (53, 1),
        81..=84 => (81, 8),
        85..=88 => (85, 8),
        121..=124 => (121, 2),
        _ => return 0,
    };
    (raw - first + 1) as usize * scalar
}

/// Sets every constant `function` declares that `values` leaves out to zero (false for a bool).
fn zero_rest(function: &metal2vulkan::library::Function, values: &mut FunctionConstantValues) -> Result<(), String> {
    for (key, constant) in function.function_constants_dictionary().map_err(e("function constants"))? {
        if values.by_name.contains_key(&key) || values.by_index.contains_key(&constant.index()) {
            continue;
        }
        let ty = constant.data_type();
        let bytes = constant_bytes(ty.as_raw());
        if bytes == 0 {
            return Err(format!("function constant {key} of type {}", ty.as_raw()));
        }
        values.by_index.insert(constant.index(), FunctionConstantValue::new(ty, vec![0; bytes]));
    }
    Ok(())
}

unsafe fn specialized(library: *mut NmbLibrary, name: *const c_char, constants: *const NmbConstant, count: u32, zero: bool) -> *mut NmbFunction {
    guard(std::ptr::null_mut(), || {
        if constants.is_null() && count > 0 {
            return Err("null constants".into());
        }
        let function = get(library)?.0.function(name_of(name)?).map_err(e("function"))?;
        // SAFETY: the caller's contract.
        let constants = if count == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(constants, count as usize) } };
        let mut values = FunctionConstantValues::defaults();
        for c in constants {
            if c.bytes.is_null() {
                return Err("constant without bytes".into());
            }
            // SAFETY: the caller's contract.
            let bytes = unsafe { std::slice::from_raw_parts(c.bytes as *const u8, c.length as usize) };
            let value = FunctionConstantValue::new(raw::<DataType>(c.kind, "constant type")?, bytes.to_vec());
            if c.name.is_null() {
                values.by_index.insert(u32::try_from(c.index).map_err(e("constant index"))?, value);
            } else {
                values.by_name.insert(name_of(c.name)?.to_string(), value);
            }
        }
        if zero {
            zero_rest(&function, &mut values)?;
        }
        Ok(boxed(NmbFunction::new(function, values, true)))
    })
}

/// # Safety
/// `name` is a C string; `constants` holds `count` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_new_specialized(library: *mut NmbLibrary, name: *const c_char, constants: *const NmbConstant,
                                                      count: u32) -> *mut NmbFunction {
    // SAFETY: the caller's contract.
    unsafe { specialized(library, name, constants, count, false) }
}

/// # Safety
/// As for `nmb_function_new_specialized`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_new_specialized_zero_rest(library: *mut NmbLibrary, name: *const c_char,
                                                                constants: *const NmbConstant, count: u32) -> *mut NmbFunction {
    // SAFETY: the caller's contract.
    unsafe { specialized(library, name, constants, count, true) }
}

/// The function with every constant it declares set to zero, for shader priming.
/// # Safety
/// `name` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_new_zero_constants(library: *mut NmbLibrary, name: *const c_char) -> *mut NmbFunction {
    // SAFETY: no constants are read.
    unsafe { specialized(library, name, std::ptr::null(), 0, true) }
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_function_release(function: *mut NmbFunction) {
    release(function)
}

/// `MTLFunctionDescriptor.specializedName`: the function links under `name` (null: its own name).
/// # Safety
/// `name` is null or a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_function_set_specialized_name(function: *mut NmbFunction, name: *const c_char) -> bool {
    guard(false, || {
        let name = if name.is_null() { None } else { Some(name_of(name)?.to_string()) };
        get_mut(function)?.specialized_name = name;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_function_type(function: *const NmbFunction) -> u64 {
    guard(0, || Ok(get(function)?.function.properties(&budget()).map_err(e("function type"))?.function_type().as_raw() as u64))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_function_patch_type(function: *const NmbFunction) -> u64 {
    guard(0, || Ok(get(function)?.function.properties(&budget()).map_err(e("patch type"))?.patch_type().as_raw() as u64))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_function_patch_control_point_count(function: *const NmbFunction) -> i64 {
    guard(-1, || {
        let p = get(function)?.function.properties(&budget()).map_err(e("patch control point count"))?;
        Ok(p.patch_control_point_count().map_or(-1, i64::from))
    })
}

// ---- dynamic libraries ----

pub struct NmbDynamicLibrary(metal2vulkan::library::DynamicLibrary);

/// `newDynamicLibraryWithURL:error:`: the dynamic library in the file `path` (Core Image's CoreImage.metallib).
/// # Safety
/// `path` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_dynamic_library_new_with_url(device: *const NmbDevice, path: *const c_char) -> *mut NmbDynamicLibrary {
    guard(std::ptr::null_mut(), || {
        get(device)?;
        let bytes = std::fs::read(name_of(path)?).map_err(e("dynamic library"))?;
        let library = Library::parse(&bytes).map_err(e("dynamic library"))?;
        Ok(boxed(NmbDynamicLibrary(metal2vulkan::library::DynamicLibrary::loaded(library).map_err(e("dynamic library"))?)))
    })
}

/// `newDynamicLibrary:error:`.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_dynamic_library_new(device: *const NmbDevice, library: *const NmbLibrary) -> *mut NmbDynamicLibrary {
    guard(std::ptr::null_mut(), || {
        get(device)?;
        let library = metal2vulkan::library::DynamicLibrary::new(get(library)?.0.clone(), &budget()).map_err(e("dynamic library"))?;
        Ok(boxed(NmbDynamicLibrary(library)))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_dynamic_library_install_name(library: *const NmbDynamicLibrary, out: *mut c_char, size: usize) -> usize {
    guard(0, || Ok(copy_out(get(library)?.0.install_name(), out, size)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_dynamic_library_release(library: *mut NmbDynamicLibrary) {
    release(library)
}

fn handles<'a, T>(p: *const *mut T, count: u64, what: &str) -> Result<&'a [*mut T], String> {
    if count == 0 {
        return Ok(&[]);
    }
    if p.is_null() {
        return Err(format!("null {what}"));
    }
    // SAFETY: the caller's contract: `count` live handles.
    Ok(unsafe { std::slice::from_raw_parts(p, usize::try_from(count).map_err(e("count"))?) })
}

fn preloaded(p: *const *mut NmbDynamicLibrary, count: u64) -> Result<DynamicLibraries, String> {
    let mut out = DynamicLibraries::new();
    for &l in handles(p, count, "preloaded libraries")? {
        out = out.with_preloaded(get(l)?.0.clone());
    }
    Ok(out)
}

fn linked(functions: *const *mut NmbFunction, count: u64, private: *const *mut NmbFunction, private_count: u64)
          -> Result<LinkedFunctions, String> {
    let mut out = LinkedFunctions::new();
    out.functions = handles(functions, count, "linked functions")?.iter().map(|&f| Ok(get(f)?.linked())).collect::<Result<_, String>>()?;
    out.private_functions =
        handles(private, private_count, "private linked functions")?.iter().map(|&f| Ok(get(f)?.linked())).collect::<Result<_, String>>()?;
    Ok(out)
}

// ---- pipelines ----

pub struct NmbComputePipeline(pub(crate) rt::ComputePipelineState);
pub struct NmbRenderPipeline(pub(crate) rt::RenderPipelineState);

#[repr(C)]
pub struct NmbComputePipelineDescriptor {
    pub function: *mut NmbFunction,
    pub max_threads: u64,
    pub multiple_of_execution_width: u64,
    pub indirect: u64,
    pub max_call_stack_depth: u64,
    pub adding_binary_functions: u64,
    pub mutability: [u64; 31],
    pub linked: *const *mut NmbFunction,
    pub linked_count: u64,
    pub private_linked: *const *mut NmbFunction,
    pub private_linked_count: u64,
    pub preloaded: *const *mut NmbDynamicLibrary,
    pub preloaded_count: u64,
}

#[repr(C)]
pub struct NmbPipelineOptions {
    pub flags: u64,
    pub archives: *const *mut NmbBinaryArchive,
    pub archive_count: usize,
}

fn compute_descriptor(descriptor: *const NmbComputePipelineDescriptor) -> Result<ComputePipelineDescriptor, String> {
    let d = get(descriptor)?;
    let kernel = get(d.function)?
        .descriptor()
        .with_linked(linked(d.linked, d.linked_count, d.private_linked, d.private_linked_count)?)
        .with_libraries(preloaded(d.preloaded, d.preloaded_count)?);
    let mut out = ComputePipelineDescriptor::new(StageSource::Function(kernel));
    out.max_total_threads_per_threadgroup = if d.max_threads == 0 { None } else { Some(u32::try_from(d.max_threads).map_err(e("max threads"))?) };
    out.threadgroup_size_is_multiple_of_execution_width = d.multiple_of_execution_width != 0;
    out.support_indirect_command_buffers = d.indirect != 0;
    out.max_call_stack_depth = u32::try_from(d.max_call_stack_depth).map_err(e("stack depth"))?;
    out.support_adding_binary_functions = d.adding_binary_functions != 0;
    for (index, &m) in d.mutability.iter().enumerate() {
        if m != 0 {
            out.buffers.insert(index as u32, raw::<metal2vulkan::metal::Mutability>(m, "buffer mutability")?);
        }
    }
    Ok(out)
}

pub(crate) fn make_compute(d: &NmbDevice, descriptor: &ComputePipelineDescriptor) -> Result<rt::ComputePipelineState, String> {
    let indices = d.indices.clone();
    d.device.new_compute_pipeline_state(descriptor, |s| indices.sampler_ids(s)).map_err(e("compute pipeline"))
}

pub(crate) fn make_render(d: &NmbDevice, descriptor: &RenderPipelineDescriptor) -> Result<rt::RenderPipelineState, String> {
    let indices = d.indices.clone();
    d.device.new_render_pipeline_state(descriptor, |s| indices.sampler_ids(s)).map_err(e("render pipeline"))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_new(device: *mut NmbDevice, function: *mut NmbFunction) -> *mut NmbComputePipeline {
    // SAFETY: null options.
    unsafe { nmb_compute_pipeline_new_with_options(device, function, std::ptr::null()) }
}

/// # Safety
/// `options` is null or a valid record (its archives are not read: rt keeps its own pipeline cache).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_compute_pipeline_new_with_options(device: *mut NmbDevice, function: *mut NmbFunction,
                                                              _options: *const NmbPipelineOptions) -> *mut NmbComputePipeline {
    guard(std::ptr::null_mut(), || {
        let descriptor = ComputePipelineDescriptor::new(StageSource::Function(get(function)?.descriptor()));
        Ok(boxed(NmbComputePipeline(make_compute(get(device)?, &descriptor)?)))
    })
}

/// # Safety
/// As above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_compute_pipeline_new_with_descriptor(device: *mut NmbDevice, descriptor: *const NmbComputePipelineDescriptor,
                                                                 _options: *const NmbPipelineOptions) -> *mut NmbComputePipeline {
    guard(std::ptr::null_mut(), || Ok(boxed(NmbComputePipeline(make_compute(get(device)?, &compute_descriptor(descriptor)?)?))))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_release(pipeline: *mut NmbComputePipeline) {
    release(pipeline)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_max_threads(pipeline: *const NmbComputePipeline) -> u64 {
    guard(0, || Ok(get(pipeline)?.0.max_total_threads_per_threadgroup() as u64))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_execution_width(pipeline: *const NmbComputePipeline) -> u64 {
    guard(0, || Ok(get(pipeline)?.0.thread_execution_width() as u64))
}

#[repr(C)]
pub struct NmbColorAttachment {
    pub format: u64,
    pub write_mask: u64,
    pub blending: u64,
    pub source_rgb: u64,
    pub destination_rgb: u64,
    pub rgb_operation: u64,
    pub source_alpha: u64,
    pub destination_alpha: u64,
    pub alpha_operation: u64,
}

#[repr(C)]
pub struct NmbVertexAttribute {
    pub format: u64,
    pub offset: u64,
    pub buffer: u64,
}

#[repr(C)]
pub struct NmbVertexLayout {
    pub stride: u64,
    pub step_function: u64,
    pub step_rate: u64,
}

#[repr(C)]
pub struct NmbTessellation {
    pub tessellates: u64,
    pub max_factor: u64,
    pub partition_mode: u64,
    pub factor_format: u64,
    pub factor_step_function: u64,
    pub output_winding: u64,
    pub control_point_index_type: u64,
    pub factor_scale_enabled: u64,
}

#[repr(C)]
pub struct NmbRenderPipelineDescriptor {
    pub vertex: *mut NmbFunction,
    pub fragment: *mut NmbFunction,
    pub colors: [NmbColorAttachment; 8],
    pub depth_format: u64,
    pub stencil_format: u64,
    pub samples: u64,
    pub topology: u64,
    pub rasterization: u64,
    pub vertex_inputs: u64,
    pub attributes: [NmbVertexAttribute; 31],
    pub layouts: [NmbVertexLayout; 31],
    pub tessellation: NmbTessellation,
    pub vertex_linked: *const *mut NmbFunction,
    pub vertex_linked_count: u64,
    pub vertex_private_linked: *const *mut NmbFunction,
    pub vertex_private_linked_count: u64,
    pub fragment_linked: *const *mut NmbFunction,
    pub fragment_linked_count: u64,
    pub fragment_private_linked: *const *mut NmbFunction,
    pub fragment_private_linked_count: u64,
    pub vertex_preloaded: *const *mut NmbDynamicLibrary,
    pub vertex_preloaded_count: u64,
    pub fragment_preloaded: *const *mut NmbDynamicLibrary,
    pub fragment_preloaded_count: u64,
}

fn optional_format(value: u64) -> Result<Option<PixelFormat>, String> {
    if value == 0 { Ok(None) } else { raw::<PixelFormat>(value, "pixel format").map(Some) }
}

fn render_descriptor(descriptor: *const NmbRenderPipelineDescriptor) -> Result<RenderPipelineDescriptor, String> {
    let d = get(descriptor)?;
    let vertex = get(d.vertex)?
        .descriptor()
        .with_linked(linked(d.vertex_linked, d.vertex_linked_count, d.vertex_private_linked, d.vertex_private_linked_count)?)
        .with_libraries(preloaded(d.vertex_preloaded, d.vertex_preloaded_count)?);
    let mut out = RenderPipelineDescriptor::new(StageSource::Function(vertex));
    if !d.fragment.is_null() {
        let fragment = get(d.fragment)?
            .descriptor()
            .with_linked(linked(d.fragment_linked, d.fragment_linked_count, d.fragment_private_linked, d.fragment_private_linked_count)?)
            .with_libraries(preloaded(d.fragment_preloaded, d.fragment_preloaded_count)?);
        out.fragment = Some(StageSource::Function(fragment));
    }
    for (slot, c) in out.color_attachments.iter_mut().zip(d.colors.iter()) {
        if c.format == 0 {
            continue;
        }
        let mut a = ColorAttachmentDescriptor::new(raw::<PixelFormat>(c.format, "color format")?);
        a.write_mask = raw::<ColorWriteMask>(c.write_mask, "write mask")?;
        a.blending_enabled = c.blending != 0;
        a.source_rgb_blend_factor = raw::<BlendFactor>(c.source_rgb, "blend factor")?;
        a.destination_rgb_blend_factor = raw::<BlendFactor>(c.destination_rgb, "blend factor")?;
        a.rgb_blend_operation = raw::<BlendOperation>(c.rgb_operation, "blend operation")?;
        a.source_alpha_blend_factor = raw::<BlendFactor>(c.source_alpha, "blend factor")?;
        a.destination_alpha_blend_factor = raw::<BlendFactor>(c.destination_alpha, "blend factor")?;
        a.alpha_blend_operation = raw::<BlendOperation>(c.alpha_operation, "blend operation")?;
        *slot = Some(a);
    }
    out.depth_format = optional_format(d.depth_format)?;
    out.stencil_format = optional_format(d.stencil_format)?;
    out.raster_sample_count = d.samples.max(1) as u32;
    out.input_primitive_topology = raw::<PrimitiveTopologyClass>(d.topology, "topology")?;
    out.rasterization_enabled = d.rasterization != 0;
    if d.vertex_inputs != 0 {
        let mut inputs = VertexDescriptor::new();
        for (i, a) in d.attributes.iter().enumerate() {
            if a.format == 0 {
                continue;
            }
            inputs.attributes.insert(i as u32, VertexAttributeDescriptor::new(raw::<VertexFormat>(a.format, "vertex format")?,
                                                                                a.offset as u32, a.buffer as u32));
            if let Some(l) = d.layouts.get(a.buffer as usize) {
                let mut layout = VertexBufferLayoutDescriptor::new(Some(l.stride as u32));
                layout.step_function = raw::<VertexStepFunction>(l.step_function, "step function")?;
                layout.step_rate = l.step_rate as u32;
                inputs.layouts.insert(a.buffer as u32, layout);
            }
        }
        out.vertex_descriptor = Some(inputs);
    }
    let t = &d.tessellation;
    if t.tessellates != 0 {
        let mut tessellation = TessellationDescriptor::default();
        tessellation.max_tessellation_factor = u32::try_from(t.max_factor).map_err(|_| "tessellation factor".to_string())?;
        tessellation.partition_mode = raw::<TessellationPartitionMode>(t.partition_mode, "partition mode")?;
        tessellation.factor_format = raw::<TessellationFactorFormat>(t.factor_format, "factor format")?;
        tessellation.factor_step_function = raw::<TessellationFactorStepFunction>(t.factor_step_function, "factor step function")?;
        tessellation.output_winding_order = raw::<Winding>(t.output_winding, "output winding")?;
        tessellation.control_point_index_type =
            raw::<TessellationControlPointIndexType>(t.control_point_index_type, "control point index type")?;
        tessellation.factor_scale_enabled = t.factor_scale_enabled != 0;
        out.tessellation = Some(tessellation);
    }
    Ok(out)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_pipeline_new(device: *mut NmbDevice, descriptor: *const NmbRenderPipelineDescriptor) -> *mut NmbRenderPipeline {
    // SAFETY: null options.
    unsafe { nmb_render_pipeline_new_with_options(device, descriptor, std::ptr::null()) }
}

/// # Safety
/// As for the compute pipelines.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_render_pipeline_new_with_options(device: *mut NmbDevice, descriptor: *const NmbRenderPipelineDescriptor,
                                                              _options: *const NmbPipelineOptions) -> *mut NmbRenderPipeline {
    guard(std::ptr::null_mut(), || Ok(boxed(NmbRenderPipeline(make_render(get(device)?, &render_descriptor(descriptor)?)?))))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_pipeline_release(pipeline: *mut NmbRenderPipeline) {
    release(pipeline)
}

/// No pipeline state's resource ID in rt (indirect command buffers are refused on this device): 0.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_pipeline_resource_id(_pipeline: *const NmbRenderPipeline) -> u64 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_resource_id(_pipeline: *const NmbComputePipeline) -> u64 {
    0
}

// ---- reflection ----

pub struct NmbReflection(std::ffi::CString);

mod reflection_json {
    use metal2vulkan::reflection::{ArrayType, Binding, PointerType, StructType, TextureReferenceType};
    use std::fmt::Write;

    fn string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn structure(out: &mut String, s: &StructType) {
        out.push_str("{\"members\":[");
        for (i, m) in s.members().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            string(out, m.name());
            let _ = write!(out, ",\"offset\":{},\"dataType\":{},\"argumentIndex\":{}", m.offset(), m.data_type().as_raw(), m.argument_index());
            if let Some(t) = m.struct_type() {
                out.push_str(",\"struct\":");
                structure(out, t);
            }
            if let Some(t) = m.array_type() {
                out.push_str(",\"array\":");
                array(out, t);
            }
            if let Some(t) = m.pointer_type() {
                out.push_str(",\"pointer\":");
                pointer(out, t);
            }
            if let Some(t) = m.texture_reference_type() {
                out.push_str(",\"texture\":");
                texture_reference(out, t);
            }
            out.push('}');
        }
        out.push_str("]}");
    }

    fn array(out: &mut String, a: &ArrayType) {
        let _ = write!(out, "{{\"elementType\":{},\"length\":{},\"stride\":{},\"argumentIndexStride\":{}", a.element_type().as_raw(),
                       a.array_length(), a.stride(), a.argument_index_stride());
        if let Some(t) = a.element_struct_type() {
            out.push_str(",\"struct\":");
            structure(out, t);
        }
        if let Some(t) = a.element_array_type() {
            out.push_str(",\"array\":");
            array(out, t);
        }
        if let Some(t) = a.element_pointer_type() {
            out.push_str(",\"pointer\":");
            pointer(out, t);
        }
        if let Some(t) = a.element_texture_reference_type() {
            out.push_str(",\"texture\":");
            texture_reference(out, t);
        }
        out.push('}');
    }

    fn pointer(out: &mut String, p: &PointerType) {
        let _ = write!(out, "{{\"elementType\":{},\"access\":{},\"alignment\":{},\"size\":{},\"argumentBuffer\":{}", p.element_type().as_raw(),
                       p.access().as_raw(), p.alignment(), p.data_size(), p.element_is_argument_buffer());
        if let Some(t) = p.element_struct_type() {
            out.push_str(",\"struct\":");
            structure(out, t);
        }
        if let Some(t) = p.element_array_type() {
            out.push_str(",\"array\":");
            array(out, t);
        }
        out.push('}');
    }

    fn texture_reference(out: &mut String, t: &TextureReferenceType) {
        let _ = write!(out, "{{\"dataType\":{},\"textureType\":{},\"access\":{},\"depth\":{}}}", t.texture_data_type().as_raw(),
                       t.texture_type().as_raw(), t.access().as_raw(), t.is_depth_texture());
    }

    fn binding(out: &mut String, b: &Binding) {
        out.push_str("{\"name\":");
        string(out, b.name());
        let _ = write!(out, ",\"type\":{},\"access\":{},\"index\":{},\"used\":{},\"argument\":{}", b.binding_type().as_raw(), b.access().as_raw(),
                       b.index(), b.is_used(), b.is_argument());
        if let Some(buffer) = b.buffer() {
            let _ = write!(out, ",\"buffer\":{{\"alignment\":{},\"size\":{},\"dataType\":{}", buffer.buffer_alignment(), buffer.buffer_data_size(),
                           buffer.buffer_data_type().as_raw());
            if let Some(t) = buffer.buffer_struct_type() {
                out.push_str(",\"struct\":");
                structure(out, t);
            }
            if let Some(t) = buffer.buffer_pointer_type() {
                out.push_str(",\"pointer\":");
                pointer(out, t);
            }
            out.push('}');
        }
        if let Some(t) = b.threadgroup() {
            let _ = write!(out, ",\"threadgroup\":{{\"alignment\":{},\"size\":{}}}", t.threadgroup_memory_alignment(), t.threadgroup_memory_data_size());
        }
        if let Some(t) = b.texture() {
            let _ = write!(out, ",\"texture\":{{\"textureType\":{},\"dataType\":{},\"depth\":{},\"arrayLength\":{}}}", t.texture_type().as_raw(),
                           t.texture_data_type().as_raw(), t.is_depth_texture(), t.array_length());
        }
        out.push('}');
    }

    pub(super) fn stage(out: &mut String, name: &str, bindings: &[Binding]) {
        let _ = write!(out, "\"{name}\":[");
        for (i, b) in bindings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            binding(out, b);
        }
        out.push(']');
    }
}

fn reflection_handle(json: String) -> Result<*mut NmbReflection, String> {
    Ok(boxed(NmbReflection(std::ffi::CString::new(json).map_err(|_| "a NUL in the reflection".to_string())?)))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_render_pipeline_reflection(descriptor: *const NmbRenderPipelineDescriptor) -> *mut NmbReflection {
    guard(std::ptr::null_mut(), || {
        let reflection = render_descriptor(descriptor)?.reflection(&budget()).map_err(e("render pipeline reflection"))?;
        let mut json = String::from("{");
        reflection_json::stage(&mut json, "vertex", reflection.vertex_bindings());
        json.push(',');
        reflection_json::stage(&mut json, "fragment", reflection.fragment_bindings());
        json.push('}');
        reflection_handle(json)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_compute_pipeline_reflection(descriptor: *const NmbComputePipelineDescriptor) -> *mut NmbReflection {
    guard(std::ptr::null_mut(), || {
        let reflection = compute_descriptor(descriptor)?.reflection(&budget()).map_err(e("compute pipeline reflection"))?;
        let mut json = String::from("{");
        reflection_json::stage(&mut json, "bindings", reflection.bindings());
        json.push('}');
        reflection_handle(json)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_reflection_json(reflection: *const NmbReflection) -> *const c_char {
    guard(std::ptr::null(), || Ok(get(reflection)?.0.as_ptr()))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_reflection_release(reflection: *mut NmbReflection) {
    release(reflection)
}

// ---- binary archives ----

/// `MTLBinaryArchive`: what Metal keeps in one (the compiled states) rt keeps in its pipeline cache directory, so an archive's
/// additions make the state (warming that cache) and its file is written empty.
pub struct NmbBinaryArchive(DeviceRef);

/// # Safety
/// `path` is null or a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_binary_archive_new(device: *mut NmbDevice, _path: *const c_char) -> *mut NmbBinaryArchive {
    guard(std::ptr::null_mut(), || Ok(boxed(NmbBinaryArchive(get(device)? as *const NmbDevice))))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_binary_archive_release(archive: *mut NmbBinaryArchive) {
    release(archive)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_binary_archive_add_render(archive: *mut NmbBinaryArchive, descriptor: *const NmbRenderPipelineDescriptor) -> bool {
    guard(false, || {
        make_render(device_of(get(archive)?.0), &render_descriptor(descriptor)?)?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_binary_archive_add_compute(archive: *mut NmbBinaryArchive, function: *mut NmbFunction) -> bool {
    guard(false, || {
        let descriptor = ComputePipelineDescriptor::new(StageSource::Function(get(function)?.descriptor()));
        make_compute(device_of(get(archive)?.0), &descriptor)?;
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_binary_archive_add_compute_descriptor(archive: *mut NmbBinaryArchive, descriptor: *const NmbComputePipelineDescriptor) -> bool {
    guard(false, || {
        make_compute(device_of(get(archive)?.0), &compute_descriptor(descriptor)?)?;
        Ok(true)
    })
}

/// # Safety
/// `path` is a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_binary_archive_serialize(archive: *mut NmbBinaryArchive, path: *const c_char) -> bool {
    guard(false, || {
        get(archive)?;
        std::fs::write(name_of(path)?, b"").map_err(e("archive serialize"))?;
        Ok(true)
    })
}

// ---- samplers and depth/stencil states ----

pub struct NmbSampler {
    pub(crate) state: rt::SamplerState,
    device: DeviceRef,
    index: u32,
}

impl Drop for NmbSampler {
    fn drop(&mut self) {
        let d = device_of(self.device);
        let (indices, index) = (d.indices.clone(), self.index);
        d.gate.after_retire(move || indices.give(rt::IndexClass::Sampler, index));
    }
}

#[repr(C)]
pub struct NmbSamplerDescriptor {
    pub min_filter: u64,
    pub mag_filter: u64,
    pub mip_filter: u64,
    pub s_address: u64,
    pub t_address: u64,
    pub r_address: u64,
    pub border_color: u64,
    pub normalized: u64,
    pub max_anisotropy: u64,
    pub compare: u64,
    pub lod_average: u64,
    pub lod_min: f32,
    pub lod_max: f32,
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_sampler_new(device: *mut NmbDevice, descriptor: *const NmbSamplerDescriptor) -> *mut NmbSampler {
    guard(std::ptr::null_mut(), || {
        let dev = get(device)?;
        let d = get(descriptor)?;
        let mut out = rt::SamplerDescriptor::new();
        out.min_filter = raw::<SamplerMinMagFilter>(d.min_filter, "min filter")?;
        out.mag_filter = raw::<SamplerMinMagFilter>(d.mag_filter, "mag filter")?;
        out.mip_filter = raw::<SamplerMipFilter>(d.mip_filter, "mip filter")?;
        out.s_address_mode = raw::<SamplerAddressMode>(d.s_address, "address mode")?;
        out.t_address_mode = raw::<SamplerAddressMode>(d.t_address, "address mode")?;
        out.r_address_mode = raw::<SamplerAddressMode>(d.r_address, "address mode")?;
        out.border_color = raw::<SamplerBorderColor>(d.border_color, "border color")?;
        out.normalized_coordinates = d.normalized != 0;
        out.max_anisotropy = d.max_anisotropy.max(1) as u32;
        out.compare_function = raw::<CompareFunction>(d.compare, "compare function")?;
        out.lod_average = d.lod_average != 0;
        out.lod_min_clamp = d.lod_min;
        out.lod_max_clamp = d.lod_max;
        let index = dev.indices.take(rt::IndexClass::Sampler).ok_or("no sampler index is free")?;
        match dev.device.new_sampler_state(&out, index) {
            Ok(state) => Ok(boxed(NmbSampler { state, device: dev as *const NmbDevice, index })),
            Err(why) => {
                dev.indices.give(rt::IndexClass::Sampler, index);
                Err(format!("sampler: {why}"))
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_sampler_release(sampler: *mut NmbSampler) {
    release(sampler)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_sampler_resource_id(sampler: *const NmbSampler) -> u64 {
    guard(0, || Ok(get(sampler)?.state.gpu_resource_id().to_bits()))
}

pub struct NmbDepthStencil(pub(crate) rt::DepthStencilState);

#[repr(C)]
pub struct NmbStencil {
    pub compare: u64,
    pub failure: u64,
    pub depth_failure: u64,
    pub pass: u64,
    pub read_mask: u64,
    pub write_mask: u64,
}

#[repr(C)]
pub struct NmbDepthStencilDescriptor {
    pub compare: u64,
    pub write: u64,
    pub front: NmbStencil,
    pub back: NmbStencil,
}

fn stencil(s: &NmbStencil) -> Result<StencilDescriptor, String> {
    let mut out = StencilDescriptor::default();
    out.stencil_compare_function = raw::<CompareFunction>(s.compare, "stencil compare")?;
    out.stencil_failure_operation = raw::<StencilOperation>(s.failure, "stencil operation")?;
    out.depth_failure_operation = raw::<StencilOperation>(s.depth_failure, "stencil operation")?;
    out.depth_stencil_pass_operation = raw::<StencilOperation>(s.pass, "stencil operation")?;
    out.read_mask = s.read_mask as u32;
    out.write_mask = s.write_mask as u32;
    Ok(out)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_depth_stencil_new(device: *mut NmbDevice, descriptor: *const NmbDepthStencilDescriptor) -> *mut NmbDepthStencil {
    guard(std::ptr::null_mut(), || {
        let d = get(descriptor)?;
        let mut out = DepthStencilDescriptor::default();
        out.depth_compare_function = raw::<CompareFunction>(d.compare, "depth compare")?;
        out.depth_write_enabled = d.write != 0;
        out.front_face_stencil = stencil(&d.front)?;
        out.back_face_stencil = stencil(&d.back)?;
        Ok(boxed(NmbDepthStencil(get(device)?.device.new_depth_stencil_state(&out))))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_depth_stencil_release(state: *mut NmbDepthStencil) {
    release(state)
}

// ---- argument encoders ----

/// `MTLArgumentEncoder`: a translator layout, and the argument buffer it writes into (its mapping, offset by the start and
/// element). Each argument is 8 bytes: a buffer's address plus its offset, any other object's resource ID.
pub struct NmbArgumentEncoder {
    layout: Arc<ArgumentBufferLayout>,
    target: Option<(*mut u8, u64)>,
}

#[repr(C)]
pub struct NmbArgumentDescriptor {
    pub data_type: u32,
    pub index: u32,
    pub array_length: u32,
    pub access: u32,
    pub texture_type: u32,
    pub constant_block_alignment: u32,
}

/// # Safety
/// `arguments` holds `count` descriptors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nmb_argument_encoder_new(device: *const NmbDevice, arguments: *const NmbArgumentDescriptor, count: usize)
                                                  -> *mut NmbArgumentEncoder {
    guard(std::ptr::null_mut(), || {
        get(device)?;
        // SAFETY: the caller's contract.
        let list = if count == 0 || arguments.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(arguments, count) } };
        let mut descriptors = Vec::with_capacity(list.len());
        for a in list {
            let mut d = ArgumentDescriptor::new(raw::<DataType>(a.data_type as u64, "argument data type")?, a.index);
            d.array_length = a.array_length;
            d.access = raw::<BindingAccess>(a.access as u64, "argument access")?;
            d.texture_type = raw::<TextureType>(a.texture_type as u64, "argument texture type")?;
            d.constant_block_alignment = a.constant_block_alignment;
            descriptors.push(d);
        }
        let layout = ArgumentBufferLayout::from_descriptors(&descriptors).map_err(|w| format!("argument encoder: {w:?}"))?;
        Ok(boxed(NmbArgumentEncoder { layout: Arc::new(layout), target: None }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_function_argument_encoder(function: *const NmbFunction, index: u32) -> *mut NmbArgumentEncoder {
    guard(std::ptr::null_mut(), || {
        let f = get(function)?;
        let layout = f.function.argument_buffer_layout(index, &f.constants, &budget()).map_err(|w| format!("function argument encoder: {w:?}"))?;
        Ok(boxed(NmbArgumentEncoder { layout: Arc::new(layout), target: None }))
    })
}

/// The member of `layout` that holds argument `index`, and the byte offset of that element.
fn member(layout: &ArgumentBufferLayout, index: u32) -> Option<(&metal2vulkan::bind::ArgumentMember, u64)> {
    for m in &layout.members {
        let span = m.count.saturating_mul(m.ids_per_element.max(1));
        if index >= m.id && index < m.id.saturating_add(span) {
            let element = (index - m.id) / m.ids_per_element.max(1);
            let at = m.offset + element as u64 * m.stride;
            if m.ty == ArgumentType::Struct {
                let inner = m.layout.as_ref()?;
                let base = m.id + element * m.ids_per_element;
                return member(inner, index - base).map(|(im, off)| (im, at + off));
            }
            return Some((m, at));
        }
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_nested(encoder: *const NmbArgumentEncoder, index: u32) -> *mut NmbArgumentEncoder {
    guard(std::ptr::null_mut(), || {
        let (m, _) = member(&get(encoder)?.layout, index).ok_or("no argument at the index")?;
        let layout = m.layout.clone().ok_or("the argument points to no argument buffer")?;
        Ok(boxed(NmbArgumentEncoder { layout, target: None }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_release(encoder: *mut NmbArgumentEncoder) {
    release(encoder)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_encoded_length(encoder: *const NmbArgumentEncoder) -> u64 {
    guard(0, || Ok(get(encoder)?.layout.encoded_length))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_alignment(encoder: *const NmbArgumentEncoder) -> u64 {
    guard(0, || Ok(get(encoder)?.layout.alignment))
}

/// `setArgumentBuffer:startOffset:arrayElement:`; a null buffer clears it.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_argument_buffer(encoder: *mut NmbArgumentEncoder, buffer: *const NmbBuffer, offset: u64,
                                                           element: u64) -> bool {
    guard(false, || {
        let enc = get_mut(encoder)?;
        enc.target = if buffer.is_null() {
            None
        } else {
            let b = get(buffer)?;
            let contents = b.buffer.contents().ok_or("an argument buffer the host does not reach")?;
            let at = offset + element * enc.layout.encoded_length;
            if at + enc.layout.encoded_length > b.buffer.length() {
                return Err("the argument buffer is too short".into());
            }
            Some((contents.as_ptr(), at))
        };
        Ok(true)
    })
}

fn write_word(encoder: *const NmbArgumentEncoder, index: u32, word: u64) -> Result<bool, String> {
    let enc = get(encoder)?;
    let (base, at) = enc.target.ok_or("no argument buffer is set")?;
    let (_, offset) = member(&enc.layout, index).ok_or("no argument at the index")?;
    // SAFETY: set_argument_buffer checked that the whole encoded length lies in the buffer's mapping.
    unsafe { (base.add((at + offset) as usize) as *mut u64).write_unaligned(word) };
    Ok(true)
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_buffer(encoder: *const NmbArgumentEncoder, buffer: *const NmbBuffer, offset: u64, index: u32) -> bool {
    guard(false, || {
        let word = if buffer.is_null() { 0 } else { get(buffer)?.buffer.gpu_address() + offset };
        write_word(encoder, index, word)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_texture(encoder: *const NmbArgumentEncoder, texture: *const NmbTexture, index: u32) -> bool {
    guard(false, || {
        let word = if texture.is_null() { 0 } else { get(texture)?.texture.gpu_resource_id().to_bits() };
        write_word(encoder, index, word)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_sampler(encoder: *const NmbArgumentEncoder, sampler: *const NmbSampler, index: u32) -> bool {
    guard(false, || {
        let word = if sampler.is_null() { 0 } else { get(sampler)?.state.gpu_resource_id().to_bits() };
        write_word(encoder, index, word)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_render_pipeline(encoder: *const NmbArgumentEncoder, _pipeline: *const NmbRenderPipeline, index: u32) -> bool {
    guard(false, || write_word(encoder, index, 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_set_compute_pipeline(encoder: *const NmbArgumentEncoder, _pipeline: *const NmbComputePipeline, index: u32) -> bool {
    guard(false, || write_word(encoder, index, 0))
}

/// `constantDataAtIndex:`.
#[unsafe(no_mangle)]
pub extern "C" fn nmb_argument_encoder_constant_data(encoder: *const NmbArgumentEncoder, index: u32) -> *mut c_void {
    guard(std::ptr::null_mut(), || {
        let enc = get(encoder)?;
        let (base, at) = enc.target.ok_or("no argument buffer is set")?;
        let (_, offset) = member(&enc.layout, index).ok_or("no argument at the index")?;
        // SAFETY: within the mapping (set_argument_buffer).
        Ok(unsafe { base.add((at + offset) as usize) } as *mut c_void)
    })
}
