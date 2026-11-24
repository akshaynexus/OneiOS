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
    // The Metal plug-in's names are OneiOSAdreno.cpp's (a56-gpu, 2026-11-24): NativeAdrenoMetal and
    // NativeAdrenoMetalDevice as before for the Adreno accelerator; OneiOSRadeonMetal and OneiOSRadeonMetalDevice, and
    // OneiOSDrmContext "amdgpu", for the A56's (oneios-gpu-amd=1 on an AMDGPU capset; docs/a56-gpu.md 5).
    ("extern \"C\" void oneios_gpu_client_list(char *, size_t);\n",
     concat!("extern \"C\" void oneios_gpu_client_list(char *, size_t);\n",
             "extern \"C\" const char *oneios_gpu_metal_plugin(void);\n",
             "extern \"C\" const char *oneios_gpu_metal_class(void);\n",
             "extern \"C\" const char *oneios_gpu_gles_bundle(void);\n",
             "extern \"C\" void oneios_gpu_accelerator_properties(IOService *);\n")),
    ("    accelerator->setProperty(\"MetalPluginName\", \"NativeAdrenoMetal\");\n",
     "    accelerator->setProperty(\"MetalPluginName\", oneios_gpu_metal_plugin());\n"),
    ("    accelerator->setProperty(\"MetalPluginClassName\", \"NativeAdrenoMetalDevice\");\n",
     concat!("    accelerator->setProperty(\"MetalPluginClassName\", oneios_gpu_metal_class());\n",
             "    oneios_gpu_accelerator_properties(accelerator);\n")),
    // OpenGL ES's bundle is OneiOSAdreno.cpp's too (the A56 GPU port's GL stage, 2026-11-24; docs/a56-gpu.md 4a):
    // AppleMetalGLRenderer as before, OneiOSRadeonGL on the A56's accelerator with oneios-gpu-amd-gl=1.
    ("    accelerator->setProperty(\"IOGLESBundleName\", \"AppleMetalGLRenderer\");\n",
     "    accelerator->setProperty(\"IOGLESBundleName\", oneios_gpu_gles_bundle());\n"),
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
             "void oneios_gpu_blob_gone(uint32_t) __attribute__((weak));\n",
             "// OneiOS: who submits, for the host's GPU policy (role, QoS, deadline: OneiOSAdreno.cpp, docs/gpu.md 2.2b).\n",
             "void oneios_gpu_hint(uint8_t *) __attribute__((weak));\n")),
    // The hint at a context's creation (CTX_CREATE's context_init bits 8-23, beside the capset) and at each fenced
    // submission (the SUBMIT_3D header's three padding bytes), on the client's own thread (2026-11-15, fix-gpu-perf).
    ("    CtxCreate create = {{CmdCtxCreate, 0, 0, context, 0, {}}, 0, in.capset_id, {'x', 'n', 'u', ':'}};\n",
     concat!("    CtxCreate create = {{CmdCtxCreate, 0, 0, context, 0, {}}, 0, in.capset_id, {'x', 'n', 'u', ':'}};\n",
             "    if (oneios_gpu_hint) {\n",
             "        uint8_t hint[3] = {};\n",
             "        oneios_gpu_hint(hint);\n",
             "        create.contextInit |= uint32_t(hint[0]) << 8 | uint32_t(hint[1]) << 16;\n",
             "    }\n")),
    ("    Submit submit = {{CmdSubmit3D, uint32_t(FlagFence) | uint32_t(FlagRingIndex), fence, c->context, uint8_t(in.ring_idx), {}}, in.size, 0};\n",
     concat!("    Submit submit = {{CmdSubmit3D, uint32_t(FlagFence) | uint32_t(FlagRingIndex), fence, c->context, uint8_t(in.ring_idx), {}}, in.size, 0};\n",
             "    if (oneios_gpu_hint) oneios_gpu_hint(submit.h.padding);\n")),
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
    // The surface trail (widgets, 2026-11-13; docs/gpu.md 5.1 "Where a surface's pixels go"): each IOSurface use a command
    // buffer names as it commits (surface ID, written or read, the client's slot, its ticket), each use ended (the
    // command buffer completed, or ticket 0: the connection closed), each CPU lock of a surface (IOSurfaceRootUserClient's
    // lock trap, in the locking process), each submission by client, and the guest physical segments of each blob of the
    // caller's own pages (CREATE_BLOB_USER), so OneiOSAdreno.cpp can tell which process's imports share pages and read
    // what the pages hold. Weak, as above.
    ("volatile int droidvm_virtgpu_compositor_pid;\n",
     concat!("volatile int droidvm_virtgpu_compositor_pid;\n",
             "// OneiOS: the surface trail (OneiOSAdreno.cpp trail::).\n",
             "void oneios_gpu_surface_use(uint32_t, uint32_t, uint32_t, uint64_t) __attribute__((weak));\n",
             "void oneios_gpu_surface_done(uint32_t, uint32_t, uint32_t, uint64_t) __attribute__((weak));\n",
             "void oneios_gpu_surface_lock(uint32_t, uint32_t) __attribute__((weak));\n",
             "void oneios_gpu_submit(uint32_t) __attribute__((weak));\n",
             "void oneios_gpu_blob_segments(uint32_t, uint32_t, uint64_t, const void *, uint32_t) __attribute__((weak));\n",
             "bool oneios_gpu_reader_waits(int) __attribute__((weak));\n")),
    // A test switch (oneios-gpu-read-wait=1, off by default): the render server's command buffer waits, as it names the
    // surfaces it samples, for the other processes' GPU work begun before it that writes them (the kernel's own
    // awaitWriters, which this plug-in pin never asks for), at most droidvm_surface_read_wait_ms (100).
    ("    IOLockUnlock(lock);\n    return status;\n}\n}\n}\n// For the display: the holds taken so far",
     concat!("    IOLockUnlock(lock);\n",
             "    if (!status && head.count && oneios_gpu_reader_waits && oneios_gpu_reader_waits(c->pid)) awaitWriters(c, answer);\n",
             "    return status;\n}\n}\n}\n// For the display: the holds taken so far")),
    ("            gSurfaceUses[gSurfaceUseCount++] = {record.surface, record.written != 0, c, answer};\n",
     concat!("            gSurfaceUses[gSurfaceUseCount++] = {record.surface, record.written != 0, c, answer};\n",
             "            if (oneios_gpu_surface_use) oneios_gpu_surface_use(record.surface, record.written != 0, c->slot, answer);\n")),
    ("        if (gSurfaceUses[i].client == c && (!ticket || gSurfaceUses[i].ticket == ticket)) {\n",
     concat!("        if (gSurfaceUses[i].client == c && (!ticket || gSurfaceUses[i].ticket == ticket)) {\n",
             "            if (oneios_gpu_surface_done) oneios_gpu_surface_done(gSurfaceUses[i].surface, gSurfaceUses[i].write, c->slot, ticket);\n")),
    ("    droidvm_surface_lock_options[options & 3] = droidvm_surface_lock_options[options & 3] + 1;\n",
     concat!("    droidvm_surface_lock_options[options & 3] = droidvm_surface_lock_options[options & 3] + 1;\n",
             "    if (oneios_gpu_surface_lock) oneios_gpu_surface_lock(uint32_t(surface), uint32_t(options));\n")),
    ("    count(4);\n",
     concat!("    count(4);\n",
             "    if (oneios_gpu_submit) oneios_gpu_submit(c->slot);\n")),
    // Only the caller's own pages (r.wired), only once the host took the blob (status 0 here is the CreateBlob's answer).
    ("    if (entries) IODeleteData(entries, Entry, entryCapacity);\n",
     concat!("    if (!status && entries && r.wired && oneios_gpu_blob_segments) oneios_gpu_blob_segments(r.id, c->slot, r.size, entries, entryCount);\n",
             "    if (entries) IODeleteData(entries, Entry, entryCapacity);\n")),
    // The AMD path (port-a56, Galaxy A56's Xclipse 540; docs/a56.md): the selftest's context query goes to the
    // amdgpu context type instead of MSM's GET_PARAM, and the reply is read by length instead of by the MSM layout.
    // All of it branches on the context type at run time, so the Adreno path stages byte-identical to before; the
    // host side (OneiOSAdreno.cpp, the virtio backend) still serves MSM only. The response handling below also
    // hardens the MSM path (a wrong-sized reply fails the selftest instead of being read past its end).
    (r#"    CcmdIoctlSimple ="#,
     r#"    ContextMsm = 1, ContextAmdgpu = 2, AmdgpuQueryInfo = 1, AmdgpuInfoAccelWorking = 0,
    CcmdIoctlSimple ="#),
    (r#"void stage(uint64_t"#,
     r#"// AMDGPU_CCMD_QUERY_INFO contains the 32-byte drm_amdgpu_info UAPI structure.
struct AmdgpuQuery {
    uint32_t command, length, seqno, responseOffset;
    uint64_t returnPointer;
    uint32_t returnSize, query, arguments[4];
};
static_assert(sizeof(AmdgpuQuery) == 48, "AMDGPU query wire size");
void stage(uint64_t"#),
    (r#"    uint64_t poolBase ="#,
     r#"    const bool amd = caps.contextType == ContextAmdgpu;
    if (!amd && caps.contextType != ContextMsm) {
        stage(3, kIOReturnUnsupported);
        return;
    }
    uint64_t poolBase ="#),
    (r#"        uint8_t request[16 + 4 + sizeof(DrmMsmParam)] = {};"#,
     r#"        uint8_t request[sizeof(AmdgpuQuery)] = {};
        const uint32_t requestSize = amd ? sizeof(AmdgpuQuery) : 20 + sizeof(DrmMsmParam);
        const uint32_t responseSize = amd ? 12 : 8 + sizeof(DrmMsmParam);"#),
    (r#"uint32_t(sizeof request), 1, 0}, ioctl"#,
     r#"requestSize, 1, 0}, ioctl"#),
    (r#"        memcpy(request, header, sizeof header); memcpy(request + 16, &ioctl, 4); memcpy(request + 20, &param, sizeof param);"#,
     r#"        if (amd) {
            const AmdgpuQuery query = {AmdgpuQueryInfo, requestSize, 1, 0, 0,
                                       sizeof(uint32_t), AmdgpuInfoAccelWorking, {}};
            memcpy(request, &query, sizeof query);
        } else {
            memcpy(request, header, sizeof header);
            memcpy(request + 16, &ioctl, 4);
            memcpy(request + 20, &param, sizeof param);
        }"#),
    (r#"        // The host copies back at most the length the guest wrote at the response (its guard against the guest changing it
        // meanwhile), as vdrm_alloc_rsp does: msm_ccmd_ioctl_simple_rsp, its length and return value and the drm_msm_param.
        *reinterpret_cast<volatile uint32_t *>(reinterpret_cast<volatile uint8_t *>(shmem) + shmem->responseOffset) = 8 + sizeof(DrmMsmParam);
        Submit submit = {{CmdSubmit3D, 0, 0, context, 0, {}}, uint32_t(sizeof request), 0};
        uint32_t ticket = 0;
        status = post(&submit, sizeof submit, request, sizeof request, sizeof reply, false, &ticket);"#,
     r#"        const uint32_t responseOffset = shmem->responseOffset;
        const bool validOffset = responseOffset >= sizeof(Shmem) && !(responseOffset & 3) &&
                                 responseOffset <= ShmemBytes - responseSize;
        volatile uint8_t *response = validOffset ? reinterpret_cast<volatile uint8_t *>(shmem) + responseOffset : nullptr;
        Submit submit = {{CmdSubmit3D, 0, 0, context, 0, {}}, requestSize, 0};
        uint32_t ticket = 0;
        status = kIOReturnBadArgument;
        if (response) {
            // vdrm_alloc_rsp writes the space available to the host before submission.
            *reinterpret_cast<volatile uint32_t *>(response) = responseSize;
            status = post(&submit, sizeof submit, request, requestSize, sizeof reply, false, &ticket);
        }"#),
    (r#"            // msm_ccmd_ioctl_simple_rsp: the length, the ioctl's return value, then the drm_msm_param as the host filled it.
            const volatile uint8_t *response = reinterpret_cast<const volatile uint8_t *>(shmem) + shmem->responseOffset;
            int32_t ret; DrmMsmParam answered;
            memcpy(&ret, const_cast<const uint8_t *>(response) + 4, 4);
            memcpy(&answered, const_cast<const uint8_t *>(response) + 8, sizeof answered);
            droidvm_virtgpu_selftest[5] = uint32_t(ret); droidvm_virtgpu_selftest[6] = answered.value;
            droidvm_virtgpu_selftest[8] = shmem->seqno; droidvm_virtgpu_selftest[9] = shmem->responseOffset;
            for (unsigned i = 0; i < 4; ++i) memcpy(const_cast<uint64_t *>(&droidvm_virtgpu_selftest[10 + i]), const_cast<const uint8_t *>(response) + 8 * i, 8);
            stage(9, 0);
            IOLog("virtgpu-selftest: shmem at pool %llx, seqno %u; GET_PARAM(GPU_ID) ret %d value %llu\n", droidvm_virtgpu_selftest[4],
                  shmem->seqno, ret, answered.value);"#,
     r#"            uint32_t words[8] = {};
            for (unsigned i = 0; i < responseSize / 4; ++i)
                words[i] = reinterpret_cast<const volatile uint32_t *>(response)[i];
            const int32_t ret = int32_t(words[1]);
            uint64_t value = 0;
            if (words[0] == responseSize) {
                if (amd) value = words[2];
                else memcpy(&value, words + 4, sizeof value);
            }
            droidvm_virtgpu_selftest[5] = uint32_t(ret); droidvm_virtgpu_selftest[6] = value;
            droidvm_virtgpu_selftest[8] = shmem->seqno; droidvm_virtgpu_selftest[9] = responseOffset;
            memcpy(const_cast<uint64_t *>(&droidvm_virtgpu_selftest[10]), words, sizeof words);
            stage(9, !ret && words[0] == responseSize && value ? 0 : kIOReturnIOError);
            IOLog("virtgpu-selftest: %s ret %d value %llu response %u seqno %u\n",
                  amd ? "AMDGPU ACCEL_WORKING" : "MSM GPU_ID", ret, value, words[0], shmem->seqno);"#),
    // Each DRM context made (a56-gpu, 2026-11-24): OneiOSAdreno.cpp logs the first 32 on the A56's accelerator
    // ("oneios-gpu-amd: NAME[PID] DRM context N on capset 6 ..."), nothing on the Adreno one. Weak, as above.
    // (in the file's extern "C" block at file scope, after the surface trail's hooks above)
    ("bool oneios_gpu_reader_waits(int) __attribute__((weak));\n",
     concat!("bool oneios_gpu_reader_waits(int) __attribute__((weak));\n",
             "void oneios_gpu_context_made(uint32_t, uint32_t) __attribute__((weak));\n")),
    ("    c->context = context; c->rings = in.num_rings; out.context_id = context; count(1);\n",
     concat!("    c->context = context; c->rings = in.num_rings; out.context_id = context; count(1);\n",
             "    if (oneios_gpu_context_made) oneios_gpu_context_made(context, in.capset_id);\n")),
];

/// NativeAdrenoAccelerator.cpp's lines that change (a56-gpu, 2026-11-24; docs/a56-gpu.md 5): the A56's accelerator
/// (OneiOSAdreno.cpp, oneios-gpu-amd=1 on an AMDGPU capset) refuses every open type but 0x4149 and the plug-in's
/// markers, and hides Metal's and GL's discovery keys from its CPU-composition processes (oneios-gpu-amd-cpu), in
/// copyProperty and in the whole-table read (serializeProperties, IORegistryEntryCreateCFProperties). With the Adreno
/// accelerator each hook answers false and every method does what it did before. Each anchor must be there exactly once.
pub const ACCELERATOR: &[(&str, &str)] = &[
    ("extern \"C\" void native_adreno_note_property_read(const char *);\n",
     concat!("extern \"C\" void native_adreno_note_property_read(const char *);\n",
             "// OneiOS (OneiOSAdreno.cpp): the A56 accelerator's open types and its CPU-composition processes.\n",
             "extern \"C\" bool oneios_gpu_open_refused(uint32_t type);\n",
             "extern \"C\" bool oneios_gpu_discovery_hidden(const char *key);\n",
             "extern \"C\" bool oneios_gpu_discovery_hidden_here(void);\n")),
    ("    OSObject *copyProperty(const char *key) const APPLE_KEXT_OVERRIDE;\n};\n",
     concat!("    OSObject *copyProperty(const char *key) const APPLE_KEXT_OVERRIDE;\n",
             "    bool serializeProperties(OSSerialize *s) const APPLE_KEXT_OVERRIDE;\n",
             "};\n")),
    ("    unsigned limit=40;",
     concat!("    if(oneios_gpu_open_refused(type))return kIOReturnUnsupported;\n",
             "    unsigned limit=40;")),
    ("OSObject *AGXAccelerator::copyProperty(const OSSymbol *key) const\n{\n    native_adreno_note_property_read(key?key->getCStringNoCopy():nullptr);\n",
     concat!("OSObject *AGXAccelerator::copyProperty(const OSSymbol *key) const\n{\n",
             "    native_adreno_note_property_read(key?key->getCStringNoCopy():nullptr);\n",
             "    if(oneios_gpu_discovery_hidden(key?key->getCStringNoCopy():nullptr))return nullptr;\n")),
    ("OSObject *AGXAccelerator::copyProperty(const OSString *key) const\n{\n    native_adreno_note_property_read(key?key->getCStringNoCopy():nullptr);\n",
     concat!("OSObject *AGXAccelerator::copyProperty(const OSString *key) const\n{\n",
             "    native_adreno_note_property_read(key?key->getCStringNoCopy():nullptr);\n",
             "    if(oneios_gpu_discovery_hidden(key?key->getCStringNoCopy():nullptr))return nullptr;\n")),
    ("OSObject *AGXAccelerator::copyProperty(const char *key) const\n{\n    native_adreno_note_property_read(key);\n",
     concat!("OSObject *AGXAccelerator::copyProperty(const char *key) const\n{\n",
             "    native_adreno_note_property_read(key);\n",
             "    if(oneios_gpu_discovery_hidden(key))return nullptr;\n")),
    ("extern \"C\" IOService *native_adreno_accelerator_allocate(void) {return new AGXAccelerator;}\n",
     concat!("// OneiOS: the whole table without the discovery keys the A56 accelerator hides from its CPU-composition processes\n",
             "// (oneios_gpu_discovery_hidden, as copyProperty answers them one by one); for everyone else IOService's, as before.\n",
             "bool AGXAccelerator::serializeProperties(OSSerialize *s) const\n",
             "{\n",
             "    if(!oneios_gpu_discovery_hidden_here())return IOService::serializeProperties(s);\n",
             "    OSDictionary *table=dictionaryWithProperties();\n",
             "    if(!table)return false;\n",
             "    static const char *const keys[]={\"MetalPluginName\",\"MetalPluginClassName\",\"IOGLESBundleName\",\"IOGLESMetalBundleName\",\n",
             "        \"IOGLESDefaultUseMetal\"};\n",
             "    for(const char *key:keys)if(oneios_gpu_discovery_hidden(key))table->removeObject(key);\n",
             "    const bool done=table->serialize(s);\n",
             "    table->release();\n",
             "    return done;\n",
             "}\n",
             "extern \"C\" IOService *native_adreno_accelerator_allocate(void) {return new AGXAccelerator;}\n")),
];
