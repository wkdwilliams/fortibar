use crate::config::{helper_path, Vpn};
use crate::UserEvent;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

/// A running `sudo fortivpn-helper` process. Output lines and exit are
/// reported back to the event loop tagged with `session`, so events from an
/// old tunnel can't be mistaken for the current one.
pub struct Tunnel {
    pub vpn_id: String,
    pub session: u64,
    pid: i32,
    exited: Arc<AtomicBool>,
}

impl Tunnel {
    pub fn start(
        vpn: &Vpn,
        cookie: &str,
        session: u64,
        proxy: EventLoopProxy<UserEvent>,
    ) -> std::io::Result<Tunnel> {
        let mut cmd = Command::new("/usr/bin/sudo");
        cmd.arg("-n").arg(helper_path()).arg(vpn.host_port());
        if !vpn.trusted_cert.is_empty() {
            cmd.arg(&vpn.trusted_cert);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // The cookie goes over stdin so it never shows up in `ps`.
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "SVPNCOOKIE={cookie}")?;
        drop(stdin);

        forward_lines(child.stdout.take().unwrap(), session, proxy.clone());
        forward_lines(child.stderr.take().unwrap(), session, proxy.clone());

        let exited = Arc::new(AtomicBool::new(false));
        let pid = child.id() as i32;
        let flag = exited.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            flag.store(true, Ordering::SeqCst);
            let _ = proxy.send_event(UserEvent::TunnelExited(session, code));
        });

        Ok(Tunnel { vpn_id: vpn.id.clone(), session, pid, exited })
    }

    /// Asks the tunnel to shut down. sudo relays SIGTERM to the helper, which
    /// passes it to openfortivpn so routes and DNS are restored.
    pub fn stop(&self) {
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
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
