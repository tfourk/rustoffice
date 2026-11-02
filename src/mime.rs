//! MIME parsing (via `mail-parser`) and message building (via `lettre`).

use anyhow::{anyhow, Result};
use mail_parser::{Address, HeaderValue, MessageParser, MimeHeaders};

use crate::model::{Addr, Attachment, Importance, ParsedMessage};
use crate::util;

fn addrs(list: Option<&Address<'_>>) -> Vec<Addr> {
    match list {
        Some(l) => l
            .iter()
            .filter_map(|m| {
                let email = m.address().unwrap_or("").trim().to_string();
                let name = m.name().unwrap_or("").trim().to_string();
                if email.is_empty() && name.is_empty() {
                    None
                } else {
                    Some(Addr { name, email })
                }
            })
            .collect(),
        None => Vec::new(),
    }
}

fn text_list(v: &HeaderValue<'_>) -> Vec<String> {
    if let Some(list) = v.as_text_list() {
        return list.iter().map(|s| s.trim().to_string()).collect();
    }
    v.as_text()
        .map(|t| vec![t.trim().to_string()])
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
}

fn header_str(msg: &mail_parser::Message<'_>, name: &str) -> Option<String> {
    msg.header_raw(name)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn importance_of(msg: &mail_parser::Message<'_>) -> Importance {
    if let Some(v) = header_str(msg, "Importance") {
        let v = v.to_ascii_lowercase();
        if v.starts_with("high") {
            return Importance::High;
        }
        if v.starts_with("low") {
            return Importance::Low;
        }
    }
    if let Some(v) = header_str(msg, "X-Priority") {
        match v.trim().chars().next() {
            Some('1') | Some('2') => return Importance::High,
            Some('4') | Some('5') => return Importance::Low,
            _ => {}
        }
    }
    Importance::Normal
}

/// Picks the https link from a List-Unsubscribe header (falls back to mailto).
pub fn unsubscribe_target(raw: &str) -> Option<String> {
    let links: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    links
        .iter()
        .find(|l| l.starts_with("https://") || l.starts_with("http://"))
        .or_else(|| links.iter().find(|l| l.starts_with("mailto:")))
        .cloned()
}

/// Information extracted from headers during sync.
#[derive(Debug, Default, Clone)]
pub struct HeaderInfo {
    pub message_id: String,
    pub in_reply_to: String,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub date: i64,
    pub list_unsubscribe: Option<String>,
    pub importance: Importance,
    pub bulk: bool,
    pub multipart_mixed: bool,
}

pub fn header_info(raw: &[u8]) -> HeaderInfo {
    let parser = MessageParser::new();
    let Some(msg) = parser.parse_headers(raw) else {
        return HeaderInfo::default();
    };
    let from = addrs(msg.from()).into_iter().next().unwrap_or_default();
    let list_unsub = header_str(&msg, "List-Unsubscribe").and_then(|v| unsubscribe_target(&v));
    let precedence = header_str(&msg, "Precedence")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let auto_submitted = header_str(&msg, "Auto-Submitted")
        .map(|v| !v.eq_ignore_ascii_case("no"))
        .unwrap_or(false);
    let from_l = from.email.to_ascii_lowercase();
    let noreply = [
        "noreply", "no-reply", "donotreply", "do-not-reply", "newsletter", "notifications",
        "notification", "mailer-daemon", "marketing", "news@", "updates@", "digest",
    ]
    .iter()
    .any(|p| from_l.contains(p));
    let has = |name: &str| msg.header_raw(name).is_some();
    let bulk = list_unsub.is_some()
        || has("List-Id")
        || matches!(precedence.as_str(), "bulk" | "list" | "junk")
        || auto_submitted
        || noreply
        || has("X-Campaign")
        || has("X-Mailchimp-Campaign")
        || has("Feedback-ID");
    let multipart_mixed = msg
        .content_type()
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct.subtype().map(|s| s.eq_ignore_ascii_case("mixed")).unwrap_or(false)
        })
        .unwrap_or(false);
    HeaderInfo {
        message_id: msg.message_id().unwrap_or("").to_string(),
        in_reply_to: text_list(msg.in_reply_to()).into_iter().next().unwrap_or_default(),
        references: text_list(msg.references()),
        subject: msg.subject().unwrap_or("").to_string(),
        from,
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        list_unsubscribe: list_unsub,
        importance: importance_of(&msg),
        bulk,
        multipart_mixed,
    }
}

pub fn parse_full(raw: &[u8]) -> Option<ParsedMessage> {
    let parser = MessageParser::new();
    let msg = parser.parse(raw)?;

    let html = msg
        .html_bodies()
        .find(|p| p.is_text_html())
        .and_then(|p| p.text_contents())
        .map(|s| s.to_string());
    let text = msg
        .text_bodies()
        .find(|p| p.is_text() && !p.is_text_html())
        .and_then(|p| p.text_contents())
        .map(|s| s.to_string())
        .or_else(|| {
            if html.is_none() {
                msg.body_text(0).map(|s| s.into_owned())
            } else {
                None
            }
        });

    let mut attachments = Vec::new();
    for &id in &msg.attachments {
        let Some(part) = msg.part(id) else { continue };
        let mime = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(st) => format!("{}/{}", ct.ctype(), st),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| {
                if part.is_message() {
                    "message/rfc822".into()
                } else {
                    "application/octet-stream".into()
                }
            })
            .to_ascii_lowercase();
        let name = part
            .attachment_name()
            .map(|s| s.to_string())
            .or_else(|| {
                part.message()
                    .and_then(|m| m.subject())
                    .map(|s| format!("{}.eml", util::sanitize_filename(s)))
            })
            .unwrap_or_else(|| {
                let ext = mime.split('/').nth(1).unwrap_or("bin");
                format!("attachment.{ext}")
            });
        let cid = part
            .content_id()
            .map(|s| s.trim().trim_start_matches('<').trim_end_matches('>').to_string());
        let inline = cid.is_some()
            && part
                .content_disposition()
                .map(|d| d.is_inline())
                .unwrap_or(true);
        attachments.push(Attachment {
            part: id as usize,
            name,
            size: part.len(),
            inline,
            mime,
        });
    }

    let calendar = msg
        .parts
        .iter()
        .find(|p| p.is_content_type("text", "calendar"))
        .and_then(|p| p.text_contents())
        .map(|s| s.to_string())
        .or_else(|| {
            attachments
                .iter()
                .find(|a| a.mime == "text/calendar" || a.name.to_lowercase().ends_with(".ics"))
                .and_then(|a| msg.part(a.part as u32))
                .map(|p| String::from_utf8_lossy(p.contents()).into_owned())
        });

    Some(ParsedMessage {
        subject: msg.subject().unwrap_or("").to_string(),
        from: addrs(msg.from()),
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        bcc: addrs(msg.bcc()),
        reply_to: addrs(msg.reply_to()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        message_id: msg.message_id().unwrap_or("").to_string(),
        in_reply_to: text_list(msg.in_reply_to()).into_iter().next().unwrap_or_default(),
        references: text_list(msg.references()),
        html,
        text,
        attachments,
        importance: importance_of(&msg),
        calendar,
        read_receipt_to: header_str(&msg, "Disposition-Notification-To"),
    })
}

/// Plain text used for the search index and previews.
pub fn plain_text(p: &ParsedMessage) -> String {
    if let Some(t) = &p.text {
        if !t.trim().is_empty() {
            return t.clone();
        }
    }
    p.html
        .as_deref()
        .map(mail_parser::decoders::html::html_to_text)
        .unwrap_or_default()
}

/// Decoded bytes of one MIME part.
pub fn part_bytes(raw: &[u8], part: usize) -> Option<Vec<u8>> {
    let parser = MessageParser::new();
    let msg = parser.parse(raw)?;
    let p = msg.part(part as u32)?;
    Some(p.contents().to_vec())
}

/// Inline images referenced with `cid:`: (content-id, bytes).
pub fn inline_parts(raw: &[u8]) -> Vec<(String, Vec<u8>)> {
    let parser = MessageParser::new();
    let Some(msg) = parser.parse(raw) else {
        return Vec::new();
    };
    msg.parts
        .iter()
        .filter_map(|p| {
            let cid = p.content_id()?;
            let is_image = p
                .content_type()
                .map(|c| c.ctype().eq_ignore_ascii_case("image"))
                .unwrap_or(false);
            if !is_image {
                return None;
            }
            Some((
                cid.trim().trim_start_matches('<').trim_end_matches('>').to_string(),
                p.contents().to_vec(),
            ))
        })
        .collect()
}

// ------------------------------------------------------------------ building

#[derive(Clone, Debug, Default)]
pub struct OutAttachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct OutgoingMail {
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<OutAttachment>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub importance: Importance,
    pub read_receipt: bool,
    /// Keep the Bcc header (drafts).
    pub keep_bcc: bool,
}

pub struct BuiltMail {
    pub raw: Vec<u8>,
    pub recipients: Vec<String>,
}

fn mailbox(a: &Addr) -> Result<lettre::message::Mailbox> {
    let email: lettre::Address = a
        .email
        .trim()
        .parse()
        .map_err(|e| anyhow!("Invalid address \"{}\": {e}", a.email))?;
    let name = if a.name.trim().is_empty() {
        None
    } else {
        Some(a.name.trim().to_string())
    };
    Ok(lettre::message::Mailbox::new(name, email))
}

fn raw_header(name: &str, value: String) -> Result<lettre::message::header::HeaderValue> {
    let hn = lettre::message::header::HeaderName::new_from_ascii(name.to_string())
        .map_err(|e| anyhow!("bad header name: {e}"))?;
    Ok(lettre::message::header::HeaderValue::new(hn, value))
}

pub fn guess_mime(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "eml" => "message/rfc822",
        "json" => "application/json",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
    .to_string()
}

pub fn build(mail: &OutgoingMail) -> Result<BuiltMail> {
    use lettre::message::{header::ContentType, Attachment as LAttachment, MultiPart, SinglePart};

    let mut b = lettre::Message::builder()
        .from(mailbox(&mail.from)?)
        .subject(mail.subject.clone())
        .date_now()
        .message_id(None)
        .user_agent(format!("RustMail {}", env!("CARGO_PKG_VERSION")));
    for a in &mail.to {
        b = b.to(mailbox(a)?);
    }
    for a in &mail.cc {
        b = b.cc(mailbox(a)?);
    }
    for a in &mail.bcc {
        b = b.bcc(mailbox(a)?);
    }
    if let Some(id) = mail.in_reply_to.as_ref().filter(|s| !s.is_empty()) {
        b = b.in_reply_to(format!("<{}>", id.trim_matches(['<', '>'])));
    }
    if !mail.references.is_empty() {
        let refs: Vec<String> = mail
            .references
            .iter()
            .map(|r| format!("<{}>", r.trim_matches(['<', '>'])))
            .collect();
        b = b.references(refs.join(" "));
    }
    match mail.importance {
        Importance::High => {
            b = b
                .raw_header(raw_header("Importance", "high".into())?)
                .raw_header(raw_header("X-Priority", "1".into())?);
        }
        Importance::Low => {
            b = b
                .raw_header(raw_header("Importance", "low".into())?)
                .raw_header(raw_header("X-Priority", "5".into())?);
        }
        Importance::Normal => {}
    }
    if mail.read_receipt {
        b = b.raw_header(raw_header(
            "Disposition-Notification-To",
            mail.from.to_header(),
        )?);
    }
    if mail.keep_bcc {
        b = b.keep_bcc();
    }

    let body = match &mail.html {
        Some(html) => MultiPart::alternative_plain_html(mail.text.clone(), html.clone()),
        None => MultiPart::mixed().singlepart(SinglePart::plain(mail.text.clone())),
    };
    let message = if mail.attachments.is_empty() {
        match &mail.html {
            Some(_) => b.multipart(body)?,
            None => b.singlepart(SinglePart::plain(mail.text.clone()))?,
        }
    } else {
        let mut mixed = match &mail.html {
            Some(_) => MultiPart::mixed().multipart(body),
            None => MultiPart::mixed().singlepart(SinglePart::plain(mail.text.clone())),
        };
        for att in &mail.attachments {
            let ct = ContentType::parse(&att.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            mixed = mixed.singlepart(LAttachment::new(att.name.clone()).body(att.data.clone(), ct));
        }
        b.multipart(mixed)?
    };
    let recipients = message
        .envelope()
        .to()
        .iter()
        .map(|a| a.to_string())
        .collect();
    Ok(BuiltMail {
        raw: message.formatted(),
        recipients,
    })
}

/// Converts the Markdown-flavoured compose text to e-mail friendly HTML.
pub fn markdown_to_html(text: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    // Keep single line breaks as typed (e-mail style), Markdown would merge them.
    let prepared: String = text
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                format!("{l}  ")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let parser = Parser::new_ext(&prepared, opts);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

pub fn wrap_html_document(inner: &str) -> String {
    format!(
        "<html><head><meta charset=\"utf-8\"></head><body style=\"font-family: 'Segoe UI', \
         Calibri, Arial, sans-serif; font-size: 11pt; color: #242424;\">{inner}</body></html>"
    )
}

pub fn escape_html(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_roundtrip() {
        let mail = OutgoingMail {
            from: Addr::new("Ann Example", "ann@example.com"),
            to: vec![Addr::new("Bob", "bob@example.com")],
            cc: vec![Addr::new("", "carol@example.com")],
            bcc: vec![Addr::new("", "dave@example.com")],
            subject: "Quarterly report ✓".into(),
            text: "Hello **Bob**\nSee attached.".into(),
            html: Some(wrap_html_document(&markdown_to_html("Hello **Bob**\nSee attached."))),
            attachments: vec![OutAttachment {
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                data: b"some notes".to_vec(),
            }],
            in_reply_to: Some("abc@x".into()),
            references: vec!["root@x".into(), "abc@x".into()],
            importance: Importance::High,
            read_receipt: false,
            keep_bcc: false,
        };
        let built = build(&mail).unwrap();
        assert_eq!(built.recipients.len(), 3);
        let p = parse_full(&built.raw).unwrap();
        assert_eq!(p.subject, "Quarterly report ✓");
        assert!(!p.message_id.is_empty(), "outgoing mail has a Message-ID");
        assert_eq!(p.to[0].email, "bob@example.com");
        assert!(p.bcc.is_empty(), "bcc must not leak into the headers");
        assert_eq!(p.in_reply_to, "abc@x");
        assert_eq!(p.references, vec!["root@x".to_string(), "abc@x".to_string()]);
        assert_eq!(p.importance, Importance::High);
        assert!(p.html.as_deref().unwrap().contains("<strong>Bob</strong>"));
        assert!(p.text.as_deref().unwrap().contains("See attached."));
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].name, "notes.txt");
        let bytes = part_bytes(&built.raw, p.attachments[0].part).unwrap();
        assert_eq!(bytes, b"some notes");
        let h = header_info(&built.raw);
        assert_eq!(h.from.email, "ann@example.com");
        assert!(h.multipart_mixed);
        assert!(!h.bulk);
    }

    #[test]
    fn unsubscribe_prefers_https() {
        assert_eq!(
            unsubscribe_target("<mailto:u@x.com>, <https://x.com/u>").as_deref(),
            Some("https://x.com/u")
        );
    }
}
