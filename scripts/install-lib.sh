# Installs the root-owned helper and its sudoers rule. Sourced by setup.sh and
# by install.sh in the prebuilt package (./setup.sh --package); not run alone.

BIN=/usr/local/libexec/fortibar

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
					"(it's owned by uid $owner, mode $mode). Fix it, then run this again." >&2
				exit 1
			fi
		fi
		[[ "$d" == / ]] && break
		d="$(dirname "$d")"
	done
}

# install_helper <openfortivpn binary> <helper script>
install_helper() {
	local ofv="$1" helper="$2"
	check_bin_parents
	echo "Installing root-owned helper into $BIN (needs your password)…"
	sudo mkdir -p "$BIN"
	sudo install -o root -g wheel -m 755 "$ofv" "$BIN/openfortivpn"
	sudo install -o root -g wheel -m 755 "$helper" "$BIN/fortivpn-helper"
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
