//! OAuth 2.0 authorization-code flow with PKCE and a loopback redirect, for
//! providers that no longer accept passwords over IMAP (Microsoft, Google).
//!
//! Signing in needs an app key ("OAuth client") registered with the provider.
//! For a new sign-in RustMail uses, in order: the key saved in Settings, a key
//! built into this copy (`RUSTMAIL_GOOGLE_CLIENT_ID` / `RUSTMAIL_GOOGLE_CLIENT_SECRET`
//! or `RUSTMAIL_MICROSOFT_CLIENT_ID` at compile time) and the key the account
//! signed in with before. Accounts keep the key they signed in with, because
//! a refresh token only works with the key that issued it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::config::{OAuthApps, OAuthClientKey, OAuthConfig};
use crate::util;

/// Ends every error that can only be fixed by signing in with the browser
/// again; the UI looks for it to offer a "Sign in" button.
pub const SIGN_IN_AGAIN: &str = "Please sign in again.";

/// How long to wait for the person to finish signing in in the browser.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: i64,
    /// Space-separated permissions granted (from the sign-in only).
    pub granted: Option<String>,
}

/// Google permissions for two-way calendar sync: edit events, list calendars.
pub const GOOGLE_CALENDAR_SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
];

/// Whether a Google sign-in allowed calendar access: `Some(false)` when the
/// person left it unticked, `None` when unknown (signed in before RustMail
/// asked for it, or never told).
pub fn calendar_granted(cfg: &OAuthConfig) -> Option<bool> {
    if cfg.granted.trim().is_empty() {
        return None;
    }
    let has = |s: &str| cfg.granted.split_whitespace().any(|g| g == s);
    Some(GOOGLE_CALENDAR_SCOPES.iter().all(|s| has(s)))
}

/// Brings an account's requested permissions up to date (older accounts
/// only asked for mail), keeping its sign-in addresses (tests use others).
pub fn upgrade_scopes(cfg: &mut OAuthConfig) {
    if let Some(p) = preset(&cfg.provider) {
        cfg.scopes = p.scopes;
    }
}

pub fn preset(provider: &str) -> Option<OAuthConfig> {
    match provider {
        "google" => Some(OAuthConfig {
            provider: "google".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            // Mail plus calendar (no sign-in scopes such as openid/email). With
            // more than one permission Google may show tick boxes; what was
            // granted is checked after sign-in.
            scopes: format!("https://mail.google.com/ {}", GOOGLE_CALENDAR_SCOPES.join(" ")),
            granted: String::new(),
        }),
        "microsoft" => Some(OAuthConfig {
            provider: "microsoft".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            scopes: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access".into(),
            granted: String::new(),
        }),
        _ => None,
    }
}

pub fn provider_name(provider: &str) -> &'static str {
    match provider {
        "google" => "Google",
        "microsoft" => "Microsoft",
        _ => "your provider",
    }
}

// ------------------------------------------------------------------ app keys

/// The key compiled into this copy of RustMail, if any.
pub fn builtin_key(provider: &str) -> Option<OAuthClientKey> {
    let (id, secret) = match provider {
        "google" => (option_env!("RUSTMAIL_GOOGLE_CLIENT_ID"), option_env!("RUSTMAIL_GOOGLE_CLIENT_SECRET")),
        "microsoft" => (option_env!("RUSTMAIL_MICROSOFT_CLIENT_ID"), None),
        _ => (None, None),
    };
    let id = id.map(str::trim).filter(|s| !s.is_empty())?;
    Some(OAuthClientKey {
        client_id: id.to_string(),
        client_secret: secret.unwrap_or("").trim().to_string(),
        project_id: String::new(),
        source: "built into this copy of RustMail".into(),
    })
}

/// The key to use for a new sign-in (see the module docs for the order).
/// Microsoft keys are typed into the account itself, so there the account's
/// own client ID comes before the one remembered from earlier accounts.
pub fn key_for_sign_in(apps: &OAuthApps, current: &OAuthConfig) -> Option<OAuthClientKey> {
    let own = (!current.client_id.trim().is_empty()).then(|| OAuthClientKey {
        client_id: current.client_id.trim().to_string(),
        client_secret: current.client_secret.trim().to_string(),
        project_id: String::new(),
        source: String::new(),
    });
    let shared = apps.get(&current.provider).filter(|k| k.is_set()).cloned();
    if current.provider == "microsoft" {
        return builtin_key("microsoft").or(own).or(shared);
    }
    shared.or_else(|| builtin_key(&current.provider)).or(own)
}

pub fn apply_key(cfg: &mut OAuthConfig, key: &OAuthClientKey) {
    cfg.client_id = key.client_id.trim().to_string();
    cfg.client_secret = key.client_secret.trim().to_string();
}

/// Reads a Google Cloud key from what the person pasted or dropped: the
/// downloaded `client_secret_….json`, or the client ID and secret as text.
/// The secret may be missing when only the ID was pasted.
pub fn parse_google_key(text: &str) -> std::result::Result<OAuthClientKey, String> {
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.starts_with('{') {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|_| "This doesn't look like a Google key file.".to_string())?;
        if v.get("web").is_some() && v.get("installed").is_none() {
            return Err("This key was made for a website. In Google Cloud, create a new key and choose \"Desktop app\" as the application type.".into());
        }
        let inner = v.get("installed").unwrap_or(&v);
        let field = |k: &str| inner.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let key = OAuthClientKey {
            client_id: field("client_id"),
            client_secret: field("client_secret"),
            project_id: field("project_id"),
            source: String::new(),
        };
        if !looks_like_google_id(&key.client_id) {
            return Err("This file has no Google client ID in it.".into());
        }
        return Ok(key);
    }
    let mut key = OAuthClientKey::default();
    // The ID and secret pasted back to back ("…googleusercontent.comGOCSPX-…").
    let text = text.replace("GOCSPX-", " GOCSPX-");
    let tokens = text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '='));
    for t in tokens.map(str::trim).filter(|t| !t.is_empty()) {
        if looks_like_google_id(t) && key.client_id.is_empty() {
            key.client_id = t.to_string();
        } else if t.starts_with("GOCSPX-") && key.client_secret.is_empty() {
            key.client_secret = t.to_string();
        }
    }
    if key.client_id.is_empty() {
        return Err(if key.client_secret.is_empty() {
            "That isn't a Google client ID. It ends with .apps.googleusercontent.com.".into()
        } else {
            "That's the client secret. Paste the client ID too (it ends with .apps.googleusercontent.com).".into()
        });
    }
    Ok(key)
}

fn looks_like_google_id(s: &str) -> bool {
    let s = s.trim();
    s.len() > ".apps.googleusercontent.com".len() + 4
        && s.ends_with(".apps.googleusercontent.com")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// Folders where a browser saves downloads, including the Windows one when
/// running under WSL (the browser then runs on the Windows side).
pub fn download_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(u) = directories::UserDirs::new() {
        if let Some(d) = u.download_dir() {
            dirs.push(d.to_path_buf());
        }
        dirs.push(u.home_dir().join("Downloads"));
    }
    let wsl = std::fs::read_to_string("/proc/version")
        .map(|v| v.to_lowercase().contains("microsoft"))
        .unwrap_or(false);
    if wsl {
        if let Ok(rd) = std::fs::read_dir("/mnt/c/Users") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), "Public" | "Default" | "Default User" | "All Users") {
                    continue;
                }
                dirs.push(e.path().join("Downloads"));
            }
        }
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let key = d.canonicalize().unwrap_or_else(|_| d.clone());
        let new = d.is_dir() && !seen.contains(&key);
        seen.push(key);
        new
    });
    dirs
}

/// Reads a downloaded Google key file.
pub fn load_google_key_file(path: &Path) -> std::result::Result<OAuthClientKey, String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("Could not open the file: {e}"))?;
    // Only plain files: reading a pipe or device with this name could block forever.
    if !meta.is_file() {
        return Err("This isn't a Google key file.".into());
    }
    if meta.len() > 64 * 1024 {
        return Err("This file is too large to be a Google key file.".into());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(64 * 1024).read_to_string(&mut text))
        .map_err(|e| format!("Could not read the file: {e}"))?;
    let mut key = parse_google_key(&text)?;
    if key.client_secret.is_empty() {
        return Err("This key file has no client secret. Create a new Desktop app key and download it right away.".into());
    }
    key.source = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(key)
}

/// The newest Desktop-app key downloaded from Google Cloud (a file named
/// `client_secret_….json` in a downloads folder), optionally only one saved
/// after `since`.
pub fn find_downloaded_google_key(since: Option<SystemTime>) -> Option<OAuthClientKey> {
    find_google_key_in(&download_dirs(), since).ok().flatten()
}

/// Like [`find_downloaded_google_key`], but says why when the only new
/// `client_secret_….json` files aren't usable (e.g. a "Web application" key).
pub fn scan_downloads_for_google_key(since: SystemTime) -> std::result::Result<Option<OAuthClientKey>, String> {
    find_google_key_in(&download_dirs(), Some(since))
}

fn find_google_key_in(dirs: &[PathBuf], since: Option<SystemTime>) -> std::result::Result<Option<OAuthClientKey>, String> {
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !(name.starts_with("client_secret") && name.ends_with(".json")) {
                continue;
            }
            let Ok(modified) = e.metadata().and_then(|m| m.modified()) else { continue };
            if since.is_some_and(|s| modified < s) {
                continue;
            }
            files.push((modified, e.path()));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut first_error = None;
    for (_, path) in files {
        match load_google_key_file(&path) {
            Ok(key) => return Ok(Some(key)),
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                first_error.get_or_insert(format!("{name}: {e}"));
            }
        }
    }
    first_error.map_or(Ok(None), Err)
}

// --------------------------------------------------------------------- flow

fn random_token(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; bytes];
    let _ = ring::rand::SystemRandom::new().fill(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    /// The permissions actually granted (people can untick some at Google).
    scope: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct TokenErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

enum TokenFailure {
    Network(String),
    Rejected { status: u16, error: String, description: String },
}

/// The permission that reading and sending mail needs, per provider.
fn mail_scope(provider: &str) -> Option<&'static str> {
    match provider {
        "google" => Some("https://mail.google.com/"),
        "microsoft" => Some("IMAP.AccessAsUser.All"),
        _ => None,
    }
}

fn post_token(url: &str, form: Vec<(String, String)>) -> std::result::Result<(Tokens, Option<String>), TokenFailure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut resp = agent
        .post(url)
        .send_form(pairs)
        .map_err(|e| TokenFailure::Network(e.to_string()))?;
    let status = resp.status();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    if !status.is_success() {
        let e: TokenErrorBody = serde_json::from_str(&body).unwrap_or_default();
        return Err(TokenFailure::Rejected {
            status: status.as_u16(),
            error: e.error,
            description: if e.error_description.is_empty() { util::truncate_chars(body.trim(), 200) } else { e.error_description },
        });
    }
    let t: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| TokenFailure::Network("the sign-in server sent an unexpected reply".into()))?;
    Ok((
        Tokens {
            access: t.access_token,
            refresh: t.refresh_token,
            expires_at: util::now_ts() + t.expires_in.unwrap_or(3600),
            granted: t.scope.clone(),
        },
        t.scope,
    ))
}

/// Turns a token endpoint failure into advice a person can act on.
fn token_error(f: TokenFailure, provider: &str, refreshing: bool) -> anyhow::Error {
    let name = provider_name(provider);
    match f {
        TokenFailure::Network(e) => anyhow!("Could not reach {name} to sign in. Check your internet connection. ({e})"),
        TokenFailure::Rejected { status, error, description } => {
            let detail = if description.is_empty() { error.clone() } else { format!("{error}: {description}") };
            match error.as_str() {
                "invalid_grant" | "interaction_required" | "consent_required" | "login_required" if refreshing => {
                    let hint = if provider == "google" {
                        " (While a Google key is in Google's \"Testing\" mode, this happens every 7 days.)"
                    } else {
                        ""
                    };
                    anyhow!("Your {name} sign-in has expired or was revoked.{hint} {SIGN_IN_AGAIN}")
                }
                "invalid_grant" => anyhow!("The sign-in took too long or was already used. Please try again. ({detail})"),
                "invalid_client" | "unauthorized_client" => anyhow!(
                    "{name} did not accept RustMail's app key (client ID or secret). Check the key in Settings → Accounts{}. ({detail})",
                    if refreshing { format!(", then sign in again") } else { String::new() }
                ),
                _ => anyhow!("{name} refused the sign-in (HTTP {status}). ({detail})"),
            }
        }
    }
}

/// Error from the redirect (`?error=…`), in plain words.
fn redirect_error(provider: &str, error: &str, description: &str) -> anyhow::Error {
    let name = provider_name(provider);
    let msg = match error {
        "access_denied" => "Sign-in was cancelled in the browser, or the requested permissions weren't allowed. To read and send mail, RustMail needs every box ticked.".to_string(),
        "admin_policy_enforced" => format!("Your organization's {name} administrator doesn't allow this app."),
        "org_internal" => "This Google key only works for accounts in its own organization. In Google Cloud, set the audience to \"External\".".to_string(),
        "invalid_client" | "unauthorized_client" => format!("{name} did not accept RustMail's app key. Check the key in Settings → Accounts."),
        _ => format!("Sign-in was not completed ({error})."),
    };
    if description.is_empty() {
        anyhow!(msg)
    } else {
        anyhow!("{msg} ({description})")
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>RustMail</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32em;margin:15vh auto;padding:0 1em;color:#222\">\
         <h2>{title}</h2><p>{body}</p></body></html>"
    )
}

async fn respond(sock: &mut TcpStream, status: &str, html: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// Serves one connection to the loopback redirect. Returns the query
/// parameters when this was the redirect for our request.
async fn handle_redirect(mut sock: TcpStream, state: &str) -> Option<Vec<(String, String)>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    // Browsers sometimes open spare connections that never send anything.
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match tokio::time::timeout(Duration::from_secs(20), sock.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    let request = String::from_utf8_lossy(&buf).to_string();
    let target = request.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let params = parse_query(query);
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    if path != "/" || (get("code").is_none() && get("error").is_none()) {
        respond(&mut sock, "404 Not Found", &page("Nothing here", "This address is only used to finish signing in to RustMail.")).await;
        return None;
    }
    if get("state") != Some(state) {
        // An old tab from an earlier attempt; keep waiting for the current one.
        respond(&mut sock, "400 Bad Request", &page("This sign-in link has expired", "Go back to RustMail and start signing in again.")).await;
        return None;
    }
    if let Some(e) = get("error") {
        let msg = redirect_error("", e, get("error_description").unwrap_or("")).to_string();
        respond(&mut sock, "200 OK", &page("Sign-in was not completed", &html_escape(&msg))).await;
    } else {
        respond(&mut sock, "200 OK", &page("You're signed in", "You can close this tab and return to RustMail.")).await;
    }
    Some(params)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Stops a spawned task when dropped, so cancelling a sign-in also closes
/// its loopback server.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits for the browser to come back to the loopback address with our
/// `state`, serving any other requests (favicon, stale tabs) on the way.
async fn wait_for_redirect(listener: TcpListener, state: String, timeout: Duration) -> Result<Vec<(String, String)>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let _server = AbortOnDrop(tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let tx = tx.clone();
            let state = state.clone();
            tokio::spawn(async move {
                if let Some(params) = handle_redirect(sock, &state).await {
                    let _ = tx.send(params).await;
                }
            });
        }
    }));
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(params)) => Ok(params),
        Ok(None) => Err(anyhow!("The sign-in was interrupted")),
        Err(_) => Err(anyhow!("Timed out waiting for you to finish signing in in the browser. Please try again.")),
    }
}

/// Builds the consent URL, hands it to `open_browser` and waits for the
/// browser to return to the loopback redirect, then exchanges the code.
pub async fn authorize(
    cfg: &OAuthConfig,
    login_hint: &str,
    open_browser: impl FnOnce(&str) + Send,
) -> Result<Tokens> {
    let name = provider_name(&cfg.provider);
    if cfg.client_id.trim().is_empty() {
        return Err(anyhow!("RustMail needs an app key from {name} before you can sign in. Set it up in Settings → Accounts."));
    }
    if cfg.provider == "google" && cfg.client_secret.trim().is_empty() {
        return Err(anyhow!("The Google key is missing its client secret. Set up Google sign-in again in Settings → Accounts."));
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect = if cfg.provider == "microsoft" {
        format!("http://localhost:{port}")
    } else {
        format!("http://127.0.0.1:{port}")
    };
    let verifier = random_token(48);
    let state = random_token(16);
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        cfg.auth_url,
        url_encode(cfg.client_id.trim()),
        url_encode(&redirect),
        url_encode(&cfg.scopes),
        pkce_challenge(&verifier),
        url_encode(&state)
    );
    if !login_hint.is_empty() {
        url.push_str(&format!("&login_hint={}", url_encode(login_hint)));
    }
    if cfg.provider == "google" {
        // Always show the account chooser, so the right account is picked.
        url.push_str("&access_type=offline&prompt=consent%20select_account");
    }
    open_browser(&url);

    let params = wait_for_redirect(listener, state, BROWSER_TIMEOUT).await?;
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    if let Some(err) = get("error") {
        return Err(redirect_error(&cfg.provider, &err, &get("error_description").unwrap_or_default()));
    }
    let code = get("code").ok_or_else(|| anyhow!("No authorization code was returned"))?;
    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), redirect),
        ("client_id".to_string(), cfg.client_id.trim().to_string()),
        ("code_verifier".to_string(), verifier),
    ];
    if !cfg.client_secret.trim().is_empty() {
        form.push(("client_secret".to_string(), cfg.client_secret.trim().to_string()));
    }
    let url = cfg.token_url.clone();
    let provider = cfg.provider.clone();
    let (tokens, granted) = tokio::task::spawn_blocking(move || post_token(&url, form))
        .await?
        .map_err(|f| token_error(f, &provider, false))?;
    if let (Some(needed), Some(granted)) = (mail_scope(&cfg.provider), granted.as_deref()) {
        if !granted.split_whitespace().any(|s| s.ends_with(needed)) {
            return Err(anyhow!(
                "RustMail wasn't given permission to read and send your mail. Sign in again and allow access to your mail (tick every box if {} shows any).",
                provider_name(&cfg.provider)
            ));
        }
    }
    Ok(tokens)
}

pub async fn refresh(cfg: &OAuthConfig, refresh_token: &str) -> Result<Tokens> {
    let mut form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.to_string()),
        ("client_id".to_string(), cfg.client_id.trim().to_string()),
    ];
    if !cfg.client_secret.trim().is_empty() {
        form.push(("client_secret".to_string(), cfg.client_secret.trim().to_string()));
    }
    if cfg.provider == "microsoft" {
        form.push(("scope".to_string(), cfg.scopes.clone()));
    }
    let url = cfg.token_url.clone();
    let provider = cfg.provider.clone();
    tokio::task::spawn_blocking(move || post_token(&url, form))
        .await?
        .map(|(t, _)| t)
        .map_err(|f| token_error(f, &provider, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding() {
        assert_eq!(url_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(url_decode("a%20b%2Fc+d"), "a b/c d");
        let q = parse_query("code=4%2F0Ab&state=xyz");
        assert_eq!(q[0], ("code".into(), "4/0Ab".into()));
        // RFC 7636 appendix B test vector.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    const ID: &str = "123456789012-abcdefghijklmnop0123456789abcdef.apps.googleusercontent.com";

    #[test]
    fn google_key_parsing() {
        let file = format!(
            r#"{{"installed":{{"client_id":"{ID}","project_id":"rustmail-12345","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","client_secret":"GOCSPX-abcDEF123_xyz","redirect_uris":["http://localhost"]}}}}"#
        );
        let k = parse_google_key(&file).unwrap();
        assert_eq!(k.client_id, ID);
        assert_eq!(k.client_secret, "GOCSPX-abcDEF123_xyz");
        assert_eq!(k.project_id, "rustmail-12345");

        let web = format!(r#"{{"web":{{"client_id":"{ID}","client_secret":"x"}}}}"#);
        assert!(parse_google_key(&web).unwrap_err().contains("Desktop app"));

        // Copied from the console's "client created" dialog.
        let pasted = format!("Client ID\n{ID}\nClient secret\nGOCSPX-abcDEF123_xyz\n");
        let k = parse_google_key(&pasted).unwrap();
        assert_eq!((k.client_id.as_str(), k.client_secret.as_str()), (ID, "GOCSPX-abcDEF123_xyz"));

        // Pasted back to back without a space.
        let k = parse_google_key(&format!("{ID}GOCSPX-abcDEF123_xyz")).unwrap();
        assert_eq!((k.client_id.as_str(), k.client_secret.as_str()), (ID, "GOCSPX-abcDEF123_xyz"));

        let only_id = parse_google_key(&format!("  {ID} ")).unwrap();
        assert!(only_id.client_secret.is_empty());
        assert!(parse_google_key("GOCSPX-abc").unwrap_err().contains("client ID too"));
        assert!(parse_google_key("hello").is_err());
    }

    #[test]
    fn finds_newest_downloaded_key() {
        let dir = std::env::temp_dir().join(format!("rustmail-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |secret: &str| {
            format!(r#"{{"installed":{{"client_id":"{ID}","client_secret":"{secret}","project_id":"p"}}}}"#)
        };
        std::fs::write(dir.join("client_secret_old.apps.googleusercontent.com.json"), file("GOCSPX-old")).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("client_secret_new.apps.googleusercontent.com.json"), file("GOCSPX-new")).unwrap();
        std::fs::write(dir.join("notes.json"), file("GOCSPX-other")).unwrap();
        let k = find_google_key_in(&[dir.clone()], None).unwrap().unwrap();
        assert_eq!(k.client_secret, "GOCSPX-new");
        assert_eq!(k.source, "client_secret_new.apps.googleusercontent.com.json");
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(find_google_key_in(&[dir.clone()], Some(later)), Ok(None));
        // A new file that can't be used is reported, not silently skipped.
        let before_web = SystemTime::now() - Duration::from_millis(5);
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("client_secret_web.json"), format!(r#"{{"web":{{"client_id":"{ID}","client_secret":"x"}}}}"#)).unwrap();
        let err = find_google_key_in(&[dir.clone()], Some(before_web + Duration::from_millis(10))).unwrap_err();
        assert!(err.contains("client_secret_web.json") && err.contains("Desktop app"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_order() {
        let mut apps = OAuthApps::default();
        let mut current = preset("google").unwrap();
        current.client_id = "old.apps.googleusercontent.com".into();
        // Without a shared key, the account's own key is used (unless one is built in).
        if builtin_key("google").is_none() {
            assert_eq!(key_for_sign_in(&apps, &current).unwrap().client_id, "old.apps.googleusercontent.com");
        }
        apps.google.client_id = ID.into();
        apps.google.client_secret = "GOCSPX-x".into();
        assert_eq!(key_for_sign_in(&apps, &current).unwrap().client_id, ID);
        apply_key(&mut current, &apps.google);
        assert_eq!(current.client_secret, "GOCSPX-x");

        // Microsoft: the ID typed into the account wins over the remembered one.
        if builtin_key("microsoft").is_none() {
            let mut ms = preset("microsoft").unwrap();
            apps.microsoft.client_id = "remembered".into();
            assert_eq!(key_for_sign_in(&apps, &ms).unwrap().client_id, "remembered");
            ms.client_id = "typed".into();
            assert_eq!(key_for_sign_in(&apps, &ms).unwrap().client_id, "typed");
        }
    }

    /// A fake token endpoint that answers one request with `body` and
    /// records the form it got.
    async fn fake_token_endpoint(body: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}/token", server.local_addr().unwrap().port());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            let (mut s, _) = server.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut req = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some((head, form)) = text.split_once("\r\n\r\n") {
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                        .unwrap_or(0);
                    if form.len() >= len {
                        *seen2.lock().unwrap() = form.to_string();
                        break;
                    }
                }
            }
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        (url, seen)
    }

    fn google_test_config(token_url: String) -> OAuthConfig {
        let mut cfg = preset("google").unwrap();
        cfg.auth_url = "http://127.0.0.1:9/auth".into();
        cfg.token_url = token_url;
        cfg.client_id = ID.into();
        cfg.client_secret = "GOCSPX-test".into();
        cfg
    }

    /// Plays the browser: a stray request first (like a favicon fetch), then
    /// the redirect back with a code.
    fn approve(url: &str) {
        let q = parse_query(url.split_once('?').unwrap().1);
        let get = |k: &str| q.iter().find(|(pk, _)| pk == k).unwrap().1.clone();
        let (redirect, state) = (get("redirect_uri"), get("state"));
        std::thread::spawn(move || {
            let _ = ureq::get(&format!("{redirect}/favicon.ico")).call();
            let page = ureq::get(&format!("{redirect}/?state={state}&code=c-123&scope=x"))
                .call()
                .unwrap()
                .body_mut()
                .read_to_string()
                .unwrap();
            assert!(page.contains("signed in"));
        });
    }

    /// Full browser round trip against a fake provider: the "browser"
    /// follows the consent URL straight to the loopback redirect.
    #[test]
    fn authorize_round_trip() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (token_url, seen) = fake_token_endpoint(
                r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3599,"scope":"https://mail.google.com/","token_type":"Bearer"}"#,
            )
            .await;
            let cfg = google_test_config(token_url);
            let tokens = authorize(&cfg, "jane@gmail.com", |url| {
                let q = parse_query(url.split_once('?').unwrap().1);
                let get = |k: &str| q.iter().find(|(pk, _)| pk == k).unwrap().1.clone();
                assert_eq!(get("login_hint"), "jane@gmail.com");
                assert_eq!(get("access_type"), "offline");
                assert_eq!(get("prompt"), "consent select_account");
                assert_eq!(
                    get("scope"),
                    "https://mail.google.com/ https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.calendarlist.readonly"
                );
                approve(url);
            })
            .await
            .unwrap();
            assert_eq!(tokens.access, "at-1");
            assert_eq!(tokens.refresh.as_deref(), Some("rt-1"));
            assert_eq!(tokens.granted.as_deref(), Some("https://mail.google.com/"));
            let mut cfg2 = preset("google").unwrap();
            cfg2.granted = "https://mail.google.com/".into();
            assert_eq!(calendar_granted(&cfg2), Some(false), "calendar left unticked");
            cfg2.granted = format!("https://mail.google.com/ {}", GOOGLE_CALENDAR_SCOPES.join(" "));
            assert_eq!(calendar_granted(&cfg2), Some(true));
            let form = seen.lock().unwrap().clone();
            assert!(form.contains("code=c-123"));
            assert!(form.contains("client_secret=GOCSPX-test"));
            assert!(form.contains("code_verifier="));
        });
    }

    /// Mail permission unticked at Google: no "Signed in", a clear message.
    #[test]
    fn partial_grant_is_refused() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (token_url, _) = fake_token_endpoint(
                r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3599,"scope":"openid https://www.googleapis.com/auth/userinfo.email"}"#,
            )
            .await;
            let err = authorize(&google_test_config(token_url), "", approve).await.err().unwrap().to_string();
            assert!(err.contains("permission to read and send your mail"), "{err}");
        });
    }

    /// Cancelling a sign-in (dropping its task) closes the loopback server,
    /// so an old browser tab can't report a sign-in that went nowhere.
    #[test]
    fn cancel_closes_loopback() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let cfg = google_test_config("http://127.0.0.1:9/token".into());
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let task = tokio::spawn(async move {
                let _ = authorize(&cfg, "", move |url| {
                    let _ = tx.send(url.to_string());
                })
                .await;
            });
            let url = tokio::task::spawn_blocking(move || rx.recv().unwrap()).await.unwrap();
            let q = parse_query(url.split_once('?').unwrap().1);
            let redirect = q.iter().find(|(k, _)| k == "redirect_uri").unwrap().1.clone();
            let addr = redirect.trim_start_matches("http://").to_string();
            assert!(std::net::TcpStream::connect(&addr).is_ok(), "listening while waiting");
            task.abort();
            let _ = task.await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(std::net::TcpStream::connect(&addr).is_err(), "closed after cancel");
        });
    }
}
