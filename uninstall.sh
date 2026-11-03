#!/bin/bash
# Removes what setup.sh and the app put outside this folder: the sudoers rule,
# the root-owned helper, saved VPNs, logins and passwords. After this, the
# whole folder can be deleted normally.
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
pkill -f 'FortiBar.app/Contents/MacOS/FortiBar' 2>/dev/null
# Bring down a tunnel that is still up (the helper stops when the app goes,
# but older helpers didn't), then its split-DNS resolver.
sudo pkill -TERM -f '/usr/local/libexec/fortibar/' 2>/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
	pgrep -f '/usr/local/libexec/fortibar/' >/dev/null || break
	sleep 1
done
sudo pkill -KILL -f '/usr/local/libexec/fortibar/' 2>/dev/null
printf 'remove State:/Network/Service/fortibar/DNS\n' | sudo scutil
sudo rm -f /etc/sudoers.d/fortibar
sudo rm -rf /usr/local/libexec/fortibar "$ROOT/vendor/bin"
# VPN list, Microsoft session cookies and log.
rm -rf "$HOME/Library/Application Support/FortiBar" "$ROOT/data"
# Installed app bundles (only if they really are FortiBar).
for app in /Applications/FortiBar.app "$HOME/Applications/FortiBar.app"; do
	id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist" 2>/dev/null)"
	if [[ "$id" == local.fortibar ]]; then
		rm -rf "$app" 2>/dev/null || sudo rm -rf "$app"
	fi
done
# Start at Login item.
rm -f "$HOME/Library/LaunchAgents/local.fortibar.plist"
# Per-VPN browser profiles (Microsoft login cookies) kept by WebKit.
rm -rf "$HOME/Library/WebKit/local.fortibar" "$HOME/Library/WebKit/fortibar" \
	"$HOME/Library/Caches/local.fortibar" "$HOME/Library/Caches/fortibar"
# Saved passwords for password VPNs (Keychain service "FortiBar").
while security delete-generic-password -s FortiBar >/dev/null 2>&1; do :; done
echo "Removed. You can now delete $ROOT"
