# Host bridges of OneiOS (docs/hardware.md).
#   make            build/android/libonebridges.a (linked into xnuhost) and build/android/oneios-audio,
#                   build/android/bridgectl (the on-phone test tool)
#   make test       unit tests on the build host (no phone, no Apple files), with ASan and UBSan
#   make test-binder the Bluetooth HAL client against a fake HAL on a real binder driver (a binderfs
#                   mounted in a user namespace: CONFIG_ANDROID_BINDERFS, no root); skipped without it
include $(dir $(lastword $(MAKEFILE_LIST)))../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
ANDROID_AR ?= $(TOOLS)/llvm-ar
XNUHOST := ../xnuhost
WARN := -Wall -Wextra -Werror
INC := -Iinclude -I../../drivers/bridge -I$(XNUHOST)/src -I$(XNUHOST)/include
CFLAGS ?= -O2 -g
CORE := cpu/topology haptics/haptic net/net bt/hci bt/hidl_parcel bt/hci_hidl audio/backend brightness/brightness battery/battery
DEVICES := devices/bridge_dev devices/virtio_net devices/virtio_snd
HDR := $(wildcard include/bridges/*.h bt/*.h audio/*.h brightness/*.h battery/*.h ../../drivers/bridge/*.h $(XNUHOST)/src/xnuhost.h)
A := build/android
H := build/host

all: $(A)/libonebridges.a $(A)/oneios-audio $(A)/bridgectl brightctl batteryctl
brightctl:
	$(MAKE) -C brightness NDK="$(NDK)"
batteryctl:
	$(MAKE) -C battery NDK="$(NDK)"

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

BINDER_TEST := tests/test_hidl_binder.c bt/hci.c bt/hidl_parcel.c bt/hci_hidl.c cpu/topology.c
$(H)/test_hidl_binder: $(BINDER_TEST) $(HDR)
# The AAudio helper against a fake AAudio (tests/fake/aaudio): its ring, drift steering, stop; real time, about 20 s.
$(H)/test_audio_helper: tests/test_audio_helper.c audio/aaudio_helper.c audio/audio_proto.h tests/fake/aaudio/AAudio.h
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) -Itests/fake -Iaudio -fsanitize=address,undefined tests/test_audio_helper.c -ldl -lpthread -lm -o $@

	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined $(BINDER_TEST) -lpthread -o $@
# The binderfs is mounted on build/host/binderfs inside the namespace only; it goes with the namespace.
test-binder: $(H)/test_hidl_binder
	@if ! grep -qw binder /proc/filesystems; then echo "test-binder: skipped (no binderfs in this kernel)"; exit 0; fi; \
	mkdir -p $(H)/binderfs; \
	unshare -Urm sh -c 'mount -t binder binder "$$1" && ASAN_OPTIONS=detect_leaks=0 ONEIOS_HWBINDER="$$1/hwbinder" "$$2"' sh $(H)/binderfs $(H)/test_hidl_binder

test: $(H)/test_bridges $(H)/test_audio_helper test-binder
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_bridges   # the devices live as long as xnuhost: the transport has no destroy
	$(MAKE) -C brightness test
	$(MAKE) -C battery test
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_audio_helper   # the fake streams are kept for the checks

clean:
	rm -rf build
.PHONY: all test test-binder clean brightctl batteryctl
