#!/bin/bash
# Host test of the kernel-side drivers (CI runs every executable */tests/host.sh): each source is
# compiled with the kernel build's own compile records, when a kernel build is on this machine.
# Without one (CI has no XNU tree) it checks what needs no kernel: the protocol header on its own.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
printf '#include "oneios_bridge_proto.h"\n_Static_assert(sizeof(struct ob_header) == 8 && sizeof(struct ob_haptic_vib) == 8 && sizeof(struct ob_hci_state) == 12 && sizeof(struct ob_ctl_hello) == 8 && sizeof(struct ob_bright_set) == 12 && sizeof(struct ob_bright_state) == 16 && sizeof(struct ob_bright_als) == 16 && sizeof(struct ob_battery_state) == 36, "wire layout");\nint main(void) { return 0; }\n' |
  ${CC:-cc} -std=c11 -Wall -Wextra -Werror -Ibridge -x c - -o /dev/null
echo "drivers: protocol header ok"
${CC:-cc} -std=c11 -Wall -Wextra -Werror brightness/test_als_hid.c -o "${TMPDIR:-/tmp}/oneios-test-als-hid.$$" && "${TMPDIR:-/tmp}/oneios-test-als-hid.$$"; rc=$?; rm -f "${TMPDIR:-/tmp}/oneios-test-als-hid.$$"; [ $rc = 0 ]
${CC:-cc} -std=c11 -Wall -Wextra -Werror brightness/test_bl_request.c -lm -o "${TMPDIR:-/tmp}/oneios-test-bl-request.$$" && "${TMPDIR:-/tmp}/oneios-test-bl-request.$$"; rc=$?; rm -f "${TMPDIR:-/tmp}/oneios-test-bl-request.$$"; [ $rc = 0 ]
${CC:-cc} -std=c11 -Wall -Wextra -Werror -D_DEFAULT_SOURCE haptics/test_actuator.c -lm -o "${TMPDIR:-/tmp}/oneios-test-actuator.$$" && "${TMPDIR:-/tmp}/oneios-test-actuator.$$"; rc=$?; rm -f "${TMPDIR:-/tmp}/oneios-test-actuator.$$"; [ $rc = 0 ]
if ls ../out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/*/osfmk/DEVELOPMENT/startup.o.json >/dev/null 2>&1; then
  ./build.py --check
else
  echo "drivers: no kernel build records here: compile check skipped"
fi
