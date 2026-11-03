//! Native dialogs, notifications and "open this file" per platform.

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;
    use std::process::Command;

    fn applescript_str(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// The .app this binary runs from, if any (not for `cargo run`).
    fn app_bundle() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let bundle = exe.parent()?.parent()?.parent()?;
        bundle.extension().is_some_and(|e| e == "app").then(|| bundle.to_path_buf())
    }

    /// Dialogs run in osascript, so macOS would show Script Editor's icon;
    /// point them at FortiBar's own instead.
    fn dialog_icon() -> String {
        let icon = app_bundle().map(|b| b.join("Contents/Resources/FortiBar.icns")).filter(|p| p.is_file());
        match icon {
            Some(p) => format!("(POSIX file {} as alias)", applescript_str(&p.display().to_string())),
            None => "caution".to_string(),
        }
    }

    /// Notifications go through UserNotifications so macOS shows them as
    /// FortiBar's, with its icon. That needs to run from the .app (it raises
    /// an uncaught exception otherwise), so a bare binary (e.g. `cargo run`)
    /// falls back to osascript, which shows them as Script Editor's.
    mod un {
        use block2::{Block, RcBlock};
        use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Imp, Sel};
        use objc2::{class, msg_send, sel};
        use std::ffi::CString;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;

        #[link(name = "UserNotifications", kind = "framework")]
        extern "C" {}

        const AUTHORIZE_ALERT: usize = 1 << 2;
        const PRESENT_LIST_AND_BANNER: usize = (1 << 3) | (1 << 4);

        struct Center(*mut AnyObject);
        // UNUserNotificationCenter is thread-safe.
        unsafe impl Send for Center {}
        unsafe impl Sync for Center {}

        static CENTER: OnceLock<Option<Center>> = OnceLock::new();

        fn ns_string(s: &str) -> *mut AnyObject {
            let s = CString::new(s.replace('\0', "")).unwrap();
            unsafe { msg_send![class!(NSString), stringWithUTF8String: s.as_ptr()] }
        }

        /// Without this, macOS hides a notification whenever FortiBar is the
        /// active app, e.g. while a sign-in window is open.
        unsafe extern "C-unwind" fn will_present(
            _this: *mut AnyObject,
            _cmd: Sel,
            _center: *mut AnyObject,
            _notification: *mut AnyObject,
            handler: &Block<dyn Fn(usize)>,
        ) {
            handler.call((PRESENT_LIST_AND_BANNER,));
        }

        fn delegate() -> *mut AnyObject {
            // Only called once, from center()'s OnceLock.
            let cls = ClassBuilder::new(c"FortiBarNotificationDelegate", class!(NSObject)).unwrap().register();
            unsafe {
                let imp: Imp = std::mem::transmute(
                    will_present
                        as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, &Block<dyn Fn(usize)>),
                );
                let sel = sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:);
                objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, sel, imp, c"v@:@@@?".as_ptr());
                // Never released: the centre holds its delegate weakly.
                msg_send![cls, new]
            }
        }

        /// Sets up the centre and asks for permission (macOS prompts once).
        fn center() -> Option<*mut AnyObject> {
            let center = CENTER.get_or_init(|| unsafe {
                let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
                let id: *mut AnyObject = msg_send![bundle, bundleIdentifier];
                // A bundle id alone isn't enough: without a real .app,
                // currentNotificationCenter throws and aborts the process.
                if id.is_null() || super::app_bundle().is_none() {
                    return None;
                }
                let center: *mut AnyObject = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
                let _: () = msg_send![center, setDelegate: delegate()];
                let done = RcBlock::new(|_granted: Bool, _error: *mut AnyObject| {});
                let _: () = msg_send![center, requestAuthorizationWithOptions: AUTHORIZE_ALERT, completionHandler: &*done];
                Some(Center(center))
            });
            center.as_ref().map(|c| c.0)
        }

        pub fn init() {
            center();
        }

        pub fn post(title: &str, message: &str) -> bool {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let Some(center) = center() else { return false };
            // Unique across launches: a reused id would replace an older
            // notification in Notification Center.
            let id = format!("fortibar-{}-{}", std::process::id(), NEXT_ID.fetch_add(1, Ordering::Relaxed));
            unsafe {
                let content: *mut AnyObject = msg_send![class!(UNMutableNotificationContent), new];
                let _: () = msg_send![content, setTitle: ns_string(title)];
                let _: () = msg_send![content, setBody: ns_string(message)];
                let request: *mut AnyObject = msg_send![
                    class!(UNNotificationRequest),
                    requestWithIdentifier: ns_string(&id),
                    content: content,
                    trigger: std::ptr::null_mut::<AnyObject>()
                ];
                let done = RcBlock::new(|error: *mut AnyObject| {
                    if !error.is_null() {
                        eprintln!("fortibar: notification not shown (notifications turned off for FortiBar?)");
                    }
                });
                let _: () = msg_send![center, addNotificationRequest: request, withCompletionHandler: &*done];
                let _: () = msg_send![content, release];
            }
            true
        }
    }

    /// Call once at startup so macOS asks for notification permission then,
    /// not at the first connect.
    pub fn init_notifications() {
        un::init();
    }

    pub fn notify(title: &str, message: &str) {
        if un::post(title, message) {
            return;
        }
        let script = format!(
            "display notification {} with title {}",
            applescript_str(message),
            applescript_str(title)
        );
        let _ = Command::new("osascript").arg("-e").arg(script).spawn();
    }

    pub fn alert(title: &str, message: &str) {
        let script = format!(
            "display dialog {} with title \"FortiBar\" buttons {{\"OK\"}} default button \"OK\" with icon {}",
            applescript_str(&format!("{title}\n\n{message}")),
            dialog_icon()
        );
        std::thread::spawn(move || {
            let _ = Command::new("osascript").arg("-e").arg(script).status();
        });
    }

    pub fn confirm(message: &str, ok: &str) -> bool {
        let script = format!(
            "display dialog {} with title \"FortiBar\" buttons {{\"Cancel\", {ok}}} default button {ok} cancel button \"Cancel\" with icon {icon}",
            applescript_str(message),
            ok = applescript_str(ok),
            icon = dialog_icon()
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

    pub fn init_notifications() {}

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
