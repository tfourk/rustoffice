//! User configuration, persisted as TOML in the platform config directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::{Accent, Density, ThemeMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Security {
    #[default]
    Tls,
    StartTls,
    None,
}

impl Security {
    pub const ALL: [Security; 3] = [Security::Tls, Security::StartTls, Security::None];
    pub fn label(self) -> &'static str {
        match self {
            Security::Tls => "SSL/TLS",
            Security::StartTls => "STARTTLS",
            Security::None => "None (insecure)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub accept_invalid_certs: bool,
}

impl ServerConfig {
    pub fn summary(&self) -> String {
        format!("{}:{} ({})", self.host, self.port, self.security.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthKind {
    #[default]
    Password,
    OAuth2,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OAuthConfig {
    pub provider: String,
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: String,
    /// Permissions the person actually granted at the last sign-in (the
    /// token response's `scope`); empty when unknown (older sign-ins).
    pub granted: String,
}

/// An app key ("OAuth client") registered with Google Cloud or Microsoft
/// Entra. Every account of that provider signs in with it, so it only has
/// to be set up once per computer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthClientKey {
    pub client_id: String,
    pub client_secret: String,
    /// Google Cloud project the key belongs to (used for links into the console).
    pub project_id: String,
    /// Where the key came from, shown in Settings (e.g. the downloaded file's name).
    pub source: String,
}

impl OAuthClientKey {
    pub fn is_set(&self) -> bool {
        !self.client_id.trim().is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthApps {
    pub google: OAuthClientKey,
    pub microsoft: OAuthClientKey,
}

impl OAuthApps {
    pub fn get(&self, provider: &str) -> Option<&OAuthClientKey> {
        match provider {
            "google" => Some(&self.google),
            "microsoft" => Some(&self.microsoft),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, provider: &str) -> Option<&mut OAuthClientKey> {
        match provider {
            "google" => Some(&mut self.google),
            "microsoft" => Some(&mut self.microsoft),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CalendarSourceKind {
    /// Google Calendar API, signed in through a Google mail account.
    #[default]
    Google,
    /// Any CalDAV server (iCloud, Fastmail, Nextcloud, Yahoo, …).
    CalDav,
}

/// Where synced calendars come from. Each source has any number of
/// calendars (see `model::Calendar::source`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarSource {
    pub id: String,
    pub label: String,
    pub kind: CalendarSourceKind,
    /// Google: the mail account whose sign-in is used. CalDAV: the mail
    /// account whose password is reused (empty: own password in the keychain).
    pub account_id: String,
    /// CalDAV: server address or calendar-home URL; discovery starts here.
    pub url: String,
    /// CalDAV user name (empty: the mail account's user name).
    pub username: String,
    pub enabled: bool,
}

impl Default for CalendarSource {
    fn default() -> Self {
        CalendarSource {
            id: uuid::Uuid::new_v4().to_string(),
            label: String::new(),
            kind: CalendarSourceKind::Google,
            account_id: String::new(),
            url: String::new(),
            username: String::new(),
            enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AccountKind {
    #[default]
    Imap,
    /// Offline account stored only on this computer (used by the demo mailbox).
    Local,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    pub id: String,
    pub kind: AccountKind,
    pub label: String,
    pub display_name: String,
    pub email: String,
    pub provider: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    /// Use the IMAP password for SMTP as well.
    pub smtp_same_password: bool,
    pub auth: AuthKind,
    pub oauth: OAuthConfig,
    pub signature: String,
    pub signature_on_reply: bool,
    pub sync_interval_min: u32,
    pub initial_sync_count: u32,
    pub prefetch_bodies: bool,
    pub save_sent_copy: bool,
    /// Use IMAP IDLE to receive new mail instantly.
    pub push: bool,
    pub enabled: bool,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            id: uuid::Uuid::new_v4().to_string(),
            kind: AccountKind::Imap,
            label: String::new(),
            display_name: String::new(),
            email: String::new(),
            provider: String::new(),
            imap: ServerConfig {
                port: 993,
                ..Default::default()
            },
            smtp: ServerConfig {
                port: 587,
                security: Security::StartTls,
                ..Default::default()
            },
            smtp_same_password: true,
            auth: AuthKind::Password,
            oauth: OAuthConfig::default(),
            signature: String::new(),
            signature_on_reply: true,
            sync_interval_min: 5,
            initial_sync_count: 1000,
            prefetch_bodies: true,
            save_sent_copy: true,
            push: true,
            enabled: true,
        }
    }
}

impl AccountConfig {
    pub fn name(&self) -> &str {
        if !self.label.trim().is_empty() {
            self.label.trim()
        } else {
            &self.email
        }
    }
    pub fn is_local(&self) -> bool {
        self.kind == AccountKind::Local
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReadingPanePos {
    #[default]
    Right,
    Bottom,
    Off,
}

impl ReadingPanePos {
    pub const ALL: [ReadingPanePos; 3] = [ReadingPanePos::Right, ReadingPanePos::Bottom, ReadingPanePos::Off];
    pub fn label(self) -> &'static str {
        match self {
            ReadingPanePos::Right => "Show on the right",
            ReadingPanePos::Bottom => "Show on the bottom",
            ReadingPanePos::Off => "Hidden (open messages full view)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MarkReadMode {
    #[default]
    OnSelect,
    AfterDelay,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImagePolicy {
    #[default]
    Block,
    Contacts,
    Always,
}

impl ImagePolicy {
    pub const ALL: [ImagePolicy; 3] = [ImagePolicy::Block, ImagePolicy::Contacts, ImagePolicy::Always];
    pub fn label(self) -> &'static str {
        match self {
            ImagePolicy::Block => "Always block external images",
            ImagePolicy::Contacts => "Show images from contacts and safe senders",
            ImagePolicy::Always => "Always show external images",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: ThemeMode,
    pub accent: Accent,
    pub density: Density,
    pub zoom: f32,
    pub custom_titlebar: bool,
    pub time_24h: bool,
    /// One compact toolbar with the everyday commands instead of the
    /// tabbed ribbon; the rest lives in its "More" menu.
    pub slim: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings {
            theme: ThemeMode::System,
            accent: Accent::Blue,
            density: Density::Cozy,
            zoom: 1.0,
            custom_titlebar: true,
            time_24h: false,
            slim: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MailSettings {
    pub focused_inbox: bool,
    pub reading_pane: ReadingPanePos,
    pub conversation_view: bool,
    pub show_preview: bool,
    pub show_avatars: bool,
    pub group_by_date: bool,
    pub mark_read: MarkReadMode,
    pub mark_read_delay_s: u32,
    pub images: ImagePolicy,
    pub safe_senders: Vec<String>,
    pub blocked_senders: Vec<String>,
    /// sender address -> true (Focused) / false (Other)
    pub focused_overrides: BTreeMap<String, bool>,
    pub confirm_delete: bool,
    pub unified_inbox: bool,
    pub collect_contacts: bool,
}

impl Default for MailSettings {
    fn default() -> Self {
        MailSettings {
            focused_inbox: true,
            reading_pane: ReadingPanePos::Right,
            conversation_view: true,
            show_preview: true,
            show_avatars: true,
            group_by_date: true,
            mark_read: MarkReadMode::OnSelect,
            mark_read_delay_s: 3,
            images: ImagePolicy::Block,
            safe_senders: Vec::new(),
            blocked_senders: Vec::new(),
            focused_overrides: BTreeMap::new(),
            confirm_delete: false,
            unified_inbox: true,
            collect_contacts: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposeSettings {
    pub html: bool,
    pub default_reply_all: bool,
    pub undo_send_s: u32,
    pub read_receipt: bool,
    pub pop_out: bool,
}

impl Default for ComposeSettings {
    fn default() -> Self {
        ComposeSettings {
            html: true,
            default_reply_all: false,
            undo_send_s: 5,
            read_receipt: false,
            pop_out: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarSettings {
    pub first_day_monday: bool,
    pub work_start: u32,
    pub work_end: u32,
    pub default_reminder_min: i64,
    pub default_duration_min: i64,
    pub show_week_numbers: bool,
    /// Minutes between automatic calendar syncs.
    pub sync_interval_min: u32,
    /// Calendar new events go into (a `Calendar` id).
    pub default_calendar: i64,
}

impl Default for CalendarSettings {
    fn default() -> Self {
        CalendarSettings {
            first_day_monday: false,
            work_start: 8,
            work_end: 17,
            default_reminder_min: 15,
            default_duration_min: 30,
            show_week_numbers: false,
            sync_interval_min: 10,
            default_calendar: crate::model::LOCAL_CALENDAR,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub show_preview: bool,
    pub calendar_reminders: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        NotificationSettings {
            enabled: true,
            show_preview: true,
            calendar_reminders: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoryDef {
    pub name: String,
    pub color: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuleField {
    #[default]
    From,
    To,
    Subject,
    Body,
    HasAttachment,
}

impl RuleField {
    pub const ALL: [RuleField; 5] = [
        RuleField::From,
        RuleField::To,
        RuleField::Subject,
        RuleField::Body,
        RuleField::HasAttachment,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RuleField::From => "From",
            RuleField::To => "To or Cc",
            RuleField::Subject => "Subject",
            RuleField::Body => "Message body",
            RuleField::HasAttachment => "Has attachment",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuleOp {
    #[default]
    Contains,
    NotContains,
    Is,
    StartsWith,
    EndsWith,
}

impl RuleOp {
    pub const ALL: [RuleOp; 5] = [
        RuleOp::Contains,
        RuleOp::NotContains,
        RuleOp::Is,
        RuleOp::StartsWith,
        RuleOp::EndsWith,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RuleOp::Contains => "contains",
            RuleOp::NotContains => "does not contain",
            RuleOp::Is => "is",
            RuleOp::StartsWith => "starts with",
            RuleOp::EndsWith => "ends with",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuleCondition {
    pub field: RuleField,
    pub op: RuleOp,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RuleAction {
    MoveTo(String),
    MarkRead,
    Flag,
    Pin,
    Categorize(String),
    Delete,
    MoveToOther,
}

impl RuleAction {
    pub fn label(&self) -> String {
        match self {
            RuleAction::MoveTo(p) => format!("Move to \"{}\"", crate::util::decode_imap_utf7(p)),
            RuleAction::MarkRead => "Mark as read".into(),
            RuleAction::Flag => "Flag".into(),
            RuleAction::Pin => "Pin".into(),
            RuleAction::Categorize(c) => format!("Categorize as \"{c}\""),
            RuleAction::Delete => "Delete".into(),
            RuleAction::MoveToOther => "Move to Other inbox".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Empty = all accounts.
    pub account_id: String,
    pub match_all: bool,
    pub conditions: Vec<RuleCondition>,
    pub actions: Vec<RuleAction>,
    pub stop_processing: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New rule".into(),
            enabled: true,
            account_id: String::new(),
            match_all: true,
            conditions: vec![RuleCondition::default()],
            actions: vec![RuleAction::MarkRead],
            stop_processing: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteRef {
    pub account_id: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    pub folder_width: f32,
    pub list_width: f32,
    pub list_height: f32,
    pub folder_pane_open: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState {
            width: 1440.0,
            height: 900.0,
            maximized: false,
            folder_width: 250.0,
            list_width: 380.0,
            list_height: 320.0,
            folder_pane_open: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub accounts: Vec<AccountConfig>,
    pub default_account: String,
    pub appearance: AppearanceSettings,
    pub mail: MailSettings,
    pub compose: ComposeSettings,
    pub calendar: CalendarSettings,
    pub notifications: NotificationSettings,
    pub categories: Vec<CategoryDef>,
    pub rules: Vec<Rule>,
    pub favorites: Vec<FavoriteRef>,
    pub window: WindowState,
    pub onboarded: bool,
    /// Shared Google / Microsoft sign-in keys.
    pub oauth_apps: OAuthApps,
    /// Keys from older per-account settings were copied into `oauth_apps`.
    pub oauth_keys_adopted: bool,
    pub calendar_sources: Vec<CalendarSource>,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            accounts: Vec::new(),
            default_account: String::new(),
            appearance: AppearanceSettings::default(),
            mail: MailSettings::default(),
            compose: ComposeSettings::default(),
            calendar: CalendarSettings::default(),
            notifications: NotificationSettings::default(),
            categories: default_categories(),
            rules: Vec::new(),
            favorites: Vec::new(),
            window: WindowState::default(),
            onboarded: false,
            oauth_apps: OAuthApps::default(),
            oauth_keys_adopted: false,
            calendar_sources: Vec::new(),
        }
    }
}

pub fn default_categories() -> Vec<CategoryDef> {
    [
        ("Important", "Red"),
        ("Work", "Blue"),
        ("Personal", "Green"),
        ("Follow up", "Orange"),
        ("Travel", "Purple"),
        ("Finance", "Yellow"),
    ]
    .iter()
    .map(|(n, c)| CategoryDef {
        name: n.to_string(),
        color: c.to_string(),
    })
    .collect()
}

pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn get() -> Paths {
        if let Ok(home) = std::env::var("RUSTMAIL_HOME") {
            let p = PathBuf::from(home);
            return Paths {
                config_dir: p.clone(),
                data_dir: p,
            };
        }
        match directories::ProjectDirs::from("org", "RustMail", "RustMail") {
            Some(d) => Paths {
                config_dir: d.config_dir().to_path_buf(),
                data_dir: d.data_dir().to_path_buf(),
            },
            None => {
                let p = PathBuf::from(".rustmail");
                Paths {
                    config_dir: p.clone(),
                    data_dir: p,
                }
            }
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("mail.db")
    }
    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }
}

impl AppConfig {
    /// Loads the config; on parse errors returns defaults plus the error text
    /// (the broken file is kept as `config.toml.bak`).
    pub fn load() -> (AppConfig, Option<String>) {
        let paths = Paths::get();
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => match toml::from_str::<AppConfig>(&text) {
                Ok(mut cfg) => {
                    if !cfg.oauth_keys_adopted {
                        cfg.adopt_account_keys();
                        cfg.oauth_keys_adopted = true;
                    }
                    (cfg, None)
                }
                Err(e) => {
                    let _ = std::fs::copy(&file, file.with_extension("toml.bak"));
                    (
                        AppConfig::fresh(),
                        Some(format!("Your settings file could not be read and was reset: {e}")),
                    )
                }
            },
            Err(_) => (AppConfig::fresh(), None),
        }
    }

    /// A brand-new config: it has no older per-account keys to adopt.
    /// (`Default` keeps `oauth_keys_adopted` false so that files written by
    /// older versions, which lack the field, are migrated once.)
    fn fresh() -> AppConfig {
        AppConfig {
            oauth_keys_adopted: true,
            ..AppConfig::default()
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let paths = Paths::get();
        std::fs::create_dir_all(&paths.config_dir)?;
        let text = toml::to_string_pretty(self)?;
        write_atomic(&paths.config_file(), text.as_bytes())
    }

    /// Older versions kept the OAuth client in each account; share the first
    /// one found so new accounts of that provider can use it. Runs once, so a
    /// key the person later forgets stays forgotten.
    fn adopt_account_keys(&mut self) {
        for acc in &self.accounts {
            if acc.auth != AuthKind::OAuth2 || acc.oauth.client_id.trim().is_empty() {
                continue;
            }
            if let Some(key) = self.oauth_apps.get_mut(&acc.oauth.provider) {
                if !key.is_set() {
                    *key = OAuthClientKey {
                        client_id: acc.oauth.client_id.trim().to_string(),
                        client_secret: acc.oauth.client_secret.trim().to_string(),
                        project_id: String::new(),
                        source: format!("from the account {}", acc.email),
                    };
                }
            }
        }
    }

    pub fn account(&self, id: &str) -> Option<&AccountConfig> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn account_mut(&mut self, id: &str) -> Option<&mut AccountConfig> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    pub fn default_account(&self) -> Option<&AccountConfig> {
        self.account(&self.default_account)
            .filter(|a| a.enabled)
            .or_else(|| self.accounts.iter().find(|a| a.enabled))
    }

    pub fn category_color(&self, name: &str) -> egui::Color32 {
        self.categories
            .iter()
            .find(|c| c.name == name)
            .map(|c| crate::theme::category_color(&c.color))
            .unwrap_or_else(|| crate::theme::color_for(name))
    }

    pub fn is_favorite(&self, account_id: &str, path: &str) -> bool {
        self.favorites
            .iter()
            .any(|f| f.account_id == account_id && f.path == path)
    }
}

pub fn write_atomic(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
