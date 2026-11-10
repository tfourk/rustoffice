//! Calendar sync with Google Calendar (REST API v3) and CalDAV servers
//! (iCloud, Fastmail, Nextcloud, Yahoo, …).
//!
//! One background task runs all syncs: every few minutes, right after the
//! person changes an event in a synced calendar, and on request. Each sync is
//! blocking HTTP on a worker thread with its own database connection, and
//! follows the same order for every source: upload local changes first
//! (rows with `SyncState::Changed / Deleted`), then download what changed on
//! the server (Google sync tokens, CalDAV sync-collection or ctag/etag).

pub mod caldav;
pub mod google;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;

use super::{oauth, Event, Shared};
use crate::config::{AccountConfig, AuthKind, CalendarSource, CalendarSourceKind};
use crate::secrets::{self, SecretKind};
use crate::store::Store;
use crate::util;

/// What the calendar pane shows next to a source.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceStatus {
    Idle { last: i64 },
    Syncing,
    /// Google: the account's sign-in didn't include calendar access.
    NeedsPermission,
    /// Google: the Calendar API is turned off in the person's Cloud project.
    ApiDisabled { url: String },
    /// The server refused the password / sign-in.
    AuthFailed(String),
    Error(String),
}

#[derive(Debug)]
pub enum SyncError {
    NeedsPermission,
    ApiDisabled { url: String },
    Auth(String),
    Other(String),
}

impl From<anyhow::Error> for SyncError {
    fn from(e: anyhow::Error) -> Self {
        SyncError::Other(format!("{e:#}"))
    }
}

impl From<SyncError> for SourceStatus {
    fn from(e: SyncError) -> Self {
        match e {
            SyncError::NeedsPermission => SourceStatus::NeedsPermission,
            SyncError::ApiDisabled { url } => SourceStatus::ApiDisabled { url },
            SyncError::Auth(m) => SourceStatus::AuthFailed(m),
            SyncError::Other(m) => SourceStatus::Error(m),
        }
    }
}

/// Result of one sync of one source.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Something in the local database changed (the UI reloads).
    pub changed: bool,
    /// Things the person should know, e.g. an edit that lost to a newer
    /// change on the server.
    pub notices: Vec<String>,
}

/// A calendar found on a server (CalDAV discovery / connection test).
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteCalendar {
    pub remote_id: String,
    pub name: String,
    pub color: Option<u32>,
    pub read_only: bool,
}

pub(crate) enum CalCmd {
    /// Sync one source, or all when `None`.
    Sync(Option<String>),
    Stop,
}

/// The engine's calendar task.
pub(crate) async fn run(sh: Arc<Shared>, mut rx: UnboundedReceiver<CalCmd>) {
    let mut tokens: HashMap<String, (String, i64)> = HashMap::new();
    // First sync shortly after start-up, once mail has had a head start.
    let mut next = Instant::now() + Duration::from_secs(4);
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        let cmd = tokio::select! {
            c = rx.recv() => c,
            _ = tokio::time::sleep(wait) => Some(CalCmd::Sync(None)),
        };
        match cmd {
            None | Some(CalCmd::Stop) => break,
            Some(CalCmd::Sync(only)) => {
                let cfg = sh.cfg();
                let full = only.is_none();
                for source in cfg.calendar_sources.iter().filter(|s| s.enabled) {
                    if only.as_ref().is_some_and(|id| *id != source.id) {
                        continue;
                    }
                    sync_source(&sh, source, &cfg.accounts, &mut tokens).await;
                }
                if full {
                    // ±20% so many clients don't sync in lockstep.
                    let base = cfg.calendar.sync_interval_min.clamp(1, 240) as u64 * 60;
                    let jitter = (util::now_ts() as u64 % (base / 5 + 1)) as i64 - (base / 10) as i64;
                    next = Instant::now() + Duration::from_secs((base as i64 + jitter).max(30) as u64);
                }
            }
        }
    }
}

fn status(sh: &Shared, source_id: &str, status: SourceStatus) {
    sh.notify.emit(Event::CalendarStatus { source_id: source_id.to_string(), status });
}

async fn sync_source(sh: &Arc<Shared>, source: &CalendarSource, accounts: &[AccountConfig], tokens: &mut HashMap<String, (String, i64)>) {
    status(sh, &source.id, SourceStatus::Syncing);
    let account = accounts.iter().find(|a| a.id == source.account_id).cloned();
    let result: Result<SyncReport, SyncError> = match source.kind {
        CalendarSourceKind::Google => match google_token(account.as_ref(), tokens).await {
            Ok(token) => {
                let api = google::GoogleApi {
                    base: if source.url.trim().is_empty() { google::API_BASE.to_string() } else { source.url.trim().to_string() },
                    token,
                };
                let (db, src) = (sh.db_path.clone(), source.clone());
                let r = tokio::task::spawn_blocking(move || {
                    let store = Store::open(&db)?;
                    google::sync(&store, &src, &api).map_err(|e| anyhow::anyhow!(SyncErrorBox(e)))
                })
                .await;
                flatten(r)
            }
            Err(e) => Err(e),
        },
        CalendarSourceKind::CalDav => match caldav_credentials(source, account.as_ref()) {
            Ok(creds) => {
                let (db, src) = (sh.db_path.clone(), source.clone());
                let r = tokio::task::spawn_blocking(move || {
                    let store = Store::open(&db)?;
                    caldav::sync(&store, &src, &creds).map_err(|e| anyhow::anyhow!(SyncErrorBox(e)))
                })
                .await;
                flatten(r)
            }
            Err(e) => Err(e),
        },
    };
    match result {
        Ok(report) => {
            if report.changed {
                sh.notify.emit(Event::CalendarsChanged);
            }
            for n in report.notices {
                sh.notify.emit(Event::Toast { text: n, error: false });
            }
            status(sh, &source.id, SourceStatus::Idle { last: util::now_ts() });
        }
        Err(e) => {
            if matches!(e, SyncError::Auth(_)) {
                tokens.remove(&source.account_id);
            }
            // A failed sync can still have written something.
            sh.notify.emit(Event::CalendarsChanged);
            status(sh, &source.id, e.into());
        }
    }
}

/// Carries a `SyncError` through `anyhow` across the blocking task.
#[derive(Debug)]
struct SyncErrorBox(SyncError);

impl std::fmt::Display for SyncErrorBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::error::Error for SyncErrorBox {}

fn flatten(r: Result<anyhow::Result<SyncReport>, tokio::task::JoinError>) -> Result<SyncReport, SyncError> {
    match r {
        Ok(Ok(report)) => Ok(report),
        Ok(Err(e)) => match e.downcast::<SyncErrorBox>() {
            Ok(b) => Err(b.0),
            Err(e) => Err(SyncError::Other(format!("{e:#}"))),
        },
        Err(e) => Err(SyncError::Other(e.to_string())),
    }
}

/// An access token for a Google account, refreshed when close to expiry.
async fn google_token(account: Option<&AccountConfig>, tokens: &mut HashMap<String, (String, i64)>) -> Result<String, SyncError> {
    let acc = account.ok_or_else(|| SyncError::Other("The Google account for this calendar was removed.".into()))?;
    if acc.auth != AuthKind::OAuth2 || acc.oauth.provider != "google" {
        return Err(SyncError::Other("This account doesn't sign in with Google.".into()));
    }
    if oauth::calendar_granted(&acc.oauth) == Some(false) {
        return Err(SyncError::NeedsPermission);
    }
    if let Some((tok, exp)) = tokens.get(&acc.id) {
        if *exp > util::now_ts() + 120 {
            return Ok(tok.clone());
        }
    }
    let id = acc.id.clone();
    let refresh = tokio::task::spawn_blocking(move || secrets::get(&id, SecretKind::OAuthRefresh))
        .await
        .map_err(|e| SyncError::Other(e.to_string()))?
        .ok_or_else(|| SyncError::Auth(format!("RustMail isn't signed in to {} yet. {}", acc.email, oauth::SIGN_IN_AGAIN)))?;
    match oauth::refresh(&acc.oauth, &refresh).await {
        Ok(t) => {
            tokens.insert(acc.id.clone(), (t.access.clone(), t.expires_at));
            Ok(t.access)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains(oauth::SIGN_IN_AGAIN) {
                Err(SyncError::Auth(msg))
            } else {
                Err(SyncError::Other(msg))
            }
        }
    }
}

/// User name and password for a CalDAV source: its own (keychain) or those
/// of the mail account it belongs to.
pub fn caldav_credentials(source: &CalendarSource, account: Option<&AccountConfig>) -> Result<caldav::Credentials, SyncError> {
    let (username, password) = match account {
        Some(acc) if !source.account_id.is_empty() => {
            let user = if source.username.trim().is_empty() {
                if acc.imap.username.trim().is_empty() { acc.email.clone() } else { acc.imap.username.trim().to_string() }
            } else {
                source.username.trim().to_string()
            };
            (user, secrets::get(&acc.id, SecretKind::Password))
        }
        _ => (source.username.trim().to_string(), secrets::get(&source.id, SecretKind::CalDavPassword)),
    };
    let password = password.ok_or_else(|| SyncError::Auth("No password is saved for this calendar account. Open Settings → Calendar.".into()))?;
    Ok(caldav::Credentials { username, password })
}

/// The calendar sources RustMail sets up by itself: one per Google account,
/// and one per mail account at a provider known to offer CalDAV with the
/// same password. Returns true when `sources` changed.
pub fn ensure_auto_sources(accounts: &[AccountConfig], sources: &mut Vec<CalendarSource>) -> bool {
    let mut changed = false;
    for acc in accounts.iter().filter(|a| !a.is_local() && a.enabled) {
        if sources.iter().any(|s| s.account_id == acc.id) {
            continue;
        }
        let google = acc.auth == AuthKind::OAuth2 && acc.oauth.provider == "google";
        let caldav_url = if acc.auth == AuthKind::Password { caldav::preset_url(&util::domain_of(&acc.email), &acc.imap.host) } else { None };
        if google {
            sources.push(CalendarSource {
                label: acc.email.clone(),
                kind: CalendarSourceKind::Google,
                account_id: acc.id.clone(),
                ..Default::default()
            });
            changed = true;
        } else if let Some(url) = caldav_url {
            sources.push(CalendarSource {
                label: acc.email.clone(),
                kind: CalendarSourceKind::CalDav,
                account_id: acc.id.clone(),
                url: url.to_string(),
                ..Default::default()
            });
            changed = true;
        }
    }
    // Sources of removed accounts go too.
    let before = sources.len();
    sources.retain(|s| s.account_id.is_empty() || accounts.iter().any(|a| a.id == s.account_id));
    changed || sources.len() != before
}
