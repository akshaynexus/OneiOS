# Host bridges of OneiOS (docs/hardware.md).
#   make            build/android/libonebridges.a (linked into xnuhost) and build/android/oneios-audio,
#                   build/android/bridgectl (the on-phone test tool)
#   make test       unit tests on the build host (no phone, no Apple files), with ASan and UBSan
NDK ?= $(HOME)/Android/Sdk/ndk/28.2.13676358
TOOLS := $(NDK)/toolchains/llvm/prebuilt/linux-x86_64/bin
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
ANDROID_AR ?= $(TOOLS)/llvm-ar
XNUHOST := ../xnuhost
WARN := -Wall -Wextra -Werror
INC := -Iinclude -I../../drivers/bridge -I$(XNUHOST)/src -I$(XNUHOST)/include
CFLAGS ?= -O2 -g
CORE := cpu/topology haptics/haptic net/net bt/hci bt/hidl_parcel bt/hci_hidl audio/backend
DEVICES := devices/bridge_dev devices/virtio_net devices/virtio_snd
HDR := $(wildcard include/bridges/*.h bt/*.h audio/*.h ../../drivers/bridge/*.h $(XNUHOST)/src/xnuhost.h)
A := build/android
H := build/host

all: $(A)/libonebridges.a $(A)/oneios-audio $(A)/bridgectl

$(A)/%.o: %.c $(HDR)
	@mkdir -p $(dir $@)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -c $< -o $@
$(A)/libonebridges.a: $(CORE:%=$(A)/%.o) $(DEVICES:%=$(A)/%.o)
	@rm -f $@
	$(ANDROID_AR) rcs $@ $^
# The AAudio helper: dynamic (libaaudio.so is a system library), everything else of it static.
$(A)/oneios-audio: audio/aaudio_helper.c audio/audio_proto.h
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) -Iaudio audio/aaudio_helper.c -laaudio -o $@
$(A)/bridgectl: tools/bridgectl.c $(CORE:%=$(A)/%.o)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -static tools/bridgectl.c $(CORE:%=$(A)/%.o) -lm -o $@

$(H)/test_bridges: tests/test_bridges.c $(CORE:%=%.c) $(DEVICES:%=%.c) $(XNUHOST)/src/virtio.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined tests/test_bridges.c $(CORE:%=%.c) $(DEVICES:%=%.c) $(XNUHOST)/src/virtio.c -lpthread -o $@

test: $(H)/test_bridges
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_bridges   # the devices live as long as xnuhost: the transport has no destroy

clean:
	rm -rf build
.PHONY: all test clean
