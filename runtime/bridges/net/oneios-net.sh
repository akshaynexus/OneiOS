#!/system/bin/sh
# Routes the guest's tap through the phone's active network (IPv4 NAT). Root only; One UI's own
# rules and chains are not edited: everything added is in chains and rules named here and removed
# by `down`. Not yet run on the phone (docs/hardware.md, Wi-Fi and network).
#   oneios-net.sh up [TAP]      address, forwarding, policy rules and NAT for TAP (default oneios0)
#   oneios-net.sh down [TAP]    removes all of it
#   oneios-net.sh status [TAP]  what is there
# The tap itself is made by xnuhost (br_net_open_tap); run `up` once it exists.
set -u
TAP=${2:-oneios0}
HOST=192.168.77.1
NET=192.168.77.0/24
PRIO_OUT=16010      # guest -> uplink: before netd's own rules (they start at 10000 for VPNs; none run in a session)
PRIO_BACK=16000     # uplink -> guest: the tap's connected route is in main, which netd's rules do not consult
CHAIN=oneios_fwd

say() { echo "oneios-net: $*"; }

uplink() {
  # The interface of the default network: what a root socket without a mark would use.
  ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p' | head -n 1
}

up() {
  [ -d "/sys/class/net/$TAP" ] || { say "no interface $TAP (xnuhost makes it)"; exit 1; }
  UP=$(uplink)
  [ -n "$UP" ] && [ "$UP" != "$TAP" ] || { say "no uplink: the phone has no default network"; exit 1; }
  TABLE=$((1000 + $(cat "/sys/class/net/$UP/ifindex")))        # netd's table of an interface
  ip addr replace "$HOST/24" dev "$TAP" || exit 1
  ip link set "$TAP" up || exit 1
  echo 1 > /proc/sys/net/ipv4/ip_forward
  ip rule del priority $PRIO_OUT 2>/dev/null
  ip rule del priority $PRIO_BACK 2>/dev/null
  ip rule add from all iif "$TAP" lookup "$TABLE" priority $PRIO_OUT || exit 1
  ip rule add to "$NET" lookup main priority $PRIO_BACK || exit 1
  iptables -w -N $CHAIN 2>/dev/null || iptables -w -F $CHAIN
  iptables -w -A $CHAIN -i "$TAP" -o "$UP" -j ACCEPT
  iptables -w -A $CHAIN -i "$UP" -o "$TAP" -m state --state RELATED,ESTABLISHED -j ACCEPT
  iptables -w -C FORWARD -j $CHAIN 2>/dev/null || iptables -w -I FORWARD 1 -j $CHAIN
  iptables -w -t nat -N $CHAIN 2>/dev/null || iptables -w -t nat -F $CHAIN
  iptables -w -t nat -A $CHAIN -s "$NET" -o "$UP" -j MASQUERADE
  iptables -w -t nat -C POSTROUTING -j $CHAIN 2>/dev/null || iptables -w -t nat -I POSTROUTING 1 -j $CHAIN
  say "up: $TAP $HOST/24 -> $UP (table $TABLE)"
}

down() {
  ip rule del priority $PRIO_OUT 2>/dev/null
  ip rule del priority $PRIO_BACK 2>/dev/null
  iptables -w -D FORWARD -j $CHAIN 2>/dev/null
  iptables -w -F $CHAIN 2>/dev/null; iptables -w -X $CHAIN 2>/dev/null
  iptables -w -t nat -D POSTROUTING -j $CHAIN 2>/dev/null
  iptables -w -t nat -F $CHAIN 2>/dev/null; iptables -w -t nat -X $CHAIN 2>/dev/null
  [ -d "/sys/class/net/$TAP" ] && ip addr flush dev "$TAP" 2>/dev/null
  say "down: $TAP"
}

status() {
  say "uplink: $(uplink)"
  ip -4 addr show dev "$TAP" 2>&1 | sed 's/^/  /'
  ip rule show | grep -E "^($PRIO_OUT|$PRIO_BACK):" | sed 's/^/  /'
  iptables -w -S $CHAIN 2>/dev/null | sed 's/^/  /'
  iptables -w -t nat -S $CHAIN 2>/dev/null | sed 's/^/  /'
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  status) status ;;
  *) echo "usage: oneios-net.sh up|down|status [TAP]"; exit 2 ;;
esac
