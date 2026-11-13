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

/// DroidVMVirtioGpu3D.cpp's lines that change (hang, 2026-11-13): each blob's life goes to OneiOSAdreno.cpp, which
/// prints by client what kind the live blobs are, how old, and which closed ones the GPU's work in flight still holds
/// (oneios-gpu-kinds:, oneios-gpu-ages:; docs/gpu.md 5.1). The hooks are weak: without OneiOSAdreno.cpp nothing changes.
/// Each anchor must be there exactly once.
pub const GPU3D: &[(&str, &str)] = &[
    ("DroidVMClientMemory droidvm_virtgpu_client_memory[64];\n",
     concat!("DroidVMClientMemory droidvm_virtgpu_client_memory[64];\n",
             "// OneiOS: a blob made (its resource id, client slot, bytes, kind bits: 1 host-visible, 2 the caller's pages wired,\n",
             "// 4 this driver's pages in the caller's task, the blob flags << 4), closed while the GPU may still use it, and gone.\n",
             "void oneios_gpu_blob_made(uint32_t, uint32_t, uint64_t, uint32_t, task_t, uint64_t) __attribute__((weak));\n",
             "void oneios_gpu_blob_closed(uint32_t) __attribute__((weak));\n",
             "void oneios_gpu_blob_gone(uint32_t) __attribute__((weak));\n")),
    ("    c->resources[c->resourceCount++] = r;\n",
     concat!("    c->resources[c->resourceCount++] = r;\n",
             "    if (oneios_gpu_blob_made)\n",
             "        oneios_gpu_blob_made(r.id, c->slot, r.size, (r.hostVisible ? 1u : 0u) | (r.wired ? 2u : 0u) | (r.prepared ? 4u : 0u) |\n",
             "                             ((in.blob_flags & 0xfu) << 4), task, user);\n")),
    ("        c->closed[c->closedCount++] = {*r, c->sequence};\n",
     concat!("        c->closed[c->closedCount++] = {*r, c->sequence};\n",
             "        if (oneios_gpu_blob_closed) oneios_gpu_blob_closed(r->id);\n")),
    ("    countPages(c, r, -1);\n",
     concat!("    countPages(c, r, -1);\n",
             "    if (oneios_gpu_blob_gone) oneios_gpu_blob_gone(r.id);\n")),
];
