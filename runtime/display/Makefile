# The display helper (README.md): iOS's frames from xnuhost's session/display.sock on the phone's panel.
#   make            build/android/oneios-display (the launcher), liboneios-display.so (socket, frame, copy),
#                   oneios-display.dex (the SurfaceControl layer, run by app_process) and displayctl (the phone test)
#   make test       unit tests on the build host (protocol, fit, copy), with ASan and UBSan; the choice of the panel's mode on the JVM
# Needs the NDK, a JDK (javac; on a Mac the Homebrew JDK 17, ../host.mk) and d8 from the Android SDK's build-tools.
include $(dir $(lastword $(MAKEFILE_LIST)))../host.mk
SDK ?= $(or $(ANDROID_SDK_ROOT),$(ANDROID_HOME),$(abspath $(NDK)/../..))
ANDROID_CC ?= $(TOOLS)/aarch64-linux-android30-clang
D8 ?= $(or $(lastword $(sort $(wildcard $(SDK)/build-tools/*/d8))),$(shell command -v d8))
WARN := -Wall -Wextra -Werror
CFLAGS ?= -O2 -g
A := build/android
H := build/host

all: $(A)/oneios-display $(A)/liboneios-display.so $(A)/oneios-display.dex $(A)/displayctl

$(A)/oneios-display: src/launcher.c
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) -static src/launcher.c -o $@
# Dynamic: it is loaded into app_process and calls the NDK's native-window API.
$(A)/liboneios-display.so: src/jni.c src/xhd.c src/xhd.h
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) -fPIC -shared -Wl,-soname,liboneios-display.so src/jni.c src/xhd.c -landroid -o $@
$(A)/displayctl: src/displayctl.c src/xhd.c src/xhd.h
	@mkdir -p $(A)
	$(ANDROID_CC) -std=gnu11 $(CFLAGS) $(WARN) -static src/displayctl.c src/xhd.c -o $@
# No android.jar: the source reaches the framework by reflection only.
$(A)/oneios-display.dex: java/OneiOSDisplay.java
	@[ -n "$(D8)" ] || { echo "display: no d8 (Android SDK build-tools) under $(SDK)" >&2; exit 1; }
	@rm -rf $(A)/classes $(A)/dex && mkdir -p $(A)/classes $(A)/dex
	$(JAVAC) --release 11 -Xlint:all -Werror -d $(A)/classes java/OneiOSDisplay.java
	$(D8_ENV) $(D8) --release --min-api 30 --output $(A)/dex $(A)/classes/OneiOSDisplay.class
	cp $(A)/dex/classes.dex $@

$(H)/test_display: tests/test_display.c src/xhd.c src/xhd.h
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) -Isrc -fsanitize=address,undefined tests/test_display.c src/xhd.c -lpthread -o $@
$(H)/displayctl: src/displayctl.c src/xhd.c src/xhd.h
	@mkdir -p $(H)
	$(CC) -std=gnu11 -O1 -g $(WARN) src/displayctl.c src/xhd.c -o $@

# The choice among the panel's modes is plain Java: it runs on the host's JVM.
$(H)/modes/TestModes.class: tests/TestModes.java java/OneiOSDisplay.java
	@mkdir -p $(H)/modes
	$(JAVAC) --release 11 -Xlint:all -Werror -d $(H)/modes java/OneiOSDisplay.java tests/TestModes.java

test: $(H)/test_display $(H)/displayctl $(H)/modes/TestModes.class
	$(H)/test_display $(H)/displayctl
	$(JAVA) -cp $(H)/modes TestModes

clean:
	rm -rf build
.PHONY: all test clean
