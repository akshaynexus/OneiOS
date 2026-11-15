# The video encoder bridge (README.md; docs/hardware.md, "Video encoder").
#   make test       host tests (ASan, UBSan): the stand-in encoder alone (test_standin: its streams' layout and
#                   samples; tests/decode.sh then decodes them with ffmpeg when there is one and compares the
#                   pictures), and the virtio-media device driven over xnuhost's real transport by a guest of plain
#                   memory (test_encoder), through the real oneios-encoder helper process built for the host with the
#                   stand-in encoder (no MediaCodec on the box) and with the stand-in in the device's own process
#   make            build/android/oneios-encoder (the helper; bin/oneios-encoder of the runtime package),
#                   build/android/encctl (the phone test), build/android/liboneencoder.a (xnuhost's end: the device,
#                   the helper backend and the stand-in encoder, static, libc only -- no MediaCodec, which is in the
#                   helper)
include $(dir $(lastword $(MAKEFILE_LIST)))../../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
ANDROID_AR ?= $(TOOLS)/llvm-ar
XNUHOST := ../../xnuhost
WARN := -Wall -Wextra -Werror
INC := -I. -I../include -I$(XNUHOST)/src -I$(XNUHOST)/include
CFLAGS ?= -O2 -g
SAN := -O1 -g -fsanitize=address,undefined
# xnuhost's end (linked into xnuhost, static): the virtio-media device, the helper-process backend, and the stand-in
# encoder for a host with no phone (xnuhost's encoder=sim, and encoder=auto with the bridges off). No MediaCodec.
LINK := media_device.c backend_helper.c backend_stub.c
LIBS := -lmediandk -ldl -lm
HDR := $(wildcard *.h ../include/bridges/*.h $(XNUHOST)/src/xnuhost.h)
A := build/android
H := build/host

all: $(A)/oneios-encoder $(A)/encctl $(A)/liboneencoder.a
$(A)/obj/%.o: %.c $(HDR)
	@mkdir -p $(A)/obj
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -c $< -o $@
$(A)/liboneencoder.a: $(LINK:%.c=$(A)/obj/%.o)
	$(ANDROID_AR) rcs $@.new $^
	mv $@.new $@
# The helper: dynamic (libmediandk is a shared library), with the stand-in backend for ONEIOS_ENCODER_STUB debugging.
$(A)/oneios-encoder: helper.c backend_ndk.c backend_stub.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) helper.c backend_ndk.c backend_stub.c $(LIBS) -o $@
# encctl: the whole path in one process (dynamic), encoding a test pattern through MediaCodec directly (no helper).
$(A)/encctl: encctl.c media_device.c backend_ndk.c $(XNUHOST)/src/virtio.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) encctl.c media_device.c backend_ndk.c $(XNUHOST)/src/virtio.c $(LIBS) -o $@

# On the build host: the helper with the stand-in encoder, and the device test that drives it over the real transport.
$(H)/oneios-encoder: helper.c backend_stub.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) -DONEIOS_ENCODER_STUB_ONLY helper.c backend_stub.c -lpthread -o $@
$(H)/test_encoder: tests/test_encoder.c tests/standin_reader.h media_device.c backend_helper.c backend_stub.c $(XNUHOST)/src/virtio.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) tests/test_encoder.c media_device.c backend_helper.c backend_stub.c $(XNUHOST)/src/virtio.c -lpthread -o $@
$(H)/test_standin: tests/test_standin.c tests/standin_reader.h backend_stub.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) tests/test_standin.c backend_stub.c -o $@
test: $(H)/test_standin $(H)/test_encoder $(H)/oneios-encoder
	rm -rf $(H)/streams && mkdir -p $(H)/streams
	$(H)/test_standin $(H)/streams
	sh tests/decode.sh $(H)/streams
	ONEIOS_ENCODER_STUB=1 $(H)/test_encoder $(H)/oneios-encoder
.PHONY: all test
