#!/bin/sh
# The tap's script against a stand-in phone: a fake `ip` and `iptables` record every change, and a stand-in
# /sys and /proc hold an interface. What it checks: `up` builds the address, forwarding, policy rules and NAT
# from the tap's name and the uplink's ifindex; every rule delete names our selector, so netd's own rule at
# priority 16000 (fwmark 0x10063/0x1ffff iif lo lookup local_network) is never touched; `down` removes only ours.
#   sh runtime/bridges/net/test-oneios-net.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
script=$here/oneios-net.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
log=$tmp/log
mkdir -p "$tmp/bin" "$tmp/sys/class/net/oneios0" "$tmp/sys/class/net/rmnet0" "$tmp/proc/sys/net/ipv4"
echo 5 > "$tmp/sys/class/net/rmnet0/ifindex"
: > "$tmp/proc/sys/net/ipv4/ip_forward"
{
  printf '%s\n' '#!/bin/sh'
  printf 'echo "ip $*" >> %s\n' "$log"
  printf '%s\n' 'case "$*" in *"route get"*) echo "1.1.1.1 via 10.77.0.1 dev rmnet0 src 10.77.0.2";; esac'
  printf '%s\n' 'exit 0'
} > "$tmp/bin/ip"
{
  printf '%s\n' '#!/bin/sh'
  printf 'echo "iptables $*" >> %s\n' "$log"
  printf '%s\n' 'case "$*" in *" -C "*) exit 1;; esac' # a fresh phone: no chain jump or rule yet
  printf '%s\n' 'exit 0'
} > "$tmp/bin/iptables"
chmod +x "$tmp/bin/ip" "$tmp/bin/iptables"

run() {
  PATH=$tmp/bin:$PATH ONEIOS_NET_SYS=$tmp/sys ONEIOS_NET_PROC=$tmp/proc sh "$script" "$@"
}
check() { # DESCRIPTION PATTERN: the log has a line matching the pattern (grep -F: no regex metacharacters in the calls)
  if ! grep -qF -- "$2" "$log"; then
    echo "FAIL: $1: no line with: $2"
    echo "--- the calls were:"
    cat "$log"
    exit 1
  fi
}
refuse() { # DESCRIPTION PATTERN: no line matches
  if grep -qF -- "$2" "$log"; then
    echo "FAIL: $1: found: $2"
    cat "$log"
    exit 1
  fi
}

: > "$log"
run up
check "the tap's address" "ip addr replace 192.168.77.1/24 dev oneios0"
check "the tap is up" "ip link set oneios0 up"
check "the uplink's table" "lookup 1005"
check "the delete names the tap and the table" \
  "ip rule del from all iif oneios0 lookup 1005 priority 16010"
check "the guest's rule" "ip rule add from all iif oneios0 lookup 1005 priority 16010"
check "the delete to the guest's net" "ip rule del to 192.168.77.0/24 lookup main priority 16020"
check "the way back" "ip rule add to 192.168.77.0/24 lookup main priority 16020"
refuse "no delete by priority alone" "ip rule del priority "
check "the forward chain" "iptables -w -I FORWARD 1 -j oneios_fwd"
check "the NAT" "iptables -w -t nat -A oneios_fwd -s 192.168.77.0/24 -o rmnet0 -j MASQUERADE"
check "the NAT jumps" "iptables -w -t nat -I POSTROUTING 1 -j oneios_fwd"
if [ "$(cat "$tmp/proc/sys/net/ipv4/ip_forward")" != 1 ]; then echo "FAIL: forwarding not written"; exit 1; fi

: > "$log"
run down
check "the guest's rule is deleted by its selector" "ip rule del from all iif oneios0 priority 16010"
check "the way back is deleted by its selector" "ip rule del to 192.168.77.0/24 lookup main priority 16020"
refuse "no delete by priority alone (down)" "ip rule del priority "
check "the forward jump removed" "iptables -w -D FORWARD -j oneios_fwd"
check "the NAT jump removed" "iptables -w -t nat -D POSTROUTING -j oneios_fwd"

echo "oneios-net: pass (up and down; only our rules, never netd's 16000)"
