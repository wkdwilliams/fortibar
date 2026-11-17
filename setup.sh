#!/bin/bash
# One-time setup for FortiBar. Builds openfortivpn and the app inside this
# folder, then installs the root-owned helper into /usr/local/libexec/fortibar
# plus a sudoers rule (/etc/sudoers.d/fortibar) so the app can start the VPN
# without asking for your password each time.
#
#   ./setup.sh            build openfortivpn + the app, install sudoers rule
#   ./setup.sh --app      rebuild just the app
#   ./setup.sh --package  build a zip for another Apple Silicon Mac into
#                         dist/package (installs nothing here); the other
#                         Mac unzips it and runs: bash install.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
VENDOR="$ROOT/vendor"
# shellcheck source=scripts/install-lib.sh
source "$ROOT/scripts/install-lib.sh"   # sets BIN
export CARGO_HOME="$ROOT/.cargo"   # crate downloads stay in this folder
export CARGO_TARGET_DIR="$ROOT/target"
# Oldest macOS the app and openfortivpn run on. Also LSMinimumSystemVersion
# below. Before macOS 14, every VPN shares one sign-in browser profile.
MACOS_MIN=12.0
export MACOSX_DEPLOYMENT_TARGET="$MACOS_MIN"
# openfortivpn ends up running as root, so build a known commit rather than
# whatever upstream HEAD is today. Bump deliberately, after reviewing changes.
OFV_REPO=https://github.com/adrienverge/openfortivpn.git
OFV_COMMIT=49e7b8c3881a849751aa67fc210ed5bd7054d6d5
# Recorded with the commit, so changing how it's built forces a rebuild.
OFV_BUILD="$OFV_COMMIT static-openssl"
# OpenSSL for --package, built from source (checksum from Homebrew's formula).
OPENSSL_VERSION=3.5.9
OPENSSL_SHA256=603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a

# build_openssl <work dir>: static libs end up in <work dir>/install$BIN/openssl.
# Its config, CA certificates and modules are looked up under $BIN, which is
# root-owned. A build whose OPENSSLDIR the user can write to would let them
# load code into openfortivpn, which runs as root.
build_openssl() {
	local work="$1"
	local tgz="$work/openssl-$OPENSSL_VERSION.tar.gz"
	mkdir -p "$work"
	curl -fsSL -o "$tgz" \
		"https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL_VERSION/openssl-$OPENSSL_VERSION.tar.gz"
	echo "$OPENSSL_SHA256  $tgz" | shasum -a 256 -c - >/dev/null
	tar -xzf "$tgz" -C "$work"
	(
		cd "$work/openssl-$OPENSSL_VERSION"
		./Configure darwin64-arm64-cc no-shared no-module no-tests \
			--prefix="$BIN/openssl" --openssldir="$BIN/ssl" \
			-mmacosx-version-min="$MACOS_MIN"
		make -j"$(sysctl -n hw.ncpu)" build_libs
		make install_dev DESTDIR="$work/install"
	)
}

# build_openfortivpn <source dir> <openssl prefix> <install prefix> [destdir]
build_openfortivpn() {
	local src="$1" openssl="$2" prefix="$3" destdir="${4:-}"
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
			--prefix="$prefix" \
			--sysconfdir="$prefix/etc" \
			--with-pppd=/usr/sbin/pppd \
			--enable-legacy-pppd \
			--disable-resolvconf
		make -j"$(sysctl -n hw.ncpu)"
		make install DESTDIR="$destdir"
	)
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

# build_app [bundle path]
build_app() {
	local app="${1:-$ROOT/dist/FortiBar.app}"
	# Not with MACOSX_DEPLOYMENT_TARGET: proc-macro dylibs built for 12.0 fail
	# to load in rustc on recent macOS ("mis-aligned LINKEDIT string pool").
	# Rust's own default for Apple Silicon is 11.0, below MACOS_MIN anyway.
	env -u MACOSX_DEPLOYMENT_TARGET cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml"
	rm -rf "$app"
	mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
	cp "$CARGO_TARGET_DIR/release/fortibar" "$app/Contents/MacOS/FortiBar"
	# MCP server for AI agents; it finds the app it sits in.
	cp "$CARGO_TARGET_DIR/release/fortibar-mcp" "$app/Contents/MacOS/fortibar-mcp"
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
	  <key>LSMinimumSystemVersion</key><string>$MACOS_MIN</string>
	</dict></plist>
	EOF
	# Nested code has to be signed before the bundle around it.
	codesign --force --sign - "$app/Contents/MacOS/fortibar-mcp" >/dev/null 2>&1 || true
	codesign --force --sign - "$app" >/dev/null 2>&1 || true
	echo "Built $app"
}

# Everything another Mac needs, in dist/package/FortiBar-macos-arm64.zip.
package() {
	if [[ "$(uname -m)" != arm64 ]]; then
		echo "--package builds for Apple Silicon, so run it on an Apple Silicon Mac." >&2
		exit 1
	fi
	local out="$ROOT/dist/package"
	local build="$out/build" stage="$out/FortiBar" zip=FortiBar-macos-arm64.zip
	rm -rf "$out"
	mkdir -p "$build" "$stage"
	build_openssl "$build/openssl"
	build_openfortivpn "$build/openfortivpn-src" "$build/openssl/install$BIN/openssl" "$BIN" "$build/openfortivpn"
	local ofv="$build/openfortivpn$BIN/bin/openfortivpn"
	check_no_user_libs "$ofv"
	# -S drops the debug map, which names the build folder.
	install -m 755 "$ofv" "$stage/openfortivpn"
	strip -S "$stage/openfortivpn"
	install -m 755 "$ROOT/scripts/fortivpn-helper.sh" "$stage/fortivpn-helper"
	install -m 755 "$ROOT/scripts/install-prebuilt.sh" "$stage/install.sh"
	install -m 644 "$ROOT/scripts/install-lib.sh" "$stage/install-lib.sh"
	install -m 755 "$ROOT/uninstall.sh" "$stage/uninstall.sh"
	# Rust embeds source paths (for panic messages). Don't ship this Mac's
	# home folder or project path. A separate target dir, since changing
	# RUSTFLAGS would otherwise make the next plain build start over.
	export CARGO_TARGET_DIR="$ROOT/target/package"
	# Unit-separated rather than RUSTFLAGS, so paths with spaces work.
	export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=~"$'\x1f'"--remap-path-prefix=$ROOT=fortibar"
	build_app "$stage/FortiBar.app"
	if grep -rqF "$HOME" "$stage"; then
		echo "The package still contains $HOME, so it wasn't zipped." >&2
		exit 1
	fi
	# No extended attributes: they'd unzip as stray ._ files.
	(cd "$out" && ditto -c -k --norsrc --noextattr --keepParent FortiBar "$zip" \
		&& shasum -a 256 "$zip" | tee "$zip.sha256")
	echo "Built $out/$zip"
}

case "${1:-}" in
	--app) build_app; exit 0 ;;
	--package) package; exit 0 ;;
esac

# Rebuilt when missing or built from a different commit than the pin.
if [[ ! -x "$VENDOR/openfortivpn/bin/openfortivpn" \
	|| "$(cat "$VENDOR/openfortivpn/.commit" 2>/dev/null)" != "$OFV_BUILD" ]]; then
	openssl="$(brew --prefix openssl@3 2>/dev/null || true)"
	if [[ -z "$openssl" || ! -d "$openssl" ]]; then
		echo "openssl@3 not found. Install it with: brew install openssl@3" >&2
		exit 1
	fi
	build_openfortivpn "$VENDOR/openfortivpn-src" "$openssl" "$VENDOR/openfortivpn"
	echo "$OFV_BUILD" >"$VENDOR/openfortivpn/.commit"
fi
check_no_user_libs "$VENDOR/openfortivpn/bin/openfortivpn"
# Older versions installed it inside the project folder.
sudo rm -rf "$VENDOR/bin"
install_helper "$VENDOR/openfortivpn/bin/openfortivpn" "$ROOT/scripts/fortivpn-helper.sh"
build_app
echo
if [[ -z "${MAKELEVEL:-}" ]]; then
	echo "Done. Start it with: open \"$ROOT/dist/FortiBar.app\""
fi
