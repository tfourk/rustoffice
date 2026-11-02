//! Calendar helpers: recurrence expansion and iCalendar (RFC 5545) import /
//! export, including iMIP replies to invitations.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday};

use crate::model::{CalEvent, Occurrence, RepeatFreq, ShowAs};
use crate::util;

/// Expands an event into its occurrences overlapping `[from, to)`.
pub fn expand(ev: &CalEvent, from: i64, to: i64) -> Vec<Occurrence> {
    let dur = (ev.end - ev.start).max(0);
    if ev.repeat == RepeatFreq::None {
        if ev.start < to && ev.end.max(ev.start + 1) > from {
            return vec![Occurrence {
                event: ev.clone(),
                start: ev.start,
                end: ev.end,
            }];
        }
        return Vec::new();
    }
    let first = util::local(ev.start);
    let (h, m) = (first.hour(), first.minute());
    let mut date = first.date_naive();
    let until = ev.repeat_until.unwrap_or(i64::MAX);
    let mut out = Vec::new();
    // Skip ahead for fast-repeating series that started long ago.
    let from_date = util::local(from).date_naive();
    match ev.repeat {
        RepeatFreq::Daily | RepeatFreq::Weekdays => {
            if from_date > date + Duration::days(2) {
                date = from_date - Duration::days(1);
            }
        }
        RepeatFreq::Weekly => {
            let weeks = (from_date - date).num_days() / 7 - 1;
            if weeks > 0 {
                date += Duration::days(weeks * 7);
            }
        }
        _ => {}
    }
    let base_day = first.day();
    let mut month_index = 0i32;
    for _ in 0..3000 {
        let start = if ev.all_day {
            util::local_midnight(date)
        } else {
            util::ts_from_date_time(date, h, m)
        };
        if start >= to || start > until {
            break;
        }
        let ok_day = !(ev.repeat == RepeatFreq::Weekdays && util::is_weekend(date));
        if ok_day && start + dur.max(1) > from && start >= ev.start {
            out.push(Occurrence {
                event: ev.clone(),
                start,
                end: start + dur,
            });
        }
        date = match ev.repeat {
            RepeatFreq::Daily | RepeatFreq::Weekdays => date + Duration::days(1),
            RepeatFreq::Weekly => date + Duration::days(7),
            RepeatFreq::Monthly | RepeatFreq::Yearly => {
                month_index += if ev.repeat == RepeatFreq::Monthly { 1 } else { 12 };
                let d = util::add_months(first.date_naive(), month_index);
                // Keep the original day of month when possible.
                NaiveDate::from_ymd_opt(d.year(), d.month(), base_day).unwrap_or(d)
            }
            RepeatFreq::None => break,
        };
    }
    out
}

pub fn occurrences(events: &[CalEvent], from: i64, to: i64) -> Vec<Occurrence> {
    let mut v: Vec<Occurrence> = events.iter().flat_map(|e| expand(e, from, to)).collect();
    v.sort_by_key(|o| (o.start, o.end));
    v
}

// ---------------------------------------------------------------- iCalendar

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&line[1..]);
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn unescape(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// Splits `NAME;PARAM=x:VALUE` into (name, params, value).
fn split_prop(line: &str) -> Option<(String, Vec<(String, String)>, String)> {
    let mut in_quote = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    Some((name, params, value.to_string()))
}

/// Parses DTSTART/DTEND values. Returns (timestamp, is_date).
/// Times with a TZID are interpreted in the local time zone.
fn parse_dt(value: &str, params: &[(String, String)]) -> Option<(i64, bool)> {
    let v = value.trim();
    let is_date = params.iter().any(|(k, v)| k == "VALUE" && v.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if is_date {
        let d = NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok()?;
        return Some((util::local_midnight(d), true));
    }
    let (body, utc) = match v.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (v, false),
    };
    let dt = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
    if utc {
        Some((Utc.from_utc_datetime(&dt).timestamp(), false))
    } else {
        chrono::Local
            .from_local_datetime(&dt)
            .earliest()
            .map(|d| (d.timestamp(), false))
    }
}

fn parse_duration(v: &str) -> Option<i64> {
    // e.g. PT1H30M, P1D, -PT15M
    let neg = v.starts_with('-');
    let v = v.trim_start_matches(['-', '+']);
    let v = v.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().unwrap_or(0);
                num.clear();
                total += match (c, in_time) {
                    ('W', _) => n * 7 * 86400,
                    ('D', _) => n * 86400,
                    ('H', true) => n * 3600,
                    ('M', true) => n * 60,
                    ('S', true) => n,
                    _ => 0,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

#[derive(Debug, Clone, Default)]
pub struct IcsImport {
    pub method: String,
    pub events: Vec<CalEvent>,
    /// RSVP information for invitations: (organizer email, attendee emails)
    pub organizer_email: String,
}

pub fn parse_ics(text: &str) -> IcsImport {
    let mut out = IcsImport::default();
    let mut cur: Option<CalEvent> = None;
    let mut depth_other = 0;
    let mut has_end = false;
    let mut duration: Option<i64> = None;
    for line in unfold(text) {
        let Some((name, params, value)) = split_prop(&line) else { continue };
        match (name.as_str(), value.trim().to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") => {
                depth_other = 0;
                cur = Some(CalEvent::default());
                has_end = false;
                duration = None;
                continue;
            }
            ("BEGIN", _) => {
                if cur.is_some() {
                    depth_other += 1;
                }
                continue;
            }
            ("END", "VEVENT") => {
                if let Some(mut e) = cur.take() {
                    if !has_end {
                        e.end = e.start + duration.unwrap_or(if e.all_day { 86400 } else { 3600 });
                    }
                    if e.title.is_empty() {
                        e.title = "(no title)".into();
                    }
                    out.events.push(e);
                }
                continue;
            }
            ("END", _) => {
                if cur.is_some() && depth_other > 0 {
                    depth_other -= 1;
                }
                continue;
            }
            _ => {}
        }
        if name == "METHOD" && cur.is_none() {
            out.method = value.trim().to_ascii_uppercase();
        }
        let Some(e) = cur.as_mut() else { continue };
        if depth_other > 0 {
            continue; // inside VALARM etc.
        }
        match name.as_str() {
            "UID" => e.uid = value.trim().to_string(),
            "SUMMARY" => e.title = unescape(&value),
            "LOCATION" => e.location = unescape(&value),
            "DESCRIPTION" => e.description = unescape(&value),
            "DTSTART" => {
                if let Some((ts, date)) = parse_dt(&value, &params) {
                    e.start = ts;
                    e.all_day = date;
                }
            }
            "DTEND" => {
                if let Some((ts, _)) = parse_dt(&value, &params) {
                    e.end = ts;
                    has_end = true;
                }
            }
            "DURATION" => duration = parse_duration(value.trim()),
            "TRANSP" if value.trim().eq_ignore_ascii_case("TRANSPARENT") => e.show_as = ShowAs::Free,
            "ORGANIZER" => {
                let email = value.trim().trim_start_matches("mailto:").trim_start_matches("MAILTO:").to_string();
                let cn = params.iter().find(|(k, _)| k == "CN").map(|(_, v)| v.clone());
                out.organizer_email = email.clone();
                e.organizer = match cn {
                    Some(n) => format!("{n} <{email}>"),
                    None => email,
                };
            }
            "ATTENDEE" => {
                let email = value.trim().trim_start_matches("mailto:").trim_start_matches("MAILTO:").to_string();
                let cn = params.iter().find(|(k, _)| k == "CN").map(|(_, v)| v.clone());
                let entry = match cn {
                    Some(n) => format!("{n} <{email}>"),
                    None => email,
                };
                if !e.attendees.is_empty() {
                    e.attendees.push_str(", ");
                }
                e.attendees.push_str(&entry);
            }
            "RRULE" => {
                let parts: Vec<(String, String)> = value
                    .split(';')
                    .filter_map(|p| p.split_once('=').map(|(a, b)| (a.to_ascii_uppercase(), b.to_string())))
                    .collect();
                let get = |k: &str| parts.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
                e.repeat = match get("FREQ").as_deref() {
                    Some("DAILY") => RepeatFreq::Daily,
                    Some("WEEKLY") => {
                        let byday = get("BYDAY").unwrap_or_default();
                        if byday.split(',').count() == 5 && !byday.contains("SA") && !byday.contains("SU") {
                            RepeatFreq::Weekdays
                        } else {
                            RepeatFreq::Weekly
                        }
                    }
                    Some("MONTHLY") => RepeatFreq::Monthly,
                    Some("YEARLY") => RepeatFreq::Yearly,
                    _ => RepeatFreq::None,
                };
                if let Some(u) = get("UNTIL") {
                    e.repeat_until = parse_dt(&u, &[]).map(|(ts, _)| ts + 86399);
                }
            }
            _ => {}
        }
    }
    out
}

fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if count + len > 74 {
            out.push_str("\r\n ");
            count = 1;
        }
        out.push(ch);
        count += len;
    }
    out.push_str("\r\n");
    out
}

fn fmt_utc(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}

fn fmt_date(ts: i64) -> String {
    util::local(ts).format("%Y%m%d").to_string()
}

fn event_lines(e: &CalEvent, extra: &[String]) -> Vec<String> {
    let mut l = vec!["BEGIN:VEVENT".to_string()];
    l.push(format!("UID:{}", if e.uid.is_empty() { format!("{}@rustmail", e.id) } else { e.uid.clone() }));
    l.push(format!("DTSTAMP:{}", fmt_utc(util::now_ts())));
    if e.all_day {
        l.push(format!("DTSTART;VALUE=DATE:{}", fmt_date(e.start)));
        l.push(format!("DTEND;VALUE=DATE:{}", fmt_date(e.end.max(e.start + 86400))));
    } else {
        l.push(format!("DTSTART:{}", fmt_utc(e.start)));
        l.push(format!("DTEND:{}", fmt_utc(e.end)));
    }
    l.push(format!("SUMMARY:{}", escape(&e.title)));
    if !e.location.is_empty() {
        l.push(format!("LOCATION:{}", escape(&e.location)));
    }
    if !e.description.is_empty() {
        l.push(format!("DESCRIPTION:{}", escape(&e.description)));
    }
    if e.show_as == ShowAs::Free {
        l.push("TRANSP:TRANSPARENT".into());
    }
    let freq = match e.repeat {
        RepeatFreq::None => None,
        RepeatFreq::Daily => Some("FREQ=DAILY".to_string()),
        RepeatFreq::Weekdays => Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string()),
        RepeatFreq::Weekly => Some(format!(
            "FREQ=WEEKLY;BYDAY={}",
            match util::local(e.start).weekday() {
                Weekday::Mon => "MO",
                Weekday::Tue => "TU",
                Weekday::Wed => "WE",
                Weekday::Thu => "TH",
                Weekday::Fri => "FR",
                Weekday::Sat => "SA",
                Weekday::Sun => "SU",
            }
        )),
        RepeatFreq::Monthly => Some("FREQ=MONTHLY".to_string()),
        RepeatFreq::Yearly => Some("FREQ=YEARLY".to_string()),
    };
    if let Some(mut f) = freq {
        if let Some(u) = e.repeat_until {
            f.push_str(&format!(";UNTIL={}", fmt_utc(u)));
        }
        l.push(format!("RRULE:{f}"));
    }
    if let Some(m) = e.reminder_min {
        l.push("BEGIN:VALARM".into());
        l.push("ACTION:DISPLAY".into());
        l.push(format!("DESCRIPTION:{}", escape(&e.title)));
        l.push(format!("TRIGGER:-PT{m}M"));
        l.push("END:VALARM".into());
    }
    l.extend(extra.iter().cloned());
    l.push("END:VEVENT".into());
    l
}

pub fn to_ics(events: &[CalEvent]) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//RustMail//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    for e in events {
        lines.extend(event_lines(e, &[]));
    }
    lines.push("END:VCALENDAR".into());
    lines.iter().map(|l| fold(l)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rsvp {
    Accepted,
    Tentative,
    Declined,
}

impl Rsvp {
    pub fn partstat(self) -> &'static str {
        match self {
            Rsvp::Accepted => "ACCEPTED",
            Rsvp::Tentative => "TENTATIVE",
            Rsvp::Declined => "DECLINED",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Rsvp::Accepted => "Accepted",
            Rsvp::Tentative => "Tentative",
            Rsvp::Declined => "Declined",
        }
    }
}

/// iMIP REPLY body for an invitation (RFC 6047).
pub fn rsvp_reply(e: &CalEvent, organizer: &str, me_name: &str, me_email: &str, rsvp: Rsvp) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//RustMail//EN".to_string(),
        "METHOD:REPLY".to_string(),
    ];
    let extra = vec![
        format!("ORGANIZER:mailto:{organizer}"),
        format!(
            "ATTENDEE;PARTSTAT={};CN=\"{}\":mailto:{}",
            rsvp.partstat(),
            me_name.replace('"', ""),
            me_email
        ),
    ];
    let mut copy = e.clone();
    copy.reminder_min = None;
    lines.extend(event_lines(&copy, &extra));
    lines.push("END:VCALENDAR".into());
    lines.iter().map(|l| fold(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ics_roundtrip_and_expand() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(); // a Monday
        let ev = CalEvent {
            id: 1,
            uid: "abc@x".into(),
            title: "Standup, daily; team".into(),
            start: util::ts_from_date_time(d, 9, 30),
            end: util::ts_from_date_time(d, 9, 45),
            repeat: RepeatFreq::Weekdays,
            reminder_min: Some(10),
            ..Default::default()
        };
        let text = to_ics(&[ev.clone()]);
        let parsed = parse_ics(&text);
        assert_eq!(parsed.events.len(), 1);
        let p = &parsed.events[0];
        assert_eq!(p.title, ev.title);
        assert_eq!(p.start, ev.start);
        assert_eq!(p.end, ev.end);
        assert_eq!(p.repeat, RepeatFreq::Weekdays);

        let from = util::local_midnight(d);
        let to = util::local_midnight(d + Duration::days(14));
        let occ = expand(&ev, from, to);
        assert_eq!(occ.len(), 10, "two weeks of weekdays");

        let monthly = CalEvent {
            start: util::ts_from_date_time(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(), 10, 0),
            end: util::ts_from_date_time(NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(), 11, 0),
            repeat: RepeatFreq::Monthly,
            ..Default::default()
        };
        let occ = expand(
            &monthly,
            util::local_midnight(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            util::local_midnight(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
        );
        assert_eq!(occ.len(), 5);
    }

    #[test]
    fn parse_invite() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:42\r\nSUMMARY:Design review\r\nDTSTART:20261105T150000Z\r\nDURATION:PT1H30M\r\nORGANIZER;CN=\"Kim Lee\":mailto:kim@example.com\r\nATTENDEE;CN=Bob:mailto:bob@example.com\r\nLOCATION:Room 4\\, Floor 2\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nDESCRIPTION:ignored\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let p = parse_ics(ics);
        assert_eq!(p.method, "REQUEST");
        let e = &p.events[0];
        assert_eq!(e.title, "Design review");
        assert_eq!(e.end - e.start, 5400);
        assert_eq!(e.location, "Room 4, Floor 2");
        assert_eq!(p.organizer_email, "kim@example.com");
        assert!(e.attendees.contains("bob@example.com"));
        let reply = rsvp_reply(e, &p.organizer_email, "Bob", "bob@example.com", Rsvp::Accepted);
        assert!(reply.contains("METHOD:REPLY"));
        assert!(reply.contains("PARTSTAT=ACCEPTED"));
    }
}
