//! Message composer: inline in the reading pane or in its own window.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text_selection::CCursorRange;
use egui::widgets::text_edit::TextEditState;
use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, KeyboardShortcut,
    Layout, Margin, Modifiers, Pos2, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::app::{App, ToastAction, ToastKind};
use crate::engine::{Command, MsgRef};
use crate::icons;
use crate::mime::{self, OutAttachment, OutgoingMail};
use crate::model::*;
use crate::store::Store;
use crate::theme::{font, font_semibold, icon_font, mix, pal};
use crate::ui::html::{self, HtmlDoc};
use crate::ui::widgets;
use crate::util;

pub struct Quoted {
    pub header_text: String,
    pub header_html: String,
    pub html: Option<String>,
    pub text: String,
    pub doc: Arc<HtmlDoc>,
    pub expanded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fmt {
    Bold,
    Italic,
    Underline,
    Strike,
    Bullet,
    Numbered,
    Quote,
    Heading,
    Link,
    Code,
}

pub struct Compose {
    pub id: u64,
    pub account_id: String,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub inputs: [String; 3],
    pub show_cc: bool,
    pub show_bcc: bool,
    pub subject: String,
    pub body: String,
    pub quoted: Option<Quoted>,
    pub attachments: Vec<OutAttachment>,
    pub importance: Importance,
    pub read_receipt: bool,
    pub html: bool,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub reply_to_msg: Option<MsgId>,
    pub forward_of: Option<MsgId>,
    pub draft_of: Option<MsgRef>,
    pub popped_out: bool,
    pub preview: bool,
    pub preview_cache: Option<(u64, Arc<HtmlDoc>)>,
    pub suggestions: Vec<(String, String)>,
    pub suggest_for: Option<Field>,
    pub suggest_index: usize,
    pub waiting_for: Option<(MsgId, Waiting)>,
    pub initial_hash: u64,
    pub focus_to: bool,
    pub focus_body: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Waiting {
    Reply { all: bool },
    Forward,
    Draft,
}

enum Outcome {
    None,
    Send,
    Discard,
    Close,
    PopOut,
    SaveDraft,
}

impl Compose {
    pub fn new(id: u64, account_id: &str, html: bool) -> Compose {
        Compose {
            id,
            account_id: account_id.to_string(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            inputs: Default::default(),
            show_cc: false,
            show_bcc: false,
            subject: String::new(),
            body: String::new(),
            quoted: None,
            attachments: Vec::new(),
            importance: Importance::Normal,
            read_receipt: false,
            html,
            in_reply_to: None,
            references: Vec::new(),
            reply_to_msg: None,
            forward_of: None,
            draft_of: None,
            popped_out: false,
            preview: false,
            preview_cache: None,
            suggestions: Vec::new(),
            suggest_for: None,
            suggest_index: 0,
            waiting_for: None,
            initial_hash: 0,
            focus_to: true,
            focus_body: false,
        }
    }

    fn content_hash(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.to.hash(&mut h);
        self.cc.hash(&mut h);
        self.bcc.hash(&mut h);
        self.inputs.hash(&mut h);
        self.subject.hash(&mut h);
        self.body.hash(&mut h);
        self.attachments.len().hash(&mut h);
        h.finish()
    }

    pub fn mark_clean(&mut self) {
        self.initial_hash = self.content_hash();
    }

    pub fn is_dirty(&self) -> bool {
        self.content_hash() != self.initial_hash
    }

    pub fn title(&self) -> String {
        if self.subject.trim().is_empty() {
            "(No subject)".into()
        } else {
            self.subject.trim().to_string()
        }
    }

    fn list_mut(&mut self, f: Field) -> &mut Vec<Addr> {
        match f {
            Field::To => &mut self.to,
            Field::Cc => &mut self.cc,
            Field::Bcc => &mut self.bcc,
        }
    }

    fn commit_input(&mut self, f: Field) {
        let idx = f as usize;
        let text = std::mem::take(&mut self.inputs[idx]);
        let addrs = Addr::parse_list(&text);
        let list = self.list_mut(f);
        for a in addrs {
            if !a.email.is_empty() && !list.iter().any(|x| x.email.eq_ignore_ascii_case(&a.email)) {
                list.push(a);
            }
        }
    }

    fn commit_all(&mut self) {
        for f in [Field::To, Field::Cc, Field::Bcc] {
            self.commit_input(f);
        }
    }

    /// Fills quoted text / attachments once the original body is available.
    pub fn on_body_ready(&mut self, store: &Store, id: MsgId) {
        let Some((wid, what)) = self.waiting_for else { return };
        if wid != id {
            return;
        }
        let Ok(Some(raw)) = store.raw_body(id) else { return };
        let Some(parsed) = mime::parse_full(&raw) else { return };
        let Ok(Some(m)) = store.message(id) else { return };
        self.waiting_for = None;
        match what {
            Waiting::Reply { .. } => {
                self.quoted = Some(make_quote(&m, &parsed, false));
                if self.in_reply_to.is_none() {
                    self.in_reply_to = Some(parsed.message_id.clone()).filter(|s| !s.is_empty());
                }
            }
            Waiting::Forward => {
                self.quoted = Some(make_quote(&m, &parsed, true));
                for a in parsed.attachments.iter().filter(|a| !a.inline) {
                    if let Some(data) = mime::part_bytes(&raw, a.part) {
                        self.attachments.push(OutAttachment {
                            name: a.name.clone(),
                            mime: a.mime.clone(),
                            data,
                        });
                    }
                }
            }
            Waiting::Draft => {
                fill_from_draft(self, &parsed, &raw);
            }
        }
        self.mark_clean();
    }
}

fn fill_from_draft(c: &mut Compose, parsed: &ParsedMessage, raw: &[u8]) {
    c.to = parsed.to.clone();
    c.cc = parsed.cc.clone();
    c.bcc = parsed.bcc.clone();
    c.show_cc = !c.cc.is_empty();
    c.show_bcc = !c.bcc.is_empty();
    c.subject = parsed.subject.clone();
    c.body = parsed.text.clone().unwrap_or_else(|| {
        parsed
            .html
            .as_deref()
            .map(mail_parser::decoders::html::html_to_text)
            .unwrap_or_default()
    });
    c.importance = parsed.importance;
    c.in_reply_to = Some(parsed.in_reply_to.clone()).filter(|s| !s.is_empty());
    c.references = parsed.references.clone();
    for a in parsed.attachments.iter().filter(|a| !a.inline) {
        if let Some(data) = mime::part_bytes(raw, a.part) {
            c.attachments.push(OutAttachment {
                name: a.name.clone(),
                mime: a.mime.clone(),
                data,
            });
        }
    }
}

fn body_inner_html(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(start) = lower.find("<body") {
        if let Some(gt) = lower[start..].find('>') {
            let from = start + gt + 1;
            let to = lower.rfind("</body>").unwrap_or(html.len());
            if to > from {
                return html[from..to].to_string();
            }
        }
    }
    html.to_string()
}

fn make_quote(m: &MessageSummary, p: &ParsedMessage, forward: bool) -> Quoted {
    let from = p.from.first().map(|a| a.to_header()).unwrap_or_else(|| m.sender().to_string());
    let sent = util::local(if p.date > 0 { p.date } else { m.date });
    let sent_s = format!("{} {}", sent.format("%A, %B %-d, %Y"), util::fmt_time(&sent));
    let to = Addr::list_to_string(&p.to);
    let cc = Addr::list_to_string(&p.cc);
    let mut header_text = String::new();
    if forward {
        header_text.push_str("-------- Forwarded message --------\n");
    }
    header_text.push_str(&format!("From: {from}\nSent: {sent_s}\nTo: {to}\n"));
    if !cc.is_empty() {
        header_text.push_str(&format!("Cc: {cc}\n"));
    }
    header_text.push_str(&format!("Subject: {}\n", p.subject));
    let esc = mime::escape_html;
    let mut header_html = format!(
        "<b>From:</b> {}<br><b>Sent:</b> {}<br><b>To:</b> {}<br>",
        esc(&from),
        esc(&sent_s),
        esc(&to)
    );
    if !cc.is_empty() {
        header_html.push_str(&format!("<b>Cc:</b> {}<br>", esc(&cc)));
    }
    header_html.push_str(&format!("<b>Subject:</b> {}", esc(&p.subject)));
    let text = mime::plain_text(p);
    let doc = match &p.html {
        Some(h) => html::parse_html(h),
        None => html::from_plain_text(&text),
    };
    Quoted {
        header_text,
        header_html,
        html: p.html.as_ref().map(|h| body_inner_html(h)),
        text,
        doc: Arc::new(doc),
        expanded: false,
    }
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

fn apply_format(ctx: &egui::Context, id: Id, body: &mut String, fmt: Fmt) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    let total = body.chars().count();
    let (a, b) = state
        .cursor
        .char_range()
        .map(|r| {
            let (x, y) = (r.primary.index.0, r.secondary.index.0);
            (x.min(y), x.max(y))
        })
        .unwrap_or((total, total));
    let (ba, bb) = (byte_index(body, a), byte_index(body, b));
    let selected = body[ba..bb].to_string();
    let (new_text, sel_start, sel_end) = match fmt {
        Fmt::Bold | Fmt::Italic | Fmt::Strike | Fmt::Code | Fmt::Underline => {
            let (l, r) = match fmt {
                Fmt::Bold => ("**", "**"),
                Fmt::Italic => ("*", "*"),
                Fmt::Strike => ("~~", "~~"),
                Fmt::Code => ("`", "`"),
                _ => ("<u>", "</u>"),
            };
            let inner = if selected.is_empty() { "text".to_string() } else { selected.clone() };
            let s = format!("{l}{inner}{r}");
            let start = a + l.chars().count();
            (s, start, start + inner.chars().count())
        }
        Fmt::Link => {
            let inner = if selected.is_empty() { "link text".to_string() } else { selected.clone() };
            let s = format!("[{inner}](https://)");
            let url_start = a + inner.chars().count() + 3;
            (s, url_start, url_start + 8)
        }
        Fmt::Bullet | Fmt::Numbered | Fmt::Quote | Fmt::Heading => {
            // Operate on whole lines.
            let line_start = body[..ba].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = body[bb..].find('\n').map(|i| bb + i).unwrap_or(body.len());
            let block = body[line_start..line_end].to_string();
            let out: Vec<String> = block
                .lines()
                .enumerate()
                .map(|(i, l)| match fmt {
                    Fmt::Bullet => format!("- {l}"),
                    Fmt::Numbered => format!("{}. {l}", i + 1),
                    Fmt::Quote => format!("> {l}"),
                    _ => format!("## {l}"),
                })
                .collect();
            let joined = if out.is_empty() {
                match fmt {
                    Fmt::Bullet => "- ".to_string(),
                    Fmt::Numbered => "1. ".to_string(),
                    Fmt::Quote => "> ".to_string(),
                    _ => "## ".to_string(),
                }
            } else {
                out.join("\n")
            };
            let start_chars = body[..line_start].chars().count();
            let end = start_chars + joined.chars().count();
            body.replace_range(line_start..line_end, &joined);
            state.cursor.set_char_range(Some(CCursorRange::two(
                egui::text::CCursor::new(end),
                egui::text::CCursor::new(end),
            )));
            state.store(ctx, id);
            ctx.memory_mut(|m| m.request_focus(id));
            return;
        }
    };
    body.replace_range(ba..bb, &new_text);
    state.cursor.set_char_range(Some(CCursorRange::two(
        egui::text::CCursor::new(sel_start),
        egui::text::CCursor::new(sel_end),
    )));
    state.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

impl App {
    fn compose_account(&self) -> String {
        let current = self
            .mail
            .current_folder_id()
            .and_then(|id| self.mail.find_folder(id))
            .map(|f| f.account_id.clone());
        current
            .filter(|id| self.cfg.account(id).map(|a| a.enabled).unwrap_or(false))
            .or_else(|| self.cfg.default_account().map(|a| a.id.clone()))
            .unwrap_or_default()
    }

    fn signature_for(&self, account_id: &str, reply: bool) -> String {
        match self.cfg.account(account_id) {
            Some(a) if !a.signature.trim().is_empty() && (!reply || a.signature_on_reply) => {
                format!("\n\n{}", a.signature.trim_end())
            }
            _ => String::new(),
        }
    }

    fn open_compose(&mut self, mut c: Compose) {
        c.mark_clean();
        self.module = crate::app::Module::Mail;
        if self.cfg.compose.pop_out || self.mail.compose_inline.is_some() || self.cfg.mail.reading_pane == crate::config::ReadingPanePos::Off && false {
            c.popped_out = true;
            self.composes.push(c);
        } else {
            self.mail.compose_inline = Some(c);
        }
    }

    pub fn new_compose(&mut self) {
        let account = self.compose_account();
        if account.is_empty() {
            self.toast("Add an account before writing a message", ToastKind::Error);
            self.wizard = Some(crate::ui::wizard::WizardState::new(true));
            return;
        }
        let id = self.next_id();
        let mut c = Compose::new(id, &account, self.cfg.compose.html);
        c.body = self.signature_for(&account, false);
        c.read_receipt = self.cfg.compose.read_receipt;
        self.open_compose(c);
    }

    pub fn compose_to(&mut self, addr: &str) {
        let (addr, query) = addr.split_once('?').unwrap_or((addr, ""));
        self.new_compose();
        if let Some(c) = self.mail.compose_inline.as_mut().or(self.composes.last_mut()) {
            c.to = Addr::parse_list(&addr.replace("%40", "@"));
            for kv in query.split('&') {
                if let Some((k, v)) = kv.split_once('=') {
                    let v = v.replace("%20", " ").replace('+', " ");
                    match k.to_ascii_lowercase().as_str() {
                        "subject" => c.subject = v,
                        "body" => c.body = format!("{v}{}", c.body),
                        "cc" => c.cc = Addr::parse_list(&v),
                        _ => {}
                    }
                }
            }
            c.focus_to = false;
            c.focus_body = true;
        }
    }

    pub fn reply(&mut self, id: MsgId, all: bool) {
        let Ok(Some(m)) = self.store.message(id) else { return };
        let all = all || false;
        let mine: Vec<String> = self.my_addresses();
        let parsed = self.store.raw_body(id).ok().flatten().and_then(|r| mime::parse_full(&r));
        let account = if self.cfg.account(&m.account_id).is_some() { m.account_id.clone() } else { self.compose_account() };
        let cid = self.next_id();
        let mut c = Compose::new(cid, &account, self.cfg.compose.html);
        let sent_by_me = mine.contains(&m.from_addr.to_lowercase());
        let (reply_to, to_list, cc_list) = match &parsed {
            Some(p) => (
                if !p.reply_to.is_empty() { p.reply_to.clone() } else { p.from.clone() },
                p.to.clone(),
                p.cc.clone(),
            ),
            None => (
                vec![Addr::new(m.from_name.clone(), m.from_addr.clone())],
                Addr::parse_list(&m.to),
                Addr::parse_list(&m.cc),
            ),
        };
        if sent_by_me {
            // Replying to something I sent goes to the original recipients.
            c.to = to_list.clone();
        } else {
            c.to = reply_to;
        }
        if all {
            let mut cc: Vec<Addr> = Vec::new();
            let extra = if sent_by_me { cc_list.clone() } else { [to_list.clone(), cc_list.clone()].concat() };
            for a in extra {
                let e = a.email.to_lowercase();
                if mine.contains(&e) || c.to.iter().any(|x| x.email.eq_ignore_ascii_case(&e)) || cc.iter().any(|x| x.email.eq_ignore_ascii_case(&e)) {
                    continue;
                }
                cc.push(a);
            }
            c.show_cc = !cc.is_empty();
            c.cc = cc;
        }
        c.subject = util::reply_subject(&m.subject);
        c.body = self.signature_for(&account, true);
        c.reply_to_msg = Some(id);
        match &parsed {
            Some(p) => {
                c.in_reply_to = Some(p.message_id.clone()).filter(|s| !s.is_empty());
                let mut refs = p.references.clone();
                if !p.message_id.is_empty() {
                    refs.push(p.message_id.clone());
                }
                c.references = refs;
                c.quoted = Some(make_quote(&m, p, false));
            }
            None => {
                c.in_reply_to = Some(m.message_id.clone()).filter(|s| !s.is_empty());
                c.references = c.in_reply_to.iter().cloned().collect();
                c.waiting_for = Some((id, Waiting::Reply { all }));
                self.engine.send(Command::FetchBody { msg_id: id });
            }
        }
        c.focus_to = false;
        c.focus_body = true;
        self.open_compose(c);
    }

    pub fn forward(&mut self, id: MsgId) {
        let Ok(Some(m)) = self.store.message(id) else { return };
        let raw = self.store.raw_body(id).ok().flatten();
        let parsed = raw.as_deref().and_then(mime::parse_full);
        let account = if self.cfg.account(&m.account_id).is_some() { m.account_id.clone() } else { self.compose_account() };
        let cid = self.next_id();
        let mut c = Compose::new(cid, &account, self.cfg.compose.html);
        c.subject = util::forward_subject(&m.subject);
        c.body = self.signature_for(&account, true);
        c.forward_of = Some(id);
        match (&parsed, &raw) {
            (Some(p), Some(raw)) => {
                c.quoted = Some(make_quote(&m, p, true));
                for a in p.attachments.iter().filter(|a| !a.inline) {
                    if let Some(data) = mime::part_bytes(raw, a.part) {
                        c.attachments.push(OutAttachment { name: a.name.clone(), mime: a.mime.clone(), data });
                    }
                }
            }
            _ => {
                c.waiting_for = Some((id, Waiting::Forward));
                self.engine.send(Command::FetchBody { msg_id: id });
            }
        }
        self.open_compose(c);
    }

    pub fn edit_draft(&mut self, id: MsgId) {
        let Ok(Some(m)) = self.store.message(id) else { return };
        let cid = self.next_id();
        let mut c = Compose::new(cid, &m.account_id, self.cfg.compose.html);
        c.draft_of = Some(MsgRef::of(&m));
        c.subject = m.subject.clone();
        match self.store.raw_body(id).ok().flatten() {
            Some(raw) => {
                if let Some(p) = mime::parse_full(&raw) {
                    fill_from_draft(&mut c, &p, &raw);
                }
            }
            None => {
                c.waiting_for = Some((id, Waiting::Draft));
                self.engine.send(Command::FetchBody { msg_id: id });
            }
        }
        c.focus_to = false;
        c.focus_body = true;
        self.open_compose(c);
    }

    pub fn undo_send(&mut self, outbox_id: i64) {
        let removed = self
            .store
            .conn
            .execute("DELETE FROM outbox WHERE id = ?1 AND sending = 0", [outbox_id])
            .unwrap_or(0);
        if removed == 0 {
            self.toast("Too late — the message was already sent", ToastKind::Error);
            return;
        }
        if let Some(c) = self.unsent.remove(&outbox_id) {
            self.mail.compose_inline = Some(c);
            self.module = crate::app::Module::Mail;
        }
        self.toast("Sending canceled", ToastKind::Info);
    }

    fn build_mail(&self, c: &Compose, for_draft: bool) -> anyhow::Result<mime::BuiltMail> {
        let acc = self
            .cfg
            .account(&c.account_id)
            .ok_or_else(|| anyhow::anyhow!("Choose an account to send from"))?;
        let mut text = c.body.clone();
        let mut html_body = if c.html { Some(mime::markdown_to_html(&c.body)) } else { None };
        if let Some(q) = &c.quoted {
            text.push_str("\n\n");
            text.push_str(&q.header_text);
            text.push('\n');
            for line in q.text.lines() {
                text.push_str("> ");
                text.push_str(line);
                text.push('\n');
            }
            if let Some(h) = html_body.as_mut() {
                let original = match &q.html {
                    Some(o) => o.clone(),
                    None => format!("<div style=\"white-space:pre-wrap\">{}</div>", mime::escape_html(&q.text)),
                };
                h.push_str(&format!(
                    "<div style=\"border-top:1px solid #E1E1E1;padding-top:8px;margin-top:18px;font-size:10.5pt;color:#444\">{}</div>\
                     <blockquote style=\"margin:8px 0 0 0;padding-left:10px;border-left:3px solid #D1D1D1\">{}</blockquote>",
                    q.header_html, original
                ));
            }
        }
        let mail = OutgoingMail {
            from: Addr::new(acc.display_name.clone(), acc.email.clone()),
            to: c.to.clone(),
            cc: c.cc.clone(),
            bcc: c.bcc.clone(),
            subject: c.subject.clone(),
            text,
            html: html_body.map(|h| mime::wrap_html_document(&h)),
            attachments: c.attachments.clone(),
            in_reply_to: c.in_reply_to.clone(),
            references: c.references.clone(),
            importance: c.importance,
            read_receipt: c.read_receipt && !for_draft,
            keep_bcc: for_draft,
        };
        mime::build(&mail)
    }

    fn send_compose(&mut self, mut c: Compose) -> Result<(), Compose> {
        c.commit_all();
        if c.to.is_empty() && c.cc.is_empty() && c.bcc.is_empty() {
            self.toast("Add at least one recipient", ToastKind::Error);
            return Err(c);
        }
        if let Some(bad) = c.to.iter().chain(&c.cc).chain(&c.bcc).find(|a| !util::is_valid_email(&a.email)) {
            self.toast(format!("\"{}\" isn't a valid e-mail address", bad.email), ToastKind::Error);
            return Err(c);
        }
        if c.waiting_for.is_some() {
            self.toast("Still downloading the original message, try again in a moment", ToastKind::Error);
            return Err(c);
        }
        let built = match self.build_mail(&c, false) {
            Ok(b) => b,
            Err(e) => {
                self.toast(format!("Couldn't prepare the message: {e}"), ToastKind::Error);
                return Err(c);
            }
        };
        let acc_email = self.cfg.account(&c.account_id).map(|a| a.email.clone()).unwrap_or_default();
        let delay = self.cfg.compose.undo_send_s as i64;
        let item = OutboxItem {
            id: 0,
            account_id: c.account_id.clone(),
            raw: built.raw,
            from: acc_email,
            recipients: built.recipients,
            send_at: util::now_ts() + delay,
            attempts: 0,
            last_error: None,
            subject: c.subject.clone(),
            reply_to_msg: c.reply_to_msg,
            forward_of: c.forward_of,
            draft_msg: c.draft_of.map(|d| d.id),
        };
        match self.store.add_outbox(&item) {
            Ok(outbox_id) => {
                if self.cfg.mail.collect_contacts {
                    for a in c.to.iter().chain(&c.cc).chain(&c.bcc) {
                        let _ = self.store.touch_contact(&a.name, &a.email);
                    }
                    self.people.dirty = true;
                }
                if delay > 0 {
                    self.toast_with("Sending…", ToastKind::Info, Some(ToastAction::UndoSend(outbox_id)));
                    if let Some(t) = self.toasts.last_mut() {
                        t.ttl = delay as f64 + 0.5;
                    }
                } else {
                    self.engine.send(Command::ProcessOutbox);
                }
                self.unsent.insert(outbox_id, c);
                Ok(())
            }
            Err(e) => {
                self.toast(format!("Couldn't queue the message: {e}"), ToastKind::Error);
                Err(c)
            }
        }
    }

    fn save_draft(&mut self, c: &mut Compose, quiet: bool) {
        c.commit_all();
        match self.build_mail(c, true) {
            Ok(built) => {
                self.engine.send(Command::SaveDraft {
                    account_id: c.account_id.clone(),
                    raw: built.raw,
                    replace: c.draft_of.take(),
                });
                c.mark_clean();
                if !quiet {
                    self.toast("Saving draft…", ToastKind::Info);
                }
            }
            Err(e) => {
                self.toast(format!("Couldn't save draft: {e}"), ToastKind::Error);
            }
        }
    }

    fn handle_outcome(&mut self, mut c: Compose, outcome: Outcome) -> Option<Compose> {
        match outcome {
            Outcome::None => Some(c),
            Outcome::Send => match self.send_compose(c) {
                Ok(()) => None,
                Err(c) => Some(c),
            },
            Outcome::SaveDraft => {
                self.save_draft(&mut c, false);
                Some(c)
            }
            Outcome::Close => {
                if c.is_dirty() {
                    self.save_draft(&mut c, true);
                    self.toast("Draft saved to Drafts", ToastKind::Success);
                }
                None
            }
            Outcome::Discard => {
                if let Some(d) = c.draft_of {
                    let acc = c.account_id.clone();
                    let _ = self.store.delete_messages(&[d.id]);
                    self.engine.send(Command::Delete { account_id: acc, items: vec![d], permanent: true });
                    self.mail.list_dirty = true;
                }
                None
            }
            Outcome::PopOut => {
                c.popped_out = true;
                self.composes.push(c);
                None
            }
        }
    }

    pub fn compose_inline_ui(&mut self, ui: &mut Ui) {
        let Some(mut c) = self.mail.compose_inline.take() else { return };
        let outcome = self.compose_form(ui, &mut c);
        if let Some(c) = self.handle_outcome(c, outcome) {
            if c.popped_out {
                self.composes.push(c);
            } else if self.mail.compose_inline.is_none() {
                self.mail.compose_inline = Some(c);
            } else {
                // Another compose was opened meanwhile; keep this one in a window.
                let mut c = c;
                c.popped_out = true;
                self.composes.push(c);
            }
        }
    }

    pub fn compose_windows(&mut self, ctx: &egui::Context) {
        if self.composes.is_empty() {
            return;
        }
        let list = std::mem::take(&mut self.composes);
        let mut keep = Vec::new();
        for mut c in list {
            let vid = egui::ViewportId::from_hash_of(("compose", c.id));
            let title = format!("{} — RustMail", c.title());
            let mut outcome = Outcome::None;
            ctx.show_viewport_immediate(
                vid,
                egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([860.0, 720.0])
                    .with_min_inner_size([520.0, 420.0]),
                |ui, _class| {
                    let p = pal();
                    egui::CentralPanel::no_frame()
                        .frame(Frame::NONE.fill(p.card))
                        .show(ui, |ui| {
                            outcome = self.compose_form(ui, &mut c);
                        });
                    if ui.input(|i| i.viewport().close_requested()) {
                        outcome = Outcome::Close;
                    }
                },
            );
            if let Some(c) = self.handle_outcome(c, outcome) {
                if c.popped_out {
                    keep.push(c);
                } else {
                    self.mail.compose_inline = Some(c);
                }
            }
        }
        keep.extend(std::mem::take(&mut self.composes));
        self.composes = keep;
    }

    fn compose_form(&mut self, ui: &mut Ui, c: &mut Compose) -> Outcome {
        let p = pal();
        let ctx = ui.ctx().clone();
        let mut outcome = Outcome::None;
        let body_id = Id::new(("compose-body", c.id));

        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter))) {
            outcome = Outcome::Send;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S))) {
            outcome = Outcome::SaveDraft;
        }

        // Toolbar
        Frame::NONE.inner_margin(Margin { left: 16, right: 12, top: 12, bottom: 8 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if widgets::primary_button(ui, Some(icons::PAPER_PLANE_TILT), "Send").on_hover_text("Send (Ctrl+Enter)").clicked() {
                    outcome = Outcome::Send;
                }
                if widgets::secondary_button(ui, Some(icons::TRASH), "Discard").clicked() {
                    outcome = Outcome::Discard;
                }
                widgets::ribbon_separator(ui);
                if widgets::icon_button(ui, icons::PAPERCLIP, "Attach files").clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        for f in files {
                            match std::fs::read(&f) {
                                Ok(data) => {
                                    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                                    c.attachments.push(OutAttachment { mime: mime::guess_mime(&name), name, data });
                                }
                                Err(e) => {
                                    self.toast(format!("Couldn't read {}: {e}", f.display()), ToastKind::Error);
                                }
                            };
                        }
                    }
                }
                widgets::ribbon_separator(ui);
                let fmt_enabled = c.html && !c.preview;
                let fmt_buttons: [(&str, &str, Fmt); 10] = [
                    (icons::TEXT_B, "Bold", Fmt::Bold),
                    (icons::TEXT_ITALIC, "Italic", Fmt::Italic),
                    (icons::TEXT_UNDERLINE, "Underline", Fmt::Underline),
                    (icons::TEXT_STRIKETHROUGH, "Strikethrough", Fmt::Strike),
                    (icons::LIST_BULLETS, "Bulleted list", Fmt::Bullet),
                    (icons::LIST_NUMBERS, "Numbered list", Fmt::Numbered),
                    (icons::QUOTES, "Quote", Fmt::Quote),
                    (icons::TEXT_H, "Heading", Fmt::Heading),
                    (icons::LINK, "Insert link", Fmt::Link),
                    (icons::CODE, "Code", Fmt::Code),
                ];
                for (icon, tip, f) in fmt_buttons {
                    let r = ui.add_enabled_ui(fmt_enabled, |ui| widgets::icon_button(ui, icon, tip)).inner;
                    if r.clicked() {
                        apply_format(&ctx, body_id, &mut c.body, f);
                    }
                }
                widgets::ribbon_separator(ui);
                let imp_icon = match c.importance {
                    Importance::High => icons::ARROW_UP,
                    Importance::Low => icons::ARROW_DOWN,
                    Importance::Normal => icons::WARNING_CIRCLE,
                };
                let imp = widgets::icon_button_ex(
                    ui,
                    imp_icon,
                    30.0,
                    17.0,
                    "Importance",
                    c.importance != Importance::Normal,
                    match c.importance {
                        Importance::High => Some(p.danger),
                        Importance::Low => Some(p.accent),
                        _ => None,
                    },
                );
                egui::Popup::menu(&imp).show(|ui| {
                    for (label, v) in [("High importance", Importance::High), ("Normal", Importance::Normal), ("Low importance", Importance::Low)] {
                        if widgets::menu_item(ui, if c.importance == v { icons::CHECK } else { "" }, label).clicked() {
                            c.importance = v;
                        }
                    }
                });
                if widgets::icon_button(ui, icons::SIGNATURE, "Insert signature").clicked() {
                    let sig = self.signature_for(&c.account_id, false);
                    if sig.is_empty() {
                        self.toast("No signature set for this account (Settings → Accounts)", ToastKind::Info);
                    } else {
                        c.body.push_str(&sig);
                    }
                }
                let more = widgets::icon_button(ui, icons::DOTS_THREE, "More options");
                egui::Popup::menu(&more).width(260.0).show(|ui| {
                    if widgets::menu_item(ui, if c.read_receipt { icons::CHECK } else { "" }, "Request a read receipt").clicked() {
                        c.read_receipt = !c.read_receipt;
                    }
                    if widgets::menu_item(ui, if !c.html { icons::CHECK } else { "" }, "Plain text only").clicked() {
                        c.html = !c.html;
                    }
                    if widgets::menu_item(ui, icons::FLOPPY_DISK, "Save draft (Ctrl+S)").clicked() {
                        outcome = Outcome::SaveDraft;
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icons::X, "Close (saves a draft)").clicked() {
                        outcome = Outcome::Close;
                    }
                    if !c.popped_out && widgets::icon_button(ui, icons::ARROW_SQUARE_OUT, "Open in new window").clicked() {
                        outcome = Outcome::PopOut;
                    }
                    if c.html {
                        let r = widgets::icon_button_ex(ui, icons::EYE, 30.0, 17.0, "Preview formatting", c.preview, None);
                        if r.clicked() {
                            c.preview = !c.preview;
                        }
                    }
                    if c.waiting_for.is_some() {
                        widgets::spinner(ui, 14.0);
                    }
                });
            });
        });
        let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));

        ScrollArea::vertical()
            .id_salt(("compose-scroll", c.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                Frame::NONE.inner_margin(Margin { left: 18, right: 18, top: 6, bottom: 18 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // From
                    let accounts: Vec<(String, String)> = self
                        .cfg
                        .accounts
                        .iter()
                        .filter(|a| a.enabled)
                        .map(|a| (a.id.clone(), format!("{} <{}>", a.display_name, a.email)))
                        .collect();
                    if accounts.len() > 1 {
                        self.field_row(ui, "From", |ui| {
                            let current = accounts.iter().find(|(id, _)| *id == c.account_id).map(|(_, l)| l.clone()).unwrap_or_default();
                            egui::ComboBox::from_id_salt(("from", c.id))
                                .selected_text(current)
                                .width(ui.available_width().min(420.0))
                                .show_ui(ui, |ui| {
                                    for (id, label) in &accounts {
                                        ui.selectable_value(&mut c.account_id, id.clone(), label);
                                    }
                                });
                        });
                    }
                    self.recipients_row(ui, c, Field::To);
                    if c.show_cc {
                        self.recipients_row(ui, c, Field::Cc);
                    }
                    if c.show_bcc {
                        self.recipients_row(ui, c, Field::Bcc);
                    }
                    // Subject
                    ui.horizontal(|ui| {
                        ui.set_min_height(40.0);
                        let r = ui.add(
                            TextEdit::singleline(&mut c.subject)
                                .id(Id::new(("subject", c.id)))
                                .hint_text(RichText::new("Add a subject").color(p.text3))
                                .frame(egui::Frame::NONE)
                                .font(font_semibold(16.0))
                                .desired_width(ui.available_width()),
                        );
                        if c.importance == Importance::High {
                            ui.painter().text(r.rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, "High importance", font(12.0), p.danger);
                        }
                    });
                    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));
                    ui.add_space(8.0);

                    // Body
                    if c.preview && c.html {
                        let mut h = DefaultHasher::new();
                        c.body.hash(&mut h);
                        let key = h.finish();
                        let doc = match &c.preview_cache {
                            Some((k, d)) if *k == key => d.clone(),
                            _ => {
                                let d = Arc::new(html::parse_html(&mime::markdown_to_html(&c.body)));
                                c.preview_cache = Some((key, d.clone()));
                                d
                            }
                        };
                        Frame::NONE
                            .fill(p.card_alt)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.set_min_height(260.0);
                                let empty = std::collections::HashMap::new();
                                html::show(ui, &doc, &html::RenderOpts { show_images: false, cid: &empty, base_size: 14.5 });
                            });
                    } else {
                        let out = TextEdit::multiline(&mut c.body)
                            .id(body_id)
                            .frame(egui::Frame::NONE)
                            .font(font(14.5))
                            .hint_text(RichText::new(if c.html {
                                "Write your message… Formatting: **bold**, *italic*, - lists, > quotes, [links](https://)"
                            } else {
                                "Write your message…"
                            }).color(p.text3))
                            .desired_width(f32::INFINITY)
                            .desired_rows(16)
                            .lock_focus(true)
                            .show(ui);
                        if c.focus_body {
                            out.response.request_focus();
                            let mut state = out.state.clone();
                            state.cursor.set_char_range(Some(CCursorRange::one(egui::text::CCursor::new(0))));
                            state.store(ui.ctx(), body_id);
                            c.focus_body = false;
                        }
                    }

                    // Attachments
                    if !c.attachments.is_empty() {
                        ui.add_space(10.0);
                        let total: usize = c.attachments.iter().map(|a| a.data.len()).sum();
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            for (i, a) in c.attachments.iter().enumerate() {
                                Frame::NONE
                                    .fill(p.card_alt)
                                    .stroke(Stroke::new(1.0, p.stroke))
                                    .corner_radius(CornerRadius::same(6))
                                    .inner_margin(Margin::symmetric(10, 6))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(icons::FILE).font(icon_font(16.0)).color(p.accent_text));
                                            ui.label(RichText::new(util::truncate_chars(&a.name, 32)).font(font(13.0)));
                                            ui.label(RichText::new(util::human_size(a.data.len())).font(font(12.0)).color(p.text3));
                                            if widgets::icon_button_ex(ui, icons::X, 22.0, 12.0, "Remove", false, None).clicked() {
                                                remove = Some(i);
                                            }
                                        });
                                    });
                            }
                        });
                        if let Some(i) = remove {
                            c.attachments.remove(i);
                        }
                        if total > 20 * 1024 * 1024 {
                            ui.label(RichText::new(format!("{} Attachments total {} — many servers reject messages over 20 MB.", icons::WARNING, util::human_size(total))).font(icon_font(12.5)).color(p.warning));
                        }
                    }

                    // Quoted original
                    if let Some(q) = c.quoted.as_mut() {
                        ui.add_space(12.0);
                        let label = if q.expanded { "Hide original message" } else { "··· Show original message" };
                        if widgets::link(ui, label).clicked() {
                            q.expanded = !q.expanded;
                        }
                        if q.expanded {
                            ui.add_space(6.0);
                            Frame::NONE
                                .stroke(Stroke::new(1.0, p.stroke))
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(&q.header_text).font(font(12.5)).color(p.text2));
                                    ui.add_space(6.0);
                                    let empty = std::collections::HashMap::new();
                                    html::show(ui, &q.doc, &html::RenderOpts { show_images: false, cid: &empty, base_size: 13.5 });
                                });
                        }
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, Some(icons::PAPER_PLANE_TILT), "Send").clicked() {
                            outcome = Outcome::Send;
                        }
                        if widgets::secondary_button(ui, None, "Discard").clicked() {
                            outcome = Outcome::Discard;
                        }
                        if c.is_dirty() {
                            ui.label(RichText::new("Unsaved changes — closing saves a draft").font(font(12.0)).color(p.text3));
                        }
                    });
                });
            });
        outcome
    }

    fn field_row(&mut self, ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
        let p = pal();
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            let (r, _) = ui.allocate_exact_size(vec2(52.0, 30.0), Sense::hover());
            ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, font(14.0), p.text2);
            add(ui);
        });
        let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));
    }

    fn recipients_row(&mut self, ui: &mut Ui, c: &mut Compose, field: Field) {
        let p = pal();
        let label = match field {
            Field::To => "To",
            Field::Cc => "Cc",
            Field::Bcc => "Bcc",
        };
        let input_id = Id::new(("rcpt", c.id, field as u8));
        let mut input_rect = None;
        let mut remove = None;
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            let (r, _) = ui.allocate_exact_size(vec2(52.0, 30.0), Sense::hover());
            ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, font(14.0), p.text2);
            let right_links = if field == Field::To { 70.0 } else { 0.0 };
            let w = ui.available_width() - right_links;
            ui.allocate_ui_with_layout(vec2(w, 30.0), Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                let list = match field {
                    Field::To => &c.to,
                    Field::Cc => &c.cc,
                    Field::Bcc => &c.bcc,
                };
                for (i, a) in list.iter().enumerate() {
                    let valid = util::is_valid_email(&a.email);
                    let text = a.display().to_string();
                    let g = ui.painter().layout_no_wrap(text, font(13.5), p.text);
                    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 54.0, 26.0), Sense::click());
                    let bg = if !valid { p.danger_soft } else if resp.hovered() { p.selected_hover } else { p.accent_soft };
                    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
                    widgets::paint_avatar(ui.painter(), Pos2::new(rect.left() + 13.0, rect.center().y), 10.0, &a.name, &a.email);
                    ui.painter().galley(Pos2::new(rect.left() + 28.0, rect.center().y - g.size().y / 2.0), g, if valid { p.text } else { p.danger });
                    let x_rect = egui::Rect::from_center_size(Pos2::new(rect.right() - 12.0, rect.center().y), vec2(18.0, 18.0));
                    ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, icons::X, icon_font(11.0), p.text2);
                    let resp = resp.on_hover_text(format!("{}{}", a.email, if valid { "" } else { " (invalid address)" }));
                    if resp.clicked() && resp.interact_pointer_pos().map(|pp| x_rect.contains(pp)).unwrap_or(false) {
                        remove = Some(i);
                    }
                }
                let text_w = (ui.available_width() - 6.0).max(120.0);
                let idx = field as usize;
                let r = ui.add(
                    TextEdit::singleline(&mut c.inputs[idx])
                        .id(input_id)
                        .frame(egui::Frame::NONE)
                        .font(font(14.0))
                        .desired_width(text_w)
                        .hint_text(if list.is_empty() { RichText::new("Add recipients").color(p.text3) } else { RichText::new("") }),
                );
                input_rect = Some(r.rect);
                if field == Field::To && c.focus_to {
                    r.request_focus();
                    c.focus_to = false;
                }
                if r.changed() {
                    let t = c.inputs[idx].clone();
                    if t.contains(',') || t.contains(';') {
                        c.commit_input(field);
                        c.suggestions.clear();
                    } else {
                        c.suggestions = self.store.suggest_recipients(t.trim(), 8).unwrap_or_default();
                        c.suggest_for = Some(field);
                        c.suggest_index = 0;
                    }
                }
                let has_focus = r.has_focus();
                if has_focus && c.suggest_for == Some(field) && !c.suggestions.is_empty() {
                    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                        c.suggest_index = (c.suggest_index + 1).min(c.suggestions.len() - 1);
                    }
                    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                        c.suggest_index = c.suggest_index.saturating_sub(1);
                    }
                }
                if has_focus && c.inputs[idx].is_empty() && ui.input(|i| i.key_pressed(Key::Backspace)) {
                    let list = c.list_mut(field);
                    list.pop();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Tab));
                if enter {
                    if c.suggest_for == Some(field) && !c.suggestions.is_empty() && !c.inputs[idx].trim().is_empty() {
                        let (n, e) = c.suggestions[c.suggest_index.min(c.suggestions.len() - 1)].clone();
                        c.inputs[idx].clear();
                        let list = c.list_mut(field);
                        if !list.iter().any(|a| a.email.eq_ignore_ascii_case(&e)) {
                            list.push(Addr::new(n, e));
                        }
                    } else {
                        c.commit_input(field);
                    }
                    c.suggestions.clear();
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        r.request_focus();
                    }
                } else if r.lost_focus() && !c.inputs[idx].trim().is_empty() && c.suggestions.is_empty() {
                    c.commit_input(field);
                }
            });
            if field == Field::To {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !c.show_bcc && widgets::link(ui, "Bcc").clicked() {
                        c.show_bcc = true;
                    }
                    if !c.show_cc && widgets::link(ui, "Cc").clicked() {
                        c.show_cc = true;
                    }
                });
            }
        });
        if let Some(i) = remove {
            c.list_mut(field).remove(i);
        }
        let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));

        // Suggestions dropdown.
        if c.suggest_for == Some(field) && !c.suggestions.is_empty() && ui.memory(|m| m.has_focus(input_id)) {
            if let Some(r) = input_rect {
                let mut picked = None;
                egui::Area::new(Id::new(("suggest", c.id, field as u8)))
                    .fixed_pos(r.left_bottom() + vec2(0.0, 4.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_width(340.0);
                            for (i, (n, e)) in c.suggestions.iter().enumerate() {
                                let (rect, resp) = ui.allocate_exact_size(vec2(340.0, 42.0), Sense::click());
                                if resp.hovered() || i == c.suggest_index {
                                    ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
                                }
                                widgets::paint_avatar(ui.painter(), Pos2::new(rect.left() + 20.0, rect.center().y), 14.0, n, e);
                                let name = if n.is_empty() { e.as_str() } else { n.as_str() };
                                ui.painter().text(Pos2::new(rect.left() + 42.0, rect.top() + 13.0), Align2::LEFT_CENTER, name, font_semibold(13.5), p.text);
                                ui.painter().text(Pos2::new(rect.left() + 42.0, rect.bottom() - 12.0), Align2::LEFT_CENTER, e, font(12.0), p.text3);
                                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                    picked = Some((n.clone(), e.clone()));
                                }
                            }
                        });
                    });
                if let Some((n, e)) = picked {
                    let idx = field as usize;
                    c.inputs[idx].clear();
                    let list = c.list_mut(field);
                    if !list.iter().any(|a| a.email.eq_ignore_ascii_case(&e)) {
                        list.push(Addr::new(n, e));
                    }
                    c.suggestions.clear();
                    ui.memory_mut(|m| m.request_focus(input_id));
                }
            }
        }
        let _ = Color32::TRANSPARENT;
        let _ = mix;
    }
}
