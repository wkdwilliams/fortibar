//! fortibar-mcp: an MCP server (stdio) that lets AI agents see FortiBar's
//! status, connect and disconnect its VPNs, and read its log.
//!
//! It doesn't run tunnels itself. It relays to the running FortiBar app
//! (src/control.rs), which does the work exactly as if the user had clicked
//! the menu, sign-in windows included. `connect` starts FortiBar if it isn't
//! running; nothing else does.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Newest first; the first is offered to clients asking for something else.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "FortiBar is the user's FortiGate SSL-VPN client. Call get_status first: it \
lists the configured VPNs and the current connection. Only one VPN is connected at a time; connecting \
to another switches over. Connecting may open a FortiBar window where the user has to sign in \
(Microsoft SSO) or type a password. When a result says it's waiting for the user, tell them to finish \
signing in rather than retrying.";

const USAGE: &str = "fortibar-mcp: MCP server (stdio) for FortiBar.

Add it to an MCP client as a stdio server with this program as the command
and no arguments. For Claude Code:

  claude mcp add fortibar -- /Applications/FortiBar.app/Contents/MacOS/fortibar-mcp

Tools: get_status, connect, disconnect, get_logs.";

fn main() {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    if std::env::args().skip(1).any(|a| a == "-V" || a == "--version") {
        println!("fortibar-mcp {VERSION}");
        return;
    }

    let out = Arc::new(Mutex::new(std::io::stdout()));
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let msg = match serde_json::from_str::<Value>(&line) {
            Ok(m) => m,
            Err(_) => {
                send(&out, json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }));
                continue;
            }
        };
        // Notifications (no id) and replies to us (no method) need no answer.
        let (Some(id), Some(method)) = (msg.get("id").cloned(), msg["method"].as_str()) else { continue };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
                let result = json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fortibar", "title": "FortiBar VPN", "version": VERSION },
                    "instructions": INSTRUCTIONS,
                });
                reply(&out, id, result);
            }
            "ping" => reply(&out, id, json!({})),
            "tools/list" => reply(&out, id, json!({ "tools": tools() })),
            "tools/call" => {
                // connect can wait a minute; keep answering pings meanwhile.
                let out = out.clone();
                std::thread::spawn(move || {
                    let name = params["name"].as_str().unwrap_or("");
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let result = match call_tool(name, &args) {
                        Some(r) => r,
                        None => {
                            let error = json!({ "code": -32602, "message": format!("Unknown tool: {name}") });
                            return send(&out, json!({ "jsonrpc": "2.0", "id": id, "error": error }));
                        }
                    };
                    reply(&out, id, result);
                });
            }
            _ => {
                let error = json!({ "code": -32601, "message": format!("Method not found: {method}") });
                send(&out, json!({ "jsonrpc": "2.0", "id": id, "error": error }));
            }
        }
    }
}

fn reply(out: &Mutex<std::io::Stdout>, id: Value, result: Value) {
    send(out, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn send(out: &Mutex<std::io::Stdout>, msg: Value) {
    let mut out = out.lock().unwrap();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn tools() -> Value {
    json!([
        {
            "name": "get_status",
            "title": "Get VPN status",
            "description": "Shows whether a VPN is connected (and which), whether FortiBar is waiting for the user \
                to sign in, the last error, and every configured VPN with its name, id and gateway.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
        {
            "name": "connect",
            "title": "Connect VPN",
            "description": "Connects to a VPN, by name or id (see get_status). If another VPN is connected, it's \
                disconnected first. Starts FortiBar if it isn't running. Waits until the tunnel is up, the attempt \
                fails, or wait_seconds pass. Sign-in may need the user: FortiBar opens a window for the Microsoft \
                login or the password when it can't sign in on its own.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "vpn": { "type": "string", "description": "The VPN's name (case-insensitive) or id." },
                    "wait_seconds": {
                        "type": "integer", "minimum": 0, "maximum": 300,
                        "description": "How long to wait for the result. Default 60; 0 returns at once."
                    }
                },
                "required": ["vpn"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        },
        {
            "name": "disconnect",
            "title": "Disconnect VPN",
            "description": "Disconnects the VPN, or cancels a sign-in in progress. Waits until it's down.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "wait_seconds": {
                        "type": "integer", "minimum": 0, "maximum": 120,
                        "description": "How long to wait for the tunnel to close. Default 20."
                    }
                },
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "get_logs",
            "title": "Get VPN log",
            "description": "Returns the tunnel's output (openfortivpn on macOS, openconnect on Windows) for the \
                current or most recent connection attempt: gateway messages, assigned IP, routes, DNS and errors. \
                It starts over with each connection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer", "minimum": 1, "maximum": 500,
                        "description": "How many of the most recent lines to return. Default 100."
                    }
                },
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        },
    ])
}

// ---- tools ------------------------------------------------------------------

fn call_tool(name: &str, args: &Value) -> Option<Value> {
    let result = match name {
        "get_status" => get_status(),
        "connect" => connect(args),
        "disconnect" => disconnect(args),
        "get_logs" => get_logs(args),
        _ => return None,
    };
    Some(match result {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
    })
}

const NOT_RUNNING: &str = "FortiBar isn't running.";

fn get_status() -> Result<String, String> {
    match request(json!({ "cmd": "status" })) {
        Ok(r) => Ok(with_status(&summary(&r["status"]), &r["status"])),
        Err(AppError::NotRunning) => {
            Ok(with_status(&format!("{NOT_RUNNING} connect starts it."), &json!({ "state": "not_running" })))
        }
        Err(e) => Err(e.text()),
    }
}

fn connect(args: &Value) -> Result<String, String> {
    let vpn = args["vpn"].as_str().unwrap_or("").trim();
    if vpn.is_empty() {
        return Err("Say which VPN to connect to: its name or id from get_status.".to_string());
    }
    let wait = wait_seconds(args, 60, 300);
    let cmd = json!({ "cmd": "connect", "vpn": vpn });
    let r = match request(cmd.clone()) {
        Err(AppError::NotRunning) => {
            launch_app()?;
            wait_until_running(Duration::from_secs(20))?;
            request(cmd)
        }
        r => r,
    }
    .map_err(AppError::text)?;

    let target = r["target"]["id"].as_str().unwrap_or("").to_string();
    let target_name = r["target"]["name"].as_str().unwrap_or(vpn).to_string();
    let mut status = r["status"].clone();
    let deadline = Instant::now() + wait;
    loop {
        let state = status["state"].as_str().unwrap_or("");
        let on = status["vpn"]["id"].as_str() == Some(target.as_str());
        match state {
            "connected" if on => return Ok(with_status(&summary(&status), &status)),
            "disconnected" => {
                let why = status["last_error"].as_str().unwrap_or("The attempt was cancelled.");
                return Err(with_status(&format!("Couldn't connect to {target_name}. {why}"), &status));
            }
            "signing_in" | "connecting" | "connected" if !on => {
                let other = status["vpn"]["name"].as_str().unwrap_or("another VPN");
                let text = format!("FortiBar switched to {other} before {target_name} connected.");
                return Err(with_status(&text, &status));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let text = format!("{} Call get_status to check again.", summary(&status));
            return Ok(with_status(&text, &status));
        }
        std::thread::sleep(Duration::from_millis(500));
        match request(json!({ "cmd": "status" })) {
            Ok(r) => status = r["status"].clone(),
            Err(AppError::Busy(_)) => {} // a dialog is up; keep waiting
            Err(e) => return Err(e.text()),
        }
    }
}

fn disconnect(args: &Value) -> Result<String, String> {
    let wait = wait_seconds(args, 20, 120);
    let mut status = match request(json!({ "cmd": "disconnect" })) {
        Ok(r) => r["status"].clone(),
        Err(AppError::NotRunning) => return Ok(format!("{NOT_RUNNING} No VPN is connected through it.")),
        Err(e) => return Err(e.text()),
    };
    let deadline = Instant::now() + wait;
    while status["state"] != "disconnected" {
        if Instant::now() >= deadline {
            let text = format!("{} Call get_status to check again.", summary(&status));
            return Ok(with_status(&text, &status));
        }
        std::thread::sleep(Duration::from_millis(500));
        match request(json!({ "cmd": "status" })) {
            Ok(r) => status = r["status"].clone(),
            // FortiBar quitting takes the tunnel down with it.
            Err(AppError::NotRunning) => return Ok(format!("Disconnected. {NOT_RUNNING}")),
            Err(AppError::Busy(_)) => {}
            Err(e) => return Err(e.text()),
        }
    }
    Ok(with_status("Disconnected.", &status))
}

fn get_logs(args: &Value) -> Result<String, String> {
    let lines = args["lines"].as_u64().unwrap_or(100).clamp(1, 500);
    let r = match request(json!({ "cmd": "logs", "lines": lines })) {
        Ok(r) => r,
        Err(AppError::NotRunning) => return Err(format!("{NOT_RUNNING} Its log only lives while it runs.")),
        Err(e) => return Err(e.text()),
    };
    let log: Vec<&str> = r["lines"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let head = summary(&r["status"]);
    if log.is_empty() {
        return Ok(format!("{head}\n\nThe log is empty. It fills once a connection starts."));
    }
    Ok(format!("{head}\n\nLast {} log lines:\n{}", log.len(), log.join("\n")))
}

fn wait_seconds(args: &Value, default: u64, max: u64) -> Duration {
    Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(default).min(max))
}

fn summary(status: &Value) -> String {
    let name = status["vpn"]["name"].as_str().unwrap_or("");
    match status["state"].as_str().unwrap_or("") {
        "connected" => format!("Connected to {name}."),
        "signing_in" if status["waiting_for_user"] == true => format!(
            "Signing in to {name}: a FortiBar window is waiting for the user to sign in. Ask them to finish it."
        ),
        "signing_in" => format!("Signing in to {name}…"),
        "connecting" => format!("Connecting to {name}…"),
        "disconnecting" => format!("Disconnecting from {name}…"),
        _ => match status["last_error"].as_str() {
            Some(e) => format!("Not connected. Last error: {e}"),
            None => "Not connected.".to_string(),
        },
    }
}

fn with_status(text: &str, status: &Value) -> String {
    format!("{text}\n\n{}", serde_json::to_string_pretty(status).unwrap_or_default())
}

// ---- talking to FortiBar ------------------------------------------------------

enum AppError {
    NotRunning,
    /// FortiBar is stuck on one of its dialogs; asking again later works.
    Busy(String),
    Failed(String),
}

impl AppError {
    fn text(self) -> String {
        match self {
            AppError::NotRunning => NOT_RUNNING.to_string(),
            AppError::Busy(e) | AppError::Failed(e) => e,
        }
    }
}

/// Where FortiBar writes `mcp.json`; matches config::data_dir() in the app.
fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/FortiBar"));
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FortiBar"));
    #[cfg(not(any(target_os = "macos", windows)))]
    return None;
}

/// One request per connection: a JSON line out, a JSON line back.
fn request(mut cmd: Value) -> Result<Value, AppError> {
    let info = data_dir()
        .and_then(|d| std::fs::read_to_string(d.join("mcp.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .ok_or(AppError::NotRunning)?;
    let port = info["port"].as_u64().filter(|p| (1..=65535).contains(p)).ok_or(AppError::NotRunning)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port as u16));
    // Left behind by a FortiBar that crashed: nothing listens there now.
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| AppError::NotRunning)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));

    cmd["token"] = info["token"].clone();
    let failed = |e: std::io::Error| AppError::Failed(format!("Lost contact with FortiBar: {e}"));
    (&stream).write_all(format!("{cmd}\n").as_bytes()).map_err(failed)?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(failed)?;
    let r: Value = serde_json::from_str(&line).map_err(|_| AppError::Failed("FortiBar sent a bad reply.".into()))?;
    match r["error"].as_str() {
        // Another program is on a recycled port, or a stale file.
        Some("unauthorized") => Err(AppError::NotRunning),
        Some(e) if r["busy"] == true => Err(AppError::Busy(e.to_string())),
        Some(e) => Err(AppError::Failed(e.to_string())),
        None => Ok(r),
    }
}

fn wait_until_running(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match request(json!({ "cmd": "status" })) {
            Err(AppError::NotRunning) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(300)),
            Err(AppError::NotRunning) => return Err("Started FortiBar, but it didn't come up in time.".to_string()),
            _ => return Ok(()),
        }
    }
}

/// Starts FortiBar from next to this program: the .app it sits in on macOS,
/// FortiBar.exe in the same folder on Windows (which asks for admin rights).
fn launch_app() -> Result<(), String> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Can't tell where FortiBar is.")?;
    #[cfg(target_os = "macos")]
    let mut cmd = match exe.ancestors().find(|d| d.extension().is_some_and(|e| e == "app")) {
        Some(bundle) => {
            let mut c = Command::new("/usr/bin/open");
            c.arg("-g").arg(bundle); // -g: don't take focus from the user
            c
        }
        // A cargo build: target/<profile>/fortibar
        None => Command::new(dir.join("fortibar")),
    };
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let app = ["FortiBar.exe", "fortibar.exe"].iter().map(|n| dir.join(n)).find(|p| p.is_file());
        let app = app.ok_or(format!("{NOT_RUNNING} Couldn't find FortiBar.exe next to fortibar-mcp.exe."))?;
        // start goes through ShellExecute, which shows the UAC prompt that
        // FortiBar's manifest asks for; a plain spawn would just fail.
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(app).creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut cmd = Command::new(dir.join("fortibar"));
    // Our stdin/stdout are the MCP connection; FortiBar mustn't inherit them.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("{NOT_RUNNING} Starting it failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_status() {
        let s = json!({ "state": "connected", "vpn": { "id": "a", "name": "Office" } });
        assert_eq!(summary(&s), "Connected to Office.");
        let s = json!({ "state": "signing_in", "vpn": { "name": "Office" }, "waiting_for_user": true });
        assert!(summary(&s).contains("waiting for the user"));
        let s = json!({ "state": "disconnected", "vpn": null, "last_error": "boom" });
        assert_eq!(summary(&s), "Not connected. Last error: boom");
    }

    #[test]
    fn unknown_tool_is_none() {
        assert!(call_tool("nope", &json!({})).is_none());
    }
}
