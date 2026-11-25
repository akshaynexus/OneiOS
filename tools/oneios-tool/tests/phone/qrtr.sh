# Sourced by tests/phone/run.sh (with lib.sh's helpers, HERE, W and TOOL set): `phone qrtr` against a stand-in phone of
# its own ($W/qrtrphone) with a stand-in sensorctl (a shell script that notes its arguments in .fake/sensorctl.log and
# prints what the phone's sensorctl prints: a hub that answers; with .fake/sensorctl-closed the report of a phone whose
# QRTR socket is refused). The real sensorctl's steps are host-tested in runtime/bridges/sensors (cargo test -p onebridges
# sensors::).
# Checked: root asked for first; no sensorctl staged (and the hint to --binary); the staged one run, its report and the
# verdict; --sensors passed on; --save; --binary: pushed to /data/local/tmp/oneios-sensorctl, run, and removed after
# (also when it fails); a missing --binary refused before anything reaches the phone; a closed QRTR fails the command.
qrtr_checks() {
  local P=$W/qrtrphone before=${FAKE_PHONE:-} out st
  local S=$P/data/local/tmp/oneios L=$P/.fake/sensorctl.log
  export FAKE_PHONE=$P
  make_phone "$P"
  qphone() { "$TOOL" phone qrtr "$@"; }
  ran() { cat "$L" 2>/dev/null; }
  local fake=$W/sensorctl-fake
  cat > "$fake" <<'SH'
#!/bin/sh
# the stand-in sensorctl (tests/phone/qrtr.sh)
echo "$0 $*" >> "$FAKE_PHONE/.fake/sensorctl.log"
if [ -e "$FAKE_PHONE/.fake/sensorctl-closed" ]; then
  echo "qrtr: socket(AF_QIPCRTR): Permission denied (13): this process cannot reach QRTR (refused: SELinux or a capability)"
  echo "qrtr: SSC: unreachable: sensors=auto takes the old path (no motion device, the rear light sensor)"
  echo failed; exit 1
fi
echo "qrtr: socket(AF_QIPCRTR) open: local node 1, our port 0x4001"
echo "qrtr: 2 services on 2 nodes ([0, 5])"
echo "qrtr: service 69 version 1 instance 0 node 0 port 0x2: wlfw"
echo "qrtr: service 400 version 1 instance 0 node 5 port 0x9: SNS client (sensors: SEE)"
echo "qrtr: SSC: service 400 version 1 at node 5 port 0x9 answers: accel lookup gave 1 SUID(s): sensors=auto takes the hub"
[ "$2" = --sensors ] && echo 'sensors: accel 0: 99aabbccddeeff001122334455667788 "lsm6dsv" (STMicro), streaming 15..480 Hz'
echo ok
SH
  chmod 755 "$fake"

  check "qrtr: no root says so" 1 "no root on the phone FAKE0001" env FAKE_NO_ROOT=1 "$TOOL" phone qrtr
  check "qrtr: no sensorctl staged" 1 "the staged runtime has no /data/local/tmp/oneios/runtime/bin/sensorctl: give this machine's copy with --binary" qphone
  check "qrtr --binary: a missing file is refused here" 1 "--binary $W/nothing: no such file here" qphone --binary "$W/nothing"
  [ -z "$(ran)" ] && ok "qrtr: the refusals ran nothing on the phone" || bad "qrtr: a refusal ran" "$(ran | tr '\n' '|')"

  # ---- the staged sensorctl: its report, the verdict, --sensors, --save
  cp "$fake" "$S/runtime/bin/sensorctl"
  out=$(qphone --save "$W/qrtr.txt" 2>&1); st=$?
  [ $st = 0 ] && [ "$(ran)" = "$S/runtime/bin/sensorctl qrtr" ] && ok "qrtr: status 0, the staged sensorctl qrtr ran" || bad "qrtr" "status $st, ran: $(ran | tr '\n' '|'): $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  printf '%s\n' "$out" | grep -q "^qrtr: service 400 version 1 instance 0 node 5 port 0x9: SNS client" \
    && [ "$(printf '%s\n' "$out" | sed -n '/^---- verdict$/,$p' | grep -c .)" = 3 ] \
    && printf '%s\n' "$out" | sed -n '/^---- verdict$/,$p' | grep -q "^qrtr: SSC: service 400 version 1 at node 5 port 0x9 answers" \
    && ok "qrtr: the phone's report as it is, then the verdict (the socket's and the SSC's lines)" || bad "qrtr: the report" "$(printf '%s' "$out" | tr '\n' '|')"
  [ "$(tail -n 1 "$W/qrtr.txt")" = ok ] && grep -q "^qrtr: SSC:" "$W/qrtr.txt" && ok "qrtr --save: the phone's report in the file" || bad "qrtr --save" "$(tail -n 2 "$W/qrtr.txt" 2>&1 | tr '\n' '|')"
  rm -f "$L"
  check "qrtr --sensors" 0 '^sensors: accel 0: 99aabbccddeeff001122334455667788 "lsm6dsv"' qphone --sensors
  [ "$(ran)" = "$S/runtime/bin/sensorctl qrtr --sensors" ] && ok "qrtr --sensors: passed on" || bad "qrtr --sensors" "$(ran)"
  rm -f "$L" "$S/runtime/bin/sensorctl"

  # ---- --binary: pushed, run, removed (also after a failure)
  check "qrtr --binary: this machine's copy" 0 "^qrtr: SSC: service 400" qphone --binary "$fake"
  [ "$(ran)" = "$P/data/local/tmp/oneios-sensorctl qrtr" ] && [ ! -e "$P/data/local/tmp/oneios-sensorctl" ] \
    && ok "qrtr --binary: run from /data/local/tmp/oneios-sensorctl and removed after" || bad "qrtr --binary" "ran: $(ran); left: $(ls "$P/data/local/tmp" | tr '\n' ' ')"
  touch "$P/.fake/sensorctl-closed"
  check "qrtr: a closed QRTR fails the command" 1 "the sensor hub is not reachable this way.*sensors HAL" qphone --binary "$fake"
  [ ! -e "$P/data/local/tmp/oneios-sensorctl" ] && ok "qrtr --binary: removed after a failure too" || bad "qrtr --binary: left after a failure" "$(ls "$P/data/local/tmp")"
  out=$(qphone --binary "$fake" 2>&1)
  printf '%s\n' "$out" | sed -n '/^---- verdict$/,$p' | grep -q "^qrtr: SSC: unreachable" && ok "qrtr: the closed phone's verdict is shown" || bad "qrtr: closed verdict" "$(printf '%s' "$out" | tr '\n' '|')"
  rm -f "$P/.fake/sensorctl-closed" "$L"
  export FAKE_PHONE=$before
}
