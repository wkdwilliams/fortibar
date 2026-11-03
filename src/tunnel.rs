use crate::config::{tunnel_program, Vpn};
use crate::UserEvent;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

/// Keeps console programs (openconnect, taskkill) from flashing a window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A running tunnel process: `sudo fortivpn-helper` (openfortivpn) on macOS,
/// `openconnect --protocol=fortinet` on Windows. Output lines and exit are
/// reported back to the event loop tagged with `session`, so events from an
/// old tunnel can't be mistaken for the current one.
pub struct Tunnel {
    pub vpn_id: String,
    pub session: u64,
    pid: u32,
    exited: Arc<AtomicBool>,
}

/// How the tunnel logs in. Either way the secret goes over stdin so it never
/// shows up in the process list.
pub enum Auth {
    /// FortiGate session cookie from SAML sign-in.
    Cookie(String),
    Password { username: String, password: String },
}

#[cfg(target_os = "macos")]
fn command(vpn: &Vpn, _auth: &Auth) -> Command {
    let mut cmd = Command::new("/usr/bin/sudo");
    cmd.arg("-n").arg(tunnel_program()).arg(vpn.host_port());
    if !vpn.trusted_cert.is_empty() {
        cmd.arg(&vpn.trusted_cert);
    }
    cmd
}

#[cfg(windows)]
fn command(vpn: &Vpn, auth: &Auth) -> Command {
    use std::os::windows::process::CommandExt;
    let program = tunnel_program();
    let mut cmd = Command::new(&program);
    cmd.arg("--protocol=fortinet");
    match auth {
        Auth::Cookie(_) => cmd.arg("--cookie-on-stdin"),
        Auth::Password { username, .. } => cmd.arg(format!("--user={username}")).arg("--passwd-on-stdin"),
    };
    cmd.arg("--non-inter")
        .arg("--script")
        .arg(program.with_file_name("vpnc-script-win.js"))
        .creation_flags(CREATE_NO_WINDOW);
    if !vpn.trusted_cert.is_empty() {
        // Plain hex digests are SHA-256 of the certificate; openconnect's own
        // suggestions already carry a prefix (pin-sha256:…).
        let cert = &vpn.trusted_cert;
        cmd.arg("--servercert")
            .arg(if cert.contains(':') { cert.clone() } else { format!("sha256:{cert}") });
    }
    cmd.arg(vpn.host_port());
    cmd
}

impl Tunnel {
    pub fn start(
        vpn: &Vpn,
        auth: &Auth,
        session: u64,
        proxy: EventLoopProxy<UserEvent>,
    ) -> std::io::Result<Tunnel> {
        let mut child = command(vpn, auth)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        match auth {
            Auth::Cookie(cookie) => writeln!(stdin, "SVPNCOOKIE={cookie}")?,
            // The helper takes the username on the first line (macOS);
            // openconnect gets it as --user and only wants the password.
            Auth::Password { username, password } => {
                #[cfg(target_os = "macos")]
                writeln!(stdin, "USER={username}")?;
                #[cfg(windows)]
                let _ = username;
                writeln!(stdin, "{password}")?;
            }
        }
        drop(stdin);

        forward_lines(child.stdout.take().unwrap(), session, proxy.clone());
        forward_lines(child.stderr.take().unwrap(), session, proxy.clone());

        let exited = Arc::new(AtomicBool::new(false));
        let pid = child.id();
        let flag = exited.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            flag.store(true, Ordering::SeqCst);
            let _ = proxy.send_event(UserEvent::TunnelExited(session, code));
        });

        Ok(Tunnel { vpn_id: vpn.id.clone(), session, pid, exited })
    }

    /// Asks the tunnel to shut down. On macOS sudo relays SIGTERM to the
    /// helper, which passes it to openfortivpn so routes and DNS are
    /// restored. On Windows the wintun adapter, and with it the VPN's routes
    /// and DNS, goes away when openconnect exits.
    pub fn stop(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = Command::new("taskkill")
                .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }

    pub fn wait_exit(&self, timeout: Duration) {
        let start = Instant::now();
        while !self.exited.load(Ordering::SeqCst) && start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn forward_lines(stream: impl Read + Send + 'static, session: u64, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = proxy.send_event(UserEvent::TunnelLine(session, line));
        }
    });
}
