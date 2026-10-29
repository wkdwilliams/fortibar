#!/bin/bash
# Removes the system-level bits setup.sh created (sudoers rule + root-owned
# helper dir). After this, the whole folder can be deleted normally.
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
pkill -f "$ROOT/dist/FortiBar.app" 2>/dev/null
sudo rm -f /etc/sudoers.d/fortibar
sudo rm -rf "$ROOT/vendor/bin"
# Per-VPN browser profiles (Microsoft login cookies) kept by WebKit.
rm -rf "$HOME/Library/WebKit/local.fortibar" "$HOME/Library/WebKit/fortibar" \
	"$HOME/Library/Caches/local.fortibar" "$HOME/Library/Caches/fortibar"
echo "Removed. You can now delete $ROOT"
