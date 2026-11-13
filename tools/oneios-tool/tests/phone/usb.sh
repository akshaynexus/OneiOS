# Sourced by tests/phone/run.sh (with lib.sh's helpers, W and FORK set): USB and adb during a session
# (oneos/interface.md, "USB and adb during a session"), against the OneOS fork's own scripts under their test root.
# A stand-in phone of its own ($W/usbphone): its properties in a file, getprop and setprop as init answers them
# (sys.usb.config=none unbinds the gadget's UDC and stops adbd; a config with adb starts adbd and binds the UDC, as
# init.usb.configfs.rc does; with .fake/no-usb-init only ctl.* is init's, as on a phone whose USB gadget HAL does the
# rest), the gadget's UDC file (config/usb_gadget/g1/UDC), the UDC's state file (sys/class/udc/<controller>/state)
# and the cable's (sys/class/power_supply/usb/online). Checked:
#   - the switch's record of One UI's USB (oneios_switch.sh --usb-save, control/base/usb);
#   - the session's USB watcher (--usb-watch): an unplug alone changes nothing; a replug whose gadget was unbound
#     gets its UDC back; adb dropped from the gadget and adbd stopped are put back; a cable whose UDC never leaves
#     "not attached" is re-armed (sys.usb.config none, then One UI's), twice per plug-in at most; it ends on
#     usb-watch.off and when the session is over; with USB debugging off it does nothing;
#   - the return (oneios_return.sh --usb-restore) puts back only what differs, through init or by the UDC file;
#   - the session's order: the record in step 1, the watcher after the framework's stop, the watcher ended in the
#     return's first step and the restore before One UI's zygote starts; no listing of /sys/class/udc or the gadget.
usb_checks() {
  local SW="$FORK/switcher/assets/oneios_switch.sh" RT="$FORK/switcher/assets/oneios_return.sh"
  local U=$W/usbphone C=$W/usbphone/data/local/tmp/oneios/control CTRL=a600000.dwc3
  rm -rf "$U"; mkdir -p "$U/bin" "$U/log" "$U/.fake" "$C/base" "$U/config/usb_gadget/g1" "$U/sys/class/udc/$CTRL" "$U/sys/class/power_supply/usb"
  cat > "$U/bin/getprop" <<'SH'
#!/bin/sh
awk -v k="$1" 'index($0, k "=") == 1 { v = substr($0, length(k) + 2); f = 1 } END { if (f) print v }' "$ONEIOS_TEST_ROOT/props"
SH
  cat > "$U/bin/setprop" <<'SH'
#!/bin/sh
# init, as far as these properties go
T=$ONEIOS_TEST_ROOT
set_() { awk -v k="$1" 'index($0, k "=") != 1' "$T/props" > "$T/props.$$"; echo "$1=$2" >> "$T/props.$$"; mv "$T/props.$$" "$T/props"; }
# (logged once init is done with it: a check that sees the line sees its effects)
L="$1 $2"; trap 'echo "$L" >> "$T/log/setprop"' EXIT
case "$1" in
  ctl.start) set_ "init.svc.$2" running ;;
  ctl.stop) set_ "init.svc.$2" stopped ;;
  sys.usb.config)
    set_ "$1" "$2"
    [ -e "$T/.fake/no-usb-init" ] && exit 0
    if [ "$2" = none ]; then
      : > "$T/config/usb_gadget/g1/UDC"; set_ init.svc.adbd stopped; set_ sys.usb.ffs.ready 0
    else
      case ",$2," in *,adb,*) set_ init.svc.adbd running; set_ sys.usb.ffs.ready 1 ;; esac
      "$T/bin/getprop" sys.usb.controller > "$T/config/usb_gadget/g1/UDC"
    fi
    set_ sys.usb.state "$2" ;;
  *) set_ "$1" "$2" ;;
esac
SH
  # the scripts' sleep: a short real one, and a count of the watcher's polls
  printf '#!/bin/sh\necho "$1" >> "$ONEIOS_TEST_ROOT/log/sleep"\nexec %s 0.02\n' "$(command -v sleep)" > "$U/bin/sleep"
  chmod +x "$U/bin/"*
  props() { # One UI's USB with USB debugging on
    printf '%s\n' "sys.usb.config=mtp,adb" "sys.usb.state=mtp,adb" "persist.sys.usb.config=mtp,adb" "sys.usb.controller=$CTRL" \
      "sys.usb.configfs=1" "sys.usb.ffs.ready=1" "init.svc.adbd=running" > "$U/props"
    echo "$CTRL" > "$U/config/usb_gadget/g1/UDC"; echo configured > "$U/sys/class/udc/$CTRL/state"; echo 1 > "$U/sys/class/power_supply/usb/online"
    rm -f "$U/log/setprop" "$U/.fake/no-usb-init"
    touch "$U/data/local/tmp/oneios/usb-repair"     # the repairs on (off by default: checked below)
  }
  prop() { awk -v k="$1" 'index($0, k "=") == 1 { print substr($0, length(k) + 2) }' "$U/props"; }
  setp() { awk -v k="$1" 'index($0, k "=") != 1' "$U/props" > "$U/props.n"; echo "$1=$2" >> "$U/props.n"; mv "$U/props.n" "$U/props"; }
  polls() { grep -c . "$U/log/sleep" 2>/dev/null || echo 0; }
  polls_past() { [ "$(polls)" -ge "$1" ]; }
  setprops() { cat "$U/log/setprop" 2>/dev/null | tr '\n' '|'; }
  run_sw() { ONEIOS_TEST_ROOT=$U ONEIOS_TEST_SH=/bin/sh sh "$SW" "$@"; }
  props; echo running > "$C/state"

  # ---- the switch's record
  check "usb: the switch records One UI's USB" 0 "USB of One UI: state mtp,adb, config mtp,adb, UDC $CTRL configured; bound '$CTRL', adbd running.*USB debugging on" run_sw --usb-save
  for want in adb=1 config=mtp,adb controller=$CTRL udc=$CTRL adbd=running; do
    grep -qx "$want" "$C/base/usb" || { bad "usb: control/base/usb has $want" "$(tr '\n' '|' < "$C/base/usb")"; break; }
  done && ok "usb: control/base/usb: adb, config, controller, the bound UDC, adbd"
  [ ! -e "$U/log/setprop" ] && ok "usb: the record changes nothing" || bad "usb: the record set properties" "$(setprops)"

  # ---- the watcher
  rm -f "$U/log/sleep" "$C/usb-watch.off"
  run_sw --usb-watch > "$U/usb.log" 2>&1 &
  local wp=$!
  wait_for 10 grep -q "usb: state mtp,adb, config mtp,adb, UDC $CTRL configured" "$U/usb.log" &&
    ok "usb watcher: says what it keeps and logs the gadget" || bad "usb watcher: its first lines" "$(tr '\n' '|' < "$U/usb.log")"
  local up; up=$(cat "$C/usb-watch.pid" 2>/dev/null)
  ps -o args= -p "${up:-0}" 2>/dev/null | grep -q "oneios_switch.sh --usb-watch" && ok "usb watcher: its pid for the return (control/usb-watch.pid)" ||
    bad "usb watcher: usb-watch.pid" "'$up': $(ps -o args= -p "${up:-0}" 2>/dev/null)"
  # an unplug: logged, nothing done
  echo 0 > "$U/sys/class/power_supply/usb/online"; echo "not attached" > "$U/sys/class/udc/$CTRL/state"
  local p0; p0=$(polls)
  wait_for 10 polls_past $((p0 + 12))
  grep -q "UDC $CTRL not attached;.*cable 0$" "$U/usb.log" && [ ! -e "$U/log/setprop" ] && ok "usb watcher: an unplug is logged and changes nothing" ||
    bad "usb watcher: the unplug" "$(setprops) $(tail -n 2 "$U/usb.log" | tr '\n' '|')"
  # a replug after something unbound the gadget (no framework to bind it again)
  : > "$U/config/usb_gadget/g1/UDC"; echo 1 > "$U/sys/class/power_supply/usb/online"
  wait_for 10 grep -qx "$CTRL" "$U/config/usb_gadget/g1/UDC" && grep -q "the gadget was unbound: UDC $CTRL written" "$U/usb.log" &&
    ok "usb watcher: a replug with the gadget unbound gets One UI's UDC back" || bad "usb watcher: the UDC" "$(tail -n 2 "$U/usb.log" | tr '\n' '|')"
  echo configured > "$U/sys/class/udc/$CTRL/state"
  # adb dropped from the gadget
  setp sys.usb.config mtp; setp sys.usb.state mtp
  wait_for 10 grep -q "^sys.usb.config mtp,adb$" "$U/log/setprop" && [ "$(prop sys.usb.state)" = mtp,adb ] &&
    ok "usb watcher: adb dropped from the gadget: One UI's config again" || bad "usb watcher: the config" "$(setprops)"
  # adbd stopped
  setp init.svc.adbd stopped
  wait_for 10 grep -q "^ctl.start adbd$" "$U/log/setprop" && [ "$(prop init.svc.adbd)" = running ] &&
    ok "usb watcher: adbd stopped: ctl.start adbd" || bad "usb watcher: adbd" "$(setprops)"
  # a cable whose UDC stays "not attached" (bound, adb on, adbd running): re-armed twice for this plug-in, no more
  echo 0 > "$U/sys/class/power_supply/usb/online"; echo "not attached" > "$U/sys/class/udc/$CTRL/state"
  p0=$(polls); wait_for 10 polls_past $((p0 + 2))
  echo 1 > "$U/sys/class/power_supply/usb/online"
  wait_for 20 test "$(grep -c "re-armed" "$U/usb.log")" -ge 2
  p0=$(polls); wait_for 20 polls_past $((p0 + 30))
  local n; n=$(grep -c "re-armed" "$U/usb.log")
  [ "$n" = 2 ] && grep -q "is 'not attached': re-armed (sys.usb.config none, then mtp,adb; 2 of 2 for this plug-in)" "$U/usb.log" &&
    grep -q "^sys.usb.config none|sys.usb.config mtp,adb|sys.usb.config none|sys.usb.config mtp,adb|$" <(grep '^sys.usb.config' "$U/log/setprop" | tail -n 4 | tr '\n' '|'; echo) &&
    ok "usb watcher: a cable that never enumerates is re-armed twice for its plug-in (none, then One UI's config), then left alone" ||
    bad "usb watcher: the re-arm" "$n re-arms; $(setprops)"
  # a new plug-in that enumerates: nothing more
  echo 0 > "$U/sys/class/power_supply/usb/online"; p0=$(polls); wait_for 10 polls_past $((p0 + 2))
  local before; before=$(grep -c . "$U/log/setprop")
  echo configured > "$U/sys/class/udc/$CTRL/state"; echo 1 > "$U/sys/class/power_supply/usb/online"
  p0=$(polls); wait_for 20 polls_past $((p0 + 12))
  [ "$(grep -c . "$U/log/setprop")" = "$before" ] && ok "usb watcher: a replug that enumerates changes nothing" || bad "usb watcher: a good replug" "$(setprops)"
  touch "$C/usb-watch.off"
  wait_for 10 sh -c "! kill -0 $wp" && grep -q "usb: ended (control/usb-watch.off)" "$U/usb.log" && ok "usb watcher: ends on usb-watch.off" ||
    { bad "usb watcher: did not end on usb-watch.off"; kill "$wp" 2>/dev/null; }
  wait "$wp" 2>/dev/null
  # the session is over: it ends by itself
  rm -f "$C/usb-watch.off"; run_sw --usb-watch > "$U/usb2.log" 2>&1 & wp=$!
  wait_for 10 grep -q "usb: state" "$U/usb2.log"; echo returning > "$C/state"
  wait_for 10 sh -c "! kill -0 $wp" && grep -q "the session is over: the watcher ends" "$U/usb2.log" && ok "usb watcher: ends when the return begins" ||
    { bad "usb watcher: did not end with the session"; kill "$wp" 2>/dev/null; }
  wait "$wp" 2>/dev/null; echo running > "$C/state"
  # USB debugging off at the switch
  props; printf '%s\n' "sys.usb.config=mtp" "sys.usb.state=mtp" "persist.sys.usb.config=mtp" "sys.usb.controller=$CTRL" "init.svc.adbd=stopped" > "$U/props"
  run_sw --usb-save > /dev/null 2>&1
  grep -qx adb=0 "$C/base/usb" && ok "usb: USB debugging off is recorded" || bad "usb: adb=0" "$(tr '\n' '|' < "$C/base/usb")"
  check "usb watcher: with USB debugging off it keeps nothing" 0 "One UI had USB debugging off at the switch.*nothing to keep" timeout 10 env ONEIOS_TEST_ROOT=$U sh "$SW" --usb-watch
  [ ! -e "$U/log/setprop" ] && ok "usb watcher: with USB debugging off nothing is set" || bad "usb watcher: set with USB debugging off" "$(setprops)"

  # ---- the repairs off (the default: no usb-repair in the stage): logged only, nothing written, by either script
  props; rm -f "$U/data/local/tmp/oneios/usb-repair" "$C/usb-watch.off"; run_sw --usb-save > /dev/null 2>&1
  grep -qx repair=0 "$C/base/usb" && ok "usb: the repairs are off by default (repair=0)" || bad "usb: repair=0" "$(tr '\n' '|' < "$C/base/usb")"
  run_sw --usb-watch > "$U/usb3.log" 2>&1 & wp=$!
  wait_for 10 grep -q "repairs off" "$U/usb3.log"
  : > "$U/config/usb_gadget/g1/UDC"; setp init.svc.adbd stopped
  wait_for 10 grep -q "repairs are off: logged only" "$U/usb3.log"
  touch "$C/usb-watch.off"; wait_for 10 sh -c "! kill -0 $wp"; wait "$wp" 2>/dev/null
  [ ! -e "$U/log/setprop" ] && [ ! -s "$U/config/usb_gadget/g1/UDC" ] && grep -q "ctl.start adbd (repairs are off" "$U/usb3.log" &&
    ok "usb watcher: with the repairs off it logs what it would do and writes nothing" || bad "usb watcher: repairs off" "$(setprops) $(tail -n 3 "$U/usb3.log" | tr '\n' '|')"
  setp sys.usb.config adb
  check "usb return: with the repairs off nothing is put back" 0 "usb: repairs are off .*nothing put back; One UI's were config 'mtp,adb', adbd 'running', UDC '$CTRL'" env ONEIOS_TEST_ROOT=$U sh "$RT" --usb-restore
  [ ! -e "$U/log/setprop" ] && [ "$(prop sys.usb.config)" = adb ] && ok "usb return: with the repairs off no property is set" || bad "usb return: repairs off set" "$(setprops)"

  # ---- the return
  props; run_sw --usb-save > /dev/null 2>&1; rm -f "$U/log/setprop"
  check "usb return: nothing differs: nothing is set" 0 "usb: after the restore: state mtp,adb, config mtp,adb, UDC $CTRL configured" env ONEIOS_TEST_ROOT=$U sh "$RT" --usb-restore
  [ ! -e "$U/log/setprop" ] && ok "usb return: nothing differs: no property set" || bad "usb return: set with nothing to restore" "$(setprops)"
  setp sys.usb.config adb; setp sys.usb.state adb; setp init.svc.adbd stopped; : > "$U/config/usb_gadget/g1/UDC"
  check "usb return: One UI's config, through init" 0 "usb: sys.usb.config adb -> mtp,adb \(One UI's\)" env ONEIOS_TEST_ROOT=$U sh "$RT" --usb-restore
  [ "$(prop sys.usb.config)" = mtp,adb ] && [ "$(prop init.svc.adbd)" = running ] && grep -qx "$CTRL" "$U/config/usb_gadget/g1/UDC" &&
    ok "usb return: config, adbd and the UDC as at the switch" || bad "usb return: through init" "$(setprops)"
  setp sys.usb.config adb; setp sys.usb.state adb; setp init.svc.adbd stopped; : > "$U/config/usb_gadget/g1/UDC"; touch "$U/.fake/no-usb-init"; rm -f "$U/log/setprop"
  check "usb return: with a gadget HAL instead of init's triggers, adbd and the UDC by the return" 0 "usb: UDC $CTRL written back" env ONEIOS_TEST_ROOT=$U sh "$RT" --usb-restore
  [ "$(setprops)" = "sys.usb.config mtp,adb|ctl.start adbd|" ] && grep -qx "$CTRL" "$U/config/usb_gadget/g1/UDC" &&
    ok "usb return: sys.usb.config, ctl.start adbd, then the UDC file" || bad "usb return: with a HAL" "$(setprops)"
  rm -f "$C/base/usb"
  check "usb return: no record of the switch: left as it is" 0 "usb: no record of the switch" env ONEIOS_TEST_ROOT=$U sh "$RT" --usb-restore

  # ---- the session's order, and no listing
  line() { grep -n -m1 -F -- "$2" "$1" | cut -d: -f1; }
  local a b c d
  a=$(grep -n -x 'usb_save' "$SW" | head -n 1 | cut -d: -f1); b=$(grep -n -x 'setprop ctl.stop zygote' "$SW" | cut -d: -f1)
  c=$(line "$SW" '--usb-watch </dev/null'); d=$(grep -n '^echo "=== \[4/5\]' "$SW" | cut -d: -f1)
  [ -n "$a" ] && [ -n "$b" ] && [ -n "$c" ] && [ -n "$d" ] && [ "$a" -lt "$b" ] && [ "$b" -lt "$c" ] && [ "$c" -lt "$d" ] &&
    ok "usb: the switch records One UI's USB before the stop and starts the watcher after it ($a < $b < $c < $d)" || bad "usb: the switch's order" "$a $b $c $d"
  a=$(line "$RT" 'end_helper "$CTL/usb-watch.pid" --usb-watch'); b=$(line "$RT" 'step "[2/5]'); c=$(grep -n -x 'usb_restore' "$RT" | cut -d: -f1)
  d=$(line "$RT" 'step "[4/5]'); local e; e=$(line "$RT" 'setprop ctl.start zygote')
  [ -n "$a" ] && [ -n "$b" ] && [ -n "$c" ] && [ -n "$d" ] && [ -n "$e" ] && [ "$a" -lt "$b" ] && [ "$d" -lt "$c" ] && [ "$c" -lt "$e" ] &&
    ok "usb: the return ends the watcher in its first step and restores USB before One UI's zygote ($a < $b; $d < $c < $e)" || bad "usb: the return's order" "$a $b $c $d $e"
  if grep -nE '(ls|find|du)[[:space:]][^#]*(udc|usb_gadget|power_supply)|/sys/class/udc/?\*|usb_gadget/g1/?\*|class/udc"?$' "$SW" "$RT"; then bad "usb: a listing of the UDCs or the gadget in the fork's scripts"
  else ok "usb: the fork's scripts read the UDC by name only (no listing of /sys/class/udc or the gadget)"; fi
}
