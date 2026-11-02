//! IMAP client operations on top of `async-imap`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::{Authenticator, Client, Session};
use futures::TryStreamExt;

use super::net::{self, BoxStream};
use crate::config::{Security, ServerConfig};
use crate::model::FolderRole;
use crate::util;

pub type ImapSession = Session<BoxStream>;

pub const HEADER_QUERY: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])";
pub const BODY_QUERY: &str = "(UID FLAGS BODY.PEEK[])";
const IO_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Credentials {
    pub username: String,
    pub secret: String,
    pub oauth: bool,
}

pub struct ImapConn {
    pub session: ImapSession,
    pub has_move: bool,
    pub has_uidplus: bool,
    pub has_idle: bool,
    pub selected: Option<String>,
}

struct XOAuth2 {
    user: String,
    token: String,
}

impl Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

async fn read_greeting<T>(client: &mut Client<T>) -> Result<()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    tokio::time::timeout(net::CONNECT_TIMEOUT, client.read_response())
        .await
        .map_err(|_| anyhow!("The server did not send a greeting"))??
        .ok_or_else(|| anyhow!("The server closed the connection"))?;
    Ok(())
}

fn login_error(e: async_imap::error::Error) -> anyhow::Error {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("authenticationfailed")
        || lower.contains("invalid credentials")
        || lower.contains("authentication failed")
        || lower.contains("login failed")
        || lower.starts_with("no response")
    {
        anyhow!(
            "Sign-in was rejected by the server. Check your user name and password \
             (some providers require an app password). Details: {msg}"
        )
    } else {
        anyhow!("Sign-in failed: {msg}")
    }
}

pub async fn connect(server: &ServerConfig, creds: &Credentials) -> Result<ImapConn> {
    if server.host.trim().is_empty() {
        return Err(anyhow!("No incoming (IMAP) server is configured"));
    }
    let tcp = net::connect_tcp(server.host.trim(), server.port).await?;
    let stream: BoxStream = match server.security {
        Security::Tls => Box::new(net::tls_wrap(tcp, server.host.trim(), server.accept_invalid_certs).await?),
        Security::StartTls => {
            let mut client = Client::new(tcp);
            read_greeting(&mut client).await?;
            client
                .run_command_and_check_ok("STARTTLS", None)
                .await
                .context("The server refused STARTTLS")?;
            let tcp = client.into_inner();
            Box::new(net::tls_wrap(tcp, server.host.trim(), server.accept_invalid_certs).await?)
        }
        Security::None => Box::new(tcp),
    };
    let mut client = Client::new(stream);
    if server.security != Security::StartTls {
        read_greeting(&mut client).await?;
    }
    let login = async {
        if creds.oauth {
            client
                .authenticate(
                    "XOAUTH2",
                    XOAuth2 {
                        user: creds.username.clone(),
                        token: creds.secret.clone(),
                    },
                )
                .await
                .map_err(|(e, _)| login_error(e))
        } else {
            client
                .login(&creds.username, &creds.secret)
                .await
                .map_err(|(e, _)| login_error(e))
        }
    };
    let mut session = tokio::time::timeout(net::CONNECT_TIMEOUT * 2, login)
        .await
        .map_err(|_| anyhow!("Timed out while signing in"))??;
    let caps = session.capabilities().await?;
    Ok(ImapConn {
        has_move: caps.has_str("MOVE"),
        has_uidplus: caps.has_str("UIDPLUS"),
        has_idle: caps.has_str("IDLE"),
        session,
        selected: None,
    })
}

#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub path: String,
    pub delimiter: Option<String>,
    pub selectable: bool,
    pub special: Option<FolderRole>,
}

impl ImapConn {
    pub async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>> {
        let names: Vec<_> = timeout(async {
            Ok(self
                .session
                .list(Some(""), Some("*"))
                .await?
                .try_collect::<Vec<_>>()
                .await?)
        })
        .await?;
        Ok(names
            .iter()
            .map(|n| {
                let mut selectable = true;
                let mut special = None;
                for a in n.attributes() {
                    match a {
                        NameAttribute::NoSelect => selectable = false,
                        NameAttribute::Sent => special = Some(FolderRole::Sent),
                        NameAttribute::Drafts => special = Some(FolderRole::Drafts),
                        NameAttribute::Trash => special = Some(FolderRole::Trash),
                        NameAttribute::Junk => special = Some(FolderRole::Junk),
                        NameAttribute::Archive => special = Some(FolderRole::Archive),
                        NameAttribute::All => special = Some(FolderRole::All),
                        NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent") => {
                            selectable = false
                        }
                        _ => {}
                    }
                }
                if n.name().eq_ignore_ascii_case("INBOX") {
                    special = Some(FolderRole::Inbox);
                }
                RemoteFolder {
                    path: n.name().to_string(),
                    delimiter: n.delimiter().map(|d| d.to_string()),
                    selectable,
                    special,
                }
            })
            .collect())
    }

    pub async fn select(&mut self, path: &str) -> Result<async_imap::types::Mailbox> {
        let mb = timeout(async { Ok(self.session.select(path).await?) })
            .await
            .with_context(|| format!("Could not open folder \"{}\"", util::decode_imap_utf7(path)))?;
        self.selected = Some(path.to_string());
        Ok(mb)
    }

    async fn ensure_selected(&mut self, path: &str) -> Result<()> {
        if self.selected.as_deref() != Some(path) {
            self.select(path).await?;
        }
        Ok(())
    }

    pub async fn fetch_seq(&mut self, set: &str, query: &str) -> Result<Vec<Fetch>> {
        timeout(async {
            Ok(self
                .session
                .fetch(set, query)
                .await?
                .try_collect::<Vec<_>>()
                .await?)
        })
        .await
    }

    pub async fn fetch_uid(&mut self, set: &str, query: &str) -> Result<Vec<Fetch>> {
        timeout(async {
            Ok(self
                .session
                .uid_fetch(set, query)
                .await?
                .try_collect::<Vec<_>>()
                .await?)
        })
        .await
    }

    pub async fn uid_search(&mut self, query: &str) -> Result<Vec<u32>> {
        let set = timeout(async { Ok(self.session.uid_search(query).await?) }).await?;
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    pub async fn store_flag(&mut self, path: &str, uids: &[i64], flag: &str, add: bool) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(path).await?;
        let set = uid_set(uids);
        let query = format!("{}FLAGS.SILENT ({})", if add { "+" } else { "-" }, flag);
        timeout(async {
            let _: Vec<Fetch> = self
                .session
                .uid_store(&set, &query)
                .await?
                .try_collect()
                .await?;
            Ok(())
        })
        .await
    }

    async fn expunge_uids(&mut self, uids: &[i64]) -> Result<()> {
        let set = uid_set(uids);
        if self.has_uidplus {
            timeout(async {
                let _: Vec<_> = self.session.uid_expunge(&set).await?.try_collect().await?;
                Ok(())
            })
            .await
        } else {
            timeout(async {
                let _: Vec<_> = self.session.expunge().await?.try_collect().await?;
                Ok(())
            })
            .await
        }
    }

    pub async fn move_uids(&mut self, src: &str, uids: &[i64], dest: &str) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(src).await?;
        let set = uid_set(uids);
        if self.has_move {
            timeout(async { Ok(self.session.uid_mv(&set, dest).await?) }).await
        } else {
            timeout(async { Ok(self.session.uid_copy(&set, dest).await?) }).await?;
            self.store_flag(src, uids, "\\Deleted", true).await?;
            self.expunge_uids(uids).await
        }
    }

    pub async fn delete_uids(&mut self, path: &str, uids: &[i64]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.store_flag(path, uids, "\\Deleted", true).await?;
        self.expunge_uids(uids).await
    }

    /// APPEND that tolerates unsolicited responses (e.g. `* 5 EXISTS`)
    /// arriving before the server's continuation request.
    pub async fn append(&mut self, path: &str, flags: &str, raw: &[u8]) -> Result<()> {
        use async_imap::imap_proto::{Response, Status};
        use tokio::io::AsyncWriteExt;
        let name = util::decode_imap_utf7(path);
        timeout(async {
            let quoted = format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""));
            let id = self
                .session
                .run_command(&format!("APPEND {quoted} {flags} {{{}}}", raw.len()))
                .await?;
            loop {
                let resp = self
                    .session
                    .read_response()
                    .await?
                    .ok_or_else(|| anyhow!("The server closed the connection"))?;
                match resp.parsed() {
                    Response::Continue { .. } => break,
                    Response::Done { tag, outcome, .. } if *tag == id => {
                        return Err(anyhow!(
                            "The server refused the message: {}",
                            outcome.information.as_deref().unwrap_or("no reason given")
                        ));
                    }
                    _ => {} // untagged data, keep waiting
                }
            }
            let stream = self.session.as_mut();
            stream.write_all(raw).await?;
            stream.write_all(b"\r\n").await?;
            stream.flush().await?;
            loop {
                let resp = self
                    .session
                    .read_response()
                    .await?
                    .ok_or_else(|| anyhow!("The server closed the connection"))?;
                if let Response::Done { tag, status, outcome, .. } = resp.parsed() {
                    if *tag == id {
                        return match status {
                            Status::Ok => Ok(()),
                            _ => Err(anyhow!(
                                "The server refused the message: {}",
                                outcome.information.as_deref().unwrap_or("no reason given")
                            )),
                        };
                    }
                }
            }
        })
        .await
        .with_context(|| format!("Could not save to \"{name}\""))
    }

    pub async fn create_folder(&mut self, path: &str) -> Result<()> {
        timeout(async { Ok(self.session.create(path).await?) }).await?;
        let _ = self.session.subscribe(path).await;
        Ok(())
    }

    pub async fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        if self.selected.as_deref() == Some(from) {
            let _ = self.session.close().await;
            self.selected = None;
        }
        timeout(async { Ok(self.session.rename(from, to).await?) }).await
    }

    pub async fn delete_folder(&mut self, path: &str) -> Result<()> {
        if self.selected.as_deref() == Some(path) {
            let _ = self.session.close().await;
            self.selected = None;
        }
        timeout(async { Ok(self.session.delete(path).await?) }).await
    }

    pub async fn noop(&mut self) -> Result<()> {
        timeout(async { Ok(self.session.noop().await?) }).await
    }

    /// Waits in IMAP IDLE on `path` until the server reports a change, the
    /// timeout passes, or `interrupt` resolves. Returns the reconnected
    /// connection and whether the server reported new data.
    pub async fn idle<F>(mut self, path: &str, timeout: Duration, interrupt: F) -> Result<(ImapConn, IdleEnd)>
    where
        F: std::future::Future<Output = ()>,
    {
        if self.selected.as_deref() != Some(path) {
            self.select(path).await?;
        }
        let ImapConn { session, has_move, has_uidplus, has_idle, .. } = self;
        let mut handle = session.idle();
        handle.init().await?;
        let end = {
            let (wait, stop) = handle.wait_with_timeout(timeout);
            tokio::select! {
                r = wait => match r? {
                    async_imap::extensions::idle::IdleResponse::NewData(_) => IdleEnd::Changed,
                    _ => IdleEnd::Timeout,
                },
                _ = interrupt => {
                    drop(stop);
                    IdleEnd::Interrupted
                }
            }
        };
        let session = tokio::time::timeout(Duration::from_secs(30), handle.done())
            .await
            .map_err(|_| anyhow!("The server did not end IDLE"))??;
        Ok((
            ImapConn {
                session,
                has_move,
                has_uidplus,
                has_idle,
                selected: Some(path.to_string()),
            },
            end,
        ))
    }

    pub fn drain_unsolicited(&mut self) {
        while self.session.unsolicited_responses.try_recv().is_ok() {}
    }

    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.session.logout()).await;
    }
}

async fn timeout<T>(fut: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(IO_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow!("The mail server stopped responding"))?
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEnd {
    Changed,
    Timeout,
    Interrupted,
}

/// Server flags of a fetch result.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub forwarded: bool,
}

pub fn flags_of(f: &Fetch) -> Flags {
    let mut out = Flags::default();
    for flag in f.flags() {
        match flag {
            Flag::Seen => out.seen = true,
            Flag::Flagged => out.flagged = true,
            Flag::Answered => out.answered = true,
            Flag::Draft => out.draft = true,
            Flag::Custom(c) if c.eq_ignore_ascii_case("$Forwarded") => out.forwarded = true,
            _ => {}
        }
    }
    out
}

/// Compresses uids into an IMAP sequence set such as `1:4,7,9:12`.
pub fn uid_set(uids: &[i64]) -> String {
    let mut v: Vec<i64> = uids.iter().copied().filter(|u| *u > 0).collect();
    v.sort_unstable();
    v.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    parts.join(",")
}

/// Assigns folder roles: special-use attributes first, then well-known names.
pub fn assign_roles(folders: &mut [RemoteFolder]) -> HashMap<String, FolderRole> {
    let mut roles: HashMap<String, FolderRole> = HashMap::new();
    let mut taken: Vec<FolderRole> = Vec::new();
    for f in folders.iter() {
        if let Some(r) = f.special {
            if f.selectable && !taken.contains(&r) {
                roles.insert(f.path.clone(), r);
                taken.push(r);
            }
        }
    }
    const NAMES: &[(FolderRole, &[&str])] = &[
        (
            FolderRole::Sent,
            &["sent", "sent items", "sent mail", "sent messages", "gesendet", "gesendete elemente", "gesendete objekte", "envoyés", "éléments envoyés", "enviados", "posta inviata", "verzonden items", "skickat"],
        ),
        (
            FolderRole::Drafts,
            &["drafts", "draft", "entwürfe", "brouillons", "borradores", "bozze", "concepten", "utkast"],
        ),
        (
            FolderRole::Trash,
            &["trash", "deleted", "deleted items", "deleted messages", "bin", "papierkorb", "gelöschte elemente", "corbeille", "éléments supprimés", "papelera", "cestino", "prullenbak", "papperskorg"],
        ),
        (
            FolderRole::Junk,
            &["junk", "spam", "junk e-mail", "junk email", "junk mail", "bulk mail", "spamverdacht", "courrier indésirable", "correo no deseado"],
        ),
        (FolderRole::Archive, &["archive", "archives", "archiv", "archivio", "archief"]),
    ];
    for (role, names) in NAMES {
        if taken.contains(role) {
            continue;
        }
        // Prefer top-level folders, then INBOX.<name> style children.
        let mut best: Option<(&RemoteFolder, usize)> = None;
        for f in folders.iter().filter(|f| f.selectable && !roles.contains_key(&f.path)) {
            let decoded = util::decode_imap_utf7(&f.path).to_lowercase();
            let leaf = match f.delimiter.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => decoded.rsplit(d).next().unwrap_or(&decoded).to_string(),
                None => decoded.clone(),
            };
            if names.contains(&leaf.as_str()) {
                let depth = f
                    .delimiter
                    .as_deref()
                    .filter(|d| !d.is_empty())
                    .map(|d| decoded.matches(d).count())
                    .unwrap_or(0);
                if best.map(|(_, bd)| depth < bd).unwrap_or(true) {
                    best = Some((f, depth));
                }
            }
        }
        if let Some((f, _)) = best {
            roles.insert(f.path.clone(), *role);
            taken.push(*role);
        }
    }
    for f in folders.iter() {
        if f.path.eq_ignore_ascii_case("INBOX") {
            roles.insert(f.path.clone(), FolderRole::Inbox);
        }
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_sets() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10, 12]), "1:3,5,9:10,12");
        assert_eq!(uid_set(&[7]), "7");
        assert_eq!(uid_set(&[-3, 4]), "4");
    }

    #[test]
    fn roles_by_name() {
        let mk = |p: &str, special: Option<FolderRole>| RemoteFolder {
            path: p.into(),
            delimiter: Some("/".into()),
            selectable: true,
            special,
        };
        let mut f = vec![
            mk("INBOX", None),
            mk("Sent Items", None),
            mk("INBOX/Sent", None),
            mk("Deleted Items", None),
            mk("[Gmail]/Spam", Some(FolderRole::Junk)),
            mk("Projects", None),
        ];
        let r = assign_roles(&mut f);
        assert_eq!(r.get("Sent Items"), Some(&FolderRole::Sent));
        assert_eq!(r.get("INBOX/Sent"), None);
        assert_eq!(r.get("Deleted Items"), Some(&FolderRole::Trash));
        assert_eq!(r.get("[Gmail]/Spam"), Some(&FolderRole::Junk));
        assert_eq!(r.get("INBOX"), Some(&FolderRole::Inbox));
        assert_eq!(r.get("Projects"), None);
    }
}
