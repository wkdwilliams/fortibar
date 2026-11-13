//! Local control endpoint for fortibar-mcp, the MCP server AI agents start.
//!
//! The app listens on a random loopback port and writes the port plus a
//! random token to `<data>/mcp.json`, which only this user can read. Each
//! request is one connection carrying one JSON line, e.g.
//! `{"token":"…","cmd":"connect","vpn":"Office"}`, answered with one JSON line.
//! Commands run on the event loop, the same as clicking the menu.

use crate::{config, UserEvent};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

pub enum Command {
    Status,
    /// A VPN name or id.
    Connect(String),
    Disconnect,
    /// The last this many log lines.
    Logs(usize),
}

pub const MAX_LOG_LINES: usize = 500;

fn info_file() -> PathBuf {
    config::data_dir().join("mcp.json")
}

pub fn start(proxy: EventLoopProxy<UserEvent>) {
    if let Err(e) = try_start(proxy) {
        eprintln!("fortibar: AI agent (MCP) access is off: {e}");
    }
}

fn try_start(proxy: EventLoopProxy<UserEvent>) -> std::io::Result<()> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    write_info(&json!({ "port": port, "token": token }).to_string())?;

    // Any local process can connect, token or not, so cap how many threads
    // that can tie up. The MCP server makes one request at a time.
    let open = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                open.fetch_sub(1, Ordering::SeqCst);
                continue; // Dropping it closes the connection.
            }
            let (proxy, token, open) = (proxy.clone(), token.clone(), open.clone());
            std::thread::spawn(move || {
                handle(stream, &token, &proxy);
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(())
}

const MAX_CONNECTIONS: usize = 8;
const MAX_REQUEST: usize = 64 * 1024;
/// For the whole request line, so a client sending a byte at a time can't
/// hold a connection open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads up to the first newline, within REQUEST_TIMEOUT and MAX_REQUEST.
fn read_request(mut stream: &TcpStream) -> Option<String> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.contains(&b'\n') {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
        if buf.len() > MAX_REQUEST {
            return None;
        }
    }
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}

/// Written whole and then renamed, so a reader never sees half a file.
fn write_info(text: &str) -> std::io::Result<()> {
    let path = info_file();
    let tmp = path.with_extension("json.tmp");
    // The mode below only applies to a new file, so don't reuse an old one.
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp)?.write_all(text.as_bytes())?;
    std::fs::rename(tmp, path)
}

/// Called on quit, so the MCP server knows FortiBar isn't running.
pub fn stop() {
    let _ = std::fs::remove_file(info_file());
}

fn handle(stream: TcpStream, token: &str, proxy: &EventLoopProxy<UserEvent>) {
    let Some(line) = read_request(&stream) else { return };
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(req) if same(req["token"].as_str().unwrap_or(""), token) => run(&req, proxy),
        // Also what a browser gets for a stray request to this port.
        _ => json!({ "error": "unauthorized" }),
    };
    let _ = (&stream).write_all(format!("{reply}\n").as_bytes());
}

/// Compares without stopping at the first difference, so timing doesn't
/// leak how much of a guessed token was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn run(req: &Value, proxy: &EventLoopProxy<UserEvent>) -> Value {
    let command = match req["cmd"].as_str() {
        Some("status") => Command::Status,
        Some("connect") => Command::Connect(req["vpn"].as_str().unwrap_or("").trim().to_string()),
        Some("disconnect") => Command::Disconnect,
        Some("logs") => {
            let n = req["lines"].as_u64().unwrap_or(100).min(MAX_LOG_LINES as u64);
            Command::Logs(n as usize)
        }
        _ => return json!({ "error": "unknown command" }),
    };
    let (tx, rx) = mpsc::channel();
    if proxy.send_event(UserEvent::Control(command, tx)).is_err() {
        return json!({ "error": "FortiBar is quitting." });
    }
    // The event loop stalls while FortiBar shows a question (e.g. whether
    // to trust a certificate), so don't wait on it forever.
    rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| {
        json!({
            "busy": true,
            "error": "FortiBar didn't answer. It's probably waiting for the user to answer one of its dialogs."
        })
    })
}

#[cfg(test)]
mod tests {
    use super::same;

    #[test]
    fn compares_tokens() {
        assert!(same("abc123", "abc123"));
        assert!(!same("abc123", "abc124"));
        assert!(!same("abc123", "abc12"));
        assert!(!same("", "abc"));
    }
}
