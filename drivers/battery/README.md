# drivers/battery: iOS's power source

The host side: `runtime/bridges/battery/`. The mapping: `docs/hardware.md` section 7c. No kernel patch: the service
is made by hand from `oneios_drivers_start()`, like the backlight.

**Nothing here has run.** `OneiOSBattery.cpp` compiles and links with the kernel's own flags (`drivers/build.py --check`,
`kernel/build-hosted.sh`).

| iOS | Here |
|---|---|
| `powerd` matches `IOServiceMatching("IOPMPowerSource")` [F 24A446 disassembly] | `OneiOSBattery`, a subclass of XNU's `IOPMPowerSource`, on a nub under IOResources (the shape of UniXNU-droidvm's `DroidVMBattery.cpp`) |
| `powerd`'s reader needs `BatteryInstalled` = true, `CurrentCapacity`, `MaxCapacity` [F]; it also takes `ExternalConnected`, `ExternalChargeCapable`, `IsCharging`, `FullyCharged`, `AppleRawExternalConnected`, `AtCriticalLevel` | published from each host message, then `updateStatus()` (which messages `kIOPMMessageBatteryStatusHasChanged`). `AdapterDetails` and `Location`, which it also reads, are not published [I: `DroidVMBattery` leaves them out too] |
| `CycleCount`, `Voltage`, `Amperage`, `InstantAmperage`, `Temperature`, `TimeRemaining`, `DesignCapacity`, `NominalChargeCapacity`, `AppleRawCurrentCapacity`, `AppleRawMaxCapacity` | published; `TimeRemaining` 0xFFFF while unknown |

`AmperageRemaining` is not a key 24A446's `powerd` reads, so it is not published. No `AppleSmartBattery*` object exists here: iOS's
battery health and per-cell paths (`AppleSmartBatteryPack`) find nothing and stay off.

`AtWarnLevel` at or below 10 % and `AtCriticalLevel` at or below 2 %, off power, as `DroidVMBattery`. What iOS does at critical
level under this kernel (a shutdown of the guest) is not seen yet [U].

## Start

`oneios_battery_start()` runs after the backlight in `oneios_drivers_start()`. It needs the bridge's hello with the
battery bit, sends `OB_BATTERY_QUERY` and waits up to 1 s for the answer, then publishes the service with the host's
state already in it, so `powerd` never sees it empty. Changes arrive on the bridge's worker thread under one lock.
Boot argument: `oneios_battery=0` (no service). One line tells the outcome: `ONEIOS BATTERY: on|off: <why>`, and
one line per change of level or state.

## Only a session can show

1. `ONEIOS BATTERY: on` in the guest's log, then that the lock screen shows the phone's percent.
2. The bolt appears on a charger within about 3 s, and leaves on unplugging.
3. That no other `IOPMPowerSource` appears in the registry first: `AppleARMPMUPowerSource` and the others need hardware that is not here.
