#!/bin/bash
# Root helper for FortiBar. Runs openfortivpn with a SAML session cookie read
# from stdin and wires the VPN's DNS servers into macOS (scutil), which
# openfortivpn itself cannot do (it only knows /etc/resolv.conf).
#
# usage: sudo fortivpn-helper <host:port> [trusted-cert-sha256]
#   stdin: SVPNCOOKIE=<value>
#
# setup.sh installs this root-owned next to the openfortivpn binary and adds a
# sudoers rule for exactly this path.

set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin

DIR="$(cd "$(dirname "$0")" && pwd)"
OFV="$DIR/openfortivpn"
DNS_KEY="State:/Network/Service/fortibar/DNS"

GW="${1:-}"
CERT="${2:-}"

if ! [[ "$GW" =~ ^[A-Za-z0-9.-]+:[0-9]{1,5}$ ]]; then
	echo "ERROR: invalid gateway '$GW'" >&2
	exit 2
fi
if [[ -n "$CERT" ]] && ! [[ "$CERT" =~ ^[0-9A-Fa-f]{64}$ ]]; then
	echo "ERROR: invalid trusted cert digest" >&2
	exit 2
fi

IFS= read -r COOKIE
if ! [[ "$COOKIE" =~ ^(SVPNCOOKIE=)?[A-Za-z0-9%+/=._-]+$ ]]; then
	echo "ERROR: invalid cookie on stdin" >&2
	exit 2
fi

WORK="$(mktemp -d /tmp/fortibar.XXXXXX)"
FIFO="$WORK/out"
mkfifo "$FIFO"

# Primary-service DNS override, used only when the gateway sends no DNS
# suffix (so split DNS has nothing to match on).
backup_primary_dns() {
	local svc
	svc="$(printf 'show State:/Network/Global/IPv4\n' | scutil | awk '/PrimaryService/ {print $3}')"
	[[ -z "$svc" ]] && return 1
	echo "$svc" >"$WORK/primary"
	printf 'get State:/Network/Service/%s/DNS\nd.show\n' "$svc" | scutil >"$WORK/primary.dns" 2>&1
	return 0
}

set_dns() {
	# "Got addresses: [10.1.2.3], ns [10.0.0.1, 10.0.0.2], ns_suffix [a.local;b.local]"
	local line="$1" ns suffix servers="" domains=""
	ns="$(sed -n 's/.*ns \[\([^]]*\)\].*/\1/p' <<<"$line")"
	suffix="$(sed -n 's/.*ns_suffix \[\([^]]*\)\].*/\1/p' <<<"$line")"
	for s in ${ns//,/ }; do
		[[ "$s" != "0.0.0.0" && "$s" =~ ^[0-9.]+$ ]] && servers="$servers $s"
	done
	for d in ${suffix//;/ }; do
		[[ "$d" =~ ^[A-Za-z0-9.-]+$ ]] && domains="$domains $d"
	done
	[[ -z "$servers" ]] && return

	if [[ -n "$domains" ]]; then
		# Split DNS: only names under the VPN's domains go to its servers.
		scutil <<-EOF
			d.init
			d.add ServerAddresses *$servers
			d.add SearchDomains *$domains
			d.add SupplementalMatchDomains *$domains
			set $DNS_KEY
		EOF
		echo "DNS: split DNS for$domains via$servers"
	elif backup_primary_dns; then
		scutil <<-EOF
			d.init
			d.add ServerAddresses *$servers
			set State:/Network/Service/$(cat "$WORK/primary")/DNS
		EOF
		echo "DNS: all lookups via$servers"
	fi
	dscacheutil -flushcache 2>/dev/null
	killall -HUP mDNSResponder 2>/dev/null
}

clear_dns() {
	printf 'remove %s\n' "$DNS_KEY" | scutil
	if [[ -f "$WORK/primary" ]]; then
		local svc
		svc="$(cat "$WORK/primary")"
		if grep -q 'No such key' "$WORK/primary.dns"; then
			printf 'remove State:/Network/Service/%s/DNS\n' "$svc" | scutil
		else
			# Re-apply the saved dictionary.
			{
				echo "d.init"
				awk -F' : ' '
					/^  [A-Za-z]+ : <array>/ {key=$1; gsub(/ /,"",key); vals=""; inarr=1; next}
					inarr && /^  }/ {print "d.add " key " *" vals; inarr=0; next}
					inarr {split($0, kv, " : "); vals = vals " " kv[2]; next}
					/^  [A-Za-z]+ : / {k=$1; gsub(/ /,"",k); print "d.add " k " " $2}
				' "$WORK/primary.dns"
				echo "set State:/Network/Service/$svc/DNS"
			} | scutil
		fi
	fi
	dscacheutil -flushcache 2>/dev/null
	killall -HUP mDNSResponder 2>/dev/null
}

ARGS=("$GW" --cookie-on-stdin --set-dns=0 --pppd-use-peerdns=0)
[[ -n "$CERT" ]] && ARGS+=(--trusted-cert "$CERT")

printf '%s\n' "$COOKIE" | "$OFV" "${ARGS[@]}" >"$FIFO" 2>&1 &
OFV_PID=$!

(
	# Keep handling DNS even if the app went away and stdout is closed.
	trap '' PIPE
	while IFS= read -r line; do
		echo "$line" 2>/dev/null
		case "$line" in
			*"Got addresses:"*) set_dns "$line" ;;
		esac
	done <"$FIFO"
) &
READER_PID=$!

# sudo relays SIGTERM from the menu bar app to us; pass it on so
# openfortivpn tears down routes and pppd cleanly.
trap 'kill -TERM "$OFV_PID" 2>/dev/null' TERM INT HUP

rc=0
while kill -0 "$OFV_PID" 2>/dev/null; do
	wait "$OFV_PID"
	rc=$?
done
wait "$READER_PID" 2>/dev/null

clear_dns >/dev/null 2>&1
rm -rf "$WORK"
exit "$rc"
