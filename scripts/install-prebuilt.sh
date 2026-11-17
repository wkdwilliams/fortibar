#!/bin/bash
# Installs a prebuilt FortiBar made by ./setup.sh --package: the root-owned
# helper and openfortivpn into /usr/local/libexec/fortibar, a sudoers rule so
# the app can start the VPN without asking for your password, and
# FortiBar.app into /Applications. Shipped in the zip as install.sh.
#
#   bash install.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=install-lib.sh
source "$HERE/install-lib.sh"
APP=/Applications/FortiBar.app

if [[ "$(uname -m)" != arm64 ]]; then
	echo "This build is for Apple Silicon Macs only." >&2
	exit 1
fi
if pgrep -qx FortiBar; then
	echo "FortiBar is running. Disconnect, quit it, then run this again." >&2
	exit 1
fi

# Unzipped downloads are quarantined, and macOS won't run them unsigned.
xattr -dr com.apple.quarantine "$HERE" 2>/dev/null || true

install_helper "$HERE/openfortivpn" "$HERE/fortivpn-helper"
# This openfortivpn's OpenSSL reads its config and CA certificates only from
# $BIN/ssl (root-owned). Point it at the macOS CA bundle.
sudo install -d -o root -g wheel -m 755 "$BIN/ssl"
sudo ln -sfn /etc/ssl/cert.pem "$BIN/ssl/cert.pem"

# Only ever replace an older FortiBar, never some other app.
if [[ -e "$APP" ]]; then
	id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" 2>/dev/null || true)"
	if [[ "$id" != local.fortibar ]]; then
		echo "$APP exists and isn't FortiBar, so it was left alone." >&2
		exit 1
	fi
fi
sudo=()
[[ -w /Applications ]] || sudo=(sudo)
"${sudo[@]+"${sudo[@]}"}" rm -rf "$APP"
"${sudo[@]+"${sudo[@]}"}" ditto "$HERE/FortiBar.app" "$APP"
echo "Installed $APP"
echo "Start it with: open $APP"
