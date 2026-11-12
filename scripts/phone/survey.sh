#!/bin/sh
# Read-only survey of the phone's heat, clocks and the session's process (docs/building.md, "Test cycle on the phone").
#   scripts/phone/survey.sh            thermal zones, cpufreq, KGSL clocks, battery, load (One UI or a session)
#   scripts/phone/survey.sh session    the same, then bridgectl cpu, gpuctl nodes, xnuhost's threads (scheduling policy,
#                                      nice), oom_score_adj, cgroup, the devices it holds, and KGSL's limits
# Output on stdout; save it under out/phone-baseline/DATE/. Runs as root through adb; changes nothing (bridgectl cpu
# places only its own thread).
set -eu
{
cat <<'PHONE'
echo "== sample $(date '+%F %T') uptime $(cut -d' ' -f1 /proc/uptime)"
echo "-- thermal (type temp)"
for z in /sys/class/thermal/thermal_zone*; do t=$(cat $z/type 2>/dev/null); v=$(cat $z/temp 2>/dev/null); echo "${z##*/} $t $v"; done
echo "-- cpufreq (cpu cur min max governor)"
for c in /sys/devices/system/cpu/cpu[0-9]*; do f=$c/cpufreq; [ -d $f ] && echo "${c##*/} $(cat $f/scaling_cur_freq) $(cat $f/scaling_min_freq) $(cat $f/scaling_max_freq) $(cat $f/scaling_governor)"; done
echo "-- gpu"
G=/sys/class/kgsl/kgsl-3d0
echo "gpuclk $(cat $G/gpuclk 2>/dev/null) devfreq_cur $(cat $G/devfreq/cur_freq 2>/dev/null) governor $(cat $G/devfreq/governor 2>/dev/null) busy $(cat $G/gpubusy 2>/dev/null) busy_pct $(cat $G/gpu_busy_percentage 2>/dev/null) temp $(cat $G/temp 2>/dev/null) pwrlevel $(cat $G/default_pwrlevel 2>/dev/null)"
echo "-- battery"
B=/sys/class/power_supply/battery
echo "temp $(cat $B/temp 2>/dev/null) capacity $(cat $B/capacity 2>/dev/null) status $(cat $B/status 2>/dev/null) current_now $(cat $B/current_now 2>/dev/null) voltage_now $(cat $B/voltage_now 2>/dev/null)"
echo "-- load $(cat /proc/loadavg)"
PHONE
[ "${1:-}" != session ] || cat <<'PHONE'
R=/data/local/tmp/oneios/runtime/bin
echo "== bridgectl cpu"; $R/bridgectl cpu 2>&1
echo "== gpuctl nodes"; timeout 20 $R/gpuctl nodes 2>&1
echo "== xnuhost threads (policy, nice)"; p=$(cat /data/local/tmp/oneios/session/xnuhost.pid); for t in /proc/$p/task/*; do echo "$(cat $t/comm) $(awk '{print $41, $19}' $t/stat)"; done | sort | uniq -c | sort -rn | head -12
echo "oom_score_adj $(cat /proc/$p/oom_score_adj) cgroup $(cat /proc/$p/cgroup | head -2 | tr '\n' ' ')"
echo "== input devices held by xnuhost"; ls -l /proc/$p/fd 2>/dev/null | grep -o '/dev/[a-z_/0-9-]*' | sort | uniq -c
echo "== kgsl"; G=/sys/class/kgsl/kgsl-3d0; for f in gpuclk max_gpuclk min_clock_mhz max_clock_mhz devfreq/cur_freq devfreq/governor gpu_busy_percentage gpubusy temp thermal_pwrlevel; do echo "$f: $(cat $G/$f 2>/dev/null)"; done
PHONE
} | adb shell "su -c 'sh -s'"
