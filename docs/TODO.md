# OneiOS to-do

Work that is known and wanted but not on the critical path to a first phone boot. The boot itself and its
blockers live in the board, [agents.md](agents.md), and the status in [../README.md](../README.md). Keep this list
short: an item here has a one-line what and why, and points at the file or doc that describes it.

## Bridges: follow Project Treble / GSI standards where a HAL is reachable

OneiOS runs as a Treble/GSI-style container: the switch stops the Android framework (zygote, system_server, apps)
but leaves the vendor HALs and `surfaceflinger`/`audioserver`/`cameraserver` running, and iOS's bridges become
their clients ([hardware.md](hardware.md)). Today the bridges are a mix:

- **Through the vendor HAL (Treble-standard), keep as is:** Bluetooth (HIDL `android.hardware.bluetooth@1.1`
  over hwbinder), camera (provider HAL + Camera2 NDK), GPU (KGSL), display (surfaceflinger).
- **Direct to a kernel node (below the HAL), because the owning HAL lives in the dead framework:** haptics (evdev
  FF), backlight (`/sys/class/backlight`), light sensor (evdev `als_rear`), touch/buttons (evdev), network
  (`/dev/tun`).

**To do:** audit each direct-node bridge and move it onto a vendor HAL that stays up in a session where one exists,
so the path is Treble-correct and portable to other devices. Known concrete case first:

- [ ] **Light sensor through the sensors HAL.** `als_rear` read by evdev returns 0 lux on the S24 (the rear ALS
  data goes through the sensor hub / SSC, not that evdev node; the front ALS is hub-only). The correct path is a
  direct client of the vendor sensors HAL (AIDL `android.hardware.sensors`), which keeps running when
  system_server is gone. See [hardware.md](hardware.md) section 7a and the `als_rear` facts. This is also what
  auto-brightness needs to reach CoreBrightness.
- [ ] Audit haptics, backlight, touch and network the same way: is there a vendor HAL that stays up in a session,
  and is the HAL or the kernel node the right surface for each? Record the verdict per bridge in `hardware.md`.
- [ ] Where a bridge must stay on a kernel node (no HAL survives the framework), note why in `hardware.md`, so the
      choice is deliberate and documented, not accidental.

**Why:** a HAL is the device-independent, vendor-maintained surface; a raw `/dev`/`/sys` node is device-specific
and can be fed by a different subsystem on another SoC (the ALS-vs-sensor-hub case). Using the HAL where one is
reachable makes OneiOS correct here and portable later.
