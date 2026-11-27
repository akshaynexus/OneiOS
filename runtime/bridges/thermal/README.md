# Thermal bridge

The phone's temperatures for iOS, a thermal pressure level on iOS's scale, and the step by which xnuhost holds itself
back when the phone is hot. Host end: `onebridges::thermal` (`mod.rs`, `load.rs`, `session.rs`; in xnuhost, from the
crate `onebridges`; a Rust port of the C `thermal.c`, 2026-11-23). Guest end:
`drivers/thermal/OneiOSThermal.cpp` (the registry) and `userland/helpers/oneios-thermal.c` (the notify state).
Channel `OB_CH_THERMAL` (6) of `drivers/bridge/oneios_bridge_proto.h`. `docs/hardware.md` section 7d has the phone's
numbers.

## Why

In a session One UI's system_server is gone, and with it Samsung's case-temperature policy (SSRM). What keeps running
[F, the phone, 2026-11-12]:

- the kernel's thermal zones (`step_wise`) with their trips: the X4 at 90 C (`cpu-1-2-1` -> `cpufreq-cpu7`), the GPU at
  95 C (`gpuss-*` -> the KGSL devfreq), DDR at 90 C, the cores pause at 108-110 C, `xo-therm` at 78/80 C, the PMIC's
  BCL levels;
- LMh in hardware, the vendor daemons `thermal-engine-v2` (its `/vendor/etc/thermal-engine.conf` is empty) and
  `vendor.samsung.hardware.thermal-service`;
- the charger's own limits by the case sensor `lrp` (below).

These caps apply to every Linux thread, so xnuhost's vCPUs, iOS's task threads and the GPU follow them (a KGSL power
constraint asks for the top level the thermal limit allows [inference: KGSL's `_adjust_pwrlevel` clamps a constraint to
`thermal_pwrlevel`; not read in Samsung's source]). Nothing acts on the case temperature any more: the junction trips
are 50-60 C above it. This bridge does, inside xnuhost only.

## Sensors (read only)

| Reading | Node | Note |
|---|---|---|
| cpu | the hottest genuine `cpu-*` / `cpuss-*` temperature zone of `/sys/class/thermal` | millidegrees; an off sensor (-273 C) is skipped; `cpu-hw-trip-*` hardware trip pseudo-zones are excluded |
| gpu | the hottest `gpuss-*` zone | |
| ddr, xo | `ddr`, `xo-therm` | |
| soc | the hottest of cpu, gpu, ddr, with the zone's name | |
| ap (board) | `/sys/class/sec/sec-ap-thermistor/temperature` | tenths; Samsung's board thermistor by the SoC |
| battery | `/sys/class/power_supply/battery/temp` | tenths |
| lrp (case) | `/sys/class/power_supply/battery/lrp` | tenths. Samsung's charger limits current by it from 37-39 C with the screen on and acts again at 46 C (the phone's device tree: `battery,temp_table_LRP_NORMAL` 390/380/380/370, `battery,mix_v2_lrp_cond` 460). Taken as the case temperature [inference]; `ap` when there is no `lrp` |

Read every 5 s from xnuhost's start (about 40 files), whether or not iOS listens.

### The two backends of the sensors (hw-backend-thermal, 2026-11-15)

`hw-backend=samsung|treble` or `hw-backend-thermal=...` (`hw/hw_backend.rs`, `docs/hardware.md` 7e);
`thermal::Options.backend` forces one, `Thermal::backend()` says which runs; `hw: thermal: backend X (why)`.
Only the sensors differ: the levels, the hysteresis, xnuhost's step and the load line are the same code for both
(the policy is fix-ram-locked's and not touched here).

- **samsung** (the default): the table above, unchanged.
- **treble**: the thermal HAL `android.hardware.thermal.IThermal/default` by name from servicemanager (`hal/`),
  one `getTemperatures()` (code 3 of the frozen `IThermal.aidl`) per reading. `thermal::from_hal`: cpu and gpu the
  hottest of their `TemperatureType`, soc the hottest of CPU, GPU and SOC (its name in `soc_zone`), battery, and the
  case = the hottest SKIN (the HAL's own skin sensor, where samsung takes Samsung's `lrp`). A type the HAL leaves out
  keeps the files' reading for skin (`lrp`/`ap`) and battery, and the open line says which answered (`(lrp)` or
  `(ap thermistor)` in place of `(SKIN)`). ddr, xo, ap and lrp are unknown. Values below -40 C or above 200 C (an off sensor) are skipped.
- Not registered, or no temperature at the open: `hw: thermal: treble: missing: ...; the bridge stays off`, or the
  files with `hw-backend-fallback=1`. A HAL that stops answering: one line, the readings unknown until it answers
  (each reading then counts as a missing file does).

`thermalctl` (no argument) reads `ONEIOS_HW_BACKEND`, `ONEIOS_HW_BACKEND_THERMAL`, `ONEIOS_HW_FALLBACK` and prints
`backend: ...` and the reading (treble: through the bridge; `case = N (the HAL's SKIN)`). Tests: `tests.rs`
`from_hal_temperatures` (the mapping, the same level as Samsung's lrp gives; the parcel is `hal/`'s tests), `tests/hal_binder.rs` (a reading, a
hotter one, the HAL's death, not registered, no temperature), the stand-in phone (`tools/oneios-tool/tests/phone/hw-backend.sh`).

## Levels

iOS's levels (`kOSThermalPressureLevel*` of the iPhoneOS SDK's `OSThermalNotification.h`): 0 nominal, 10 light,
20 moderate, 30 heavy, 40 trapping, 50 sleeping. The level is the highest of three rules; each rule keeps its level
until its reading falls a hysteresis below the threshold.

| Rule | light | moderate | heavy | trapping | sleeping | hysteresis | Override (degrees C) |
|---|---|---|---|---|---|---|---|
| case (lrp) | 39 | 42 | 44 | 46 | 50 | 2 | `ONEIOS_THERMAL_SKIN=39,42,44,46,50` |
| SoC | | | 95 | 105 | | 5 | `ONEIOS_THERMAL_SOC=95,105` |
| battery | | | 45 | 50 | | 2 | `ONEIOS_THERMAL_BATTERY=45,50` |

The case thresholds are a guess on Samsung's scale above, not Samsung's own policy (that lived in system_server and is
not readable). Apple's meaning of the names [F, `ProcessInfo.ThermalState` documentation]: serious (heavy here) is
"the system reduces performance", critical (trapping and above) is "the device needs to cool down".

### The SoC rule on the phone's own trips (opt-in, 2026-11-22)

Ported from `YumikoHayamato/OneiOS` branch `s25-ultra-sm8750` (876419b, the S25 Ultra's sessions 1-7 on 2026-11-19),
behind two environment variables so the S24 Ultra keeps today's rule (one reading, 95/105 C). The host-device table
sets both on the SM8750 phones (`galaxy-s25-ultra`, `galaxy-s25`; `docs/hardware.md` 7h).

- **The kernel's trips, read on every phone** (files backend; the HAL backend reads none): at the bridge's start each
  SoC zone's (cpu, gpu, ddr) lowest passive trip from 60 to 129.9 C and lowest hot or critical trip from 60 to 149.9 C
  (`trip_point_N_type`/`_temp`; Qualcomm's 135 C passive trips are placeholders), the median of each over the zones.
  One line at the start, on every phone: `thermal: the SoC rule: heavy from 95.0 C, trapping from 105.0 C, on 1 reading;
  the kernel's trips: first passive X C, hot Y C (the median of N SoC zones)`. `thermalctl` prints the same trips.
- **`ONEIOS_THERMAL_SOC_TRIPS=1`**: HEAVY 10 C over the first passive trip (from there the kernel throttles: a SoC at
  its trip is the kernel doing its job), TRAPPING 5 C under the hot trip (15 C over the first passive one without a hot
  trip), never under 95/105 C. On the S25 Ultra (first passive 105 C, hot 125 C): 115 and 120 C. `ONEIOS_THERMAL_SOC=`
  still wins over it.
- **`ONEIOS_THERMAL_SOC_SAMPLES=N`** (1-4, default 1): the SoC rule reads the lowest of the last N readings (N x 5 s),
  so it rises only on heat that lasts and falls at the first reading under its hysteresis; a missing reading keeps the
  window and the level. The level lines then add `; X C the lowest of N` after the SoC's zone.
- **Why**: on the S25 Ultra the hottest of about 30 junction sensors swung 68 -> 104 -> 68 C between two polls, and the
  kernel's first passive trip (105 C) is the S24's TRAPPING threshold: 20 level changes in 8 minutes, 5 into TRAPPING
  (iOS's "too hot" screen), with the case at 37-46 C. With the trips and 3 readings the fork's sessions 3-7 had no SoC
  level; the levels came from the case. `tests.rs` (`trips`) replays that session's 33 SoC readings: 10 or more changes with
  today's rule, none with the S25 Ultra's.
- The SoC rule takes no step (only the case and the cell do), so this changes what iOS is told, not what xnuhost runs.

## xnuhost's step

**One step, at a critical temperature only (fix-ram-locked, 2026-11-15; the owner's rule: all CPU cores, no caps of
ours beyond a true over-temperature guard).**

| When (`thermal::cap_for`) | Step | P vCPUs on (this phone) | E vCPUs on |
|---|---|---|---|
| otherwise: every level up to "trapping" by the case, every SoC temperature, the cell under 50 C | 0 | cpu2-7 (one per core with cpu-pin) | cpu0-1 |
| the case (lrp) at 50 C ("sleeping"), or the cell at 50 C (the battery rule's "trapping"); each held down to its 2 C hysteresis | 2 (`BR_THERMAL_CAP_CRITICAL`) | cpu0-1, 5-6 (the P domain of the lowest top clock, 2.96 GHz, and the E cores) | cpu0-1 |

Until 2026-11-15 the steps began at "heavy": step 1 (the X4 left out) from the case at 44 C, the SoC at 95 C or the
cell at 45 C, step 2 at "trapping" (the case at 46 C, the SoC at 105 C, the cell at 50 C), step 3 (the E cores only) at
"sleeping". Why they went: the heat of the 2026-11-14 and 2026-11-15 sessions came mostly from the host swapping iOS's
RAM (2026-11-15: SoC 59.4 C at the start, 70.6 C at the first swap, 83.8 C later with host sys 200 %, swap in 179 out
2,364 KiB/s, xnuhost 171 %), which the locked RAM removes (`docs/booter.md` 2.2, "Fixed RAM"); and a step at 44 C on
the case took the X4 for the whole of a warm session. What stays, and why:

- **The phone's own throttling** for the SoC, the GPU and DDR: the kernel's thermal zones (the X4 at 90 C, the GPU at
  95 C, DDR at 90 C, the cores pausing at 108-110 C), LMh in hardware, the PMIC's BCL, KGSL's `thermal_pwrlevel`. They
  act on every Linux thread, xnuhost's included, and need nothing of ours: the SoC rule only tells iOS.
- **iOS's own reaction** to the level it is told (its thermal state, as One UI's SSRM reacts on the phone): the levels
  and thresholds below are unchanged.
- **This step**, where nothing else acts in a session (SSRM is gone with system_server; the charger only limits its
  current): the case at 50 C is past what a hand should hold for long [inference], and a cell at 50 C is where charging
  stops [inference]. Step 2, not 3: the E cores alone put eight vCPUs on two cores, the crowding behind the hang of
  2026-11-13 (below).

`Topology::thermal_mask` (`cpu/topology.rs`) works from the topology, not from these numbers, and still knows steps 1 and
3 (`thermalctl` prints steps 0 and 2). xnuhost's `thermal_step` (`runtime/xnuhost/src/session/bridges.rs`) narrows the vCPU threads' affinity and bumps
an epoch (`task::PLACE_EPOCH`), and `thread_run` (`runtime/xnuhost/src/task.rs`) places each iOS thread again at its next run.

CPU placement otherwise (read for fix-ram-locked, on main `1b5fb9f`; nothing of it caps): xnuhost runs one vCPU per
host core (`--cpus` defaults to the host's cores, at most 8; on this phone 2 E and 6 P vCPUs), `cpu-pin` (on) pins each
to its own core and the iOS threads it runs follow it, the virtual cpufreq device only raises a thread's uclamp minimum
when iOS asks for a clock (it never sets a maximum), and nothing writes a cpufreq, devfreq or GPU node. The GPU had no
cap of ours either: drm2kgsl's power constraint and its clock hold (`docs/gpu.md` 2.2) only asked KGSL for the top level
while iOS draws; the 8 MiB GPU pool is the renderer's response buffers, not a limit; KGSL's own thermal limit stays.
Merged into main after that reading (fix-host-overhead #40, fix-gpu-perf #42; left as they are here, the owner's to
decide against the rule "all CPU cores, no caps of ours on the GPU"): decided 2026-11-16, the uclamp cap is gone and
iOS threads the kernel hints as background or utility QoS get no floor and no ceiling of ours (the `uclamp.max` 512 of
fix-host-overhead slowed chronod's widget extensions, #57; `docs/booter.md` 2.6a), and background GPU contexts run
at KGSL's lowest level at a still screen, the render server's at the top only for a frame that would miss its deadline
(`gpu-level=hold` and `gpu-priority=0` turn those off; `docs/gpu.md` 2.2b). Neither touches UI or realtime threads.
Narrowed by fix-gpu-widgets (2026-11-15): the GPU's lowest level is asked only while no other context has GPU work
queued, no other process reads the background context's surfaces and none of its fences has waited 50 ms; background
is only by role or for six daemons that never draw what the user sees (widgets, posters and icons were on the list
until then), never for a context whose surfaces the render server reads; and no GPU context is below the default's
KGSL priority any more (background was 12, now 8).

**Fixed 2026-11-13 (hang):** until then the P vCPUs at step 2 ran on cpu5-6 only (`thermal: step 2 (trapping): P vCPUs
on host CPUs 0x60` on the phone, 2026-11-13 10:4x): the C's `br_cpu_plan_cap` intersected the step's CPUs with the vCPU's own
kind and lost the E cores the step adds, and cpu-pin then put the six P vCPUs three to a core (`rank % 2`), each with
the iOS threads it runs, while the E cores carried only the two E vCPUs. A vCPU preempted on its core while it holds a
kernel lock holds up every vCPU spinning on it; in that session interrupts took up to 953 ms, system calls 639 ms and
faults 399 ms, and the display got 10 frames in 30 s [inference: the step is one of several loads there, with the
keyboard's and Spotlight's CPU, backboardd's GPU memory and the host's low memory; not measured apart]. Now
`CpuPlan::cap` (C: `br_cpu_plan_cap`) keeps the step's CPUs of the other kind (P vCPUs at step 2 on `0x63`, as the table says), and
`CpuPlan::pin_mask` pins a vCPU to one core only while every vCPU of its kind has a core of the mask to itself; with more
vCPUs than cores (steps 1-3: six P vCPUs on five, four or two cores) the vCPUs get the whole mask and Linux spreads them.
The cores each step allows are the same as before, so the heat the step holds back is the same: what changes is that no
core runs three vCPUs while another of the step's cores idles. Step 0 is unchanged (one P vCPU per P core). The log
line names the P vCPUs' mask (`0x63` at step 2 now). Tests: `cargo test -p onebridges thermal::tests::masks` (the plan at steps 2 and 3, the
pin at steps 0-2). Not run on the phone. Only xnuhost's own threads and its tasks' are changed: they end with the session, nothing outside
the process is written, so a SIGKILL leaves nothing to restore. No cpufreq or devfreq node, nothing under `/sys/power`.
`ONEIOS_THERMAL_CAP=0` in xnuhost's environment: the level is still sent to iOS, the step stays 0.

Log lines (xnuhost's log): `thermal: on: 29 zones; case 31.9 C (lrp), SoC 37.4 C (cpu-2-1-1), battery 29.2 C; nominal;
read every 5000 ms`, then `thermal: heavy (was moderate; by case): ...` at each new level and `thermal: step 2 (sleeping): P
vCPUs on host CPUs 0x63` (and `thermal: step 0 (...): ... (all of their kind)` when it lifts) at each new step: since 2026-11-15 only with the case or the cell at 50 C.

**Where the heat comes from (2026-11-14):** every 30 s (`thermal::load::LOAD_MS`), one line:

```
thermal: 30 s: host CPU 405% of 800 (sys 226%, user 167%, irq 12%), swap in 9380 out 7920 KiB/s, xnuhost 152%; battery 40.3 C, Charging 1600 mA; case 41.0 C, SoC 78.0 C (cpu-1-2-1)
```

From named files only, read by the bridge's worker (`HostLoad::read`, `HostLoad::line` in `load.rs`):
`/proc/stat`'s `cpu` line (busy = all ticks less idle and iowait; sys, user + nice, irq + softirq) and its `cpuN`
count, `/proc/vmstat`'s `pswpin`/`pswpout` (pages swapped in and out per second, as KiB: on the phone that is zram
compressing and decompressing, much of it the guest's RAM), `/proc/self/stat`'s utime + stime (xnuhost itself, its
task processes not included), and `power_supply/battery/temp`, `current_now` (as the phone gives it: Samsung's is mA,
positive while charging) and `status`, with the case and SoC readings this bridge already has. Percentages are of one
CPU, as top shows them (all eight busy = 800). A part that cannot be read says `-`. Read only; nothing is written.
How to read it: high `swap` with high `sys` is the host swapping the guest's RAM (the memory fix is another agent's,
`ram=` held fixed); `xnuhost` far below `host CPU` is other processes (the display helper, surfaceflinger, KGSL's
threads, zram's kswapd); `Charging` with a large current while the case rises is the charger's heat on top.
Tests: `cargo test -p onebridges thermal::tests::load` (the line from fake `/proc` and battery files, missing files, the
worker's line at its period). Not run on the phone.

**What the clocks did (2026-11-23):** with the load line, one more (`Clocks::read`, `Clocks::line` in `clocks.rs`):

```
thermal: 30 s clocks: policy0 (cpu0-1) busy 40%, mean 1478 of 2265 MHz, at the top 33%, under half 33%; policy2 (cpu2-4) ...
```

For each cpufreq policy that exists of policy0..policy15 (by name): its CPUs (`related_cpus`), their busy share over the
period (`/proc/stat`'s `cpuN` lines: all ticks less idle and iowait), and its frequency residency from
`policyN/stats/time_in_state` (every moment at a frequency, idle or not): the mean frequency, the top (`cpuinfo_max_freq`,
or the highest listed when a boost frequency is above it), the share at 95% of the top or more and at half or less. A
policy without time counted says so. Why and how to read it: docs/booter.md 2.6e (with One UI's power HAL gone the
governor alone picks the clocks). Tests: `cargo test -p onebridges thermal::tests::clocks` (a fake S24 Ultra's four
policies, the X4's boost frequency, two samples). Not run on the phone.

**Charging.** The line shows the charge current and status; the bridge does not and will not change charging (nothing
is written, nothing under `/sys/power`). Samsung's charger already limits current by the case sensor `lrp` (37-39 C,
46 C). Limiting it further during a session (Samsung's `batt_slate_mode`, `siop_level` or a charge-current limit
node) would be the OneOS fork's job, set at the switch and restored at the return, like its CPU caps; worth asking
for only if the line shows the case still rising with a large `Charging` current while the guest is idle [not
decided; the nodes named are Samsung's usual ones, not read on this phone].

## Messages

`OB_THERMAL_QUERY` (guest, no payload) -> `OB_THERMAL_STATE` (`struct ob_thermal_state`, `proto::bridge::ObThermalState` in Rust: level, reason bits, the step,
eight readings in tenths of a degree), then a new one at once for a new level or step and every 30 s otherwise. A guest
reset stops the reports; the reads go on.

## Tests

`cd runtime && cargo test -p onebridges thermal::` (11 tests, `tests.rs`: every check of the C's `test_thermal.c`; Linux;
on a Mac in the container: `runtime/host-tests.sh`, part `workspace`): the
limits parser, each rule and its hysteresis, the steps' CPUs and pins on the S24 Ultra's topology and on a one-kind host, the
sysfs reader on a fake tree (an off zone, no `lrp`), the base S25's five thermistors (ap, cf, lp, pa, wf; only ap is a
reading, `lrp` wins for the case, an off sensor is missing; NoNameBuilder/OneiOS), the kernel's trips (the S25 Ultra's
zones: a 135 C placeholder, 105, 118 and 125 C hot; the median, the start line with and without `soc_trips`), the trips
rule and the N-reading window (the S25 Ultra's session 2 replayed), the worker's two listeners, and the guest header's
`OB_THERMAL_*` numbers. History of the C test: 158 checks on
2026-11-22 (the test files are written whole and renamed: a truncated `lrp` read as missing made `test_worker` (now `worker`) fail
about 1 run in 6). 99 with the merge of hw-backend (79 on fix-ram-locked's own, 2026-11-15: the step at the case or the cell at 50 C only, none at "heavy" or for
the SoC, its hysteresis, the worker's cap listener at 50 C).

## On the phone

`bin/thermalctl` (static, from `runtime/build.sh STAGE`; read only; as root, One UI up or in a session). The phone runs
quoted in this file were of the C build; the Rust `thermalctl` has not run on a phone yet:

```
thermalctl                  # the zones found, one reading, the level, and each step's CPUs
thermalctl watch -i 10 -n 90
#   a line every 10 s for 15 minutes:
# time cpu gpu ddr xo ap battery lrp (C) level step | MHz cpu0 cpu2 cpu5 cpu7 (cpu7 max) gpu gpu% | battery mA mV | CPU% phone xnuhost session
thermalctl top -i 10        # in a session: xnuhost's busiest threads and the busiest processes of its namespace
```

"CPU% phone" is all eight cores (800 = all busy), "xnuhost" its own threads, "session" every process in xnuhost's mount
namespace (iOS's processes). "battery mA" is `current_now` as the phone gives it: on USB power it is the battery's net
current, not the phone's draw.
