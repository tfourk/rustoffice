//! Small helpers: date formatting, IMAP modified UTF-7, text clean-up.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike, Weekday};

static TIME_24H: AtomicBool = AtomicBool::new(false);
/// 0 = Sunday, 1 = Monday
static FIRST_DAY: AtomicU8 = AtomicU8::new(0);

pub fn set_time_24h(v: bool) {
    TIME_24H.store(v, Ordering::Relaxed);
}
pub fn time_24h() -> bool {
    TIME_24H.load(Ordering::Relaxed)
}
pub fn set_first_day_monday(v: bool) {
    FIRST_DAY.store(v as u8, Ordering::Relaxed);
}
pub fn first_day_monday() -> bool {
    FIRST_DAY.load(Ordering::Relaxed) == 1
}

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn local(ts: i64) -> DateTime<Local> {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap())
}

pub fn local_midnight(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .and_then(|dt| Local.from_local_datetime(&dt).earliest())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

pub fn ts_from_date_time(date: NaiveDate, hour: u32, minute: u32) -> i64 {
    date.and_hms_opt(hour.min(23), minute.min(59), 0)
        .and_then(|dt| Local.from_local_datetime(&dt).earliest())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

pub fn fmt_time(dt: &DateTime<Local>) -> String {
    if time_24h() {
        format!("{:02}:{:02}", dt.hour(), dt.minute())
    } else {
        let (pm, h) = dt.hour12();
        format!("{}:{:02} {}", h, dt.minute(), if pm { "PM" } else { "AM" })
    }
}

pub fn fmt_hour(hour: u32) -> String {
    if time_24h() {
        format!("{:02}:00", hour)
    } else {
        let h = if hour % 12 == 0 { 12 } else { hour % 12 };
        format!("{} {}", h, if hour < 12 { "AM" } else { "PM" })
    }
}

/// Date shown in the message list.
pub fn fmt_list_date(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let dt = local(ts);
    let now = Local::now();
    let days = (now.date_naive() - dt.date_naive()).num_days();
    if days == 0 {
        fmt_time(&dt)
    } else if (1..7).contains(&days) {
        format!("{} {}", dt.format("%a"), fmt_time(&dt))
    } else if dt.year() == now.year() {
        dt.format("%a %-m/%-d").to_string()
    } else {
        dt.format("%-m/%-d/%Y").to_string()
    }
}

/// Full date used in the reading pane, e.g. "Wed 10/30/2026 4:12 PM".
pub fn fmt_full_date(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let dt = local(ts);
    format!("{} {}", dt.format("%a %-m/%-d/%Y"), fmt_time(&dt))
}

pub fn fmt_long_date(date: NaiveDate) -> String {
    date.format("%A, %B %-d, %Y").to_string()
}

/// Group heading for the message list.
pub fn date_group(ts: i64) -> &'static str {
    let dt = local(ts).date_naive();
    let today = today();
    let days = (today - dt).num_days();
    let week_start = start_of_week(today);
    if days <= 0 {
        "Today"
    } else if days == 1 {
        "Yesterday"
    } else if dt >= week_start {
        "This week"
    } else if dt >= week_start - chrono::Duration::days(7) {
        "Last week"
    } else if dt.year() == today.year() && dt.month() == today.month() {
        "Earlier this month"
    } else if (today.year() * 12 + today.month() as i32) - (dt.year() * 12 + dt.month() as i32) == 1
    {
        "Last month"
    } else {
        "Older"
    }
}

pub fn start_of_week(d: NaiveDate) -> NaiveDate {
    let wd = if first_day_monday() {
        d.weekday().num_days_from_monday()
    } else {
        d.weekday().num_days_from_sunday()
    };
    d - chrono::Duration::days(wd as i64)
}

pub fn weekday_names() -> [&'static str; 7] {
    if first_day_monday() {
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    } else {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
    }
}

pub fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

pub fn add_months(d: NaiveDate, months: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + months;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let mut day = d.day();
    loop {
        if let Some(nd) = NaiveDate::from_ymd_opt(y, m, day) {
            return nd;
        }
        day -= 1;
    }
}

pub fn human_size(bytes: usize) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{} B", bytes)
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn time_ago(ts: i64) -> String {
    let d = now_ts() - ts;
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{} min ago", d / 60)
    } else if d < 86400 {
        format!("{} h ago", d / 3600)
    } else {
        format!("{} days ago", d / 86400)
    }
}

/// Up to two initials for an avatar.
pub fn initials(name: &str, email: &str) -> String {
    let source = if name.trim().is_empty() {
        email.split('@').next().unwrap_or("")
    } else {
        name
    };
    let words: Vec<&str> = source
        .split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
        .filter(|w| w.chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false))
        .collect();
    let mut out = String::new();
    if let Some(first) = words.first() {
        out.extend(first.chars().next().map(|c| c.to_uppercase()).into_iter().flatten());
    }
    if words.len() > 1 {
        if let Some(last) = words.last() {
            out.extend(last.chars().next().map(|c| c.to_uppercase()).into_iter().flatten());
        }
    }
    if out.is_empty() {
        "?".into()
    } else {
        out
    }
}

/// Strips Re:/Fwd: prefixes (for threading and display).
pub fn base_subject(s: &str) -> String {
    let mut s = s.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in ["re:", "fw:", "fwd:", "aw:", "wg:", "sv:", "vs:", "tr:", "rv:"] {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.to_string()
}

pub fn reply_subject(s: &str) -> String {
    format!("RE: {}", base_subject(s))
}

pub fn forward_subject(s: &str) -> String {
    format!("FW: {}", base_subject(s))
}

pub fn is_valid_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
}

/// Collapses whitespace and removes quoted reply lines for list previews.
pub fn make_preview(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('>') {
            continue;
        }
        if l.starts_with("-----Original Message")
            || (l.starts_with("On ") && l.ends_with("wrote:"))
            || l.starts_with("From: ") && !out.is_empty()
        {
            break;
        }
        if l.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(l);
        if out.chars().count() > max_chars {
            break;
        }
    }
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Decodes an IMAP mailbox name in modified UTF-7 (RFC 3501 §5.1.3).
pub fn decode_imap_utf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let b64 = chunk.replace(',', "/");
            match base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.as_bytes()) {
                Ok(bytes) => {
                    let units: Vec<u16> = bytes
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                Err(_) => {
                    out.push('&');
                    out.push_str(chunk);
                    out.push('-');
                }
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Encodes a mailbox name in IMAP modified UTF-7.
pub fn encode_imap_utf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        let enc = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
        out.push('&');
        out.push_str(&enc.replace('/', ","));
        out.push('-');
        pending.clear();
    };
    for ch in s.chars() {
        if (' '..='~').contains(&ch) {
            flush(&mut pending, &mut out);
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Finds http(s) and mailto links in plain text: returns byte ranges.
pub fn find_links(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let start = ["https://", "http://", "mailto:", "www."]
            .iter()
            .filter_map(|p| rest.find(p).map(|pos| pos))
            .min();
        let Some(off) = start else { break };
        let s = i + off;
        // Must be at a word boundary.
        if s > 0 && (bytes[s - 1].is_ascii_alphanumeric()) {
            i = s + 1;
            continue;
        }
        let mut e = s;
        for (idx, ch) in text[s..].char_indices() {
            if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`') {
                break;
            }
            e = s + idx + ch.len_utf8();
        }
        while e > s && matches!(bytes[e - 1], b'.' | b',' | b')' | b';' | b':' | b'!' | b'?') {
            e -= 1;
        }
        if e > s + 6 {
            out.push((s, e));
        }
        i = e.max(s + 1);
    }
    out
}

pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() {
        "attachment".into()
    } else {
        t
    }
}

pub fn domain_of(email: &str) -> String {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf7_roundtrip() {
        for s in ["INBOX", "Entwürfe", "日本語", "A&B", "Sent Items", "Tâches/Été"] {
            assert_eq!(decode_imap_utf7(&encode_imap_utf7(s)), s);
        }
        assert_eq!(decode_imap_utf7("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(encode_imap_utf7("A&B"), "A&-B");
    }

    #[test]
    fn subjects() {
        assert_eq!(base_subject("Re: RE: Fwd: Hello"), "Hello");
        assert_eq!(reply_subject("Re: Hi"), "RE: Hi");
    }

    #[test]
    fn initials_work() {
        assert_eq!(initials("Jane Doe", ""), "JD");
        assert_eq!(initials("", "bob.smith@x.org"), "BS");
        assert_eq!(initials("", ""), "?");
    }

    #[test]
    fn links() {
        let t = "See https://example.com/a?b=1. Or www.rust-lang.org!";
        let l = find_links(t);
        assert_eq!(&t[l[0].0..l[0].1], "https://example.com/a?b=1");
        assert_eq!(&t[l[1].0..l[1].1], "www.rust-lang.org");
    }
}
