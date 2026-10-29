#!/usr/bin/env bash
# Run inside an MSYS2 UCRT64 shell. Builds openconnect from its release
# tarball and copies it into $1 with the DLLs it needs, the Windows vpnc
# script (routes/DNS) and wintun.dll (the tunnel driver).
#
# Build deps:
#   pacman -S --needed make unzip mingw-w64-ucrt-x86_64-{gcc,pkgconf,gnutls,libxml2,zlib,lz4}
set -euo pipefail

OUT="$1"
OPENCONNECT_VERSION=9.21
OPENCONNECT_SHA256=5b32369467db6e5f317aa1ed12cfcbb81ed00bdbc765450b6bfcbdc300944a58
VPNC_SCRIPTS_COMMIT=6cdb2fe2d75be324564fe27ff85a5e1bb17a66b0
VPNC_SCRIPT_SHA256=e196fdf0cc8b325180154535f843034dc0ae9eb43ead9980adfcfc57c58069b2
WINTUN_VERSION=0.14.1
WINTUN_SHA256=07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51

fetch() { # url sha256 dest
	curl -fsSL "$1" -o "$3"
	echo "$2  $3" | sha256sum -c -
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$OUT"

fetch "https://www.infradead.org/openconnect/download/openconnect-$OPENCONNECT_VERSION.tar.gz" \
	"$OPENCONNECT_SHA256" "$work/openconnect.tar.gz"
tar -xzf "$work/openconnect.tar.gz" -C "$work"
# Pinned copy; also stops the Makefile from fetching the latest one itself.
fetch "https://gitlab.com/openconnect/vpnc-scripts/-/raw/$VPNC_SCRIPTS_COMMIT/vpnc-script-win.js" \
	"$VPNC_SCRIPT_SHA256" "$work/openconnect-$OPENCONNECT_VERSION/vpnc-script-win.js"
(
	cd "$work/openconnect-$OPENCONNECT_VERSION"
	# Newer mingw-w64 dropped sec_api/; <stdlib.h> already declares what it gave.
	sed -i 's|^#include <sec_api/stdlib_s.h>.*||' compat.c
	./configure --prefix="$work/install" \
		--with-gnutls \
		--with-vpnc-script=vpnc-script-win.js \
		--disable-nls \
		--disable-docs \
		--without-gssapi --without-libproxy --without-stoken \
		--without-libpskc --without-libpcsclite --without-tss2
	make -j"$(nproc)"
	make install
)

cp "$work/install/sbin/openconnect.exe" "$work/install/bin/"libopenconnect*.dll "$OUT/"
# Every MSYS2 DLL the executable and libopenconnect pull in.
ldd "$OUT/openconnect.exe" "$OUT/"libopenconnect*.dll \
	| awk -v p="$MINGW_PREFIX/" 'index($3, p) == 1 {print $3}' | sort -u \
	| while read -r dll; do cp "$dll" "$OUT/"; done

cp "$work/openconnect-$OPENCONNECT_VERSION/vpnc-script-win.js" "$OUT/"

fetch "https://www.wintun.net/builds/wintun-$WINTUN_VERSION.zip" "$WINTUN_SHA256" "$work/wintun.zip"
unzip -q -o -j "$work/wintun.zip" 'wintun/bin/amd64/wintun.dll' -d "$OUT"

"$OUT/openconnect.exe" --version | head -3
ls "$OUT"
