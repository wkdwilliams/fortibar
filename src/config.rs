use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Windows: the folder FortiBar.exe was unzipped into, which holds data\ and
/// openconnect\. macOS: the project folder, where data/ lived before it moved
/// to Application Support (only read to migrate it).
pub fn app_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    #[cfg(windows)]
    if let Some(dir) = exe.as_ref().and_then(|p| p.parent().map(PathBuf::from)) {
        return dir;
    }
    // Found at runtime rather than baked in, so the project folder can be
    // moved: the nearest ancestor of dist/FortiBar.app or target/release
    // that holds Cargo.toml.
    #[cfg(not(windows))]
    if let Some(dir) = exe.as_ref().and_then(|p| p.ancestors().find(|d| d.join("Cargo.toml").is_file())) {
        return dir.to_path_buf();
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// macOS: ~/Library/Application Support/FortiBar, so the app works from
/// anywhere (e.g. /Applications). Windows: %LOCALAPPDATA%\FortiBar, which only
/// this user can read, since it holds Microsoft session cookies. Older builds
/// kept data/ next to the app; it's moved over on first start.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let Some(dir) = base.map(|b| b.join("FortiBar")) else {
        return app_dir().join("data");
    };
    if !dir.join("vpns.json").exists() {
        migrate_data(&app_dir().join("data"), &dir);
    }
    let _ = std::fs::create_dir_all(&dir);
    // Holds saved login cookies; keep it private to this user.
    #[cfg(unix)]
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    dir
}

/// Moves data/ from where older builds kept it (the project folder on macOS,
/// next to FortiBar.exe on Windows).
fn migrate_data(old: &Path, new: &Path) {
    if !old.join("vpns.json").is_file() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Fails if `new` already holds files; copying covers that and a move
    // across volumes.
    if std::fs::rename(old, new).is_ok() {
        return;
    }
    if copy_dir(old, new).is_ok() {
        let _ = std::fs::remove_dir_all(old);
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else if !dest.exists() {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

/// macOS: root helper wrapping openfortivpn, installed by setup.sh.
/// Windows: bundled openconnect.
pub fn tunnel_program() -> PathBuf {
    #[cfg(target_os = "macos")]
    return PathBuf::from("/usr/local/libexec/fortibar/fortivpn-helper");
    #[cfg(windows)]
    return app_dir().join("openconnect").join("openconnect.exe");
}

/// Hex SHA-256 of the certificate, or (Windows/openconnect) a `pin-sha256:`
/// public-key pin as openconnect suggests it.
pub fn valid_cert(s: &str) -> bool {
    let hex = s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit());
    // openfortivpn and the sign-in window's pin check only understand the
    // hex digest; pin-sha256: is openconnect's (Windows) format.
    #[cfg(target_os = "macos")]
    return hex;
    #[cfg(not(target_os = "macos"))]
    {
        let pin = s.strip_prefix("pin-sha256:").is_some_and(|b| {
            !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
        });
        hex || pin
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Vpn {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub host: String,
    pub port: u16,
    /// SHA-256 digest of the gateway certificate, for gateways whose
    /// certificate doesn't validate against the system CAs.
    #[serde(default)]
    pub trusted_cert: String,
    /// true: Microsoft SSO (SAML) in a browser profile. false: username and
    /// password, handed straight to the tunnel program.
    #[serde(default = "yes")]
    pub sso: bool,
    #[serde(default)]
    pub username: String,
    /// Keep the password in the Keychain / Credential Manager after the first
    /// successful connect, instead of asking every time.
    #[serde(default)]
    pub save_login: bool,
    /// macOS: names under these domains are resolved by the VPN's DNS
    /// servers, on top of any domains the gateway pushes. With none at all,
    /// the VPN's DNS servers aren't used.
    #[serde(default)]
    pub dns_domains: Vec<String>,
    /// A host on the VPN that FortiBar pings every minute while connected,
    /// for gateways that drop idle tunnels. Empty: no keep-alive.
    #[serde(default)]
    pub keepalive_host: String,
    /// Reconnect by itself when the gateway or the network drops the tunnel.
    #[serde(default)]
    pub auto_reconnect: bool,
}

fn yes() -> bool {
    true
}

impl Vpn {
    pub fn base_url(&self) -> String {
        if self.port == 443 {
            format!("https://{}", self.host)
        } else {
            format!("https://{}:{}", self.host, self.port)
        }
    }

    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Identifier of this VPN's private WebKit profile, so each VPN keeps
    /// its own Microsoft login.
    #[cfg(target_os = "macos")]
    pub fn store_id(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(self.id.get(i * 2..i * 2 + 2).unwrap_or("00"), 16).unwrap_or(0);
        }
        out
    }

    /// This VPN's private WebView2 profile folder (Windows).
    #[cfg(windows)]
    pub fn profile_dir(&self) -> PathBuf {
        data_dir().join("profiles").join(&self.id)
    }

    pub fn cookie_file(&self) -> PathBuf {
        data_dir().join(format!("{}.cookies", self.id))
    }

    fn keyring_entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new("FortiBar", &self.id).ok()
    }

    pub fn saved_password(&self) -> Option<String> {
        self.keyring_entry()?.get_password().ok().filter(|p| valid_password(p))
    }

    pub fn save_password(&self, password: &str) {
        if let Some(e) = self.keyring_entry() {
            let _ = e.set_password(password);
        }
    }

    pub fn forget_password(&self) {
        if let Some(e) = self.keyring_entry() {
            let _ = e.delete_credential();
        }
    }
}

/// openfortivpn's limits; no control characters since each goes on one line
/// of the tunnel's stdin.
/// "corp.example.com, example.net" -> ["corp.example.com", "example.net"].
/// None if any entry isn't a plain domain name.
pub fn parse_dns_domains(input: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for d in input.split(|c: char| c == ',' || c.is_whitespace()).filter(|d| !d.is_empty()) {
        let d = d.trim_matches('.').to_ascii_lowercase();
        // At least two labels: a bare "com" would send every .com lookup to
        // the VPN's DNS servers (the helper refuses those too).
        let valid = d.contains('.')
            && d.len() <= 253
            && d.split('.').all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        if !valid {
            return None;
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    (out.len() <= 16).then_some(out)
}

pub fn valid_username(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.chars().any(char::is_control)
}

pub fn valid_password(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
}

pub fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let mut id = String::new();
    for salt in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(salt);
        id.push_str(&format!("{:016x}", h.finish()));
    }
    id
}

/// Accepts "vpn.example.org", "vpn.example.org:10443" or a pasted URL like
/// "https://vpn.example.org:10443/remote/login". Returns (host, port-if-given).
pub fn parse_gateway(input: &str) -> Option<(String, Option<u16>)> {
    let s = input.trim();
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next()?;
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().ok()?)),
        None => (s, None),
    };
    valid_host(host).then(|| (host.to_ascii_lowercase(), port))
}

/// A hostname or IPv4 address. Must start with a letter or digit: a leading
/// '-' would be read as an option by openfortivpn / openconnect / ping.
fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.starts_with(|c: char| c.is_ascii_alphanumeric())
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// The keep-alive host as typed in the form: "" (off) or a hostname / IPv4
/// address. None if it's neither.
pub fn parse_keepalive_host(input: &str) -> Option<String> {
    let host = input.trim();
    (host.is_empty() || valid_host(host)).then(|| host.to_ascii_lowercase())
}

fn config_file() -> PathBuf {
    data_dir().join("vpns.json")
}

/// The saved VPNs. A file that can't be read is moved aside to
/// vpns.json.bad first, so the next save can't overwrite the user's list.
pub fn load() -> Vec<Vpn> {
    let path = config_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return load_failed(&path, &e.to_string()),
    };
    match serde_json::from_str::<Vec<Vpn>>(&text) {
        Ok(mut vpns) => {
            // The id names files and the keychain entry; repair any that a
            // hand edit broke instead of letting VPNs share them.
            for v in &mut vpns {
                if !valid_id(&v.id) {
                    v.id = new_id();
                }
            }
            vpns
        }
        Err(e) => load_failed(&path, &e.to_string()),
    }
}

fn load_failed(path: &Path, error: &str) -> Vec<Vpn> {
    let kept = path.with_extension("json.bad");
    let note = match std::fs::rename(path, &kept) {
        Ok(()) => format!("It was kept as {}.", kept.display()),
        Err(_) => format!("Check {} before adding a VPN, which would overwrite it.", path.display()),
    };
    crate::platform::alert("FortiBar couldn't read your VPN list", &format!("{note}\n\n{error}"));
    Vec::new()
}

/// Writes to a temporary file and renames it over vpns.json, so a crash
/// mid-write leaves the old list intact.
pub fn save(vpns: &[Vpn]) -> std::io::Result<()> {
    let path = config_file();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(vpns).map_err(std::io::Error::other)?;
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)
}

/// Ids come from new_id(): 32 lowercase hex characters.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gateways() {
        assert_eq!(parse_gateway("vpn.example.org"), Some(("vpn.example.org".into(), None)));
        assert_eq!(parse_gateway(" VPN.example.org:10443 "), Some(("vpn.example.org".into(), Some(10443))));
        assert_eq!(
            parse_gateway("https://vpn.example.org:8443/remote/login?lang=en"),
            Some(("vpn.example.org".into(), Some(8443)))
        );
        assert_eq!(parse_gateway("10.1.2.3"), Some(("10.1.2.3".into(), None)));
        assert_eq!(parse_gateway("bad host"), None);
        assert_eq!(parse_gateway("-pfoo:443"), None);
        assert_eq!(parse_gateway(".example.com"), None);
        assert_eq!(parse_gateway("host:99999"), None);
        assert_eq!(parse_gateway(""), None);
    }

    #[test]
    fn validates_ids() {
        assert!(valid_id(&new_id()));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
    }

    #[test]
    fn validates_certs() {
        assert!(valid_cert("b075ab189818ed1e4a91858e0ccc4d412293ae88dc1ce981e5f5808a8b3fc200"));
        assert_eq!(valid_cert("pin-sha256:ABCdef+/123="), cfg!(not(target_os = "macos")));
        assert!(!valid_cert("pin-sha256:"));
        assert!(!valid_cert("zz"));
    }

    #[test]
    fn validates_credentials() {
        assert!(valid_username("user$name"));
        assert!(valid_username("dom\\user"));
        assert!(!valid_username(""));
        assert!(!valid_username("a\nb"));
        assert!(!valid_username(&"x".repeat(65)));
        assert!(valid_password(" p@ss word\\$ "));
        assert!(!valid_password(""));
        assert!(!valid_password("a\nb"));
    }

    #[test]
    fn old_configs_default_to_sso() {
        let v: Vpn = serde_json::from_str(r#"{"id":"ab","name":"n","host":"h","port":443}"#).unwrap();
        assert!(v.sso);
        assert!(v.username.is_empty());
        assert!(v.dns_domains.is_empty());
        assert!(v.keepalive_host.is_empty());
        assert!(!v.auto_reconnect);
    }

    #[test]
    fn keepalive_hosts() {
        assert_eq!(parse_keepalive_host("").as_deref(), Some(""));
        assert_eq!(parse_keepalive_host(" 10.1.2.3 ").as_deref(), Some("10.1.2.3"));
        assert_eq!(parse_keepalive_host("Intranet.Example.COM").as_deref(), Some("intranet.example.com"));
        assert_eq!(parse_keepalive_host("-c 999 host"), None);
        assert_eq!(parse_keepalive_host("-t1"), None);
        assert_eq!(parse_keepalive_host("a b"), None);
    }

    #[test]
    fn dns_domains() {
        assert_eq!(parse_dns_domains(""), Some(vec![]));
        assert_eq!(
            parse_dns_domains(" Intranet.Example.org, .corp.example.net  corp.example.net,"),
            Some(vec!["intranet.example.org".to_string(), "corp.example.net".to_string()])
        );
        assert_eq!(parse_dns_domains("a..b"), None);
        assert_eq!(parse_dns_domains("com"), None);
        assert_eq!(parse_dns_domains("bad_domain.com"), None);
        assert_eq!(parse_dns_domains("x.com;rm -rf"), None);
        assert_eq!(parse_dns_domains(&(0..17).map(|i| format!("d{i}.com")).collect::<Vec<_>>().join(",")), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn store_id_is_stable_and_distinct() {
        let a = Vpn {
            id: new_id(),
            name: "a".into(),
            description: String::new(),
            host: "h".into(),
            port: 443,
            trusted_cert: String::new(),
            sso: true,
            username: String::new(),
            save_login: false,
            dns_domains: Vec::new(),
            keepalive_host: String::new(),
            auto_reconnect: false,
        };
        let b = Vpn { id: new_id(), ..a.clone() };
        assert_eq!(a.id.len(), 32);
        assert_eq!(a.store_id(), a.store_id());
        assert_ne!(a.store_id(), b.store_id());
    }
}
