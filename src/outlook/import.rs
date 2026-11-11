//! Writes the contents of an Outlook data file into the store: mail into a
//! mailbox of its own (an offline account that mirrors the file's folders),
//! contacts, calendar events and tasks next to the ones already there.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::convert::{self, Kind};
use super::reader::{Archive, FolderInfo};
use super::{Named, RawItem};
use crate::config::{AccountConfig, AccountKind, AppConfig, CategoryDef};
use crate::model::{Addr, FolderId, FolderRole, Task, LOCAL_CALENDAR};
use crate::store::{NewMessage, Store};
use crate::{cal, mime, util};

/// Items written per database transaction.
const BATCH: usize = 200;
const MAX_ERRORS: usize = 20;
/// Name of the task list imported tasks go to.
const TASK_LIST: &str = "Outlook";
/// `provider` of the accounts made by an import (shown in Settings).
pub const PROVIDER: &str = "Outlook data file";

/// Outlook's own bookkeeping folders, by the start of their name.
const SKIPPED_FOLDERS: &[&str] = &[
    "Sync Issues",
    "Conflicts",
    "Local Failures",
    "Server Failures",
    "Conversation Action Settings",
    "Quick Step Settings",
    "ExternalContacts",
    "PersonMetadata",
    "Yammer Root",
    "Files",
    "Social Activity Notifications",
];

/// How far an import has got; also its final summary.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Folder being read.
    pub folder: String,
    pub done: usize,
    pub total: usize,
    pub mails: usize,
    pub contacts: usize,
    pub events: usize,
    pub tasks: usize,
    /// Items with nothing to carry over (distribution lists, journal
    /// entries…) and ones that were already imported.
    pub skipped: usize,
    pub failed: usize,
    /// The first few problems, for the summary.
    pub errors: Vec<String>,
    /// The address most of the sent mail came from.
    pub owner: String,
    /// The data file's own name.
    pub name: String,
    /// Categories used by the imported mail.
    pub categories: Vec<String>,
    pub stopped: bool,
    /// Set when the import is over: `Err` if the file couldn't be read at all.
    pub finished: Option<Result<(), String>>,
}

impl Progress {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (n, one, many) in [
            (self.mails, "message", "messages"),
            (self.contacts, "contact", "contacts"),
            (self.events, "event", "events"),
            (self.tasks, "task", "tasks"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        }
        if parts.is_empty() {
            "Nothing to import was found".into()
        } else {
            format!("Imported {}", parts.join(", "))
        }
    }
}

/// The offline account an import's mail goes to.
pub fn mailbox_account(file: &Path, cfg: &AppConfig) -> AccountConfig {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("Outlook").trim().to_string();
    let base = format!("Outlook: {}", if stem.is_empty() { "data file" } else { &stem });
    let mut label = base.clone();
    let mut n = 2;
    while cfg.accounts.iter().any(|a| a.label == label) {
        label = format!("{base} ({n})");
        n += 1;
    }
    AccountConfig {
        kind: AccountKind::Local,
        label,
        // An .ost is named after its mailbox.
        email: if util::is_valid_email(&stem) { stem } else { String::new() },
        provider: PROVIDER.into(),
        ..Default::default()
    }
}

fn role_of(name: &str) -> FolderRole {
    match name.trim().to_lowercase().as_str() {
        "inbox" | "posteingang" | "boîte de réception" | "bandeja de entrada" | "posta in arrivo" | "postvak in" | "caixa de entrada" => FolderRole::Inbox,
        "sent items" | "sent" | "sent mail" | "gesendete elemente" | "éléments envoyés" | "elementos enviados" | "posta inviata" | "verzonden items" | "itens enviados" => FolderRole::Sent,
        "drafts" | "entwürfe" | "brouillons" | "borradores" | "bozze" | "concepten" | "rascunhos" => FolderRole::Drafts,
        "deleted items" | "trash" | "gelöschte elemente" | "éléments supprimés" | "elementos eliminados" | "posta eliminata" | "verwijderde items" | "itens excluídos" => FolderRole::Trash,
        "junk e-mail" | "junk email" | "junk" | "spam" | "junk-e-mail" | "courrier indésirable" | "correo no deseado" | "posta indesiderata" | "ongewenste e-mail" | "lixo eletrônico" => FolderRole::Junk,
        "archive" | "archiv" | "archivo" | "archivio" | "archief" | "arquivo morto" => FolderRole::Archive,
        _ => FolderRole::Custom,
    }
}

/// Creates the mail folders of the mailbox as they are needed.
struct Folders<'a> {
    store: &'a Store,
    account_id: &'a str,
    ids: HashMap<String, FolderId>,
    next_uid: HashMap<FolderId, i64>,
    used_roles: HashSet<FolderRole>,
}

impl Folders<'_> {
    /// The folder for `path`, with its parents.
    fn ensure(&mut self, path: &[String]) -> Result<FolderId> {
        let mut id = 0;
        for depth in 1..=path.len() {
            let names: Vec<String> = path[..depth].iter().map(|n| n.replace('/', "-")).collect();
            let key = names.join("/");
            id = match self.ids.get(&key) {
                Some(id) => *id,
                None => {
                    // Only top-level folders are Outlook's special ones.
                    let role = Some(role_of(&names[depth - 1])).filter(|r| depth == 1 && *r != FolderRole::Custom && self.used_roles.insert(*r));
                    let id = self.store.upsert_folder(self.account_id, &key, &names[depth - 1], Some("/"), role.unwrap_or(FolderRole::Custom), true)?;
                    self.ids.insert(key, id);
                    id
                }
            };
        }
        Ok(id)
    }

    fn uid(&mut self, folder: FolderId) -> Result<i64> {
        let next = match self.next_uid.get(&folder) {
            Some(n) => *n,
            None => self.store.max_uid(folder)?.max(0) + 1,
        };
        self.next_uid.insert(folder, next + 1);
        Ok(next)
    }
}

struct Importer<'a> {
    store: &'a Store,
    account_id: &'a str,
    named: &'a Named,
    folders: Folders<'a>,
    senders: HashMap<String, usize>,
    categories: HashSet<String>,
    /// Names of the contacts without an address, to recognise them again.
    contact_names: HashSet<String>,
    /// (title, due) of the tasks already in the task list.
    task_keys: HashSet<(String, Option<i64>)>,
    task_list: Option<i64>,
}

/// What became of one item.
enum Outcome {
    Mail,
    Contact,
    Event,
    Task,
    Skipped,
}

impl<'a> Importer<'a> {
    fn new(store: &'a Store, account_id: &'a str, named: &'a Named) -> Result<Self> {
        Ok(Importer {
            store,
            account_id,
            named,
            folders: Folders {
                store,
                account_id,
                ids: HashMap::new(),
                next_uid: HashMap::new(),
                used_roles: HashSet::new(),
            },
            senders: HashMap::new(),
            categories: HashSet::new(),
            contact_names: store.contacts()?.into_iter().filter(|c| c.emails.is_empty()).map(|c| c.name.to_lowercase()).collect(),
            task_keys: HashSet::new(),
            task_list: None,
        })
    }

    /// Stores one item found in the folder at `path`.
    fn item(&mut self, path: &[String], item: &RawItem) -> Result<Outcome> {
        let named = self.named;
        match convert::kind_of(&item.class) {
            Kind::Mail => {
                if convert::is_blank(item) {
                    return Ok(Outcome::Skipped);
                }
                let m = convert::mail(item, named);
                let folder_id = self.folders.ensure(path)?;
                let info = mime::header_info(&m.raw);
                let parsed = mime::parse_full(&m.raw);
                let text = parsed.as_ref().map(mime::plain_text).unwrap_or_default();
                let new = NewMessage {
                    account_id: self.account_id.to_string(),
                    folder_id,
                    uid: self.folders.uid(folder_id)?,
                    message_id: info.message_id,
                    in_reply_to: info.in_reply_to,
                    references: info.references,
                    subject: info.subject,
                    from_name: info.from.name,
                    from_addr: info.from.email,
                    to_list: Addr::list_to_string(&info.to),
                    cc_list: Addr::list_to_string(&info.cc),
                    date: m.date,
                    preview: util::make_preview(&text, 220),
                    size: m.raw.len() as i64,
                    seen: m.seen,
                    flagged: m.flagged,
                    answered: m.answered,
                    forwarded: m.forwarded,
                    draft: m.draft,
                    has_attachments: parsed.as_ref().is_some_and(|p| p.attachments.iter().any(|a| !a.inline)),
                    importance: m.importance,
                    focused: !info.bulk,
                    list_unsubscribe: info.list_unsubscribe,
                    raw: Some(m.raw),
                    body_text: text,
                };
                let (id, _) = self.store.insert_message(&new)?;
                if !m.categories.is_empty() {
                    self.store.set_categories(id, &m.categories)?;
                    self.categories.extend(m.categories);
                }
                let sent_folder = path.first().is_some_and(|n| role_of(n) == FolderRole::Sent);
                if (m.from_me || sent_folder) && !m.sender.is_empty() {
                    *self.senders.entry(m.sender.to_lowercase()).or_default() += 1;
                }
                Ok(Outcome::Mail)
            }
            Kind::Contact => {
                let Some(mut c) = convert::contact(item, named) else { return Ok(Outcome::Skipped) };
                // Someone already in the contacts (a second import, or a
                // person added by hand) is left as they are.
                let known = match c.emails.first() {
                    Some(email) => self.store.contact_by_email(email)?.is_some_and(|old| !old.auto),
                    None => !self.contact_names.insert(c.name.to_lowercase()),
                };
                if known {
                    return Ok(Outcome::Skipped);
                }
                // A contact the app only suggested so far becomes a real one.
                if let Some(old) = c.emails.first().and_then(|e| self.store.contact_by_email(e).ok().flatten()) {
                    c.id = old.id;
                }
                self.store.save_contact(&c)?;
                Ok(Outcome::Contact)
            }
            Kind::Event => {
                let Some(ics) = convert::event_ics(item, named) else { return Ok(Outcome::Skipped) };
                let Some(mut e) = cal::parse_ics(&ics).events.into_iter().next() else { return Ok(Outcome::Skipped) };
                if self.store.event_by_uid(&e.uid)?.is_some() {
                    return Ok(Outcome::Skipped);
                }
                e.calendar_id = LOCAL_CALENDAR;
                self.store.save_event(&e)?;
                Ok(Outcome::Event)
            }
            Kind::Task => {
                let Some(t) = convert::task(item, named) else { return Ok(Outcome::Skipped) };
                let list_id = match self.task_list {
                    Some(id) => id,
                    None => {
                        let existing = self.store.task_lists()?.into_iter().find(|l| l.name == TASK_LIST).map(|l| l.id);
                        let id = match existing {
                            Some(id) => id,
                            None => self.store.save_task_list(0, TASK_LIST)?,
                        };
                        self.task_keys = self.store.tasks()?.into_iter().filter(|t| t.list_id == id).map(|t| (t.title, t.due)).collect();
                        self.task_list = Some(id);
                        id
                    }
                };
                if !self.task_keys.insert((t.title.clone(), t.due)) {
                    return Ok(Outcome::Skipped);
                }
                self.store.save_task(&Task {
                    list_id,
                    title: t.title,
                    notes: t.notes,
                    due: t.due,
                    important: t.important,
                    completed: t.completed,
                    completed_at: t.completed_at,
                    created: util::now_ts(),
                    ..Default::default()
                })?;
                Ok(Outcome::Task)
            }
            Kind::Other => Ok(Outcome::Skipped),
        }
    }
}

/// Imports `file` into `store`, with its mail under `account_id`. Progress
/// is published through `progress` (and `wake` is called when it changed);
/// setting `cancel` stops after the current item, keeping what is in.
pub fn run(file: &Path, store: &Store, account_id: &str, progress: &Mutex<Progress>, cancel: &AtomicBool, wake: &dyn Fn()) -> Result<()> {
    let update = |f: &mut dyn FnMut(&mut Progress)| {
        if let Ok(mut p) = progress.lock() {
            f(&mut p);
        }
    };
    let archive = Archive::open(file)?;
    let folders: Vec<FolderInfo> = archive
        .folders()?
        .into_iter()
        .filter(|f| !f.path.iter().any(|n| SKIPPED_FOLDERS.iter().any(|s| n.starts_with(s))))
        .filter(|f| !f.class.eq_ignore_ascii_case("IPF.Configuration"))
        .collect();
    update(&mut |p| {
        p.total = folders.iter().map(|f| f.count).sum();
        p.name = archive.name.clone();
    });
    wake();

    let mut importer = Importer::new(store, account_id, &archive.named)?;
    let mut transaction = Some(store.conn.unchecked_transaction()?);
    let mut in_batch = 0;
    'folders: for folder in &folders {
        let shown = folder.path.join(" / ");
        update(&mut |p| p.folder = shown.clone());
        wake();
        let ids = match archive.item_ids(folder) {
            Ok(ids) => ids,
            Err(e) => {
                update(&mut |p| {
                    p.failed += folder.count;
                    p.done += folder.count;
                    if p.errors.len() < MAX_ERRORS {
                        p.errors.push(format!("{shown}: {e}"));
                    }
                });
                continue;
            }
        };
        // Mail folders exist in the mailbox even when empty. (Old files
        // don't say what a folder holds, so there only the well-known ones.)
        let class = folder.class.to_ascii_uppercase();
        let well_known = folder.path.len() == 1 && role_of(&folder.path[0]) != FolderRole::Custom;
        if class.starts_with("IPF.NOTE") || class == "IPF.IMAP" || (class.is_empty() && well_known) {
            importer.folders.ensure(&folder.path)?;
        }
        for id in ids {
            if cancel.load(Ordering::Relaxed) {
                update(&mut |p| p.stopped = true);
                break 'folders;
            }
            let outcome = archive.item(id).and_then(|item| importer.item(&folder.path, &item));
            update(&mut |p| {
                p.done += 1;
                match &outcome {
                    Ok(Outcome::Mail) => p.mails += 1,
                    Ok(Outcome::Contact) => p.contacts += 1,
                    Ok(Outcome::Event) => p.events += 1,
                    Ok(Outcome::Task) => p.tasks += 1,
                    Ok(Outcome::Skipped) => p.skipped += 1,
                    Err(e) => {
                        p.failed += 1;
                        if p.errors.len() < MAX_ERRORS {
                            p.errors.push(format!("{shown}: {e}"));
                        }
                    }
                }
            });
            in_batch += 1;
            if in_batch >= BATCH {
                in_batch = 0;
                if let Some(t) = transaction.take() {
                    t.commit()?;
                }
                transaction = Some(store.conn.unchecked_transaction()?);
                wake();
            }
        }
    }
    // Nothing here waits to be sent to a server.
    store.conn.execute("UPDATE messages SET dirty = NULL WHERE account_id = ?1", [account_id])?;
    if let Some(t) = transaction.take() {
        t.commit()?;
    }
    let owner = importer.senders.iter().max_by_key(|(_, n)| **n).map(|(e, _)| e.clone()).unwrap_or_default();
    let mut categories: Vec<String> = importer.categories.into_iter().collect();
    categories.sort();
    update(&mut |p| {
        p.owner = owner.clone();
        p.categories = categories.clone();
    });
    Ok(())
}

/// Finishes an import: adds the mailbox to the settings when mail was
/// imported (returns whether it was), and the categories the mail uses. The
/// caller saves the settings.
pub fn apply(cfg: &mut AppConfig, store: &Store, mut account: AccountConfig, progress: &Progress) -> bool {
    let imported = matches!(progress.finished, Some(Ok(())) | None) && progress.mails > 0;
    if !imported {
        // No mail (a contacts-only file, or the file couldn't be read):
        // don't leave an empty mailbox behind.
        let _ = store.delete_account(&account.id);
        return false;
    }
    if account.email.is_empty() {
        account.email = progress.owner.clone();
    }
    for name in &progress.categories {
        if !cfg.categories.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            let color = crate::theme::CATEGORY_COLORS[cfg.categories.len() % crate::theme::CATEGORY_COLORS.len()];
            cfg.categories.push(CategoryDef {
                name: name.clone(),
                color: color.to_string(),
            });
        }
    }
    cfg.accounts.push(account);
    cfg.onboarded = true;
    true
}

/// An import running on its own thread.
pub struct Job {
    pub file: PathBuf,
    /// The mailbox the mail goes to; added to the settings when done.
    pub account: AccountConfig,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn snapshot(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

/// Starts importing `file` in the background, into the database at `db`.
pub fn start(file: PathBuf, db: PathBuf, account: AccountConfig, wake: impl Fn() + Send + 'static) -> Job {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let job = Job {
        file: file.clone(),
        account: account.clone(),
        progress: progress.clone(),
        cancel: cancel.clone(),
    };
    std::thread::Builder::new()
        .name("outlook-import".into())
        .spawn(move || {
            let result = Store::open(&db).and_then(|store| run(&file, &store, &account.id, &progress, &cancel, &wake));
            if let Ok(mut p) = progress.lock() {
                p.finished = Some(result.map_err(|e| format!("{e:#}")));
            }
            wake();
        })
        .expect("thread");
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outlook::{Props, Value};

    #[test]
    fn mailboxes_get_distinct_names() {
        let mut cfg = AppConfig::default();
        let a = mailbox_account(Path::new("/tmp/archive 2019.pst"), &cfg);
        assert_eq!((a.label.as_str(), a.email.as_str(), a.is_local()), ("Outlook: archive 2019", "", true));
        cfg.accounts.push(a);
        assert_eq!(mailbox_account(Path::new("archive 2019.pst"), &cfg).label, "Outlook: archive 2019 (2)");
        let ost = mailbox_account(Path::new("jane@example.com.ost"), &cfg);
        assert_eq!((ost.label.as_str(), ost.email.as_str()), ("Outlook: jane@example.com", "jane@example.com"));
    }

    #[test]
    fn folder_names_map_to_roles() {
        assert_eq!(role_of("Inbox"), FolderRole::Inbox);
        assert_eq!(role_of("Gesendete Elemente"), FolderRole::Sent);
        assert_eq!(role_of(" Junk E-mail "), FolderRole::Junk);
        assert_eq!(role_of("Projects"), FolderRole::Custom);
    }

    #[test]
    fn summaries_read_naturally() {
        let mut p = Progress::default();
        assert_eq!(p.summary(), "Nothing to import was found");
        p.mails = 1;
        p.contacts = 12;
        assert_eq!(p.summary(), "Imported 1 message, 12 contacts");
    }

    fn text(s: &str) -> Value {
        Value::Text(s.into())
    }

    fn item(class: &str, list: Vec<(u16, Value)>) -> RawItem {
        RawItem {
            class: class.into(),
            props: Props::new(list.into_iter().collect(), 0),
            recipients: vec![],
            attachments: vec![],
        }
    }

    fn note(subject: &str, from: &str, flags: i64) -> RawItem {
        item(
            "IPM.Note",
            vec![
                (0x0037, text(subject)),
                (0x0C1F, text(from)),
                (0x1000, text("Body of the message")),
                (0x0039, Value::Time(1_794_321_000)),
                (0x0E07, Value::Int(flags)),
                (0x300B, Value::Bin(subject.as_bytes().to_vec())),
                (0x8000, Value::List(vec!["Project X".into()])),
            ],
        )
    }

    #[test]
    fn items_land_where_they_belong_and_only_once() {
        let store = Store::memory().unwrap();
        let mut named = Named::default();
        named.names.insert((convert::PS_PUBLIC_STRINGS, "Keywords".into()), 0x8000);
        named.ids.insert((convert::PSETID_APPOINTMENT, 0x820D), 0x8001);
        named.ids.insert((convert::PSETID_ADDRESS, 0x8083), 0x8002);
        let path = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let contact = || item("IPM.Contact", vec![(0x3001, text("Jane Doe")), (0x8002, text("jane@example.com"))]);
        let nameless = || item("IPM.Contact", vec![(0x3001, text("No Address"))]);
        let event = || item("IPM.Appointment", vec![(0x0037, text("Review")), (0x8001, Value::Time(1_794_321_000)), (0x300B, Value::Bin(vec![7]))]);
        let task = || item("IPM.Task", vec![(0x0037, text("Call back"))]);

        let mut importer = Importer::new(&store, "acc", &named).unwrap();
        let mut run = |importer: &mut Importer, p: &[&str], it: RawItem| importer.item(&path(p), &it).unwrap();
        assert!(matches!(run(&mut importer, &["Inbox"], note("Hello", "ann@example.com", 0x1)), Outcome::Mail));
        assert!(matches!(run(&mut importer, &["Inbox", "Clients/2024"], note("Nested", "ann@example.com", 0)), Outcome::Mail));
        assert!(matches!(run(&mut importer, &["Sent Items"], note("Mine", "me@example.com", 0x1)), Outcome::Mail));
        assert!(matches!(run(&mut importer, &["Archive", "Inbox"], note("Old", "bob@example.com", 0x1)), Outcome::Mail));
        assert!(matches!(run(&mut importer, &["Contacts"], contact()), Outcome::Contact));
        assert!(matches!(run(&mut importer, &["Contacts"], nameless()), Outcome::Contact));
        assert!(matches!(run(&mut importer, &["Calendar"], event()), Outcome::Event));
        assert!(matches!(run(&mut importer, &["Tasks"], task()), Outcome::Task));
        assert!(matches!(run(&mut importer, &["Contacts"], item("IPM.DistList", vec![])), Outcome::Skipped));
        assert_eq!(importer.senders.get("me@example.com"), Some(&1), "the owner is whoever the sent mail is from");
        assert!(importer.categories.contains("Project X"));

        let folders: Vec<_> = store.folders("acc").unwrap().into_iter().map(|f| (f.path, f.name, f.role)).collect();
        let has = |p: &str, n: &str, r: FolderRole| folders.contains(&(p.to_string(), n.to_string(), r));
        assert!(has("Inbox", "Inbox", FolderRole::Inbox), "{folders:?}");
        assert!(has("Inbox/Clients-2024", "Clients-2024", FolderRole::Custom));
        assert!(has("Sent Items", "Sent Items", FolderRole::Sent));
        assert!(has("Archive", "Archive", FolderRole::Archive));
        assert!(has("Archive/Inbox", "Inbox", FolderRole::Custom), "only top-level folders are special");
        assert_eq!(folders.len(), 5, "contact, calendar and task folders are not mail folders: {folders:?}");

        let mails = store.list_messages(&crate::store::ListQuery { limit: 50, ..Default::default() }).unwrap();
        assert_eq!(mails.len(), 4);
        let hello = mails.iter().find(|m| m.subject == "Hello").unwrap();
        assert!(hello.seen && hello.categories == ["Project X"] && hello.preview == "Body of the message");
        assert!(!mails.iter().find(|m| m.subject == "Nested").unwrap().seen);
        assert!(store.raw_body(hello.id).unwrap().is_some());

        // The same file again: nothing is duplicated outside the new mailbox.
        let mut again = Importer::new(&store, "acc2", &named).unwrap();
        assert!(matches!(run(&mut again, &["Contacts"], contact()), Outcome::Skipped));
        assert!(matches!(run(&mut again, &["Contacts"], nameless()), Outcome::Skipped));
        assert!(matches!(run(&mut again, &["Calendar"], event()), Outcome::Skipped));
        assert!(matches!(run(&mut again, &["Tasks"], task()), Outcome::Skipped));
        assert_eq!(store.contacts().unwrap().len(), 2);
        assert_eq!(store.events_between(i64::MIN / 2, i64::MAX / 2).unwrap().len(), 1);
        assert_eq!(store.tasks().unwrap().iter().filter(|t| t.title == "Call back").count(), 1);
    }

    #[test]
    fn a_mailbox_is_only_added_when_mail_came_in() {
        let store = Store::memory().unwrap();
        let mut cfg = AppConfig::default();
        let categories = cfg.categories.len();
        let account = mailbox_account(Path::new("old.pst"), &cfg);
        store.upsert_folder(&account.id, "Inbox", "Inbox", Some("/"), FolderRole::Inbox, true).unwrap();
        let contacts_only = Progress {
            contacts: 3,
            finished: Some(Ok(())),
            ..Default::default()
        };
        assert!(!apply(&mut cfg, &store, account.clone(), &contacts_only));
        assert!(cfg.accounts.is_empty() && store.folders(&account.id).unwrap().is_empty());

        let with_mail = Progress {
            mails: 2,
            owner: "me@example.com".into(),
            categories: vec!["Project X".into()],
            finished: Some(Ok(())),
            ..Default::default()
        };
        assert!(apply(&mut cfg, &store, account, &with_mail));
        assert_eq!((cfg.accounts.len(), cfg.accounts[0].email.as_str(), cfg.accounts[0].is_archive()), (1, "me@example.com", true));
        assert_eq!(cfg.categories.len(), categories + 1);
    }

    /// Imports real files: `RUSTMAIL_TEST_OUTLOOK=a.pst:b.ost cargo test outlook -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_outlook_files() {
        let files = std::env::var("RUSTMAIL_TEST_OUTLOOK").expect("RUSTMAIL_TEST_OUTLOOK");
        for file in files.split(':') {
            let store = Store::memory().unwrap();
            let progress = Mutex::new(Progress::default());
            let result = run(Path::new(file), &store, "acc", &progress, &AtomicBool::new(false), &|| {});
            let p = progress.lock().unwrap().clone();
            println!("{file}: {result:?}\n  {} (skipped {}, failed {}, of {}) owner={:?} name={:?}", p.summary(), p.skipped, p.failed, p.total, p.owner, p.name);
            for e in &p.errors {
                println!("  error: {e}");
            }
            for f in store.folders("acc").unwrap() {
                println!("  folder {:?} role={:?}", f.path, f.role);
            }
            let list = store.list_messages(&crate::store::ListQuery { limit: 50, ..Default::default() }).unwrap();
            for m in &list {
                let raw = store.raw_body(m.id).unwrap().unwrap();
                let parsed = mime::parse_full(&raw).unwrap();
                println!(
                    "  mail {:?} from {:?} <{}> to {:?} date={} seen={} flagged={} html={} text={} attachments={:?}\n    preview: {:?}",
                    m.subject,
                    m.from_name,
                    m.from_addr,
                    m.to,
                    util::fmt_full_date(m.date),
                    m.seen,
                    m.flagged,
                    parsed.html.as_ref().map(|h| h.len()).unwrap_or(0),
                    parsed.text.as_ref().map(|h| h.len()).unwrap_or(0),
                    parsed.attachments.iter().map(|a| (&a.name, a.size, a.inline)).collect::<Vec<_>>(),
                    m.preview
                );
            }
            for c in store.contacts().unwrap() {
                println!("  contact {:?} {:?} {:?} {:?}", c.name, c.emails, c.phones, c.company);
            }
            for e in store.events_between(i64::MIN / 2, i64::MAX / 2).unwrap() {
                println!("  event {:?} {} - {} all_day={} rrule={:?} at {:?}", e.title, util::fmt_full_date(e.start), util::fmt_full_date(e.end), e.all_day, e.rrule, e.location);
            }
            for t in store.tasks().unwrap() {
                println!("  task {:?} due={:?} done={}", t.title, t.due, t.completed);
            }
            assert!(result.is_ok());
        }
    }
}
