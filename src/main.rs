#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod icon;
mod platform;
#[cfg(target_os = "macos")]
mod tls;
mod tunnel;

use config::Vpn;
use platform::{alert, confirm, notify};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};
use tunnel::Tunnel;
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
    AuthSamlId(String, String),
    /// Sign-in didn't complete silently; show the window to the user.
    AuthReveal(String, u64),
    Form(String),
    TunnelLine(u64, String),
    TunnelExited(u64, Option<i32>),
}

enum Status {
    Idle,
    SigningIn(String),
    Connecting(String),
    Connected(String),
    Disconnecting(String),
}

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

struct App {
    proxy: EventLoopProxy<UserEvent>,
    vpns: Vec<Vpn>,
    tray: Option<TrayIcon>,
    _app_menu: Option<Menu>,
    auth: HashMap<String, AuthWindow>,
    form: Option<(Window, WebView)>,
    status: Status,
    tunnel: Option<Tunnel>,
    /// VPN to connect to once the current tunnel has shut down.
    next_vpn: Option<String>,
    /// Certificate digest openfortivpn asked us to trust, if any.
    pending_cert: Option<String>,
    sudo_failed: bool,
    attempt: u64,
    session: u64,
    log: VecDeque<String>,
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
        status: Status::Idle,
        tunnel: None,
        next_vpn: None,
        pending_cert: None,
        sudo_failed: false,
        attempt: 0,
        session: 0,
        log: VecDeque::new(),
    };

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
                if app.form.as_ref().is_some_and(|(w, _)| w.id() == window_id) {
                    app.form = None;
                } else if let Some(id) =
                    app.auth.iter().find(|(_, a)| a.window.id() == window_id).map(|(id, _)| id.clone())
                {
                    app.auth[&id].window.set_visible(false);
                    if matches!(&app.status, Status::SigningIn(s) if *s == id) {
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

    fn vpn_name(&self, id: &str) -> String {
        self.vpn(id).map(|v| v.name.clone()).unwrap_or_default()
    }

    fn active_vpn(&self) -> Option<&str> {
        match &self.status {
            Status::SigningIn(id) | Status::Connecting(id) | Status::Connected(id) => Some(id),
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

        let can_disconnect = self.tunnel.is_some() || matches!(self.status, Status::SigningIn(_));
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
            Status::SigningIn(_) | Status::Connecting(_) | Status::Disconnecting(_) => Some("…".to_string()),
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
                if confirm(&format!("Forget the saved Microsoft login for {name}?"), "Forget") {
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
                    config::save(&self.vpns);
                }
            }
            "log" => {
                let path = config::data_dir().join("fortibar.log");
                let _ = std::fs::write(&path, self.log.iter().cloned().collect::<Vec<_>>().join("\n"));
                platform::open_text(&path);
            }
            "login" => platform::set_launch_at_login(!platform::launch_at_login()),
            "quit" => {
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
        match &self.status {
            // Clicking the active VPN toggles it off.
            Status::Connected(cur) | Status::Connecting(cur) if *cur == id => return self.disconnect(),
            Status::SigningIn(cur) if *cur == id => {
                if let Some(a) = self.auth.get(&id) {
                    a.window.set_visible(true);
                    a.window.set_focus();
                }
                return;
            }
            Status::SigningIn(cur) => {
                if let Some(a) = self.auth.get(cur) {
                    a.window.set_visible(false);
                }
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
        if let Some(t) = &self.tunnel {
            self.status = Status::Disconnecting(t.vpn_id.clone());
            t.stop();
        } else if let Status::SigningIn(id) = &self.status {
            if let Some(a) = self.auth.get(id) {
                a.window.set_visible(false);
            }
            self.status = Status::Idle;
        }
    }

    fn start_auth(&mut self, target: &Target, id: &str) {
        let Some(vpn) = self.vpn(id).cloned() else { return };
        self.attempt += 1;
        self.status = Status::SigningIn(id.to_string());
        self.ensure_auth_window(target, &vpn);

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
                    if let Some(saml_id) = query_param(&url, "id") {
                        let _ = nav_proxy.send_event(UserEvent::AuthSamlId(nav_id.clone(), saml_id));
                    }
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

    fn start_tunnel(&mut self, vpn: &Vpn, cookie: &str) {
        if !config::tunnel_program().exists() {
            self.status = Status::Idle;
            #[cfg(target_os = "macos")]
            alert("FortiBar isn't set up yet", "Run ./setup.sh in the FortiBar folder once, then try again.");
            #[cfg(windows)]
            alert(
                "openconnect is missing",
                "The openconnect folder should sit next to FortiBar.exe. Re-extract the FortiBar zip.",
            );
            return;
        }
        self.session += 1;
        self.pending_cert = None;
        self.sudo_failed = false;
        self.log.clear();
        match Tunnel::start(vpn, cookie, self.session, self.proxy.clone()) {
            Ok(t) => {
                self.tunnel = Some(t);
                self.status = Status::Connecting(vpn.id.clone());
            }
            Err(e) => {
                self.status = Status::Idle;
                alert("Couldn't start the VPN", &e.to_string());
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
                    self.start_tunnel(&vpn, &cookie);
                    self.refresh();
                } else if let Some(a) = attempt {
                    self.later(Duration::from_millis(800), UserEvent::AuthCheck(id, Some(a)));
                }
            }
            UserEvent::AuthSamlId(id, saml_id) => {
                let signing_in = matches!(&self.status, Status::SigningIn(s) if *s == id);
                let safe = saml_id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
                if let (true, true, Some(vpn)) = (signing_in, safe, self.vpn(&id)) {
                    // Exchanging the id sets SVPNCOOKIE in this profile.
                    let url = format!("{}/remote/saml/auth_id?id={saml_id}", vpn.base_url());
                    let _ = self.auth[&id].webview.load_url(&url);
                }
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
            UserEvent::TunnelLine(session, line) => self.on_tunnel_line(session, line),
            UserEvent::TunnelExited(session, code) => self.on_tunnel_exit(target, session, code),
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
                notify("VPN connected", &self.vpn_name(&id));
                self.status = Status::Connected(id);
                self.refresh();
            }
        }
        if line.contains("a password is required") || line.contains("a terminal is required") {
            self.sudo_failed = true;
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
        self.log.push_back(line);
        if self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    fn on_tunnel_exit(&mut self, target: &Target, session: u64, code: Option<i32>) {
        if self.tunnel.as_ref().map(|t| t.session) != Some(session) {
            return;
        }
        let tunnel = self.tunnel.take().unwrap();
        let name = self.vpn_name(&tunnel.vpn_id);
        let prev = std::mem::replace(&mut self.status, Status::Idle);

        if let Some(digest) = self.pending_cert.take() {
            let msg = format!(
                "The certificate for {name} isn't trusted by macOS.\n\nSHA-256: {digest}\n\nTrust it for this VPN?"
            );
            if confirm(&msg, "Trust") {
                if let Some(v) = self.vpns.iter_mut().find(|v| v.id == tunnel.vpn_id) {
                    v.trusted_cert = digest;
                }
                config::save(&self.vpns);
                return self.start_auth(target, &tunnel.vpn_id);
            }
        } else {
            match prev {
                Status::Disconnecting(_) => match self.next_vpn.take() {
                    Some(next) => return self.start_auth(target, &next),
                    None => notify("VPN disconnected", &name),
                },
                Status::Connected(_) => notify("VPN connection dropped", &name),
                _ if self.sudo_failed => alert(
                    "FortiBar needs setup",
                    "sudo asked for a password. Run ./setup.sh in the FortiBar folder to install the helper rule.",
                ),
                _ => {
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
                    alert(&format!("Couldn't connect to {name} (exit {code})"), &tail.join("\n"));
                }
            }
        }
        self.refresh();
    }

    // ---- add / edit form ------------------------------------------------

    fn open_form(&mut self, target: &Target, vpn: Option<Vpn>) {
        if let Some((w, _)) = &self.form {
            w.set_focus();
            return;
        }
        let title = if vpn.is_some() { "Edit VPN Connection" } else { "New VPN Connection" };
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(640.0, 520.0))
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
        self.form = Some((window, webview));
    }

    fn on_form(&mut self, body: String) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body) else { return };
        let s = |k: &str| msg[k].as_str().unwrap_or("").trim().to_string();
        if s("action") != "save" {
            self.form = None;
            return;
        }
        let show_error = |text: &str| {
            if let Some((_, wv)) = &self.form {
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

        let vpn = Vpn {
            id: msg["id"].as_str().map(String::from).unwrap_or_else(config::new_id),
            name,
            description: s("description"),
            host,
            port,
            trusted_cert,
        };
        match self.vpns.iter_mut().find(|v| v.id == vpn.id) {
            Some(existing) => *existing = vpn,
            None => self.vpns.push(vpn),
        }
        config::save(&self.vpns);
        self.form = None;
        self.refresh();
    }
}

// ---- helpers -------------------------------------------------------------

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
