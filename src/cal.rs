//! Calendar helpers: recurrence expansion and iCalendar (RFC 5545) import /
//! export, including iMIP replies to invitations.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;

use crate::model::{CalEvent, Occurrence, RepeatFreq, ShowAs};
use crate::util;

// ------------------------------------------------------------- time zones

/// The IANA name of this computer's time zone ("" if unknown).
pub fn local_tz_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_default()
}

/// Windows / Outlook zone names seen in invitations, mapped to IANA names.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Mountain Standard Time", "America/Denver"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Central Standard Time", "America/Chicago"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Eastern Standard Time", "America/New_York"),
    ("US Eastern Standard Time", "America/Indianapolis"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Argentina Standard Time", "America/Buenos_Aires"),
    ("UTC", "UTC"),
    ("Coordinated Universal Time", "UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("Romance Standard Time", "Europe/Paris"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("India Standard Time", "Asia/Kolkata"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
];

/// Resolves a TZID (IANA name, Windows name, or vendor-prefixed name like
/// "/mozilla.org/20050126_1/Europe/Berlin") to a time zone.
pub fn find_tz(tzid: &str) -> Option<Tz> {
    let t = tzid.trim().trim_matches('"');
    if t.is_empty() {
        return None;
    }
    if let Ok(z) = t.parse::<Tz>() {
        return Some(z);
    }
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(t)) {
        return iana.parse().ok();
    }
    // Vendor prefixes: try ever shorter "Area/City" suffixes.
    let parts: Vec<&str> = t.split('/').filter(|p| !p.is_empty()).collect();
    (0..parts.len()).find_map(|i| parts[i..].join("/").parse().ok())
}

/// Wall-clock time in `tz` (or this computer's zone when `None`) to a
/// timestamp. In a DST gap the time is moved forward; in an overlap the
/// earlier instant wins.
pub fn wall_to_ts(dt: NaiveDateTime, tz: Option<Tz>) -> i64 {
    fn resolve<Z: TimeZone>(z: &Z, dt: NaiveDateTime) -> i64 {
        match z.from_local_datetime(&dt) {
            chrono::LocalResult::Single(d) => d.timestamp(),
            chrono::LocalResult::Ambiguous(a, _) => a.timestamp(),
            chrono::LocalResult::None => z
                .from_local_datetime(&(dt + Duration::hours(1)))
                .earliest()
                .map(|d| d.timestamp())
                .unwrap_or_else(|| Utc.from_utc_datetime(&dt).timestamp()),
        }
    }
    match tz {
        Some(z) => resolve(&z, dt),
        None => resolve(&chrono::Local, dt),
    }
}

/// Timestamp to wall-clock time in `tz` (or this computer's zone).
pub fn ts_to_wall(ts: i64, tz: Option<Tz>) -> NaiveDateTime {
    let utc = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    match tz {
        Some(z) => utc.with_timezone(&z).naive_local(),
        None => utc.with_timezone(&chrono::Local).naive_local(),
    }
}

// ------------------------------------------------------------- recurrence

/// Expands an event into its occurrences overlapping `[from, to)`.
/// Removed occurrences (EXDATE) are left out; changed single occurrences are
/// handled by [`occurrences`].
pub fn expand(ev: &CalEvent, from: i64, to: i64) -> Vec<Occurrence> {
    let dur = (ev.end - ev.start).max(0);
    let starts = if !ev.rrule.trim().is_empty() {
        expand_rrule(ev, from, to).unwrap_or_else(|| expand_simple(ev, from, to))
    } else {
        expand_simple(ev, from, to)
    };
    starts
        .into_iter()
        .filter(|s| !is_excluded(ev, *s))
        .map(|start| Occurrence {
            event: ev.clone(),
            start,
            end: start + dur,
        })
        .collect()
}

fn same_occurrence(ev: &CalEvent, a: i64, b: i64) -> bool {
    if ev.all_day {
        util::local(a).date_naive() == util::local(b).date_naive()
    } else {
        (a - b).abs() < 60
    }
}

fn is_excluded(ev: &CalEvent, start: i64) -> bool {
    ev.exdates.iter().any(|x| same_occurrence(ev, *x, start))
}

/// Start times from the simple repeat choices (`RepeatFreq`).
fn expand_simple(ev: &CalEvent, from: i64, to: i64) -> Vec<i64> {
    let dur = (ev.end - ev.start).max(0);
    if ev.repeat == RepeatFreq::None {
        if ev.start < to && ev.end.max(ev.start + 1) > from {
            return vec![ev.start];
        }
        return Vec::new();
    }
    let tz = find_tz(&ev.tz);
    let first = ts_to_wall(ev.start, if ev.all_day { None } else { tz });
    let (h, m) = (first.hour(), first.minute());
    let mut date = first.date();
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
            wall_to_ts(date.and_hms_opt(h, m, 0).unwrap_or_default(), tz)
        };
        if start >= to || start > until {
            break;
        }
        let ok_day = !(ev.repeat == RepeatFreq::Weekdays && util::is_weekend(date));
        if ok_day && start + dur.max(1) > from && start >= ev.start {
            out.push(start);
        }
        date = match ev.repeat {
            RepeatFreq::Daily | RepeatFreq::Weekdays => date + Duration::days(1),
            RepeatFreq::Weekly => date + Duration::days(7),
            RepeatFreq::Monthly | RepeatFreq::Yearly => {
                month_index += if ev.repeat == RepeatFreq::Monthly { 1 } else { 12 };
                let d = util::add_months(first.date(), month_index);
                // Keep the original day of month when possible.
                NaiveDate::from_ymd_opt(d.year(), d.month(), base_day).unwrap_or(d)
            }
            RepeatFreq::None => break,
        };
    }
    out
}

/// Makes an RRULE acceptable to the `rrule` crate: UNTIL must be in UTC
/// when DTSTART carries a zone (RFC 5545), but real-world data often has a
/// date or a local time there.
fn normalize_until(rule: &str, tz: Option<Tz>, all_day: bool) -> String {
    rule.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|part| {
            let Some((k, v)) = part.split_once('=') else { return part.to_string() };
            if !k.trim().eq_ignore_ascii_case("UNTIL") {
                return part.to_string();
            }
            let v = v.trim();
            let ts = if v.len() == 8 {
                NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(|d| {
                    let end = d.and_hms_opt(23, 59, 59).unwrap_or_default();
                    if all_day { Utc.from_utc_datetime(&end).timestamp() } else { wall_to_ts(end, tz) }
                })
            } else if let Some(b) = v.strip_suffix('Z') {
                NaiveDateTime::parse_from_str(b, "%Y%m%dT%H%M%S").ok().map(|d| Utc.from_utc_datetime(&d).timestamp())
            } else {
                NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok().map(|d| wall_to_ts(d, tz))
            };
            match ts {
                Some(t) => format!("UNTIL={}", fmt_utc(t)),
                None => part.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Start times from a full RRULE, computed in the event's own time zone so
/// repeats keep their wall-clock time across daylight-saving changes.
/// `None` when the rule can't be read (the caller falls back to `repeat`).
fn expand_rrule(ev: &CalEvent, from: i64, to: i64) -> Option<Vec<i64>> {
    let dur = (ev.end - ev.start).max(0);
    let (dtstart, tz_name) = if ev.all_day {
        // All-day: expand dates in UTC, then place them at local midnight.
        let d = util::local(ev.start).date_naive();
        (format!("DTSTART:{}T000000Z", d.format("%Y%m%d")), String::new())
    } else {
        let tz = find_tz(&ev.tz).or_else(|| find_tz(&local_tz_name()));
        match tz {
            Some(z) => {
                let wall = ts_to_wall(ev.start, Some(z));
                (format!("DTSTART;TZID={}:{}", z.name(), wall.format("%Y%m%dT%H%M%S")), z.name().to_string())
            }
            None => (format!("DTSTART:{}", fmt_utc(ev.start)), String::new()),
        }
    };
    let tz = find_tz(&tz_name);
    let rule = normalize_until(ev.rrule.trim().trim_start_matches("RRULE:"), tz, ev.all_day);
    let set: rrule::RRuleSet = format!("{dtstart}\nRRULE:{rule}").parse().ok()?;
    let lo = Utc.timestamp_opt(from - dur - 1, 0).single()?.with_timezone(&rrule::Tz::UTC);
    let hi = Utc.timestamp_opt(to, 0).single()?.with_timezone(&rrule::Tz::UTC);
    let result = set.after(lo).before(hi).all(5000);
    let out = result
        .dates
        .iter()
        .map(|d| {
            if ev.all_day {
                util::local_midnight(d.naive_utc().date())
            } else {
                d.timestamp()
            }
        })
        .filter(|s| *s < to && s + dur.max(1) > from)
        .collect();
    Some(out)
}

/// Occurrences of all events in `[from, to)`, sorted. A changed single
/// occurrence (same calendar and UID, `recurrence_id` set) replaces the one
/// it overrides; cancelled ones just remove it.
pub fn occurrences(events: &[CalEvent], from: i64, to: i64) -> Vec<Occurrence> {
    use std::collections::HashMap;
    let mut overrides: HashMap<(i64, &str), Vec<&CalEvent>> = HashMap::new();
    for e in events.iter().filter(|e| e.recurrence_id.is_some()) {
        overrides.entry((e.calendar_id, e.uid.as_str())).or_default().push(e);
    }
    let mut v: Vec<Occurrence> = Vec::new();
    for e in events {
        if e.recurrence_id.is_some() {
            if !e.cancelled && e.start < to && e.end.max(e.start + 1) > from {
                v.push(Occurrence { event: e.clone(), start: e.start, end: e.end });
            }
            continue;
        }
        if e.cancelled {
            continue;
        }
        let ovs = overrides.get(&(e.calendar_id, e.uid.as_str()));
        for o in expand(e, from, to) {
            let replaced = ovs.is_some_and(|list| {
                list.iter().any(|x| x.recurrence_id.is_some_and(|r| same_occurrence(e, r, o.start)))
            });
            if !replaced {
                v.push(o);
            }
        }
    }
    v.sort_by_key(|o| (o.start, o.end));
    v
}

/// The simple repeat choice closest to an RRULE, and its end (UNTIL), for
/// the editor and for database queries.
pub fn approximate_rrule(rule: &str, tz: Option<Tz>) -> (RepeatFreq, Option<i64>) {
    let parts: Vec<(String, String)> = rule
        .trim_start_matches("RRULE:")
        .split(';')
        .filter_map(|p| p.split_once('=').map(|(a, b)| (a.trim().to_ascii_uppercase(), b.trim().to_string())))
        .collect();
    let get = |k: &str| parts.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    let freq = match get("FREQ").as_deref() {
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
        // HOURLY etc. still repeat; "Daily" keeps the event in queries.
        Some(_) => RepeatFreq::Daily,
        None => RepeatFreq::None,
    };
    let until = get("UNTIL").and_then(|u| parse_dt(&u, &[], tz).map(|(ts, date)| if date { ts + 86399 } else { ts }));
    (freq, until)
}

/// True when the editor's simple choices describe the rule exactly, so
/// editing it there won't lose anything.
pub fn rrule_is_simple(ev: &CalEvent) -> bool {
    let rule = ev.rrule.trim().trim_start_matches("RRULE:");
    if rule.is_empty() {
        return true;
    }
    let keys: Vec<String> = rule
        .split(';')
        .filter_map(|p| p.split_once('=').map(|(k, _)| k.trim().to_ascii_uppercase()))
        .collect();
    let (freq, _) = approximate_rrule(rule, find_tz(&ev.tz));
    let byday = rule.split(';').find_map(|p| p.strip_prefix("BYDAY=")).unwrap_or("");
    let allowed = |k: &str| matches!(k, "FREQ" | "UNTIL" | "WKST" | "BYDAY");
    keys.iter().all(|k| allowed(k))
        && match freq {
            RepeatFreq::Weekdays => true,
            RepeatFreq::Weekly => byday.split(',').count() <= 1 && !byday.chars().any(|c| c.is_ascii_digit() || c == '-'),
            _ => byday.is_empty(),
        }
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

/// Parses DTSTART/DTEND/EXDATE/RECURRENCE-ID values. Returns (timestamp,
/// is_date). `TZID` params are resolved (IANA, Windows and vendor names);
/// `zones` supplies fixed offsets from the file's VTIMEZONEs for unknown ones.
fn parse_dt(value: &str, params: &[(String, String)], default_tz: Option<Tz>) -> Option<(i64, bool)> {
    parse_dt_with(value, params, default_tz, &[])
}

fn parse_dt_with(
    value: &str,
    params: &[(String, String)],
    default_tz: Option<Tz>,
    zones: &[(String, i64)],
) -> Option<(i64, bool)> {
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
        return Some((Utc.from_utc_datetime(&dt).timestamp(), false));
    }
    let tzid = params.iter().find(|(k, _)| k == "TZID").map(|(_, v)| v.as_str());
    if let Some(id) = tzid {
        if let Some(z) = find_tz(id) {
            return Some((wall_to_ts(dt, Some(z)), false));
        }
        if let Some((_, offset)) = zones.iter().find(|(n, _)| n == id) {
            return Some((Utc.from_utc_datetime(&dt).timestamp() - offset, false));
        }
    }
    Some((wall_to_ts(dt, default_tz), false))
}

/// "+0130" / "-0500" to seconds east of UTC.
fn parse_offset(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, rest) = match v.as_bytes().first()? {
        b'-' => (-1, &v[1..]),
        b'+' => (1, &v[1..]),
        _ => (1, v),
    };
    let h: i64 = rest.get(0..2)?.parse().ok()?;
    let m: i64 = rest.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60))
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

/// Fixed offsets of the file's VTIMEZONE blocks (their STANDARD offset),
/// used for zone names no time-zone database knows.
fn vtimezone_offsets(lines: &[String]) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let mut tzid = None;
    let mut in_standard = false;
    for line in lines {
        let Some((name, _, value)) = split_prop(line) else { continue };
        let value = value.trim();
        match name.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VTIMEZONE") => tzid = None,
            "TZID" => tzid = Some(value.to_string()),
            "BEGIN" if value.eq_ignore_ascii_case("STANDARD") => in_standard = true,
            "END" if value.eq_ignore_ascii_case("STANDARD") => in_standard = false,
            "TZOFFSETTO" if in_standard => {
                if let (Some(id), Some(off)) = (&tzid, parse_offset(value)) {
                    if !out.iter().any(|(n, _): &(String, i64)| n == id) {
                        out.push((id.clone(), off));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn parse_ics(text: &str) -> IcsImport {
    let lines = unfold(text);
    let zones = vtimezone_offsets(&lines);
    let mut out = IcsImport::default();
    let mut cur: Option<CalEvent> = None;
    // Nesting inside the VEVENT: VALARM etc.
    let mut inner: Vec<String> = Vec::new();
    let mut has_end = false;
    let mut duration: Option<i64> = None;
    let mut alarm_trigger: Option<i64> = None;
    for line in lines {
        let Some((name, params, value)) = split_prop(&line) else { continue };
        let upper = value.trim().to_ascii_uppercase();
        match (name.as_str(), upper.as_str()) {
            ("BEGIN", "VEVENT") => {
                inner.clear();
                cur = Some(CalEvent::default());
                has_end = false;
                duration = None;
                alarm_trigger = None;
                continue;
            }
            ("BEGIN", other) => {
                if cur.is_some() {
                    inner.push(other.to_string());
                }
                continue;
            }
            ("END", "VEVENT") => {
                if let Some(mut e) = cur.take() {
                    if !has_end {
                        e.end = e.start + duration.unwrap_or(if e.all_day { 86400 } else { 0 });
                    }
                    if e.title.is_empty() {
                        e.title = "(no title)".into();
                    }
                    if !e.rrule.is_empty() {
                        let (freq, until) = approximate_rrule(&e.rrule, find_tz(&e.tz));
                        e.repeat = freq;
                        e.repeat_until = until;
                    }
                    if e.reminder_min.is_none() {
                        e.reminder_min = alarm_trigger;
                    }
                    out.events.push(e);
                }
                continue;
            }
            ("END", _) => {
                if cur.is_some() {
                    inner.pop();
                }
                continue;
            }
            _ => {}
        }
        if name == "METHOD" && cur.is_none() {
            out.method = upper.clone();
        }
        let Some(e) = cur.as_mut() else { continue };
        if !inner.is_empty() {
            // First display/audio alarm before the start becomes the reminder.
            if inner.last().map(|s| s.as_str()) == Some("VALARM") && name == "TRIGGER" && alarm_trigger.is_none() {
                let related_end = params.iter().any(|(k, v)| k == "RELATED" && v.eq_ignore_ascii_case("END"));
                if !related_end {
                    if let Some(secs) = parse_duration(value.trim()) {
                        if secs <= 0 {
                            alarm_trigger = Some(-secs / 60);
                        }
                    }
                }
            }
            continue;
        }
        let tzid = params.iter().find(|(k, _)| k == "TZID").map(|(_, v)| v.clone());
        match name.as_str() {
            "UID" => e.uid = value.trim().to_string(),
            "SUMMARY" => e.title = unescape(&value),
            "LOCATION" => e.location = unescape(&value),
            "DESCRIPTION" => e.description = unescape(&value),
            "DTSTART" => {
                if let Some((ts, date)) = parse_dt_with(&value, &params, None, &zones) {
                    e.start = ts;
                    e.all_day = date;
                    e.tz = if date {
                        String::new()
                    } else if value.trim().ends_with('Z') {
                        "UTC".into()
                    } else {
                        tzid.as_deref().and_then(find_tz).map(|z| z.name().to_string()).unwrap_or_default()
                    };
                }
            }
            "DTEND" => {
                if let Some((ts, _)) = parse_dt_with(&value, &params, None, &zones) {
                    e.end = ts;
                    has_end = true;
                }
            }
            "DURATION" => duration = parse_duration(value.trim()),
            "TRANSP" if upper == "TRANSPARENT" => e.show_as = ShowAs::Free,
            "STATUS" => {
                if upper == "CANCELLED" {
                    e.cancelled = true;
                } else if upper == "TENTATIVE" && e.show_as == ShowAs::Busy {
                    e.show_as = ShowAs::Tentative;
                }
            }
            "RECURRENCE-ID" => {
                e.recurrence_id = parse_dt_with(&value, &params, None, &zones).map(|(ts, _)| ts);
            }
            "EXDATE" => {
                for v in value.split(',') {
                    if let Some((ts, _)) = parse_dt_with(v, &params, None, &zones) {
                        e.exdates.push(ts);
                    }
                }
            }
            "CATEGORIES" if e.category.is_empty() => {
                e.category = unescape(value.split(',').next().unwrap_or("")).trim().to_string();
            }
            "URL" => {
                let u = value.trim();
                if u.starts_with("https://") && e.meeting_url.is_empty() {
                    e.meeting_url = u.to_string();
                }
            }
            "X-GOOGLE-CONFERENCE" | "X-MICROSOFT-SKYPETEAMSMEETINGURL" => e.meeting_url = value.trim().to_string(),
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
            "RRULE" => e.rrule = value.trim().to_string(),
            _ => {}
        }
    }
    out
}

/// Start times from an `EXDATE…:v1,v2` line (as in Google's `recurrence`).
pub fn parse_exdate_line(line: &str) -> Vec<i64> {
    let Some((name, params, value)) = split_prop(line.trim()) else { return Vec::new() };
    if name != "EXDATE" {
        return Vec::new();
    }
    value.split(',').filter_map(|v| parse_dt(v, &params, None).map(|(ts, _)| ts)).collect()
}

/// The events of one CalDAV resource / .ics file: a series (or single event)
/// plus its changed occurrences.
pub fn parse_calendar_resource(text: &str) -> Vec<CalEvent> {
    parse_ics(text).events
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

/// The zone an event's times are written in: its own (if known), else none
/// (times are then written in UTC).
fn write_tz(e: &CalEvent) -> Option<Tz> {
    if e.all_day || e.tz.eq_ignore_ascii_case("UTC") {
        return None;
    }
    find_tz(&e.tz)
}

/// `NAME;TZID=Zone:local` or `NAME:utcZ` / `NAME;VALUE=DATE:date`.
fn dt_prop(name: &str, ts: i64, e: &CalEvent) -> String {
    if e.all_day {
        return format!("{name};VALUE=DATE:{}", fmt_date(ts));
    }
    match write_tz(e) {
        Some(z) => format!("{name};TZID={}:{}", z.name(), ts_to_wall(ts, Some(z)).format("%Y%m%dT%H%M%S")),
        None => format!("{name}:{}", fmt_utc(ts)),
    }
}

/// The RRULE value for an event (its own rule, or the simple choice).
pub fn rrule_value(e: &CalEvent) -> Option<String> {
    if !e.rrule.trim().is_empty() {
        return Some(normalize_until(e.rrule.trim().trim_start_matches("RRULE:"), write_tz(e), e.all_day));
    }
    let tz = write_tz(e);
    let weekday = ts_to_wall(e.start, tz).weekday();
    let mut f = match e.repeat {
        RepeatFreq::None => return None,
        RepeatFreq::Daily => "FREQ=DAILY".to_string(),
        RepeatFreq::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string(),
        RepeatFreq::Weekly => format!(
            "FREQ=WEEKLY;BYDAY={}",
            match weekday {
                Weekday::Mon => "MO",
                Weekday::Tue => "TU",
                Weekday::Wed => "WE",
                Weekday::Thu => "TH",
                Weekday::Fri => "FR",
                Weekday::Sat => "SA",
                Weekday::Sun => "SU",
            }
        ),
        RepeatFreq::Monthly => "FREQ=MONTHLY".to_string(),
        RepeatFreq::Yearly => "FREQ=YEARLY".to_string(),
    };
    if let Some(u) = e.repeat_until {
        f.push_str(&format!(";UNTIL={}", fmt_utc(u)));
    }
    Some(f)
}

/// EXDATE lines for an event (one per date, same form as DTSTART).
pub fn exdate_lines(e: &CalEvent) -> Vec<String> {
    e.exdates.iter().map(|x| dt_prop("EXDATE", *x, e)).collect()
}

fn event_lines(e: &CalEvent, extra: &[String]) -> Vec<String> {
    let mut l = vec!["BEGIN:VEVENT".to_string()];
    l.push(format!("UID:{}", if e.uid.is_empty() { format!("{}@rustmail", e.id) } else { e.uid.clone() }));
    l.push(format!("DTSTAMP:{}", fmt_utc(util::now_ts())));
    if let Some(r) = e.recurrence_id {
        l.push(dt_prop("RECURRENCE-ID", r, e));
    }
    if e.all_day {
        l.push(dt_prop("DTSTART", e.start, e));
        l.push(dt_prop("DTEND", e.end.max(e.start + 86400), e));
    } else {
        l.push(dt_prop("DTSTART", e.start, e));
        l.push(dt_prop("DTEND", e.end.max(e.start), e));
    }
    l.push(format!("SUMMARY:{}", escape(&e.title)));
    if !e.location.is_empty() {
        l.push(format!("LOCATION:{}", escape(&e.location)));
    }
    if !e.description.is_empty() {
        l.push(format!("DESCRIPTION:{}", escape(&e.description)));
    }
    match e.show_as {
        ShowAs::Free => l.push("TRANSP:TRANSPARENT".into()),
        ShowAs::Tentative => l.push("STATUS:TENTATIVE".into()),
        _ => {}
    }
    if e.cancelled {
        l.push("STATUS:CANCELLED".into());
    }
    if !e.category.is_empty() {
        l.push(format!("CATEGORIES:{}", escape(&e.category)));
    }
    if e.recurrence_id.is_none() {
        if let Some(f) = rrule_value(e) {
            l.push(format!("RRULE:{f}"));
            l.extend(exdate_lines(e));
        }
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

/// A VTIMEZONE for an IANA zone, listing its offset changes around the
/// event (RDATE form), as RFC 5545 wants alongside TZID references.
fn vtimezone(z: Tz, around: i64) -> Vec<String> {
    let offset_at = |ts: i64| -> i64 {
        use chrono::Offset;
        Utc.timestamp_opt(ts, 0).single().map(|d| d.with_timezone(&z).offset().fix().local_minus_utc() as i64).unwrap_or(0)
    };
    let fmt_off = |o: i64| format!("{}{:02}{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 3600, (o.abs() % 3600) / 60);
    // Transitions from two years before to ten years after the event.
    let (from, to) = (around - 2 * 365 * 86400, around + 10 * 365 * 86400);
    let mut transitions: Vec<(i64, i64, i64)> = Vec::new(); // (utc ts, from, to)
    let mut t = from;
    let mut prev = offset_at(t);
    while t < to {
        let next = t + 6 * 3600;
        let o = offset_at(next);
        if o != prev {
            // Narrow down to the exact second of the change.
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if offset_at(mid) == prev { lo = mid } else { hi = mid }
            }
            transitions.push((hi, prev, o));
            prev = o;
        }
        t = next;
    }
    let mut l = vec!["BEGIN:VTIMEZONE".to_string(), format!("TZID:{}", z.name())];
    if transitions.is_empty() {
        let o = offset_at(around);
        l.push("BEGIN:STANDARD".into());
        l.push("DTSTART:19700101T000000".into());
        l.push(format!("TZOFFSETFROM:{}", fmt_off(o)));
        l.push(format!("TZOFFSETTO:{}", fmt_off(o)));
        l.push("END:STANDARD".into());
    } else {
        // Group onsets by (from, to): one STANDARD / DAYLIGHT block each.
        let mut groups: Vec<((i64, i64), Vec<i64>)> = Vec::new();
        for (ts, f, tt) in &transitions {
            match groups.iter_mut().find(|(k, _)| *k == (*f, *tt)) {
                Some((_, v)) => v.push(*ts),
                None => groups.push(((*f, *tt), vec![*ts])),
            }
        }
        for ((f, tt), onsets) in groups {
            let kind = if tt > f { "DAYLIGHT" } else { "STANDARD" };
            // Onsets are written in the local time before the change.
            let local = |ts: i64| (Utc.timestamp_opt(ts + f, 0).single().unwrap_or_default()).format("%Y%m%dT%H%M%S").to_string();
            l.push(format!("BEGIN:{kind}"));
            l.push(format!("DTSTART:{}", local(onsets[0])));
            l.push(format!("TZOFFSETFROM:{}", fmt_off(f)));
            l.push(format!("TZOFFSETTO:{}", fmt_off(tt)));
            if onsets.len() > 1 {
                l.push(format!("RDATE:{}", onsets[1..].iter().map(|o| local(*o)).collect::<Vec<_>>().join(",")));
            }
            l.push(format!("END:{kind}"));
        }
    }
    l.push("END:VTIMEZONE".into());
    l
}

fn vcalendar(events: &[CalEvent], method: Option<&str>, extra: &[String]) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//RustMail//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    if let Some(m) = method {
        lines.push(format!("METHOD:{m}"));
    }
    let mut zones: Vec<Tz> = Vec::new();
    for e in events {
        if let Some(z) = write_tz(e) {
            if !zones.contains(&z) {
                zones.push(z);
                lines.extend(vtimezone(z, e.start));
            }
        }
    }
    for e in events {
        lines.extend(event_lines(e, extra));
    }
    lines.push("END:VCALENDAR".into());
    lines.iter().map(|l| fold(l)).collect()
}

pub fn to_ics(events: &[CalEvent]) -> String {
    vcalendar(events, None, &[])
}

/// One CalDAV resource: a series and its changed occurrences (all sharing
/// the UID). Cancelled occurrences are written as EXDATEs on the series.
pub fn calendar_resource(events: &[CalEvent]) -> String {
    let mut list: Vec<CalEvent> = events.to_vec();
    let cancelled: Vec<i64> = list.iter().filter(|e| e.recurrence_id.is_some() && e.cancelled).filter_map(|e| e.recurrence_id).collect();
    list.retain(|e| !(e.recurrence_id.is_some() && e.cancelled));
    if let Some(master) = list.iter_mut().find(|e| e.recurrence_id.is_none()) {
        for c in cancelled {
            if !master.exdates.contains(&c) {
                master.exdates.push(c);
            }
        }
    }
    vcalendar(&list, None, &[])
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
    vcalendar(&[copy], Some("REPLY"), &extra)
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

    fn ny(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        wall_to_ts(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap(), find_tz("America/New_York"))
    }

    #[test]
    fn repeats_keep_wall_time_across_dst() {
        // Weekly Monday 9:30 in New York; DST ends 2026-11-01.
        let ev = CalEvent {
            uid: "w@x".into(),
            start: ny(2026, 10, 26, 9, 30),
            end: ny(2026, 10, 26, 10, 0),
            rrule: "FREQ=WEEKLY;BYDAY=MO;COUNT=3".into(),
            tz: "America/New_York".into(),
            ..Default::default()
        };
        let occ = expand(&ev, ny(2026, 10, 1, 0, 0), ny(2026, 12, 1, 0, 0));
        let starts: Vec<i64> = occ.iter().map(|o| o.start).collect();
        assert_eq!(starts, vec![ny(2026, 10, 26, 9, 30), ny(2026, 11, 2, 9, 30), ny(2026, 11, 9, 9, 30)]);
        // 13:30 UTC before the change, 14:30 UTC after it.
        assert_eq!(starts[1] - starts[0], 7 * 86400 + 3600);
    }

    #[test]
    fn complex_rules_exdates_and_overrides() {
        let master = CalEvent {
            calendar_id: 5,
            uid: "m@x".into(),
            title: "Board".into(),
            start: ny(2026, 11, 9, 9, 30),
            end: ny(2026, 11, 9, 10, 30),
            rrule: "FREQ=MONTHLY;BYDAY=2MO;COUNT=4".into(),
            tz: "America/New_York".into(),
            exdates: vec![ny(2026, 12, 14, 9, 30)],
            ..Default::default()
        };
        let moved = CalEvent {
            calendar_id: 5,
            uid: "m@x".into(),
            title: "Board (moved)".into(),
            recurrence_id: Some(ny(2027, 1, 11, 9, 30)),
            start: ny(2027, 1, 12, 14, 0),
            end: ny(2027, 1, 12, 15, 0),
            ..Default::default()
        };
        let cancelled = CalEvent {
            calendar_id: 5,
            uid: "m@x".into(),
            recurrence_id: Some(ny(2027, 2, 8, 9, 30)),
            start: ny(2027, 2, 8, 9, 30),
            end: ny(2027, 2, 8, 10, 30),
            cancelled: true,
            ..Default::default()
        };
        let all = occurrences(&[master, moved, cancelled], ny(2026, 11, 1, 0, 0), ny(2027, 3, 1, 0, 0));
        let got: Vec<(i64, &str)> = all.iter().map(|o| (o.start, o.event.title.as_str())).collect();
        assert_eq!(got, vec![(ny(2026, 11, 9, 9, 30), "Board"), (ny(2027, 1, 12, 14, 0), "Board (moved)")]);
    }

    #[test]
    fn zone_names() {
        assert_eq!(find_tz("Eastern Standard Time").map(|z| z.name()), Some("America/New_York"));
        assert_eq!(find_tz("/mozilla.org/20050126_1/Europe/Berlin").map(|z| z.name()), Some("Europe/Berlin"));
        assert_eq!(find_tz("Europe/Paris").map(|z| z.name()), Some("Europe/Paris"));
        assert!(find_tz("Mars/Olympus").is_none());
    }

    #[test]
    fn all_day_rule_with_date_until() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let ev = CalEvent {
            uid: "a@x".into(),
            start: util::local_midnight(d),
            end: util::local_midnight(d + Duration::days(1)),
            all_day: true,
            rrule: "FREQ=WEEKLY;INTERVAL=2;UNTIL=20261201".into(),
            ..Default::default()
        };
        let occ = expand(&ev, util::local_midnight(d), util::local_midnight(d + Duration::days(60)));
        let days: Vec<NaiveDate> = occ.iter().map(|o| util::local(o.start).date_naive()).collect();
        assert_eq!(days, vec![d, d + Duration::days(14), d + Duration::days(28)]);
    }

    #[test]
    fn resource_roundtrip_with_zones() {
        let master = CalEvent {
            uid: "r@x".into(),
            title: "Review".into(),
            start: ny(2026, 11, 3, 15, 0),
            end: ny(2026, 11, 3, 16, 0),
            rrule: "FREQ=WEEKLY;BYDAY=TU;UNTIL=20261231T235959Z".into(),
            tz: "America/New_York".into(),
            exdates: vec![ny(2026, 11, 24, 15, 0)],
            reminder_min: Some(10),
            ..Default::default()
        };
        let moved = CalEvent {
            uid: "r@x".into(),
            title: "Review (later)".into(),
            recurrence_id: Some(ny(2026, 12, 1, 15, 0)),
            start: ny(2026, 12, 1, 17, 0),
            end: ny(2026, 12, 1, 18, 0),
            tz: "America/New_York".into(),
            ..Default::default()
        };
        let gone = CalEvent { uid: "r@x".into(), recurrence_id: Some(ny(2026, 12, 8, 15, 0)), cancelled: true, ..moved.clone() };
        let text = calendar_resource(&[master.clone(), moved.clone(), gone]);
        assert!(text.contains("BEGIN:VTIMEZONE") && text.contains("TZID:America/New_York"));
        assert!(text.contains("DTSTART;TZID=America/New_York:20261103T150000"));
        assert!(text.contains("RECURRENCE-ID;TZID=America/New_York:20261201T150000"));
        let back = parse_calendar_resource(&text);
        assert_eq!(back.len(), 2, "cancelled occurrence became an EXDATE");
        let m = back.iter().find(|e| e.recurrence_id.is_none()).unwrap();
        assert_eq!((m.start, m.end, m.tz.as_str()), (master.start, master.end, "America/New_York"));
        assert_eq!(m.reminder_min, Some(10));
        assert_eq!(m.exdates.len(), 2);
        assert!(m.exdates.contains(&ny(2026, 12, 8, 15, 0)));
        let o = back.iter().find(|e| e.recurrence_id.is_some()).unwrap();
        assert_eq!((o.recurrence_id, o.start), (moved.recurrence_id, moved.start));
        let all = occurrences(&back, ny(2026, 11, 1, 0, 0), ny(2027, 1, 1, 0, 0));
        assert_eq!(all.len(), 7, "9 Tuesdays minus one EXDATE and one cancelled");
    }

    #[test]
    fn outlook_style_invite() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:Customized Time Zone\r\nBEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:o1\r\nSUMMARY:Sync\r\nDTSTART;TZID=W. Europe Standard Time:20261110T100000\r\nDTEND;TZID=Customized Time Zone:20261110T110000\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &parse_ics(ics).events[0];
        let berlin = wall_to_ts(NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(10, 0, 0).unwrap(), find_tz("Europe/Berlin"));
        assert_eq!(e.start, berlin);
        assert_eq!(e.end, berlin + 3600, "unknown zone read via its VTIMEZONE offset");
        assert_eq!(e.tz, "Europe/Berlin");
        assert_eq!(e.reminder_min, Some(15));
    }

    #[test]
    fn vtimezone_has_both_offsets() {
        let z = find_tz("Europe/Berlin").unwrap();
        let lines = vtimezone(z, ny(2026, 11, 3, 0, 0)).join("\n");
        assert!(lines.contains("BEGIN:DAYLIGHT") && lines.contains("TZOFFSETTO:+0200"));
        assert!(lines.contains("BEGIN:STANDARD") && lines.contains("TZOFFSETTO:+0100"));
        // Germany falls back at 03:00 local summer time on the last Sunday of October.
        assert!(lines.contains("20261025T030000"), "{lines}");
    }

    #[test]
    fn simple_rules_are_detected() {
        let mut e = CalEvent { rrule: "FREQ=WEEKLY;BYDAY=TU".into(), ..Default::default() };
        assert!(rrule_is_simple(&e));
        e.rrule = "FREQ=MONTHLY;BYDAY=2MO".into();
        assert!(!rrule_is_simple(&e));
        e.rrule = "FREQ=DAILY;INTERVAL=2".into();
        assert!(!rrule_is_simple(&e));
    }

    #[test]
    fn exdate_lines() {
        let v = parse_exdate_line("EXDATE;TZID=America/New_York:20261124T150000,20261201T150000");
        assert_eq!(v, vec![ny(2026, 11, 24, 15, 0), ny(2026, 12, 1, 15, 0)]);
        assert_eq!(parse_exdate_line("EXDATE:20261124T200000Z"), vec![ny(2026, 11, 24, 15, 0)]);
        assert!(parse_exdate_line("RRULE:FREQ=DAILY").is_empty());
    }
}
