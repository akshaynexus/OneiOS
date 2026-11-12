# The battery bridge (README.md).
#   make test       host unit tests (ASan, UBSan): the sysfs parse, the mapping, the change test, a fake sysfs
#   make            build/android/batteryctl (static; the phone test)
include $(dir $(lastword $(MAKEFILE_LIST)))../../host.mk
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
WARN := -Wall -Wextra -Werror
INC := -I. -I../include
SRC := battery.c ../cpu/topology.c
HDR := battery.h ../include/bridges/bridges.h

all: build/android/batteryctl
build/android/batteryctl: batteryctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(ANDROID_CC) -std=gnu11 -O2 -g $(WARN) $(INC) -static batteryctl.c $(SRC) -o $@
build/host/test_battery: test_battery.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined test_battery.c $(SRC) -lpthread -o $@
build/host/batteryctl: batteryctl.c $(SRC) $(HDR)
	@mkdir -p $(dir $@)
	$(CC) -std=gnu11 -O1 -g $(WARN) $(INC) -fsanitize=address,undefined batteryctl.c $(SRC) -lpthread -o $@
test: build/host/test_battery build/host/batteryctl
	build/host/test_battery
clean:
	rm -rf build
.PHONY: all test clean
