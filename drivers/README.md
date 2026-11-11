# drivers: the kernel side of OneiOS's hardware

Owner: the bridges agent. What each device is, on the phone and for iOS: `docs/hardware.md`.

These sources are built into the kernel (not loaded later). Each talks to its host bridge in xnuhost
(`runtime/bridges/`), never to hardware: One UI's kernel keeps the hardware.

| Folder | What | From |
|---|---|---|
| `bridge/` | `oneios_bridge_proto.h` (the message protocol, shared with xnuhost), `OneiOSBridge.c` (the guest end of the `oneios,bridge` virtio-mmio device), `oneios_bridge.h` (the API the drivers below use) | new; the transport follows UniXNU-droidvm's `DroidVMVirtioNet.c` |
| `haptics/` | `OneiOSVibrator.cpp` (the `vibrator` IOService and the closed-loop node), `oneios_haptic.c` (the Actuator stream node, requests to the host) | iOSPlus10 `2700-laurel-pmi632-haptics.patch`: the iOS side as is, the PMI632 motor replaced by the host |
| `bluetooth/` | bluetoothd's transport: `oneios_bt_ctl.c` (kernel control), `oneios_bt_dev.c` (`/dev` nodes), `oneios_bt_h4.c` (framing, answers), `oneios_bt_ios.c` (core), `oneios_bt.c` (start), `OneiOSBluetooth.cpp` (device-tree nub) | iOSPlus10 `2600-laurel-bluetooth-wcn3990.patch`: the iOS side as is, UART/firmware/power replaced by the host |
| `brightness/` | `OneiOSBacklight.cpp` (the backlight service CoreBrightness drives; the host's light readings), `oneios_als_hid.h` and `OneiOSALSDevice.cpp` (the light sensor as a HID device; built into the hosted kernel's HID archive, `kernel/patches/oneios-hosted/hid`, not into this one) | iOSPlus10 `2201-laurel-backlight-panel-power.patch`, `9005-laurel-backlight-calibration.patch`: the iOS side as is, the DSI panel replaced by the host |
| `battery/` | `OneiOSBattery.cpp` (the `IOPMPowerSource` powerd reads: level, charging, power connected) | new; shaped like UniXNU-droidvm's `DroidVMBattery.cpp`, fed by the host's messages instead of a goldfish device; `battery/README.md` |
| `camera/` | `oneios_camera.c` (the guest end of the `oneios,camera` virtio device: whole NV12 frames), `oneios_camera_dev.c` (`/dev/oneios-camera`), `oneios_camera_abi.h` (what the capture plug-in reads) | new; the node's layout is iOSPlus10's `/dev/negronicam` (HakiOS's, MIT) with finished pictures; `camera/README.md` |
| `cpu/`, `net/`, `audio/`, `gpu/` | notes only: these use UniXNU-droidvm's drivers unchanged, or are not built yet | `README.md` in each |

## Build

```sh
drivers/build.py            # out/drivers/liboneiosdrivers.a + objects.json
drivers/build.py --check    # compile only, a few seconds
```

It needs the kernel build's compile records (`kernel/build-linux.sh` leaves them under
`out/kernel/unixnu/oneios/*/oneios/tree/build/xnu/ios-obj/<CONFIG>/`) and compiles each source with the recorded
command of its universe (Mach, BSD, IOKit) plus `-Wall -Wextra -Werror`. No Apple file is read.

## Into the kernel (drivers -> release-kernel)

1. Link `out/drivers/liboneiosdrivers.a` with `CONFIG_XNU_EXTRA_LD_ARCHIVES` (`-Wl,-force_load,...`).
2. Call `oneios_drivers_start()` once from the board's late probe, after the IOKit bootstrap (where
   `native_source_storage_start()` is called): it starts the bridge transport, haptics and Bluetooth. Each part
   prints one `ONEIOS <PART>: on|off: <why>` line and does nothing when its device or its host is missing.
   Its last call is `oneios_hid_input_start()`: the touch screen, the buttons and the light sensor's HID device,
   which subclass a class of the kernel collection and live in the hosted kernel's own archive
   (`kernel/patches/oneios-hosted/hid`); `OneiOSBridge.c` has an empty weak default for a kernel without it.
3. Symbols it needs from the rest of the board: `armcpu_spi_attach` (ArmCpu). Everything else is XNU's.
4. For the CPU: `droidvm-amp.patch` in the series and `DroidVMCpufreq.c` in the board's devices archive
   (`cpu/README.md`).
5. No KernelConfigTables entry, no patch to XNU's `conf/files`: the vibrator service is made by hand.

Boot arguments: `oneios-bridge=0`, `oneios_haptic=0`, `oneios_haptic_ui=0`, `oneios_vib_cap=0`, `oneios_bt=0`, `oneios_bl=0`,
`oneios_bl_conform=0`, `oneios_battery=0`; `oneios-hid=0` (the HID archive: no touch, buttons or light sensor device).

## Device tree (drivers -> booter)

- `/arm-io/bridge`: `compatible = "oneios,bridge"`, `reg` (one 4 KiB virtio-mmio window), `interrupts` (its SPI),
  `dma-coherent`. Host device: `br_bridge_create()`.
- `/arm-io/cpufreq`, cpu nodes: `cpu/README.md`. `/arm-io/net`, `/arm-io/virtio-snd`: as booter's
  `runtime/loader/src/machine.c` already names them.
- Bluetooth node: `docs/hardware.md` section 4, "Missing".
