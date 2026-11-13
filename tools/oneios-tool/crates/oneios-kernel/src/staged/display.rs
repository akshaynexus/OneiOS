// From build-oneios-display.py (STAGED), written out by a helper of the port and kept by hand since.
/// The probe's lines that change. Each anchor must be there exactly once.
pub const DISPLAY: &[(&str, &str)] = &[
    ("#include \"DroidVMVirtioGpu.h\"\n",
     concat!("#include \"DroidVMVirtioGpu.h\"\n",
             "extern \"C\" bool oneios_display_parent_bound(void);\n",
             "extern \"C\" void oneios_display_parent_slide(void);\n",
             "extern \"C\" void oneios_display_client_seen(IOReturn,int,uint32_t,IOUserClient *);\n",
             "extern \"C\" void oneios_display_frame_seen(unsigned,uint64_t,uint64_t);\n",
             "extern \"C\" void oneios_display_swap_seen(bool,unsigned);\n")),
    // the bindings are for one kernel collection: nothing is registered under another class
    ("    native_legacy_stage=1;void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n",
     concat!("    native_legacy_stage=1;if(!oneios_display_parent_bound()){native_legacy_display_checkpoint();return;}\n",
             "    void *handle=OSMetaClass::preModLoad(g_kernel_kmod_info.name);\n")),
    // the superclass link is the parent's link address until it is slid (OneiOSDisplay.cpp)
    ("native_legacy_display_static_init();native_gop_presenter_static_init();native_legacy_meta_status=OSMetaClass::postModLoad(handle);",
     "native_legacy_display_static_init();native_gop_presenter_static_init();oneios_display_parent_slide();native_legacy_meta_status=OSMetaClass::postModLoad(handle);"),
    // the log: who opened the framebuffer, the first swaps, the first frames
    ("    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n",
     concat!("    native_legacy_client_status=status;native_legacy_client_object=(uintptr_t)client;\n",
             "    oneios_display_client_seen(status,native_legacy_client_pid,native_legacy_client_type,client);\n")),
    ("    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n",
     concat!("    if(kind<2)droidvm_virtio_gpu_boot_dirty();\n",
             "    oneios_display_frame_seen(kind,maxx,maxy);\n")),
    ("    if(accepted)native_legacy_swap_stage(0);\n",
     concat!("    if(accepted)native_legacy_swap_stage(0);\n",
             "    oneios_display_swap_seen(accepted,reason);\n")),
];
