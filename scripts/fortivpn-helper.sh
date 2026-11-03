#!/bin/bash
# Root helper for FortiBar. Runs openfortivpn with a SAML session cookie or a
# username and password read from stdin, and wires the VPN's DNS servers into
# macOS (scutil), which openfortivpn itself cannot do (it only knows
# /etc/resolv.conf).
#
# usage: sudo fortivpn-helper <host:port> [trusted-cert-sha256] [dns-domains]
#   trusted-cert-sha256: may be empty when only dns-domains is given
#   dns-domains: comma-separated, e.g. corp.example.com,example.net; names
#                under them go to the VPN's DNS servers, along with any
#                domains the gateway pushes
#   stdin: SVPNCOOKIE=<value>
#      or: USER=<username>
#          <password>
#          then held open while the tunnel should stay up: EOF (the app
#          disconnecting, quitting or crashing) takes the tunnel down.
#
# setup.sh installs this root-owned next to the openfortivpn binary in
# /usr/local/libexec/fortibar and adds a sudoers rule for exactly this path.

set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin

DIR="$(cd "$(dirname "$0")" && pwd)"
OFV="$DIR/openfortivpn"
DNS_KEY="State:/Network/Service/fortibar/DNS"

GW="${1:-}"
CERT="${2:-}"
EXTRA_DOMAINS="${3:-}"

# Starts with a letter or digit, so openfortivpn can't read it as an option.
if ! [[ "$GW" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*:[0-9]{1,5}$ ]]; then
	echo "ERROR: invalid gateway '$GW'" >&2
	exit 2
fi
if [[ -n "$CERT" ]] && ! [[ "$CERT" =~ ^[0-9A-Fa-f]{64}$ ]]; then
	echo "ERROR: invalid trusted cert digest" >&2
	exit 2
fi
# Length checked separately: macOS regex caps repeat counts at 255
# (RE_DUP_MAX), and a bigger {m,n} fails to compile, rejecting everything.
if ! [[ "$EXTRA_DOMAINS" =~ ^[A-Za-z0-9.,-]*$ && ${#EXTRA_DOMAINS} -le 1024 ]]; then
	echo "ERROR: invalid DNS domains" >&2
	exit 2
fi

IFS= read -r FIRST
if [[ "$FIRST" == USER=* ]]; then
	VPN_USER="${FIRST#USER=}"
	IFS= read -r SECRET || true
	if ! [[ "$VPN_USER" =~ ^[[:print:]]{1,64}$ ]]; then
		echo "ERROR: invalid username on stdin" >&2
		exit 2
	fi
	if [[ -z "$SECRET" || ${#SECRET} -gt 256 ]]; then
		echo "ERROR: invalid password on stdin" >&2
		exit 2
	fi
	# openfortivpn reads the password from stdin when none is configured.
	ARGS=("$GW" "--username=$VPN_USER")
else
	SECRET="$FIRST"
	if ! [[ "$SECRET" =~ ^(SVPNCOOKIE=)?[A-Za-z0-9%+/=._-]+$ ]]; then
		echo "ERROR: invalid cookie on stdin" >&2
		exit 2
	fi
	ARGS=("$GW" --cookie-on-stdin)
fi

WORK="$(mktemp -d /tmp/fortibar.XXXXXX)"
FIFO="$WORK/out"
mkfifo "$FIFO"

# Wires the VPN's DNS servers in as a split-DNS resolver: only names under the
# VPN's domains go to them. The resolver isn't tied to an interface, so its
# queries follow the routing table into the tunnel and leave from the tunnel
# address, which is the only source the gateway's DNS servers answer.
#
# There is deliberately no "all lookups via the VPN" mode. Overriding the
# primary service's DNS ties the servers to Wi-Fi/Ethernet, so queries leave
# from that interface's address, time out, and fall through to any public
# server in the list, which answers NXDOMAIN for internal names.
#
# Runs once the tunnel is up, so the gateway's routes are in place.
set_dns() {
	# "Got addresses: [10.1.2.3], ns [10.0.0.1, 10.0.0.2], ns_suffix [a.local;b.local]"
	local line="$1" ns suffix all="" servers="" domains="" s d
	ns="$(sed -n 's/.*ns \[\([^]]*\)\].*/\1/p' <<<"$line")"
	suffix="$(sed -n 's/.*ns_suffix \[\([^]]*\)\].*/\1/p' <<<"$line")"
	for s in ${ns//,/ }; do
		[[ "$s" != "0.0.0.0" && "$s" =~ ^[0-9.]+$ ]] || continue
		all="$all $s"
		# Keep only servers reached through the tunnel. A public server the
		# gateway also lists (e.g. 8.8.8.8) can't know internal names, and
		# its NXDOMAIN could win over the real answer.
		if [[ "$(route -n get "$s" 2>/dev/null | awk '/interface:/ {print $2}')" == ppp* ]]; then
			servers="$servers $s"
		fi
	done
	[[ -z "$servers" ]] && servers="$all"
	[[ -z "$servers" ]] && return
	for d in ${suffix//;/ } ${EXTRA_DOMAINS//,/ }; do
		d="${d#.}"
		# At least two labels: a bare "com" would send every .com lookup to
		# the VPN's DNS servers.
		[[ "$d" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ && " $domains " != *" $d "* ]] && domains="$domains $d"
	done

	if [[ -z "$domains" ]]; then
		echo "DNS: the gateway sent DNS servers$servers but no DNS domains, so they aren't used." \
			"To resolve internal names, add the VPN's domains under DNS Domains in Edit VPN."
		return
	fi
	scutil <<-EOF
		d.init
		d.add ServerAddresses *$servers
		d.add SearchDomains *$domains
		d.add SupplementalMatchDomains *$domains
		set $DNS_KEY
	EOF
	echo "DNS: names under$domains go to$servers"
	dscacheutil -flushcache 2>/dev/null
	killall -HUP mDNSResponder 2>/dev/null
}

clear_dns() {
	printf 'remove %s\n' "$DNS_KEY" | scutil
	dscacheutil -flushcache 2>/dev/null
	killall -HUP mDNSResponder 2>/dev/null
}

# Everything comes from the command line; an empty --config stops openfortivpn
# looking for a config file under the sysconfdir it was built with.
ARGS+=(--config= --set-dns=0 --pppd-use-peerdns=0)
[[ -n "$CERT" ]] && ARGS+=(--trusted-cert "$CERT")

# Set up before openfortivpn starts, so a stop request can't slip past.
# sudo relays SIGTERM from the menu bar app to us; pass it on so openfortivpn
# tears down routes and pppd cleanly, and kill it if it hasn't after 10s.
OFV_PID=""
STOPPING=0
STOP_AT=0
WATCH_PID=""
on_stop() {
	[[ -z "$OFV_PID" ]] && exit 143
	(( STOPPING )) || { STOPPING=1; STOP_AT=$SECONDS; }
	kill -TERM "$OFV_PID" 2>/dev/null
}
# Runs however the helper ends, so a split-DNS resolver never outlives the
# tunnel it points into.
cleanup() {
	[[ -n "$WATCH_PID" ]] && kill "$WATCH_PID" 2>/dev/null
	clear_dns >/dev/null 2>&1
	rm -rf "$WORK"
}
trap on_stop TERM INT HUP
trap cleanup EXIT

# A resolver left behind by a helper that was killed outright.
clear_dns >/dev/null 2>&1

# The app holds our stdin open while it wants the tunnel up. EOF means it
# disconnected, quit or crashed: stop, rather than leave a tunnel running
# that nothing shows or controls. (Background jobs get /dev/null as stdin,
# hence fd 3.)
exec 3<&0
(
	cat <&3 >/dev/null
	kill -TERM $$ 2>/dev/null
) &
WATCH_PID=$!

printf '%s\n' "$SECRET" | "$OFV" "${ARGS[@]}" >"$FIFO" 2>&1 3<&- &
OFV_PID=$!

(
	# Keep handling DNS even if the app went away and stdout is closed.
	trap '' PIPE
	addresses=""
	while IFS= read -r line; do
		echo "$line" 2>/dev/null
		case "$line" in
			*"Got addresses:"*) addresses="$line" ;;
			*"Tunnel is up and running."*) [[ -n "$addresses" ]] && set_dns "$addresses" ;;
		esac
	done <"$FIFO"
) &
READER_PID=$!

# Polls rather than blocking in `wait`, so a hung openfortivpn can be killed.
while kill -0 "$OFV_PID" 2>/dev/null; do
	if (( STOPPING && SECONDS - STOP_AT >= 10 )); then
		kill -KILL "$OFV_PID" 2>/dev/null
	fi
	sleep 1
done
wait "$OFV_PID"
rc=$?
wait "$READER_PID" 2>/dev/null
exit "$rc"
