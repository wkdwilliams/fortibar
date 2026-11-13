//! Encrypted files for secrets that are too big for the OS credential store
//! (Windows Credential Manager holds at most 2,560 bytes per secret), such as
//! a VPN's saved Microsoft session cookies.
//!
//! One random key lives in the Keychain / Credential Manager. Each file is
//! `MAGIC || nonce || ChaCha20-Poly1305(contents)`. Without the key, nothing
//! is written: a secret is never left on disk unencrypted.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const MAGIC: &[u8] = b"FBSEAL1\n";
const NONCE_LEN: usize = 12;
const KEY_ACCOUNT: &str = "file-key";

/// Looked up once per run. On macOS, reading the key after the app was rebuilt
/// asks for the login password, and this keeps it to one prompt.
fn key() -> Option<&'static [u8; 32]> {
    static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    KEY.get_or_init(load_or_create_key).as_ref()
}

fn load_or_create_key() -> Option<[u8; 32]> {
    let entry = keyring::Entry::new("FortiBar", KEY_ACCOUNT).ok()?;
    match entry.get_password() {
        Ok(hex) => decode_key(&hex),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            getrandom::getrandom(&mut key).ok()?;
            entry.set_password(&encode_key(&key)).ok()?;
            Some(key)
        }
        // E.g. the user denied access to the Keychain.
        Err(_) => None,
    }
}

fn encode_key(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_key(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, pair) in bytes.chunks(2).enumerate() {
        key[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

fn seal(key: &[u8; 32], plain: &[u8]) -> Option<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).ok()?;
    let sealed = ChaCha20Poly1305::new(Key::from_slice(key)).encrypt(Nonce::from_slice(&nonce), plain).ok()?;
    Some([MAGIC, &nonce, &sealed].concat())
}

fn open(key: &[u8; 32], data: &[u8]) -> Option<Vec<u8>> {
    let rest = data.strip_prefix(MAGIC)?;
    if rest.len() < NONCE_LEN {
        return None;
    }
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    ChaCha20Poly1305::new(Key::from_slice(key)).decrypt(Nonce::from_slice(nonce), sealed).ok()
}

/// The key, looked up only when a file actually needs it, so a VPN with no
/// saved cookies never touches the Keychain.
type KeyFn = fn() -> Option<&'static [u8; 32]>;

/// The startup migration runs on its own thread while the main thread may be
/// reading or saving the same file, and both use the same temp file.
static FILES: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FILES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Encrypts `text` into `path`. Removes `path` instead if there's no key.
pub fn write(path: &Path, text: &str) {
    let _guard = lock();
    write_with(key, path, text);
}

/// Encrypts every `*.<extension>` file in `dir` that an older build saved
/// unencrypted.
pub fn migrate_dir(dir: &Path, extension: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_file() && path.extension().is_some_and(|e| e == extension) {
            let _guard = lock();
            migrate_with(key, &path);
        }
    }
}

/// Reads a file written by `write`. A file from an older build, saved
/// unencrypted, is read and then rewritten encrypted.
/// Returns None, and removes the file, if it can't be decrypted.
pub fn read(path: &Path) -> Option<String> {
    let _guard = lock();
    read_with(key, path)
}

fn write_with(key: KeyFn, path: &Path, text: &str) {
    let Some(data) = key().and_then(|k| seal(k, text.as_bytes())) else {
        let _ = std::fs::remove_file(path);
        return;
    };
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&tmp).and_then(|mut f| std::io::Write::write_all(&mut f, &data));
    if written.is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Encrypts `path` if it's unencrypted, or removes it if that can't be done
/// (no key, or not text). Returns its contents if it was unencrypted.
fn migrate_with(key: KeyFn, path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.starts_with(MAGIC) {
        return None;
    }
    let text = String::from_utf8(data).ok();
    match &text {
        Some(t) => write_with(key, path, t),
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
    text
}

fn read_with(key: KeyFn, path: &Path) -> Option<String> {
    if let Some(text) = migrate_with(key, path) {
        return Some(text);
    }
    let data = std::fs::read(path).ok()?;
    let text = key().and_then(|k| open(k, &data)).and_then(|p| String::from_utf8(p).ok());
    if text.is_none() {
        // Sealed with a key that's gone (or the user denied access to it).
        // The only cost is signing in again.
        let _ = std::fs::remove_file(path);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let key = [7u8; 32];
        let sealed = seal(&key, b"ESTSAUTH=abc").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(8).any(|w| w == b"ESTSAUTH"));
        assert_eq!(open(&key, &sealed).unwrap(), b"ESTSAUTH=abc");
    }

    #[test]
    fn rejects_wrong_key_or_tampering() {
        let sealed = seal(&[1u8; 32], b"secret").unwrap();
        assert!(open(&[2u8; 32], &sealed).is_none());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open(&[1u8; 32], &tampered).is_none());
        assert!(open(&[1u8; 32], b"secret").is_none());
    }

    #[test]
    fn key_hex_round_trips() {
        let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(9));
        assert_eq!(decode_key(&encode_key(&key)), Some(key));
        assert_eq!(decode_key("zz"), None);
    }

    static TEST_KEY: [u8; 32] = [7u8; 32];
    fn test_key() -> Option<&'static [u8; 32]> {
        Some(&TEST_KEY)
    }
    fn no_key() -> Option<&'static [u8; 32]> {
        None
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fortibar-seal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migrates_plaintext_file() {
        let dir = temp_dir("migrate");
        let path = dir.join("a.cookies");
        std::fs::write(&path, "ESTSAUTH=abc; Secure").unwrap();

        assert_eq!(migrate_with(test_key, &path).as_deref(), Some("ESTSAUTH=abc; Secure"));
        let on_disk = std::fs::read(&path).unwrap();
        assert!(on_disk.starts_with(MAGIC));
        assert!(!on_disk.windows(8).any(|w| w == b"ESTSAUTH"));
        // Already encrypted: left alone, and still readable.
        assert_eq!(migrate_with(test_key, &path), None);
        assert_eq!(read_with(test_key, &path).as_deref(), Some("ESTSAUTH=abc; Secure"));
        assert!(!dir.join("a.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removes_plaintext_file_without_key() {
        let dir = temp_dir("nokey");
        let path = dir.join("a.cookies");
        std::fs::write(&path, "ESTSAUTH=abc").unwrap();
        migrate_with(no_key, &path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removes_file_it_cannot_decrypt() {
        let dir = temp_dir("undecryptable");
        let path = dir.join("a.cookies");
        std::fs::write(&path, seal(&[9u8; 32], b"ESTSAUTH=abc").unwrap()).unwrap();
        assert_eq!(read_with(test_key, &path), None);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
