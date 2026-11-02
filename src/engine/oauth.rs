//! OAuth 2.0 authorization-code flow with PKCE and a loopback redirect, for
//! providers that no longer accept passwords over IMAP (Microsoft, Google).

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::OAuthConfig;
use crate::util;

pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: i64,
}

pub fn preset(provider: &str) -> Option<OAuthConfig> {
    match provider {
        "google" => Some(OAuthConfig {
            provider: "google".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: "https://mail.google.com/".into(),
        }),
        "microsoft" => Some(OAuthConfig {
            provider: "microsoft".into(),
            client_id: String::new(),
            client_secret: String::new(),
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            scopes: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access".into(),
        }),
        _ => None,
    }
}

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
}

fn post_token(url: &str, form: Vec<(String, String)>) -> Result<Tokens> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut resp = agent
        .post(url)
        .send_form(pairs)
        .context("Could not reach the sign-in server")?;
    let status = resp.status();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("Sign-in server returned {status}: {body}"));
    }
    let t: TokenResponse = serde_json::from_str(&body).context("Unexpected token response")?;
    Ok(Tokens {
        access: t.access_token,
        refresh: t.refresh_token,
        expires_at: util::now_ts() + t.expires_in.unwrap_or(3600),
    })
}

/// Opens the browser for consent and waits for the redirect.
pub async fn authorize(cfg: &OAuthConfig, login_hint: &str) -> Result<Tokens> {
    if cfg.client_id.trim().is_empty() {
        return Err(anyhow!(
            "An OAuth client ID is required. Register a desktop app with your provider and paste its client ID."
        ));
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
        url.push_str("&access_type=offline&prompt=consent");
    }
    open::that(&url).context("Could not open the web browser")?;

    let (mut sock, _) = tokio::time::timeout(Duration::from_secs(300), listener.accept())
        .await
        .map_err(|_| anyhow!("Timed out waiting for the browser sign-in"))??;
    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await??;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let first = request.lines().next().unwrap_or("");
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    let ok = get("code").is_some() && get("state").as_deref() == Some(state.as_str());
    let page = if ok {
        "<h2 style='font-family:sans-serif'>You're signed in.</h2><p style='font-family:sans-serif'>You can close this tab and return to RustMail.</p>"
    } else {
        "<h2 style='font-family:sans-serif'>Sign-in failed.</h2><p style='font-family:sans-serif'>Return to RustMail and try again.</p>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.shutdown().await;

    if let Some(err) = get("error") {
        return Err(anyhow!(
            "Sign-in was not completed: {} {}",
            err,
            get("error_description").unwrap_or_default()
        ));
    }
    if get("state").as_deref() != Some(state.as_str()) {
        return Err(anyhow!("Sign-in response did not match the request"));
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
    tokio::task::spawn_blocking(move || post_token(&url, form)).await?
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
    tokio::task::spawn_blocking(move || post_token(&url, form))
        .await?
        .context("Could not refresh the sign-in token; please sign in again")
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
}
