/* SPDX-License-Identifier: MIT */
/* The rt bridge's C ABI as the frontend calls it, compiled against NativeMetal's own headers (NativeMetalObjects.h), so the C
 * compiler lays out every struct the Rust side reads: a mismatch shows as a wrong result here, not as a crash on the phone.
 * Run on the build laptop against a real driver (userland/metal/rt/build.sh abi DRIVER: ANV). Each check prints one line; the
 * exit status is the number of checks that failed. */
#include "NativeMetalObjects.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

static int failures;
#define CHECK(cond, ...) do { int ok_ = (cond); printf(ok_ ? "ok   " : "FAIL "); failures += !ok_; printf(__VA_ARGS__); \
    if (!ok_) { printf(" (%s)", nmb_last_error()); } printf("\n"); } while (0)

static void released(void *context, void *pointer, uint64_t length) {
    (void)pointer; (void)length;
    *(int *)context = 1;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: abi_smoke DRIVER [CACHE]\n");
        return 2;
    }
    NmbDevice *device = nmb_device_open(argv[1], argc > 2 ? argv[2] : "/tmp");
    CHECK(device != NULL, "nmb_device_open");
    if (!device) return 1;
    char name[128];
    nmb_device_name(device, name, sizeof name);
    printf("     device %s, maxBufferLength %llu\n", name, (unsigned long long)nmb_device_max_buffer_length(device));

    /* Device answers: every selector the frontend installs. */
    unsigned answered = 0, refused = 0;
    uint32_t kind = 0;
    for (size_t i = 0; nmb_device_answer_selector(i, &kind); i++) {
        uint64_t v = 0;
        const char *sel = nmb_device_answer_selector(i, &kind);
        uint64_t arg = kind == NMB_ANSWER_COUNT_ARG ? 70 /* RGBA8Unorm */ : kind == NMB_ANSWER_FLAG_ARG ? 1 : 0;
        if (nmb_device_answer(device, sel, arg, &v)) answered++; else refused++;
    }
    uint64_t family = 0;
    CHECK(nmb_device_answer(device, "supportsFamily:", 1003, &family) && family == 1, "supportsFamily:Apple3 (%u answered, %u without an answer)",
          answered, refused);

    /* Buffers: a small one (a slab slot), a large one (a block), one with bytes. */
    NmbBuffer *small = nmb_buffer_new(device, 100, 0, NULL);
    NmbBuffer *large = nmb_buffer_new(device, 1 << 20, 0, NULL);
    uint32_t words[4] = {1, 2, 3, 4};
    NmbBuffer *with = nmb_buffer_new(device, sizeof words, 0, words);
    CHECK(small && large && with && nmb_buffer_length(small) == 100 && nmb_buffer_contents(with) &&
          !memcmp(nmb_buffer_contents(with), words, sizeof words) && ((uint8_t *)nmb_buffer_contents(small))[99] == 0,
          "nmb_buffer_new small, large, with bytes (zeroed, copied)");
    CHECK(nmb_buffer_gpu_address(small) && nmb_buffer_gpu_address(small) % 256 == 0, "small buffer GPU address %#llx",
          (unsigned long long)nmb_buffer_gpu_address(small));

    /* No-copy buffer with its deallocator. */
    void *pages = mmap(NULL, 1 << 16, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    int was_released = 0;
    NmbBuffer *nocopy = nmb_buffer_new_no_copy_releasing(device, pages, 1 << 16, 0, released, &was_released);
    CHECK(nocopy && nmb_buffer_contents(nocopy) == pages, "nmb_buffer_new_no_copy_releasing");

    /* Compute through the ABI: the fixture's scale_and_offset. */
    size_t air_length = 0;
    const void *air = nmb_fixture_air(&air_length);
    NmbLibrary *library = nmb_library_new(device, air, air_length, true);
    CHECK(library && nmb_library_function_count(library) > 0, "nmb_library_new (fixture, %u functions)", library ? nmb_library_function_count(library) : 0);
    NmbFunction *kernel = nmb_function_new(library, "scale_and_offset");
    CHECK(kernel && nmb_function_type(kernel) == 3, "nmb_function_new, type kernel");
    NmbComputePipeline *pipeline = nmb_compute_pipeline_new(device, kernel);
    CHECK(pipeline && nmb_compute_pipeline_max_threads(pipeline) > 0, "nmb_compute_pipeline_new (max threads %llu, width %llu)",
          pipeline ? (unsigned long long)nmb_compute_pipeline_max_threads(pipeline) : 0,
          pipeline ? (unsigned long long)nmb_compute_pipeline_execution_width(pipeline) : 0);
    enum { N = 256 };
    float in[N];
    for (int i = 0; i < N; i++) in[i] = (float)i;
    NmbBuffer *input = nmb_buffer_new(device, sizeof in, 0, in);
    NmbBuffer *output = nmb_buffer_new(device, sizeof in, 0, NULL);
    NmbQueue *queue = nmb_queue_new(device);
    NmbCommandBuffer *cb = nmb_command_buffer_new(queue);
    NmbEncoder *enc = nmb_compute_encoder_new(cb);
    float scale = 2.0f;
    uint64_t grid[3] = {N, 1, 1}, group[3] = {64, 1, 1};
    int encoded = enc && nmb_compute_set_pipeline(enc, pipeline) && nmb_encoder_set_buffer(enc, 0, input, 0, 0) &&
                  nmb_encoder_set_buffer(enc, 0, output, 0, 1) && nmb_encoder_set_bytes(enc, 0, &scale, sizeof scale, 2) &&
                  nmb_compute_dispatch(enc, grid, group, true) && nmb_encoder_end(enc);
    CHECK(encoded, "compute encoder: pipeline, buffers, bytes, dispatch, end");
    CHECK(nmb_command_buffer_uses_buffer(cb, input) == 1 && nmb_command_buffer_uses_buffer(cb, large) == 0, "nmb_command_buffer_uses_buffer");
    CHECK(nmb_command_buffer_commit(cb) && nmb_command_buffer_wait(cb, 10000) == 0 && nmb_command_buffer_status(cb) == 4,
          "commit, wait, status completed");
    const float *out = nmb_buffer_contents(output);
    int bad = 0;
    for (int i = 0; i < N; i++) bad += out[i] != 3.0f * i;
    CHECK(bad == 0, "compute results (%d of %d wrong)", bad, N);
    CHECK(nmb_command_buffer_gpu_time(cb, true) >= nmb_command_buffer_gpu_time(cb, false), "GPU times %.6f .. %.6f",
          nmb_command_buffer_gpu_time(cb, false), nmb_command_buffer_gpu_time(cb, true));
    nmb_command_buffer_release(cb);

    /* A render pass through the ABI's descriptors: the fixture's colored quad into a texture, read back with getBytes. */
    NmbFunction *vf = nmb_function_new(library, "colored_vertex"), *ff = nmb_function_new(library, "colored_fragment");
    struct NmbRenderPipelineDescriptor rd;
    memset(&rd, 0, sizeof rd);
    rd.vertex = vf;
    rd.fragment = ff;
    rd.colors[0].format = 70; /* RGBA8Unorm */
    rd.colors[0].write_mask = 15;
    rd.colors[0].source_rgb = 1; rd.colors[0].source_alpha = 1; /* One */
    rd.samples = 1;
    rd.topology = 3; /* triangle */
    rd.rasterization = 1;
    NmbRenderPipeline *rp = nmb_render_pipeline_new(device, &rd);
    CHECK(rp != NULL, "nmb_render_pipeline_new");
    NmbReflection *reflection = nmb_render_pipeline_reflection(&rd);
    CHECK(reflection && strstr(nmb_reflection_json(reflection), "\"vertex\":["), "nmb_render_pipeline_reflection");
    struct NmbTextureDescriptor td;
    memset(&td, 0, sizeof td);
    td.type = 2; td.format = 70; td.width = 8; td.height = 8; td.depth = 1; td.levels = 1; td.samples = 1; td.array_length = 1;
    td.usage = 1 | 4; /* shader read, render target */
    NmbTexture *target = nmb_texture_new(device, &td);
    CHECK(target && nmb_texture_resource_id(target) != 0, "nmb_texture_new (resource id %#llx)", target ? (unsigned long long)nmb_texture_resource_id(target) : 0);
    struct NmbTextureDescriptor back;
    nmb_texture_describe(target, &back);
    CHECK(back.width == 8 && back.format == 70 && back.usage == 5, "nmb_texture_describe");
    float positions[12] = {-1, 1, 0, 1, -1, 0, 0, 1, 0, 0, -1, 0};
    float colors[24];
    for (int i = 0; i < 6; i++) { colors[4 * i] = 1; colors[4 * i + 1] = 0.2f; colors[4 * i + 2] = 0; colors[4 * i + 3] = 1; }
    NmbBuffer *pb = nmb_buffer_new(device, sizeof positions, 0, positions), *colb = nmb_buffer_new(device, sizeof colors, 0, colors);
    struct NmbRenderPassDescriptor pass;
    memset(&pass, 0, sizeof pass);
    pass.colors[0].texture = target;
    pass.colors[0].load = 2; /* clear */
    pass.colors[0].store = 1;
    pass.colors[0].clear[2] = 1; pass.colors[0].clear[3] = 1;
    pass.width = 8; pass.height = 8; pass.array_length = 1; pass.samples = 1;
    cb = nmb_command_buffer_new(queue);
    enc = nmb_render_encoder_new(cb, &pass);
    double viewport[6] = {0, 0, 8, 8, 0, 1};
    encoded = enc && nmb_render_set_pipeline(enc, rp) && nmb_render_set_viewport(enc, viewport) &&
              nmb_encoder_set_buffer(enc, 1, pb, 0, 0) && nmb_encoder_set_buffer(enc, 1, colb, 0, 1) &&
              nmb_render_draw(enc, 3, 0, 6, 1, 0) && nmb_encoder_end(enc);
    CHECK(encoded, "render encoder: pipeline, viewport, vertex buffers, draw, end");
    CHECK(nmb_command_buffer_commit(cb) && nmb_command_buffer_wait(cb, 0) == 0 && nmb_command_buffer_status(cb) == 4, "render committed and completed");
    uint8_t texels[8 * 8 * 4];
    uint64_t region[6] = {0, 0, 0, 8, 8, 1};
    CHECK(nmb_texture_get_bytes(target, texels, 32, 256, sizeof texels, region, 0, 0), "nmb_texture_get_bytes");
    bad = 0;
    for (int y = 0; y < 8; y++)
        for (int x = 0; x < 8; x++) {
            const uint8_t *t = texels + y * 32 + x * 4;
            int inside = x < 4 && y < 4;
            bad += inside ? !(t[0] == 255 && t[1] == 51 && t[2] == 0) : !(t[0] == 0 && t[1] == 0 && t[2] == 255);
        }
    CHECK(bad == 0, "render texels (%d of 64 wrong)", bad);
    nmb_command_buffer_release(cb);

    /* replaceRegion and back. */
    uint8_t pattern[8 * 8 * 4];
    for (size_t i = 0; i < sizeof pattern; i++) pattern[i] = (uint8_t)i;
    CHECK(nmb_texture_replace(target, region, 0, 0, pattern, 32, 256, sizeof pattern) &&
          nmb_texture_get_bytes(target, texels, 32, 256, sizeof texels, region, 0, 0) && !memcmp(pattern, texels, sizeof texels),
          "nmb_texture_replace, read back");

    /* A view, a heap and its buffer and texture, a sampler, a depth/stencil state, an argument encoder, events, a fence. */
    uint64_t levels[2] = {0, 1}, slices[2] = {0, 1};
    NmbTexture *view = nmb_texture_new_view(target, 71 /* RGBA8Unorm_sRGB */, 2, levels, slices, false);
    CHECK(view != NULL, "nmb_texture_new_view");
    NmbHeap *heap = nmb_heap_new(device, 0, 4 << 20, 2 << 4 /* private */);
    NmbBuffer *hb = heap ? nmb_heap_new_buffer(heap, 4096, 2 << 4, UINT64_MAX) : NULL;
    NmbTexture *ht = heap ? nmb_heap_new_texture(heap, &td, UINT64_MAX) : NULL;
    CHECK(heap && hb && ht && nmb_heap_used_size(heap) > 4096, "heap, its buffer and texture (used %llu of %llu)",
          heap ? (unsigned long long)nmb_heap_used_size(heap) : 0, heap ? (unsigned long long)nmb_heap_size(heap) : 0);
    uint64_t sa[2] = {0, 0};
    CHECK(nmb_device_heap_texture_size_and_align(device, &td, sa) && sa[0] % sa[1] == 0, "heapTextureSizeAndAlign %llu, %llu",
          (unsigned long long)sa[0], (unsigned long long)sa[1]);
    struct NmbSamplerDescriptor sd;
    memset(&sd, 0, sizeof sd);
    sd.normalized = 1; sd.max_anisotropy = 1; sd.lod_max = 1000;
    NmbSampler *sampler = nmb_sampler_new(device, &sd);
    CHECK(sampler && nmb_sampler_resource_id(sampler), "nmb_sampler_new");
    struct NmbDepthStencilDescriptor dd;
    memset(&dd, 0, sizeof dd);
    dd.compare = 7; dd.front.compare = 7; dd.back.compare = 7; dd.front.read_mask = dd.back.read_mask = 0xff;
    NmbDepthStencil *ds = nmb_depth_stencil_new(device, &dd);
    CHECK(ds != NULL, "nmb_depth_stencil_new");
    NmbArgumentDescriptor args[2] = {{60 /* pointer */, 0, 0, 0, 2, 0}, {58 /* texture */, 1, 0, 0, 2, 0}};
    NmbArgumentEncoder *ae = nmb_argument_encoder_new(device, args, 2);
    NmbBuffer *ab = ae ? nmb_buffer_new(device, nmb_argument_encoder_encoded_length(ae), 0, NULL) : NULL;
    int argued = ae && ab && nmb_argument_encoder_set_argument_buffer(ae, ab, 0, 0) && nmb_argument_encoder_set_buffer(ae, large, 16, 0) &&
                 nmb_argument_encoder_set_texture(ae, target, 1);
    const uint64_t *aw = ab ? nmb_buffer_contents(ab) : NULL;
    CHECK(argued && aw[0] == nmb_buffer_gpu_address(large) + 16 && aw[1] == nmb_texture_resource_id(target),
          "argument encoder (length %llu): a buffer's address, a texture's resource id", ae ? (unsigned long long)nmb_argument_encoder_encoded_length(ae) : 0);
    NmbEvent *event = nmb_event_new(device, true);
    cb = nmb_command_buffer_new(queue);
    CHECK(event && nmb_command_buffer_signal_event(cb, event, 7) && nmb_command_buffer_commit(cb) && nmb_shared_event_wait(event, 7, 10000) &&
          nmb_shared_event_value(event) == 7, "shared event signaled by a command buffer");
    nmb_command_buffer_wait(cb, 0);
    nmb_command_buffer_release(cb);
    NmbFence *fence = nmb_fence_new(device);
    CHECK(fence != NULL, "nmb_fence_new");

    /* Blit: fill, copy buffer to buffer, texture to buffer. */
    cb = nmb_command_buffer_new(queue);
    enc = nmb_blit_encoder_new(cb);
    uint64_t origin[3] = {0, 0, 0}, size[3] = {8, 8, 1};
    encoded = enc && nmb_blit_fill_buffer(enc, large, 0, 4096, 0x5a) && nmb_blit_copy_buffer(enc, large, 0, small, 0, 64) &&
              nmb_blit_texture_to_buffer(enc, target, 0, 0, origin, size, output, 0, 32, 256, 0) && nmb_encoder_end(enc);
    CHECK(encoded && nmb_command_buffer_commit(cb) && nmb_command_buffer_wait(cb, 0) == 0 &&
          ((uint8_t *)nmb_buffer_contents(small))[63] == 0x5a && !memcmp(nmb_buffer_contents(output), pattern, 256),
          "blit: fill, buffer copy, texture to buffer");
    nmb_command_buffer_release(cb);

    /* Counts the plug-in's telemetry rows read. */
    uint64_t mem[10];
    CHECK(nmb_device_memory_counts(device, mem) && mem[0] > 0, "nmb_device_memory_counts (allocated %llu, placed %llu)", (unsigned long long)mem[0],
          (unsigned long long)mem[1]);

    nmb_fence_release(fence);
    nmb_event_release(event);
    nmb_argument_encoder_release(ae);
    nmb_buffer_release(ab);
    nmb_depth_stencil_release(ds);
    nmb_sampler_release(sampler);
    nmb_texture_release(ht);
    nmb_buffer_release(hb);
    nmb_heap_release(heap);
    nmb_texture_release(view);
    nmb_buffer_release(pb);
    nmb_buffer_release(colb);
    nmb_texture_release(target);
    nmb_reflection_release(reflection);
    nmb_render_pipeline_release(rp);
    nmb_function_release(vf);
    nmb_function_release(ff);
    nmb_compute_pipeline_release(pipeline);
    nmb_function_release(kernel);
    nmb_library_release(library);
    nmb_buffer_release(input);
    nmb_buffer_release(output);
    nmb_buffer_release(nocopy);
    nmb_buffer_release(small);
    nmb_buffer_release(large);
    nmb_buffer_release(with);
    nmb_queue_release(queue);
    nmb_device_release(device);
    CHECK(was_released == 1, "the no-copy buffer's deallocator ran once its import was freed");
    printf("%d failed\n", failures);
    return failures;
}
