# The thermal bridge (README.md).
#   make test       host unit tests (ASan, UBSan): the limits, the rules and their hysteresis, the CPU steps, a fake sysfs,
#                   the worker's listeners
#   make            build/android/thermalctl (static; the phone tool)
include $(dir $(lastword $(MAKEFILE_LIST)))../../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
WARN := -Wall -Wextra -Werror
INC := -I. -I../include
SRC := thermal.c ../cpu/topology.c
HDR := thermal.h ../include/bridges/bridges.h

all: build/android/thermalctl
build/android/thermalctl: thermalctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(ANDROID_CC) -std=gnu11 -O2 -g $(WARN) $(INC) -static thermalctl.c $(SRC) -o $@
build/host/test_thermal: test_thermal.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined test_thermal.c $(SRC) -lpthread -o $@
build/host/thermalctl: thermalctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined thermalctl.c $(SRC) -lpthread -o $@
test: build/host/test_thermal build/host/thermalctl
	build/host/test_thermal
clean:
	rm -rf build
.PHONY: all test clean
