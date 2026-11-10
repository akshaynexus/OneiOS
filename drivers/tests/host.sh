#!/bin/bash
# Host test of the kernel-side drivers (CI runs every executable */tests/host.sh): each source is
# compiled with the kernel build's own compile records, when a kernel build is on this machine.
# Without one (CI has no XNU tree) it checks what needs no kernel: the protocol header on its own.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
printf '#include "oneios_bridge_proto.h"\n_Static_assert(sizeof(struct ob_header) == 8 && sizeof(struct ob_haptic_vib) == 8 && sizeof(struct ob_hci_state) == 12 && sizeof(struct ob_ctl_hello) == 8, "wire layout");\nint main(void) { return 0; }\n' |
  ${CC:-cc} -std=c11 -Wall -Wextra -Werror -Ibridge -x c - -o /dev/null
echo "drivers: protocol header ok"
if ls ../out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/*/osfmk/DEVELOPMENT/startup.o.json >/dev/null 2>&1; then
  ./build.py --check
else
  echo "drivers: no kernel build records here: compile check skipped"
fi
