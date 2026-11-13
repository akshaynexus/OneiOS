// From build-oneios-gpu.py (STAGED), written out by a helper of the port and kept by hand since.
/// DroidVMGpu.cpp's lines that change. Each anchor must be there exactly once.
pub const GPU: &[(&str, &str)] = &[
    ("#include \"NativeAdrenoMemory.h\"\n",
     concat!("#include \"NativeAdrenoMemory.h\"\n",
             "extern \"C\" bool oneios_adreno_wanted(void);\n",
             "extern \"C\" bool oneios_adreno_parent_bound(void);\n",
             "extern \"C\" void oneios_adreno_parent_slide(void);\n",
             "extern \"C\" void oneios_gpu_client_list(char *, size_t);\n")),
    // the start is OneiOSAdreno.cpp's decision (the host offers 3D, or oneios-gpu-metal), not two boot arguments
    ("    if (!bootArg(\"vgpu-3d\", 0) || bootArg(\"droidvm-gpu-metal\", 0) != 1) return;\n",
     "    if (!oneios_adreno_wanted()) return;\n"),
    // the bindings are for one kernel collection: nothing is registered under another class
    ("    if (!parent || parent->getClassSize() != 136) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotFound); return;}\n",
     concat!("    if (!parent || parent->getClassSize() != 136) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotFound); return;}\n",
             "    if (!oneios_adreno_parent_bound()) {droidvm_metal_publish[2] = uint32_t(kIOReturnNotPermitted); return;}\n")),
    // On the caller's thread (bsd_init's, in the board's late start), not on one of its own: registering a class holds
    // OSMetaClass's lock from preModLoad to postModLoad, and postModLoad takes the kext lock. A kext load on another
    // thread holds the kext lock and waits for the class lock: with the display's archive loading its graphics
    // family right after this start, the two threads stopped each other (seen: the boot hung at accelerator stage 2).
    (concat!("    thread_t thread = nullptr;\n",
             "    if (kernel_thread_start(publishThread, nullptr, &thread) == KERN_SUCCESS) thread_deallocate(thread);\n"),
     "    publishThread(nullptr, THREAD_AWAKENED);\n"),
    // the superclass link is the parent's link address until it is slid (OneiOSAdreno.cpp)
    (concat!("    native_adreno_accelerator_static_init();\n",
             "    status = OSMetaClass::postModLoad(handle);\n"),
     concat!("    native_adreno_accelerator_static_init();\n",
             "    oneios_adreno_parent_slide();\n",
             "    status = OSMetaClass::postModLoad(handle);\n")),
    // The grant's list is OneiOSAdreno.cpp's oneios_gpu_client_list: droidvm-gpu-clients, or without it the default there
    // ("*", every process, since 2026-11-12). The same list is
    // written for the plug-in as NativeAdrenoMetal.clients at every boot (0039), so the two gates never differ.
    // droidvm-gpu-clients= (empty) is backboardd alone; a list replaces the default; "*" is every process.
    ("    if (PE_parse_boot_argn(\"droidvm-gpu-clients\", clients, sizeof clients - 1)) {\n",
     concat!("    oneios_gpu_client_list(clients, sizeof clients);\n",
             "    {\n")),
];
