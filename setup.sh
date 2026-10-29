#!/bin/bash
# One-time setup for FortiBar. Everything lands inside this folder except the
# sudoers rule (/etc/sudoers.d/fortibar), which lets the app start the VPN
# without asking for your password each time.
#
#   ./setup.sh          build openfortivpn + the app, install sudoers rule
#   ./setup.sh --app    rebuild just the app
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
VENDOR="$ROOT/vendor"
BIN="$VENDOR/bin"
export CARGO_HOME="$ROOT/.cargo"   # crate downloads stay in this folder
export CARGO_TARGET_DIR="$ROOT/target"

build_openfortivpn() {
	local src="$VENDOR/openfortivpn-src"
	local openssl
	openssl="$(brew --prefix openssl@3 2>/dev/null || true)"
	if [[ -z "$openssl" || ! -d "$openssl" ]]; then
		echo "openssl@3 not found. Install it with: brew install openssl@3" >&2
		exit 1
	fi
	if [[ ! -d "$src" ]]; then
		git clone --depth 1 https://github.com/adrienverge/openfortivpn.git "$src"
	fi
	(
		cd "$src"
		./autogen.sh
		PKG_CONFIG_PATH="$openssl/lib/pkgconfig" ./configure \
			--prefix="$VENDOR/openfortivpn" \
			--sysconfdir="$VENDOR/openfortivpn/etc" \
			--with-pppd=/usr/sbin/pppd \
			--enable-legacy-pppd \
			--disable-resolvconf
		make -j"$(sysctl -n hw.ncpu)"
		make install
	)
}

install_helper() {
	echo "Installing root-owned helper into $BIN (needs your password)…"
	sudo mkdir -p "$BIN"
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
	cargo build --release --manifest-path "$ROOT/Cargo.toml"
	local app="$ROOT/dist/FortiBar.app"
	rm -rf "$app"
	mkdir -p "$app/Contents/MacOS"
	cp "$ROOT/target/release/fortibar" "$app/Contents/MacOS/FortiBar"
	cat >"$app/Contents/Info.plist" <<-EOF
	<?xml version="1.0" encoding="UTF-8"?>
	<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
	<plist version="1.0"><dict>
	  <key>CFBundleName</key><string>FortiBar</string>
	  <key>CFBundleIdentifier</key><string>local.fortibar</string>
	  <key>CFBundleExecutable</key><string>FortiBar</string>
	  <key>CFBundlePackageType</key><string>APPL</string>
	  <key>CFBundleVersion</key><string>0.1.0</string>
	  <key>LSUIElement</key><true/>
	  <key>LSMinimumSystemVersion</key><string>14.0</string>
	</dict></plist>
	EOF
	codesign --force --sign - "$app" >/dev/null 2>&1 || true
	echo "Built $app"
}

if [[ "${1:-}" == "--app" ]]; then
	build_app
	exit 0
fi

if [[ ! -x "$VENDOR/openfortivpn/bin/openfortivpn" ]]; then
	build_openfortivpn
fi
install_helper
build_app
echo
echo "Done. Start it with: open \"$ROOT/dist/FortiBar.app\""
