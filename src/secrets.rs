//! Password / token storage. Uses the OS credential store (Secret Service,
//! Keychain, Credential Manager) and falls back to a private file when no
//! credential store is available.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;

use crate::config::Paths;

const SERVICE: &str = "rustmail";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    SmtpPassword,
    OAuthRefresh,
    /// Password of a CalDAV calendar source (keyed by the source id).
    CalDavPassword,
}

impl SecretKind {
    fn suffix(self) -> &'static str {
        match self {
            SecretKind::Password => "password",
            SecretKind::SmtpPassword => "smtp-password",
            SecretKind::OAuthRefresh => "oauth-refresh",
            SecretKind::CalDavPassword => "caldav-password",
        }
    }
}

static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static BACKEND: Mutex<Option<&'static str>> = Mutex::new(None);

fn key(account_id: &str, kind: SecretKind) -> String {
    format!("{account_id}:{}", kind.suffix())
}

fn cache_get(k: &str) -> Option<String> {
    CACHE.lock().ok()?.as_ref()?.get(k).cloned()
}

fn cache_put(k: &str, v: Option<&str>) {
    if let Ok(mut c) = CACHE.lock() {
        let map = c.get_or_insert_with(HashMap::new);
        match v {
            Some(v) => {
                map.insert(k.to_string(), v.to_string());
            }
            None => {
                map.remove(k);
            }
        }
    }
}

/// Human readable name of the storage used for the last write.
pub fn backend_name() -> &'static str {
    BACKEND
        .lock()
        .ok()
        .and_then(|b| *b)
        .unwrap_or("system keychain")
}

fn set_backend(name: &'static str) {
    if let Ok(mut b) = BACKEND.lock() {
        *b = Some(name);
    }
}

pub fn get(account_id: &str, kind: SecretKind) -> Option<String> {
    let k = key(account_id, kind);
    if let Some(v) = cache_get(&k) {
        return Some(v);
    }
    if cfg!(test) {
        return None;
    }
    let from_keyring = keyring::Entry::new(SERVICE, &k)
        .ok()
        .and_then(|e| e.get_password().ok());
    let value = from_keyring.or_else(|| file_get(&k));
    if let Some(v) = &value {
        cache_put(&k, Some(v));
    }
    value
}

pub fn set(account_id: &str, kind: SecretKind, value: &str) {
    let k = key(account_id, kind);
    cache_put(&k, Some(value));
    // Tests never touch the real keychain.
    if cfg!(test) {
        return;
    }
    let stored = keyring::Entry::new(SERVICE, &k)
        .ok()
        .map(|e| e.set_password(value).is_ok())
        .unwrap_or(false);
    if stored {
        set_backend("system keychain");
        // Make sure a stale file copy does not linger.
        file_set(&k, None);
    } else {
        log::warn!("OS keychain unavailable, storing secret in private file");
        set_backend("private file");
        file_set(&k, Some(value));
    }
}

pub fn delete(account_id: &str, kind: SecretKind) {
    let k = key(account_id, kind);
    cache_put(&k, None);
    if cfg!(test) {
        return;
    }
    if let Ok(e) = keyring::Entry::new(SERVICE, &k) {
        let _ = e.delete_credential();
    }
    file_set(&k, None);
}

pub fn delete_all(account_id: &str) {
    for kind in [SecretKind::Password, SecretKind::SmtpPassword, SecretKind::OAuthRefresh] {
        delete(account_id, kind);
    }
}

/// Keeps a value in memory only (used by tests).
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_session_only(account_id: &str, kind: SecretKind, value: &str) {
    cache_put(&key(account_id, kind), Some(value));
}

fn file_load() -> HashMap<String, String> {
    let path = Paths::get().secrets_file();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn file_get(k: &str) -> Option<String> {
    let map = file_load();
    let enc = map.get(k)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(enc).ok()?;
    String::from_utf8(bytes).ok()
}

fn file_set(k: &str, v: Option<&str>) {
    let paths = Paths::get();
    let path = paths.secrets_file();
    let mut map = file_load();
    let changed = match v {
        Some(v) => {
            map.insert(
                k.to_string(),
                base64::engine::general_purpose::STANDARD.encode(v.as_bytes()),
            );
            true
        }
        None => map.remove(k).is_some(),
    };
    if !changed {
        return;
    }
    let _ = std::fs::create_dir_all(&paths.data_dir);
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        if let Err(e) = write_private(&path, text.as_bytes()) {
            log::error!("could not write secrets file: {e}");
        }
    }
}

/// Writes a file readable only by the current user (created with 0600).
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}
