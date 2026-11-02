//! Local cache: SQLite database holding folders, message headers, bodies,
//! a full-text index, contacts, calendar events, tasks and the outbox.
//!
//! The UI thread and the sync engine each open their own connection (WAL mode
//! lets them work concurrently).

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Row};

use crate::model::*;
use crate::util;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS folders (
    id INTEGER PRIMARY KEY,
    account_id TEXT NOT NULL,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    delimiter TEXT,
    role TEXT NOT NULL DEFAULT 'custom',
    selectable INTEGER NOT NULL DEFAULT 1,
    uid_validity INTEGER NOT NULL DEFAULT 0,
    uid_next INTEGER NOT NULL DEFAULT 0,
    UNIQUE(account_id, path)
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    account_id TEXT NOT NULL,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    message_id TEXT NOT NULL DEFAULT '',
    thread_id TEXT NOT NULL DEFAULT '',
    in_reply_to TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL DEFAULT '',
    from_name TEXT NOT NULL DEFAULT '',
    from_addr TEXT NOT NULL DEFAULT '',
    to_list TEXT NOT NULL DEFAULT '',
    cc_list TEXT NOT NULL DEFAULT '',
    date INTEGER NOT NULL DEFAULT 0,
    preview TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    seen INTEGER NOT NULL DEFAULT 0,
    flagged INTEGER NOT NULL DEFAULT 0,
    answered INTEGER NOT NULL DEFAULT 0,
    forwarded INTEGER NOT NULL DEFAULT 0,
    draft INTEGER NOT NULL DEFAULT 0,
    pinned INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    importance INTEGER NOT NULL DEFAULT 1,
    categories TEXT NOT NULL DEFAULT '',
    focused INTEGER NOT NULL DEFAULT 1,
    snoozed_until INTEGER,
    has_body INTEGER NOT NULL DEFAULT 0,
    list_unsubscribe TEXT,
    flag_due INTEGER,
    pending INTEGER,
    dirty INTEGER,
    orig_folder INTEGER,
    orig_uid INTEGER,
    UNIQUE(folder_id, uid)
);
CREATE INDEX IF NOT EXISTS idx_msg_folder_date ON messages(folder_id, date DESC);
CREATE INDEX IF NOT EXISTS idx_msg_folder_seen ON messages(folder_id, seen);
CREATE INDEX IF NOT EXISTS idx_msg_thread ON messages(account_id, thread_id);
CREATE INDEX IF NOT EXISTS idx_msg_msgid ON messages(account_id, message_id);
CREATE INDEX IF NOT EXISTS idx_msg_flagged ON messages(flagged);
CREATE INDEX IF NOT EXISTS idx_msg_from ON messages(from_addr);

CREATE TABLE IF NOT EXISTS bodies (
    msg_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    raw BLOB NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    subject, sender, recipients, body,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TABLE IF NOT EXISTS contacts (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    emails TEXT NOT NULL DEFAULT '[]',
    phones TEXT NOT NULL DEFAULT '[]',
    company TEXT NOT NULL DEFAULT '',
    job_title TEXT NOT NULL DEFAULT '',
    address TEXT NOT NULL DEFAULT '',
    website TEXT NOT NULL DEFAULT '',
    birthday TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    favorite INTEGER NOT NULL DEFAULT 0,
    auto INTEGER NOT NULL DEFAULT 0,
    last_used INTEGER NOT NULL DEFAULT 0,
    use_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    location TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    all_day INTEGER NOT NULL DEFAULT 0,
    reminder_min INTEGER,
    category TEXT NOT NULL DEFAULT '',
    repeat TEXT NOT NULL DEFAULT 'none',
    repeat_until INTEGER,
    show_as TEXT NOT NULL DEFAULT 'busy',
    organizer TEXT NOT NULL DEFAULT '',
    attendees TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_events_start ON events(start);

CREATE TABLE IF NOT EXISTS task_lists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    list_id INTEGER NOT NULL DEFAULT 1,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    due INTEGER,
    reminder INTEGER,
    important INTEGER NOT NULL DEFAULT 0,
    completed INTEGER NOT NULL DEFAULT 0,
    completed_at INTEGER,
    my_day TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY,
    account_id TEXT NOT NULL,
    raw BLOB NOT NULL,
    env_from TEXT NOT NULL,
    recipients TEXT NOT NULL,
    send_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    subject TEXT NOT NULL DEFAULT '',
    reply_to_msg INTEGER,
    forward_of INTEGER,
    draft_msg INTEGER,
    sending INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO task_lists (id, name) VALUES (1, 'Tasks');
"#;

/// `snoozed_until` value used to hide messages awaiting a server operation.
pub const HIDDEN_UNTIL: i64 = 9_999_999_999;

const MSG_COLS: &str = "m.id, m.account_id, m.folder_id, m.uid, m.message_id, m.thread_id, \
    m.subject, m.from_name, m.from_addr, m.to_list, m.cc_list, m.date, m.preview, m.size, \
    m.seen, m.flagged, m.answered, m.forwarded, m.draft, m.pinned, m.has_attachments, \
    m.importance, m.categories, m.focused, m.snoozed_until, m.has_body, m.list_unsubscribe, \
    m.flag_due";

fn row_to_summary(r: &Row) -> rusqlite::Result<MessageSummary> {
    let cats: String = r.get(22)?;
    Ok(MessageSummary {
        id: r.get(0)?,
        account_id: r.get(1)?,
        folder_id: r.get(2)?,
        uid: r.get(3)?,
        message_id: r.get(4)?,
        thread_id: r.get(5)?,
        subject: r.get(6)?,
        from_name: r.get(7)?,
        from_addr: r.get(8)?,
        to: r.get(9)?,
        cc: r.get(10)?,
        date: r.get(11)?,
        preview: r.get(12)?,
        size: r.get(13)?,
        seen: r.get(14)?,
        flagged: r.get(15)?,
        answered: r.get(16)?,
        forwarded: r.get(17)?,
        draft: r.get(18)?,
        pinned: r.get(19)?,
        has_attachments: r.get(20)?,
        importance: Importance::from_i64(r.get(21)?),
        categories: cats
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect(),
        focused: r.get(23)?,
        snoozed_until: r.get(24)?,
        has_body: r.get(25)?,
        list_unsubscribe: r.get(26)?,
        flag_due: r.get(27)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortField {
    #[default]
    Date,
    From,
    Subject,
    Size,
    Importance,
}

impl SortField {
    pub const ALL: [SortField; 5] = [
        SortField::Date,
        SortField::From,
        SortField::Subject,
        SortField::Size,
        SortField::Importance,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SortField::Date => "Date",
            SortField::From => "From",
            SortField::Subject => "Subject",
            SortField::Size => "Size",
            SortField::Importance => "Importance",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    /// Restrict to these folders (empty = every folder).
    pub folders: Vec<FolderId>,
    pub focused: Option<bool>,
    pub unread_only: bool,
    pub flagged_only: bool,
    pub attachments_only: bool,
    pub to_me: Vec<String>,
    pub search: Option<String>,
    pub sort: SortField,
    pub ascending: bool,
    pub pinned_first: bool,
    pub limit: usize,
}

/// Parsed search box input. Supports `from:`, `to:`, `subject:`,
/// `has:attachment`, `is:unread|read|flagged|pinned`, `before:`/`after:`
/// (YYYY-MM-DD) and free text (full text, prefix match).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedSearch {
    pub terms: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub has_attachment: bool,
    pub unread: Option<bool>,
    pub flagged: bool,
    pub pinned: bool,
    pub before: Option<i64>,
    pub after: Option<i64>,
}

pub fn parse_search(q: &str) -> ParsedSearch {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in q.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut ps = ParsedSearch::default();
    for t in tokens {
        let lower = t.to_lowercase();
        let (key, val) = match lower.split_once(':') {
            Some((k, v)) if !v.is_empty() => (k.to_string(), t[k.len() + 1..].to_string()),
            _ => (String::new(), t.clone()),
        };
        let date = |v: &str| {
            chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d")
                .ok()
                .map(util::local_midnight)
        };
        match key.as_str() {
            "from" => ps.from.push(val),
            "to" | "cc" => ps.to.push(val),
            "subject" => ps.subject.push(val),
            "has" if val.to_lowercase().starts_with("attach") => ps.has_attachment = true,
            "is" => match val.to_lowercase().as_str() {
                "unread" => ps.unread = Some(true),
                "read" => ps.unread = Some(false),
                "flagged" => ps.flagged = true,
                "pinned" => ps.pinned = true,
                _ => {}
            },
            "before" => ps.before = date(&val),
            "after" | "since" => ps.after = date(&val),
            _ => ps.terms.push(t),
        }
    }
    ps
}

fn fts_expr(terms: &[String]) -> Option<String> {
    let parts: Vec<String> = terms
        .iter()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '@' | '.' | '-' | '_' | '\''))
                .collect::<String>()
        })
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

pub struct NewMessage {
    pub account_id: String,
    pub folder_id: FolderId,
    pub uid: i64,
    pub message_id: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_list: String,
    pub cc_list: String,
    pub date: i64,
    pub preview: String,
    pub size: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub forwarded: bool,
    pub draft: bool,
    pub has_attachments: bool,
    pub importance: Importance,
    pub focused: bool,
    pub list_unsubscribe: Option<String>,
    pub raw: Option<Vec<u8>>,
    pub body_text: String,
}

pub struct Store {
    pub conn: Connection,
}

/// Adds columns introduced after the first release to existing databases.
fn migrate(conn: &Connection) -> Result<()> {
    let mut cols = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for c in rows {
            cols.push(c?);
        }
    }
    for (name, ty) in [("dirty", "INTEGER"), ("orig_folder", "INTEGER"), ("orig_uid", "INTEGER")] {
        if !cols.iter().any(|c| c == name) {
            conn.execute_batch(&format!("ALTER TABLE messages ADD COLUMN {name} {ty}"))?;
        }
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(15))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Store { conn })
    }

    /// In-memory store (tests).
    #[allow(dead_code)]
    pub fn memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    // ---------------------------------------------------------------- folders

    fn folder_from_row(r: &Row) -> rusqlite::Result<Folder> {
        Ok(Folder {
            id: r.get(0)?,
            account_id: r.get(1)?,
            path: r.get(2)?,
            name: r.get(3)?,
            delimiter: r.get(4)?,
            role: FolderRole::parse(&r.get::<_, String>(5)?),
            selectable: r.get(6)?,
            unread: r.get(7)?,
            total: r.get(8)?,
        })
    }

    const FOLDER_SQL: &'static str = "SELECT f.id, f.account_id, f.path, f.name, f.delimiter, f.role, f.selectable, \
        (SELECT COUNT(*) FROM messages m WHERE m.folder_id = f.id AND m.seen = 0 \
            AND (m.snoozed_until IS NULL OR m.snoozed_until <= strftime('%s','now'))), \
        (SELECT COUNT(*) FROM messages m WHERE m.folder_id = f.id) \
        FROM folders f";

    pub fn folders(&self, account_id: &str) -> Result<Vec<Folder>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("{} WHERE f.account_id = ?1", Self::FOLDER_SQL))?;
        let mut v: Vec<Folder> = stmt
            .query_map([account_id], Self::folder_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        v.sort_by(|a, b| {
            a.role
                .sort_key()
                .cmp(&b.role.sort_key())
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });
        Ok(v)
    }

    pub fn folder(&self, id: FolderId) -> Result<Option<Folder>> {
        Ok(self
            .conn
            .query_row(
                &format!("{} WHERE f.id = ?1", Self::FOLDER_SQL),
                [id],
                Self::folder_from_row,
            )
            .optional()?)
    }

    pub fn folder_by_role(&self, account_id: &str, role: FolderRole) -> Result<Option<Folder>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "{} WHERE f.account_id = ?1 AND f.role = ?2 ORDER BY f.id LIMIT 1",
                    Self::FOLDER_SQL
                ),
                params![account_id, role.as_str()],
                Self::folder_from_row,
            )
            .optional()?)
    }

    pub fn folder_by_path(&self, account_id: &str, path: &str) -> Result<Option<Folder>> {
        Ok(self
            .conn
            .query_row(
                &format!("{} WHERE f.account_id = ?1 AND f.path = ?2", Self::FOLDER_SQL),
                params![account_id, path],
                Self::folder_from_row,
            )
            .optional()?)
    }

    pub fn upsert_folder(
        &self,
        account_id: &str,
        path: &str,
        name: &str,
        delimiter: Option<&str>,
        role: FolderRole,
        selectable: bool,
    ) -> Result<FolderId> {
        self.conn.execute(
            "INSERT INTO folders (account_id, path, name, delimiter, role, selectable) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(account_id, path) DO UPDATE SET name = excluded.name, \
             delimiter = excluded.delimiter, role = excluded.role, selectable = excluded.selectable",
            params![account_id, path, name, delimiter, role.as_str(), selectable],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND path = ?2",
            params![account_id, path],
            |r| r.get(0),
        )?)
    }

    pub fn rename_folder(&self, id: FolderId, path: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET path = ?2, name = ?3 WHERE id = ?1",
            params![id, path, name],
        )?;
        Ok(())
    }

    pub fn delete_folder(&self, id: FolderId) -> Result<()> {
        self.conn.execute("DELETE FROM folders WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn delete_account(&self, account_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM folders WHERE account_id = ?1", [account_id])?;
        self.conn
            .execute("DELETE FROM outbox WHERE account_id = ?1", [account_id])?;
        Ok(())
    }

    pub fn folder_sync_state(&self, id: FolderId) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT uid_validity, uid_next FROM folders WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    pub fn set_folder_sync_state(&self, id: FolderId, uid_validity: i64, uid_next: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET uid_validity = ?2, uid_next = ?3 WHERE id = ?1",
            params![id, uid_validity, uid_next],
        )?;
        Ok(())
    }

    pub fn clear_folder_messages(&self, folder_id: FolderId) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE folder_id = ?1", [folder_id])?;
        Ok(())
    }

    // --------------------------------------------------------------- messages

    pub fn list_messages(&self, q: &ListQuery) -> Result<Vec<MessageSummary>> {
        let mut sql = format!("SELECT {MSG_COLS} FROM messages m WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();
        if !q.folders.is_empty() {
            sql.push_str(&format!(
                " AND m.folder_id IN ({})",
                vec!["?"; q.folders.len()].join(",")
            ));
            args.extend(q.folders.iter().map(|f| Value::Integer(*f)));
        }
        let searching = q.search.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        if !searching {
            sql.push_str(" AND (m.snoozed_until IS NULL OR m.snoozed_until <= ?)");
            args.push(Value::Integer(util::now_ts()));
        }
        if let Some(f) = q.focused {
            sql.push_str(" AND m.focused = ?");
            args.push(Value::Integer(f as i64));
        }
        if q.unread_only {
            sql.push_str(" AND m.seen = 0");
        }
        if q.flagged_only {
            sql.push_str(" AND m.flagged = 1");
        }
        if q.attachments_only {
            sql.push_str(" AND m.has_attachments = 1");
        }
        if !q.to_me.is_empty() {
            let parts: Vec<String> = q
                .to_me
                .iter()
                .map(|_| "(m.to_list LIKE ? OR m.cc_list LIKE ?)".to_string())
                .collect();
            sql.push_str(&format!(" AND ({})", parts.join(" OR ")));
            for e in &q.to_me {
                let pat = format!("%{}%", e);
                args.push(Value::Text(pat.clone()));
                args.push(Value::Text(pat));
            }
        }
        if searching {
            let ps = parse_search(q.search.as_deref().unwrap_or(""));
            for f in &ps.from {
                sql.push_str(" AND (m.from_name LIKE ? OR m.from_addr LIKE ?)");
                let pat = format!("%{f}%");
                args.push(Value::Text(pat.clone()));
                args.push(Value::Text(pat));
            }
            for t in &ps.to {
                sql.push_str(" AND (m.to_list LIKE ? OR m.cc_list LIKE ?)");
                let pat = format!("%{t}%");
                args.push(Value::Text(pat.clone()));
                args.push(Value::Text(pat));
            }
            for s in &ps.subject {
                sql.push_str(" AND m.subject LIKE ?");
                args.push(Value::Text(format!("%{s}%")));
            }
            if ps.has_attachment {
                sql.push_str(" AND m.has_attachments = 1");
            }
            match ps.unread {
                Some(true) => sql.push_str(" AND m.seen = 0"),
                Some(false) => sql.push_str(" AND m.seen = 1"),
                None => {}
            }
            if ps.flagged {
                sql.push_str(" AND m.flagged = 1");
            }
            if ps.pinned {
                sql.push_str(" AND m.pinned = 1");
            }
            if let Some(b) = ps.before {
                sql.push_str(" AND m.date < ?");
                args.push(Value::Integer(b));
            }
            if let Some(a) = ps.after {
                sql.push_str(" AND m.date >= ?");
                args.push(Value::Integer(a));
            }
            if let Some(expr) = fts_expr(&ps.terms) {
                sql.push_str(" AND m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)");
                args.push(Value::Text(expr));
            }
        }
        let dir = if q.ascending { "ASC" } else { "DESC" };
        let order = match q.sort {
            SortField::Date => format!("COALESCE(m.snoozed_until, m.date) {dir}"),
            SortField::From => format!(
                "LOWER(CASE WHEN m.from_name != '' THEN m.from_name ELSE m.from_addr END) {dir}, m.date DESC"
            ),
            SortField::Subject => format!("LOWER(m.subject) {dir}, m.date DESC"),
            SortField::Size => format!("m.size {dir}"),
            SortField::Importance => format!("m.importance {dir}, m.date DESC"),
        };
        if q.pinned_first && !searching {
            sql.push_str(&format!(" ORDER BY m.pinned DESC, {order}"));
        } else {
            sql.push_str(&format!(" ORDER BY {order}"));
        }
        sql.push_str(&format!(" LIMIT {}", if q.limit == 0 { 5000 } else { q.limit }));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_summary)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn message(&self, id: MsgId) -> Result<Option<MessageSummary>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MSG_COLS} FROM messages m WHERE m.id = ?1"),
                [id],
                row_to_summary,
            )
            .optional()?)
    }

    pub fn messages_by_ids(&self, ids: &[MsgId]) -> Result<Vec<MessageSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {MSG_COLS} FROM messages m WHERE m.id IN ({})",
            vec!["?"; ids.len()].join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(ids.iter()), row_to_summary)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn thread_messages(&self, account_id: &str, thread_id: &str) -> Result<Vec<MessageSummary>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {MSG_COLS} FROM messages m WHERE m.account_id = ?1 AND m.thread_id = ?2 \
             ORDER BY m.date ASC LIMIT 200"
        ))?;
        let rows = stmt
            .query_map(params![account_id, thread_id], row_to_summary)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn flagged_messages(&self) -> Result<Vec<MessageSummary>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {MSG_COLS} FROM messages m WHERE m.flagged = 1 ORDER BY m.date DESC LIMIT 500"
        ))?;
        let rows = stmt
            .query_map([], row_to_summary)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn messages_with(&self, email: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let pat = format!("%{}%", email);
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {MSG_COLS} FROM messages m WHERE m.from_addr = ?1 COLLATE NOCASE \
             OR m.to_list LIKE ?2 ORDER BY m.date DESC LIMIT ?3"
        ))?;
        let rows = stmt
            .query_map(params![email, pat, limit as i64], row_to_summary)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_thread_id(&self, account_id: &str, ids: &[String]) -> Result<Option<String>> {
        for id in ids.iter().filter(|s| !s.is_empty()) {
            let t: Option<String> = self
                .conn
                .query_row(
                    "SELECT thread_id FROM messages WHERE account_id = ?1 AND message_id = ?2 LIMIT 1",
                    params![account_id, id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(t) = t.filter(|t| !t.is_empty()) {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }

    /// Inserts (or refreshes) a message. Matches messages moved locally that
    /// are waiting for their new server UID.
    pub fn insert_message(&self, m: &NewMessage) -> Result<(MsgId, bool)> {
        // Was this message moved here by us (negative placeholder uid)?
        if m.uid > 0 && !m.message_id.is_empty() {
            let pending: Option<MsgId> = self
                .conn
                .query_row(
                    "SELECT id FROM messages WHERE folder_id = ?1 AND uid < 0 AND message_id = ?2 LIMIT 1",
                    params![m.folder_id, m.message_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = pending {
                self.conn.execute(
                    "UPDATE messages SET uid = ?2, pending = NULL, orig_folder = NULL, orig_uid = NULL, \
                     seen = ?3, flagged = ?4, answered = ?5, draft = ?6 WHERE id = ?1",
                    params![id, m.uid, m.seen, m.flagged, m.answered, m.draft],
                )?;
                return Ok((id, false));
            }
        }
        let existing: Option<MsgId> = self
            .conn
            .query_row(
                "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
                params![m.folder_id, m.uid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            self.update_server_flags(id, m.seen, m.flagged, m.answered, m.forwarded, m.draft)?;
            return Ok((id, false));
        }

        let mut candidates = m.references.clone();
        if !m.in_reply_to.is_empty() {
            candidates.push(m.in_reply_to.clone());
        }
        let thread_id = match self.find_thread_id(&m.account_id, &candidates)? {
            Some(t) => t,
            None => m
                .references
                .first()
                .cloned()
                .or_else(|| Some(m.in_reply_to.clone()).filter(|s| !s.is_empty()))
                .or_else(|| Some(m.message_id.clone()).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| format!("local-{}", uuid::Uuid::new_v4())),
        };

        self.conn.execute(
            "INSERT INTO messages (account_id, folder_id, uid, message_id, thread_id, in_reply_to, \
             subject, from_name, from_addr, to_list, cc_list, date, preview, size, seen, flagged, \
             answered, forwarded, draft, has_attachments, importance, focused, list_unsubscribe, has_body) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22, ?23, ?24)",
            params![
                m.account_id,
                m.folder_id,
                m.uid,
                m.message_id,
                thread_id,
                m.in_reply_to,
                m.subject,
                m.from_name,
                m.from_addr,
                m.to_list,
                m.cc_list,
                m.date,
                m.preview,
                m.size,
                m.seen,
                m.flagged,
                m.answered,
                m.forwarded,
                m.draft,
                m.has_attachments,
                m.importance.to_i64(),
                m.focused,
                m.list_unsubscribe,
                m.raw.is_some(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "INSERT INTO messages_fts (rowid, subject, sender, recipients, body) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                m.subject,
                format!("{} {}", m.from_name, m.from_addr),
                format!("{} {}", m.to_list, m.cc_list),
                m.body_text
            ],
        )?;
        if let Some(raw) = &m.raw {
            self.conn.execute(
                "INSERT OR REPLACE INTO bodies (msg_id, raw) VALUES (?1, ?2)",
                params![id, raw],
            )?;
        }
        Ok((id, true))
    }

    pub fn update_server_flags(
        &self,
        id: MsgId,
        seen: bool,
        flagged: bool,
        answered: bool,
        forwarded: bool,
        draft: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET seen = ?2, flagged = ?3, answered = ?4, forwarded = ?5, draft = ?6 \
             WHERE id = ?1 AND (seen != ?2 OR flagged != ?3 OR answered != ?4 OR forwarded != ?5 OR draft != ?6) \
             AND (dirty IS NULL OR dirty < ?7)",
            params![id, seen, flagged, answered, forwarded, draft, util::now_ts() - 600],
        )?;
        Ok(())
    }

    pub fn store_body(
        &self,
        id: MsgId,
        raw: &[u8],
        preview: &str,
        has_attachments: bool,
        body_text: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO bodies (msg_id, raw) VALUES (?1, ?2)",
            params![id, raw],
        )?;
        self.conn.execute(
            "UPDATE messages SET has_body = 1, preview = ?2, has_attachments = ?3 WHERE id = ?1",
            params![id, preview, has_attachments],
        )?;
        self.conn.execute(
            "UPDATE messages_fts SET body = ?2 WHERE rowid = ?1",
            params![id, body_text],
        )?;
        Ok(())
    }

    pub fn raw_body(&self, id: MsgId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT raw FROM bodies WHERE msg_id = ?1", [id], |r| r.get(0))
            .optional()?)
    }

    /// `(uid, id)` of every synced message in a folder.
    pub fn folder_uids(&self, folder_id: FolderId) -> Result<Vec<(i64, MsgId)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT uid, id FROM messages WHERE folder_id = ?1 AND uid > 0")?;
        let rows = stmt
            .query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn max_uid(&self, folder_id: FolderId) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(uid), 0) FROM messages WHERE folder_id = ?1",
            [folder_id],
            |r| r.get(0),
        )?)
    }

    pub fn min_uid(&self, folder_id: FolderId) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MIN(uid), 0) FROM messages WHERE folder_id = ?1 AND uid > 0",
            [folder_id],
            |r| r.get(0),
        )?)
    }

    pub fn messages_missing_body(
        &self,
        folder_id: FolderId,
        limit: usize,
        max_size: i64,
    ) -> Result<Vec<(MsgId, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, uid FROM messages WHERE folder_id = ?1 AND has_body = 0 AND uid > 0 \
             AND size <= ?2 ORDER BY date DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![folder_id, max_size, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_bool(&self, ids: &[MsgId], column: &str, value: bool) -> Result<()> {
        debug_assert!(matches!(
            column,
            "seen" | "flagged" | "pinned" | "focused" | "answered" | "forwarded"
        ));
        if ids.is_empty() {
            return Ok(());
        }
        // Server-backed flags are marked dirty until the engine confirms them,
        // so a concurrent sync does not flip them back.
        let dirty = if matches!(column, "seen" | "flagged" | "answered" | "forwarded") {
            ", dirty = strftime('%s','now')"
        } else {
            ""
        };
        let sql = format!(
            "UPDATE messages SET {column} = ?{dirty} WHERE id IN ({})",
            vec!["?"; ids.len()].join(",")
        );
        let mut args: Vec<Value> = vec![Value::Integer(value as i64)];
        args.extend(ids.iter().map(|i| Value::Integer(*i)));
        self.conn.execute(&sql, params_from_iter(args.iter()))?;
        Ok(())
    }

    pub fn clear_dirty(&self, ids: &[MsgId]) -> Result<()> {
        for id in ids {
            self.conn
                .execute("UPDATE messages SET dirty = NULL WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    pub fn set_categories(&self, id: MsgId, cats: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET categories = ?2 WHERE id = ?1",
            params![id, cats.join(",")],
        )?;
        Ok(())
    }

    pub fn set_snooze(&self, ids: &[MsgId], until: Option<i64>) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE messages SET snoozed_until = ?2, seen = CASE WHEN ?2 IS NULL THEN seen ELSE 0 END WHERE id = ?1",
                params![id, until],
            )?;
        }
        Ok(())
    }

    pub fn snoozed_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE snoozed_until > strftime('%s','now')",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn set_flag_due(&self, id: MsgId, due: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET flag_due = ?2 WHERE id = ?1",
            params![id, due],
        )?;
        Ok(())
    }

    /// Optimistic local move. For server accounts the uid becomes a negative
    /// placeholder until the next sync of the destination folder.
    pub fn move_local(&self, ids: &[MsgId], dest: FolderId, final_uid: bool) -> Result<()> {
        for id in ids {
            if final_uid {
                let uid = self.max_uid(dest)? + 1;
                self.conn.execute(
                    "UPDATE messages SET folder_id = ?2, uid = ?3, pending = NULL WHERE id = ?1",
                    params![id, dest, uid],
                )?;
            } else {
                // Remember where the message was so it can be restored if the
                // server never confirms the move.
                self.conn.execute(
                    "UPDATE messages SET orig_folder = COALESCE(orig_folder, folder_id), \
                     orig_uid = CASE WHEN orig_folder IS NULL THEN uid ELSE orig_uid END, \
                     folder_id = ?2, uid = -id, pending = ?3 WHERE id = ?1",
                    params![id, dest, util::now_ts()],
                )?;
            }
        }
        Ok(())
    }

    /// Puts optimistically moved / hidden messages back where they were
    /// (used when the server operation failed).
    pub fn restore_pending(&self, ids: &[MsgId]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE messages SET folder_id = orig_folder, uid = orig_uid, pending = NULL, \
                 orig_folder = NULL, orig_uid = NULL WHERE id = ?1 AND orig_folder IS NOT NULL \
                 AND NOT EXISTS (SELECT 1 FROM messages o WHERE o.folder_id = messages.orig_folder \
                 AND o.uid = messages.orig_uid AND o.id != messages.id)",
                [id],
            )?;
            tx.execute(
                "UPDATE messages SET snoozed_until = NULL WHERE id = ?1 AND snoozed_until = ?2",
                params![id, HIDDEN_UNTIL],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Hides messages until the server confirms an operation.
    pub fn hide(&self, ids: &[MsgId]) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "UPDATE messages SET snoozed_until = ?2, pending = ?3, orig_folder = COALESCE(orig_folder, folder_id), \
                 orig_uid = CASE WHEN orig_folder IS NULL THEN uid ELSE orig_uid END WHERE id = ?1",
                params![id, HIDDEN_UNTIL, util::now_ts()],
            )?;
        }
        Ok(())
    }

    /// Placeholders that never received a server uid go back to their original
    /// folder (the server still has them there); anything else is dropped.
    pub fn purge_stale_pending(&self, folder_id: FolderId, older_than_s: i64) -> Result<usize> {
        let cutoff = util::now_ts() - older_than_s;
        let ids: Vec<MsgId> = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT id FROM messages WHERE folder_id = ?1 AND uid < 0 AND pending < ?2")?;
            let rows = stmt.query_map(params![folder_id, cutoff], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        self.restore_pending(&ids)?;
        // Messages hidden for an operation that never reached the server.
        self.conn.execute(
            "UPDATE messages SET snoozed_until = NULL, pending = NULL, orig_folder = NULL, orig_uid = NULL \
             WHERE folder_id = ?1 AND snoozed_until = ?2 AND pending < ?3",
            params![folder_id, HIDDEN_UNTIL, cutoff],
        )?;
        Ok(self.conn.execute(
            "DELETE FROM messages WHERE folder_id = ?1 AND uid < 0 AND pending < ?2",
            params![folder_id, cutoff],
        )?)
    }

    pub fn delete_messages(&self, ids: &[MsgId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "DELETE FROM messages WHERE id IN ({})",
            vec!["?"; ids.len()].join(",")
        );
        self.conn.execute(&sql, params_from_iter(ids.iter()))?;
        Ok(())
    }

    pub fn delete_by_uids(&self, folder_id: FolderId, uids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for uid in uids {
            tx.execute(
                "DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2",
                params![folder_id, uid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn unread_inbox_total(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id \
             WHERE f.role = 'inbox' AND m.seen = 0",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('optimize');")?;
        Ok(())
    }

    // --------------------------------------------------------------- contacts

    fn contact_from_row(r: &Row) -> rusqlite::Result<Contact> {
        let emails: String = r.get(2)?;
        let phones: String = r.get(3)?;
        Ok(Contact {
            id: r.get(0)?,
            name: r.get(1)?,
            emails: serde_json::from_str(&emails).unwrap_or_default(),
            phones: serde_json::from_str(&phones).unwrap_or_default(),
            company: r.get(4)?,
            job_title: r.get(5)?,
            address: r.get(6)?,
            website: r.get(7)?,
            birthday: r.get(8)?,
            notes: r.get(9)?,
            favorite: r.get(10)?,
            auto: r.get(11)?,
            last_used: r.get(12)?,
            use_count: r.get(13)?,
        })
    }

    const CONTACT_COLS: &'static str = "id, name, emails, phones, company, job_title, address, website, \
        birthday, notes, favorite, auto, last_used, use_count";

    pub fn contacts(&self) -> Result<Vec<Contact>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM contacts ORDER BY LOWER(name), id",
            Self::CONTACT_COLS
        ))?;
        let rows = stmt
            .query_map([], Self::contact_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn contact_by_email(&self, email: &str) -> Result<Option<Contact>> {
        let pat = format!("%\"{}\"%", email.to_lowercase());
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM contacts WHERE LOWER(emails) LIKE ?1 ORDER BY auto ASC LIMIT 1",
                    Self::CONTACT_COLS
                ),
                [pat],
                Self::contact_from_row,
            )
            .optional()?)
    }

    pub fn save_contact(&self, c: &Contact) -> Result<i64> {
        let emails = serde_json::to_string(&c.emails)?;
        let phones = serde_json::to_string(&c.phones)?;
        if c.id > 0 {
            self.conn.execute(
                "UPDATE contacts SET name=?2, emails=?3, phones=?4, company=?5, job_title=?6, \
                 address=?7, website=?8, birthday=?9, notes=?10, favorite=?11, auto=?12 WHERE id=?1",
                params![
                    c.id, c.name, emails, phones, c.company, c.job_title, c.address, c.website,
                    c.birthday, c.notes, c.favorite, c.auto
                ],
            )?;
            Ok(c.id)
        } else {
            self.conn.execute(
                "INSERT INTO contacts (name, emails, phones, company, job_title, address, website, \
                 birthday, notes, favorite, auto, last_used, use_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    c.name,
                    emails,
                    phones,
                    c.company,
                    c.job_title,
                    c.address,
                    c.website,
                    c.birthday,
                    c.notes,
                    c.favorite,
                    c.auto,
                    c.last_used,
                    c.use_count
                ],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    pub fn delete_contact(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM contacts WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Records that we exchanged mail with someone (auto-complete list).
    pub fn touch_contact(&self, name: &str, email: &str) -> Result<()> {
        let email = email.trim().to_lowercase();
        if !util::is_valid_email(&email) {
            return Ok(());
        }
        if let Some(mut c) = self.contact_by_email(&email)? {
            if c.name.trim().is_empty() && !name.trim().is_empty() {
                c.name = name.trim().to_string();
                self.save_contact(&c)?;
            }
            self.conn.execute(
                "UPDATE contacts SET use_count = use_count + 1, last_used = ?2 WHERE id = ?1",
                params![c.id, util::now_ts()],
            )?;
        } else {
            let c = Contact {
                name: name.trim().to_string(),
                emails: vec![email],
                auto: true,
                last_used: util::now_ts(),
                use_count: 1,
                ..Default::default()
            };
            self.save_contact(&c)?;
        }
        Ok(())
    }

    /// Recipient suggestions for the compose form: `(name, email)`.
    pub fn suggest_recipients(&self, needle: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let pat = format!("%{}%", needle);
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM contacts WHERE LOWER(name) LIKE ?1 OR LOWER(emails) LIKE ?1 \
             ORDER BY favorite DESC, auto ASC, use_count DESC, last_used DESC LIMIT 40",
            Self::CONTACT_COLS
        ))?;
        let contacts = stmt
            .query_map([pat], Self::contact_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for c in contacts {
            let name_hit = c.name.to_lowercase().contains(&needle);
            for e in &c.emails {
                if name_hit || e.to_lowercase().contains(&needle) {
                    out.push((c.name.clone(), e.clone()));
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    // ----------------------------------------------------------------- events

    fn event_from_row(r: &Row) -> rusqlite::Result<CalEvent> {
        Ok(CalEvent {
            id: r.get(0)?,
            uid: r.get(1)?,
            title: r.get(2)?,
            location: r.get(3)?,
            description: r.get(4)?,
            start: r.get(5)?,
            end: r.get(6)?,
            all_day: r.get(7)?,
            reminder_min: r.get(8)?,
            category: r.get(9)?,
            repeat: RepeatFreq::parse(&r.get::<_, String>(10)?),
            repeat_until: r.get(11)?,
            show_as: ShowAs::parse(&r.get::<_, String>(12)?),
            organizer: r.get(13)?,
            attendees: r.get(14)?,
        })
    }

    const EVENT_COLS: &'static str = "id, uid, title, location, description, start, end, all_day, \
        reminder_min, category, repeat, repeat_until, show_as, organizer, attendees";

    /// Events that may have occurrences in `[start, end)`.
    pub fn events_between(&self, start: i64, end: i64) -> Result<Vec<CalEvent>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM events WHERE (repeat = 'none' AND start < ?2 AND end > ?1) \
             OR (repeat != 'none' AND start < ?2 AND (repeat_until IS NULL OR repeat_until >= ?1)) \
             ORDER BY start",
            Self::EVENT_COLS
        ))?;
        let rows = stmt
            .query_map(params![start, end], Self::event_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn event(&self, id: i64) -> Result<Option<CalEvent>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM events WHERE id = ?1", Self::EVENT_COLS),
                [id],
                Self::event_from_row,
            )
            .optional()?)
    }

    pub fn event_by_uid(&self, uid: &str) -> Result<Option<CalEvent>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM events WHERE uid = ?1", Self::EVENT_COLS),
                [uid],
                Self::event_from_row,
            )
            .optional()?)
    }

    pub fn save_event(&self, e: &CalEvent) -> Result<i64> {
        let uid = if e.uid.is_empty() {
            format!("{}@rustmail", uuid::Uuid::new_v4())
        } else {
            e.uid.clone()
        };
        if e.id > 0 {
            self.conn.execute(
                "UPDATE events SET uid=?2, title=?3, location=?4, description=?5, start=?6, end=?7, \
                 all_day=?8, reminder_min=?9, category=?10, repeat=?11, repeat_until=?12, show_as=?13, \
                 organizer=?14, attendees=?15 WHERE id=?1",
                params![
                    e.id,
                    uid,
                    e.title,
                    e.location,
                    e.description,
                    e.start,
                    e.end,
                    e.all_day,
                    e.reminder_min,
                    e.category,
                    e.repeat.as_str(),
                    e.repeat_until,
                    e.show_as.as_str(),
                    e.organizer,
                    e.attendees
                ],
            )?;
            Ok(e.id)
        } else {
            self.conn.execute(
                "INSERT INTO events (uid, title, location, description, start, end, all_day, \
                 reminder_min, category, repeat, repeat_until, show_as, organizer, attendees) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    uid,
                    e.title,
                    e.location,
                    e.description,
                    e.start,
                    e.end,
                    e.all_day,
                    e.reminder_min,
                    e.category,
                    e.repeat.as_str(),
                    e.repeat_until,
                    e.show_as.as_str(),
                    e.organizer,
                    e.attendees
                ],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    pub fn delete_event(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
        Ok(())
    }

    // ------------------------------------------------------------------ tasks

    pub fn task_lists(&self) -> Result<Vec<TaskList>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, name FROM task_lists ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TaskList {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn save_task_list(&self, id: i64, name: &str) -> Result<i64> {
        if id > 0 {
            self.conn.execute(
                "UPDATE task_lists SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            Ok(id)
        } else {
            self.conn
                .execute("INSERT INTO task_lists (name) VALUES (?1)", [name])?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    pub fn delete_task_list(&self, id: i64) -> Result<()> {
        if id == 1 {
            return Ok(());
        }
        self.conn.execute("DELETE FROM tasks WHERE list_id = ?1", [id])?;
        self.conn.execute("DELETE FROM task_lists WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, list_id, title, notes, due, reminder, important, completed, completed_at, \
             my_day, created FROM tasks ORDER BY completed ASC, created DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Task {
                    id: r.get(0)?,
                    list_id: r.get(1)?,
                    title: r.get(2)?,
                    notes: r.get(3)?,
                    due: r.get(4)?,
                    reminder: r.get(5)?,
                    important: r.get(6)?,
                    completed: r.get(7)?,
                    completed_at: r.get(8)?,
                    my_day: r.get(9)?,
                    created: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn save_task(&self, t: &Task) -> Result<i64> {
        if t.id > 0 {
            self.conn.execute(
                "UPDATE tasks SET list_id=?2, title=?3, notes=?4, due=?5, reminder=?6, important=?7, \
                 completed=?8, completed_at=?9, my_day=?10 WHERE id=?1",
                params![
                    t.id,
                    t.list_id,
                    t.title,
                    t.notes,
                    t.due,
                    t.reminder,
                    t.important,
                    t.completed,
                    t.completed_at,
                    t.my_day
                ],
            )?;
            Ok(t.id)
        } else {
            self.conn.execute(
                "INSERT INTO tasks (list_id, title, notes, due, reminder, important, completed, \
                 completed_at, my_day, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    t.list_id,
                    t.title,
                    t.notes,
                    t.due,
                    t.reminder,
                    t.important,
                    t.completed,
                    t.completed_at,
                    t.my_day,
                    if t.created > 0 { t.created } else { util::now_ts() }
                ],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    pub fn delete_task(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(())
    }

    // ----------------------------------------------------------------- outbox

    pub fn add_outbox(&self, item: &OutboxItem) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO outbox (account_id, raw, env_from, recipients, send_at, subject, \
             reply_to_msg, forward_of, draft_msg) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                item.account_id,
                item.raw,
                item.from,
                serde_json::to_string(&item.recipients)?,
                item.send_at,
                item.subject,
                item.reply_to_msg,
                item.forward_of,
                item.draft_msg
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn outbox_from_row(r: &Row) -> rusqlite::Result<OutboxItem> {
        let rec: String = r.get(4)?;
        Ok(OutboxItem {
            id: r.get(0)?,
            account_id: r.get(1)?,
            raw: r.get(2)?,
            from: r.get(3)?,
            recipients: serde_json::from_str(&rec).unwrap_or_default(),
            send_at: r.get(5)?,
            attempts: r.get(6)?,
            last_error: r.get(7)?,
            subject: r.get(8)?,
            reply_to_msg: r.get(9)?,
            forward_of: r.get(10)?,
            draft_msg: r.get(11)?,
        })
    }

    const OUTBOX_COLS: &'static str = "id, account_id, raw, env_from, recipients, send_at, attempts, \
        last_error, subject, reply_to_msg, forward_of, draft_msg";

    pub fn outbox(&self) -> Result<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM outbox ORDER BY send_at",
            Self::OUTBOX_COLS
        ))?;
        let rows = stmt
            .query_map([], Self::outbox_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn outbox_item(&self, id: i64) -> Result<Option<OutboxItem>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM outbox WHERE id = ?1", Self::OUTBOX_COLS),
                [id],
                Self::outbox_from_row,
            )
            .optional()?)
    }

    /// Claims due outbox items (marks them as being sent).
    pub fn claim_due_outbox(&self, now: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM outbox WHERE send_at <= ?1 AND sending = 0 AND attempts < 5",
        )?;
        let ids = stmt
            .query_map([now], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in &ids {
            self.conn
                .execute("UPDATE outbox SET sending = 1 WHERE id = ?1", [id])?;
        }
        Ok(ids)
    }

    pub fn outbox_failed(&self, id: i64, err: &str, retry_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET sending = 0, attempts = attempts + 1, last_error = ?2, send_at = ?3 WHERE id = ?1",
            params![id, err, retry_at],
        )?;
        Ok(())
    }

    pub fn outbox_reset_sending(&self) -> Result<()> {
        self.conn.execute("UPDATE outbox SET sending = 0", [])?;
        Ok(())
    }

    pub fn outbox_retry(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET attempts = 0, sending = 0, send_at = ?2 WHERE id = ?1",
            params![id, util::now_ts()],
        )?;
        Ok(())
    }

    pub fn remove_outbox(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
        Ok(())
    }

    // --------------------------------------------------------------------- kv

    pub fn kv_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(folder: FolderId, uid: i64, mid: &str, refs: &[&str], subject: &str) -> NewMessage {
        NewMessage {
            account_id: "a".into(),
            folder_id: folder,
            uid,
            message_id: mid.into(),
            in_reply_to: refs.last().map(|s| s.to_string()).unwrap_or_default(),
            references: refs.iter().map(|s| s.to_string()).collect(),
            subject: subject.into(),
            from_name: "Ann".into(),
            from_addr: "ann@example.com".into(),
            to_list: "bob@example.com".into(),
            cc_list: String::new(),
            date: 1000 + uid,
            preview: String::new(),
            size: 10,
            seen: false,
            flagged: false,
            answered: false,
            forwarded: false,
            draft: false,
            has_attachments: false,
            importance: Importance::Normal,
            focused: true,
            list_unsubscribe: None,
            raw: None,
            body_text: "quarterly budget report".into(),
        }
    }

    #[test]
    fn threading_search_and_moves() {
        let s = Store::memory().unwrap();
        let inbox = s
            .upsert_folder("a", "INBOX", "INBOX", Some("/"), FolderRole::Inbox, true)
            .unwrap();
        let archive = s
            .upsert_folder("a", "Archive", "Archive", Some("/"), FolderRole::Archive, true)
            .unwrap();
        let (a, new) = s.insert_message(&msg(inbox, 1, "m1", &[], "Budget")).unwrap();
        assert!(new);
        let (b, _) = s
            .insert_message(&msg(inbox, 2, "m2", &["m1"], "Re: Budget"))
            .unwrap();
        let ma = s.message(a).unwrap().unwrap();
        let mb = s.message(b).unwrap().unwrap();
        assert_eq!(ma.thread_id, mb.thread_id);
        // Re-inserting the same uid does not duplicate.
        let (b2, new2) = s
            .insert_message(&msg(inbox, 2, "m2", &["m1"], "Re: Budget"))
            .unwrap();
        assert_eq!(b, b2);
        assert!(!new2);

        let q = ListQuery {
            folders: vec![inbox],
            search: Some("budg".into()),
            ..Default::default()
        };
        assert_eq!(s.list_messages(&q).unwrap().len(), 2);
        let q = ListQuery {
            folders: vec![inbox],
            search: Some("from:ann subject:re".into()),
            ..Default::default()
        };
        assert_eq!(s.list_messages(&q).unwrap().len(), 1);

        // Optimistic move then server sync assigns the real uid.
        s.move_local(&[a], archive, false).unwrap();
        assert!(s.message(a).unwrap().unwrap().uid < 0);
        let (a2, _) = s.insert_message(&msg(archive, 77, "m1", &[], "Budget")).unwrap();
        assert_eq!(a, a2);
        assert_eq!(s.message(a).unwrap().unwrap().uid, 77);

        // A move the server never confirmed is undone, not lost.
        s.move_local(&[b], archive, false).unwrap();
        s.restore_pending(&[b]).unwrap();
        let back = s.message(b).unwrap().unwrap();
        assert_eq!((back.folder_id, back.uid), (inbox, 2));
        s.hide(&[b]).unwrap();
        assert!(s.list_messages(&ListQuery { folders: vec![inbox], ..Default::default() }).unwrap().iter().all(|m| m.id != b));
        s.restore_pending(&[b]).unwrap();
        assert!(s.list_messages(&ListQuery { folders: vec![inbox], ..Default::default() }).unwrap().iter().any(|m| m.id == b));
        assert_eq!(s.folders("a").unwrap().len(), 2);
    }

    #[test]
    fn search_parser() {
        let p = parse_search("from:alice \"quarterly report\" is:unread has:attachments before:2026-01-02");
        assert_eq!(p.from, vec!["alice".to_string()]);
        assert_eq!(p.terms, vec!["quarterly report".to_string()]);
        assert_eq!(p.unread, Some(true));
        assert!(p.has_attachment);
        assert!(p.before.is_some());
    }

    #[test]
    fn contacts_suggest() {
        let s = Store::memory().unwrap();
        s.touch_contact("Jane Doe", "Jane@Example.com").unwrap();
        s.touch_contact("", "jane@example.com").unwrap();
        let c = s.contacts().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].use_count, 2);
        let sug = s.suggest_recipients("jan", 5).unwrap();
        assert_eq!(sug[0].1, "jane@example.com");
    }
}
