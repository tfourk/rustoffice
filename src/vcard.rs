//! vCard 3.0/4.0 import and export for contacts.

use crate::model::Contact;

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
    v.replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(';', "\\;")
        .replace('\n', "\\n")
}

pub fn parse(text: &str) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut cur: Option<Contact> = None;
    let mut n_name = String::new();
    for line in unfold(text) {
        let Some((head, value)) = line.split_once(':') else { continue };
        let name = head
            .split(';')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match name.as_str() {
            "BEGIN" if value.trim().eq_ignore_ascii_case("VCARD") => {
                cur = Some(Contact::default());
                n_name.clear();
            }
            "END" if value.trim().eq_ignore_ascii_case("VCARD") => {
                if let Some(mut c) = cur.take() {
                    if c.name.trim().is_empty() {
                        c.name = n_name.clone();
                    }
                    if !c.name.trim().is_empty() || !c.emails.is_empty() {
                        out.push(c);
                    }
                }
            }
            _ => {
                let Some(c) = cur.as_mut() else { continue };
                let v = unescape(value.trim());
                match name.as_str() {
                    "FN" => c.name = v,
                    "N" => {
                        let parts: Vec<&str> = value.split(';').collect();
                        let given = parts.get(1).copied().unwrap_or("");
                        let family = parts.first().copied().unwrap_or("");
                        n_name = format!("{} {}", unescape(given), unescape(family)).trim().to_string();
                    }
                    "EMAIL" => {
                        if !v.is_empty() {
                            c.emails.push(v.trim_start_matches("mailto:").to_string());
                        }
                    }
                    "TEL" => {
                        if !v.is_empty() {
                            c.phones.push(v.trim_start_matches("tel:").to_string());
                        }
                    }
                    "ORG" => c.company = v.split(';').next().unwrap_or("").to_string(),
                    "TITLE" => c.job_title = v,
                    "URL" => c.website = v,
                    "NOTE" => c.notes = v,
                    "BDAY" => {
                        let d = v.replace('-', "");
                        if d.len() >= 8 {
                            c.birthday = format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]);
                        }
                    }
                    "ADR" => {
                        c.address = value
                            .split(';')
                            .map(|s| unescape(s.trim()))
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(", ");
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

pub fn to_vcard(contacts: &[Contact]) -> String {
    let mut s = String::new();
    for c in contacts {
        s.push_str("BEGIN:VCARD\r\nVERSION:3.0\r\n");
        s.push_str(&format!("FN:{}\r\n", escape(&c.display_name())));
        let mut parts = c.name.trim().rsplitn(2, ' ');
        let family = parts.next().unwrap_or("");
        let given = parts.next().unwrap_or("");
        s.push_str(&format!("N:{};{};;;\r\n", escape(family), escape(given)));
        for e in &c.emails {
            s.push_str(&format!("EMAIL;TYPE=INTERNET:{}\r\n", escape(e)));
        }
        for p in &c.phones {
            s.push_str(&format!("TEL:{}\r\n", escape(p)));
        }
        if !c.company.is_empty() {
            s.push_str(&format!("ORG:{}\r\n", escape(&c.company)));
        }
        if !c.job_title.is_empty() {
            s.push_str(&format!("TITLE:{}\r\n", escape(&c.job_title)));
        }
        if !c.address.is_empty() {
            s.push_str(&format!("ADR:;;{};;;;\r\n", escape(&c.address)));
        }
        if !c.website.is_empty() {
            s.push_str(&format!("URL:{}\r\n", escape(&c.website)));
        }
        if !c.birthday.is_empty() {
            s.push_str(&format!("BDAY:{}\r\n", c.birthday));
        }
        if !c.notes.is_empty() {
            s.push_str(&format!("NOTE:{}\r\n", escape(&c.notes)));
        }
        s.push_str("END:VCARD\r\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = Contact {
            name: "Jane Q. Doe".into(),
            emails: vec!["jane@example.com".into(), "jd@work.example".into()],
            phones: vec!["+1 555 0100".into()],
            company: "Acme, Inc.".into(),
            job_title: "Engineer".into(),
            birthday: "1990-04-12".into(),
            notes: "Line one\nLine two".into(),
            ..Default::default()
        };
        let text = to_vcard(&[c.clone()]);
        let back = parse(&text);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, c.name);
        assert_eq!(back[0].emails, c.emails);
        assert_eq!(back[0].company, c.company);
        assert_eq!(back[0].notes, c.notes);
        assert_eq!(back[0].birthday, c.birthday);
    }
}
