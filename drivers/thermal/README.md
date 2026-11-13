# Thermal: the phone's temperatures and pressure level in iOS

`OneiOSThermal.cpp`, started by `oneios_drivers_start()` after the battery. The host end and the levels are in
`runtime/bridges/thermal/README.md`.

## What it does

At start it asks the host (`OB_THERMAL_QUERY` on `OB_CH_THERMAL`), waits up to 1 s for the answer, and publishes
`IOService:/IOResources/oneios-thermal/OneiOSThermal` with:

| Property | Value |
|---|---|
| `ThermalPressureLevel` | iOS's level: 0 nominal, 10 light, 20 moderate, 30 heavy, 40 trapping, 50 sleeping |
| `ThermalPressureLevelName` | its name |
| `ThermalReason` | `case`, `SoC`, `battery` (the readings that set it) |
| `HostStep` | xnuhost's CPU step, 0-3 |
| `Temperatures` | `Case`, `SoC`, `CPU`, `GPU`, `DDR`, `XO`, `Board`, `Battery` in hundredths of a degree C, each only when the phone has it |

Each new state from the host updates them; a new level or step also logs a line and sends
`kIOMessageServicePropertyChange` to the service's clients:

```
ONEIOS THERMAL: on: OneiOSThermal published
ONEIOS THERMAL: heavy (30) by case: case 44.3 C, SoC 61.0 C, battery 36.2 C; the host's CPU step 1
```

Off: `oneios_thermal=0`, or a host that serves no thermal channel (`ONEIOS THERMAL: off: ...`, one line).

## How iOS gets the level

iOS's frameworks read the thermal pressure from the notify state of `com.apple.system.thermalpressurelevel`
(`kOSThermalNotificationPressureLevelName`; the name is in 24A446's libsystem). thermalmonitord, which sets it on an
iPhone, reads Apple's sensors through an SMC and the PMU's HID temperature services (its `V54AP-Info.plist`:
`usesSMCSensorExchange`, sensors `Tarc`, `TN0n`, the camera dies; `TG0B-IOPMPowerSource` for the battery), none of which
exist here: it keeps nominal. The guest helper `oneios-thermal` (`userland/helpers`, service `thermal` of
`oneios-init`) reads `ThermalPressureLevel` every 5 s, sets the notify state and posts it, and sets it again when
something else (thermalmonitord) changed it. thermalmonitord is not disabled.

## Not proven

- Nothing of this has run on the phone or in a PC boot: compiled (`oneios-tool kernel drivers --check`), the host end and the
  helper host-tested.
- The helpers are not yet in any image (`docs/userland.md` section 7): until they are, the level is in the registry
  and the log, and iOS's frameworks still see nominal.
- That ProcessInfo.thermalState, backboardd's frame rate or SpringBoard's "needs to cool down" screen follow the notify
  state alone [inference]. `com.apple.system.thermalstatus` (the older `OSThermalNotificationCurrentLevel`) and the
  per-behaviour `com.apple.system.thermalmitigation.*` levels are not set.
- That thermalmonitord does not set the name every few seconds (the helper's "overwritten" line says so once).
