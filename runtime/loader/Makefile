# The OneiOS loader library (libxnuloader.a), its tests and xnuloader-inspect.
#   make            host build into build/host
#   make test       host unit tests (generated fixtures, no Apple files)
#   make android    cross-build for aarch64-linux-android into build/android
NDK ?= $(HOME)/Android/Sdk/ndk/28.2.13676358
ANDROID_CC ?= $(NDK)/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang
ANDROID_AR ?= $(NDK)/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar
CFLAGS ?= -O2 -g
CFLAGS += -std=c11 -D_POSIX_C_SOURCE=200809L -Wall -Wextra -Werror -Iinclude
SRC := src/macho.c src/adt.c src/boot.c src/machine.c
HDR := $(wildcard include/xnuloader/*.h) tests/fixture.h

all: build/host/libxnuloader.a build/host/xnuloader-inspect

build/host/%.o: src/%.c $(HDR)
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -c $< -o $@
build/host/libxnuloader.a: $(SRC:src/%.c=build/host/%.o)
	$(AR) rcs $@ $^
build/host/xnuloader-inspect: tools/xnuloader-inspect.c build/host/libxnuloader.a
	$(CC) $(CFLAGS) $^ -o $@
build/host/test_loader: tests/test_loader.c build/host/libxnuloader.a $(HDR)
	$(CC) $(CFLAGS) -fsanitize=address,undefined tests/test_loader.c $(SRC) -o $@

test: build/host/test_loader build/host/xnuloader-inspect
	build/host/test_loader
	build/host/xnuloader-inspect board 4 > build/host/board.adt
	build/host/xnuloader-inspect adt build/host/board.adt > /dev/null

build/android/%.o: src/%.c $(HDR)
	@mkdir -p $(@D)
	$(ANDROID_CC) $(CFLAGS) -fPIC -c $< -o $@
build/android/libxnuloader.a: $(SRC:src/%.c=build/android/%.o)
	$(ANDROID_AR) rcs $@ $^
android: build/android/libxnuloader.a

clean:
	rm -rf build
.PHONY: all test android clean
