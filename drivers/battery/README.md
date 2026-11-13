# drivers/battery: iOS's power source

The host side: `runtime/bridges/battery/`. The mapping: `docs/hardware.md` section 7c. No kernel patch: the service
is made by hand from `oneios_drivers_start()`, like the backlight.

**On the phone once (the boot of 2026-11-12 23:45, on the charger, Full at 100 %):** xnuhost said `battery: on: ...
plugged in` and iOS played the "plugged in" sound before the lock screen appeared; a real iPhone that boots on a
charger does not. Why, and what changed: "The charging chime" below. `OneiOSBattery.cpp` compiles and links with the
kernel's own flags (`drivers/build.py --check`, `kernel/build-hosted.sh`); what it publishes and when it tells powerd is
`oneios_battery_props.h`, host-tested by `test_battery_props.c` (`drivers/tests/host.sh`: 21 checks).

| iOS | Here |
|---|---|
| `powerd` matches `IOServiceMatching("IOPMPowerSource")` [F 24A446 disassembly] | `OneiOSBattery`, a subclass of XNU's `IOPMPowerSource`, on a nub under IOResources (the shape of UniXNU-droidvm's `DroidVMBattery.cpp`) |
| `powerd`'s reader needs `BatteryInstalled` = true, `CurrentCapacity`, `MaxCapacity` [F]; it also takes `ExternalConnected`, `ExternalChargeCapable`, `IsCharging`, `FullyCharged`, `AppleRawExternalConnected`, `AtCriticalLevel` | the host's first state is published before `registerService()` with no message; later a host message that changes a value is published, then `updateStatus()` (which messages `kIOPMMessageBatteryStatusHasChanged`); one that changes nothing sets nothing. `AdapterDetails` and `Location`, which it also reads, are not published [I: `DroidVMBattery` leaves them out too] |
| `CycleCount`, `Voltage`, `Amperage`, `InstantAmperage`, `Temperature`, `TimeRemaining`, `DesignCapacity`, `NominalChargeCapacity`, `AppleRawCurrentCapacity`, `AppleRawMaxCapacity` | published; `TimeRemaining` 0xFFFF while unknown |

`AmperageRemaining` is not a key 24A446's `powerd` reads, so it is not published. No `AppleSmartBattery*` object exists here: iOS's
battery health and per-cell paths (`AppleSmartBatteryPack`) find nothing and stay off.

`AtWarnLevel` at or below 10 % and `AtCriticalLevel` at or below 2 %, off power, as `DroidVMBattery`. What iOS does at critical
level under this kernel (a shutdown of the guest) is not seen yet [U].

## Start

`oneios_battery_start()` runs after the backlight in `oneios_drivers_start()` (from `bsd_init`, before launchd). It
needs the bridge's hello with the battery bit, sends `OB_BATTERY_QUERY` and waits up to 1 s for the answer (no answer:
no service, never a default state). Then, in this order: the service is made and attached, the host's first state is
set and copied into the registry **without** `kIOPMMessageBatteryStatusHasChanged` (`publishWithoutMessage()`: the
copy `updateStatus()` makes, without its message), and only then `registerService()`. powerd's first match therefore
reads the real ExternalConnected, IsCharging, FullyCharged and level, and no change is ever reported for the start.
Later host messages arrive on the bridge's worker thread under one lock; `oba_publish()` (`oneios_battery_props.h`)
sets the properties and messages only when a published value differs (an unknown reading keeps the last value).
Boot argument: `oneios_battery=0` (no service). The lines: `ONEIOS BATTERY: first state, no change message: 100%, on
power, full (...)`, `ONEIOS BATTERY: on: IOPMPowerSource published at 100%, on power (ExternalConnected true,
IsCharging false, FullyCharged true before registerService)` or `ONEIOS BATTERY: off: <why>`, then one line per change
of level or state (`ONEIOS BATTERY: 99%, on power, charging (...)`).

## The charging chime

[F 24A446, read with ipsw 3.1.743: `powerd` from the OS image, SpringBoard.framework in the shared cache, 2026-11-13]

- **powerd** matches `IOPMPowerSource` (first match: `Found battery: %d`), adds its interest notification and at once
  reads the registry properties (`IORegistryEntryCreateCFProperties`) and publishes the internal battery: `Power Source
  State` = `AC Power` when `ExternalConnected` is true, `Raw External Connected` from `AppleRawExternalConnected`,
  `Show Charging UI`, and **`Play Charging Chime` = `ExternalConnected`** (or a battery case, or an `AdapterDetails`
  with `IsWireless`), with a `Power Source ID` made from powerd's pid. It posts
  `com.apple.system.powersources.timeremaining` when its state word changes. After a
  `kIOPMMessageBatteryStatusHasChanged` it reads and publishes again.
- **SpringBoard**'s `SBChargingController` keeps `_powerSourceHasChimed` (Power Source ID -> marked).
  `_powerSourceWantsToPlayChime` walks `IOPSCopyPowerSourcesByType`: a source with `Play Charging Chime` true and not
  marked is a reason to chime and is marked; one with it false is unmarked. `playChargingChimeIfAppropriate` always
  asks it (so it marks), logs `Should chime: %u, charging enabled: %u, _powerSourceWantsToPlayChime: %u` and plays
  (an `SBUISound` feedback event) only when the chime is enabled, the restart manager is initialized and its startup
  transition's context has no overlay and is not dark. `suppressChimeForConnectedPowerSources` marks every source
  whose `Play Charging Chime` is true, without a sound. `ACPowerChanged` only wakes the screen.
- **Who calls them:** the handler of `com.apple.system.powersources.timeremaining` (it logs `isChimeDisabled: %u, isOnAC:
  %u, isConnectedToWirelessCharging: %u` first, then plays if appropriate); `setChargingChimeEnabled:` (NO clears the
  marks, YES plays if appropriate); and `-[SpringBoard _finalizeStartupAfterScenesDidConnect:]`: when the startup
  context has an overlay or is dark, **suppress**; otherwise **play if appropriate**. The context's overlay is
  `[BKSDisplayRenderOverlay existingOverlayForDisplay:[CADisplay mainDisplay]]`
  (`-[SBStartupTransitionContext _parseOverlayInfo]`), i.e. backboardd's boot UI (backboardd logs `Current bootUI is
  an Apple Logo`).

So a real iPhone booting on a charger is silent because its boot overlay is up when SpringBoard finalizes its startup:
the battery is marked by the suppress. A chime before the lock screen means one of:

1. [I, the likely one] SpringBoard's startup context had **no boot overlay** (backboardd's boot UI not there when
   SpringBoard made the context: in the 23:45 boot SpringBoard started at +38 s and the first frame came at +48.8 s),
   so the finalize took the play branch with an unmarked, connected battery. The battery service cannot prevent this
   (any connected source chimes there); it is the boot overlay's timing (backboardd and the display path).
2. `Play Charging Chime` turning true after the startup: ExternalConnected false (or missing) first and true later. This
   service does not do that: the host's first state is in the registry before powerd can match the service (the code
   before 2026-11-13 did that too), and the phone's Full on USB maps to plugged in (`runtime/bridges/battery`). Since 2026-11-13 the
   first publish sends no change message and an unchanged host message sets nothing (host-tested).
3. A new Power Source ID after the startup (powerd restarted) or `setChargingChimeEnabled:` NO then YES.

What the next boot's iOS log says (`oneios-tool ioslog show data.img --proc SpringBoard`, `--proc backboardd`,
`--proc powerd`): SpringBoard's `Should chime: 1, charging enabled: 1, _powerSourceWantsToPlayChime: 1` **without** an
`isChimeDisabled: ...` line just before it is case 1: then compare the time of backboardd's
`[com.apple.BackBoard:BootUI] addOverlay(1-init ...): Adding the overlay` with SpringBoard's first lines; **with**
`isChimeDisabled: 0, isOnAC: 1, ...` just before it is case 2 or 3 (powerd's pid, `Found battery`, `Created new power
source id`, and the guest's `ONEIOS BATTERY:` lines at that time in xnuhost's log). powerd's lines may log under `?[pid]`
in `ioslog` (no process name): search the whole log for its pid.

PC boot (qemu-aarch64, 2026-11-13, the kernel of this change, a fake `power_supply` of the phone on its charger
bind-mounted over `/sys/class/power_supply` in a user namespace, `--set bridges=1 --set wifi=off`): the guest logged
`ONEIOS BATTERY: first state, no change message: 100%, on power, full (389/10 C, 4294 mV, 74 mA)` and `ONEIOS BATTERY:
on: IOPMPowerSource published at 100%, on power (ExternalConnected true, IsCharging false, FullyCharged true before
registerService)` 3 s in, about 6 minutes before launchd. backboardd added its boot overlay (`addOverlay(1-init 2):
Adding the overlay`, `Current bootUI is an Apple Logo`) 5 s after SpringBoard's first log line. SpringBoard did not
finalize its startup in the 900 s run (no `Should chime:` line; the PC boot of 2026-11-12 18:29 had none either), so the PC cannot
show the chime; powerd's battery lines were not in the stored log.

## Only a session can show

1. `ONEIOS BATTERY: on` in the guest's log, then that the lock screen shows the phone's percent.
2. The bolt appears on a charger within about 3 s, and leaves on unplugging; a plug after the lock screen chimes once.
3. A boot on the charger is silent (above: the boot overlay decides it, not only this service).
4. That no other `IOPMPowerSource` appears in the registry first: `AppleARMPMUPowerSource` and the others need hardware that is not here.
