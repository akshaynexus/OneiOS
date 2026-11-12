#!/bin/sh
# The stand-in phone's view of a command or script for the phone (stdin to stdout), for fake-adb.sh and the
# stand-in su: every /data/... path becomes $FAKE_PHONE/data/..., and with FAKE_PROC=stand-in every /proc/...
# path becomes $FAKE_PHONE/.fake/proc/..., the stand-in's own /proc (a host without /proc, macOS; or forced on
# Linux). There fake-session.sh keeps PID/cmdline of each stand-in session while it runs, and fake-adb.sh drops
# the entry of a process that is gone, as the kernel does. The text the tool sent is not changed otherwise.
# sed -E: GNU's and BSD's sed read the same alternation.
P=${FAKE_PHONE:?}
c="[ '\"=<>(;]"
if [ "${FAKE_PROC:-host}" = stand-in ]; then
  exec sed -E -e "s#(^|$c)/data/#\\1$P/data/#g" -e "s#(^|$c)/proc/#\\1$P/.fake/proc/#g"
fi
exec sed -E -e "s#(^|$c)/data/#\\1$P/data/#g"
