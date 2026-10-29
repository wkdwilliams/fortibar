#!/usr/bin/env bash
# Run inside an MSYS2 MINGW64 shell with mingw-w64-x86_64-openconnect
# installed. Copies openconnect.exe with the DLLs it needs, the Windows vpnc
# script (routes/DNS) and wintun.dll (the tunnel driver) into $1.
set -euo pipefail

OUT="$1"
WINTUN_VERSION=0.14.1
mkdir -p "$OUT"

cp /mingw64/bin/openconnect.exe "$OUT/"
ldd /mingw64/bin/openconnect.exe | awk '$3 ~ /^\/mingw64\// {print $3}' | sort -u | while read -r dll; do
	cp "$dll" "$OUT/"
done

script="$(find /mingw64 -name 'vpnc-script*.js' 2>/dev/null | head -1)"
if [[ -n "$script" ]]; then
	cp "$script" "$OUT/vpnc-script-win.js"
else
	curl -fsSL https://gitlab.com/openconnect/vpnc-scripts/-/raw/master/vpnc-script.js -o "$OUT/vpnc-script-win.js"
fi

tmp="$(mktemp -d)"
curl -fsSL "https://www.wintun.net/builds/wintun-$WINTUN_VERSION.zip" -o "$tmp/wintun.zip"
unzip -q -o -j "$tmp/wintun.zip" 'wintun/bin/amd64/wintun.dll' -d "$OUT"
rm -rf "$tmp"

"$OUT/openconnect.exe" --version | head -1
ls "$OUT"
