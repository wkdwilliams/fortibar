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
}

pub use imp::*;
