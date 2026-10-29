//! Native dialogs, notifications and "open this file" per platform.

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;
    use std::process::Command;

    fn applescript_str(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    pub fn notify(title: &str, message: &str) {
        let script = format!(
            "display notification {} with title {}",
            applescript_str(message),
            applescript_str(title)
        );
        let _ = Command::new("osascript").arg("-e").arg(script).spawn();
    }

    pub fn alert(title: &str, message: &str) {
        let script = format!(
            "display alert {} message {} as warning",
            applescript_str(title),
            applescript_str(message)
        );
        std::thread::spawn(move || {
            let _ = Command::new("osascript").arg("-e").arg(script).status();
        });
    }

    pub fn confirm(message: &str, ok: &str) -> bool {
        let script = format!(
            "display dialog {} with title \"FortiBar\" buttons {{\"Cancel\", {ok}}} default button {ok} cancel button \"Cancel\" with icon caution",
            applescript_str(message),
            ok = applescript_str(ok)
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn open_text(path: &Path) {
        let _ = Command::new("open").arg("-e").arg(path).spawn();
    }

    fn agent_path() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join("Library/LaunchAgents/local.fortibar.plist"))
    }

    pub fn launch_at_login() -> bool {
        agent_path().is_some_and(|p| p.is_file())
    }

    /// A per-user LaunchAgent that opens the app at login. It records where
    /// the app is now, so it's rewritten on each start in case it moved.
    pub fn set_launch_at_login(on: bool) {
        let Some(path) = agent_path() else { return };
        if !on {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        // Launch the .app through `open` so it starts like a double-click;
        // a bare binary (cargo run) is started directly.
        let args = match exe.ancestors().find(|d| d.extension().is_some_and(|e| e == "app")) {
            Some(bundle) => vec!["/usr/bin/open".into(), bundle.display().to_string()],
            None => vec![exe.display().to_string()],
        };
        let args: String = args
            .iter()
            .map(|a| format!("<string>{}</string>", a.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")))
            .collect();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
  <key>Label</key><string>local.fortibar</string>\n\
  <key>ProgramArguments</key><array>{args}</array>\n\
  <key>RunAtLoad</key><true/>\n\
</dict></plist>\n"
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, plist);
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::process::Command;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDOK, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn message_box(title: &str, message: &str, style: u32) -> i32 {
        let (title, message) = (wide(title), wide(message));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                style | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
            )
        }
    }

    /// The tray icon colour and tooltip already show connection state.
    pub fn notify(_title: &str, _message: &str) {}

    pub fn alert(title: &str, message: &str) {
        let (title, message) = (title.to_string(), message.to_string());
        std::thread::spawn(move || message_box(&title, &message, MB_OK));
    }

    pub fn confirm(message: &str, _ok: &str) -> bool {
        message_box("FortiBar", message, MB_OKCANCEL) == IDOK
    }

    pub fn open_text(path: &Path) {
        let _ = Command::new("notepad.exe").arg(path).spawn();
    }

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    fn reg(args: &[&str]) -> bool {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("reg.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    pub fn launch_at_login() -> bool {
        reg(&["query", RUN_KEY, "/v", "FortiBar"])
    }

    /// A per-user Run entry pointing at this exe. Rewritten on each start in
    /// case the folder moved.
    pub fn set_launch_at_login(on: bool) {
        if !on {
            reg(&["delete", RUN_KEY, "/v", "FortiBar", "/f"]);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let value = format!("\"{}\"", exe.display());
        reg(&["add", RUN_KEY, "/v", "FortiBar", "/t", "REG_SZ", "/d", &value, "/f"]);
    }
}

pub use imp::*;
