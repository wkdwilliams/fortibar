# FortiBar

A small menu bar / system tray client for FortiGate SSL-VPN with Microsoft SSO (SAML).

Every VPN you add gets its own isolated browser profile, so each one remembers
its own Microsoft login. That holds even when several VPNs share one gateway
with different accounts. Switching VPNs normally needs no email, password or OTP.

## macOS

### Setup (once)

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

### Uninstall

```sh
./uninstall.sh   # removes the sudoers rule, root-owned helper and WebKit profiles
cd .. && rm -rf forti-clone
```

## Windows

### Install

1. Download `FortiBar-windows-x64.zip`. It's on the repo's
   [Releases](../../releases) page, or under **Actions → Windows build → Artifacts** for the latest commit.
2. Extract it anywhere, e.g. `C:\Tools\FortiBar`.
3. Run `FortiBar.exe`. Windows asks for admin rights once at startup, because creating the
   tunnel adapter, routes and DNS needs them.

Requires Windows 10/11 with the WebView2 runtime (preinstalled on current Windows).

To start it at login without a UAC prompt each time, create a Task Scheduler task:
trigger "At log on", action `FortiBar.exe`, and tick "Run with highest privileges".

To uninstall, quit FortiBar and delete the folder.

### Build it yourself

GitHub Actions builds the zip on every push (`.github/workflows/windows.yml`).
Tag a commit `v*` to attach it to a release. To build locally you need
[Rust](https://rustup.rs) and [MSYS2](https://www.msys2.org):

```powershell
C:\msys64\usr\bin\pacman -S mingw-w64-x86_64-openconnect unzip
.\scripts\build-windows.ps1
```

## Use

- **Click a VPN** to connect. If you're already connected somewhere else, it disconnects first and then switches.
- **Click the connected VPN** again to disconnect.
- On the first sign-in a window opens for the Microsoft login. Choose **Stay signed in** if you're asked.
  After that, sign-in usually finishes in the background and the window never appears.
- **Forget Saved Login** clears one VPN's browser profile.
- **Show Log** opens the last tunnel output.

## How it works

1. A hidden browser window (one isolated profile per VPN: WebKit on macOS, WebView2 on Windows) opens
   `https://<gateway>/remote/saml/start?redirect=1`. If Microsoft still has a valid session, the SAML flow completes on its own.
   If it hasn't finished after 4 seconds, the window is shown.
2. The FortiGate `SVPNCOOKIE` is read from the profile. If the gateway redirects to `127.0.0.1:8020/?id=…`, the app exchanges the id for the cookie first.
3. The cookie is passed over stdin to the tunnel:
   - **macOS:** `sudo fortivpn-helper` runs `openfortivpn --cookie-on-stdin`, then sets DNS with `scutil`.
     Domains the gateway pushes use split DNS. If it pushes none, all lookups go through the VPN. DNS is restored on disconnect.
   - **Windows:** `openconnect --protocol=fortinet --cookie-on-stdin` with wintun. Its vpnc script sets routes and DNS.

### Gateways with untrusted certificates

If the tunnel rejects the gateway certificate, FortiBar asks whether to trust it and saves the fingerprint for that VPN.
On macOS the sign-in window also accepts a certificate that matches the saved fingerprint, which covers gateways
reached by IP address. On Windows the sign-in window doesn't do this yet.

## Files

| Path | What |
|---|---|
| `data/vpns.json` | your VPN list |
| `data/<id>.cookies` | Microsoft session cookies, so logins survive app restarts |
| `data/profiles/` | per-VPN WebView2 profiles (Windows) |
| `vendor/` | openfortivpn source, build, and the root-owned helper (macOS) |
| `.cargo/`, `target/`, `dist/` | Rust crates, build output, packaged app |

On macOS, `data/` lives in the project folder. On Windows it sits next to `FortiBar.exe`.
