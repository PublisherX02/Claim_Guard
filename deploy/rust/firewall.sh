#!/usr/bin/env bash
# Host firewall for the ClaimGuard server (Linux, iptables). Run as root, on the server.
#
#   CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh apply --dry-run     # print what it would do, change nothing
#   CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh apply --auto-revert 120
#   CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh persist               # re-apply at every boot, after Docker has started
#   bash deploy/rust/firewall.sh status | remove
#
# Why this exists: Docker publishes a container port by rewriting packets in the nat table and forwarding them, so those packets never reach the
# INPUT chain where ufw and most guides put their rules. "ufw deny 6379" does NOT close a published port. Docker reserves the DOCKER-USER chain
# for exactly this: rules there run before Docker's own, on forwarded (container) traffic. This script manages two chains of its own:
#   CLAIMGUARD-FW  (jumped to first from DOCKER-USER)  traffic to containers: only what arrived for port 80 or 443 is let through; everything
#                                                       else that comes in from the outside interface is dropped
#   CLAIMGUARD-IN  (jumped to first from INPUT)        traffic to the host itself: replies, loopback, container bridges, DHCP, ping, 80, 443,
#                                                       and SSH from CG_SSH_FROM only; everything else is dropped
# It does not touch ufw. If ufw is active its allow rules come after ours, so ours win: a stray "allow 22 from anywhere" is shadowed.
#
# Settings (environment):
#   CG_SSH_FROM   REQUIRED. Comma-separated addresses or networks allowed to open SSH, e.g. 203.0.113.7/32,198.51.100.0/24.
#                 The word "any" is accepted (and warned about). Refusing to run without it is deliberate: a firewall that does not know
#                 where you are connecting from locks you out of a machine you cannot walk up to.
#   CG_SSH_PORT   default 22
#   CG_EXT_IF     the outside network interface; default: the one holding the default route
#
# Nothing here has been run on the target machine. It was checked in a throwaway Linux container (rust/tools/firewall_test.py) and by the
# dry-run tests; the colleague who runs it on the server must then run deploy/rust/portcheck.py from ANOTHER machine.
set -euo pipefail

ACTION="${1:-}"
[ $# -gt 0 ] && shift
DRY=0
REVERT=0
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1 ;;
    --auto-revert) shift; REVERT="${1:?--auto-revert needs a number of seconds}" ;;
    *) echo "unknown option: $1"; exit 2 ;;
  esac
  shift
done
case "$REVERT" in ''|*[!0-9]*) echo "--auto-revert must be a whole number of seconds"; exit 2 ;; esac

SSH_PORT="${CG_SSH_PORT:-22}"
FW=CLAIMGUARD-FW
IN=CLAIMGUARD-IN
ENVFILE=/etc/claimguard-firewall.env
UNIT=/etc/systemd/system/claimguard-firewall.service
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"

say() { echo "$*"; }
die() { echo "firewall: $*" >&2; exit 1; }
run() { if [ "$DRY" = 1 ]; then echo "+ $*"; else "$@"; fi; }

# --- input checks: the values end up in iptables commands and in a root-owned file, so only the exact shapes are accepted -----------------------
valid_port() { [[ "$1" =~ ^[0-9]{1,5}$ ]] && [ "$1" -ge 1 ] && [ "$1" -le 65535 ]; }
valid_v4() {
  local a="${1%%/*}" p="" o IFS=.
  [[ "$1" == */* ]] && p="${1#*/}"
  [[ "$a" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || return 1
  for o in $a; do [ "$o" -le 255 ] || return 1; done
  [ -z "$p" ] || { [[ "$p" =~ ^[0-9]{1,2}$ ]] && [ "$p" -le 32 ]; }
}
valid_v6() {
  local a="${1%%/*}" p=""
  [[ "$1" == */* ]] && p="${1#*/}"
  [[ "$a" == *:* && "$a" =~ ^[0-9a-fA-F:]+$ ]] || return 1
  [ -z "$p" ] || { [[ "$p" =~ ^[0-9]{1,3}$ ]] && [ "$p" -le 128 ]; }
}
valid_if() { [[ "$1" =~ ^[A-Za-z0-9_.:-]{1,15}$ ]]; }

SRC4=(); SRC6=(); ANY=0
parse_sources() {
  local raw="${CG_SSH_FROM:-}" item
  [ -n "$raw" ] || die "CG_SSH_FROM is not set. Say where you connect from, e.g. CG_SSH_FROM=203.0.113.7/32 (or CG_SSH_FROM=any to accept the risk)."
  valid_port "$SSH_PORT" || die "CG_SSH_PORT is not a port number"
  IFS=',' read -ra items <<< "$raw"
  for item in "${items[@]}"; do
    item="${item// /}"
    if [ "$item" = any ]; then ANY=1
    elif valid_v4 "$item"; then SRC4+=("$item")
    elif valid_v6 "$item"; then SRC6+=("$item")
    else die "not an address or network: '$item'"
    fi
  done
  [ "$ANY" = 0 ] || say "WARNING: SSH is open to the whole internet. Password guessing and scanners will find it; keys only, and prefer a fixed address."
}

ext_if() {
  local dev="${CG_EXT_IF:-}"
  if [ -z "$dev" ] && command -v ip >/dev/null 2>&1; then
    dev="$(ip route show default 2>/dev/null | awk '{for(i=1;i<NF;i++) if($i=="dev"){print $(i+1); exit}}')"
  fi
  if [ -z "$dev" ] && [ "$DRY" = 1 ]; then dev=eth0; say "(dry run: no default route found here, showing the rules for eth0)" >&2; fi
  [ -n "$dev" ] || die "cannot tell which interface faces the outside: set CG_EXT_IF"
  valid_if "$dev" || die "odd interface name: $dev"
  echo "$dev"
}

# One address family: $1 = iptables or ip6tables, $2 = outside interface, $3 = family (4|6)
apply_family() {
  local ipt="$1" ext="$2" fam="$3" src
  if [ "$DRY" = 0 ]; then
    command -v "$ipt" >/dev/null 2>&1 || { say "$ipt not installed: skipping IPv$fam"; return 0; }
  fi
  # --- containers (forwarded traffic) ---
  if [ "$DRY" = 1 ] || "$ipt" -n -L DOCKER-USER >/dev/null 2>&1; then
    run "$ipt" -N "$FW" 2>/dev/null || true
    run "$ipt" -F "$FW"
    run "$ipt" -A "$FW" -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
    for p in 80 443; do
      run "$ipt" -A "$FW" -p tcp -m conntrack --ctstate DNAT --ctorigdstport "$p" -j RETURN
    done
    run "$ipt" -A "$FW" -i "$ext" -m limit --limit 5/min -j LOG --log-prefix "claimguard-fw drop: "
    run "$ipt" -A "$FW" -i "$ext" -j DROP
    run "$ipt" -A "$FW" -j RETURN
    if [ "$DRY" = 1 ] || ! "$ipt" -C DOCKER-USER -j "$FW" 2>/dev/null; then run "$ipt" -I DOCKER-USER 1 -j "$FW"; fi
  elif [ "$fam" = 4 ]; then
    die "the DOCKER-USER chain does not exist: start Docker first (it creates the chain), then run this again"
  else
    say "IPv$fam: Docker has no DOCKER-USER chain here (IPv6 forwarding is off in Docker by default): container ports are not exposed over IPv6"
  fi
  # --- the host itself ---
  run "$ipt" -N "$IN" 2>/dev/null || true
  run "$ipt" -F "$IN"
  run "$ipt" -A "$IN" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  run "$ipt" -A "$IN" -i lo -j ACCEPT
  run "$ipt" -A "$IN" -i docker0 -j ACCEPT
  run "$ipt" -A "$IN" -i 'br-+' -j ACCEPT
  if [ "$fam" = 4 ]; then
    run "$ipt" -A "$IN" -p udp --sport 67 --dport 68 -j ACCEPT          # a lease renewal answer is not always matched by conntrack
    run "$ipt" -A "$IN" -p icmp --icmp-type echo-request -m limit --limit 5/second -j ACCEPT
  else
    run "$ipt" -A "$IN" -p ipv6-icmp -j ACCEPT                          # neighbour discovery: without it IPv6 stops working
  fi
  run "$ipt" -A "$IN" -p tcp --dport 80 -j ACCEPT
  run "$ipt" -A "$IN" -p tcp --dport 443 -j ACCEPT
  if [ "$ANY" = 1 ]; then
    run "$ipt" -A "$IN" -p tcp --dport "$SSH_PORT" -j ACCEPT
  else
    if [ "$fam" = 4 ]; then
      for src in ${SRC4[@]+"${SRC4[@]}"}; do run "$ipt" -A "$IN" -p tcp --dport "$SSH_PORT" -s "$src" -j ACCEPT; done
    else
      for src in ${SRC6[@]+"${SRC6[@]}"}; do run "$ipt" -A "$IN" -p tcp --dport "$SSH_PORT" -s "$src" -j ACCEPT; done
    fi
  fi
  run "$ipt" -A "$IN" -m limit --limit 5/min -j LOG --log-prefix "claimguard-in drop: "
  run "$ipt" -A "$IN" -j DROP
  if [ "$DRY" = 1 ] || ! "$ipt" -C INPUT -j "$IN" 2>/dev/null; then run "$ipt" -I INPUT 1 -j "$IN"; fi
}

remove_family() {
  local ipt="$1"
  if [ "$DRY" = 0 ]; then command -v "$ipt" >/dev/null 2>&1 || return 0; fi
  while [ "$DRY" = 1 ] || "$ipt" -C DOCKER-USER -j "$FW" 2>/dev/null; do run "$ipt" -D DOCKER-USER -j "$FW"; [ "$DRY" = 1 ] && break; done
  while [ "$DRY" = 1 ] || "$ipt" -C INPUT -j "$IN" 2>/dev/null; do run "$ipt" -D INPUT -j "$IN"; [ "$DRY" = 1 ] && break; done
  for c in "$FW" "$IN"; do
    if [ "$DRY" = 1 ] || "$ipt" -n -L "$c" >/dev/null 2>&1; then run "$ipt" -F "$c"; run "$ipt" -X "$c"; fi
  done
}

need_root() { [ "$DRY" = 1 ] || [ "$(id -u)" = 0 ] || die "run as root (sudo)"; }

case "$ACTION" in
  apply)
    parse_sources; need_root
    EXT="$(ext_if)"
    say "outside interface: $EXT   SSH port: $SSH_PORT   SSH allowed from: ${CG_SSH_FROM}"
    apply_family iptables "$EXT" 4
    apply_family ip6tables "$EXT" 6
    say "done. From ANOTHER machine run: python3 deploy/rust/portcheck.py <this server's address>   (only 80 and 443 may answer)"
    if [ "$REVERT" -gt 0 ] && [ "$DRY" = 0 ]; then
      say "Open a second terminal and connect over SSH now. If you do not answer yes within $REVERT s the rules are removed again."
      ans=""
      read -r -t "$REVERT" -p "Keep the firewall? (yes/no) " ans || true
      if [ "$ans" != yes ]; then
        say "no confirmation: removing the rules"
        remove_family iptables; remove_family ip6tables
      else
        say "kept. Run 'persist' to make it survive a reboot."
      fi
    fi
    ;;
  remove)
    need_root
    remove_family iptables; remove_family ip6tables
    say "removed (the persistent service, if any, is still installed: systemctl disable --now claimguard-firewall)"
    ;;
  status)
    for ipt in iptables ip6tables; do
      command -v "$ipt" >/dev/null 2>&1 || continue
      say "== $ipt =="
      for c in "$FW" "$IN"; do "$ipt" -n -v -L "$c" 2>/dev/null || say "(chain $c absent)"; done
    done
    ;;
  persist)
    parse_sources; need_root
    [ "$DRY" = 1 ] && { say "+ write $ENVFILE (mode 600) and $UNIT, then systemctl enable --now claimguard-firewall"; exit 0; }
    command -v systemctl >/dev/null 2>&1 || die "no systemd here: run 'apply' from your init system after Docker starts"
    ( umask 077; { echo "CG_SSH_FROM=${CG_SSH_FROM}"; echo "CG_SSH_PORT=${SSH_PORT}"; [ -z "${CG_EXT_IF:-}" ] || echo "CG_EXT_IF=${CG_EXT_IF}"; } > "$ENVFILE" )
    cat > "$UNIT" <<EOF
[Unit]
Description=ClaimGuard host firewall rules (DOCKER-USER and INPUT)
After=docker.service network-online.target
Wants=network-online.target
Requires=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
EnvironmentFile=$ENVFILE
ExecStart=/usr/bin/env bash $SELF apply
ExecStop=/usr/bin/env bash $SELF remove

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now claimguard-firewall
    say "installed: the rules are re-applied at every boot once Docker is up. Restarting Docker keeps DOCKER-USER; if you ever flush iptables, run 'apply' again."
    ;;
  *)
    sed -n '2,12p' "$0"; exit 2 ;;
esac
