#!/bin/sh
# The stand-in phone's view of a command or script for the phone (stdin to stdout), for fake-adb.sh and the
# stand-in su: every /data/... path becomes $FAKE_PHONE/data/..., and with FAKE_PROC=stand-in every /proc/...
# path becomes $FAKE_PHONE/.fake/proc/..., the stand-in's own /proc (a host without /proc, macOS; or forced on
# Linux). There fake-session.sh keeps PID/cmdline of each stand-in session while it runs, and fake-adb.sh drops
# the entry of a process that is gone, as the kernel does (fake-display.sh, the stand-in display helper, keeps its
# entry there too). The text the tool sent is not changed otherwise.
# sed -E: GNU's and BSD's sed read the same alternation.
# Every /sys/... path becomes $FAKE_PHONE/sys/... in both modes: the stand-in's own sysfs, which holds only the
# nodes a test made (tests/phone/session-cmds.sh: tsp/enabled, the panel's backlight, the session's cgroup); a
# node that is not there is missing, as on a phone without it. The host's /sys is never read or written.
P=${FAKE_PHONE:?}
c="[ '\"=<>(;]"
if [ "${FAKE_PROC:-host}" = stand-in ]; then
  exec sed -E -e "s#(^|$c)/data/#\\1$P/data/#g" -e "s#(^|$c)/sys/#\\1$P/sys/#g" -e "s#(^|$c)/proc/#\\1$P/.fake/proc/#g"
fi
exec sed -E -e "s#(^|$c)/data/#\\1$P/data/#g" -e "s#(^|$c)/sys/#\\1$P/sys/#g"
