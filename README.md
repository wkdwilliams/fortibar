# FortiBar

A small menu bar / system tray client for FortiGate SSL-VPN, with Microsoft SSO (SAML)
or a plain username and password.

Every VPN you add gets its own isolated browser profile, so each one remembers
its own Microsoft login. That holds even when several VPNs share one gateway
with different accounts. Switching VPNs normally needs no email, password or OTP.

VPNs without SSO log in with a username and password instead. Each VPN keeps its own,
so several accounts work side by side, even on the same gateway.

## macOS

### Build and install

You need macOS 14 or later, plus:

- Xcode Command Line Tools: `xcode-select --install`
- [Homebrew](https://brew.sh) packages: `brew install openssl@3 autoconf automake pkg-config`
- [Rust](https://rustup.rs)

Then:

```sh
git clone https://github.com/wkdwilliams/fortibar.git
cd fortibar
./setup.sh
open dist/FortiBar.app
```

`setup.sh` asks for your password once, for the steps that need root. It:
- clones and builds [openfortivpn](https://github.com/adrienverge/openfortivpn) into `vendor/` (skipped if it's already built)
- installs `fortivpn-helper` + `openfortivpn` root-owned into `/usr/local/libexec/fortibar/` and adds
  `/etc/sudoers.d/fortibar` so the app can start the tunnel without a password prompt
- builds the app into `dist/FortiBar.app` (crates download into `.cargo/`, build output into `target/`)

The app doesn't depend on the project folder, so you can copy `dist/FortiBar.app` to `/Applications`.
Copy it again after rebuilding.

After changing only the Rust code, `./setup.sh --app` rebuilds just the app.

To start it at login, tick **Start at Login** in the FortiBar menu.

Run `./setup.sh` again after changing `scripts/fortivpn-helper.sh`. The app runs the root-owned
copy in `/usr/local/libexec/fortibar/`, not the script in the project.

### Uninstall

```sh
./uninstall.sh   # removes the sudoers rule, root-owned helper, saved VPNs, WebKit profiles and saved passwords
```

Then delete the project folder.

## Windows

### Install

1. Download `FortiBar-windows-x64.zip` from **Actions → Windows build → Artifacts** for the latest commit
   (you need to be signed in to GitHub), or build it yourself (below).
2. Extract it anywhere, e.g. `C:\Tools\FortiBar`.
3. Run `FortiBar.exe`. Windows asks for admin rights once at startup, because creating the
   tunnel adapter, routes and DNS needs them.

Requires Windows 10/11 with the WebView2 runtime (preinstalled on current Windows).

To start it at login without a UAC prompt each time, create a Task Scheduler task:
trigger "At log on", action `FortiBar.exe`, and tick "Run with highest privileges".

To uninstall, quit FortiBar and delete the folder.

### Build it yourself

GitHub Actions builds the zip on every push (`.github/workflows/windows.yml`).
To build locally you need [Rust](https://rustup.rs) and [MSYS2](https://www.msys2.org):

```powershell
C:\msys64\usr\bin\bash -lc "pacman -S --needed make unzip mingw-w64-ucrt-x86_64-{gcc,pkgconf,gnutls,libxml2,zlib,lz4}"
.\scripts\build-windows.ps1
```

This builds OpenConnect 9.21 from its release tarball and bundles it with wintun. Every download is checked against a pinned SHA-256.

## Use

- **Click a VPN** to connect. If you're already connected somewhere else, it disconnects first and then switches.
- **Click the connected VPN** again to disconnect.
- On the first sign-in a window opens for the Microsoft login. Choose **Stay signed in** if you're asked.
  After that, sign-in usually finishes in the background and the window never appears.
- For a VPN with SSO turned off, a window asks for the username and password. With **Save login**
  the password is kept in the macOS Keychain (Windows: Credential Manager) after the first
  successful connect, so later connects don't ask. With **Prompt on login** it asks every time.
  If the gateway rejects a saved password, it is deleted and you're asked again.
- **Forget Saved Login** clears one VPN's browser profile, or its saved password.
- **Show Log** opens the last tunnel output.

## How it works

For SSO VPNs:

1. A hidden browser window (one isolated profile per VPN: WebKit on macOS, WebView2 on Windows) opens
   `https://<gateway>/remote/saml/start?redirect=1`. If Microsoft still has a valid session, the SAML flow completes on its own.
   If it hasn't finished after 4 seconds, the window is shown.
2. The FortiGate `SVPNCOOKIE` is read from the profile. If the gateway redirects to `127.0.0.1:8020/?id=…`, the app exchanges the id for the cookie first.
3. The cookie is passed over stdin to the tunnel:
   - **macOS:** `sudo fortivpn-helper` runs `openfortivpn --cookie-on-stdin`, then sets DNS with `scutil`.
     Domains the gateway pushes use split DNS. If it pushes none, all lookups go through the VPN. DNS is restored on disconnect.
   - **Windows:** `openconnect --protocol=fortinet --cookie-on-stdin` with wintun. Its vpnc script sets routes and DNS.

For password VPNs there's no browser step. The username and password go over stdin
(never the command line) to `openfortivpn --username` on macOS, or `openconnect --user --passwd-on-stdin`
on Windows. Tunnel setup and DNS work the same as above.

### Gateways with untrusted certificates

If the tunnel rejects the gateway certificate, FortiBar asks whether to trust it and saves the fingerprint for that VPN.
On macOS the sign-in window also accepts a certificate that matches the saved fingerprint, which covers gateways
reached by IP address. On Windows the sign-in window doesn't do this yet.

## Files

Data lives in `~/Library/Application Support/FortiBar/` on macOS, and in `data\` next to `FortiBar.exe` on Windows.

| Path | What |
|---|---|
| `<data>/vpns.json` | your VPN list |
| `<data>/<id>.cookies` | Microsoft session cookies, so logins survive app restarts |
| `<data>/fortibar.log` | last tunnel output, written by **Show Log** |
| `<data>/profiles/` | per-VPN WebView2 profiles (Windows) |
| Keychain / Credential Manager, service `FortiBar`, account `<id>` | saved passwords for password VPNs |
| `/usr/local/libexec/fortibar/` | root-owned helper and openfortivpn (macOS) |
| `vendor/` | openfortivpn source and build (macOS) |
| `.cargo/`, `target/`, `dist/` | Rust crates, build output, packaged app |

Older macOS builds kept data in the project's `data/` folder; it moves to Application Support the first time the app starts.
