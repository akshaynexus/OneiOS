// From build-oneios-display.py (STAGED), written out by a helper of the port and kept by hand since.
/// The probe's lines that change. Each anchor must be there exactly once.
pub const DISPLAY: &[(&str, &str)] = &[
    ("#include \"DroidVMVirtioGpu.h\"\n",
     concat!("#include \"DroidVMVirtioGpu.h\"\n",
             "extern \"C\" bool oneios_display_parent_bound(void);\n",
             "extern \"C\" void oneios_display_parent_slide(void);\n",
             "extern \"C\" void oneios_display_client_seen(IOReturn,int,uint32_t,IOUserClient *);\n",
             "extern \"C\" void oneios_display_frame_seen(unsigned,uint64_t,uint64_t);\n",
             "extern \"C\" void oneios_display_swap_seen(bool,unsigned,const void *);\n",
             "extern \"C\" bool oneios_display_wide(void);\n",
             "extern \"C\" bool oneios_virtio_gpu_offer_format(void *,IOMemoryDescriptor *,uint64_t,uint32_t,uint32_t,uint32_t);\n")),
    // the bindings are for one kernel collection: nothing is registered under another class
    ("    native_legacy_stage=1;void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n",
     concat!("    native_legacy_stage=1;if(!oneios_display_parent_bound()){native_legacy_display_checkpoint();return;}\n",
             "    void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n")),
    // the superclass link is the parent's link address until it is slid (OneiOSDisplay.cpp)
    ("native_legacy_display_static_init();native_gop_presenter_static_init();native_legacy_meta_status=OSMetaClass::postModLoad(handle);",
     "native_legacy_display_static_init();native_gop_presenter_static_init();oneios_display_parent_slide();native_legacy_meta_status=OSMetaClass::postModLoad(handle);"),
    // the log: who opened the framebuffer, the first swaps (and each surface format the compositor swaps), the first frames
    ("    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n",
     concat!("    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n",
             "    oneios_display_client_seen(status,native_legacy_client_pid,native_legacy_client_type,client);\n")),
    ("    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n",
     concat!("    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n",
             "    oneios_display_frame_seen(kind,maxx,maxy);\n")),
    ("    if(accepted)native_legacy_swap_stage(0);\n",
     concat!("    if(accepted)native_legacy_swap_stage(0);\n",
             "    oneios_display_swap_seen(accepted,reason,surface);\n")),
    // a half-float frame (NGP_RGBA16F_LINEAR, iOS's 'RGhA') goes to the virtio scanout as it is when xnuhost keeps half
    // floats (oneios-display-wide=1, display-format=rgba16f), instead of the CPU's conversion to 8 bits
    ("    offeredToVirtioDisplay=format==1 && droidvm_virtio_gpu_offer(display,memory,stride,width,height);\n",
     concat!("    offeredToVirtioDisplay=(format==1 || (format==2 && oneios_display_wide())) &&\n",
             "        oneios_virtio_gpu_offer_format(display,memory,stride,width,height,format);\n")),
];

/// The virtio scanout client's lines that change (DroidVMVirtioGpuDisplay.cpp): a resource and a frame carry their
/// scanout format, so that a half-float surface is created and scanned out as xnuhost's 'RGhA'
/// (runtime/xnuhost/include/xnuhost/pixels.h, XH_VIRTIO_GPU_FORMAT_RGBA16F) with 8 bytes a pixel, and every other one
/// as before (B8G8R8X8, format 2). Each anchor must be there exactly once.
pub const SCANOUT: &[(&str, &str)] = &[
    ("    uint32_t width, height, id;\n    bool prepared, usable;\n};\n",
     concat!("    uint32_t width, height, id;\n    bool prepared, usable;\n",
             "    uint32_t format;   // OneiOS: 0 B,G,R,X; OneiOSHalf: half floats (oneios-display-wide=1)\n",
             "};\n")),
    ("struct Frame { IOService *display; IOMemoryDescriptor *memory; uint64_t stride; uint32_t width, height; uint64_t work, holds; int pid; };\n",
     concat!("struct Frame { IOService *display; IOMemoryDescriptor *memory; uint64_t stride; uint32_t width, height; uint64_t work, holds; int pid;\n",
             "               uint32_t format; };\n",
             "// OneiOS: xnuhost's scanout format for half floats, IOSurface's 'RGhA' (virtio-gpu has none of its own).\n",
             "constexpr uint32_t OneiOSHalf = 0x52476841;\n",
             "uint32_t scanoutFormat(const Resource &r) { return r.format == OneiOSHalf ? OneiOSHalf : 2; }\n",
             "uint64_t scanoutBpp(const Resource &r) { return r.format == OneiOSHalf ? 8 : 4; }\n")),
    ("r.id, 2, uint32_t(stride / 4), height};\n",
     "r.id, scanoutFormat(r), uint32_t(stride / scanoutBpp(r)), height};\n"),
    (concat!("r.width, r.height, 2, 0, {uint32_t(r.stride)}, {}};\n",
             "            status = DroidVMVirtioGpu::post(&scanout,"),
     concat!("r.width, r.height, scanoutFormat(r), 0, {uint32_t(r.stride)}, {}};\n",
             "            status = DroidVMVirtioGpu::post(&scanout,")),
    (concat!("r.width, r.height, 2, 0, {uint32_t(r.stride)}, {}};\n",
             "            status = send(scanout);\n"),
     concat!("r.width, r.height, scanoutFormat(r), 0, {uint32_t(r.stride)}, {}};\n",
             "            status = send(scanout);\n")),
    ("        r.width == frame.width && r.height == frame.height) { r.used = ++useClock; return &r; }\n",
     "        r.width == frame.width && r.height == frame.height && r.format == frame.format) { r.used = ++useClock; return &r; }\n"),
    ("    IOReturn status = create(*slot, frame.memory, frame.stride, frame.width, frame.height, true);\n",
     concat!("    slot->format = frame.format;\n",
             "    IOReturn status = create(*slot, frame.memory, frame.stride, frame.width, frame.height, true);\n")),
    ("extern \"C\" bool droidvm_virtio_gpu_offer(void *display, IOMemoryDescriptor *memory, uint64_t stride, uint32_t width, uint32_t height) {\n",
     concat!("static bool oneiosOffer(void *display, IOMemoryDescriptor *memory, uint64_t stride, uint32_t width, uint32_t height, uint32_t format);\n",
             "extern \"C\" bool droidvm_virtio_gpu_offer(void *display, IOMemoryDescriptor *memory, uint64_t stride, uint32_t width, uint32_t height) {\n",
             "    return oneiosOffer(display, memory, stride, width, height, 1);\n",
             "}\n",
             "// OneiOS: the offer with the frame's NGP format (1 B,G,R,X; 2 half floats of linear light, LegacyDisplayProbe.cpp).\n",
             "extern \"C\" bool oneios_virtio_gpu_offer_format(void *display, IOMemoryDescriptor *memory, uint64_t stride, uint32_t width,\n",
             "                                                uint32_t height, uint32_t format) {\n",
             "    return (format == 1 || format == 2) && oneiosOffer(display, memory, stride, width, height, format);\n",
             "}\n",
             "static bool oneiosOffer(void *display, IOMemoryDescriptor *memory, uint64_t stride, uint32_t width, uint32_t height, uint32_t format) {\n")),
    ("        pending = {service, memory, stride, width, height, droidvm_virtgpu_submitted(), droidvm_virtgpu_holds(), pid};\n",
     concat!("        pending = {service, memory, stride, width, height, droidvm_virtgpu_submitted(), droidvm_virtgpu_holds(), pid,\n",
             "                   format == 2 ? OneiOSHalf : 0};\n")),
];
