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
        use std::ffi::CStr;
        use std::os::raw::c_char;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::OnceLock;
        use std::time::Duration;

        #[link(name = "UserNotifications", kind = "framework")]
        extern "C" {}

        const AUTHORIZE_ALERT: usize = 1 << 2;
        const PRESENT_LIST_AND_BANNER: usize = (1 << 3) | (1 << 4);
        /// UNAuthorizationStatusDenied: the user said no, or turned them off
        /// in System Settings. macOS won't ask again, so we have to.
        const STATUS_DENIED: isize = 1;

        struct Center(*mut AnyObject);
        // UNUserNotificationCenter is thread-safe.
        unsafe impl Send for Center {}
        unsafe impl Sync for Center {}

        static CENTER: OnceLock<Option<Center>> = OnceLock::new();
        static DENIED: AtomicBool = AtomicBool::new(false);
        static ON_CHANGE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

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
                // Once the prompt is answered (or skipped, if it was answered
                // before), find out whether notifications are blocked.
                let done = RcBlock::new(move |_granted: Bool, _error: *mut AnyObject| check_center(center));
                let _: () = msg_send![center, requestAuthorizationWithOptions: AUTHORIZE_ALERT, completionHandler: &*done];
                Some(Center(center))
            });
            center.as_ref().map(|c| c.0)
        }

        pub fn init(on_change: Box<dyn Fn() + Send + Sync>) {
            let _ = ON_CHANGE.set(on_change);
            center();
        }

        fn check_center(center: *mut AnyObject) {
            let done = RcBlock::new(|settings: *mut AnyObject| {
                if settings.is_null() {
                    return;
                }
                let status: isize = unsafe { msg_send![settings, authorizationStatus] };
                let denied = status == STATUS_DENIED;
                if DENIED.swap(denied, Ordering::Relaxed) != denied {
                    if let Some(f) = ON_CHANGE.get() {
                        f();
                    }
                }
            });
            unsafe {
                let _: () = msg_send![center, getNotificationSettingsWithCompletionHandler: &*done];
            }
        }

        pub fn denied() -> bool {
            DENIED.load(Ordering::Relaxed)
        }

        fn bundle_id() -> Option<String> {
            unsafe {
                let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
                let id: *mut AnyObject = msg_send![bundle, bundleIdentifier];
                if id.is_null() {
                    return None;
                }
                let utf8: *const c_char = msg_send![id, UTF8String];
                (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
            }
        }

        /// Opens FortiBar's page in System Settings → Notifications, then
        /// keeps checking for a couple of minutes so the menu updates as soon
        /// as they're turned on.
        pub fn open_settings() {
            let Some(id) = bundle_id() else { return };
            let url = format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}");
            let _ = std::process::Command::new("open").arg(url).spawn();
            let Some(center) = center() else { return };
            let center = Center(center);
            std::thread::spawn(move || {
                let center = center;
                for _ in 0..60 {
                    std::thread::sleep(Duration::from_secs(2));
                    check_center(center.0);
                    if !denied() {
                        break;
                    }
                }
            });
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
    /// not at the first connect. `on_change` runs (on any thread) whenever
    /// `notifications_denied` changes.
    pub fn init_notifications(on_change: impl Fn() + Send + Sync + 'static) {
        un::init(Box::new(on_change));
    }

    /// The user has blocked FortiBar's notifications, so macOS won't show
    /// them or ask again until they're turned on in System Settings.
    pub fn notifications_denied() -> bool {
        un::denied()
    }

    pub fn open_notification_settings() {
        un::open_settings();
    }

    static MUTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Turned off in FortiBar's menu: `notify` does nothing.
    pub fn set_notifications_muted(muted: bool) {
        MUTED.store(muted, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn notify(title: &str, message: &str) {
        if MUTED.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
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

    /// Like `confirm`, but for a risky choice: a warning icon, and Return
    /// picks Cancel.
    pub fn confirm_risky(message: &str, ok: &str) -> bool {
        let script = format!(
            "display dialog {} with title \"FortiBar\" buttons {{\"Cancel\", {ok}}} default button \"Cancel\" cancel button \"Cancel\" with icon caution",
            applescript_str(message),
            ok = applescript_str(ok),
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
        MessageBoxW, IDOK, MB_DEFBUTTON2, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
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

    pub fn init_notifications(_on_change: impl Fn() + Send + Sync + 'static) {}

    pub fn notifications_denied() -> bool {
        false
    }

    pub fn open_notification_settings() {}

    pub fn set_notifications_muted(_muted: bool) {}

    /// The tray icon colour and tooltip already show connection state.
    pub fn notify(_title: &str, _message: &str) {}

    pub fn alert(title: &str, message: &str) {
        let (title, message) = (title.to_string(), message.to_string());
        std::thread::spawn(move || message_box(&title, &message, MB_OK));
    }

    pub fn confirm(message: &str, _ok: &str) -> bool {
        message_box("FortiBar", message, MB_OKCANCEL) == IDOK
    }

    /// Like `confirm`, but Enter picks Cancel.
    pub fn confirm_risky(message: &str, _ok: &str) -> bool {
        message_box("FortiBar", message, MB_OKCANCEL | MB_DEFBUTTON2) == IDOK
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
