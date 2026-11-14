# Thermal bridge

The phone's temperatures for iOS, a thermal pressure level on iOS's scale, and the step by which xnuhost holds itself
back when the phone is hot. Host end: `thermal.c` (in xnuhost, linked from `libonebridges.a`). Guest end:
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
| cpu | the hottest `cpu-*` / `cpuss-*` zone of `/sys/class/thermal` | millidegrees; a zone that reads -273 C is skipped |
| gpu | the hottest `gpuss-*` zone | |
| ddr, xo | `ddr`, `xo-therm` | |
| soc | the hottest of cpu, gpu, ddr, with the zone's name | |
| ap (board) | `/sys/class/sec/sec-ap-thermistor/temperature` | tenths; Samsung's board thermistor by the SoC |
| battery | `/sys/class/power_supply/battery/temp` | tenths |
| lrp (case) | `/sys/class/power_supply/battery/lrp` | tenths. Samsung's charger limits current by it from 37-39 C with the screen on and acts again at 46 C (the phone's device tree: `battery,temp_table_LRP_NORMAL` 390/380/380/370, `battery,mix_v2_lrp_cond` 460). Taken as the case temperature [inference]; `ap` when there is no `lrp` |

Read every 5 s from xnuhost's start (about 40 files), whether or not iOS listens.

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

## xnuhost's step

| Level | Step | P vCPUs on (this phone) | E vCPUs on |
|---|---|---|---|
| below heavy | 0 | cpu2-7 | cpu0-1 |
| heavy | 1 | cpu2-6 (without the frequency domain of the highest top clock: the X4) | cpu0-1 |
| trapping | 2 | cpu0-1, 5-6 (the P domain of the lowest top clock, 2.96 GHz, and the E cores) | cpu0-1 |
| sleeping | 3 | cpu0-1 | cpu0-1 |

`br_cpu_thermal_mask` (`cpu/topology.c`) works from the topology, not from these numbers. xnuhost's `thermal_step`
(`main.c`) narrows the vCPU threads' affinity and bumps an epoch, and `xh_thread_run` (`task.c`) places each iOS thread
again at its next run.

**Fixed 2026-11-13 (hang):** until then the P vCPUs at step 2 ran on cpu5-6 only (`thermal: step 2 (trapping): P vCPUs
on host CPUs 0x60` on the phone, 2026-11-13 10:4x): `br_cpu_plan_cap` intersected the step's CPUs with the vCPU's own
kind and lost the E cores the step adds, and cpu-pin then put the six P vCPUs three to a core (`rank % 2`), each with
the iOS threads it runs, while the E cores carried only the two E vCPUs. A vCPU preempted on its core while it holds a
kernel lock holds up every vCPU spinning on it; in that session interrupts took up to 953 ms, system calls 639 ms and
faults 399 ms, and the display got 10 frames in 30 s [inference: the step is one of several loads there, with the
keyboard's and Spotlight's CPU, backboardd's GPU memory and the host's low memory; not measured apart]. Now
`br_cpu_plan_cap` keeps the step's CPUs of the other kind (P vCPUs at step 2 on `0x63`, as the table says), and
`br_cpu_pin_mask` pins a vCPU to one core only while every vCPU of its kind has a core of the mask to itself; with more
vCPUs than cores (steps 1-3: six P vCPUs on five, four or two cores) the vCPUs get the whole mask and Linux spreads them.
The cores each step allows are the same as before, so the heat the step holds back is the same: what changes is that no
core runs three vCPUs while another of the step's cores idles. Step 0 is unchanged (one P vCPU per P core). The log
line names the P vCPUs' mask (`0x63` at step 2 now). Tests: `make test` here (64 checks; the plan at steps 2 and 3, the
pin at steps 0-2). Not run on the phone. Only xnuhost's own threads and its tasks' are changed: they end with the session, nothing outside
the process is written, so a SIGKILL leaves nothing to restore. No cpufreq or devfreq node, nothing under `/sys/power`.
`ONEIOS_THERMAL_CAP=0` in xnuhost's environment: the level is still sent to iOS, the step stays 0.

Log lines (xnuhost's log): `thermal: on: 29 zones; case 31.9 C (lrp), SoC 37.4 C (cpu-2-1-1), battery 29.2 C; nominal;
read every 5000 ms`, then `thermal: heavy (was moderate; by case): ...` at each new level and `thermal: step 1 (heavy): P
vCPUs on host CPUs 0x7c` at each new step.

**Where the heat comes from (2026-11-14):** every 30 s (`BR_THERMAL_LOAD_MS`), one line:

```
thermal: 30 s: host CPU 405% of 800 (sys 226%, user 167%, irq 12%), swap in 9380 out 7920 KiB/s, xnuhost 152%; battery 40.3 C, Charging 1600 mA; case 41.0 C, SoC 78.0 C (cpu-1-2-1)
```

From named files only, read by the bridge's worker (`br_host_load_read`, `br_host_load_line` in `thermal.c`):
`/proc/stat`'s `cpu` line (busy = all ticks less idle and iowait; sys, user + nice, irq + softirq) and its `cpuN`
count, `/proc/vmstat`'s `pswpin`/`pswpout` (pages swapped in and out per second, as KiB: on the phone that is zram
compressing and decompressing, much of it the guest's RAM), `/proc/self/stat`'s utime + stime (xnuhost itself, its
task processes not included), and `power_supply/battery/temp`, `current_now` (as the phone gives it: Samsung's is mA,
positive while charging) and `status`, with the case and SoC readings this bridge already has. Percentages are of one
CPU, as top shows them (all eight busy = 800). A part that cannot be read says `-`. Read only; nothing is written.
How to read it: high `swap` with high `sys` is the host swapping the guest's RAM (the memory fix is another agent's,
`ram=` held fixed); `xnuhost` far below `host CPU` is other processes (the display helper, surfaceflinger, KGSL's
threads, zram's kswapd); `Charging` with a large current while the case rises is the charger's heat on top.
Tests: `make test` (72 checks: the line from fake `/proc` and battery files, missing files, a short buffer, the
worker's line at its period). Not run on the phone.

**Charging.** The line shows the charge current and status; the bridge does not and will not change charging (nothing
is written, nothing under `/sys/power`). Samsung's charger already limits current by the case sensor `lrp` (37-39 C,
46 C). Limiting it further during a session (Samsung's `batt_slate_mode`, `siop_level` or a charge-current limit
node) would be the OneOS fork's job, set at the switch and restored at the return, like its CPU caps; worth asking
for only if the line shows the case still rising with a large `Charging` current while the guest is idle [not
decided; the nodes named are Samsung's usual ones, not read on this phone].

## Messages

`OB_THERMAL_QUERY` (guest, no payload) -> `OB_THERMAL_STATE` (`struct ob_thermal_state`: level, reason bits, the step,
eight readings in tenths of a degree), then a new one at once for a new level or step and every 30 s otherwise. A guest
reset stops the reports; the reads go on.

## Tests

`make -C runtime/bridges/thermal test` (Linux; on a Mac in the container: `runtime/host-tests.sh`, part `bridges`): the
limits parser, each rule and its hysteresis, the steps' CPUs and pins on the S24 Ultra's topology and on a one-kind host, the
sysfs reader on a fake tree (an off zone, no `lrp`), the worker's two listeners. 64 checks.

## On the phone

`bin/thermalctl` (static, read only; as root, One UI up or in a session):

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
