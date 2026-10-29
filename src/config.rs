use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ROOT: &str = env!("CARGO_MANIFEST_DIR");

pub fn data_dir() -> PathBuf {
    let dir = PathBuf::from(ROOT).join("data");
    let _ = std::fs::create_dir_all(&dir);
    // Holds saved login cookies; keep it private to this user.
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    dir
}

pub fn helper_path() -> PathBuf {
    PathBuf::from(ROOT).join("vendor/bin/fortivpn-helper")
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

    /// Identifier of this VPN's private WebKit profile, so each hospital keeps
    /// its own Microsoft login.
    pub fn store_id(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(self.id.get(i * 2..i * 2 + 2).unwrap_or("00"), 16).unwrap_or(0);
        }
        out
    }

    pub fn cookie_file(&self) -> PathBuf {
        data_dir().join(format!("{}.cookies", self.id))
    }
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
    let valid = !host.is_empty()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    valid.then(|| (host.to_ascii_lowercase(), port))
}

fn config_file() -> PathBuf {
    data_dir().join("vpns.json")
}

pub fn load() -> Vec<Vpn> {
    std::fs::read_to_string(config_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(vpns: &[Vpn]) {
    if let Ok(json) = serde_json::to_string_pretty(vpns) {
        let _ = std::fs::write(config_file(), json);
    }
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
        assert_eq!(parse_gateway("host:99999"), None);
        assert_eq!(parse_gateway(""), None);
    }

    #[test]
    fn store_id_is_stable_and_distinct() {
        let a = Vpn { id: new_id(), name: "a".into(), description: String::new(), host: "h".into(), port: 443, trusted_cert: String::new() };
        let b = Vpn { id: new_id(), ..a.clone() };
        assert_eq!(a.id.len(), 32);
        assert_eq!(a.store_id(), a.store_id());
        assert_ne!(a.store_id(), b.store_id());
    }
}
