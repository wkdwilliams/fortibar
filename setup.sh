#!/bin/bash
# One-time setup for FortiBar. Builds openfortivpn and the app inside this
# folder, then installs the root-owned helper into /usr/local/libexec/fortibar
# plus a sudoers rule (/etc/sudoers.d/fortibar) so the app can start the VPN
# without asking for your password each time.
#
#   ./setup.sh          build openfortivpn + the app, install sudoers rule
#   ./setup.sh --app    rebuild just the app
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
VENDOR="$ROOT/vendor"
BIN=/usr/local/libexec/fortibar
export CARGO_HOME="$ROOT/.cargo"   # crate downloads stay in this folder
export CARGO_TARGET_DIR="$ROOT/target"
# openfortivpn ends up running as root, so build a known commit rather than
# whatever upstream HEAD is today. Bump deliberately, after reviewing changes.
OFV_REPO=https://github.com/adrienverge/openfortivpn.git
OFV_COMMIT=49e7b8c3881a849751aa67fc210ed5bd7054d6d5
# Recorded with the commit, so changing how it's built forces a rebuild.
OFV_BUILD="$OFV_COMMIT static-openssl"

build_openfortivpn() {
	local src="$VENDOR/openfortivpn-src"
	local openssl
	openssl="$(brew --prefix openssl@3 2>/dev/null || true)"
	if [[ -z "$openssl" || ! -d "$openssl" ]]; then
		echo "openssl@3 not found. Install it with: brew install openssl@3" >&2
		exit 1
	fi
	if [[ "$(git -C "$src" rev-parse HEAD 2>/dev/null)" != "$OFV_COMMIT" ]]; then
		rm -rf "$src"
		git init -q "$src"
		git -C "$src" fetch -q --depth 1 "$OFV_REPO" "$OFV_COMMIT"
		git -C "$src" checkout -q FETCH_HEAD
	fi
	if [[ "$(git -C "$src" rev-parse HEAD)" != "$OFV_COMMIT" ]]; then
		echo "openfortivpn source isn't at the pinned commit $OFV_COMMIT" >&2
		exit 1
	fi
	(
		cd "$src"
		./autogen.sh
		# OpenSSL is linked in statically. openfortivpn runs as root without a
		# password prompt, and Homebrew's dylibs are writable by this user, so
		# loading them would let anything running as this user run code as root.
		OPENSSL_CFLAGS="-I$openssl/include" \
		OPENSSL_LIBS="$openssl/lib/libssl.a $openssl/lib/libcrypto.a" \
		./configure \
			--prefix="$VENDOR/openfortivpn" \
			--sysconfdir="$VENDOR/openfortivpn/etc" \
			--with-pppd=/usr/sbin/pppd \
			--enable-legacy-pppd \
			--disable-resolvconf
		make -j"$(sysctl -n hw.ncpu)"
		make install
	)
	echo "$OFV_BUILD" >"$VENDOR/openfortivpn/.commit"
}

# The sudoers rule trusts $BIN/fortivpn-helper by path, so every folder above
# it must be root-owned and writable only by root. Otherwise this user could
# move $BIN aside and put their own helper in its place.
check_bin_parents() {
	local d owner mode
	d="$(dirname "$BIN")"
	while :; do
		if [[ -e "$d" ]]; then
			read -r owner mode < <(stat -f '%u %Lp' "$d")
			if [[ "$owner" != 0 || $(( 8#$mode & 8#022 )) -ne 0 ]]; then
				echo "$d must be owned by root and not writable by group or others" \
					"(it's owned by uid $owner, mode $mode). Fix it, then re-run setup.sh." >&2
				exit 1
			fi
		fi
		[[ "$d" == / ]] && break
		d="$(dirname "$d")"
	done
}

# Anything the root-run binary loads from outside the OS could be swapped by
# this user. Only /usr/lib and /System are protected (by SIP).
check_no_user_libs() {
	local bad
	bad="$(otool -L "$1" | tail -n +2 | awk '{print $1}' | grep -v -E '^(/usr/lib/|/System/)' || true)"
	if [[ -n "$bad" ]]; then
		echo "$1 links libraries outside the OS, which must not run as root:" >&2
		echo "$bad" >&2
		exit 1
	fi
}

install_helper() {
	check_bin_parents
	check_no_user_libs "$VENDOR/openfortivpn/bin/openfortivpn"
	echo "Installing root-owned helper into $BIN (needs your password)…"
	sudo mkdir -p "$BIN"
	# Older versions installed it inside the project folder.
	sudo rm -rf "$VENDOR/bin"
	sudo install -o root -g wheel -m 755 "$VENDOR/openfortivpn/bin/openfortivpn" "$BIN/openfortivpn"
	sudo install -o root -g wheel -m 755 "$ROOT/scripts/fortivpn-helper.sh" "$BIN/fortivpn-helper"
	sudo chown root:wheel "$BIN"
	sudo chmod 755 "$BIN"

	local rule="$USER ALL=(root) NOPASSWD: $BIN/fortivpn-helper"
	local tmp
	tmp="$(mktemp)"
	echo "$rule" >"$tmp"
	sudo visudo -cf "$tmp" >/dev/null
	sudo install -o root -g wheel -m 440 "$tmp" /etc/sudoers.d/fortibar
	rm -f "$tmp"
	echo "sudoers rule installed: $rule"
}

build_app() {
	cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml"
	local app="$ROOT/dist/FortiBar.app"
	rm -rf "$app"
	mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
	cp "$ROOT/target/release/fortibar" "$app/Contents/MacOS/FortiBar"
	# MCP server for AI agents; it finds the app it sits in.
	cp "$ROOT/target/release/fortibar-mcp" "$app/Contents/MacOS/fortibar-mcp"
	cp "$ROOT/assets/FortiBar.icns" "$app/Contents/Resources/FortiBar.icns"
	cat >"$app/Contents/Info.plist" <<-EOF
	<?xml version="1.0" encoding="UTF-8"?>
	<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
	<plist version="1.0"><dict>
	  <key>CFBundleName</key><string>FortiBar</string>
	  <key>CFBundleIdentifier</key><string>local.fortibar</string>
	  <key>CFBundleExecutable</key><string>FortiBar</string>
	  <key>CFBundleIconFile</key><string>FortiBar</string>
	  <key>CFBundlePackageType</key><string>APPL</string>
	  <key>CFBundleVersion</key><string>0.1.0</string>
	  <key>LSUIElement</key><true/>
	  <!-- Without this, ATS fails sign-in to a gateway whose certificate macOS
	       rejects, even after our pinned-certificate check (src/tls.rs) accepts it. -->
	  <key>NSAppTransportSecurity</key><dict><key>NSAllowsArbitraryLoadsInWebContent</key><true/></dict>
	  <key>LSMinimumSystemVersion</key><string>14.0</string>
	</dict></plist>
	EOF
	# Nested code has to be signed before the bundle around it.
	codesign --force --sign - "$app/Contents/MacOS/fortibar-mcp" >/dev/null 2>&1 || true
	codesign --force --sign - "$app" >/dev/null 2>&1 || true
	echo "Built $app"
}

if [[ "${1:-}" == "--app" ]]; then
	build_app
	exit 0
fi

# Rebuilt when missing or built from a different commit than the pin.
if [[ ! -x "$VENDOR/openfortivpn/bin/openfortivpn" \
	|| "$(cat "$VENDOR/openfortivpn/.commit" 2>/dev/null)" != "$OFV_BUILD" ]]; then
	build_openfortivpn
fi
install_helper
build_app
echo
if [[ -z "${MAKELEVEL:-}" ]]; then
	echo "Done. Start it with: open \"$ROOT/dist/FortiBar.app\""
fi
