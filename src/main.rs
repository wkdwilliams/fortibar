#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod control;
mod icon;
mod platform;
#[cfg(target_os = "macos")]
mod tls;
mod tunnel;

use config::Vpn;
use control::Command;
use serde_json::{json, Value};
use platform::{alert, confirm, notify};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};
use tunnel::{Auth, Tunnel};
use wry::cookie::{Cookie, Expiration};
use wry::{PageLoadEvent, WebView, WebViewBuilder};
#[cfg(windows)]
use wry::WebContext;
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;

const SESSION_COOKIE: &str = "SVPNCOOKIE";

pub enum UserEvent {
    Menu(MenuEvent),
    /// Look for the FortiGate session cookie. `Some(attempt)` comes from the
    /// polling timer and reschedules itself; `None` comes from a page load.
    AuthCheck(String, Option<u64>),
    /// FortiGate finished SAML and redirected to 127.0.0.1:8020/?id=…
    /// (the id is empty if the redirect didn't carry one).
    AuthSamlId(String, String),
    /// The SAML id was exchanged this long ago and still no session cookie:
    /// the gateway didn't accept the sign-in.
    AuthFailed(String, u64),
    /// Sign-in didn't complete silently; show the window to the user.
    AuthReveal(String, u64),
    Form(String),
    /// Username/password window for VPNs that don't use SSO.
    Login(String),
    TunnelLine(u64, String),
    TunnelExited(u64, Option<i32>),
    /// Time for the next automatic reconnect attempt; the u64 matches
    /// `App::reconnect_token` unless the attempt was cancelled since.
    Reconnect(String, u64),
    /// A request from fortibar-mcp (an AI agent), answered on the sender.
    Control(Command, mpsc::Sender<Value>),
    /// Notifications were blocked or unblocked for FortiBar.
    NotificationsChanged,
}

enum Status {
    Idle,
    SigningIn(String),
    Connecting(String),
    Connected(String),
    Disconnecting(String),
    /// The tunnel dropped and auto-reconnect is waiting to try again.
    Reconnecting(String),
}

/// Waits before each automatic reconnect attempt; the last repeats.
const RECONNECT_DELAYS: [u64; 5] = [2, 5, 15, 30, 60];
/// Gives up after this many attempts in a row (about 7 minutes).
const MAX_RECONNECTS: u32 = 10;
/// A tunnel that stayed up this long counts as working again, so the next
/// drop starts the attempts from the beginning.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// How long the gateway gets to issue the session cookie once it has handed
/// back a SAML id.
const SAML_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

type Target = EventLoopWindowTarget<UserEvent>;

/// One per VPN, kept alive (hidden) for the app's lifetime so session-only
/// Microsoft cookies stay in memory between connects.
struct AuthWindow {
    window: Window,
    webview: WebView,
    /// This VPN's WebView2 profile folder; must outlive the webview.
    #[cfg(windows)]
    _context: WebContext,
}

/// Credentials the current tunnel was started with, kept until it's up so a
/// certificate-trust retry doesn't ask again and a typed password can be
/// saved once it's proven right.
struct PasswordLogin {
    username: String,
    password: String,
    typed: bool,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    vpns: Vec<Vpn>,
    tray: Option<TrayIcon>,
    _app_menu: Option<Menu>,
    auth: HashMap<String, AuthWindow>,
    /// The add/edit window, and the id of the VPN it edits (None: a new one).
    form: Option<(Window, WebView, Option<String>)>,
    /// Password prompt and the VPN it's for.
    login: Option<(String, Window, WebView)>,
    status: Status,
    tunnel: Option<Tunnel>,
    /// VPN to connect to once the current tunnel has shut down.
    next_vpn: Option<String>,
    /// Certificate digest openfortivpn asked us to trust, if any.
    pending_cert: Option<String>,
    sudo_failed: bool,
    /// The gateway rejected the username or password.
    auth_failed: bool,
    password_login: Option<PasswordLogin>,
    attempt: u64,
    /// The current SSO sign-in has already been restarted once because the
    /// gateway didn't issue a session cookie.
    saml_retried: bool,
    session: u64,
    log: VecDeque<String>,
    /// Why the last connect attempt failed or dropped, for AI agents, which
    /// can't see the alert.
    last_error: Option<String>,
    /// The VPN whose sign-in / tunnel in progress is an automatic reconnect.
    reconnecting: Option<String>,
    /// Automatic reconnect attempts in a row.
    retries: u32,
    reconnect_token: u64,
    connected_at: Option<Instant>,
}

fn main() {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    // Refresh the login item's recorded path in case the app was moved.
    if platform::launch_at_login() {
        platform::set_launch_at_login(true);
    }

    let proxy = event_loop.create_proxy();
    let notify_proxy = proxy.clone();
    platform::init_notifications(move || {
        let _ = notify_proxy.send_event(UserEvent::NotificationsChanged);
    });
    platform::set_notifications_muted(!notifications_wanted());
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));

    let mut app = App {
        proxy,
        vpns: config::load(),
        tray: None,
        _app_menu: None,
        auth: HashMap::new(),
        form: None,
        login: None,
        status: Status::Idle,
        tunnel: None,
        next_vpn: None,
        pending_cert: None,
        sudo_failed: false,
        auth_failed: false,
        password_login: None,
        attempt: 0,
        saml_retried: false,
        session: 0,
        log: VecDeque::new(),
        last_error: None,
        reconnecting: None,
        retries: 0,
        reconnect_token: 0,
        connected_at: None,
    };
    control::start(app.proxy.clone());

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => app.init(target),
            Event::UserEvent(UserEvent::Menu(e)) => {
                if app.on_menu(target, e.id.0.as_str()) {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(e) => app.on_event(target, e),
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if app.form.as_ref().is_some_and(|(w, ..)| w.id() == window_id) {
                    app.form = None;
                } else if app.login.as_ref().is_some_and(|(_, w, _)| w.id() == window_id) {
                    app.cancel_login();
                } else if let Some(id) =
                    app.auth.iter().find(|(_, a)| a.window.id() == window_id).map(|(id, _)| id.clone())
                {
                    app.auth[&id].window.set_visible(false);
                    if matches!(&app.status, Status::SigningIn(s) if *s == id) {
                        app.stop_reconnecting();
                        app.status = Status::Idle;
                        app.refresh();
                    }
                }
            }
            _ => {}
        }
    });
}

impl App {
    #[cfg(windows)]
    fn init(&mut self, _target: &Target) {
        self.create_tray();
    }

    #[cfg(target_os = "macos")]
    fn init(&mut self, target: &Target) {
        // wry registers its delegate class lazily; build a throwaway webview
        // so it exists, then teach it to accept pinned gateway certificates.
        if let Ok(w) = WindowBuilder::new().with_visible(false).build(target) {
            let _ = WebViewBuilder::new().build(&w);
        }
        tls::install();
        tls::set_pins(&self.vpns);

        // Accessory apps have no visible menu bar, but WKWebView still needs
        // an Edit menu for ⌘C/⌘V/⌘A to work in the sign-in and form windows.
        let app_menu = Menu::new();
        let app_sub = Submenu::new("FortiBar", true);
        let edit = Submenu::new("Edit", true);
        let _ = edit.append_items(&[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ]);
        let _ = app_menu.append_items(&[&app_sub, &edit]);
        app_menu.init_for_nsapp();
        self._app_menu = Some(app_menu);
        self.create_tray();
    }

    fn create_tray(&mut self) {
        self.tray = TrayIconBuilder::new()
            .with_icon(icon::shield(false))
            .with_icon_as_template(true)
            .with_tooltip("FortiBar")
            .with_menu(Box::new(self.build_menu()))
            .build()
            .ok();
    }

    fn vpn(&self, id: &str) -> Option<&Vpn> {
        self.vpns.iter().find(|v| v.id == id)
    }

    fn save_vpns(&self) {
        if let Err(e) = config::save(&self.vpns) {
            alert("FortiBar couldn't save your VPNs", &e.to_string());
        }
    }

    fn vpn_name(&self, id: &str) -> String {
        self.vpn(id).map(|v| v.name.clone()).unwrap_or_default()
    }

    fn active_vpn(&self) -> Option<&str> {
        match &self.status {
            Status::SigningIn(id) | Status::Connecting(id) | Status::Connected(id) | Status::Reconnecting(id) => {
                Some(id)
            }
            _ => None,
        }
    }

    fn later(&self, delay: Duration, event: UserEvent) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = proxy.send_event(event);
        });
    }

    // ---- menu -----------------------------------------------------------

    fn build_menu(&self) -> Menu {
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::new(self.status_text(), false, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        self.append_items(&menu);
        menu
    }

    fn status_text(&self) -> String {
        match &self.status {
            Status::Idle => "Not connected".to_string(),
            Status::SigningIn(id) => format!("Signing in to {}…", self.vpn_name(id)),
            Status::Connecting(id) => format!("Connecting to {}…", self.vpn_name(id)),
            Status::Connected(id) => format!("Connected to {}", self.vpn_name(id)),
            Status::Disconnecting(id) => format!("Disconnecting from {}…", self.vpn_name(id)),
            Status::Reconnecting(id) => format!("Reconnecting to {}… (attempt {})", self.vpn_name(id), self.retries),
        }
    }

    fn append_items(&self, menu: &Menu) {

        if self.vpns.is_empty() {
            let _ = menu.append(&MenuItem::new("No VPNs yet", false, None));
        }
        let active = self.active_vpn();
        for v in &self.vpns {
            let checked = active == Some(v.id.as_str());
            let _ = menu.append(&CheckMenuItem::with_id(format!("vpn:{}", v.id), &v.name, true, checked, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());

        let can_disconnect =
            self.tunnel.is_some() || matches!(self.status, Status::SigningIn(_) | Status::Reconnecting(_));
        let _ = menu.append(&MenuItem::with_id("disconnect", "Disconnect", can_disconnect, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("add", "Add VPN…", true, None));

        if !self.vpns.is_empty() {
            for (prefix, title) in [("edit", "Edit VPN"), ("forget", "Forget Saved Login"), ("remove", "Remove VPN")] {
                let sub = Submenu::new(title, true);
                for v in &self.vpns {
                    let _ = sub.append(&MenuItem::with_id(format!("{prefix}:{}", v.id), &v.name, true, None));
                }
                let _ = menu.append(&sub);
            }
        }
        let _ = menu.append(&MenuItem::with_id("log", "Show Log", !self.log.is_empty(), None));
        let login = platform::launch_at_login();
        let _ = menu.append(&CheckMenuItem::with_id("login", "Start at Login", true, login, None));
        // Windows has no notifications (the tray icon shows the state).
        #[cfg(target_os = "macos")]
        {
            let on = notifications_wanted() && !platform::notifications_denied();
            let _ = menu.append(&CheckMenuItem::with_id("notifications", "Notifications", true, on, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "Quit FortiBar", true, None));
    }

    fn refresh(&mut self) {
        #[cfg(target_os = "macos")]
        tls::set_pins(&self.vpns);
        let menu = self.build_menu();
        let connected = matches!(self.status, Status::Connected(_));
        let title = match &self.status {
            Status::Connected(id) => Some(self.vpn_name(id)),
            Status::SigningIn(_) | Status::Connecting(_) | Status::Disconnecting(_) | Status::Reconnecting(_) => {
                Some("…".to_string())
            }
            // tray-icon ignores a None title on macOS, leaving the old one up.
            Status::Idle => Some(String::new()),
        };
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(menu)));
            // Title text shows next to the icon on macOS; Windows only has the tooltip.
            let tooltip = format!("FortiBar – {}", self.status_text());
            let _ = tray.set_tooltip(Some(tooltip));
            tray.set_title(title);
            let _ = tray.set_icon_with_as_template(Some(icon::shield(connected)), true);
        }
    }

    /// Returns true when the app should quit.
    fn on_menu(&mut self, target: &Target, id: &str) -> bool {
        let (action, vpn_id) = id.split_once(':').unwrap_or((id, ""));
        let vpn_id = vpn_id.to_string();
        match action {
            "vpn" => self.connect(target, vpn_id),
            "disconnect" => self.disconnect(),
            "add" => self.open_form(target, None),
            "edit" => self.open_form(target, self.vpn(&vpn_id).cloned()),
            "forget" => {
                let name = self.vpn_name(&vpn_id);
                let what = if self.vpn(&vpn_id).is_some_and(|v| v.sso) { "Microsoft login" } else { "password" };
                if confirm(&format!("Forget the saved {what} for {name}?"), "Forget") {
                    self.forget_login(target, &vpn_id);
                }
            }
            "remove" => {
                let name = self.vpn_name(&vpn_id);
                if confirm(&format!("Remove the VPN “{name}”?"), "Remove") {
                    if self.active_vpn() == Some(vpn_id.as_str()) {
                        self.disconnect();
                    }
                    self.forget_login(target, &vpn_id);
                    self.auth.remove(&vpn_id);
                    self.vpns.retain(|v| v.id != vpn_id);
                    self.save_vpns();
                    if self.form.as_ref().is_some_and(|f| f.2.as_deref() == Some(vpn_id.as_str())) {
                        self.form = None;
                    }
                }
            }
            "log" => {
                let path = config::data_dir().join("fortibar.log");
                let _ = std::fs::write(&path, self.log.iter().cloned().collect::<Vec<_>>().join("\n"));
                platform::open_text(&path);
            }
            "login" => platform::set_launch_at_login(!platform::launch_at_login()),
            "notifications" => {
                // macOS is blocking them: only System Settings can turn them
                // back on, so open it rather than just ticking the box.
                if platform::notifications_denied() {
                    set_notifications_wanted(true);
                    platform::open_notification_settings();
                } else {
                    set_notifications_wanted(!notifications_wanted());
                }
            }
            "quit" => {
                control::stop();
                if let Some(t) = &self.tunnel {
                    t.stop();
                    t.wait_exit(Duration::from_secs(8));
                }
                return true;
            }
            _ => {}
        }
        self.refresh();
        false
    }

    // ---- connect / disconnect -------------------------------------------

    fn connect(&mut self, target: &Target, id: String) {
        // Anything the user starts replaces an automatic reconnect.
        self.stop_reconnecting();
        match &self.status {
            // Clicking a VPN that's waiting to reconnect cancels that.
            Status::Reconnecting(cur) if *cur == id => {
                self.status = Status::Idle;
                return;
            }
            // Clicking the active VPN toggles it off.
            Status::Connected(cur) | Status::Connecting(cur) if *cur == id => return self.disconnect(),
            Status::SigningIn(cur) if *cur == id => {
                if let Some((_, w, _)) = &self.login {
                    w.set_focus();
                } else if let Some(a) = self.auth.get(&id) {
                    a.window.set_visible(true);
                    a.window.set_focus();
                }
                return;
            }
            Status::SigningIn(cur) => {
                if let Some(a) = self.auth.get(cur) {
                    a.window.set_visible(false);
                }
                self.login = None;
            }
            _ => {}
        }
        if let Some(t) = &self.tunnel {
            // Switch: bring the current tunnel down first, then sign in to
            // the new one when TunnelExited arrives.
            self.next_vpn = Some(id);
            self.status = Status::Disconnecting(t.vpn_id.clone());
            t.stop();
            return;
        }
        self.start_auth(target, &id);
    }

    fn disconnect(&mut self) {
        self.next_vpn = None;
        self.stop_reconnecting();
        if let Some(t) = &self.tunnel {
            self.status = Status::Disconnecting(t.vpn_id.clone());
            t.stop();
        } else if let Status::Reconnecting(_) = &self.status {
            self.status = Status::Idle;
        } else if let Status::SigningIn(id) = &self.status {
            if let Some(a) = self.auth.get(id) {
                a.window.set_visible(false);
            }
            self.login = None;
            self.status = Status::Idle;
        }
    }

    /// The password window was closed or cancelled.
    fn cancel_login(&mut self) {
        if let Some((id, _, _)) = self.login.take() {
            if matches!(&self.status, Status::SigningIn(s) if *s == id) {
                self.stop_reconnecting();
                self.status = Status::Idle;
                self.refresh();
            }
        }
    }

    fn start_auth(&mut self, target: &Target, id: &str) {
        self.start_auth_with(target, id, None);
    }

    fn stop_reconnecting(&mut self) {
        self.reconnecting = None;
        self.retries = 0;
        // Invalidates any attempt already scheduled.
        self.reconnect_token += 1;
    }

    /// The tunnel to `id` dropped, or an automatic attempt to bring it back
    /// failed: wait, then try again, or give up after MAX_RECONNECTS.
    fn schedule_reconnect(&mut self, id: &str) {
        let name = self.vpn_name(id);
        if self.retries >= MAX_RECONNECTS {
            let tries = self.retries;
            self.stop_reconnecting();
            self.status = Status::Idle;
            self.last_error = Some(format!("Couldn't reconnect to {name} after {tries} attempts."));
            notify("Couldn't reconnect the VPN", &name);
            return;
        }
        let delay = RECONNECT_DELAYS[(self.retries as usize).min(RECONNECT_DELAYS.len() - 1)];
        if self.retries == 0 {
            notify("VPN connection dropped – reconnecting", &name);
        }
        self.retries += 1;
        self.reconnect_token += 1;
        self.status = Status::Reconnecting(id.to_string());
        self.push_log(format!("FortiBar: reconnecting in {delay}s (attempt {} of {MAX_RECONNECTS})", self.retries));
        self.later(Duration::from_secs(delay), UserEvent::Reconnect(id.to_string(), self.reconnect_token));
    }

    fn on_reconnect(&mut self, target: &Target, id: String, token: u64) {
        let waiting = matches!(&self.status, Status::Reconnecting(s) if *s == id);
        if !waiting || token != self.reconnect_token {
            return;
        }
        // Turned off in Edit VPN while waiting.
        if !self.vpn(&id).is_some_and(|v| v.auto_reconnect) {
            self.stop_reconnecting();
            self.status = Status::Idle;
            return self.refresh();
        }
        self.reconnecting = Some(id.clone());
        let last_error = self.last_error.clone();
        self.start_auth(target, &id);
        // Keep saying why we're reconnecting until it's back up.
        if self.last_error.is_none() {
            self.last_error = last_error;
        }
        self.refresh();
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(line);
        if self.log.len() > control::MAX_LOG_LINES {
            self.log.pop_front();
        }
    }

    /// `retry` carries the credentials of a password tunnel being restarted
    /// (after trusting its certificate), so the user isn't asked again.
    fn start_auth_with(&mut self, target: &Target, id: &str, retry: Option<PasswordLogin>) {
        let Some(vpn) = self.vpn(id).cloned() else { return };
        self.last_error = None;
        if vpn.sso {
            self.saml_retried = false;
            return self.start_saml(target, &vpn);
        }
        self.status = Status::SigningIn(id.to_string());
        let saved = || {
            let password = (!vpn.username.is_empty() && vpn.save_login).then(|| vpn.saved_password()).flatten()?;
            Some(PasswordLogin { username: vpn.username.clone(), password, typed: false })
        };
        match retry.or_else(saved) {
            Some(login) => self.start_tunnel(&vpn, Auth::Password {
                username: login.username.clone(),
                password: login.password.clone(),
            }, Some(login)),
            None => self.open_login(target, &vpn, ""),
        }
        self.refresh();
    }

    fn start_saml(&mut self, target: &Target, vpn: &Vpn) {
        let id = vpn.id.as_str();
        self.attempt += 1;
        self.status = Status::SigningIn(id.to_string());
        self.ensure_auth_window(target, vpn);

        let webview = &self.auth[id].webview;
        // Drop any old session cookie so we only pick up a fresh one.
        if let Ok(cookies) = webview.cookies() {
            for c in cookies.iter().filter(|c| c.name() == SESSION_COOKIE) {
                let _ = webview.delete_cookie(c);
            }
        }
        // redirect=1 makes FortiGate finish on 127.0.0.1:8020/?id=…, which we
        // intercept; gateways that just set the cookie are caught by polling.
        let _ = webview.load_url(&format!("{}/remote/saml/start?redirect=1", vpn.base_url()));

        self.later(Duration::from_millis(800), UserEvent::AuthCheck(id.to_string(), Some(self.attempt)));
        self.later(Duration::from_secs(4), UserEvent::AuthReveal(id.to_string(), self.attempt));
        self.refresh();
    }

    /// The gateway ended SSO sign-in without issuing a session cookie, e.g.
    /// because the Microsoft login (waiting for MFA) took longer than it
    /// allows. Microsoft is signed in by now, so a fresh start usually goes
    /// straight through; restart once, then give up.
    fn saml_failed(&mut self, target: &Target, id: &str) {
        let Some(vpn) = self.vpn(id).cloned() else { return };
        if !self.saml_retried {
            self.saml_retried = true;
            self.push_log("FortiBar: the gateway didn't issue a session after sign-in; trying again".to_string());
            return self.start_saml(target, &vpn);
        }
        if let Some(a) = self.auth.get(id) {
            a.window.set_visible(false);
        }
        self.status = Status::Idle;
        if self.reconnecting.take().as_deref() == Some(id) {
            self.last_error = Some(format!("The gateway didn't accept the sign-in to {}.", vpn.name));
            self.schedule_reconnect(id);
        } else {
            self.fail(
                &format!("Couldn't sign in to {}", vpn.name),
                "Microsoft sign-in finished, but the gateway didn't accept it. Try connecting again.",
            );
        }
        self.refresh();
    }

    fn ensure_auth_window(&mut self, target: &Target, vpn: &Vpn) {
        if self.auth.contains_key(&vpn.id) {
            return;
        }
        let window = WindowBuilder::new()
            .with_title(format!("Sign in – {}", vpn.name))
            .with_inner_size(LogicalSize::new(520.0, 680.0))
            .with_visible(false)
            .build(target)
            .expect("create sign-in window");
        center(&window);

        let nav_proxy = self.proxy.clone();
        let load_proxy = self.proxy.clone();
        let nav_id = vpn.id.clone();
        let load_id = vpn.id.clone();
        // Each VPN gets its own browser profile, so each keeps its own
        // Microsoft login even when several share one gateway.
        #[cfg(target_os = "macos")]
        let builder = WebViewBuilder::new().with_data_store_identifier(vpn.store_id());
        #[cfg(windows)]
        let mut context = WebContext::new(Some(vpn.profile_dir()));
        #[cfg(windows)]
        let builder = WebViewBuilder::new_with_web_context(&mut context);

        let webview = builder
            .with_navigation_handler(move |url| {
                let local = url.starts_with("http://127.0.0.1:8020") || url.starts_with("http://localhost:8020");
                if local {
                    // Sent even without an id, so a redirect the gateway sends
                    // when it rejects the sign-in isn't silently dropped.
                    let saml_id = query_param(&url, "id").unwrap_or_default();
                    let _ = nav_proxy.send_event(UserEvent::AuthSamlId(nav_id.clone(), saml_id));
                }
                !local
            })
            .with_on_page_load_handler(move |event, _url| {
                if let PageLoadEvent::Finished = event {
                    let _ = load_proxy.send_event(UserEvent::AuthCheck(load_id.clone(), None));
                }
            })
            .build(&window)
            .expect("create sign-in webview");

        restore_session_cookies(vpn, &webview);
        self.auth.insert(
            vpn.id.clone(),
            AuthWindow {
                window,
                webview,
                #[cfg(windows)]
                _context: context,
            },
        );
    }

    fn forget_login(&mut self, target: &Target, id: &str) {
        let Some(vpn) = self.vpn(id).cloned() else { return };
        vpn.forget_password();
        if !vpn.sso && !self.auth.contains_key(id) && !vpn.cookie_file().exists() {
            return;
        }
        self.ensure_auth_window(target, &vpn);
        let _ = self.auth[id].webview.clear_all_browsing_data();
        let _ = std::fs::remove_file(vpn.cookie_file());
    }

    /// Returns the fresh FortiGate session cookie if sign-in has completed.
    fn find_session_cookie(&self, vpn: &Vpn) -> Option<String> {
        let webview = &self.auth.get(&vpn.id)?.webview;
        let cookies = webview.cookies().ok()?;
        let value = cookies
            .iter()
            .find(|c| {
                let d = c.domain().unwrap_or("").trim_start_matches('.');
                c.name() == SESSION_COOKIE
                    && !c.value().is_empty()
                    && (vpn.host == d || vpn.host.ends_with(&format!(".{d}")))
            })?
            .value()
            .to_string();
        save_session_cookies(vpn, &cookies);
        Some(value)
    }

    fn start_tunnel(&mut self, vpn: &Vpn, auth: Auth, password_login: Option<PasswordLogin>) {
        if !config::tunnel_program().exists() {
            self.status = Status::Idle;
            #[cfg(target_os = "macos")]
            self.fail("FortiBar isn't set up yet", "Run ./setup.sh in the FortiBar project folder once, then try again.");
            #[cfg(windows)]
            self.fail(
                "openconnect is missing",
                "The openconnect folder should sit next to FortiBar.exe. Re-extract the FortiBar zip.",
            );
            return;
        }
        self.session += 1;
        self.pending_cert = None;
        self.sudo_failed = false;
        self.auth_failed = false;
        self.password_login = password_login;
        // Keep the log across automatic reconnects, so it still shows why
        // the tunnel dropped.
        if self.reconnecting.is_none() {
            self.log.clear();
        }
        match Tunnel::start(vpn, &auth, self.session, self.proxy.clone()) {
            Ok(t) => {
                self.tunnel = Some(t);
                self.status = Status::Connecting(vpn.id.clone());
            }
            Err(e) => {
                self.status = Status::Idle;
                self.fail("Couldn't start the VPN", &e.to_string());
            }
        }
    }

    // ---- events -----------------------------------------------------------

    fn on_event(&mut self, target: &Target, event: UserEvent) {
        match event {
            UserEvent::Menu(_) => {}
            UserEvent::AuthCheck(id, attempt) => {
                let signing_in = matches!(&self.status, Status::SigningIn(s) if *s == id);
                if !signing_in || attempt.is_some_and(|a| a != self.attempt) {
                    return;
                }
                let Some(vpn) = self.vpn(&id).cloned() else { return };
                if let Some(cookie) = self.find_session_cookie(&vpn) {
                    self.auth[&id].window.set_visible(false);
                    self.start_tunnel(&vpn, Auth::Cookie(cookie), None);
                    self.refresh();
                } else if let Some(a) = attempt {
                    self.later(Duration::from_millis(800), UserEvent::AuthCheck(id, Some(a)));
                }
            }
            UserEvent::AuthSamlId(id, saml_id) => {
                let signing_in = matches!(&self.status, Status::SigningIn(s) if *s == id);
                let Some(vpn) = self.vpn(&id).filter(|_| signing_in).cloned() else { return };
                let safe = saml_id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
                if saml_id.is_empty() || !safe {
                    return self.saml_failed(target, &id);
                }
                // Exchanging the id sets SVPNCOOKIE in this profile; polling
                // picks it up, and AuthFailed fires if it never comes.
                let url = format!("{}/remote/saml/auth_id?id={saml_id}", vpn.base_url());
                let _ = self.auth[&id].webview.load_url(&url);
                self.later(SAML_EXCHANGE_TIMEOUT, UserEvent::AuthFailed(id, self.attempt));
            }
            UserEvent::AuthFailed(id, attempt) => {
                let signing_in = matches!(&self.status, Status::SigningIn(s) if *s == id);
                if !signing_in || attempt != self.attempt {
                    return;
                }
                let Some(vpn) = self.vpn(&id).cloned() else { return };
                // One last look, in case it arrived since the last poll.
                if let Some(cookie) = self.find_session_cookie(&vpn) {
                    self.auth[&id].window.set_visible(false);
                    self.start_tunnel(&vpn, Auth::Cookie(cookie), None);
                    return self.refresh();
                }
                self.saml_failed(target, &id);
            }
            UserEvent::AuthReveal(id, attempt) => {
                let signing_in = matches!(&self.status, Status::SigningIn(s) if *s == id);
                if signing_in && attempt == self.attempt {
                    if let Some(a) = self.auth.get(&id) {
                        a.window.set_visible(true);
                        a.window.set_focus();
                    }
                }
            }
            UserEvent::Form(body) => self.on_form(body),
            UserEvent::Login(body) => self.on_login(body),
            UserEvent::TunnelLine(session, line) => self.on_tunnel_line(session, line),
            UserEvent::TunnelExited(session, code) => self.on_tunnel_exit(target, session, code),
            UserEvent::Reconnect(id, token) => self.on_reconnect(target, id, token),
            UserEvent::Control(command, reply) => {
                let _ = reply.send(self.on_control(target, command));
            }
            UserEvent::NotificationsChanged => self.refresh(),
        }
    }

    fn on_tunnel_line(&mut self, session: u64, line: String) {
        if self.tunnel.as_ref().map(|t| t.session) != Some(session) {
            return;
        }
        // openfortivpn / openconnect respectively.
        if line.contains("Tunnel is up and running") || line.contains("Configured as ") {
            if let Status::Connecting(id) = &self.status {
                let id = id.clone();
                // A typed password just proved right; keep it if asked to.
                if let (Some(login), Some(vpn)) = (self.password_login.take(), self.vpn(&id)) {
                    if login.typed && vpn.save_login {
                        vpn.save_password(&login.password);
                    }
                }
                match self.reconnecting.take() {
                    Some(_) => notify("VPN reconnected", &self.vpn_name(&id)),
                    None => notify("VPN connected", &self.vpn_name(&id)),
                }
                self.last_error = None;
                self.connected_at = Some(Instant::now());
                let host = self.vpn(&id).map(|v| v.keepalive_host.clone()).unwrap_or_default();
                if let (false, Some(t)) = (host.is_empty(), &self.tunnel) {
                    t.keep_alive(host, self.proxy.clone());
                }
                self.status = Status::Connected(id);
                self.refresh();
            }
        }
        if line.contains("a password is required") || line.contains("a terminal is required") {
            self.sudo_failed = true;
        }
        // openfortivpn / openconnect when the gateway refuses the credentials.
        if line.contains("Could not authenticate to gateway") || line.contains("Invalid credentials") {
            self.auth_failed = true;
        }
        // Both tools print the flag to add when the gateway certificate isn't
        // trusted: openfortivpn --trusted-cert <hex>, openconnect --servercert <pin>.
        let suggestion = line.split("--trusted-cert").nth(1).or_else(|| line.split("--servercert").nth(1));
        if let Some(rest) = suggestion {
            let digest = rest.trim_start_matches([' ', '=']).split_whitespace().next().unwrap_or("");
            if config::valid_cert(digest) {
                self.pending_cert = Some(digest.to_string());
            }
        }
        self.push_log(line);
    }

    fn on_tunnel_exit(&mut self, target: &Target, session: u64, code: Option<i32>) {
        if self.tunnel.as_ref().map(|t| t.session) != Some(session) {
            return;
        }
        let tunnel = self.tunnel.take().unwrap();
        let name = self.vpn_name(&tunnel.vpn_id);
        let prev = std::mem::replace(&mut self.status, Status::Idle);
        let password_login = self.password_login.take();
        let password_vpn = self.vpn(&tunnel.vpn_id).filter(|v| !v.sso).cloned();
        let pending_cert = self.pending_cert.take();
        let next_vpn = self.next_vpn.take();
        // Set when this tunnel was itself an automatic reconnect attempt.
        let auto_attempt = self.reconnecting.take() == Some(tunnel.vpn_id.clone());
        let auto_reconnect = self.vpn(&tunnel.vpn_id).is_some_and(|v| v.auto_reconnect);
        let uptime = self.connected_at.take().map(|t| t.elapsed());

        // Only offer to trust the certificate if the user is still waiting
        // for this VPN, not after they disconnected or switched away.
        if let (Some(digest), Status::Connecting(_)) = (pending_cert, &prev) {
            let msg = format!(
                "The certificate for {name} isn't trusted by macOS.\n\nSHA-256: {digest}\n\nTrust it for this VPN?"
            );
            if confirm(&msg, "Trust") {
                if let Some(v) = self.vpns.iter_mut().find(|v| v.id == tunnel.vpn_id) {
                    v.trusted_cert = digest;
                }
                self.save_vpns();
                self.start_auth_with(target, &tunnel.vpn_id, password_login);
            } else {
                self.last_error = Some(format!("The certificate for {name} wasn't trusted."));
            }
        } else if let (true, Status::Connecting(_), Some(vpn)) = (self.auth_failed, &prev, &password_vpn) {
            // Wrong password: drop any saved one and ask again.
            self.retries = 0;
            vpn.forget_password();
            self.status = Status::SigningIn(vpn.id.clone());
            self.last_error = Some("The gateway rejected the username or password.".to_string());
            self.open_login(target, vpn, "The gateway rejected the username or password.");
        } else {
            match prev {
                Status::Disconnecting(_) => match next_vpn {
                    Some(next) => self.start_auth(target, &next),
                    None => notify("VPN disconnected", &name),
                },
                Status::Connected(_) => {
                    self.last_error = Some(format!("The connection to {name} dropped."));
                    if !auto_reconnect {
                        notify("VPN connection dropped", &name);
                    } else {
                        // Up for a while: a fresh drop, not the same one again.
                        if uptime.is_some_and(|u| u >= STABLE_AFTER) {
                            self.retries = 0;
                        }
                        self.schedule_reconnect(&tunnel.vpn_id);
                    }
                }
                _ if self.sudo_failed => {
                    self.stop_reconnecting();
                    self.fail(
                        "FortiBar needs setup",
                        "sudo asked for a password. Run ./setup.sh in the FortiBar project folder to install the helper rule.",
                    )
                }
                // An automatic attempt failed (e.g. the network isn't back
                // yet): try again later instead of showing an alert.
                _ if auto_attempt && auto_reconnect => self.schedule_reconnect(&tunnel.vpn_id),
                _ => {
                    self.stop_reconnecting();
                    let errors: Vec<&str> = self
                        .log
                        .iter()
                        .filter(|l| l.starts_with("ERROR") || l.contains("rror"))
                        .map(String::as_str)
                        .collect();
                    let tail: Vec<&str> = if errors.is_empty() {
                        self.log.iter().rev().take(6).rev().map(String::as_str).collect()
                    } else {
                        errors.into_iter().rev().take(6).rev().collect()
                    };
                    let code = code.map_or("signal".to_string(), |c| c.to_string());
                    let tail = tail.join("\n");
                    self.fail(&format!("Couldn't connect to {name} (exit {code})"), &tail);
                }
            }
        }
        self.refresh();
    }

    // ---- add / edit form ------------------------------------------------

    fn open_form(&mut self, target: &Target, vpn: Option<Vpn>) {
        if let Some((w, ..)) = &self.form {
            w.set_focus();
            return;
        }
        let title = if vpn.is_some() { "Edit VPN Connection" } else { "New VPN Connection" };
        let editing = vpn.as_ref().map(|v| v.id.clone());
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(640.0, 800.0))
            .with_resizable(false)
            .build(target)
            .expect("create form window");
        center(&window);

        let init = format!("window.__VPN = {};", serde_json::to_string(&vpn).unwrap());
        let proxy = self.proxy.clone();
        let webview = WebViewBuilder::new()
            .with_initialization_script(&init)
            .with_html(include_str!("form.html"))
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::Form(req.body().clone()));
            })
            .build(&window)
            .expect("create form webview");
        window.set_focus();
        self.form = Some((window, webview, editing));
    }

    fn on_form(&mut self, body: String) {
        // A second Save queued before the first closed the form would
        // otherwise add the VPN twice.
        let Some((_, _, editing)) = &self.form else { return };
        let editing = editing.clone();
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) else { return };
        let s = |k: &str| msg[k].as_str().unwrap_or("").trim().to_string();
        if s("action") != "save" {
            self.form = None;
            return;
        }
        let show_error = |text: &str| {
            if let Some((_, wv, _)) = &self.form {
                let _ = wv.evaluate_script(&format!("showError({})", serde_json::to_string(text).unwrap()));
            }
        };

        let name = s("name");
        if name.is_empty() {
            return show_error("Enter a connection name.");
        }
        let Some((host, gw_port)) = config::parse_gateway(&s("gateway")) else {
            return show_error("Enter a valid gateway, e.g. vpn.example.com or vpn.example.com:10443");
        };
        let port = match msg["port"].as_u64() {
            Some(p) if (1..=65535).contains(&p) => p as u16,
            Some(_) => return show_error("Port must be between 1 and 65535."),
            None => gw_port.unwrap_or(443),
        };
        let mut trusted_cert = s("trusted_cert");
        if !trusted_cert.starts_with("pin-sha256:") {
            trusted_cert.make_ascii_lowercase();
        }
        if !trusted_cert.is_empty() && !config::valid_cert(&trusted_cert) {
            return show_error("Trusted cert must be a 64-character SHA-256 hex digest.");
        }

        let sso = msg["sso"].as_bool().unwrap_or(true);
        let save_login = msg["save_login"].as_bool().unwrap_or(false);
        let username = s("username");
        if !sso && save_login && username.is_empty() {
            return show_error("Enter a username, or choose Prompt on login.");
        }
        if !username.is_empty() && !config::valid_username(&username) {
            return show_error("The username must be at most 64 characters.");
        }

        let Some(dns_domains) = config::parse_dns_domains(&s("dns_domains")) else {
            return show_error("DNS domains must be domain names like corp.example.com, separated by commas (at most 16).");
        };
        let Some(keepalive_host) = config::parse_keepalive_host(&s("keepalive_host")) else {
            return show_error("The keep-alive host must be an IP address or hostname, e.g. 10.1.2.3.");
        };

        let vpn = Vpn {
            id: editing.unwrap_or_else(config::new_id),
            name,
            description: s("description"),
            host,
            port,
            trusted_cert,
            sso,
            username,
            save_login,
            dns_domains,
            keepalive_host,
            auto_reconnect: msg["auto_reconnect"].as_bool().unwrap_or(false),
        };
        match self.vpns.iter_mut().find(|v| v.id == vpn.id) {
            Some(existing) => {
                // A saved password belongs to the old account / gateway.
                if vpn.sso
                    || !vpn.save_login
                    || existing.username != vpn.username
                    || existing.host != vpn.host
                    || existing.port != vpn.port
                    || existing.trusted_cert != vpn.trusted_cert
                {
                    existing.forget_password();
                }
                *existing = vpn;
            }
            None => self.vpns.push(vpn),
        }
        self.save_vpns();
        self.form = None;
        self.refresh();
    }
}

impl App {
    // ---- password prompt ------------------------------------------------

    fn open_login(&mut self, target: &Target, vpn: &Vpn, error: &str) {
        self.login = None;
        let window = WindowBuilder::new()
            .with_title(format!("Connect – {}", vpn.name))
            .with_inner_size(LogicalSize::new(440.0, 280.0))
            .with_resizable(false)
            .build(target)
            .expect("create login window");
        center(&window);

        let init = format!(
            "window.__LOGIN = {};",
            serde_json::json!({ "name": vpn.name, "username": vpn.username, "save": vpn.save_login, "error": error })
        );
        let proxy = self.proxy.clone();
        let webview = WebViewBuilder::new()
            .with_initialization_script(&init)
            .with_html(include_str!("login.html"))
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::Login(req.body().clone()));
            })
            .build(&window)
            .expect("create login webview");
        window.set_focus();
        self.login = Some((vpn.id.clone(), window, webview));
    }

    fn on_login(&mut self, body: String) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) else { return };
        if msg["action"].as_str() != Some("connect") {
            return self.cancel_login();
        }
        let Some((id, _, webview)) = &self.login else { return };
        let id = id.clone();
        let show_error = |text: &str| {
            let _ = webview.evaluate_script(&format!("showError({})", serde_json::to_string(text).unwrap()));
        };
        let username = msg["username"].as_str().unwrap_or("").trim().to_string();
        let password = msg["password"].as_str().unwrap_or("").to_string();
        if !config::valid_username(&username) {
            return show_error("Enter a username (at most 64 characters).");
        }
        if !config::valid_password(&password) {
            return show_error("Enter a password (at most 256 characters).");
        }
        if !matches!(&self.status, Status::SigningIn(s) if *s == id) {
            self.login = None;
            return;
        }
        self.login = None;

        // Remember a changed username; a password saved for the old one is stale.
        let Some(vpn) = self.vpns.iter_mut().find(|v| v.id == id) else { return };
        if vpn.username != username {
            vpn.forget_password();
            vpn.username = username.clone();
            self.save_vpns();
        }
        let vpn = self.vpn(&id).cloned().unwrap();
        let auth = Auth::Password { username: username.clone(), password: password.clone() };
        self.start_tunnel(&vpn, auth, Some(PasswordLogin { username, password, typed: true }));
        self.refresh();
    }
}

impl App {
    // ---- AI agents (MCP) ------------------------------------------------

    /// Shows an alert and keeps its text for get_status.
    fn fail(&mut self, title: &str, message: &str) {
        self.last_error = Some(format!("{title}: {message}"));
        alert(title, message);
    }

    fn on_control(&mut self, target: &Target, command: Command) -> Value {
        match command {
            Command::Status => {}
            Command::Logs(n) => {
                let skip = self.log.len().saturating_sub(n);
                let lines: Vec<&String> = self.log.iter().skip(skip).collect();
                return json!({ "lines": lines, "status": self.status_json() });
            }
            Command::Connect(name) => {
                let found = self.vpn(&name).or_else(|| self.vpns.iter().find(|v| v.name.eq_ignore_ascii_case(&name)));
                let Some(vpn) = found.cloned() else {
                    let names: Vec<&str> = self.vpns.iter().map(|v| v.name.as_str()).collect();
                    let error = match names.is_empty() {
                        true => format!("There's no VPN called “{name}”. No VPNs are set up yet."),
                        false => format!("There's no VPN called “{name}”. The VPNs are: {}.", names.join(", ")),
                    };
                    return json!({ "error": error });
                };
                // Unlike the menu, asking again for the VPN that's already
                // up (or coming up) leaves it alone instead of toggling it off.
                let active = self.active_vpn() == Some(vpn.id.as_str());
                if !active {
                    self.connect(target, vpn.id.clone());
                    self.refresh();
                }
                return json!({ "target": { "id": vpn.id, "name": vpn.name }, "status": self.status_json() });
            }
            Command::Disconnect => {
                if !matches!(self.status, Status::Idle) {
                    self.disconnect();
                    self.refresh();
                }
            }
        }
        json!({ "status": self.status_json() })
    }

    fn status_json(&self) -> Value {
        let (state, id) = match &self.status {
            Status::Idle => ("disconnected", None),
            Status::SigningIn(id) => ("signing_in", Some(id)),
            Status::Connecting(id) => ("connecting", Some(id)),
            Status::Connected(id) => ("connected", Some(id)),
            Status::Disconnecting(id) => ("disconnecting", Some(id)),
            Status::Reconnecting(id) => ("reconnecting", Some(id)),
        };
        // A sign-in or password window is up and only the user can finish it.
        let waiting_for_user = match &self.status {
            Status::SigningIn(id) => {
                self.login.as_ref().is_some_and(|(l, ..)| l == id)
                    || self.auth.get(id).is_some_and(|a| a.window.is_visible())
            }
            _ => false,
        };
        let vpns: Vec<Value> = self
            .vpns
            .iter()
            .map(|v| {
                json!({
                    "id": v.id,
                    "name": v.name,
                    "description": v.description,
                    "gateway": v.host_port(),
                    "login": if v.sso { "microsoft_sso" } else { "password" },
                    "keepalive_host": v.keepalive_host,
                    "auto_reconnect": v.auto_reconnect,
                })
            })
            .collect();
        json!({
            "state": state,
            "vpn": id.map(|id| json!({ "id": id, "name": self.vpn_name(id) })),
            "waiting_for_user": waiting_for_user,
            "last_error": self.last_error,
            // Automatic attempts so far while reconnecting.
            "reconnect_attempt": (self.reconnecting.is_some() || matches!(self.status, Status::Reconnecting(_)))
                .then_some(self.retries),
            "vpns": vpns,
        })
    }
}

// ---- helpers -------------------------------------------------------------

/// Whether the user wants FortiBar's notifications (the Notifications menu
/// item). Off is recorded as a file in the data folder.
fn notifications_wanted() -> bool {
    !config::data_dir().join("notifications-off").exists()
}

fn set_notifications_wanted(on: bool) {
    let path = config::data_dir().join("notifications-off");
    if on {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, "");
    }
    platform::set_notifications_muted(!on);
}

fn center(window: &Window) {
    if let Some(monitor) = window.current_monitor() {
        let scale = monitor.scale_factor();
        let screen = monitor.size().to_logical::<f64>(scale);
        let size = window.outer_size().to_logical::<f64>(scale);
        window.set_outer_position(LogicalPosition::new(
            (screen.width - size.width) / 2.0,
            (screen.height - size.height) / 3.0,
        ));
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then(|| v.to_string())
    })
}

/// Session-only cookies (e.g. Microsoft's ESTSAUTH when "Stay signed in" is
/// off) die with the app. Save them per VPN so a restart still logs in
/// silently. Persistent cookies are already kept by the WebKit profile.
fn save_session_cookies(vpn: &Vpn, cookies: &[Cookie<'static>]) {
    let lines: Vec<String> = cookies
        .iter()
        .filter(|c| c.name() != SESSION_COOKIE && matches!(c.expires(), None | Some(Expiration::Session)))
        .map(|c| c.to_string())
        .collect();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(vpn.cookie_file());
    if let Ok(mut f) = file {
        use std::io::Write;
        let _ = f.write_all(lines.join("\n").as_bytes());
    }
}

fn restore_session_cookies(vpn: &Vpn, webview: &WebView) {
    let Ok(text) = std::fs::read_to_string(vpn.cookie_file()) else { return };
    for line in text.lines().filter(|l| !l.is_empty()) {
        if let Ok(c) = Cookie::parse(line.to_string()) {
            let _ = webview.set_cookie(&c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_saml_id() {
        assert_eq!(query_param("http://127.0.0.1:8020/?id=abc123", "id").as_deref(), Some("abc123"));
        assert_eq!(query_param("http://127.0.0.1:8020/?x=1&id=Zz-9#frag", "id").as_deref(), Some("Zz-9"));
        assert_eq!(query_param("http://127.0.0.1:8020/", "id"), None);
    }

    #[test]
    fn session_cookie_round_trips() {
        let c = Cookie::build(("ESTSAUTH", "0.AXo-abc_123"))
            .domain(".login.microsoftonline.com")
            .path("/")
            .secure(true)
            .http_only(true)
            .same_site(wry::cookie::SameSite::None)
            .expires(Expiration::Session)
            .build();
        let parsed = Cookie::parse(c.to_string()).unwrap();
        assert_eq!(parsed.name(), "ESTSAUTH");
        assert_eq!(parsed.value(), "0.AXo-abc_123");
        assert_eq!(parsed.domain(), Some("login.microsoftonline.com"));
        assert_eq!(parsed.secure(), Some(true));
        assert_eq!(parsed.http_only(), Some(true));
        assert!(matches!(parsed.expires(), None | Some(Expiration::Session)));
    }
}
