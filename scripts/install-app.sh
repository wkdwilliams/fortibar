#!/bin/bash
# Moves the built dist/FortiBar.app into an Applications folder.
#
#   scripts/install-app.sh [dest]
#     dest: /Applications, ~/Applications, any folder, or "none" to leave it
#           in dist/. Asks when omitted.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/FortiBar.app"
BUNDLE_ID=local.fortibar

if [[ ! -d "$APP" ]]; then
	echo "No app at $APP. Build it first with: make" >&2
	exit 1
fi

bundle_id() {
	/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null || true
}

dest="${1:-}"
if [[ -z "$dest" ]]; then
	if [[ ! -t 0 ]]; then
		echo "Left the app in $APP (no terminal to ask; pass DEST=... to choose)."
		exit 0
	fi
	echo
	echo "Where should FortiBar.app go?"
	echo "  1) /Applications    (all users)"
	echo "  2) ~/Applications   (just you)"
	echo "  3) Leave it in dist/"
	if ! read -r -p "Choose [1]: " choice; then
		choice=3 # Ctrl-D
		echo
	fi
	case "${choice:-1}" in
		1) dest=/Applications ;;
		2) dest="$HOME/Applications" ;;
		3) dest=none ;;
		*) echo "Unknown choice: $choice" >&2; exit 1 ;;
	esac
fi

if [[ "$dest" == none ]]; then
	echo "Left the app in $APP"
	exit 0
fi

dest="${dest/#\~/$HOME}"
target="$dest/FortiBar.app"
mkdir -p "$dest" 2>/dev/null || sudo mkdir -p "$dest"

# Only ever replace an older FortiBar, never some other app.
if [[ -e "$target" && "$(bundle_id "$target")" != "$BUNDLE_ID" ]]; then
	echo "$target exists and isn't FortiBar, so it was left alone." >&2
	exit 1
fi

# A copy installed earlier with sudo can be root-owned even inside a folder
# we can write to.
sudo=()
if [[ ! -w "$dest" ]] || { [[ -e "$target" ]] && [[ -n "$(find "$target" ! -user "$(id -u)" -print -quit 2>/dev/null)" ]]; }; then
	sudo=(sudo)
fi
run() { "${sudo[@]+"${sudo[@]}"}" "$@"; }
# Move the new app in next to the old one first, so a failure partway
# leaves the old install working.
staged="$dest/.FortiBar.app.new"
run rm -rf "$staged"
run mv "$APP" "$staged"
run rm -rf "$target"
run mv "$staged" "$target"
echo "Installed $target"

# A copy left in the other Applications folder would be stale from now on.
for other in /Applications/FortiBar.app "$HOME/Applications/FortiBar.app"; do
	if [[ "$other" != "$target" && "$(bundle_id "$other")" == "$BUNDLE_ID" ]]; then
		echo "Note: an older copy is still at $other. Delete it so you don't open it by mistake."
	fi
done

echo "Start it with: open \"$target\""
