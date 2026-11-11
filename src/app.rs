//! Application state, event processing and top-level layout.

use std::collections::{BTreeMap, HashSet};

use egui::{Key, KeyboardShortcut, Modifiers};

use crate::config::{AppConfig, MarkReadMode, Paths};
use crate::engine::{Command, Engine, Event, MsgRef};
use crate::model::*;
use crate::store::Store;
use crate::theme::{self, ThemeMode};
use crate::ui::calendar::CalState;
use crate::ui::compose::Compose;
use crate::ui::mail::MailState;
use crate::ui::people::PeopleState;
use crate::ui::settings::SettingsState;
use crate::ui::todo::TodoState;
use crate::ui::wizard::WizardState;
use crate::util;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Module {
    Mail,
    Calendar,
    People,
    Todo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub enum ToastAction {
    Undo(u64),
    UndoSend(i64),
    OpenAccounts,
    /// Opens the account so the person can sign in with the browser again.
    SignIn(String),
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub kind: ToastKind,
    pub created: f64,
    pub ttl: f64,
    pub action: Option<ToastAction>,
}

/// A server operation delayed so it can be undone from the toast.
pub struct PendingOp {
    pub id: u64,
    pub run_at: f64,
    pub cmd: Command,
    /// (message, original folder, original uid)
    pub restore: Vec<(MsgId, FolderId, i64)>,
}

pub enum Dialog {
    NewFolder { account_id: String, parent: Option<FolderId>, name: String },
    RenameFolder { folder_id: FolderId, name: String },
    DeleteFolder { folder_id: FolderId, name: String },
    EmptyFolder { folder_id: FolderId, name: String },
    MoveTo { ids: Vec<MsgId>, filter: String },
    RemoveAccount { account_id: String, name: String },
    Source { title: String, text: String },
    Shortcuts,
    About,
    NewTaskList { name: String },
}

pub struct StartOptions {
    pub demo: bool,
    pub module: Option<Module>,
    pub theme: Option<ThemeMode>,
    pub open_first: bool,
    pub compose: bool,
    pub settings: bool,
    pub wizard: bool,
    pub open_subject: Option<String>,
    pub search: Option<String>,
    pub month: bool,
    pub contact: Option<String>,
    pub add_account: Option<String>,
    pub google_setup: bool,
    pub settings_accounts: bool,
}

pub struct App {
    pub ctx: egui::Context,
    pub unsent: std::collections::HashMap<i64, Compose>,
    pub cfg: AppConfig,
    pub store: Store,
    pub engine: Engine,
    pub module: Module,
    pub ribbon_tab: usize,
    pub statuses: BTreeMap<String, SyncStatus>,
    /// Calendar sync status per `CalendarSource` id.
    pub cal_statuses: BTreeMap<String, crate::engine::calsync::SourceStatus>,
    /// Latest result of a CalDAV connection test (Settings → Calendar).
    pub caldav_test: Option<Result<Vec<crate::engine::calsync::RemoteCalendar>, String>>,
    pub toasts: Vec<Toast>,
    pub next_id: u64,
    pub pending_ops: Vec<PendingOp>,
    pub mail: MailState,
    pub cal: CalState,
    pub people: PeopleState,
    pub todo: TodoState,
    pub composes: Vec<Compose>,
    pub settings: Option<SettingsState>,
    pub wizard: Option<WizardState>,
    pub dialog: Option<Dialog>,
    /// An import from an Outlook data file, running or just finished.
    pub outlook: Option<crate::ui::outlook_import::OutlookImport>,
    pub search: String,
    pub focus_search: bool,
    pub cfg_dirty_at: Option<f64>,
    pub applied_theme: Option<(bool, theme::Accent, theme::Density)>,
    pub applied_zoom: f32,
    pub font_family: String,
    pub last_reminder_check: i64,
    pub notified: HashSet<(i64, i64)>,
    pub time: f64,
    pub frame: u64,
    pub start: StartOptions,
    pub dark: bool,
    pub maximized: bool,
    /// Dev hook: save a PNG of the window to this path and exit.
    pub screenshot: Option<(String, u64)>,
    pub screenshot_requested: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: StartOptions) -> App {
        let ctx = cc.egui_ctx.clone();
        let font_family = crate::fonts::install(&ctx);
        egui_extras::install_image_loaders(&ctx);

        let (mut cfg, cfg_error) = AppConfig::load();
        let paths = Paths::get();
        let store = Store::open(&paths.db_file()).unwrap_or_else(|e| {
            log::error!("could not open database {:?}: {e}", paths.db_file());
            Store::open(&std::env::temp_dir().join("rustmail-fallback.db"))
                .expect("database")
        });
        if start.demo && !cfg.accounts.iter().any(|a| a.is_demo()) {
            if let Err(e) = crate::demo::create(&store, &mut cfg) {
                log::error!("demo: {e}");
            }
            cfg.onboarded = true;
            let _ = cfg.save();
        }
        util::set_time_24h(cfg.appearance.time_24h);
        util::set_first_day_monday(cfg.calendar.first_day_monday);

        let engine = Engine::start(ctx.clone(), paths.db_file(), cfg.clone());
        let mut app = App {
            ctx: ctx.clone(),
            unsent: std::collections::HashMap::new(),
            mail: MailState::new(&cfg),
            cal: CalState::new(),
            people: PeopleState::default(),
            todo: TodoState::default(),
            cfg,
            store,
            engine,
            module: start.module.unwrap_or(Module::Mail),
            ribbon_tab: 0,
            statuses: BTreeMap::new(),
            cal_statuses: BTreeMap::new(),
            caldav_test: None,
            toasts: Vec::new(),
            next_id: 1,
            pending_ops: Vec::new(),
            composes: Vec::new(),
            settings: None,
            wizard: None,
            dialog: None,
            outlook: None,
            search: String::new(),
            focus_search: false,
            cfg_dirty_at: None,
            applied_theme: None,
            applied_zoom: 0.0,
            font_family,
            last_reminder_check: util::now_ts(),
            notified: HashSet::new(),
            time: 0.0,
            frame: 0,
            start,
            dark: false,
            maximized: false,
            screenshot: std::env::var("RUSTMAIL_SCREENSHOT").ok().map(|p| {
                let frames = std::env::var("RUSTMAIL_SCREENSHOT_FRAMES")
                    .ok()
                    .and_then(|f| f.parse().ok())
                    .unwrap_or(12);
                (p, frames)
            }),
            screenshot_requested: false,
        };
        if let Some(e) = cfg_error {
            app.toast(e, ToastKind::Error);
        }
        if app.cfg.accounts.is_empty() || app.start.wizard {
            app.wizard = Some(WizardState::new(app.cfg.accounts.is_empty()));
        }
        if let Some(email) = app.start.add_account.clone().filter(|e| crate::util::is_valid_email(e)) {
            let mut w = WizardState::new(false);
            w.email = email.trim().to_string();
            w.step = crate::ui::wizard::Step::Discovering;
            app.engine.send(crate::engine::Command::Discover { email: w.email.clone() });
            app.wizard = Some(w);
        }
        if app.start.settings {
            app.settings = Some(if app.start.settings_accounts { SettingsState::accounts() } else { SettingsState::default() });
        }
        if app.start.google_setup {
            let mut st = SettingsState::accounts();
            st.google_setup = Some(crate::ui::oauth_setup::GoogleSetup::new());
            app.settings = Some(st);
        }
        if crate::engine::calsync::ensure_auto_sources(&app.cfg.accounts, &mut app.cfg.calendar_sources) {
            app.save_config_now();
        }
        app.mail.select_default_folder(&app.store, &app.cfg);
        app
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) -> u64 {
        self.toast_with(text, kind, None)
    }

    pub fn toast_with(&mut self, text: impl Into<String>, kind: ToastKind, action: Option<ToastAction>) -> u64 {
        let id = self.next_id();
        let text = text.into();
        // Avoid stacking identical errors.
        self.toasts.retain(|t| t.text != text);
        let ttl = match (&action, kind) {
            (Some(ToastAction::Undo(_)), _) | (Some(ToastAction::UndoSend(_)), _) => 6.0,
            (_, ToastKind::Error) => 9.0,
            _ => 4.0,
        };
        self.toasts.push(Toast {
            id,
            text,
            kind,
            created: self.time,
            ttl,
            action,
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        id
    }

    pub fn mark_config_dirty(&mut self) {
        self.cfg_dirty_at = Some(self.time);
    }

    pub fn save_config_now(&mut self) {
        self.cfg_dirty_at = None;
        // Calendar sources follow the mail accounts (Google, known CalDAV providers).
        crate::engine::calsync::ensure_auto_sources(&self.cfg.accounts, &mut self.cfg.calendar_sources);
        self.prune_calendars();
        util::set_time_24h(self.cfg.appearance.time_24h);
        util::set_first_day_monday(self.cfg.calendar.first_day_monday);
        if let Err(e) = self.cfg.save() {
            self.toast(format!("Could not save settings: {e}"), ToastKind::Error);
        }
        self.engine
            .send(Command::ConfigChanged(Box::new(self.cfg.clone())));
    }

    /// Drops synced calendars whose source is gone or turned off.
    pub fn prune_calendars(&mut self) {
        let active: Vec<String> = self.cfg.calendar_sources.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
        let mut removed = false;
        for c in self.store.calendars().unwrap_or_default() {
            if !c.source.is_empty() && !active.contains(&c.source) {
                let _ = self.store.delete_calendar(c.id);
                removed = true;
            }
        }
        if removed {
            self.cal.dirty = true;
        }
        if self.store.calendar(self.cfg.calendar.default_calendar).ok().flatten().is_none() {
            self.cfg.calendar.default_calendar = crate::model::LOCAL_CALENDAR;
        }
    }

    pub fn account_name(&self, id: &str) -> String {
        self.cfg
            .account(id)
            .map(|a| a.name().to_string())
            .unwrap_or_default()
    }

    pub fn my_addresses(&self) -> Vec<String> {
        self.cfg.accounts.iter().map(|a| a.email.to_lowercase()).collect()
    }

    // ------------------------------------------------------------ pending ops

    /// Queues an undoable server command. `restore` lets Undo put the
    /// messages back locally.
    pub fn defer(&mut self, cmd: Command, restore: Vec<(MsgId, FolderId, i64)>, text: String) {
        let id = self.next_id();
        self.pending_ops.push(PendingOp {
            id,
            run_at: self.time + 6.0,
            cmd,
            restore,
        });
        self.toast_with(text, ToastKind::Info, Some(ToastAction::Undo(id)));
    }

    pub fn undo(&mut self, op_id: u64) {
        if let Some(pos) = self.pending_ops.iter().position(|o| o.id == op_id) {
            let op = self.pending_ops.remove(pos);
            for (id, folder, uid) in &op.restore {
                let _ = self.store.conn.execute(
                    "UPDATE messages SET folder_id = ?2, uid = ?3, pending = NULL, orig_folder = NULL, \
                     orig_uid = NULL, snoozed_until = CASE WHEN snoozed_until = ?4 THEN NULL ELSE snoozed_until END \
                     WHERE id = ?1",
                    rusqlite::params![id, folder, uid, crate::store::HIDDEN_UNTIL],
                );
            }
            self.mail.list_dirty = true;
            self.mail.folders_dirty = true;
            self.toast("Undone", ToastKind::Info);
        }
    }

    pub fn flush_pending(&mut self) {
        for op in std::mem::take(&mut self.pending_ops) {
            self.engine.send(op.cmd);
        }
    }

    // ----------------------------------------------------------------- events

    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.engine.rx.try_recv() {
            match ev {
                Event::Status { account_id, status } => {
                    if let SyncStatus::Error(e) = &status {
                        let changed = !matches!(self.statuses.get(&account_id), Some(SyncStatus::Error(old)) if old == e);
                        if changed {
                            let name = self.account_name(&account_id);
                            let action = if e.contains(crate::engine::oauth::SIGN_IN_AGAIN) {
                                ToastAction::SignIn(account_id.clone())
                            } else {
                                ToastAction::OpenAccounts
                            };
                            self.toast_with(format!("{name}: {e}"), ToastKind::Error, Some(action));
                        }
                    }
                    self.statuses.insert(account_id, status);
                }
                Event::FoldersChanged { .. } => {
                    self.mail.folders_dirty = true;
                    self.mail.list_dirty = true;
                }
                Event::MessagesChanged { .. } => {
                    self.mail.list_dirty = true;
                    self.mail.folders_dirty = true;
                    self.mail.reading_flags_dirty = true;
                    self.todo.dirty = true;
                }
                Event::BodyReady { msg_id } => self.body_ready(ctx, msg_id),
                Event::BodyFailed { msg_id, error } => {
                    if let Some(r) = self.mail.reading.as_mut() {
                        for it in r.items.iter_mut().filter(|i| i.summary.id == msg_id) {
                            it.loading = false;
                            it.error = Some(error.clone());
                        }
                    }
                }
                Event::NewMail { count, samples, .. } => {
                    let text = if count == 1 {
                        samples
                            .first()
                            .map(|(s, sub)| format!("New message from {s}: {sub}"))
                            .unwrap_or_else(|| "1 new message".into())
                    } else {
                        format!("{count} new messages")
                    };
                    self.toast(text, ToastKind::Info);
                }
                Event::Sent { subject } => {
                    self.toast(format!("Sent: {}", util::truncate_chars(&subject, 60)), ToastKind::Success);
                }
                Event::SendFailed { subject, error } => {
                    self.toast(
                        format!("Couldn't send \"{}\": {error}. It stays in the Outbox and will be retried.", util::truncate_chars(&subject, 40)),
                        ToastKind::Error,
                    );
                }
                Event::DraftSaved => {
                    self.toast("Draft saved", ToastKind::Success);
                }
                Event::Toast { text, error } => {
                    self.toast(text, if error { ToastKind::Error } else { ToastKind::Info });
                }
                Event::TestResult { imap, smtp } => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.on_test_result(imap.clone(), smtp.clone());
                    }
                    if let Some(s) = self.settings.as_mut() {
                        s.on_test_result(imap, smtp);
                    }
                }
                Event::Discovered { email, result } => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.on_discovered(&email, result);
                    }
                }
                Event::CalendarStatus { source_id, status } => {
                    self.cal_statuses.insert(source_id, status);
                }
                Event::CalendarsChanged => {
                    self.cal.dirty = true;
                }
                Event::CalDavTestResult { result } => {
                    self.caldav_test = Some(result);
                }
                Event::OAuthUrl { account_id, url } => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.on_oauth_url(&account_id, &url);
                    }
                    if let Some(s) = self.settings.as_mut() {
                        s.on_oauth_url(&account_id, &url);
                    }
                }
                Event::OAuthResult { account_id, result } => {
                    if let Some(w) = self.wizard.as_mut() {
                        w.on_oauth(&account_id, &result);
                    }
                    if let Some(s) = self.settings.as_mut() {
                        s.on_oauth(&account_id, &result);
                    }
                    match result {
                        Ok(oauth) => {
                            // The new token only works with the key it was issued for,
                            // so an existing account switches to that key right away.
                            let mut changed = false;
                            if let Some(acc) = self.cfg.account_mut(&account_id) {
                                acc.oauth = (*oauth).clone();
                                acc.auth = crate::config::AuthKind::OAuth2;
                                changed = true;
                            }
                            // Remember a Microsoft app key once it has worked.
                            if oauth.provider == "microsoft" && crate::engine::oauth::builtin_key("microsoft").is_none() {
                                let ms = &mut self.cfg.oauth_apps.microsoft;
                                if ms.client_id != oauth.client_id {
                                    *ms = crate::config::OAuthClientKey {
                                        client_id: oauth.client_id.clone(),
                                        source: "entered by you".into(),
                                        ..Default::default()
                                    };
                                    changed = true;
                                }
                            }
                            if changed {
                                self.save_config_now();
                            }
                            let adding = self.wizard.as_ref().is_some_and(|w| w.editor.acc.id == account_id);
                            if self.cfg.account(&account_id).is_none() && !adding {
                                // Removed or abandoned while the browser was open.
                                crate::secrets::delete_all(&account_id);
                            }
                            if self.cfg.account(&account_id).is_some() {
                                self.toast("Signed in", ToastKind::Success);
                                self.engine.send(Command::ResetSignIn { account_id: account_id.clone() });
                                self.engine.send(Command::SyncAccount { account_id: account_id.clone(), full: false });
                                // The new sign-in may have added calendar access.
                                for s in self.cfg.calendar_sources.iter().filter(|s| s.account_id == account_id) {
                                    self.cal_statuses.remove(&s.id);
                                    self.engine.send(Command::SyncCalendars { source_id: Some(s.id.clone()) });
                                }
                            }
                        }
                        Err(e) => {
                            // The wizard and the account editor show the error themselves.
                            let shown = self.wizard.as_ref().is_some_and(|w| w.editor.acc.id == account_id)
                                || self.settings.as_ref().and_then(|s| s.account.as_ref()).is_some_and(|ed| ed.acc.id == account_id);
                            if !shown {
                                self.toast(format!("Sign-in failed: {e}"), ToastKind::Error);
                            }
                        }
                    }
                }
            }
        }
    }

    fn timers(&mut self, ctx: &egui::Context) {
        let now = self.time;
        // Deferred (undoable) operations.
        let due: Vec<usize> = self
            .pending_ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.run_at <= now)
            .map(|(i, _)| i)
            .collect();
        for i in due.into_iter().rev() {
            let op = self.pending_ops.remove(i);
            self.engine.send(op.cmd);
        }
        self.toasts.retain(|t| now - t.created < t.ttl);

        // Mark as read after a delay.
        if let Some((id, at)) = self.mail.mark_read_at {
            if now >= at {
                self.mail.mark_read_at = None;
                if self.mail.reading.as_ref().map(|r| r.msg_id) == Some(id) {
                    self.set_seen(&[id], true);
                }
            }
        }

        if let Some(t) = self.cfg_dirty_at {
            if now - t > 0.8 {
                self.save_config_now();
            }
        }

        // Reminders every 15 seconds.
        let wall = util::now_ts();
        if wall - self.last_reminder_check >= 15 {
            self.check_reminders(wall);
            self.last_reminder_check = wall;
            // Snoozed messages may have come back.
            if self.store.snoozed_count().unwrap_or(0) > 0 {
                self.mail.list_dirty = true;
                self.mail.folders_dirty = true;
            }
        }

        if !self.pending_ops.is_empty() || !self.toasts.is_empty() || self.mail.mark_read_at.is_some() || self.cfg_dirty_at.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
    }

    fn check_reminders(&mut self, now: i64) {
        if !self.cfg.notifications.calendar_reminders {
            return;
        }
        let from = self.last_reminder_check;
        let events = self.store.events_between(now - 86400, now + 2 * 86400).unwrap_or_default();
        for occ in crate::cal::occurrences(&events, now - 3600, now + 2 * 86400) {
            let Some(min) = occ.event.reminder_min else { continue };
            let at = occ.start - min * 60;
            if at > from && at <= now && self.notified.insert((occ.event.id, occ.start)) {
                let when = if occ.event.all_day {
                    "All day".to_string()
                } else {
                    format!("{} – {}", util::fmt_time(&util::local(occ.start)), util::fmt_time(&util::local(occ.end)))
                };
                let body = if occ.event.location.is_empty() {
                    when.clone()
                } else {
                    format!("{when} · {}", occ.event.location)
                };
                crate::notify::desktop(occ.event.title.clone(), body.clone());
                self.toast(format!("Reminder: {} ({body})", occ.event.title), ToastKind::Info);
            }
        }
        for t in self.store.tasks().unwrap_or_default() {
            if let Some(r) = t.reminder {
                if !t.completed && r > from && r <= now && self.notified.insert((-t.id, r)) {
                    crate::notify::desktop("Task reminder".into(), t.title.clone());
                    self.toast(format!("Task reminder: {}", t.title), ToastKind::Info);
                }
            }
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let dark = match self.start.theme.unwrap_or(self.cfg.appearance.theme) {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(false),
        };
        let key = (dark, self.cfg.appearance.accent, self.cfg.appearance.density);
        if self.applied_theme != Some(key) {
            theme::apply(ctx, dark, self.cfg.appearance.accent, self.cfg.appearance.density);
            self.applied_theme = Some(key);
            self.dark = dark;
        }
        let zoom = self.cfg.appearance.zoom.clamp(0.7, 2.0);
        if (self.applied_zoom - zoom).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
            self.applied_zoom = zoom;
        } else if (ctx.zoom_factor() - self.applied_zoom).abs() > 0.001 {
            // Changed with Ctrl +/-.
            self.cfg.appearance.zoom = ctx.zoom_factor();
            self.applied_zoom = ctx.zoom_factor();
            self.mark_config_dirty();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let cmd = |k: Key| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k: Key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

        if pressed(cmd(Key::Num1)) {
            self.module = Module::Mail;
        }
        if pressed(cmd(Key::Num2)) {
            self.module = Module::Calendar;
        }
        if pressed(cmd(Key::Num3)) {
            self.module = Module::People;
        }
        if pressed(cmd(Key::Num4)) {
            self.module = Module::Todo;
        }
        if pressed(cmd(Key::E)) || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F3)) {
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F9)) {
            self.engine.send(Command::SyncAll { full: true });
            self.toast("Checking all folders for new mail…", ToastKind::Info);
        }
        if pressed(cmd(Key::Comma)) {
            self.settings = Some(SettingsState::default());
        }
        if pressed(cmd(Key::N)) {
            match self.module {
                Module::Mail => self.new_compose(),
                Module::Calendar => self.cal_new_event(None),
                Module::People => self.people_new_contact(),
                Module::Todo => self.todo.focus_new = true,
            }
        }
        if self.module != Module::Mail || self.dialog.is_some() || self.settings.is_some() || self.wizard.is_some() {
            return;
        }
        if self.mail.compose_inline.is_some() {
            return;
        }
        let sel = self.mail.selection.clone();
        if pressed(cmd(Key::R)) && !sel.is_empty() {
            self.reply(sel[0], false);
        }
        if pressed(cmd_shift(Key::R)) && !sel.is_empty() {
            self.reply(sel[0], true);
        }
        if pressed(cmd(Key::F)) && !sel.is_empty() {
            self.forward(sel[0]);
        }
        if pressed(cmd(Key::Q)) && !sel.is_empty() {
            self.set_seen(&sel, true);
        }
        if pressed(cmd(Key::U)) && !sel.is_empty() {
            self.set_seen(&sel, false);
        }
        if pressed(cmd_shift(Key::V)) && !sel.is_empty() {
            self.dialog = Some(Dialog::MoveTo {
                ids: sel.clone(),
                filter: String::new(),
            });
        }
        if typing {
            return;
        }
        if pressed(cmd(Key::A)) {
            self.mail.selection = self.mail.visible_ids();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Delete)) && !sel.is_empty() {
            self.delete_messages(&sel, true);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) && !sel.is_empty() {
            self.delete_messages(&sel, false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Insert)) && !sel.is_empty() {
            self.toggle_flag(&sel);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace) || i.consume_key(Modifiers::NONE, Key::E)) && !sel.is_empty() {
            self.archive_messages(&sel);
        }
        let down = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown) || i.consume_key(Modifiers::NONE, Key::J));
        let up = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp) || i.consume_key(Modifiers::NONE, Key::K));
        if down || up {
            self.move_selection(if down { 1 } else { -1 });
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if !self.search.is_empty() {
                self.search.clear();
                self.mail.list_dirty = true;
            } else if self.mail.full_view {
                self.mail.full_view = false;
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && !sel.is_empty() {
            self.open_message_full(sel[0]);
        }
    }

    pub fn set_seen(&mut self, ids: &[MsgId], value: bool) {
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        let changed: Vec<&MessageSummary> = msgs.iter().filter(|m| m.seen != value).collect();
        if changed.is_empty() {
            return;
        }
        let change_ids: Vec<MsgId> = changed.iter().map(|m| m.id).collect();
        let _ = self.store.set_bool(&change_ids, "seen", value);
        let mut by_acc: BTreeMap<String, Vec<MsgRef>> = BTreeMap::new();
        for m in changed {
            by_acc.entry(m.account_id.clone()).or_default().push(MsgRef::of(m));
        }
        for (account_id, items) in by_acc {
            self.engine.send(Command::SetFlag {
                account_id,
                items,
                flag: crate::engine::FlagKind::Seen,
                value,
            });
        }
        self.mail.list_dirty = true;
        self.mail.folders_dirty = true;
        self.mail.reading_flags_dirty = true;
    }

    pub fn on_message_opened(&mut self, m: &MessageSummary) {
        if m.seen {
            return;
        }
        match self.cfg.mail.mark_read {
            MarkReadMode::OnSelect => self.set_seen(&[m.id], true),
            MarkReadMode::AfterDelay => {
                self.mail.mark_read_at = Some((m.id, self.time + self.cfg.mail.mark_read_delay_s as f64));
            }
            MarkReadMode::Never => {}
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.time = ctx.input(|i| i.time);
        self.frame += 1;
        self.apply_theme(ctx);
        self.process_events(ctx);
        self.timers(ctx);
        self.maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if ctx.input(|i| i.viewport().close_requested()) {
            self.flush_pending();
            self.persist_window(ctx);
            self.save_config_now();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.frame <= 2 {
            self.startup_actions(&ctx);
        }
        self.shortcuts(&ctx);
        self.draw(ui);
        self.screenshot_hook(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_pending();
        if self.cfg_dirty_at.is_some() {
            let _ = self.cfg.save();
        }
        // Give queued server operations a moment to complete.
        self.engine.shutdown(std::time::Duration::from_secs(10));
    }
}

impl App {
    fn startup_actions(&mut self, _ctx: &egui::Context) {
        if self.frame == 1 {
            if self.start.open_first {
                self.mail.list_dirty = true;
                self.mail.reload(&self.store, &self.cfg, &self.search);
                if let Some(id) = self.mail.first_message_id() {
                    self.select_message(id, Modifiers::NONE);
                }
            }
            if let Some(subject) = self.start.open_subject.clone() {
                let hit = self
                    .store
                    .list_messages(&crate::store::ListQuery { limit: 5000, ..Default::default() })
                    .unwrap_or_default()
                    .into_iter()
                    .find(|m| m.subject.contains(&subject));
                if let Some(m) = hit {
                    self.open_in_mail(&m);
                }
            }
            if let Some(q) = self.start.search.clone() {
                self.search = q;
                self.mail.list_dirty = true;
            }
            if self.start.month {
                self.cal.view = crate::ui::calendar::CalView::Month;
            }
            if let Some(name) = self.start.contact.clone() {
                let hit = self.store.contacts().unwrap_or_default().into_iter().find(|c| c.name.contains(&name));
                if let Some(c) = hit {
                    self.people.selected = Some(c.id);
                }
            }
            if self.start.compose {
                self.new_compose();
                if let Some(c) = self.mail.compose_inline.as_mut() {
                    c.to = vec![Addr::new("Priya Shah", "priya.shah@example.com")];
                    c.subject = "Notes from today's sync".into();
                    c.body = "Hi Priya,\n\nThanks for the **great** discussion today. Quick recap:\n\n- Ship the onboarding flow next sprint\n- Review the *Q4 roadmap* on Thursday\n\nBest,\nAlex".into();
                }
            }
        }
    }

    fn screenshot_hook(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = self.screenshot.clone() else { return };
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
            if let Err(e) = image::save_buffer(&path, &bytes, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgba8) {
                eprintln!("screenshot failed: {e}");
            }
            self.screenshot = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let delay: f64 = std::env::var("RUSTMAIL_SCREENSHOT_DELAY")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(1.2);
        if self.frame >= frames && self.time >= delay && !self.screenshot_requested {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }

    fn persist_window(&mut self, ctx: &egui::Context) {
        let info = ctx.input(|i| i.viewport().clone());
        if let Some(r) = info.inner_rect {
            if !info.maximized.unwrap_or(false) {
                self.cfg.window.width = r.width();
                self.cfg.window.height = r.height();
            }
        }
        self.cfg.window.maximized = info.maximized.unwrap_or(false);
    }
}
