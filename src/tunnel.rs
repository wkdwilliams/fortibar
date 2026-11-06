use crate::config::{tunnel_program, Vpn};
use crate::UserEvent;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    /// macOS: the helper's stdin, held open while the tunnel should stay up.
    /// If the app quits or crashes the pipe closes and the helper takes the
    /// tunnel down, instead of leaving it running with nothing to control it.
    keepalive: Mutex<Option<ChildStdin>>,
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
    if !vpn.trusted_cert.is_empty() || !vpn.dns_domains.is_empty() {
        cmd.arg(&vpn.trusted_cert);
    }
    if !vpn.dns_domains.is_empty() {
        cmd.arg(vpn.dns_domains.join(","));
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
        let secret = match auth {
            Auth::Cookie(cookie) => format!("SVPNCOOKIE={cookie}\n"),
            // The helper takes the username on the first line (macOS);
            // openconnect gets it as --user and only wants the password.
            Auth::Password { username, password } if cfg!(target_os = "macos") => {
                format!("USER={username}\n{password}\n")
            }
            Auth::Password { password, .. } => format!("{password}\n"),
        };
        // A failed write means the process already exited (e.g. sudo -n
        // refused); its output and exit, forwarded below, say why.
        let _ = stdin.write_all(secret.as_bytes()).and_then(|()| stdin.flush());
        let keepalive = cfg!(target_os = "macos").then_some(stdin);

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

        Ok(Tunnel { vpn_id: vpn.id.clone(), session, pid, exited, keepalive: Mutex::new(keepalive) })
    }

    /// Asks the tunnel to shut down. On macOS closing the helper's stdin
    /// tells it to stop, and sudo also relays SIGTERM to it; it passes that
    /// on to openfortivpn so routes and DNS are restored. On Windows the
    /// wintun adapter, and with it the VPN's routes and DNS, goes away when
    /// openconnect exits.
    pub fn stop(&self) {
        // Already gone: the pid may belong to some other process by now.
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        drop(self.keepalive.lock().unwrap().take());
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

    /// Pings `host` through the tunnel every `KEEPALIVE_INTERVAL` until the
    /// tunnel exits, so gateways that drop idle tunnels (often after 5 min)
    /// keep it up. Only changes between replying and not are logged.
    pub fn keep_alive(&self, host: String, proxy: EventLoopProxy<UserEvent>) {
        let (exited, session) = (self.exited.clone(), self.session);
        std::thread::spawn(move || {
            let say = |line: String| {
                let _ = proxy.send_event(UserEvent::TunnelLine(session, line));
            };
            say(format!("Keep-alive: pinging {host} every {}s", KEEPALIVE_INTERVAL.as_secs()));
            let mut replying = true;
            while !exited.load(Ordering::SeqCst) {
                let reply = ping(&host);
                if exited.load(Ordering::SeqCst) {
                    break;
                }
                if reply != replying {
                    say(match reply {
                        true => format!("Keep-alive: {host} replies again"),
                        false => format!("Keep-alive: no reply from {host}"),
                    });
                    replying = reply;
                }
                std::thread::sleep(KEEPALIVE_INTERVAL);
            }
        });
    }

    pub fn wait_exit(&self, timeout: Duration) {
        let start = Instant::now();
        while !self.exited.load(Ordering::SeqCst) && start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Well under FortiGate's default SSL-VPN idle timeout of 300s.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// One echo request, waiting up to 5s for the reply. `host` is validated by
/// config::parse_keepalive_host, so it can't be read as an option.
fn ping(host: &str) -> bool {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("/sbin/ping");
        c.args(["-c", "1", "-t", "5", host]);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("ping");
        c.args(["-n", "1", "-w", "5000", host]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

fn forward_lines(stream: impl Read + Send + 'static, session: u64, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = proxy.send_event(UserEvent::TunnelLine(session, line));
        }
    });
}
