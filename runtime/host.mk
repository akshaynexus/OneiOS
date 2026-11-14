# The build host, for every Makefile under runtime/ (each one includes this file first).
#   Linux x86_64: the NDK's linux-x86_64 tools, NDK default $(HOME)/Android/Sdk/ndk/28.2.13676358
#   macOS:        the NDK's darwin-x86_64 tools (universal binaries: they run natively on Apple Silicon),
#                 NDK default $(HOME)/Library/Android/sdk/ndk/28.2.13676358, javac and java from the Homebrew JDK 17
#                 (/opt/homebrew/opt/openjdk@17) when it is there
# NDK=, NDK_HOST=, JDK= override. The host tests (make test, test-binder, check-arm64) are Linux tests: on a Mac
# runtime/host-tests.sh runs them in an arm64 Linux container (docs/building.md section 4).
HOST_OS := $(shell uname -s)
HOST_ARCH := $(shell uname -m)
NDK_REVISION := 28.2.13676358
ifeq ($(HOST_OS),Darwin)
NDK ?= $(HOME)/Library/Android/sdk/ndk/$(NDK_REVISION)
NDK_HOST ?= darwin-x86_64
JDK ?= $(wildcard /opt/homebrew/opt/openjdk@17)
else
NDK ?= $(HOME)/Android/Sdk/ndk/$(NDK_REVISION)
NDK_HOST ?= linux-x86_64
endif
TOOLS := $(NDK)/toolchains/llvm/prebuilt/$(NDK_HOST)/bin
# Every Android shared object of the runtime links with $(ANDROID_SO_LDFLAGS) on its `-shared` line. A shared object
# links with symbols left undefined, and Android's linker then refuses it at load time (liboneios-display.so of
# 1f7dd1f: `dlopen failed: cannot locate symbol "ANativeWindow_getBuffersDataSpace"`, the phone, 2026-11-14).
# -z defs makes a symbol that no library named on the link line has a link error on the build machine. Executables
# (static, and dynamic like oneios-audio and oneios-camera) need no flag: lld fails on an undefined symbol in an
# executable by default. runtime/package/elf-check-test.sh tests all of this, and that every `-shared` line in a
# runtime Makefile has the variable; runtime/package/elf-check.sh checks the built files against the NDK's stubs.
ANDROID_SO_LDFLAGS := -Wl,-z,defs
# JDK empty: javac, java and d8's java from PATH (as before on Linux)
JAVAC ?= $(if $(JDK),$(JDK)/bin/javac,javac)
JAVA ?= $(if $(JDK),$(JDK)/bin/java,java)
D8_ENV = $(if $(JDK),JAVA_HOME=$(JDK) PATH="$(JDK)/bin:$$PATH")

ifeq ($(HOST_OS),Darwin)
ifneq ($(filter test test-binder check-arm64,$(MAKECMDGOALS)),)
$(error host tests are Linux tests (binderfs, udmabuf, user namespaces, LeakSanitizer, the arm64 binary): on a Mac run runtime/host-tests.sh)
endif
endif
