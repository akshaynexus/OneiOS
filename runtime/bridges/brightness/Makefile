# The brightness bridge (README.md).
#   make test       host unit tests (ASan, UBSan): the curve, the fade, the light sensor's events, a fake sysfs
#   make            build/android/brightctl (static; the phone test)
include $(dir $(lastword $(MAKEFILE_LIST)))../../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
WARN := -Wall -Wextra -Werror
INC := -I. -I../include
SRC := brightness.c ../cpu/topology.c
HDR := brightness.h ../include/bridges/bridges.h

all: build/android/brightctl
build/android/brightctl: brightctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(ANDROID_CC) -std=gnu11 -O2 -g $(WARN) $(INC) -static brightctl.c $(SRC) -o $@
build/host/test_brightness: test_brightness.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined test_brightness.c $(SRC) -lpthread -o $@
build/host/brightctl: brightctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined brightctl.c $(SRC) -lpthread -o $@
test: build/host/test_brightness build/host/brightctl
	build/host/test_brightness
clean:
	rm -rf build
.PHONY: all test clean
