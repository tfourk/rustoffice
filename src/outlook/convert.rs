//! Turns Outlook items (bags of MAPI properties) into what the rest of the
//! app works with: RFC 5322 messages, contacts, calendar events and tasks.

use std::fmt::Write as _;

use base64::Engine;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};

use super::rtf::{self, RtfBody};
use super::{decode_codepage, AttachmentData, Named, Props, RawAttachment, RawItem};
use crate::model::{Contact, Importance};
use crate::util;

// Named property sets (the first field of their GUIDs).
pub const PS_PUBLIC_STRINGS: u32 = 0x0002_0329;
pub const PSETID_APPOINTMENT: u32 = 0x0006_2002;
pub const PSETID_TASK: u32 = 0x0006_2003;
pub const PSETID_ADDRESS: u32 = 0x0006_2004;
pub const PSETID_COMMON: u32 = 0x0006_2008;
pub const PSETID_MEETING: u32 = 0x6ED8_DA90;

/// What kind of thing an item is, going by its message class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mail,
    Contact,
    Event,
    Task,
    /// Distribution lists, journal entries, configuration items…
    Other,
}

pub fn kind_of(class: &str) -> Kind {
    let c = class.to_ascii_uppercase();
    let is = |prefix: &str| c == prefix || c.starts_with(&format!("{prefix}."));
    if is("IPM.CONTACT") {
        Kind::Contact
    } else if is("IPM.APPOINTMENT") {
        Kind::Event
    } else if is("IPM.TASK") {
        Kind::Task
    } else if ["IPM.DISTLIST", "IPM.ACTIVITY", "IPM.CONFIGURATION", "IPM.RULE", "IPM.MICROSOFT", "IPM.ABCHPERSON", "IPM.NETWORK"].iter().any(|c| is(c)) {
        Kind::Other
    } else {
        // Notes, posts, meeting requests, reports, sticky notes: all read as mail.
        Kind::Mail
    }
}

// --------------------------------------------------------------------- mail

/// A message ready to be stored.
pub struct Mail {
    pub raw: Vec<u8>,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub forwarded: bool,
    pub draft: bool,
    pub importance: Importance,
    pub categories: Vec<String>,
    /// Sent by the mailbox owner (address in `sender`).
    pub from_me: bool,
    pub sender: String,
}

fn valid_email(s: &str) -> bool {
    let s = s.trim();
    s.contains('@') && !s.contains(char::is_whitespace) && !s.contains(['<', '>', '"', ',', ';']) && s.is_ascii()
}

/// Outlook marks a subject's "RE: " style prefix with two control characters.
fn clean_subject(s: &str) -> String {
    let s = match s.strip_prefix('\u{1}') {
        Some(rest) => {
            let mut chars = rest.chars();
            chars.next();
            chars.as_str()
        }
        None => s,
    };
    s.replace(['\r', '\n'], " ").trim().to_string()
}

/// An RFC 2047 encoded word sequence for text that isn't plain ASCII.
fn encode_words(s: &str) -> String {
    let mut out = String::new();
    let mut chunk = String::new();
    let emit = |chunk: &mut String, out: &mut String| {
        if !chunk.is_empty() {
            if !out.is_empty() {
                out.push_str("\r\n ");
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(chunk.as_bytes());
            let _ = write!(out, "=?UTF-8?B?{b64}?=");
            chunk.clear();
        }
    };
    for c in s.chars() {
        if chunk.len() + c.len_utf8() > 42 {
            emit(&mut chunk, &mut out);
        }
        chunk.push(c);
    }
    emit(&mut chunk, &mut out);
    out
}

/// Free text in a header (Subject).
fn header_text(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    if s.is_ascii() {
        s
    } else {
        encode_words(&s)
    }
}

/// A display name in an address header.
fn phrase(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    let s = s.trim();
    if !s.is_ascii() {
        encode_words(s)
    } else if s.chars().all(|c| c.is_ascii_alphanumeric() || " !#$%&'*+-/=?^_`{|}~".contains(c)) {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// One mailbox for an address header. People without a usable address
/// (Exchange-internal entries) keep their name.
fn mailbox(name: &str, email: &str) -> Option<String> {
    let (name, email) = (name.trim().trim_matches('\''), email.trim());
    match (name.is_empty() || name.eq_ignore_ascii_case(email), valid_email(email)) {
        (true, true) => Some(email.to_string()),
        (false, true) => Some(format!("{} <{email}>", phrase(name))),
        (false, false) => Some(format!("{} <unknown@invalid>", phrase(name))),
        (true, false) => None,
    }
}

/// The original internet headers, unfolded: (name, value).
fn transport_headers(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.replace("\r\n", "\n").split('\n') {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

/// Headers copied from the original message when it still has them: the
/// ones that decide how the app files and shows a message.
const KEPT_HEADERS: &[&str] = &["Reply-To", "List-Unsubscribe", "List-Unsubscribe-Post", "List-Id", "Precedence", "Auto-Submitted"];

fn base64_lines(data: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(b64.len() + b64.len() / 38 + 2);
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push_str("\r\n");
    }
    out
}

/// A MIME entity: its own headers, a blank line and the body.
fn leaf(content_type: &str, extra_headers: &str, data: &[u8]) -> String {
    format!("Content-Type: {content_type}\r\nContent-Transfer-Encoding: base64\r\n{extra_headers}\r\n{}", base64_lines(data))
}

fn multipart(subtype: &str, parts: &[String], counter: &mut u32) -> String {
    *counter += 1;
    let boundary = format!("=_rustmail_outlook_{counter}_=");
    let mut out = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\r\n\r\n");
    for p in parts {
        let _ = write!(out, "--{boundary}\r\n{p}");
        if !p.ends_with("\r\n") {
            out.push_str("\r\n");
        }
    }
    let _ = write!(out, "--{boundary}--\r\n");
    out
}

/// `name="..."` / `filename*=...` parameters for a file name.
fn file_name_params(name: &str) -> (String, String) {
    let clean: String = name.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
    if clean.is_ascii() {
        (format!("; name=\"{clean}\""), format!("; filename=\"{clean}\""))
    } else {
        let mut pct = String::new();
        for b in clean.bytes() {
            if b.is_ascii_alphanumeric() || b"-._".contains(&b) {
                pct.push(b as char);
            } else {
                let _ = write!(pct, "%{b:02X}");
            }
        }
        (format!("; name=\"{}\"", encode_words(&clean).replace("\r\n ", " ")), format!("; filename*=UTF-8''{pct}"))
    }
}

struct Bodies {
    plain: Option<String>,
    html: Option<String>,
}

fn bodies(p: &Props) -> Bodies {
    let mut html = match p.bin(0x1013) {
        Some(bytes) => Some(decode_codepage(bytes, p.int(0x3FDE).unwrap_or(0) as u32)),
        None => p.text(0x1013),
    };
    let mut plain = p.text(0x1000);
    if html.is_none() {
        if let Some(rtf) = p.bin(0x1009).and_then(rtf::decompress) {
            match rtf::body(&rtf) {
                Some(RtfBody::Html(h)) => html = Some(h),
                Some(RtfBody::Text(t)) if plain.is_none() => plain = Some(t),
                _ => {}
            }
        }
    }
    Bodies {
        plain: plain.filter(|s| !s.trim().is_empty()),
        html: html.filter(|s| !s.trim().is_empty()),
    }
}

fn attachment_name(a: &RawAttachment) -> String {
    let p = &a.props;
    let name = [0x3707, 0x3704, 0x3001].iter().find_map(|id| p.text(*id).filter(|s| !s.trim().is_empty()));
    match (&a.data, name) {
        (_, Some(n)) => n.trim().to_string(),
        (AttachmentData::Message(m), None) => format!("{}.eml", util::sanitize_filename(&clean_subject(&m.props.text(0x0037).unwrap_or_default()))),
        (_, None) => "attachment".into(),
    }
}

/// The message as RFC 5322 text (always plain ASCII: bodies are base64 and
/// headers use encoded words).
fn message_text(item: &RawItem, counter: &mut u32) -> String {
    let p = &item.props;
    let original = p.text(0x007D).map(|t| transport_headers(&t)).unwrap_or_default();
    let original_header = |name: &str| {
        original
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|v| v.is_ascii() && !v.is_empty())
    };
    let mut head = String::new();
    let mut add = |name: &str, value: &str| {
        if !value.is_empty() {
            let _ = write!(head, "{name}: {value}\r\n");
        }
    };
    // An original address header is only better than the properties when it
    // has real addresses (Exchange writes its internal names into them).
    let original_addresses = |name: &str| original_header(name).filter(|v| v.contains('@') && !v.to_ascii_lowercase().contains("/o="));

    // From
    let (name, email) = sender(p);
    match (mailbox(&name, &email), original_addresses("From")) {
        (Some(m), _) if valid_email(&email) => add("From", &m),
        (_, Some(h)) => add("From", h),
        (Some(m), None) => add("From", &m),
        (None, None) => {}
    }

    // Recipients
    let mut lists: [Vec<String>; 3] = Default::default();
    for r in &item.recipients {
        let kind = r.int(0x0C15).unwrap_or(1);
        if !(1..=3).contains(&kind) {
            continue;
        }
        let name = r.text(0x3001).unwrap_or_default();
        let email = [0x39FE, 0x3003].iter().filter_map(|id| r.text(*id)).find(|e| valid_email(e)).unwrap_or_default();
        if let Some(m) = mailbox(&name, &email) {
            lists[kind as usize - 1].push(m);
        }
    }
    for (i, (header, display)) in [("To", 0x0E04u16), ("Cc", 0x0E03), ("Bcc", 0x0E02)].into_iter().enumerate() {
        if !lists[i].is_empty() {
            add(header, &lists[i].join(",\r\n "));
        } else if let Some(h) = original_addresses(header) {
            add(header, h);
        } else if let Some(names) = p.text(display) {
            let list: Vec<String> = names.split(';').filter_map(|n| mailbox(n, "")).collect();
            add(header, &list.join(",\r\n "));
        }
    }

    add("Subject", &header_text(&clean_subject(&p.text(0x0037).unwrap_or_default())));
    let date = message_date(p);
    if date > 0 {
        if let Some(d) = Utc.timestamp_opt(date, 0).single() {
            add("Date", &d.to_rfc2822());
        }
    }
    let ids = |v: String| v.split_whitespace().filter(|s| s.is_ascii()).collect::<Vec<_>>().join(" ");
    let message_id = p
        .text(0x1035)
        .map(ids)
        .filter(|s| !s.is_empty())
        .or_else(|| original_header("Message-ID").map(str::to_string))
        .or_else(|| {
            // Stable across imports of the same file.
            p.bin(0x300B).map(|k| format!("<{}@outlook.import>", k.iter().map(|b| format!("{b:02x}")).collect::<String>()))
        })
        .unwrap_or_default();
    let bracket = |s: String| if s.is_empty() || s.starts_with('<') { s } else { format!("<{s}>") };
    add("Message-ID", &bracket(message_id));
    add("In-Reply-To", &bracket(p.text(0x1042).map(ids).or_else(|| original_header("In-Reply-To").map(str::to_string)).unwrap_or_default()));
    add("References", &p.text(0x1039).map(ids).or_else(|| original_header("References").map(str::to_string)).unwrap_or_default());
    match p.int(0x0017) {
        Some(2) => add("Importance", "high"),
        Some(0) => add("Importance", "low"),
        _ => {}
    }
    for name in KEPT_HEADERS {
        if let Some(v) = original_header(name) {
            add(name, v);
        }
    }
    add("MIME-Version", "1.0");

    // Body
    let b = bodies(p);
    let html_lower = b.html.as_deref().map(str::to_ascii_lowercase).unwrap_or_default();
    let text_part = |subtype: &str, text: &str| leaf(&format!("text/{subtype}; charset=utf-8"), "", text.as_bytes());
    let mut body = match (&b.plain, &b.html) {
        (Some(t), Some(h)) => multipart("alternative", &[text_part("plain", t), text_part("html", h)], counter),
        (None, Some(h)) => text_part("html", h),
        (Some(t), None) => text_part("plain", t),
        (None, None) => text_part("plain", ""),
    };

    let mut inline = Vec::new();
    let mut attached = Vec::new();
    for a in &item.attachments {
        let name = attachment_name(a);
        let (name_param, file_param) = file_name_params(&name);
        let cid = a.props.text(0x3712).map(|c| c.trim().trim_matches(['<', '>']).to_string()).filter(|c| !c.is_empty() && c.is_ascii());
        match &a.data {
            AttachmentData::Bytes(data) => {
                let mime = a
                    .props
                    .text(0x370E)
                    .map(|m| m.trim().to_ascii_lowercase())
                    .filter(|m| m.is_ascii() && m.contains('/') && !m.contains(char::is_whitespace))
                    .unwrap_or_else(|| crate::mime::guess_mime(&name));
                let shown_in_body = cid.as_ref().is_some_and(|c| html_lower.contains(&format!("cid:{}", c.to_ascii_lowercase())));
                let content_id = cid.map(|c| format!("Content-ID: <{c}>\r\n")).unwrap_or_default();
                if shown_in_body {
                    inline.push(leaf(&format!("{mime}{name_param}"), &format!("{content_id}Content-Disposition: inline{file_param}\r\n"), data));
                } else {
                    attached.push(leaf(&format!("{mime}{name_param}"), &format!("Content-Disposition: attachment{file_param}\r\n"), data));
                }
            }
            AttachmentData::Message(m) => {
                attached.push(format!(
                    "Content-Type: message/rfc822{name_param}\r\nContent-Disposition: attachment{file_param}\r\n\r\n{}",
                    message_text(m, counter)
                ));
            }
            AttachmentData::None => {}
        }
    }
    if !inline.is_empty() {
        inline.insert(0, body);
        body = multipart("related", &inline, counter);
    }
    if !attached.is_empty() {
        attached.insert(0, body);
        body = multipart("mixed", &attached, counter);
    }
    head + &body
}

/// (display name, e-mail address) of who sent the message.
fn sender(p: &Props) -> (String, String) {
    let name = [0x0C1A, 0x0042].iter().find_map(|id| p.text(*id).filter(|s| !s.trim().is_empty())).unwrap_or_default();
    let email = [0x5D01, 0x0C1F, 0x5D02, 0x0065].iter().filter_map(|id| p.text(*id)).find(|e| valid_email(e)).unwrap_or_default();
    (name, email.trim().to_string())
}

/// When the message was sent (or, failing that, received or created).
fn message_date(p: &Props) -> i64 {
    [0x0039, 0x0E06, 0x3007, 0x3008].iter().filter_map(|id| p.time(*id)).find(|t| *t > 0).unwrap_or(0)
}

/// An item with nothing in it at all (Outlook keeps a few such
/// placeholders among the mail).
pub fn is_blank(item: &RawItem) -> bool {
    let p = &item.props;
    let b = bodies(p);
    let (name, email) = sender(p);
    clean_subject(&p.text(0x0037).unwrap_or_default()).is_empty()
        && b.plain.is_none()
        && b.html.is_none()
        && name.is_empty()
        && email.is_empty()
        && item.attachments.is_empty()
        && item.recipients.is_empty()
}

pub fn mail(item: &RawItem, named: &Named) -> Mail {
    let p = &item.props;
    let flags = p.int(0x0E07).unwrap_or(0);
    let verb = p.int(0x1081).unwrap_or(0);
    let mut counter = 0;
    Mail {
        raw: message_text(item, &mut counter).into_bytes(),
        date: message_date(p),
        seen: flags & 0x1 != 0,
        draft: flags & 0x8 != 0,
        from_me: flags & 0x20 != 0,
        flagged: p.int(0x1090) == Some(2),
        answered: matches!(verb, 102 | 103),
        forwarded: verb == 104,
        importance: match p.int(0x0017) {
            Some(2) => Importance::High,
            Some(0) => Importance::Low,
            _ => Importance::Normal,
        },
        categories: categories(p, named),
        sender: sender(p).1,
    }
}

fn categories(p: &Props, named: &Named) -> Vec<String> {
    named.by_name(PS_PUBLIC_STRINGS, "Keywords").map(|id| p.list(id)).unwrap_or_default()
}

// ----------------------------------------------------------------- contacts

/// Outlook stores dates such as birthdays and all-day events as the UTC
/// instant of midnight where they were entered; the nearest date recovers
/// the day in any time zone.
fn nearest_date(ts: i64) -> Option<NaiveDate> {
    Utc.timestamp_opt(ts + 12 * 3600, 0).single().map(|d| d.date_naive())
}

pub fn contact(item: &RawItem, named: &Named) -> Option<Contact> {
    let p = &item.props;
    let text = |id: u16| p.text(id).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let address_prop = |lid: u32| named.by_id(PSETID_ADDRESS, lid).and_then(&text);

    let mut emails: Vec<String> = Vec::new();
    // E-mail 1-3: the address, or (for Exchange entries) the SMTP address
    // Outlook keeps as the "original display name".
    for base in [0x8080u32, 0x8090, 0x80A0] {
        let found = [base + 3, base + 4].into_iter().filter_map(address_prop).find(|e| valid_email(e));
        if let Some(e) = found {
            if !emails.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
                emails.push(e);
            }
        }
    }
    let full_name = [text(0x3A06), text(0x3A44), text(0x3A11)].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let company = text(0x3A16).unwrap_or_default();
    let name = text(0x3001)
        .or(Some(full_name).filter(|s| !s.is_empty()))
        .or_else(|| address_prop(0x8005))
        .unwrap_or_else(|| company.clone());
    if name.is_empty() && emails.is_empty() {
        return None;
    }
    let mut phones: Vec<String> = Vec::new();
    for id in [0x3A1C, 0x3A08, 0x3A09, 0x3A1B, 0x3A2F, 0x3A1A, 0x3A1F] {
        if let Some(n) = text(id) {
            if !phones.contains(&n) {
                phones.push(n);
            }
        }
    }
    Some(Contact {
        name,
        emails,
        phones,
        company,
        job_title: text(0x3A17).unwrap_or_default(),
        address: address_prop(0x801B)
            .or_else(|| address_prop(0x801A))
            .or_else(|| text(0x3A15))
            .map(|a| a.replace("\r\n", "\n"))
            .unwrap_or_default(),
        website: text(0x3A51).or_else(|| text(0x3A50)).or_else(|| address_prop(0x802B)).unwrap_or_default(),
        birthday: p.time(0x3A42).and_then(nearest_date).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
        notes: text(0x1000).unwrap_or_default(),
        ..Default::default()
    })
}

// ------------------------------------------------------------------- events

fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace(['\n', '\r'], "\\n")
}

/// Minutes since 1601-01-01, as recurrence patterns count them.
fn date_from_minutes(minutes: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(1601, 1, 1)?.checked_add_signed(Duration::minutes(minutes as i64))
}

const DAY_CODES: [&str; 7] = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

fn days_of(mask: u32) -> Vec<&'static str> {
    (0..7).filter(|d| mask & (1 << d) != 0).map(|d| DAY_CODES[d]).collect()
}

/// A recurring appointment's pattern.
#[derive(Debug, PartialEq)]
pub struct Recurrence {
    pub rrule: String,
    /// Dates of occurrences that were deleted from the series.
    pub deleted: Vec<NaiveDate>,
}

/// Reads an appointment's recurrence pattern (MS-OXOCAL 2.2.1.44.1).
/// `start` is the first occurrence. Returns `None` for patterns this
/// app can't repeat (other calendars than the Gregorian one, for example).
pub fn recurrence(blob: &[u8], start: NaiveDate) -> Option<Recurrence> {
    let mut at = 0usize;
    let u16le = |at: &mut usize| -> Option<u32> {
        let v = u16::from_le_bytes(blob.get(*at..*at + 2)?.try_into().ok()?);
        *at += 2;
        Some(v as u32)
    };
    let (_reader, _writer, frequency, pattern, calendar) = (u16le(&mut at)?, u16le(&mut at)?, u16le(&mut at)?, u16le(&mut at)?, u16le(&mut at)?);
    let u32le = |at: &mut usize| -> Option<u32> {
        let v = u32::from_le_bytes(blob.get(*at..*at + 4)?.try_into().ok()?);
        *at += 4;
        Some(v)
    };
    let (_first, period, _sliding) = (u32le(&mut at)?, u32le(&mut at)?, u32le(&mut at)?);
    if calendar != 0 && calendar != 1 {
        return None;
    }
    let (mut mask, mut day, mut nth) = (0u32, 0u32, 0u32);
    match pattern {
        0 => {}
        1 => mask = u32le(&mut at)?,
        2 | 4 => day = u32le(&mut at)?,
        3 => {
            mask = u32le(&mut at)?;
            nth = u32le(&mut at)?;
        }
        _ => return None,
    }
    let (end_type, count, _first_dow) = (u32le(&mut at)?, u32le(&mut at)?, u32le(&mut at)?);
    let dates = |at: &mut usize| -> Option<Vec<u32>> {
        let n = u32le(at)?;
        (0..n.min(10_000)).map(|_| u32le(at)).collect()
    };
    let deleted = dates(&mut at)?;
    let modified = dates(&mut at)?;
    let (_start_date, end_date) = (u32le(&mut at)?, u32le(&mut at)?);

    let interval = |n: u32| if n > 1 { format!(";INTERVAL={n}") } else { String::new() };
    let position = |n: u32| if n >= 5 { -1 } else { n.max(1) as i32 };
    let by_day = |mask: u32, nth: u32| {
        let days = days_of(mask);
        match days.len() {
            0 => None,
            1 => Some(format!(";BYDAY={}{}", position(nth), days[0])),
            _ => Some(format!(";BYDAY={};BYSETPOS={}", days.join(","), position(nth))),
        }
    };
    let month_day = |day: u32| if pattern == 4 || day > 31 { ";BYMONTHDAY=-1".to_string() } else { format!(";BYMONTHDAY={day}") };
    let mut rule = match (frequency, pattern) {
        (0x200A, 0) => format!("FREQ=DAILY{}", interval(period / 1440)),
        (0x200A | 0x200B, 1) => {
            let days = days_of(mask);
            if days.is_empty() {
                return None;
            }
            // "Every weekday" is stored as a daily pattern with a day mask.
            let weeks = if frequency == 0x200B { period } else { 1 };
            format!("FREQ=WEEKLY{};BYDAY={}", interval(weeks), days.join(","))
        }
        (0x200C, 2 | 4) => format!("FREQ=MONTHLY{}{}", interval(period), month_day(day)),
        (0x200C, 3) => format!("FREQ=MONTHLY{}{}", interval(period), by_day(mask, nth)?),
        (0x200D, 2 | 4) => format!("FREQ=YEARLY{};BYMONTH={}{}", interval(period / 12), start.month(), month_day(day)),
        (0x200D, 3) => format!("FREQ=YEARLY{};BYMONTH={}{}", interval(period / 12), start.month(), by_day(mask, nth)?),
        _ => return None,
    };
    match end_type {
        0x2021 => {
            if let Some(d) = date_from_minutes(end_date) {
                let _ = write!(rule, ";UNTIL={}", d.format("%Y%m%d"));
            }
        }
        0x2022 if count > 0 => {
            let _ = write!(rule, ";COUNT={count}");
        }
        _ => {}
    }
    Some(Recurrence {
        rrule: rule,
        // A changed occurrence is listed as deleted too; it stays in the
        // series (at its original time) rather than vanish.
        deleted: deleted.iter().filter(|d| !modified.contains(d)).filter_map(|d| date_from_minutes(*d)).collect(),
    })
}

/// The appointment as an iCalendar event (the calendar's own import reads it).
pub fn event_ics(item: &RawItem, named: &Named) -> Option<String> {
    let p = &item.props;
    let appointment = |lid: u32| named.by_id(PSETID_APPOINTMENT, lid);
    let common = |lid: u32| named.by_id(PSETID_COMMON, lid);
    let start = appointment(0x820D).and_then(|id| p.time(id)).or_else(|| p.time(0x0060))?;
    let end = appointment(0x820E).and_then(|id| p.time(id)).or_else(|| p.time(0x0061)).unwrap_or(start).max(start);
    let all_day = appointment(0x8215).and_then(|id| p.bool(id)).unwrap_or(false);
    let local = |ts: i64| util::local(ts).naive_local();

    let uid = named
        .by_id(PSETID_MEETING, 3)
        .and_then(|id| p.bin(id))
        .or_else(|| p.bin(0x300B))
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>())
        .unwrap_or_else(|| format!("{start}-{}", p.text(0x0037).unwrap_or_default().len()));
    let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n");
    let mut line = |name: &str, value: &str| {
        if !value.is_empty() {
            let _ = write!(ics, "{name}:{value}\r\n");
        }
    };
    line("UID", &format!("outlook-{uid}"));
    line("SUMMARY", &ics_escape(&clean_subject(&p.text(0x0037).unwrap_or_default())));
    line("LOCATION", &ics_escape(appointment(0x8208).and_then(|id| p.text(id)).unwrap_or_default().trim()));
    line("DESCRIPTION", &ics_escape(p.text(0x1000).unwrap_or_default().trim()));

    let first_start: NaiveDateTime;
    if all_day {
        let first = nearest_date(start)?;
        let last = nearest_date(end)?.max(first + Duration::days(1));
        first_start = first.and_hms_opt(0, 0, 0)?;
        line("DTSTART;VALUE=DATE", &first.format("%Y%m%d").to_string());
        line("DTEND;VALUE=DATE", &last.format("%Y%m%d").to_string());
    } else {
        // Wall-clock times, so a repeating 9:00 meeting stays at 9:00
        // across daylight-saving changes.
        first_start = local(start);
        line("DTSTART", &first_start.format("%Y%m%dT%H%M%S").to_string());
        line("DTEND", &local(end).format("%Y%m%dT%H%M%S").to_string());
    }
    let recurring = appointment(0x8223).and_then(|id| p.bool(id)).unwrap_or(false);
    if let Some(r) = appointment(0x8216).and_then(|id| p.bin(id)).filter(|_| recurring).and_then(|b| recurrence(b, first_start.date())) {
        line("RRULE", &r.rrule);
        for d in r.deleted {
            if all_day {
                line("EXDATE;VALUE=DATE", &d.format("%Y%m%d").to_string());
            } else if let Some(t) = d.and_hms_opt(first_start.hour(), first_start.minute(), first_start.second()) {
                line("EXDATE", &t.format("%Y%m%dT%H%M%S").to_string());
            }
        }
    }
    match appointment(0x8205).and_then(|id| p.int(id)) {
        Some(0) => line("TRANSP", "TRANSPARENT"),
        Some(1) => line("STATUS", "TENTATIVE"),
        _ => {}
    }
    if let Some(c) = categories(p, named).first() {
        line("CATEGORIES", &ics_escape(c));
    }
    let reminder_set = common(0x8503).and_then(|id| p.bool(id)).unwrap_or(false);
    if let Some(minutes) = common(0x8501).and_then(|id| p.int(id)).filter(|m| reminder_set && (0..=40320).contains(m)) {
        line("BEGIN", "VALARM");
        line("ACTION", "DISPLAY");
        line("TRIGGER", &format!("-PT{minutes}M"));
        line("END", "VALARM");
    }
    ics.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    Some(ics)
}

// -------------------------------------------------------------------- tasks

pub struct TaskItem {
    pub title: String,
    pub notes: String,
    /// Local midnight of the due date.
    pub due: Option<i64>,
    pub completed: bool,
    pub completed_at: Option<i64>,
    pub important: bool,
}

pub fn task(item: &RawItem, named: &Named) -> Option<TaskItem> {
    let p = &item.props;
    let task_prop = |lid: u32| named.by_id(PSETID_TASK, lid);
    let title = clean_subject(&p.text(0x0037).unwrap_or_default());
    if title.is_empty() {
        return None;
    }
    let day = |lid: u32| task_prop(lid).and_then(|id| p.time(id)).filter(|t| *t > 0).and_then(nearest_date).map(util::local_midnight);
    let completed = task_prop(0x811C).and_then(|id| p.bool(id)).unwrap_or(false) || task_prop(0x8101).and_then(|id| p.int(id)) == Some(2);
    Some(TaskItem {
        title,
        notes: p.text(0x1000).map(|s| s.trim().replace("\r\n", "\n")).unwrap_or_default(),
        due: day(0x8105),
        completed,
        completed_at: if completed { day(0x810F) } else { None },
        important: p.int(0x0017) == Some(2),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outlook::Value;

    fn props(list: Vec<(u16, Value)>) -> Props {
        Props::new(list.into_iter().collect(), 1252)
    }

    fn text(s: &str) -> Value {
        Value::Text(s.into())
    }

    /// 2026-11-10 14:30:00 UTC.
    const WHEN: i64 = 1_794_321_000;

    fn sample() -> RawItem {
        RawItem {
            class: "IPM.Note".into(),
            props: props(vec![
                (0x0037, text("\u{1}\u{5}RE: Größe & plans")),
                (0x0C1A, text("Zoë Müller")),
                (0x0C1F, text("zoe@example.com")),
                (0x0039, Value::Time(WHEN)),
                (0x1035, text("<abc@example.com>")),
                (0x1042, text("<parent@example.com>")),
                (0x0E07, Value::Int(0x1 | 0x10)),
                (0x1090, Value::Int(2)),
                (0x1081, Value::Int(102)),
                (0x0017, Value::Int(2)),
                (0x1000, text("Hello 😀\r\nplain")),
                (0x1013, Value::Bin("<p>Hello <img src=\"cid:logo@1\"> caf\u{e9}</p>".as_bytes().to_vec())),
                (0x3FDE, Value::Int(65001)),
                (0x007D, text("Received: from a\r\n\tby b\r\nList-Unsubscribe: <https://example.com/u>\r\nSubject: old\r\n\r\n")),
            ]),
            recipients: vec![
                props(vec![(0x0C15, Value::Int(1)), (0x3001, text("Alex Morgan")), (0x3003, text("alex@example.com"))]),
                props(vec![
                    (0x0C15, Value::Int(2)),
                    (0x3001, text("Doe, Jane")),
                    (0x3003, text("/O=EXCH/OU=X/CN=RECIPIENTS/CN=JANE")),
                    (0x39FE, text("jane@example.com")),
                ]),
                props(vec![(0x0C15, Value::Int(2)), (0x3001, text("Internal Only")), (0x3003, text("/O=EXCH/CN=INTERNAL"))]),
            ],
            attachments: vec![
                RawAttachment {
                    props: props(vec![(0x3707, text("logo.png")), (0x370E, text("image/png")), (0x3712, text("logo@1"))]),
                    data: AttachmentData::Bytes(vec![1, 2, 3, 4]),
                },
                RawAttachment {
                    props: props(vec![(0x3707, text("Bericht für März.pdf"))]),
                    data: AttachmentData::Bytes(b"%PDF-1.4 test".to_vec()),
                },
                RawAttachment {
                    props: props(vec![]),
                    data: AttachmentData::Message(Box::new(RawItem {
                        class: "IPM.Note".into(),
                        props: props(vec![(0x0037, text("Inner note")), (0x1000, text("inner body")), (0x0C1A, text("Sam")), (0x0C1F, text("sam@example.com"))]),
                        recipients: vec![],
                        attachments: vec![],
                    })),
                },
            ],
        }
    }

    #[test]
    fn a_message_becomes_readable_mail() {
        let m = mail(&sample(), &Named::default());
        assert!(m.raw.is_ascii());
        assert!(m.seen && m.flagged && m.answered && !m.forwarded && !m.draft);
        assert_eq!((m.importance, m.date, m.sender.as_str()), (Importance::High, WHEN, "zoe@example.com"));

        let info = crate::mime::header_info(&m.raw);
        assert_eq!(info.subject, "RE: Größe & plans");
        assert_eq!((info.from.name.as_str(), info.from.email.as_str()), ("Zoë Müller", "zoe@example.com"));
        assert_eq!(info.message_id, "abc@example.com");
        assert_eq!(info.in_reply_to, "parent@example.com");
        assert_eq!(info.date, WHEN);
        assert!(info.bulk, "List-Unsubscribe is carried over");
        let to: Vec<_> = info.to.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
        assert_eq!(to, [("Alex Morgan", "alex@example.com")]);
        let cc: Vec<_> = info.cc.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
        assert_eq!(cc, [("Doe, Jane", "jane@example.com"), ("Internal Only", "unknown@invalid")]);

        let parsed = crate::mime::parse_full(&m.raw).unwrap();
        assert_eq!(parsed.text.as_deref().map(str::trim), Some("Hello 😀\r\nplain"));
        assert!(parsed.html.as_deref().unwrap().contains("caf\u{e9}</p>"));
        let files: Vec<_> = parsed.attachments.iter().map(|a| (a.name.as_str(), a.mime.as_str(), a.inline, a.size)).collect();
        assert_eq!(
            files,
            [
                ("logo.png", "image/png", true, 4),
                ("Bericht für März.pdf", "application/pdf", false, 13),
                ("Inner note.eml", "message/rfc822", false, files[2].3),
            ]
        );
        assert_eq!(crate::mime::inline_parts(&m.raw), vec![("logo@1".to_string(), vec![1, 2, 3, 4])]);
        // The attached message is a complete message of its own.
        let inner = crate::mime::part_bytes(&m.raw, parsed.attachments[2].part).unwrap();
        let inner = crate::mime::parse_full(&inner).unwrap();
        assert_eq!((inner.subject.as_str(), inner.text.as_deref().map(str::trim)), ("Inner note", Some("inner body")));
    }

    #[test]
    fn bodies_fall_back_to_rtf_and_headers_to_the_originals() {
        let rtf = b"{\\rtf1\\ansi\\fromhtml1 {\\*\\htmltag64 <p>}\\htmlrtf {\\htmlrtf0 from rtf{\\*\\htmltag72 </p>}\\htmlrtf }\\htmlrtf0 }";
        let mut stream = Vec::new();
        for v in [rtf.len() as u32 + 12, rtf.len() as u32, 0x414C_454D, 0] {
            stream.extend_from_slice(&v.to_le_bytes());
        }
        stream.extend_from_slice(rtf);
        let item = RawItem {
            class: "IPM.Note".into(),
            props: props(vec![
                (0x0037, text("Hi")),
                (0x0C1A, text("Exchange Person")),
                (0x0C1F, text("/O=EXCH/CN=PERSON")),
                (0x1009, Value::Bin(stream)),
                (0x0E06, Value::Time(WHEN)),
                (0x0E04, text("First Person; Second Person")),
                (0x007D, text("From: \"Exchange Person\" <person@example.com>\r\nTo: First, Person </O=EXCH/CN=FIRST>\r\nMessage-ID: <orig@example.com>\r\n")),
            ]),
            recipients: vec![],
            attachments: vec![],
        };
        let m = mail(&item, &Named::default());
        let parsed = crate::mime::parse_full(&m.raw).unwrap();
        assert_eq!(parsed.html.as_deref().map(str::trim), Some("<p>from rtf</p>"));
        assert_eq!((parsed.from[0].email.as_str(), parsed.message_id.as_str(), parsed.date), ("person@example.com", "orig@example.com", WHEN));
        let to: Vec<_> = parsed.to.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(to, ["First Person", "Second Person"]);
        assert!(!m.seen && m.importance == Importance::Normal);
    }

    #[test]
    fn message_classes_sort_into_kinds() {
        assert_eq!(kind_of("IPM.Note"), Kind::Mail);
        assert_eq!(kind_of("IPM.Note.SMIME"), Kind::Mail);
        assert_eq!(kind_of("IPM.Schedule.Meeting.Request"), Kind::Mail);
        assert_eq!(kind_of("REPORT.IPM.Note.NDR"), Kind::Mail);
        assert_eq!(kind_of("IPM.Contact"), Kind::Contact);
        assert_eq!(kind_of("IPM.Appointment.Custom"), Kind::Event);
        assert_eq!(kind_of("ipm.task"), Kind::Task);
        assert_eq!(kind_of("IPM.DistList"), Kind::Other);
        assert_eq!(kind_of("IPM.Contacts"), Kind::Mail);
        assert_eq!(kind_of("IPM.AbchPerson"), Kind::Other);
        let blank = RawItem {
            class: "IPM.Note".into(),
            props: props(vec![(0x0037, text(" ")), (0x0E07, Value::Int(1))]),
            recipients: vec![],
            attachments: vec![],
        };
        assert!(is_blank(&blank) && !is_blank(&sample()));
    }

    fn named() -> Named {
        let mut n = Named::default();
        for (i, (set, lid)) in [
            (PSETID_ADDRESS, 0x8083),
            (PSETID_ADDRESS, 0x8084),
            (PSETID_ADDRESS, 0x8093),
            (PSETID_ADDRESS, 0x801B),
            (PSETID_APPOINTMENT, 0x820D),
            (PSETID_APPOINTMENT, 0x820E),
            (PSETID_APPOINTMENT, 0x8215),
            (PSETID_APPOINTMENT, 0x8216),
            (PSETID_APPOINTMENT, 0x8223),
            (PSETID_APPOINTMENT, 0x8208),
            (PSETID_APPOINTMENT, 0x8205),
            (PSETID_COMMON, 0x8503),
            (PSETID_COMMON, 0x8501),
            (PSETID_TASK, 0x8105),
            (PSETID_TASK, 0x811C),
            (PSETID_TASK, 0x810F),
        ]
        .into_iter()
        .enumerate()
        {
            n.ids.insert((set, lid), 0x8000 + i as u16);
        }
        n.names.insert((PS_PUBLIC_STRINGS, "Keywords".into()), 0x8100);
        n
    }

    fn id(n: &Named, set: u32, lid: u32) -> u16 {
        n.by_id(set, lid).unwrap()
    }

    #[test]
    fn a_contact_keeps_its_details() {
        let n = named();
        let item = RawItem {
            class: "IPM.Contact".into(),
            props: props(vec![
                (0x3001, text("Elena García")),
                (0x3A16, text("Example Corp")),
                (0x3A17, text("Customer Success Lead")),
                (0x3A1C, text("+34 555 0108")),
                (0x3A08, text("+34 555 0100")),
                (0x3A42, Value::Time(Utc.with_ymd_and_hms(1990, 4, 11, 22, 0, 0).unwrap().timestamp())),
                (0x1000, text("Met at the conference")),
                (id(&n, PSETID_ADDRESS, 0x8083), text("/o=ExchangeLabs/cn=elena")),
                (id(&n, PSETID_ADDRESS, 0x8084), text("elena.garcia@example.com")),
                (id(&n, PSETID_ADDRESS, 0x8093), text("elena@home.example")),
                (id(&n, PSETID_ADDRESS, 0x801B), text("1 Main St\r\nMadrid")),
            ]),
            recipients: vec![],
            attachments: vec![],
        };
        let c = contact(&item, &n).unwrap();
        assert_eq!(c.name, "Elena García");
        assert_eq!(c.emails, ["elena.garcia@example.com", "elena@home.example"]);
        assert_eq!(c.phones, ["+34 555 0108", "+34 555 0100"]);
        assert_eq!((c.company.as_str(), c.job_title.as_str()), ("Example Corp", "Customer Success Lead"));
        assert_eq!((c.birthday.as_str(), c.address.as_str(), c.notes.as_str()), ("1990-04-12", "1 Main St\nMadrid", "Met at the conference"));
        let empty = RawItem {
            class: "IPM.Contact".into(),
            props: props(vec![]),
            recipients: vec![],
            attachments: vec![],
        };
        assert!(contact(&empty, &n).is_none());
    }

    /// A recurrence pattern as Outlook writes it.
    fn pattern(frequency: u16, kind: u16, period: u32, specific: &[u32], end_type: u32, count: u32, deleted: &[u32], modified: &[u32], end: u32) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0x3004u16, 0x3004, frequency, kind, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let mut words = vec![0u32, period, 0];
        words.extend_from_slice(specific);
        words.extend_from_slice(&[end_type, count, 0, deleted.len() as u32]);
        words.extend_from_slice(deleted);
        words.push(modified.len() as u32);
        words.extend_from_slice(modified);
        words.extend_from_slice(&[0, end]);
        for v in words {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    fn minutes(y: i32, m: u32, d: u32) -> u32 {
        (NaiveDate::from_ymd_opt(y, m, d).unwrap() - NaiveDate::from_ymd_opt(1601, 1, 1).unwrap()).num_minutes() as u32
    }

    #[test]
    fn recurrence_patterns_become_rules() {
        let start = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let rule = |b: Vec<u8>| recurrence(&b, start).map(|r| r.rrule);
        let never = 0x2023;
        assert_eq!(rule(pattern(0x200A, 0, 1440, &[], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=DAILY"));
        assert_eq!(rule(pattern(0x200A, 0, 4320, &[], 0x2022, 5, &[], &[], 0)).as_deref(), Some("FREQ=DAILY;INTERVAL=3;COUNT=5"));
        assert_eq!(rule(pattern(0x200A, 1, 1, &[0x3E], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"));
        assert_eq!(
            rule(pattern(0x200B, 1, 2, &[0x14], 0x2021, 0, &[], &[], minutes(2026, 12, 31))).as_deref(),
            Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;UNTIL=20261231")
        );
        assert_eq!(rule(pattern(0x200C, 2, 1, &[15], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=MONTHLY;BYMONTHDAY=15"));
        assert_eq!(rule(pattern(0x200C, 3, 3, &[0x04, 2], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=MONTHLY;INTERVAL=3;BYDAY=2TU"));
        assert_eq!(rule(pattern(0x200C, 3, 1, &[0x3E, 5], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"));
        assert_eq!(rule(pattern(0x200D, 2, 12, &[10], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=10"));
        assert_eq!(rule(pattern(0x200D, 3, 12, &[0x10, 4], never, 0, &[], &[], 0)).as_deref(), Some("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH"));
        assert_eq!(rule(vec![1, 2, 3]), None, "a damaged pattern is not a rule");

        let (gone, moved) = (minutes(2026, 11, 17), minutes(2026, 11, 24));
        let r = recurrence(&pattern(0x200B, 1, 1, &[0x04], never, 0, &[gone, moved], &[moved], 0), start).unwrap();
        assert_eq!(r.deleted, [NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()], "a changed occurrence stays");
    }

    #[test]
    fn an_appointment_becomes_a_calendar_event() {
        let n = named();
        let start = util::ts_from_date_time(NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(), 9, 30);
        let gone = minutes(2026, 11, 17);
        let item = RawItem {
            class: "IPM.Appointment".into(),
            props: props(vec![
                (0x0037, text("Weekly sync; team")),
                (0x1000, text("Agenda:\r\n- roadmap")),
                (0x300B, Value::Bin(vec![0xAB, 0xCD])),
                (id(&n, PSETID_APPOINTMENT, 0x820D), Value::Time(start)),
                (id(&n, PSETID_APPOINTMENT, 0x820E), Value::Time(start + 1800)),
                (id(&n, PSETID_APPOINTMENT, 0x8208), text("Room 4")),
                (id(&n, PSETID_APPOINTMENT, 0x8223), Value::Bool(true)),
                (id(&n, PSETID_APPOINTMENT, 0x8216), Value::Bin(pattern(0x200B, 1, 1, &[0x04], 0x2022, 10, &[gone], &[], 0))),
                (id(&n, PSETID_APPOINTMENT, 0x8205), Value::Int(1)),
                (id(&n, PSETID_COMMON, 0x8503), Value::Bool(true)),
                (id(&n, PSETID_COMMON, 0x8501), Value::Int(15)),
                (0x8100, Value::List(vec!["Work".into(), "Other".into()])),
            ]),
            recipients: vec![],
            attachments: vec![],
        };
        let ics = event_ics(&item, &n).unwrap();
        let events = crate::cal::parse_ics(&ics).events;
        assert_eq!(events.len(), 1, "{ics}");
        let e = &events[0];
        assert_eq!((e.uid.as_str(), e.title.as_str(), e.location.as_str()), ("outlook-abcd", "Weekly sync; team", "Room 4"));
        assert_eq!(e.description, "Agenda:\n- roadmap");
        assert_eq!((e.start, e.end, e.all_day), (start, start + 1800, false));
        assert_eq!(e.rrule, "FREQ=WEEKLY;BYDAY=TU;COUNT=10");
        assert_eq!(e.exdates, [start + 7 * 86400]);
        assert_eq!((e.reminder_min, e.category.as_str(), e.show_as), (Some(15), "Work", crate::model::ShowAs::Tentative));

        // An all-day event entered two time zones to the east.
        let midnight = Utc.with_ymd_and_hms(2026, 12, 23, 22, 0, 0).unwrap().timestamp();
        let item = RawItem {
            class: "IPM.Appointment".into(),
            props: props(vec![
                (0x0037, text("Holiday")),
                (id(&n, PSETID_APPOINTMENT, 0x820D), Value::Time(midnight)),
                (id(&n, PSETID_APPOINTMENT, 0x820E), Value::Time(midnight + 2 * 86400)),
                (id(&n, PSETID_APPOINTMENT, 0x8215), Value::Bool(true)),
            ]),
            recipients: vec![],
            attachments: vec![],
        };
        let e = &crate::cal::parse_ics(&event_ics(&item, &n).unwrap()).events[0];
        let day = |d: u32| util::local_midnight(NaiveDate::from_ymd_opt(2026, 12, d).unwrap());
        assert_eq!((e.all_day, e.start, e.end, e.rrule.as_str()), (true, day(24), day(26), ""));
    }

    #[test]
    fn a_task_keeps_its_state() {
        let n = named();
        let due = Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap().timestamp();
        let item = RawItem {
            class: "IPM.Task".into(),
            props: props(vec![
                (0x0037, text("File expenses")),
                (0x1000, text("Receipts in the drawer\r\n")),
                (0x0017, Value::Int(2)),
                (id(&n, PSETID_TASK, 0x8105), Value::Time(due)),
                (id(&n, PSETID_TASK, 0x811C), Value::Bool(true)),
                (id(&n, PSETID_TASK, 0x810F), Value::Time(due - 86400)),
            ]),
            recipients: vec![],
            attachments: vec![],
        };
        let t = task(&item, &n).unwrap();
        let day = |d: u32| util::local_midnight(NaiveDate::from_ymd_opt(2026, 11, d).unwrap());
        assert_eq!((t.title.as_str(), t.notes.as_str()), ("File expenses", "Receipts in the drawer"));
        assert_eq!((t.due, t.completed, t.completed_at, t.important), (Some(day(20)), true, Some(day(19)), true));
    }
}
