//! Background sync engine. Runs a tokio runtime on its own thread with one
//! actor per account. The UI talks to it with [`Command`]s and receives
//! [`Event`]s; the UI applies changes to the local store optimistically and
//! the engine mirrors them on the server.

pub mod autoconfig;
pub mod imap;
pub mod net;
pub mod oauth;
pub mod rules;
pub mod smtp;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;

use crate::config::{AccountConfig, AppConfig, AuthKind, RuleAction};
use crate::mime;
use crate::model::*;
use crate::secrets::{self, SecretKind};
use crate::store::{NewMessage, Store};
use crate::util;
use imap::{Credentials, ImapConn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsgRef {
    pub id: MsgId,
    pub folder_id: FolderId,
    pub uid: i64,
}

impl MsgRef {
    pub fn of(m: &MessageSummary) -> MsgRef {
        MsgRef {
            id: m.id,
            folder_id: m.folder_id,
            uid: m.uid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagKind {
    Seen,
    Flagged,
    Answered,
    Forwarded,
}

impl FlagKind {
    fn imap(self) -> &'static str {
        match self {
            FlagKind::Seen => "\\Seen",
            FlagKind::Flagged => "\\Flagged",
            FlagKind::Answered => "\\Answered",
            FlagKind::Forwarded => "$Forwarded",
        }
    }
}

pub enum Command {
    ConfigChanged(Box<AppConfig>),
    SyncAll { full: bool },
    SyncAccount { account_id: String, full: bool },
    SyncFolder { folder_id: FolderId },
    LoadOlder { folder_id: FolderId },
    FetchBody { msg_id: MsgId },
    SetFlag { account_id: String, items: Vec<MsgRef>, flag: FlagKind, value: bool },
    Move { account_id: String, items: Vec<MsgRef>, dest: FolderId },
    Archive { account_id: String, items: Vec<MsgRef> },
    Delete { account_id: String, items: Vec<MsgRef>, permanent: bool },
    Junk { account_id: String, items: Vec<MsgRef>, not_junk: bool },
    EmptyFolder { folder_id: FolderId },
    CreateFolder { account_id: String, name: String, parent: Option<FolderId> },
    RenameFolder { folder_id: FolderId, name: String },
    DeleteFolder { folder_id: FolderId },
    SaveDraft { account_id: String, raw: Vec<u8>, replace: Option<MsgRef> },
    ProcessOutbox,
    TestAccount { account: Box<AccountConfig>, password: String, smtp_password: String },
    Discover { email: String },
    OAuthSignIn { account: Box<AccountConfig> },
    Shutdown,
}

#[derive(Debug)]
#[allow(dead_code)] // some fields are informational for the UI
pub enum Event {
    Status { account_id: String, status: SyncStatus },
    FoldersChanged { account_id: String },
    MessagesChanged { account_id: String },
    BodyReady { msg_id: MsgId },
    BodyFailed { msg_id: MsgId, error: String },
    NewMail { account_id: String, count: usize, samples: Vec<(String, String)> },
    Sent { subject: String },
    SendFailed { subject: String, error: String },
    DraftSaved,
    TestResult { imap: Result<String, String>, smtp: Result<String, String> },
    Discovered { email: String, result: Result<autoconfig::Discovered, String> },
    OAuthResult { account_id: String, result: Result<(), String> },
    Toast { text: String, error: bool },
}

#[derive(Clone)]
struct Notifier {
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
}

impl Notifier {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

struct Shared {
    db_path: PathBuf,
    notify: Notifier,
    config: RwLock<AppConfig>,
}

impl Shared {
    fn cfg(&self) -> AppConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }
}

pub struct Engine {
    tx: UnboundedSender<Command>,
    pub rx: crossbeam_channel::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    pub fn start(ctx: egui::Context, db_path: PathBuf, cfg: AppConfig) -> Engine {
        let (tx, cmd_rx) = unbounded_channel();
        let (ev_tx, rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            db_path,
            notify: Notifier { tx: ev_tx, ctx },
            config: RwLock::new(cfg),
        });
        let thread = std::thread::Builder::new()
            .name("rustmail-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_name("rustmail-io")
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(main_loop(cmd_rx, shared));
                rt.shutdown_timeout(Duration::from_secs(2));
            })
            .expect("engine thread");
        Engine {
            tx,
            rx,
            thread: Some(thread),
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Asks the engine to finish queued work and stop, waiting up to `timeout`.
    pub fn shutdown(&mut self, timeout: Duration) {
        self.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let deadline = std::time::Instant::now() + timeout;
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

// ------------------------------------------------------------------ dispatcher

enum ACmd {
    Sync { full: bool },
    SyncFolder(FolderId),
    LoadOlder(FolderId),
    FetchBody(MsgId),
    SetFlag { items: Vec<MsgRef>, flag: FlagKind, value: bool },
    Move { items: Vec<MsgRef>, dest: FolderId },
    Archive { items: Vec<MsgRef> },
    Delete { items: Vec<MsgRef>, permanent: bool },
    Junk { items: Vec<MsgRef>, not_junk: bool },
    EmptyFolder(FolderId),
    CreateFolder { name: String, parent: Option<FolderId> },
    RenameFolder { folder_id: FolderId, name: String },
    DeleteFolder(FolderId),
    SaveDraft { raw: Vec<u8>, replace: Option<MsgRef> },
    Send(i64),
    Reconfigure(Box<AccountConfig>),
    Stop,
}

async fn main_loop(mut rx: UnboundedReceiver<Command>, sh: Arc<Shared>) {
    let store = match Store::open(&sh.db_path) {
        Ok(s) => s,
        Err(e) => {
            sh.notify.emit(Event::Toast {
                text: format!("Could not open the mail database: {e}"),
                error: true,
            });
            return;
        }
    };
    let _ = store.outbox_reset_sending();
    let mut actors: HashMap<String, (UnboundedSender<ACmd>, AccountConfig)> = HashMap::new();
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for acc in sh.cfg().accounts.into_iter().filter(|a| a.enabled) {
        tasks.push(spawn_actor(acc, &sh, &mut actors));
    }
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                let to_account = |actors: &HashMap<String, (UnboundedSender<ACmd>, AccountConfig)>, id: &str, c: ACmd| {
                    if let Some((tx, _)) = actors.get(id) {
                        let _ = tx.send(c);
                    }
                };
                match cmd {
                    Command::Shutdown => break,
                    Command::ConfigChanged(cfg) => {
                        let cfg = *cfg;
                        if let Ok(mut w) = sh.config.write() {
                            *w = cfg.clone();
                        }
                        let wanted: BTreeMap<String, AccountConfig> = cfg
                            .accounts
                            .iter()
                            .filter(|a| a.enabled)
                            .map(|a| (a.id.clone(), a.clone()))
                            .collect();
                        let existing: Vec<String> = actors.keys().cloned().collect();
                        for id in existing {
                            match wanted.get(&id) {
                                None => {
                                    if let Some((tx, _)) = actors.remove(&id) {
                                        let _ = tx.send(ACmd::Stop);
                                    }
                                }
                                Some(acc) => {
                                    if let Some((tx, old)) = actors.get_mut(&id) {
                                        if old != acc {
                                            let _ = tx.send(ACmd::Reconfigure(Box::new(acc.clone())));
                                            *old = acc.clone();
                                        }
                                    }
                                }
                            }
                        }
                        for (id, acc) in wanted {
                            if !actors.contains_key(&id) {
                                tasks.push(spawn_actor(acc, &sh, &mut actors));
                            }
                        }
                    }
                    Command::SyncAll { full } => {
                        for (tx, _) in actors.values() {
                            let _ = tx.send(ACmd::Sync { full });
                        }
                    }
                    Command::SyncAccount { account_id, full } => {
                        to_account(&actors, &account_id, ACmd::Sync { full })
                    }
                    Command::SyncFolder { folder_id } => {
                        if let Ok(Some(f)) = store.folder(folder_id) {
                            to_account(&actors, &f.account_id, ACmd::SyncFolder(folder_id));
                        }
                    }
                    Command::LoadOlder { folder_id } => {
                        if let Ok(Some(f)) = store.folder(folder_id) {
                            to_account(&actors, &f.account_id, ACmd::LoadOlder(folder_id));
                        }
                    }
                    Command::FetchBody { msg_id } => match store.message(msg_id) {
                        Ok(Some(m)) => to_account(&actors, &m.account_id, ACmd::FetchBody(msg_id)),
                        _ => sh.notify.emit(Event::BodyFailed { msg_id, error: "Message not found".into() }),
                    },
                    Command::SetFlag { account_id, items, flag, value } => {
                        to_account(&actors, &account_id, ACmd::SetFlag { items, flag, value })
                    }
                    Command::Move { account_id, items, dest } => {
                        to_account(&actors, &account_id, ACmd::Move { items, dest })
                    }
                    Command::Archive { account_id, items } => {
                        to_account(&actors, &account_id, ACmd::Archive { items })
                    }
                    Command::Delete { account_id, items, permanent } => {
                        to_account(&actors, &account_id, ACmd::Delete { items, permanent })
                    }
                    Command::Junk { account_id, items, not_junk } => {
                        to_account(&actors, &account_id, ACmd::Junk { items, not_junk })
                    }
                    Command::EmptyFolder { folder_id } => {
                        if let Ok(Some(f)) = store.folder(folder_id) {
                            to_account(&actors, &f.account_id, ACmd::EmptyFolder(folder_id));
                        }
                    }
                    Command::CreateFolder { account_id, name, parent } => {
                        to_account(&actors, &account_id, ACmd::CreateFolder { name, parent })
                    }
                    Command::RenameFolder { folder_id, name } => {
                        if let Ok(Some(f)) = store.folder(folder_id) {
                            to_account(&actors, &f.account_id, ACmd::RenameFolder { folder_id, name });
                        }
                    }
                    Command::DeleteFolder { folder_id } => {
                        if let Ok(Some(f)) = store.folder(folder_id) {
                            to_account(&actors, &f.account_id, ACmd::DeleteFolder(folder_id));
                        }
                    }
                    Command::SaveDraft { account_id, raw, replace } => {
                        to_account(&actors, &account_id, ACmd::SaveDraft { raw, replace })
                    }
                    Command::ProcessOutbox => process_outbox(&store, &actors, &sh),
                    Command::Discover { email } => {
                        let n = sh.notify.clone();
                        tokio::spawn(async move {
                            let result = autoconfig::discover(&email).await.map_err(|e| format!("{e:#}"));
                            n.emit(Event::Discovered { email, result });
                        });
                    }
                    Command::TestAccount { account, password, smtp_password } => {
                        let n = sh.notify.clone();
                        tokio::spawn(async move {
                            let (imap, smtp) = test_account(&account, password, smtp_password).await;
                            n.emit(Event::TestResult { imap, smtp });
                        });
                    }
                    Command::OAuthSignIn { account } => {
                        let n = sh.notify.clone();
                        tokio::spawn(async move {
                            let result = oauth::authorize(&account.oauth, &account.email).await;
                            let result = match result {
                                Ok(t) => {
                                    if let Some(r) = t.refresh {
                                        let id = account.id.clone();
                                        let _ = tokio::task::spawn_blocking(move || {
                                            secrets::set(&id, SecretKind::OAuthRefresh, &r)
                                        })
                                        .await;
                                        Ok(())
                                    } else {
                                        Err("The provider did not return a refresh token".to_string())
                                    }
                                }
                                Err(e) => Err(format!("{e:#}")),
                            };
                            n.emit(Event::OAuthResult { account_id: account.id.clone(), result });
                        });
                    }
                }
            }
            _ = tick.tick() => process_outbox(&store, &actors, &sh),
        }
    }
    // Let actors finish queued server work (moves, sends) before exiting.
    for (_, (tx, _)) in actors {
        let _ = tx.send(ACmd::Stop);
    }
    let _ = tokio::time::timeout(Duration::from_secs(8), futures::future::join_all(tasks)).await;
}

fn spawn_actor(
    acc: AccountConfig,
    sh: &Arc<Shared>,
    actors: &mut HashMap<String, (UnboundedSender<ACmd>, AccountConfig)>,
) -> tokio::task::JoinHandle<()> {
    let (tx, rx) = unbounded_channel();
    actors.insert(acc.id.clone(), (tx, acc.clone()));
    let sh = sh.clone();
    tokio::spawn(async move {
        match Store::open(&sh.db_path) {
            Ok(store) => {
                let w = Worker {
                    acc,
                    store,
                    sh,
                    conn: None,
                    token: None,
                    exists: HashMap::new(),
                };
                w.run(rx).await;
            }
            Err(e) => log::error!("worker store: {e}"),
        }
    })
}

fn process_outbox(
    store: &Store,
    actors: &HashMap<String, (UnboundedSender<ACmd>, AccountConfig)>,
    sh: &Shared,
) {
    let Ok(ids) = store.claim_due_outbox(util::now_ts()) else {
        return;
    };
    for id in ids {
        let Ok(Some(item)) = store.outbox_item(id) else { continue };
        match actors.get(&item.account_id) {
            Some((tx, _)) => {
                let _ = tx.send(ACmd::Send(id));
            }
            None => {
                let _ = store.outbox_failed(id, "The sending account is disabled", util::now_ts() + 3600);
                sh.notify.emit(Event::SendFailed {
                    subject: item.subject,
                    error: "The sending account is disabled".into(),
                });
            }
        }
    }
}

async fn resolve_credentials(
    acc: &AccountConfig,
    smtp: bool,
    password: Option<String>,
) -> Result<Credentials> {
    Ok(resolve_credentials_with_expiry(acc, smtp, password).await?.0)
}

/// Like [`resolve_credentials`], also returning when an OAuth token expires.
async fn resolve_credentials_with_expiry(
    acc: &AccountConfig,
    smtp: bool,
    password: Option<String>,
) -> Result<(Credentials, i64)> {
    let server = if smtp { &acc.smtp } else { &acc.imap };
    let username = if server.username.trim().is_empty() {
        acc.email.clone()
    } else {
        server.username.trim().to_string()
    };
    match acc.auth {
        AuthKind::Password => {
            let pw = match password.filter(|p| !p.is_empty()) {
                Some(p) => p,
                None => {
                    let kind = if smtp && !acc.smtp_same_password {
                        SecretKind::SmtpPassword
                    } else {
                        SecretKind::Password
                    };
                    let id = acc.id.clone();
                    tokio::task::spawn_blocking(move || secrets::get(&id, kind))
                        .await?
                        .ok_or_else(|| {
                            anyhow!(
                                "No password is saved for {}. Open Settings → Accounts to enter it.",
                                acc.email
                            )
                        })?
                }
            };
            Ok((
                Credentials {
                    username,
                    secret: pw,
                    oauth: false,
                },
                i64::MAX,
            ))
        }
        AuthKind::OAuth2 => {
            let id = acc.id.clone();
            let refresh = tokio::task::spawn_blocking(move || secrets::get(&id, SecretKind::OAuthRefresh))
                .await?
                .ok_or_else(|| anyhow!("Please sign in to {} with your browser first.", acc.email))?;
            let t = oauth::refresh(&acc.oauth, &refresh).await?;
            if let Some(new_refresh) = t.refresh.clone().filter(|r| *r != refresh) {
                // Providers may rotate refresh tokens.
                let id = acc.id.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    secrets::set(&id, SecretKind::OAuthRefresh, &new_refresh)
                })
                .await;
            }
            Ok((
                Credentials {
                    username,
                    secret: t.access,
                    oauth: true,
                },
                t.expires_at,
            ))
        }
    }
}

async fn test_account(
    acc: &AccountConfig,
    password: String,
    smtp_password: String,
) -> (Result<String, String>, Result<String, String>) {
    let imap_res = async {
        let creds = resolve_credentials(acc, false, Some(password.clone())).await?;
        let mut c = imap::connect(&acc.imap, &creds).await?;
        let n = c.list_folders().await.map(|f| f.len()).unwrap_or(0);
        c.logout().await;
        Ok::<_, anyhow::Error>(format!("Signed in ({n} folders found)"))
    }
    .await
    .map_err(|e| format!("{e:#}"));
    let smtp_pw = if acc.smtp_same_password { password } else { smtp_password };
    let smtp_res = async {
        let creds = resolve_credentials(acc, true, Some(smtp_pw)).await?;
        smtp::test(&acc.smtp, &creds).await?;
        Ok::<_, anyhow::Error>("Ready to send".to_string())
    }
    .await
    .map_err(|e| format!("{e:#}"));
    (imap_res, smtp_res)
}

// ---------------------------------------------------------------------- worker

struct Worker {
    acc: AccountConfig,
    store: Store,
    sh: Arc<Shared>,
    conn: Option<ImapConn>,
    token: Option<(String, i64)>,
    /// EXISTS count seen at the last sync of each folder.
    exists: HashMap<FolderId, u32>,
}

/// Runs an IMAP operation; if it fails on an established connection the
/// connection is re-opened and the operation retried once.
macro_rules! with_retry {
    ($w:ident, $op:expr) => {{
        let had_conn = $w.conn.is_some();
        match $op {
            Ok(v) => Ok(v),
            Err(e) if had_conn => {
                log::warn!("[{}] retrying after error: {e:#}", $w.acc.email);
                $w.conn = None;
                $op
            }
            Err(e) => Err(e),
        }
    }};
}

impl Worker {
    fn emit(&self, e: Event) {
        self.sh.notify.emit(e);
    }

    fn status(&self, status: SyncStatus) {
        self.emit(Event::Status {
            account_id: self.acc.id.clone(),
            status,
        });
    }

    fn toast_err(&self, what: &str, e: &anyhow::Error) {
        self.emit(Event::Toast {
            text: format!("{what}: {e:#}"),
            error: true,
        });
    }

    fn changed(&self) {
        self.emit(Event::MessagesChanged {
            account_id: self.acc.id.clone(),
        });
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(self.acc.sync_interval_min.clamp(1, 1440) as u64 * 60)
    }

    async fn run(mut self, mut rx: UnboundedReceiver<ACmd>) {
        if self.acc.is_local() {
            self.status(SyncStatus::Idle { last: util::now_ts() });
            while let Some(cmd) = rx.recv().await {
                if matches!(cmd, ACmd::Stop) {
                    break;
                }
                if let Err(e) = self.handle_local(cmd) {
                    self.toast_err("Action failed", &e);
                }
            }
            return;
        }
        let mut next_sync = Instant::now() + Duration::from_millis(300);
        let mut idle_failures = 0u32;
        loop {
            // Push: wait in IDLE on the Inbox while nothing else is going on.
            let can_idle = self.acc.push
                && idle_failures < 3
                && self.conn.as_ref().map(|c| c.has_idle).unwrap_or(false)
                && Instant::now() < next_sync;
            let mut pending: Option<Option<ACmd>> = None;
            if can_idle {
                let inbox = self.store.folder_by_role(&self.acc.id, FolderRole::Inbox).ok().flatten();
                // Mail may have arrived while we were busy: IDLE only reports
                // changes from now on, so compare UIDNEXT first.
                if let (Some(inbox), Some(conn)) = (&inbox, self.conn.as_mut()) {
                    let known = self.store.folder_sync_state(inbox.id).map(|s| s.1).unwrap_or(0);
                    match conn.select(&inbox.path).await {
                        Ok(mb) => {
                            let uid_moved = mb.uid_next.map(|n| n as i64 != known).unwrap_or(false);
                            let count_moved = self.exists.get(&inbox.id).map(|e| *e != mb.exists).unwrap_or(true);
                            if uid_moved || count_moved {
                                let inbox = inbox.clone();
                                if let Err(e) = with_retry!(self, self.sync_folder(&inbox).await) {
                                    log::warn!("inbox refresh failed: {e:#}");
                                    self.conn = None;
                                }
                                continue;
                            }
                        }
                        Err(e) => {
                            log::info!("select before IDLE failed: {e:#}");
                            self.conn = None;
                            continue;
                        }
                    }
                }
                if let (Some(inbox), Some(conn)) = (inbox, self.conn.take()) {
                    let wait = next_sync
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(25 * 60));
                    let mut got: Option<Option<ACmd>> = None;
                    let res = conn
                        .idle(&inbox.path, wait, async {
                            got = Some(rx.recv().await);
                        })
                        .await;
                    match res {
                        Ok((conn, end)) => {
                            idle_failures = 0;
                            self.conn = Some(conn);
                            if end == imap::IdleEnd::Changed {
                                self.status(SyncStatus::Syncing("New mail…".into()));
                                match with_retry!(self, self.sync_folder(&inbox).await) {
                                    Ok(()) => self.status(SyncStatus::Idle { last: util::now_ts() }),
                                    Err(e) => {
                                        self.conn = None;
                                        self.status(SyncStatus::Error(format!("{e:#}")));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::info!("[{}] IDLE ended: {e:#}", self.acc.email);
                            idle_failures += 1;
                            self.conn = None;
                            next_sync = next_sync.min(Instant::now() + Duration::from_secs(30));
                        }
                    }
                    pending = got;
                }
            }
            let cmd = match pending {
                Some(c) => Some(c),
                None if can_idle => None,
                None => tokio::select! {
                    biased;
                    cmd = rx.recv() => Some(cmd),
                    _ = tokio::time::sleep_until(next_sync) => None,
                },
            };
            match cmd {
                None => {
                    if Instant::now() >= next_sync {
                        self.sync(false).await;
                        next_sync = Instant::now() + self.interval();
                    }
                }
                Some(None) | Some(Some(ACmd::Stop)) => break,
                Some(Some(ACmd::Sync { full })) => {
                    self.sync(full).await;
                    next_sync = Instant::now() + self.interval();
                    idle_failures = 0;
                }
                Some(Some(ACmd::Reconfigure(acc))) => {
                    let server_changed = acc.imap != self.acc.imap || acc.auth != self.acc.auth || acc.oauth != self.acc.oauth;
                    self.acc = *acc;
                    if server_changed {
                        if let Some(c) = self.conn.take() {
                            c.logout().await;
                        }
                        self.token = None;
                        next_sync = Instant::now();
                    }
                }
                Some(Some(cmd)) => self.handle(cmd).await,
            }
        }
        if let Some(c) = self.conn.take() {
            c.logout().await;
        }
    }

    async fn credentials(&mut self, smtp: bool) -> Result<Credentials> {
        if self.acc.auth == AuthKind::OAuth2 {
            if let Some((tok, exp)) = &self.token {
                if *exp > util::now_ts() + 60 {
                    let server = if smtp { &self.acc.smtp } else { &self.acc.imap };
                    let username = if server.username.trim().is_empty() {
                        self.acc.email.clone()
                    } else {
                        server.username.trim().to_string()
                    };
                    return Ok(Credentials {
                        username,
                        secret: tok.clone(),
                        oauth: true,
                    });
                }
            }
            let (c, expires_at) = resolve_credentials_with_expiry(&self.acc, smtp, None).await?;
            self.token = Some((c.secret.clone(), expires_at));
            return Ok(c);
        }
        resolve_credentials(&self.acc, smtp, None).await
    }

    async fn ensure_conn(&mut self) -> Result<()> {
        if self.conn.is_none() {
            self.status(SyncStatus::Connecting);
            let creds = self.credentials(false).await?;
            let c = imap::connect(&self.acc.imap, &creds).await?;
            self.conn = Some(c);
        }
        Ok(())
    }

    fn conn_mut(&mut self) -> Result<&mut ImapConn> {
        self.conn.as_mut().ok_or_else(|| anyhow!("not connected"))
    }

    fn folder_path(&self, id: FolderId) -> Result<String> {
        self.store
            .folder(id)?
            .map(|f| f.path)
            .ok_or_else(|| anyhow!("Folder no longer exists"))
    }

    // ------------------------------------------------------------- syncing

    async fn sync(&mut self, full: bool) {
        self.status(SyncStatus::Syncing("Checking for new mail…".into()));
        let result = with_retry!(self, self.sync_inner(full).await);
        match result {
            Ok(()) => self.status(SyncStatus::Idle { last: util::now_ts() }),
            Err(e) => {
                log::warn!("[{}] sync failed: {e:#}", self.acc.email);
                self.conn = None;
                self.status(SyncStatus::Error(format!("{e:#}")));
            }
        }
    }

    async fn sync_folder_list(&mut self) -> Result<()> {
        self.ensure_conn().await?;
        let mut remote = self.conn_mut()?.list_folders().await?;
        let roles = imap::assign_roles(&mut remote);
        let local = self.store.folders(&self.acc.id)?;
        let mut changed = false;
        for f in &remote {
            let role = roles.get(&f.path).copied().unwrap_or(FolderRole::Custom);
            let decoded = util::decode_imap_utf7(&f.path);
            let name = match f.delimiter.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => decoded.rsplit(d).next().unwrap_or(&decoded).to_string(),
                None => decoded.clone(),
            };
            let existing = local.iter().find(|l| l.path == f.path);
            let needs = match existing {
                Some(l) => l.role != role || l.selectable != f.selectable || l.name != name,
                None => true,
            };
            if needs {
                self.store.upsert_folder(
                    &self.acc.id,
                    &f.path,
                    &name,
                    f.delimiter.as_deref(),
                    role,
                    f.selectable,
                )?;
                changed = true;
            }
        }
        for l in &local {
            if !remote.iter().any(|r| r.path == l.path) {
                self.store.delete_folder(l.id)?;
                changed = true;
            }
        }
        if changed {
            self.emit(Event::FoldersChanged {
                account_id: self.acc.id.clone(),
            });
        }
        Ok(())
    }

    async fn sync_inner(&mut self, full: bool) -> Result<()> {
        self.sync_folder_list().await?;
        let first_key = format!("synced:{}", self.acc.id);
        let first_time = self.store.kv_get(&first_key).is_none();
        let folders: Vec<Folder> = self
            .store
            .folders(&self.acc.id)?
            .into_iter()
            .filter(|f| f.selectable)
            .filter(|f| {
                let never = self.store.folder_sync_state(f.id).map(|s| s.0 == 0).unwrap_or(true);
                match f.role {
                    FolderRole::All => false,
                    FolderRole::Custom => full || first_time || never,
                    _ => true,
                }
            })
            .collect();
        for f in folders {
            self.status(SyncStatus::Syncing(format!("Syncing {}…", f.label())));
            if let Err(e) = self.sync_folder(&f).await {
                log::warn!("[{}] folder {} failed: {e:#}", self.acc.email, f.path);
                // Is the connection still usable?
                let alive = match self.conn.as_mut() {
                    Some(c) => c.noop().await.is_ok(),
                    None => false,
                };
                if !alive {
                    return Err(e);
                }
            }
        }
        self.store.kv_set(&first_key, "1");
        Ok(())
    }

    async fn sync_folder(&mut self, f: &Folder) -> Result<()> {
        self.ensure_conn().await?;
        let mb = self.conn_mut()?.select(&f.path).await?;
        self.exists.insert(f.id, mb.exists);
        let uidv = mb.uid_validity.unwrap_or(0) as i64;
        let (old_uidv, _) = self.store.folder_sync_state(f.id)?;
        if old_uidv != 0 && uidv != 0 && old_uidv != uidv {
            log::info!("UIDVALIDITY changed for {}, resyncing", f.path);
            self.store.clear_folder_messages(f.id)?;
        }
        let max_uid = self.store.max_uid(f.id)?;
        let initial = max_uid <= 0;
        let mut fetched = Vec::new();
        if mb.exists > 0 {
            let conn = self.conn.as_mut().ok_or_else(|| anyhow!("not connected"))?;
            if initial {
                let limit = if f.role == FolderRole::Inbox {
                    self.acc.initial_sync_count.max(50)
                } else {
                    self.acc.initial_sync_count.clamp(50, 500)
                };
                let start = mb.exists.saturating_sub(limit.saturating_sub(1)).max(1);
                fetched = conn.fetch_seq(&format!("{start}:*"), imap::HEADER_QUERY).await?;
            } else if mb.uid_next.map(|n| n as i64 > max_uid + 1).unwrap_or(true) {
                fetched = conn
                    .fetch_uid(&format!("{}:*", max_uid + 1), imap::HEADER_QUERY)
                    .await?;
            }
        }
        let new_ids = self.insert_fetched(f, &fetched, max_uid)?;

        // Flag changes and deletions for messages we already have.
        if !initial {
            let local = self.store.folder_uids(f.id)?;
            if let Some(min) = local.iter().map(|x| x.0).min() {
                let conn = self.conn.as_mut().ok_or_else(|| anyhow!("not connected"))?;
                let fetches = conn
                    .fetch_uid(&format!("{min}:{max_uid}"), "(UID FLAGS)")
                    .await?;
                let mut server: HashMap<i64, imap::Flags> = HashMap::new();
                for fe in &fetches {
                    if let Some(u) = fe.uid {
                        server.insert(u as i64, imap::flags_of(fe));
                    }
                }
                let mut gone = Vec::new();
                {
                    let tx = self.store.conn.unchecked_transaction()?;
                    for (uid, id) in &local {
                        if *uid > max_uid {
                            continue;
                        }
                        match server.get(uid) {
                            Some(fl) => self.store.update_server_flags(
                                *id,
                                fl.seen,
                                fl.flagged,
                                fl.answered,
                                fl.forwarded,
                                fl.draft,
                            )?,
                            None => gone.push(*uid),
                        }
                    }
                    tx.commit()?;
                }
                if !gone.is_empty() {
                    self.store.delete_by_uids(f.id, &gone)?;
                }
            }
        }
        self.store.purge_stale_pending(f.id, 900)?;
        self.store
            .set_folder_sync_state(f.id, uidv, mb.uid_next.unwrap_or(0) as i64)?;
        if let Some(c) = self.conn.as_mut() {
            c.drain_unsolicited();
        }
        self.changed();

        if self.acc.prefetch_bodies {
            let limit = if f.role == FolderRole::Inbox { 60 } else { 20 };
            if let Err(e) = self.prefetch_bodies(f, limit).await {
                log::warn!("body prefetch failed: {e:#}");
            }
        }

        if !initial && f.role == FolderRole::Inbox && !new_ids.is_empty() {
            self.apply_rules(&new_ids).await;
            let msgs = self.store.messages_by_ids(&new_ids)?;
            let unseen: Vec<&MessageSummary> = msgs
                .iter()
                .filter(|m| !m.seen && m.folder_id == f.id)
                .collect();
            if !unseen.is_empty() {
                let samples: Vec<(String, String)> = unseen
                    .iter()
                    .take(3)
                    .map(|m| (m.sender().to_string(), m.subject_or_placeholder().to_string()))
                    .collect();
                let cfg = self.sh.cfg();
                if cfg.notifications.enabled {
                    let (summary, body) = if unseen.len() == 1 {
                        let m = unseen[0];
                        (
                            m.sender().to_string(),
                            if cfg.notifications.show_preview {
                                format!("{}\n{}", m.subject_or_placeholder(), util::truncate_chars(&m.preview, 120))
                            } else {
                                m.subject_or_placeholder().to_string()
                            },
                        )
                    } else {
                        (
                            format!("{} new messages", unseen.len()),
                            samples
                                .iter()
                                .map(|(s, sub)| format!("{s}: {sub}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    };
                    crate::notify::desktop(summary, body);
                }
                self.emit(Event::NewMail {
                    account_id: self.acc.id.clone(),
                    count: unseen.len(),
                    samples,
                });
                self.changed();
            }
        }
        Ok(())
    }

    fn insert_fetched(
        &mut self,
        f: &Folder,
        fetched: &[async_imap::types::Fetch],
        max_uid: i64,
    ) -> Result<Vec<MsgId>> {
        if fetched.is_empty() {
            return Ok(Vec::new());
        }
        let overrides = self.sh.cfg().mail.focused_overrides;
        let blocked = self.sh.cfg().mail.blocked_senders;
        let mut new_ids = Vec::new();
        let tx = self.store.conn.unchecked_transaction()?;
        for fe in fetched {
            let Some(uid) = fe.uid else { continue };
            if (uid as i64) <= max_uid {
                continue;
            }
            let info = mime::header_info(fe.header().unwrap_or(b""));
            let fl = imap::flags_of(fe);
            let date = if info.date > 0 {
                info.date
            } else {
                fe.internal_date().map(|d| d.timestamp()).unwrap_or(0)
            };
            let sender = info.from.email.to_lowercase();
            let focused = match overrides.get(&sender) {
                Some(v) => *v,
                None => !info.bulk,
            };
            let nm = NewMessage {
                account_id: self.acc.id.clone(),
                folder_id: f.id,
                uid: uid as i64,
                message_id: info.message_id.clone(),
                in_reply_to: info.in_reply_to.clone(),
                references: info.references.clone(),
                subject: info.subject.clone(),
                from_name: info.from.name.clone(),
                from_addr: info.from.email.clone(),
                to_list: Addr::list_to_string(&info.to),
                cc_list: Addr::list_to_string(&info.cc),
                date,
                preview: String::new(),
                size: fe.size.unwrap_or(0) as i64,
                seen: fl.seen || blocked.iter().any(|b| b.eq_ignore_ascii_case(&sender)),
                flagged: fl.flagged,
                answered: fl.answered,
                forwarded: fl.forwarded,
                draft: fl.draft,
                has_attachments: info.multipart_mixed,
                importance: info.importance,
                focused,
                list_unsubscribe: info.list_unsubscribe.clone(),
                raw: None,
                body_text: String::new(),
            };
            let (id, is_new) = self.store.insert_message(&nm)?;
            if is_new {
                new_ids.push(id);
            }
        }
        tx.commit()?;
        Ok(new_ids)
    }

    fn save_body(&self, id: MsgId, raw: &[u8]) -> Result<()> {
        let parsed = mime::parse_full(raw).ok_or_else(|| anyhow!("Could not parse message"))?;
        let has_att = parsed.attachments.iter().any(|a| !a.inline);
        let text = mime::plain_text(&parsed);
        let preview = util::make_preview(&text, 220);
        self.store.store_body(id, raw, &preview, has_att, &text)
    }

    async fn prefetch_bodies(&mut self, f: &Folder, limit: usize) -> Result<()> {
        let missing = self.store.messages_missing_body(f.id, limit, 1_500_000)?;
        if missing.is_empty() {
            return Ok(());
        }
        for chunk in missing.chunks(15) {
            let uids: Vec<i64> = chunk.iter().map(|x| x.1).collect();
            let conn = self.conn.as_mut().ok_or_else(|| anyhow!("not connected"))?;
            conn.select(&f.path).await?;
            let fetches = conn.fetch_uid(&imap::uid_set(&uids), imap::BODY_QUERY).await?;
            for fe in &fetches {
                if let (Some(uid), Some(body)) = (fe.uid, fe.body()) {
                    if let Some((id, _)) = chunk.iter().find(|x| x.1 == uid as i64) {
                        if let Err(e) = self.save_body(*id, body) {
                            log::warn!("body {id}: {e:#}");
                        }
                    }
                }
            }
            self.changed();
        }
        Ok(())
    }

    async fn apply_rules(&mut self, ids: &[MsgId]) {
        let cfg = self.sh.cfg();
        if cfg.rules.iter().all(|r| !r.enabled) {
            return;
        }
        let Ok(msgs) = self.store.messages_by_ids(ids) else {
            return;
        };
        for m in msgs {
            let body = self
                .store
                .raw_body(m.id)
                .ok()
                .flatten()
                .and_then(|raw| mime::parse_full(&raw))
                .map(|p| mime::plain_text(&p))
                .unwrap_or_default();
            let actions = rules::actions_for(&cfg.rules, &self.acc.id, &m, &body);
            let r = MsgRef::of(&m);
            for a in actions {
                let res: Result<()> = match a {
                    RuleAction::MarkRead => {
                        let _ = self.store.set_bool(&[m.id], "seen", true);
                        self.op_flag(&[r], FlagKind::Seen, true).await
                    }
                    RuleAction::Flag => {
                        let _ = self.store.set_bool(&[m.id], "flagged", true);
                        self.op_flag(&[r], FlagKind::Flagged, true).await
                    }
                    RuleAction::Pin => self.store.set_bool(&[m.id], "pinned", true),
                    RuleAction::MoveToOther => self.store.set_bool(&[m.id], "focused", false),
                    RuleAction::Categorize(c) => {
                        let mut cats = m.categories.clone();
                        if !cats.contains(&c) {
                            cats.push(c);
                        }
                        self.store.set_categories(m.id, &cats)
                    }
                    RuleAction::MoveTo(path) => match self.store.folder_by_path(&self.acc.id, &path) {
                        Ok(Some(dest)) => {
                            let res = self.op_move(&[r], dest.id).await;
                            if res.is_ok() {
                                let _ = self.store.move_local(&[m.id], dest.id, false);
                            }
                            res
                        }
                        _ => Err(anyhow!("Rule target folder \"{path}\" not found")),
                    },
                    RuleAction::Delete => {
                        let res = self.op_delete(&[r], false).await;
                        if res.is_ok() {
                            if let Ok(Some(t)) = self.store.folder_by_role(&self.acc.id, FolderRole::Trash) {
                                let _ = self.store.move_local(&[m.id], t.id, false);
                            }
                        }
                        res
                    }
                };
                if let Err(e) = res {
                    log::warn!("rule action failed: {e:#}");
                }
            }
        }
        self.changed();
    }

    // ------------------------------------------------------------ operations

    async fn op_flag(&mut self, items: &[MsgRef], flag: FlagKind, value: bool) -> Result<()> {
        self.ensure_conn().await?;
        let mut by_folder: BTreeMap<FolderId, Vec<i64>> = BTreeMap::new();
        for it in items.iter().filter(|i| i.uid > 0) {
            by_folder.entry(it.folder_id).or_default().push(it.uid);
        }
        for (fid, uids) in by_folder {
            let path = self.folder_path(fid)?;
            self.conn_mut()?.store_flag(&path, &uids, flag.imap(), value).await?;
        }
        Ok(())
    }

    async fn op_move(&mut self, items: &[MsgRef], dest: FolderId) -> Result<()> {
        self.ensure_conn().await?;
        let dest_path = self.folder_path(dest)?;
        let mut by_folder: BTreeMap<FolderId, Vec<i64>> = BTreeMap::new();
        for it in items.iter().filter(|i| i.uid > 0 && i.folder_id != dest) {
            by_folder.entry(it.folder_id).or_default().push(it.uid);
        }
        for (fid, uids) in by_folder {
            let path = self.folder_path(fid)?;
            self.conn_mut()?.move_uids(&path, &uids, &dest_path).await?;
        }
        Ok(())
    }

    async fn op_delete(&mut self, items: &[MsgRef], permanent: bool) -> Result<()> {
        let trash = if permanent {
            self.store.folder_by_role(&self.acc.id, FolderRole::Trash)?
        } else {
            // Never expunge just because no Trash folder exists yet.
            Some(self.special_folder(FolderRole::Trash, "Trash").await?)
        };
        let mut forever: Vec<MsgRef> = Vec::new();
        let mut to_trash: Vec<MsgRef> = Vec::new();
        for it in items {
            match &trash {
                Some(t) if !permanent && it.folder_id != t.id => to_trash.push(*it),
                _ => forever.push(*it),
            }
        }
        if let (Some(t), false) = (&trash, to_trash.is_empty()) {
            self.op_move(&to_trash, t.id).await?;
        }
        if !forever.is_empty() {
            self.ensure_conn().await?;
            let mut by_folder: BTreeMap<FolderId, Vec<i64>> = BTreeMap::new();
            for it in forever.iter().filter(|i| i.uid > 0) {
                by_folder.entry(it.folder_id).or_default().push(it.uid);
            }
            for (fid, uids) in by_folder {
                let path = self.folder_path(fid)?;
                self.conn_mut()?.delete_uids(&path, &uids).await?;
            }
        }
        Ok(())
    }

    async fn special_folder(&mut self, role: FolderRole, create_name: &str) -> Result<Folder> {
        if let Some(f) = self.store.folder_by_role(&self.acc.id, role)? {
            return Ok(f);
        }
        if role == FolderRole::Archive {
            if let Some(f) = self.store.folder_by_role(&self.acc.id, FolderRole::All)? {
                return Ok(f);
            }
        }
        self.ensure_conn().await?;
        self.conn_mut()?.create_folder(create_name).await?;
        let delim = self
            .store
            .folder_by_role(&self.acc.id, FolderRole::Inbox)?
            .and_then(|f| f.delimiter);
        let id = self.store.upsert_folder(
            &self.acc.id,
            create_name,
            create_name,
            delim.as_deref(),
            role,
            true,
        )?;
        self.emit(Event::FoldersChanged {
            account_id: self.acc.id.clone(),
        });
        self.store
            .folder(id)?
            .ok_or_else(|| anyhow!("folder vanished"))
    }

    async fn handle(&mut self, cmd: ACmd) {
        match cmd {
            ACmd::Sync { .. } | ACmd::Reconfigure(_) | ACmd::Stop => {}
            ACmd::SyncFolder(fid) => {
                if let Ok(Some(f)) = self.store.folder(fid) {
                    self.status(SyncStatus::Syncing(format!("Syncing {}…", f.label())));
                    match with_retry!(self, self.sync_folder(&f).await) {
                        Ok(()) => self.status(SyncStatus::Idle { last: util::now_ts() }),
                        Err(e) => {
                            self.conn = None;
                            self.status(SyncStatus::Error(format!("{e:#}")));
                        }
                    }
                }
            }
            ACmd::LoadOlder(fid) => {
                let r = with_retry!(self, self.load_older(fid).await);
                if let Err(e) = r {
                    self.toast_err("Could not load older messages", &e);
                }
                self.changed();
            }
            ACmd::FetchBody(id) => {
                let r = with_retry!(self, self.fetch_body(id).await);
                match r {
                    Ok(()) => self.emit(Event::BodyReady { msg_id: id }),
                    Err(e) => self.emit(Event::BodyFailed {
                        msg_id: id,
                        error: format!("{e:#}"),
                    }),
                }
            }
            ACmd::SetFlag { items, flag, value } => {
                let r = with_retry!(self, self.op_flag(&items, flag, value).await);
                match r {
                    Ok(()) => {
                        let ids: Vec<MsgId> = items.iter().map(|i| i.id).collect();
                        let _ = self.store.clear_dirty(&ids);
                    }
                    Err(e) => self.toast_err("Could not update the message on the server", &e),
                }
            }
            ACmd::Move { items, dest } => {
                let r = with_retry!(self, self.op_move(&items, dest).await);
                if let Err(e) = r {
                    self.restore(&items);
                    self.toast_err("Move failed", &e);
                }
                self.resync_folder(dest).await;
            }
            ACmd::Archive { items } => {
                let r = async {
                    let f = self.special_folder(FolderRole::Archive, "Archive").await?;
                    with_retry!(self, self.op_move(&items, f.id).await)?;
                    Ok::<_, anyhow::Error>(f.id)
                }
                .await;
                match r {
                    Ok(fid) => self.resync_folder(fid).await,
                    Err(e) => {
                        self.restore(&items);
                        self.toast_err("Archive failed", &e);
                    }
                }
            }
            ACmd::Delete { items, permanent } => {
                let r = with_retry!(self, self.op_delete(&items, permanent).await);
                match r {
                    Ok(()) if permanent => {
                        let ids: Vec<MsgId> = items.iter().map(|i| i.id).collect();
                        let _ = self.store.delete_messages(&ids);
                    }
                    Ok(()) => {}
                    Err(e) => {
                        self.restore(&items);
                        self.toast_err("Delete failed", &e);
                    }
                }
                if let Ok(Some(t)) = self.store.folder_by_role(&self.acc.id, FolderRole::Trash) {
                    self.resync_folder(t.id).await;
                }
            }
            ACmd::Junk { items, not_junk } => {
                let r = async {
                    let dest = if not_junk {
                        self.store
                            .folder_by_role(&self.acc.id, FolderRole::Inbox)?
                            .ok_or_else(|| anyhow!("No Inbox"))?
                    } else {
                        self.special_folder(FolderRole::Junk, "Junk").await?
                    };
                    with_retry!(self, self.op_move(&items, dest.id).await)?;
                    Ok::<_, anyhow::Error>(dest.id)
                }
                .await;
                match r {
                    Ok(fid) => self.resync_folder(fid).await,
                    Err(e) => {
                        self.restore(&items);
                        self.toast_err("Could not move the message", &e);
                    }
                }
            }
            ACmd::EmptyFolder(fid) => {
                let r = with_retry!(self, self.empty_folder(fid).await);
                if let Err(e) = r {
                    self.toast_err("Could not empty the folder", &e);
                }
                self.changed();
            }
            ACmd::CreateFolder { name, parent } => {
                let r = with_retry!(self, self.create_folder(&name, parent).await);
                match r {
                    Ok(()) => self.emit(Event::FoldersChanged {
                        account_id: self.acc.id.clone(),
                    }),
                    Err(e) => self.toast_err("Could not create the folder", &e),
                }
            }
            ACmd::RenameFolder { folder_id, name } => {
                let r = with_retry!(self, self.rename_folder(folder_id, &name).await);
                if let Err(e) = r {
                    self.toast_err("Could not rename the folder", &e);
                }
                let _ = with_retry!(self, self.sync_folder_list().await);
                self.emit(Event::FoldersChanged {
                    account_id: self.acc.id.clone(),
                });
            }
            ACmd::DeleteFolder(fid) => {
                let r = with_retry!(self, self.delete_folder(fid).await);
                match r {
                    Ok(()) => {
                        let _ = self.store.delete_folder(fid);
                        self.emit(Event::FoldersChanged {
                            account_id: self.acc.id.clone(),
                        });
                    }
                    Err(e) => self.toast_err("Could not delete the folder", &e),
                }
            }
            ACmd::SaveDraft { raw, replace } => {
                let r = with_retry!(self, self.save_draft(&raw, replace).await);
                match r {
                    Ok(fid) => {
                        self.emit(Event::DraftSaved);
                        self.resync_folder(fid).await;
                    }
                    Err(e) => self.toast_err("Could not save the draft", &e),
                }
            }
            ACmd::Send(outbox_id) => self.send(outbox_id).await,
        }
    }

    /// Undoes optimistic local changes after a failed server operation.
    fn restore(&self, items: &[MsgRef]) {
        let ids: Vec<MsgId> = items.iter().map(|i| i.id).collect();
        if let Err(e) = self.store.restore_pending(&ids) {
            log::warn!("could not restore messages: {e:#}");
        }
        self.changed();
    }

    async fn resync_folder(&mut self, fid: FolderId) {
        if let Ok(Some(f)) = self.store.folder(fid) {
            if let Err(e) = with_retry!(self, self.sync_folder(&f).await) {
                log::warn!("resync failed: {e:#}");
            }
        }
        self.changed();
    }

    async fn fetch_body(&mut self, id: MsgId) -> Result<()> {
        let m = self
            .store
            .message(id)?
            .ok_or_else(|| anyhow!("Message not found"))?;
        if m.uid <= 0 {
            return Err(anyhow!("This message is still being moved; try again in a moment"));
        }
        let path = self.folder_path(m.folder_id)?;
        self.ensure_conn().await?;
        let conn = self.conn_mut()?;
        conn.select(&path).await?;
        let fetches = conn.fetch_uid(&m.uid.to_string(), imap::BODY_QUERY).await?;
        let body = fetches
            .iter()
            .find(|f| f.uid == Some(m.uid as u32))
            .and_then(|f| f.body())
            .ok_or_else(|| anyhow!("The server did not return the message (it may have been deleted)"))?
            .to_vec();
        self.save_body(id, &body)?;
        self.changed();
        Ok(())
    }

    async fn load_older(&mut self, fid: FolderId) -> Result<()> {
        let f = self
            .store
            .folder(fid)?
            .ok_or_else(|| anyhow!("Folder not found"))?;
        let min = self.store.min_uid(fid)?;
        if min <= 1 {
            return Ok(());
        }
        self.ensure_conn().await?;
        let conn = self.conn_mut()?;
        conn.select(&f.path).await?;
        let uids = conn.uid_search(&format!("UID 1:{}", min - 1)).await?;
        let take: Vec<i64> = uids.iter().rev().take(500).map(|u| *u as i64).collect();
        if take.is_empty() {
            return Ok(());
        }
        let fetched = conn.fetch_uid(&imap::uid_set(&take), imap::HEADER_QUERY).await?;
        self.insert_fetched(&f, &fetched, 0)?;
        Ok(())
    }

    async fn empty_folder(&mut self, fid: FolderId) -> Result<()> {
        let path = self.folder_path(fid)?;
        self.ensure_conn().await?;
        let conn = self.conn_mut()?;
        conn.select(&path).await?;
        let uids: Vec<i64> = conn.uid_search("ALL").await?.into_iter().map(|u| u as i64).collect();
        for chunk in uids.chunks(500) {
            conn.delete_uids(&path, chunk).await?;
        }
        self.store.clear_folder_messages(fid)?;
        Ok(())
    }

    fn child_path(&self, parent: Option<FolderId>, name: &str) -> Result<String> {
        let encoded = util::encode_imap_utf7(name.trim());
        match parent {
            Some(pid) => {
                let p = self
                    .store
                    .folder(pid)?
                    .ok_or_else(|| anyhow!("Parent folder not found"))?;
                let d = p.delimiter.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "/".into());
                Ok(format!("{}{}{}", p.path, d, encoded))
            }
            None => Ok(encoded),
        }
    }

    async fn create_folder(&mut self, name: &str, parent: Option<FolderId>) -> Result<()> {
        if name.trim().is_empty() {
            return Err(anyhow!("Folder name is empty"));
        }
        let path = self.child_path(parent, name)?;
        self.ensure_conn().await?;
        self.conn_mut()?.create_folder(&path).await?;
        let delim = match parent {
            Some(pid) => self.store.folder(pid)?.and_then(|f| f.delimiter),
            None => self
                .store
                .folder_by_role(&self.acc.id, FolderRole::Inbox)?
                .and_then(|f| f.delimiter),
        };
        self.store.upsert_folder(
            &self.acc.id,
            &path,
            name.trim(),
            delim.as_deref(),
            FolderRole::Custom,
            true,
        )?;
        Ok(())
    }

    async fn rename_folder(&mut self, fid: FolderId, name: &str) -> Result<()> {
        let f = self
            .store
            .folder(fid)?
            .ok_or_else(|| anyhow!("Folder not found"))?;
        let encoded = util::encode_imap_utf7(name.trim());
        let new_path = match (f.delimiter.as_deref().filter(|d| !d.is_empty()), f.parent_path()) {
            (Some(d), Some(parent)) => format!("{parent}{d}{encoded}"),
            _ => encoded,
        };
        self.ensure_conn().await?;
        self.conn_mut()?.rename_folder(&f.path, &new_path).await?;
        self.store.rename_folder(fid, &new_path, name.trim())?;
        Ok(())
    }

    async fn delete_folder(&mut self, fid: FolderId) -> Result<()> {
        let path = self.folder_path(fid)?;
        self.ensure_conn().await?;
        self.conn_mut()?.delete_folder(&path).await
    }

    async fn save_draft(&mut self, raw: &[u8], replace: Option<MsgRef>) -> Result<FolderId> {
        let drafts = self.special_folder(FolderRole::Drafts, "Drafts").await?;
        self.ensure_conn().await?;
        self.conn_mut()?.append(&drafts.path, "(\\Seen \\Draft)", raw).await?;
        if let Some(old) = replace {
            if old.uid > 0 {
                let path = self.folder_path(old.folder_id)?;
                let _ = self.conn_mut()?.delete_uids(&path, &[old.uid]).await;
            }
            let _ = self.store.delete_messages(&[old.id]);
        }
        Ok(drafts.id)
    }

    async fn send(&mut self, outbox_id: i64) {
        let Ok(Some(item)) = self.store.outbox_item(outbox_id) else {
            return;
        };
        let result = async {
            let creds = self.credentials(true).await?;
            smtp::send(&self.acc.smtp, &creds, &item.from, &item.recipients, &item.raw).await
        }
        .await;
        match result {
            Ok(()) => {
                let _ = self.store.remove_outbox(outbox_id);
                self.emit(Event::Sent {
                    subject: item.subject.clone(),
                });
                // Post-send bookkeeping is best effort.
                let post = async {
                    if self.acc.save_sent_copy {
                        let sent = self.special_folder(FolderRole::Sent, "Sent").await?;
                        self.ensure_conn().await?;
                        self.conn_mut()?.append(&sent.path, "(\\Seen)", &item.raw).await?;
                    }
                    if let Some(id) = item.reply_to_msg {
                        if let Ok(Some(m)) = self.store.message(id) {
                            let _ = self.store.set_bool(&[id], "answered", true);
                            self.op_flag(&[MsgRef::of(&m)], FlagKind::Answered, true).await?;
                            let _ = self.store.clear_dirty(&[id]);
                        }
                    }
                    if let Some(id) = item.forward_of {
                        if let Ok(Some(m)) = self.store.message(id) {
                            let _ = self.store.set_bool(&[id], "forwarded", true);
                            let _ = self.op_flag(&[MsgRef::of(&m)], FlagKind::Forwarded, true).await;
                            let _ = self.store.clear_dirty(&[id]);
                        }
                    }
                    if let Some(id) = item.draft_msg {
                        if let Ok(Some(m)) = self.store.message(id) {
                            self.op_delete(&[MsgRef::of(&m)], true).await?;
                            let _ = self.store.delete_messages(&[id]);
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                if let Err(e) = post {
                    log::warn!("post-send bookkeeping: {e:#}");
                    self.conn = None;
                }
                if let Ok(Some(sent)) = self.store.folder_by_role(&self.acc.id, FolderRole::Sent) {
                    self.resync_folder(sent.id).await;
                }
            }
            Err(e) => {
                let attempts = item.attempts + 1;
                let retry = util::now_ts() + 60 * attempts.min(30);
                let _ = self.store.outbox_failed(outbox_id, &format!("{e:#}"), retry);
                self.emit(Event::SendFailed {
                    subject: item.subject.clone(),
                    error: format!("{e:#}"),
                });
            }
        }
    }

    // ------------------------------------------------------- local accounts

    fn handle_local(&mut self, cmd: ACmd) -> Result<()> {
        match cmd {
            ACmd::SetFlag { items, .. } => {
                let ids: Vec<MsgId> = items.iter().map(|i| i.id).collect();
                self.store.clear_dirty(&ids)?;
            }
            ACmd::EmptyFolder(fid) => {
                self.store.clear_folder_messages(fid)?;
                self.changed();
            }
            ACmd::CreateFolder { name, parent } => {
                let path = self.child_path(parent, &name)?;
                self.store.upsert_folder(
                    &self.acc.id,
                    &path,
                    name.trim(),
                    Some("/"),
                    FolderRole::Custom,
                    true,
                )?;
                self.emit(Event::FoldersChanged {
                    account_id: self.acc.id.clone(),
                });
            }
            ACmd::RenameFolder { folder_id, name } => {
                let f = self.store.folder(folder_id)?.ok_or_else(|| anyhow!("Folder not found"))?;
                let new_path = match f.parent_path() {
                    Some(p) => format!("{p}/{}", util::encode_imap_utf7(name.trim())),
                    None => util::encode_imap_utf7(name.trim()),
                };
                self.store.rename_folder(folder_id, &new_path, name.trim())?;
                self.emit(Event::FoldersChanged {
                    account_id: self.acc.id.clone(),
                });
            }
            ACmd::DeleteFolder(fid) => {
                self.store.delete_folder(fid)?;
                self.emit(Event::FoldersChanged {
                    account_id: self.acc.id.clone(),
                });
            }
            ACmd::SaveDraft { raw, replace } => {
                let drafts = self
                    .store
                    .folder_by_role(&self.acc.id, FolderRole::Drafts)?
                    .ok_or_else(|| anyhow!("No Drafts folder"))?;
                self.insert_local(drafts.id, &raw, true, true)?;
                if let Some(old) = replace {
                    self.store.delete_messages(&[old.id])?;
                }
                self.emit(Event::DraftSaved);
                self.changed();
            }
            ACmd::Send(outbox_id) => {
                let item = self
                    .store
                    .outbox_item(outbox_id)?
                    .ok_or_else(|| anyhow!("Outbox item missing"))?;
                let sent = self
                    .store
                    .folder_by_role(&self.acc.id, FolderRole::Sent)?
                    .ok_or_else(|| anyhow!("No Sent folder"))?;
                self.insert_local(sent.id, &item.raw, true, false)?;
                // Messages addressed to ourselves arrive in the Inbox.
                if item
                    .recipients
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(&self.acc.email))
                {
                    if let Some(inbox) = self.store.folder_by_role(&self.acc.id, FolderRole::Inbox)? {
                        self.insert_local(inbox.id, &item.raw, false, false)?;
                    }
                }
                if let Some(id) = item.reply_to_msg {
                    self.store.set_bool(&[id], "answered", true)?;
                }
                if let Some(id) = item.forward_of {
                    self.store.set_bool(&[id], "forwarded", true)?;
                }
                if let Some(id) = item.draft_msg {
                    self.store.delete_messages(&[id])?;
                }
                self.store.remove_outbox(outbox_id)?;
                self.emit(Event::Sent {
                    subject: item.subject.clone(),
                });
                self.changed();
            }
            _ => {}
        }
        Ok(())
    }

    fn insert_local(&self, folder_id: FolderId, raw: &[u8], seen: bool, draft: bool) -> Result<MsgId> {
        let p = mime::parse_full(raw).ok_or_else(|| anyhow!("Could not parse message"))?;
        let text = mime::plain_text(&p);
        let from = p.from.first().cloned().unwrap_or_default();
        let uid = self.store.max_uid(folder_id)? + 1;
        let nm = NewMessage {
            account_id: self.acc.id.clone(),
            folder_id,
            uid,
            message_id: p.message_id.clone(),
            in_reply_to: p.in_reply_to.clone(),
            references: p.references.clone(),
            subject: p.subject.clone(),
            from_name: from.name,
            from_addr: from.email,
            to_list: Addr::list_to_string(&p.to),
            cc_list: Addr::list_to_string(&p.cc),
            date: if p.date > 0 { p.date } else { util::now_ts() },
            preview: util::make_preview(&text, 220),
            size: raw.len() as i64,
            seen,
            flagged: false,
            answered: false,
            forwarded: false,
            draft,
            has_attachments: p.attachments.iter().any(|a| !a.inline),
            importance: p.importance,
            focused: true,
            list_unsubscribe: None,
            raw: Some(raw.to_vec()),
            body_text: text,
        };
        Ok(self.store.insert_message(&nm)?.0)
    }
}

#[cfg(test)]
mod live_tests {
    //! End-to-end tests against a real IMAP/SMTP server. Run the local test
    //! server first (`python tools/testserver.py`, see README), then:
    //! `cargo test -- --ignored live_imap`.
    use super::*;
    use crate::config::{Security, ServerConfig};
    use std::time::{Duration, Instant};

    fn wait_for(
        rx: &crossbeam_channel::Receiver<Event>,
        secs: u64,
        what: &str,
        mut pred: impl FnMut(&Event) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                if let Event::Status { status: SyncStatus::Error(e), .. } = &ev {
                    panic!("engine error while waiting for {what}: {e}");
                }
                if pred(&ev) {
                    return;
                }
                seen.push(format!("{ev:?}").chars().take(160).collect::<String>());
            }
        }
        panic!("timed out waiting for {what}; saw: {seen:#?}");
    }

    fn poll(secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    fn idle(ev: &Event) -> bool {
        matches!(ev, Event::Status { status: SyncStatus::Idle { last }, .. } if *last > 0)
    }

    fn folder(store: &Store, acc: &str, role: FolderRole) -> Folder {
        store.folder_by_role(acc, role).unwrap().unwrap_or_else(|| panic!("no {role:?} folder"))
    }

    #[test]
    #[ignore = "needs the local test server (tools/testserver.py)"]
    fn live_imap_smtp_roundtrip() {
        let _ = env_logger::builder().is_test(true).try_init();
        let dir = std::env::temp_dir().join(format!("rustmail-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("mail.db");
        let mut acc = AccountConfig::default();
        acc.email = "alex@example.test".into();
        acc.display_name = "Alex Test".into();
        acc.label = "Test".into();
        acc.imap = ServerConfig {
            host: "127.0.0.1".into(),
            port: 1143,
            security: Security::None,
            username: "alex@example.test".into(),
            accept_invalid_certs: false,
        };
        acc.smtp = ServerConfig {
            host: "127.0.0.1".into(),
            port: 2525,
            security: Security::None,
            username: "alex@example.test".into(),
            accept_invalid_certs: false,
        };
        acc.sync_interval_min = 60;
        secrets::set_session_only(&acc.id, SecretKind::Password, "secret");
        let mut cfg = AppConfig::default();
        cfg.accounts.push(acc.clone());
        let engine = Engine::start(egui::Context::default(), db.clone(), cfg);
        let rx = engine.rx.clone();

        // 1. Account test (IMAP login + SMTP auth).
        engine.send(Command::TestAccount { account: Box::new(acc.clone()), password: String::new(), smtp_password: String::new() });
        wait_for(&rx, 20, "test result", |e| match e {
            Event::TestResult { imap, smtp } => {
                assert!(imap.is_ok(), "imap test failed: {imap:?}");
                assert!(smtp.is_ok(), "smtp test failed: {smtp:?}");
                true
            }
            _ => false,
        });

        // 2. Initial sync happens automatically.
        wait_for(&rx, 30, "initial sync", idle);
        let store = Store::open(&db).unwrap();
        let inbox = folder(&store, &acc.id, FolderRole::Inbox);
        for role in [FolderRole::Sent, FolderRole::Drafts, FolderRole::Trash, FolderRole::Junk, FolderRole::Archive] {
            folder(&store, &acc.id, role);
        }
        assert!(store.folders(&acc.id).unwrap().iter().any(|f| f.path == "PROJECTS/ALPHA"));
        let msgs = store.list_messages(&ListQueryAll::inbox(inbox.id)).unwrap();
        assert_eq!(msgs.len(), 13, "all inbox messages synced");
        assert_eq!(msgs.iter().filter(|m| !m.seen).count(), 12);
        assert!(msgs.iter().any(|m| m.flagged));
        let news = msgs.iter().find(|m| m.subject == "HTML newsletter").unwrap();
        assert!(!news.focused, "list mail goes to Other");
        assert!(news.list_unsubscribe.is_some());
        let with_body = msgs.iter().filter(|m| m.has_body).count();
        assert_eq!(with_body, 13, "bodies prefetched");
        assert!(news.preview.contains("Plain part"));

        // 3. Flag changes reach the server.
        let m3 = msgs.iter().find(|m| m.subject == "Test message 3").unwrap().clone();
        store.set_bool(&[m3.id], "seen", true).unwrap();
        engine.send(Command::SetFlag { account_id: acc.id.clone(), items: vec![MsgRef::of(&m3)], flag: FlagKind::Seen, value: true });
        // 4. Move to Archive (as the UI does: local first, then server).
        let m4 = msgs.iter().find(|m| m.subject == "Test message 4").unwrap().clone();
        let archive = folder(&store, &acc.id, FolderRole::Archive);
        store.move_local(&[m4.id], archive.id, false).unwrap();
        engine.send(Command::Move { account_id: acc.id.clone(), items: vec![MsgRef::of(&m4)], dest: archive.id });
        std::thread::sleep(Duration::from_millis(1500));
        engine.send(Command::SyncAccount { account_id: acc.id.clone(), full: false });
        wait_for(&rx, 20, "sync after move", idle);
        let moved = store.message(m4.id).unwrap().expect("moved message keeps its id");
        assert_eq!(moved.folder_id, archive.id);
        assert!(moved.uid > 0, "placeholder uid replaced by the server uid");
        let m3_after = store.message(m3.id).unwrap().unwrap();
        assert!(m3_after.seen, "seen flag survived the resync (it is on the server)");

        // Verify flags directly on the server.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let creds = Credentials { username: acc.email.clone(), secret: "secret".into(), oauth: false };
            let mut c = imap::connect(&acc.imap, &creds).await.unwrap();
            c.select("INBOX").await.unwrap();
            let f = c.fetch_uid(&m3.uid.to_string(), "(UID FLAGS)").await.unwrap();
            assert!(imap::flags_of(&f[0]).seen, "server has \\Seen");
            c.logout().await;
        });

        // 5. Send a message to ourselves: SMTP delivery + Sent copy.
        let mail = crate::mime::OutgoingMail {
            from: Addr::new("Alex Test", "alex@example.test"),
            to: vec![Addr::new("Alex Test", "alex@example.test")],
            subject: "Loopback ✓".into(),
            text: "Sent by the live test.".into(),
            html: Some(crate::mime::wrap_html_document("<p>Sent by the <b>live</b> test.</p>")),
            ..Default::default()
        };
        let built = crate::mime::build(&mail).unwrap();
        store
            .add_outbox(&OutboxItem {
                id: 0,
                account_id: acc.id.clone(),
                raw: built.raw,
                from: acc.email.clone(),
                recipients: built.recipients,
                send_at: 0,
                attempts: 0,
                last_error: None,
                subject: mail.subject.clone(),
                reply_to_msg: Some(m3.id),
                forward_of: None,
                draft_msg: None,
            })
            .unwrap();
        engine.send(Command::ProcessOutbox);
        wait_for(&rx, 20, "sent", |e| matches!(e, Event::Sent { .. }) || matches!(e, Event::SendFailed { .. }) && panic!("{e:?}"));
        // No manual sync: the delivered copy must show up via IDLE / pre-IDLE check.
        poll(20, "loopback delivered to the inbox", || {
            store
                .list_messages(&ListQueryAll::inbox(inbox.id))
                .unwrap()
                .iter()
                .any(|m| m.subject == "Loopback ✓")
        });
        let sent = folder(&store, &acc.id, FolderRole::Sent);
        let sent_msgs = store.list_messages(&ListQueryAll::inbox(sent.id)).unwrap();
        assert!(sent_msgs.iter().any(|m| m.subject == "Loopback ✓"), "copy saved to Sent");
        let inbox_msgs = store.list_messages(&ListQueryAll::inbox(inbox.id)).unwrap();
        let looped = inbox_msgs.iter().find(|m| m.subject == "Loopback ✓").expect("delivered to inbox");
        assert_eq!(looped.thread_id, sent_msgs.iter().find(|m| m.subject == "Loopback ✓").unwrap().thread_id);
        assert!(store.message(m3.id).unwrap().unwrap().answered, "original marked answered");
        assert!(store.outbox().unwrap().is_empty());

        // 6. Drafts.
        let draft = crate::mime::build(&crate::mime::OutgoingMail {
            from: Addr::new("Alex Test", "alex@example.test"),
            to: vec![Addr::new("", "someone@example.org")],
            subject: "Draft subject".into(),
            text: "draft body".into(),
            keep_bcc: true,
            ..Default::default()
        })
        .unwrap();
        engine.send(Command::SaveDraft { account_id: acc.id.clone(), raw: draft.raw, replace: None });
        wait_for(&rx, 20, "draft saved", |e| matches!(e, Event::DraftSaved));
        std::thread::sleep(Duration::from_millis(800));
        let drafts = folder(&store, &acc.id, FolderRole::Drafts);
        let d = store.list_messages(&ListQueryAll::inbox(drafts.id)).unwrap();
        assert!(d.iter().any(|m| m.subject == "Draft subject" && m.draft));

        // 7. Folder management.
        engine.send(Command::CreateFolder { account_id: acc.id.clone(), name: "Receipts".into(), parent: None });
        wait_for(&rx, 20, "folder created", |e| matches!(e, Event::FoldersChanged { .. }));
        let receipts = store.folder_by_path(&acc.id, "Receipts").unwrap().expect("folder exists locally");
        engine.send(Command::RenameFolder { folder_id: receipts.id, name: "Bills".into() });
        wait_for(&rx, 20, "folder renamed", |e| matches!(e, Event::FoldersChanged { .. }));
        std::thread::sleep(Duration::from_millis(500));
        let bills = store.folders(&acc.id).unwrap().into_iter().find(|f| f.path.eq_ignore_ascii_case("bills")).expect("renamed");
        engine.send(Command::DeleteFolder { folder_id: bills.id });
        poll(20, "folder deleted", || store.folder(bills.id).unwrap().is_none());

        // 8. Delete to Deleted Items, then empty it.
        let m5 = store.list_messages(&ListQueryAll::inbox(inbox.id)).unwrap().into_iter().find(|m| m.subject == "Test message 5").unwrap();
        let trash = folder(&store, &acc.id, FolderRole::Trash);
        store.move_local(&[m5.id], trash.id, false).unwrap();
        engine.send(Command::Delete { account_id: acc.id.clone(), items: vec![MsgRef::of(&m5)], permanent: false });
        poll(20, "message in Deleted Items with a server uid", || {
            store
                .list_messages(&ListQueryAll::inbox(trash.id))
                .unwrap()
                .iter()
                .any(|m| m.subject == "Test message 5" && m.uid > 0)
        });
        engine.send(Command::EmptyFolder { folder_id: trash.id });
        poll(20, "Deleted Items emptied", || store.list_messages(&ListQueryAll::inbox(trash.id)).unwrap().is_empty());
        // The server agrees.
        rt.block_on(async {
            let creds = Credentials { username: acc.email.clone(), secret: "secret".into(), oauth: false };
            let mut c = imap::connect(&acc.imap, &creds).await.unwrap();
            let mb = c.select(&trash.path).await.unwrap();
            assert_eq!(mb.exists, 0, "server trash is empty");
            let mb = c.select("ARCHIVE").await.unwrap();
            assert_eq!(mb.exists, 1, "archived message is on the server");
            c.logout().await;
        });

        // 9. Push: mail delivered by someone else while we sit in IDLE.
        std::thread::sleep(Duration::from_secs(2));
        let pushed = crate::mime::build(&crate::mime::OutgoingMail {
            from: Addr::new("Outside Sender", "outside@example.org"),
            to: vec![Addr::new("", "alex@example.test")],
            subject: "Pushed while idle".into(),
            text: "IDLE should pick this up.".into(),
            ..Default::default()
        })
        .unwrap();
        let t0 = Instant::now();
        rt.block_on(async {
            let creds = Credentials { username: acc.email.clone(), secret: "secret".into(), oauth: false };
            smtp::send(&acc.smtp, &creds, "outside@example.org", &pushed.recipients, &pushed.raw).await.unwrap();
        });
        poll(15, "pushed message", || {
            store
                .list_messages(&ListQueryAll::inbox(inbox.id))
                .unwrap()
                .iter()
                .any(|m| m.subject == "Pushed while idle")
        });
        eprintln!("push latency: {:?}", t0.elapsed());

        engine.send(Command::Shutdown);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct ListQueryAll;
    impl ListQueryAll {
        fn inbox(folder: FolderId) -> crate::store::ListQuery {
            crate::store::ListQuery {
                folders: vec![folder],
                limit: 1000,
                ..Default::default()
            }
        }
    }
}
