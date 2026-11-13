# FortiBar

## Keep-alive and auto-reconnect (built 2026-11-06)

### Why

One client's VPN (password login) dropped every few minutes. We monitored it
on 2026-11-06:

- With no traffic, the gateway closed the tunnel after 5–6 minutes. openfortivpn
  logged "Cancelling threads…" first, then pppd "Modem hangup". So the gateway
  closed the connection; there was no sleep or network change on the Mac. This
  fits FortiGate's default SSL-VPN idle timeout of 300s.
- After reconnecting (about 2s with the saved password), pinging an internal
  host every 10s kept it up for 18+ min with no drop.

The client OK'd working around the idle timeout with a keep-alive.

### What was built

1. **Keep-alive.** `Vpn.keepalive_host` (`src/config.rs`, validated by
   `parse_keepalive_host`). While Connected, `Tunnel::keep_alive`
   (`src/tunnel.rs`) pings it every `KEEPALIVE_INTERVAL` (60s) until the
   tunnel exits. It only logs changes between replying and not.
2. **Auto-reconnect.** `Vpn.auto_reconnect`. In `src/main.rs`:
   - A drop from Connected goes to `Status::Reconnecting` and
     `schedule_reconnect`, which waits `RECONNECT_DELAYS` (2/5/15/30/60s) and
     gives up after `MAX_RECONNECTS` (10).
   - A failed automatic attempt (`App::reconnecting`) schedules the next one
     instead of showing an alert.
   - Any user action (`connect`, `disconnect`, cancelling a sign-in or
     password window) calls `stop_reconnecting`.
   - A wrong password, a cert prompt or a sudo failure is never retried.
   - Uptime of at least `STABLE_AFTER` (60s) resets the attempt count.
   - The log is kept across automatic reconnects, so it still shows the drop.
   - `status_json` reports `reconnecting` and `reconnect_attempt`.

Both are set in Edit VPN (`src/form.html`).

### Tested (macOS, 2026-11-06)

- Keep-alive on (60s): stayed up for 12+ min with only the pings in the tunnel
  (1 packet out and 1 in per minute), every one answered.
- Keep-alive off, auto-reconnect on: the gateway dropped it after 5.0 min idle.
  FortiBar reconnected with the saved password in about 4s.

Not tested yet: Windows (not built or run there), SSO VPNs reconnecting,
giving up after 10 attempts.

## Back burner

- Connecting to several VPNs at once. It needs a per-VPN DNS key in
  `scripts/fortivpn-helper.sh` and per-VPN status/tunnel state in `main.rs`.
  Full-tunnel VPNs, and VPNs with overlapping internal ranges, can't be
  combined.

## Security review (2026-11-13)

Fixed:

- openfortivpn links OpenSSL statically. Homebrew's dylibs are user-writable,
  and the NOPASSWD sudo rule made loading them a passwordless root.
  `setup.sh` (`check_no_user_libs`, `check_bin_parents`) refuses to install
  otherwise.
- Saved Microsoft session cookies are encrypted (`src/seal.rs`, key in the
  Keychain/Credential Manager as `FortiBar`/`file-key`). On startup,
  `seal::migrate_dir` encrypts any unencrypted `.cookies` left by older builds.
- A pinned cert that changes gets a warning dialog (`confirm_risky`), not the
  normal trust prompt.
- The sign-in webview only navigates to https/about:. The SAML callback and the
  `SVPNCOOKIE` domain are matched exactly.
- The control API caps connections at 8 and limits each request to 5s in total.
- CI: actions pinned by SHA, `contents: write` only in the tag-only release
  job, and a `.sha256` file published with the zip.

Accepted, documented in the README: the sudo rule lets any program running as
the user start the helper against any gateway, and gateway-pushed DNS domains
are used as is.

## Notes

- This repo is public: keep client names, gateway addresses, usernames and
  internal hosts out of code, tests, docs and this file.
- The tunnel log lives only in memory (`self.log`). `fortibar.log` is written
  only when you click Show Log. The control API `logs` command (see
  `src/control.rs`, port/token in `<data>/mcp.json`) reads it live.
- A helper change only takes effect after re-running `./setup.sh`, because the
  helper is installed into `/usr/local/libexec/fortibar`.
- Releases: pushing a `v*` tag makes `.github/workflows/windows.yml` build
  `FortiBar-windows-x64.zip` and attach it to a GitHub Release. There's no
  macOS release asset; Mac users clone and run `make`.
