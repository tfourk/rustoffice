//! Plain data types shared by the store, the sync engine and the UI.

use serde::{Deserialize, Serialize};

use crate::icons;

pub type FolderId = i64;
pub type MsgId = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderRole {
    Inbox,
    Drafts,
    Sent,
    Trash,
    Junk,
    Archive,
    All,
    Custom,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Drafts => "drafts",
            FolderRole::Sent => "sent",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
            FolderRole::Archive => "archive",
            FolderRole::All => "all",
            FolderRole::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "inbox" => FolderRole::Inbox,
            "drafts" => FolderRole::Drafts,
            "sent" => FolderRole::Sent,
            "trash" => FolderRole::Trash,
            "junk" => FolderRole::Junk,
            "archive" => FolderRole::Archive,
            "all" => FolderRole::All,
            _ => FolderRole::Custom,
        }
    }

    /// Outlook's folder ordering.
    pub fn sort_key(self) -> u8 {
        match self {
            FolderRole::Inbox => 0,
            FolderRole::Drafts => 1,
            FolderRole::Sent => 2,
            FolderRole::Trash => 3,
            FolderRole::Junk => 4,
            FolderRole::Archive => 5,
            FolderRole::All => 6,
            FolderRole::Custom => 10,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            FolderRole::Inbox => icons::TRAY,
            FolderRole::Drafts => icons::NOTE_PENCIL,
            FolderRole::Sent => icons::PAPER_PLANE_TILT,
            FolderRole::Trash => icons::TRASH,
            FolderRole::Junk => icons::PROHIBIT,
            FolderRole::Archive => icons::ARCHIVE,
            FolderRole::All => icons::ENVELOPE_SIMPLE,
            FolderRole::Custom => icons::FOLDER_SIMPLE,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            FolderRole::Inbox => "Inbox",
            FolderRole::Drafts => "Drafts",
            FolderRole::Sent => "Sent Items",
            FolderRole::Trash => "Deleted Items",
            FolderRole::Junk => "Junk Email",
            FolderRole::Archive => "Archive",
            FolderRole::All => "All Mail",
            FolderRole::Custom => "Folder",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: String,
    pub path: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub role: FolderRole,
    pub selectable: bool,
    pub unread: i64,
    pub total: i64,
}

impl Folder {
    /// Display name, using Outlook's names for special folders.
    pub fn label(&self) -> String {
        match self.role {
            FolderRole::Custom => self.name.clone(),
            r => r.display_name().to_string(),
        }
    }

    pub fn parent_path(&self) -> Option<String> {
        let d = self.delimiter.as_deref().filter(|d| !d.is_empty())?;
        self.path.rfind(d).map(|i| self.path[..i].to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

impl Importance {
    pub fn to_i64(self) -> i64 {
        match self {
            Importance::Low => 0,
            Importance::Normal => 1,
            Importance::High => 2,
        }
    }
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => Importance::Low,
            2 => Importance::High,
            _ => Importance::Normal,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MessageSummary {
    pub id: MsgId,
    pub account_id: String,
    pub folder_id: FolderId,
    pub uid: i64,
    pub message_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    pub preview: String,
    #[allow(dead_code)] // used for sorting in SQL
    pub size: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub forwarded: bool,
    pub draft: bool,
    pub pinned: bool,
    pub has_attachments: bool,
    pub importance: Importance,
    pub categories: Vec<String>,
    pub focused: bool,
    pub snoozed_until: Option<i64>,
    pub has_body: bool,
    pub list_unsubscribe: Option<String>,
    pub flag_due: Option<i64>,
}

impl MessageSummary {
    pub fn sender(&self) -> &str {
        if !self.from_name.trim().is_empty() {
            self.from_name.trim()
        } else if !self.from_addr.is_empty() {
            &self.from_addr
        } else {
            "(unknown sender)"
        }
    }

    pub fn subject_or_placeholder(&self) -> &str {
        if self.subject.trim().is_empty() {
            "(no subject)"
        } else {
            self.subject.trim()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Addr {
    pub name: String,
    pub email: String,
}

impl Addr {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Addr {
            name: name.into(),
            email: email.into(),
        }
    }

    pub fn display(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            self.name.trim()
        }
    }

    pub fn to_header(&self) -> String {
        if self.name.trim().is_empty() {
            self.email.clone()
        } else {
            let needs_quotes = self.name.contains([',', ';', '<', '>', '@', '"', '(', ')']);
            if needs_quotes {
                format!("\"{}\" <{}>", self.name.replace('"', "'"), self.email)
            } else {
                format!("{} <{}>", self.name.trim(), self.email)
            }
        }
    }

    /// Parses a comma/semicolon separated recipient list such as
    /// `Jane Doe <jane@x.org>, bob@y.com; "Smith, Al" <al@z.net>`.
    pub fn parse_list(s: &str) -> Vec<Addr> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        let mut in_angle = false;
        for ch in s.chars() {
            match ch {
                '"' => {
                    in_quotes = !in_quotes;
                    cur.push(ch);
                }
                '<' if !in_quotes => {
                    in_angle = true;
                    cur.push(ch);
                }
                '>' if !in_quotes => {
                    in_angle = false;
                    cur.push(ch);
                }
                ',' | ';' if !in_quotes && !in_angle => {
                    if let Some(a) = Addr::parse_one(&cur) {
                        out.push(a);
                    }
                    cur.clear();
                }
                _ => cur.push(ch),
            }
        }
        if let Some(a) = Addr::parse_one(&cur) {
            out.push(a);
        }
        out
    }

    pub fn parse_one(s: &str) -> Option<Addr> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let (Some(l), Some(r)) = (s.rfind('<'), s.rfind('>')) {
            if l < r {
                let email = s[l + 1..r].trim().to_string();
                let name = s[..l].trim().trim_matches('"').trim().to_string();
                return Some(Addr { name, email });
            }
        }
        Some(Addr {
            name: String::new(),
            email: s.trim_matches(['<', '>', '"']).trim().to_string(),
        })
    }

    pub fn list_to_string(list: &[Addr]) -> String {
        list.iter()
            .map(|a| a.to_header())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Attachment {
    /// Part id inside the parsed message.
    pub part: usize,
    pub name: String,
    pub mime: String,
    pub size: usize,
    pub inline: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedMessage {
    pub subject: String,
    pub from: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub date: i64,
    pub message_id: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub importance: Importance,
    pub calendar: Option<String>,
    pub read_receipt_to: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Contact {
    pub id: i64,
    pub name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub company: String,
    pub job_title: String,
    pub address: String,
    pub website: String,
    pub birthday: String,
    pub notes: String,
    pub favorite: bool,
    pub auto: bool,
    pub last_used: i64,
    pub use_count: i64,
}

impl Contact {
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else {
            self.emails.first().cloned().unwrap_or_else(|| "(no name)".into())
        }
    }
    pub fn primary_email(&self) -> &str {
        self.emails.first().map(|s| s.as_str()).unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RepeatFreq {
    #[default]
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl RepeatFreq {
    pub const ALL: [RepeatFreq; 6] = [
        RepeatFreq::None,
        RepeatFreq::Daily,
        RepeatFreq::Weekdays,
        RepeatFreq::Weekly,
        RepeatFreq::Monthly,
        RepeatFreq::Yearly,
    ];
    pub fn label(self) -> &'static str {
        match self {
            RepeatFreq::None => "Don't repeat",
            RepeatFreq::Daily => "Every day",
            RepeatFreq::Weekdays => "Every weekday",
            RepeatFreq::Weekly => "Every week",
            RepeatFreq::Monthly => "Every month",
            RepeatFreq::Yearly => "Every year",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            RepeatFreq::None => "none",
            RepeatFreq::Daily => "daily",
            RepeatFreq::Weekdays => "weekdays",
            RepeatFreq::Weekly => "weekly",
            RepeatFreq::Monthly => "monthly",
            RepeatFreq::Yearly => "yearly",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "daily" => RepeatFreq::Daily,
            "weekdays" => RepeatFreq::Weekdays,
            "weekly" => RepeatFreq::Weekly,
            "monthly" => RepeatFreq::Monthly,
            "yearly" => RepeatFreq::Yearly,
            _ => RepeatFreq::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ShowAs {
    Free,
    Tentative,
    #[default]
    Busy,
    OutOfOffice,
}

impl ShowAs {
    pub const ALL: [ShowAs; 4] = [ShowAs::Free, ShowAs::Tentative, ShowAs::Busy, ShowAs::OutOfOffice];
    pub fn label(self) -> &'static str {
        match self {
            ShowAs::Free => "Free",
            ShowAs::Tentative => "Tentative",
            ShowAs::Busy => "Busy",
            ShowAs::OutOfOffice => "Away",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ShowAs::Free => "free",
            ShowAs::Tentative => "tentative",
            ShowAs::Busy => "busy",
            ShowAs::OutOfOffice => "oof",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "free" => ShowAs::Free,
            "tentative" => ShowAs::Tentative,
            "oof" => ShowAs::OutOfOffice,
            _ => ShowAs::Busy,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalEvent {
    pub id: i64,
    pub uid: String,
    pub title: String,
    pub location: String,
    pub description: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub reminder_min: Option<i64>,
    pub category: String,
    pub repeat: RepeatFreq,
    pub repeat_until: Option<i64>,
    pub show_as: ShowAs,
    pub organizer: String,
    pub attendees: String,
}

/// One concrete instance of a (possibly repeating) event.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub event: CalEvent,
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Debug, Default)]
pub struct TaskList {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Task {
    pub id: i64,
    pub list_id: i64,
    pub title: String,
    pub notes: String,
    pub due: Option<i64>,
    pub reminder: Option<i64>,
    pub important: bool,
    pub completed: bool,
    pub completed_at: Option<i64>,
    pub my_day: String,
    pub created: i64,
}

#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub id: i64,
    pub account_id: String,
    pub raw: Vec<u8>,
    pub from: String,
    pub recipients: Vec<String>,
    pub send_at: i64,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub subject: String,
    pub reply_to_msg: Option<MsgId>,
    pub forward_of: Option<MsgId>,
    pub draft_msg: Option<MsgId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SyncStatus {
    Idle { last: i64 },
    Connecting,
    Syncing(String),
    Error(String),
}

impl Default for SyncStatus {
    fn default() -> Self {
        SyncStatus::Idle { last: 0 }
    }
}
