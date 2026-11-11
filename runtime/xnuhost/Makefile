# xnuhost: XNU as a process on Android's Linux kernel.
#   make            the Android binary: build/android/xnuhost (static, aarch64-linux-android)
#   make test       unit tests on the build host (portable parts; no Apple files), then, when
#                   qemu-aarch64 is installed, the Android binary booting the test kernel under it
#   make check-arm64   only the second half
# The loader library is ../loader.
NDK ?= $(HOME)/Android/Sdk/ndk/28.2.13676358
TOOLS := $(NDK)/toolchains/llvm/prebuilt/linux-x86_64/bin
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
LLD := $(TOOLS)/ld.lld
OBJCOPY := $(TOOLS)/llvm-objcopy
QEMU ?= $(shell command -v qemu-aarch64 2>/dev/null)

WARN := -Wall -Wextra -Werror
INC := -Iinclude -Isrc -Istub -I../loader/include -I../bridges/include -I../../drivers/bridge -I../../drivers/camera
CFLAGS ?= -O2 -g
SRC := main log config insn mem mmio gic sysreg uart blk imageset virtio virtio_blk virtio_input virtio_gpu gpu3d display input cpu hostcalls task
PORTABLE := log config insn mmio gic sysreg uart blk imageset virtio virtio_blk virtio_input virtio_gpu gpu3d cpu task
LOADER_SRC := $(wildcard ../loader/src/*.c)
HDR := $(wildcard include/xnuhost/*.h src/*.h stub/*.h ../loader/include/xnuloader/*.h)
A := build/android
H := build/host

all: $(A)/xnuhost $(A)/gpuctl

# ---- the GPU: Droid-VM's drm2kgsl, linked in (src/gpu3d_kgsl.c) ----
# The renderer and the few virglrenderer and mesa-util sources under it, from external/virglrenderer at the commit
# scripts/setup/get-references.sh pins; not virglrenderer.c (GL, libdrm). Without that checkout xnuhost builds with
# the 2D device only and says so at start.
VIRGL ?= ../../external/virglrenderer
DRM2KGSL_SRC := src/drm/drm2kgsl/drm2kgsl_renderer.c src/drm/drm_context.c src/drm/drm_fence.c src/drm/drm_util.c \
                src/virgl_context.c src/virgl_resource.c src/virgl_util.c src/virgl_fence.c \
                src/gallium/auxiliary/util/u_hash_table.c src/mesa/util/hash_table.c src/mesa/util/ralloc.c \
                src/mesa/util/os_file.c src/mesa/util/anon_file.c src/mesa/util/u_debug.c src/mesa/util/u_math.c \
                src/mesa/util/u_cpu_detect.c
VIRGL_INC := -imacros drm2kgsl/config.h -DHAVE_CONFIG_H=1 -D_GNU_SOURCE -Idrm2kgsl -I$(VIRGL)/src -I$(VIRGL)/src/drm -I$(VIRGL)/src/mesa \
             -I$(VIRGL)/src/mesa/util -I$(VIRGL)/src/mesa/compat -I$(VIRGL)/src/mesa/pipe -I$(VIRGL)/src/gallium/include \
             -I$(VIRGL)/src/gallium/auxiliary -I$(VIRGL)/src/gallium/auxiliary/util
ifneq ($(wildcard $(VIRGL)/src/drm/drm2kgsl/drm2kgsl_renderer.c),)
GPU3D := $(A)/gpu3d_kgsl.o $(A)/libdrm2kgsl.a
GPU3D_DEF := -DXH_DRM2KGSL=1
DRM2KGSL_OBJ := $(patsubst %.c,$(A)/drm2kgsl/%.o,$(notdir $(DRM2KGSL_SRC)))
vpath %.c $(sort $(dir $(addprefix $(VIRGL)/,$(DRM2KGSL_SRC))))
$(A)/drm2kgsl/%.o: %.c drm2kgsl/config.h
	@mkdir -p $(A)/drm2kgsl
	$(ANDROID_CC) -std=gnu11 -O2 -g -w $(VIRGL_INC) -c $< -o $@
$(A)/libdrm2kgsl.a: $(DRM2KGSL_OBJ)
	rm -f $@ && $(TOOLS)/llvm-ar rcs $@ $^
$(A)/gpu3d_kgsl.o: src/gpu3d_kgsl.c src/gpu3d.h drm2kgsl/config.h
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) -Wall -Wextra -Werror -Wno-unused-parameter -Isrc $(VIRGL_INC) -c $< -o $@
endif
$(A)/gpu3d.o: CFLAGS += $(GPU3D_DEF)
# gpu3d.o holds the 2D-only stub of xh_gpu3d_kgsl_open unless XH_DRM2KGSL is set: it must be rebuilt when the
# virglrenderer checkout appears or goes, or a stale stub and gpu3d_kgsl.o both define it (release 0.1.2's first run).
GPU3D_MODE := $(if $(GPU3D_DEF),drm2kgsl,2d)
$(A)/gpu3d.mode: FORCE
	@mkdir -p $(A); echo '$(GPU3D_MODE)' | cmp -s - $@ || echo '$(GPU3D_MODE)' > $@
$(A)/gpu3d.o: $(A)/gpu3d.mode
# gpuctl: the renderer alone, for a check on the phone (tools/gpuctl.c).
$(A)/gpuctl: tools/gpuctl.c $(A)/gpu3d.o $(GPU3D) src/gpu3d.h
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) -Wno-unused-parameter -Isrc -static -o $@ tools/gpuctl.c $(A)/gpu3d.o $(GPU3D) -lm

# ---- the task stub: a flat, position-independent blob ----
STUB_FLAGS := -O2 -ffreestanding -fno-builtin -fno-stack-protector -fpie -fvisibility=hidden -mgeneral-regs-only \
              -fno-jump-tables -mbranch-protection=none -fno-asynchronous-unwind-tables
$(A)/stub.bin: stub/stub.c stub/stub.ld $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) $(WARN) $(INC) $(STUB_FLAGS) -c stub/stub.c -o $(A)/stub.o
	$(LLD) -T stub/stub.ld -static --no-dynamic-linker -o $(A)/stub.elf $(A)/stub.o
	@if $(TOOLS)/llvm-readelf -S $(A)/stub.elf | grep -q '\.got\|\.data\|\.bss'; then echo "stub: needs relocation or data"; exit 1; fi
	$(OBJCOPY) -O binary $(A)/stub.elf $@

# ---- the Android binary ----
$(A)/%.o: src/%.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -c $< -o $@
# C that runs in the kernel's context: no stack protector (its guard lives in bionic's TLS)
$(A)/kctx.o: src/kctx.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -fno-stack-protector -c $< -o $@
$(A)/arch.o: src/arch_aarch64.S $(A)/stub.bin
	$(ANDROID_CC) -DXH_STUB_BIN='"$(A)/stub.bin"' -c $< -o $@
$(A)/loader_%.o: ../loader/src/%.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=c11 -D_POSIX_C_SOURCE=200809L $(CFLAGS) $(WARN) $(INC) -c $< -o $@
# The host bridges (runtime/bridges, the bridges agent's): network, sound, haptics, Bluetooth, CPU placement.
BRIDGES := ../bridges/build/android/libonebridges.a
$(BRIDGES): FORCE
	$(MAKE) -s -C ../bridges NDK=$(NDK) build/android/libonebridges.a
# The camera's end in xnuhost (runtime/bridges/camera, fable-camera): the helper's socket and ring, the oneios,camera device.
CAMERA := ../bridges/camera/build/android/libonecamera.a
$(CAMERA): FORCE
	$(MAKE) -s -C ../bridges/camera NDK=$(NDK) build/android/libonecamera.a
FORCE:
$(A)/xnuhost: $(SRC:%=$(A)/%.o) $(A)/kctx.o $(A)/arch.o $(patsubst ../loader/src/%.c,$(A)/loader_%.o,$(LOADER_SRC)) $(CAMERA) $(BRIDGES) $(GPU3D)
	$(ANDROID_CC) -static -o $@ $^ -lm

# ---- host unit tests ----
$(H)/test_xnuhost: tests/test_xnuhost.c $(PORTABLE:%=src/%.c) $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined tests/test_xnuhost.c $(PORTABLE:%=src/%.c) ../loader/src/adt.c ../loader/src/boot.c -lpthread -o $@
# The image set (imageset.c) on stand-in files; ONEIOS_IMAGE_SET=DIR adds a real set's lists and trust caches.
$(H)/test_imageset: tests/test_imageset.c src/imageset.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined tests/test_imageset.c src/imageset.c ../loader/src/adt.c ../loader/src/boot.c -lpthread -o $@

# ---- the test kernel: our own freestanding AArch64 program wrapped as a Mach-O (no Apple files) ----
KERNEL_FLAGS := --target=aarch64-unknown-none-elf -O2 -ffreestanding -fno-builtin -fno-stack-protector -fpie -fvisibility=hidden \
                -fno-jump-tables -mbranch-protection=none -fno-asynchronous-unwind-tables -ffixed-x18 -nostdlib
$(H)/mkkernel: tests/mkkernel.c ../loader/tests/fixture.h
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 $(WARN) -I../loader/tests tests/mkkernel.c -o $@
$(A)/testkernel.elf: tests/testkernel.c tests/testkernel.ld $(HDR)
	@mkdir -p $(A)
	$(TOOLS)/clang $(KERNEL_FLAGS) $(WARN) $(INC) -c tests/testkernel.c -o $(A)/testkernel.o
	$(LLD) -T tests/testkernel.ld -static -o $@ $(A)/testkernel.o
$(A)/testimage/kernel.kc: $(A)/testkernel.elf $(H)/mkkernel
	@mkdir -p $(A)/testimage $(A)/testdata
	$(H)/mkkernel $(A)/testkernel.elf $@ $(A)/testimage/os.img

check-arm64: $(A)/xnuhost $(A)/testimage/kernel.kc
	@if [ -z "$(QEMU)" ]; then echo "check-arm64: qemu-aarch64 not installed: skipped"; else \
	  rm -f $(A)/testdata/data.img; \
	  echo "check-arm64: booting the test kernel under $(QEMU)"; \
	  timeout 30 $(QEMU) $(A)/xnuhost --mode bench --image $(A)/testimage --data $(A)/testdata --cpus 3 --ram 256M \
	     --display none --no-input --width 64 --height 64 2>&1 | tee $(A)/check.log; \
	  grep -q 'TESTKERNEL: all passed' $(A)/check.log && \
	  echo "check-arm64: a guest that keeps restarting: xnuhost starts again in place 8 times, then exit 3" && \
	  { XNUHOST_RUNNER="$(QEMU)" timeout 60 $(QEMU) $(A)/xnuhost --mode bench --image $(A)/testimage --data $(A)/testdata --cpus 1 --ram 256M \
	     --display none --no-input --width 64 --height 64 --boot-args testkernel-restart > $(A)/restart.log 2>&1; code=$$?; \
	    [ $$code = 3 ] && [ "$$(grep -c 'TESTKERNEL: restart' $(A)/restart.log)" = 9 ] || { tail -5 $(A)/restart.log; echo "check-arm64: restart: exit $$code"; false; }; }; fi

# The tests create devices and never free them: leak detection off, the other checks on.
test: $(H)/test_xnuhost $(H)/test_imageset check-arm64
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_xnuhost
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_imageset

clean:
	rm -rf build
.PHONY: all test check-arm64 clean
