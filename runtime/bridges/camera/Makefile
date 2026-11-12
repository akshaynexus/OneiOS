# The camera bridge (README.md; docs/hardware.md section 7).
#   make test       host tests (ASan, UBSan): ring, conversion, PNG, metadata, the core, the helper process, the virtio device
#   make            build/android/oneios-camera (the helper; bin/oneios-camera of the runtime package),
#                   build/android/camctl (the phone test), build/android/libonecamera.a (xnuhost's end: static, libc only)
include $(dir $(lastword $(MAKEFILE_LIST)))../../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
ANDROID_AR ?= $(TOOLS)/llvm-ar
XNUHOST := ../../xnuhost
WARN := -Wall -Wextra -Werror
INC := -I. -I../include -I../../../drivers/camera -I../../../drivers/bridge -I$(XNUHOST)/src -I$(XNUHOST)/include
CFLAGS ?= -O2 -g
# The helper's parts that need no Android library; the back ends that do; xnuhost's end.
HELPER := ring.c convert.c meta.c core.c
ANDROID := backend_ndk.c backend_hal.c
LINK := ring.c link.c virtio_camera.c
LIBS := -lcamera2ndk -lmediandk -lbinder_ndk -lnativewindow -ldl -lm
HDR := $(wildcard *.h ../include/bridges/*.h ../../../drivers/camera/*.h $(XNUHOST)/src/xnuhost.h)
A := build/android
H := build/host
SAN := -O1 -g -fsanitize=address,undefined

all: $(A)/oneios-camera $(A)/camctl $(A)/libonecamera.a
# Dynamic, like oneios-audio: Android's camera, media, binder and window libraries are shared libraries.
$(A)/oneios-camera: helper.c $(HELPER) $(ANDROID) $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) helper.c $(HELPER) $(ANDROID) $(LIBS) -o $@
$(A)/camctl: camctl.c $(HELPER) $(ANDROID) link.c ../cpu/topology.c $(HDR)
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) camctl.c $(HELPER) $(ANDROID) link.c ../cpu/topology.c $(LIBS) -o $@
$(A)/obj/%.o: %.c $(HDR)
	@mkdir -p $(A)/obj
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) $(INC) -c $< -o $@
$(A)/libonecamera.a: $(LINK:%.c=$(A)/obj/%.o)
	$(ANDROID_AR) rcs $@.new $^
	mv $@.new $@

# On the build host the helper and camctl have the test back end only (a moving picture, no camera).
$(H)/oneios-camera: helper.c $(HELPER) $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) helper.c $(HELPER) -lpthread -lm -o $@
$(H)/camctl: camctl.c $(HELPER) link.c ../cpu/topology.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) camctl.c $(HELPER) link.c ../cpu/topology.c -lpthread -lm -o $@
$(H)/test_camera: tests/test_camera.c $(HELPER) $(LINK) $(XNUHOST)/src/virtio.c $(HDR)
	@mkdir -p $(H)
	$(CC) -std=gnu11 $(SAN) $(WARN) $(INC) tests/test_camera.c $(HELPER) link.c virtio_camera.c $(XNUHOST)/src/virtio.c -lpthread -lm -o $@
# detect_leaks=0: the device lives as long as xnuhost (the transport has no destroy).
test: $(H)/test_camera $(H)/oneios-camera $(H)/camctl
	ASAN_OPTIONS=detect_leaks=0 $(H)/test_camera $(H)/oneios-camera
.PHONY: all test
