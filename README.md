# FortiBar

A small menu bar client for FortiGate SSL-VPN with Microsoft SSO (SAML).
Each VPN has its own browser profile, so the Microsoft login for each hospital
is remembered. Switching VPNs normally needs no email, password or OTP.

## Setup (once)

```sh
./setup.sh
open dist/FortiBar.app
```

`setup.sh`:
- builds [openfortivpn](https://github.com/adrienverge/openfortivpn) into `vendor/` (needs `brew install openssl@3 autoconf automake pkg-config`)
- installs `vendor/bin/fortivpn-helper` + `openfortivpn` root-owned and adds
  `/etc/sudoers.d/fortibar` so the app can start the tunnel without a password prompt
- builds the app into `dist/FortiBar.app` (crates download into `.cargo/`, build output into `target/`)

To start it at login: System Settings → General → Login Items → add `dist/FortiBar.app`.

## Use

- **Click a VPN** to connect. If you're already connected somewhere else, it disconnects first and then switches.
- **Click the connected VPN** again to disconnect.
- On the first sign-in a window opens for the Microsoft login. Choose **Stay signed in** if you're asked.
  After that, sign-in usually finishes in the background and the window never appears.
- **Forget Saved Login** clears one VPN's browser profile.
- **Show Log** opens the last openfortivpn output.

## How it works

1. A hidden WebKit window (one isolated profile per VPN) opens `https://<gateway>/remote/saml/start?redirect=1`.
   If Microsoft still has a valid session, the SAML flow completes on its own. If it hasn't finished after 4 seconds, the window is shown.
2. The FortiGate `SVPNCOOKIE` is read from the profile. If the gateway redirects to `127.0.0.1:8020/?id=…`, the app exchanges the id for the cookie first.
3. `sudo fortivpn-helper` runs `openfortivpn --cookie-on-stdin`, then sets macOS DNS with `scutil`:
   split DNS for the domains the gateway pushes, or all lookups go through the VPN if it pushes none.
   DNS is restored on disconnect.

## Files

| Path | What |
|---|---|
| `data/vpns.json` | your VPN list |
| `data/<id>.cookies` | Microsoft session cookies, so logins survive app restarts (mode 600) |
| `vendor/` | openfortivpn source, build, and the root-owned helper |
| `.cargo/`, `target/`, `dist/` | Rust crates, build output, app bundle |

Outside this folder there's only `/etc/sudoers.d/fortibar` and WebKit's profile
data in `~/Library/WebKit/`.

## Uninstall

```sh
./uninstall.sh   # removes the sudoers rule, root-owned helper and WebKit profiles
cd .. && rm -rf forti-clone
```
