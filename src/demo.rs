//! A self-contained demo mailbox (offline "Local" account) so the app can be
//! explored without configuring a real account. All names and addresses are
//! fictional (example.com / example.org / example.net).

use anyhow::Result;
use base64::Engine;
use chrono::{Datelike, Duration};

use crate::config::{AccountConfig, AccountKind, AppConfig};
use crate::model::*;
use crate::store::{NewMessage, Store};
use crate::util;

pub const DEMO_EMAIL: &str = "alex.morgan@example.com";

fn enc_header(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
        )
    }
}

fn addr_header(name: &str, email: &str) -> String {
    if name.is_empty() {
        email.to_string()
    } else {
        format!("{} <{}>", enc_header(name), email)
    }
}

fn b64_lines(data: &[u8]) -> String {
    let enc = base64::engine::general_purpose::STANDARD.encode(data);
    enc.as_bytes()
        .chunks(76)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join("\r\n")
}

struct Att {
    name: String,
    mime: String,
    data: Vec<u8>,
    cid: Option<String>,
}

#[derive(Default)]
struct Mail<'a> {
    folder: &'a str,
    from: (&'a str, &'a str),
    to: Vec<(&'a str, &'a str)>,
    cc: Vec<(&'a str, &'a str)>,
    subject: &'a str,
    minutes_ago: i64,
    text: String,
    html: Option<String>,
    seen: bool,
    flagged: bool,
    pinned: bool,
    answered: bool,
    draft: bool,
    high: bool,
    bulk: bool,
    categories: Vec<&'a str>,
    id: &'a str,
    reply_to: Option<&'a str>,
    refs: Vec<&'a str>,
    atts: Vec<Att>,
    calendar: Option<String>,
}

fn build_raw(m: &Mail, date: i64) -> Vec<u8> {
    let mut h = String::new();
    h.push_str(&format!("From: {}\r\n", addr_header(m.from.0, m.from.1)));
    if !m.to.is_empty() {
        h.push_str(&format!(
            "To: {}\r\n",
            m.to.iter().map(|(n, e)| addr_header(n, e)).collect::<Vec<_>>().join(", ")
        ));
    }
    if !m.cc.is_empty() {
        h.push_str(&format!(
            "Cc: {}\r\n",
            m.cc.iter().map(|(n, e)| addr_header(n, e)).collect::<Vec<_>>().join(", ")
        ));
    }
    h.push_str(&format!("Subject: {}\r\n", enc_header(m.subject)));
    h.push_str(&format!("Date: {}\r\n", util::local(date).to_rfc2822()));
    h.push_str(&format!("Message-ID: <{}@demo.example.com>\r\n", m.id));
    if let Some(r) = m.reply_to {
        h.push_str(&format!("In-Reply-To: <{r}@demo.example.com>\r\n"));
    }
    if !m.refs.is_empty() {
        h.push_str(&format!(
            "References: {}\r\n",
            m.refs.iter().map(|r| format!("<{r}@demo.example.com>")).collect::<Vec<_>>().join(" ")
        ));
    }
    if m.high {
        h.push_str("Importance: high\r\nX-Priority: 1\r\n");
    }
    if m.bulk {
        h.push_str(&format!(
            "List-Unsubscribe: <https://{}/unsubscribe>\r\nPrecedence: bulk\r\n",
            util::domain_of(m.from.1)
        ));
    }
    h.push_str("MIME-Version: 1.0\r\n");

    let text_part = format!(
        "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n",
        m.text.replace('\n', "\r\n")
    );
    let body_part = match &m.html {
        Some(html) => format!(
            "Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\n{}--alt\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n--alt--\r\n",
            text_part, html
        ),
        None => text_part,
    };
    let inline: Vec<&Att> = m.atts.iter().filter(|a| a.cid.is_some()).collect();
    let files: Vec<&Att> = m.atts.iter().filter(|a| a.cid.is_none()).collect();
    let body_part = if inline.is_empty() {
        body_part
    } else {
        let mut s = format!("Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n--rel\r\n{body_part}");
        for a in inline {
            s.push_str(&format!(
                "--rel\r\nContent-Type: {}\r\nContent-Transfer-Encoding: base64\r\nContent-ID: <{}>\r\nContent-Disposition: inline; filename=\"{}\"\r\n\r\n{}\r\n",
                a.mime,
                a.cid.as_deref().unwrap_or(""),
                a.name,
                b64_lines(&a.data)
            ));
        }
        s.push_str("--rel--\r\n");
        s
    };
    let mut out = h;
    if files.is_empty() && m.calendar.is_none() {
        out.push_str(&body_part);
    } else {
        out.push_str("Content-Type: multipart/mixed; boundary=\"mixed\"\r\n\r\n--mixed\r\n");
        out.push_str(&body_part);
        if let Some(ics) = &m.calendar {
            out.push_str(&format!(
                "--mixed\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST; name=\"invite.ics\"\r\nContent-Disposition: attachment; filename=\"invite.ics\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                b64_lines(ics.as_bytes())
            ));
        }
        for a in files {
            out.push_str(&format!(
                "--mixed\r\nContent-Type: {}; name=\"{}\"\r\nContent-Disposition: attachment; filename=\"{}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                a.mime,
                a.name,
                a.name,
                b64_lines(&a.data)
            ));
        }
        out.push_str("--mixed--\r\n");
    }
    out.into_bytes()
}

fn banner_png() -> Vec<u8> {
    let (w, h) = (640u32, 220u32);
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        // Sunset gradient with soft "hills".
        let hill = (0.62 + 0.08 * (fx * 9.0).sin() + 0.05 * (fx * 23.0).cos()) as f32;
        if fy > hill {
            let t = (fy - hill) / (1.0 - hill);
            image::Rgb([(30.0 + 20.0 * t) as u8, (70.0 + 30.0 * t) as u8, (60.0 + 10.0 * t) as u8])
        } else {
            let sun = ((fx - 0.72).powi(2) + (fy - 0.42).powi(2)).sqrt();
            if sun < 0.08 {
                image::Rgb([255, 236, 179])
            } else {
                let t = fy / hill;
                image::Rgb([
                    (250.0 - 60.0 * t) as u8,
                    (140.0 + 40.0 * (1.0 - t)) as u8,
                    (90.0 + 110.0 * t) as u8,
                ])
            }
        }
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    let _ = image::DynamicImage::ImageRgb8(img).write_to(&mut buf, image::ImageFormat::Png);
    buf.into_inner()
}

fn tiny_pdf(title: &str) -> Vec<u8> {
    format!(
        "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n4 0 obj<</Length 60>>stream\nBT /F1 24 Tf 72 700 Td ({title}) Tj ET\nendstream endobj\n5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n"
    )
    .into_bytes()
}

const ME: (&str, &str) = ("Alex Morgan", DEMO_EMAIL);
const PRIYA: (&str, &str) = ("Priya Shah", "priya.shah@example.com");
const MARCUS: (&str, &str) = ("Marcus Chen", "marcus.chen@example.com");
const SOFIA: (&str, &str) = ("Sofia Rossi", "sofia.rossi@example.org");
const KIM: (&str, &str) = ("Kim Lee", "kim.lee@example.com");
const DANIEL: (&str, &str) = ("Daniel Okafor", "daniel.okafor@example.com");
const HANNAH: (&str, &str) = ("Hannah Weber", "hannah.weber@example.net");
const JAMAL: (&str, &str) = ("Jamal Wright", "jamal.wright@example.com");
const ELENA: (&str, &str) = ("Elena García", "elena.garcia@example.com");
const TOM: (&str, &str) = ("Tom Becker", "tom.becker@example.org");
const AIKO: (&str, &str) = ("Aiko Tanaka", "aiko.tanaka@example.com");

fn style_wrap(inner: &str) -> String {
    format!(
        "<html><body style=\"font-family: 'Segoe UI', Arial, sans-serif; font-size: 14px; color: #242424;\">{inner}</body></html>"
    )
}

fn mails() -> Vec<Mail<'static>> {
    let day = 24 * 60;
    let mut v = Vec::new();

    v.push(Mail {
        folder: "INBOX",
        from: PRIYA,
        to: vec![ME],
        cc: vec![MARCUS],
        subject: "Q4 roadmap review — slides attached",
        minutes_ago: 22,
        high: true,
        categories: vec!["Work"],
        id: "q4-roadmap",
        text: "Hi Alex,\n\nAttached are the slides for Thursday's Q4 roadmap review. Key points:\n\n- Apollo launch moves to November 18\n- We are adding two engineers to the sync team\n- Budget request for the design system is approved\n\nCould you review sections 3 and 4 before tomorrow noon?\n\nThanks!\nPriya".into(),
        html: Some(style_wrap(
            "<p>Hi Alex,</p><p>Attached are the slides for Thursday's <b>Q4 roadmap review</b>. Key points:</p>\
             <ul><li>Apollo launch moves to <b>November 18</b></li><li>We are adding two engineers to the sync team</li>\
             <li>Budget request for the design system is <span style=\"color:#107C10\"><b>approved</b></span></li></ul>\
             <p>Could you review sections 3 and 4 before <u>tomorrow noon</u>? The live version is on the \
             <a href=\"https://wiki.example.com/q4-roadmap\">team wiki</a>.</p><p>Thanks!<br>Priya</p>\
             <p style=\"color:#707070;font-size:12px\">Priya Shah · Director of Product · Example Corp</p>",
        )),
        atts: vec![Att {
            name: "Q4-Roadmap.pdf".into(),
            mime: "application/pdf".into(),
            data: tiny_pdf("Q4 Roadmap"),
            cid: None,
        }],
        ..Default::default()
    });

    // A three message conversation.
    v.push(Mail {
        folder: "INBOX",
        from: MARCUS,
        to: vec![ME],
        subject: "Apollo launch checklist",
        minutes_ago: day + 190,
        seen: true,
        answered: true,
        id: "apollo-1",
        categories: vec!["Work"],
        text: "Alex — here's the launch checklist draft. Can you own the comms items?\n\n1. Release notes\n2. Status page update\n3. Customer email\n4. Internal announcement\n\nMarcus".into(),
        html: Some(style_wrap(
            "<p>Alex — here's the launch checklist draft. Can you own the <b>comms</b> items?</p>\
             <ol><li>Release notes</li><li>Status page update</li><li>Customer email</li><li>Internal announcement</li></ol><p>Marcus</p>",
        )),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Sent",
        from: ME,
        to: vec![MARCUS],
        subject: "RE: Apollo launch checklist",
        minutes_ago: day + 100,
        seen: true,
        id: "apollo-2",
        reply_to: Some("apollo-1"),
        refs: vec!["apollo-1"],
        text: "Happy to own comms. I'll have a draft of the customer email by Wednesday.\n\nOne question: do we need legal review for the status page wording?\n\nAlex\n\n> Alex — here's the launch checklist draft. Can you own the comms items?".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "INBOX",
        from: MARCUS,
        to: vec![ME],
        cc: vec![HANNAH],
        subject: "RE: Apollo launch checklist",
        minutes_ago: 95,
        id: "apollo-3",
        reply_to: Some("apollo-2"),
        refs: vec!["apollo-1", "apollo-2"],
        categories: vec!["Work"],
        text: "Good call — looping in Hannah from legal. Hannah, can you take a quick look at the status page copy?\n\nThanks,\nMarcus\n\n> Happy to own comms. I'll have a draft of the customer email by Wednesday.".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: KIM,
        to: vec![ME, PRIYA],
        subject: "Invitation: Design review @ Thursday 2:00 PM",
        minutes_ago: 160,
        id: "invite-design",
        text: "Kim Lee has invited you to Design review.\n\nWhen: Thursday 2:00 PM – 3:00 PM\nWhere: Conference room Juniper / Online\n\nAgenda: navigation redesign, dark mode polish, accessibility audit.".into(),
        html: Some(style_wrap(
            "<p><b>Kim Lee</b> has invited you to <b>Design review</b>.</p><p>Agenda:</p>\
             <ul><li>Navigation redesign</li><li>Dark mode polish</li><li>Accessibility audit results</li></ul>",
        )),
        calendar: Some(String::new()),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: SOFIA,
        to: vec![ME],
        subject: "Lunch on Thursday?",
        minutes_ago: 240,
        seen: true,
        id: "lunch",
        categories: vec!["Personal"],
        text: "Hey Alex!\n\nAre you free for lunch on Thursday? There's a new place on 5th street — Café Lumen — that does amazing ramen.\n\n12:30 works for me.\n\nSofia".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: JAMAL,
        to: vec![ME],
        subject: "Photos from the offsite 📸",
        minutes_ago: 3 * day + 30,
        seen: true,
        pinned: true,
        id: "offsite-photos",
        text: "Here's my favorite shot from the offsite. More in the shared album!\n\nJamal".into(),
        html: Some(style_wrap(
            "<p>Here's my favorite shot from the offsite — the sunset hike was a highlight.</p>\
             <p><img src=\"cid:sunset@demo\" alt=\"Sunset over the hills\" width=\"560\"></p>\
             <p>More in the <a href=\"https://photos.example.com/album/offsite\">shared album</a>!</p><p>Jamal</p>",
        )),
        atts: vec![Att {
            name: "sunset.png".into(),
            mime: "image/png".into(),
            data: banner_png(),
            cid: Some("sunset@demo".into()),
        }],
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: DANIEL,
        to: vec![ME],
        subject: "Expense report approved",
        minutes_ago: day + 30,
        seen: true,
        flagged: true,
        id: "expense",
        categories: vec!["Finance"],
        text: "Hi Alex,\n\nYour expense report EXP-2041 ($1,284.50) has been approved and will be reimbursed in the next payroll cycle.\n\nPlease keep the original receipts for 90 days.\n\nDaniel Okafor\nFinance".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: HANNAH,
        to: vec![ME],
        subject: "RE: Contract draft v3",
        minutes_ago: 2 * day + 300,
        seen: true,
        flagged: true,
        id: "contract",
        text: "Alex,\n\nI've marked up the indemnification clause (section 7.2) and the data processing addendum. The vendor's changes to the liability cap are not acceptable as written.\n\nRedline attached. Happy to walk through it tomorrow.\n\nHannah".into(),
        atts: vec![Att {
            name: "Contract-v3-redline.docx".into(),
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
            data: b"PK\x03\x04 demo document".to_vec(),
            cid: None,
        }],
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: ELENA,
        to: vec![ME],
        subject: "Customer escalation: Order #48213",
        minutes_ago: 4 * day + 80,
        seen: true,
        high: true,
        id: "escalation",
        text: "Alex, Blue Harbor Logistics reported that sync stopped for 40 of their users after the 2.3 update. Support has a workaround but they want a call with product.\n\nCan you join a 30 min call this week?\n\nElena".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: AIKO,
        to: vec![ME],
        subject: "Welcome to the team, Leo!",
        minutes_ago: 6 * day,
        seen: true,
        id: "welcome",
        text: "Hi everyone,\n\nPlease welcome Leo Martins, who joins us as a senior engineer on the sync team. Leo comes from a background in distributed databases and loves bouldering.\n\nSay hi in #general!\n\nAiko\nPeople team".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: TOM,
        to: vec![ME],
        subject: "Board deck feedback",
        minutes_ago: 9 * day,
        seen: true,
        id: "board",
        text: "Alex, a few notes on the board deck:\n\n- Slide 4: lead with the retention number\n- Slide 9: the chart needs a source\n- Consider cutting the appendix in half\n\nOverall strong. Tom".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: PRIYA,
        to: vec![ME],
        subject: "Hiring plan for H1",
        minutes_ago: 16 * day,
        seen: true,
        id: "hiring",
        text: "Here's the draft hiring plan for H1. We're asking for 6 new roles across product and engineering.\n\nPriya".into(),
        ..Default::default()
    });

    v.push(Mail {
        folder: "INBOX",
        from: SOFIA,
        to: vec![ME],
        subject: "Coffee chat?",
        minutes_ago: 34 * day,
        seen: true,
        id: "coffee",
        text: "Would love to pick your brain about product roles sometime. Coffee next week?\n\nSofia".into(),
        ..Default::default()
    });

    // "Other" inbox: newsletters and notifications.
    v.push(Mail {
        folder: "INBOX",
        from: ("This Week in Rust", "newsletter@rustweekly.example.org"),
        to: vec![ME],
        subject: "This Week in Rust — Issue 612",
        minutes_ago: 300,
        bulk: true,
        id: "twir",
        text: "Hello and welcome to another issue of This Week in Rust!\n\nUpdates from the Rust project, crate of the week, call for participation, and upcoming events.".into(),
        html: Some(
            "<html><body style=\"margin:0;background:#f4f4f4\"><table width=\"100%\" bgcolor=\"#f4f4f4\"><tr><td align=\"center\">\
             <table width=\"600\" bgcolor=\"#ffffff\"><tr><td style=\"padding:24px\">\
             <h1 style=\"color:#b7410e;font-size:28px\">This Week in Rust</h1>\
             <p style=\"color:#555\">Issue 612 · Hello and welcome to another issue!</p>\
             <img src=\"https://images.example.org/twir/header.png\" width=\"552\" height=\"120\" alt=\"Rust banner\">\
             <h2>Updates from the Rust Project</h2>\
             <ul><li><a href=\"https://blog.example.org/async-closures\">Async closures are stable</a></li>\
             <li><a href=\"https://blog.example.org/compile-times\">Compile times improved by 12% this release cycle</a></li></ul>\
             <h2>Crate of the Week</h2><p>This week's crate is <code>egui</code>, an easy-to-use immediate mode GUI library.</p>\
             <table cellpadding=\"10\"><tr><td bgcolor=\"#b7410e\"><a href=\"https://rustweekly.example.org/612\" style=\"color:#ffffff;text-decoration:none\"><b>Read the full issue</b></a></td></tr></table>\
             <p style=\"font-size:11px;color:#999\">You are receiving this because you subscribed. <a href=\"https://rustweekly.example.org/unsubscribe\">Unsubscribe</a></p>\
             </td></tr></table></td></tr></table><img src=\"https://track.example.org/open.gif\" width=\"1\" height=\"1\"></body></html>".into(),
        ),
        ..Default::default()
    });
    v.push(Mail {
        folder: "INBOX",
        from: ("ErgoHome Store", "deals@ergohome.example.net"),
        to: vec![ME],
        subject: "Flash sale: 30% off ergonomic chairs this weekend",
        minutes_ago: day + 400,
        bulk: true,
        id: "sale",
        text: "This weekend only: 30% off all ergonomic chairs and standing desks. Use code COMFY30.".into(),
        html: Some(
            "<html><body><div style=\"text-align:center\"><h1 style=\"color:#0b5\">Weekend Flash Sale</h1>\
             <p style=\"font-size:18px\"><b>30% off</b> all ergonomic chairs and standing desks.</p>\
             <p>Use code <span style=\"background:#ffe066;padding:2px 6px\"><b>COMFY30</b></span> at checkout.</p>\
             <p><a href=\"https://ergohome.example.net/sale\">Shop the sale →</a></p></div></body></html>".into(),
        ),
        ..Default::default()
    });
    v.push(Mail {
        folder: "INBOX",
        from: ("Example Bank", "no-reply@bank.example.com"),
        to: vec![ME],
        subject: "Your monthly statement is ready",
        minutes_ago: 2 * day + 60,
        seen: true,
        bulk: true,
        id: "statement",
        text: "Your October statement for the account ending in 4821 is now available in online banking.".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "INBOX",
        from: ("GitForge", "notifications@gitforge.example.com"),
        to: vec![ME],
        subject: "[rustmail/app] Sync stalls on large folders (#212)",
        minutes_ago: 50,
        bulk: true,
        id: "issue212",
        text: "@alex-morgan was mentioned in issue #212:\n\nWhen a folder has more than 50k messages the initial sync appears to stall at 'Syncing Archive…'.\n\nReply to this email directly or view it on GitForge.".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "INBOX",
        from: ("SkyLink Airways", "noreply@skylink.example.com"),
        to: vec![ME],
        subject: "Your flight to Lisbon is confirmed",
        minutes_ago: 5 * day + 200,
        seen: true,
        bulk: true,
        id: "flight",
        categories: vec!["Travel"],
        text: "Booking reference: QX7P2L\nFlight SL 482 — departs 07:45, arrives 11:30.\nSeat 14A.".into(),
        ..Default::default()
    });

    // Other folders.
    v.push(Mail {
        folder: "Sent",
        from: ME,
        to: vec![SOFIA],
        subject: "RE: Coffee chat?",
        minutes_ago: 33 * day,
        seen: true,
        id: "coffee-re",
        reply_to: Some("coffee"),
        refs: vec!["coffee"],
        text: "Absolutely — how about Tuesday at 10?\n\nAlex".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Sent",
        from: ME,
        to: vec![TOM],
        subject: "Board deck v2",
        minutes_ago: 8 * day,
        seen: true,
        id: "board-v2",
        text: "Tom — updated per your notes. Retention now leads, sources added, appendix trimmed.\n\nAlex".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Drafts",
        from: ME,
        to: vec![PRIYA, MARCUS],
        subject: "Proposal: async standups",
        minutes_ago: 70,
        seen: true,
        draft: true,
        id: "draft-standups",
        text: "Hi both,\n\nWhat if we moved Monday/Friday standups to an async written update? Draft proposal below.\n\n".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Archive",
        from: TOM,
        to: vec![ME],
        subject: "Quarterly all-hands recording",
        minutes_ago: 40 * day,
        seen: true,
        id: "allhands",
        text: "The recording of the Q3 all-hands is now available on the intranet.".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Archive",
        from: ELENA,
        to: vec![ME],
        subject: "Customer advisory board notes",
        minutes_ago: 45 * day,
        seen: true,
        id: "cab",
        text: "Notes from this quarter's customer advisory board are in the shared drive.".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Junk Email",
        from: ("Prize Center", "winner@prizes.example.net"),
        to: vec![ME],
        subject: "You've won a luxury cruise!!! Claim now",
        minutes_ago: day * 2,
        bulk: true,
        id: "spam1",
        text: "Congratulations! Click here to claim your prize. Offer expires in 24 hours!".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Deleted Items",
        from: ("Webinar Team", "events@webinars.example.org"),
        to: vec![ME],
        subject: "Reminder: Productivity webinar starts in 1 hour",
        minutes_ago: 7 * day,
        seen: true,
        bulk: true,
        id: "webinar",
        text: "Join us for 10 productivity tips in 30 minutes.".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Projects/Apollo",
        from: MARCUS,
        to: vec![ME],
        subject: "Apollo beta feedback summary",
        minutes_ago: 3 * day,
        seen: true,
        id: "beta",
        categories: vec!["Work"],
        text: "Top beta feedback themes: faster search (42%), offline mode (31%), better calendar invites (18%).".into(),
        ..Default::default()
    });
    v.push(Mail {
        folder: "Receipts",
        from: ("Cloud Storage Co", "billing@cloudstorage.example.com"),
        to: vec![ME],
        subject: "Receipt for your 2 TB plan",
        minutes_ago: 12 * day,
        seen: true,
        id: "receipt",
        categories: vec!["Finance"],
        text: "Thanks for your payment of $9.99 for the 2 TB plan (Nov).".into(),
        ..Default::default()
    });
    v
}

fn invite_ics(start: i64) -> String {
    let fmt = |ts: i64| {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|d| d.format("%Y%m%dT%H%M%SZ").to_string())
            .unwrap_or_default()
    };
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Demo//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:design-review@demo.example.com\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Design review\r\nLOCATION:Conference room Juniper / Online\r\nDESCRIPTION:Navigation redesign\\, dark mode polish\\, accessibility audit.\r\nORGANIZER;CN=\"Kim Lee\":mailto:kim.lee@example.com\r\nATTENDEE;CN=\"Alex Morgan\";RSVP=TRUE:mailto:{}\r\nATTENDEE;CN=\"Priya Shah\";RSVP=TRUE:mailto:priya.shah@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        fmt(util::now_ts()),
        fmt(start),
        fmt(start + 3600),
        DEMO_EMAIL
    )
}

/// Creates the demo account (config + data). Returns the account id.
pub fn create(store: &Store, cfg: &mut AppConfig) -> Result<String> {
    if let Some(a) = cfg.accounts.iter().find(|a| a.kind == AccountKind::Local && a.email == DEMO_EMAIL) {
        return Ok(a.id.clone());
    }
    let acc = AccountConfig {
        kind: AccountKind::Local,
        label: "Demo mailbox".into(),
        display_name: "Alex Morgan".into(),
        email: DEMO_EMAIL.into(),
        provider: "Demo (offline)".into(),
        signature: "Alex Morgan\nProduct Lead · Example Corp\n+1 (555) 010-2030".into(),
        ..Default::default()
    };
    let id = acc.id.clone();

    let folders = [
        ("INBOX", "Inbox", FolderRole::Inbox),
        ("Drafts", "Drafts", FolderRole::Drafts),
        ("Sent", "Sent", FolderRole::Sent),
        ("Deleted Items", "Deleted Items", FolderRole::Trash),
        ("Junk Email", "Junk Email", FolderRole::Junk),
        ("Archive", "Archive", FolderRole::Archive),
        ("Projects", "Projects", FolderRole::Custom),
        ("Projects/Apollo", "Apollo", FolderRole::Custom),
        ("Receipts", "Receipts", FolderRole::Custom),
        ("Travel", "Travel", FolderRole::Custom),
    ];
    let mut folder_ids = std::collections::HashMap::new();
    for (path, name, role) in folders {
        let fid = store.upsert_folder(&id, path, name, Some("/"), role, true)?;
        folder_ids.insert(path, fid);
    }

    // Thursday of this week at 14:00 for the invitation.
    let today = util::today();
    let thu = util::start_of_week(today)
        + Duration::days(if util::first_day_monday() { 3 } else { 4 });
    let thu = if thu < today { thu + Duration::days(7) } else { thu };
    let invite_start = util::ts_from_date_time(thu, 14, 0);

    let now = util::now_ts();
    let mut pinned = Vec::new();
    let mut cats: Vec<(MsgId, Vec<String>)> = Vec::new();
    let mut uid_counter: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for mut m in mails() {
        if m.calendar.is_some() {
            m.calendar = Some(invite_ics(invite_start));
        }
        let date = now - m.minutes_ago * 60;
        let raw = build_raw(&m, date);
        let parsed = crate::mime::parse_full(&raw);
        let text = parsed.as_ref().map(crate::mime::plain_text).unwrap_or_default();
        let fid = folder_ids[m.folder];
        let uid = {
            let c = uid_counter.entry(m.folder).or_insert(0);
            *c += 1;
            *c
        };
        let nm = NewMessage {
            account_id: id.clone(),
            folder_id: fid,
            uid,
            message_id: format!("{}@demo.example.com", m.id),
            in_reply_to: m.reply_to.map(|r| format!("{r}@demo.example.com")).unwrap_or_default(),
            references: m.refs.iter().map(|r| format!("{r}@demo.example.com")).collect(),
            subject: m.subject.to_string(),
            from_name: m.from.0.to_string(),
            from_addr: m.from.1.to_string(),
            to_list: m.to.iter().map(|(n, e)| Addr::new(*n, *e).to_header()).collect::<Vec<_>>().join(", "),
            cc_list: m.cc.iter().map(|(n, e)| Addr::new(*n, *e).to_header()).collect::<Vec<_>>().join(", "),
            date,
            preview: util::make_preview(&text, 220),
            size: raw.len() as i64,
            seen: m.seen,
            flagged: m.flagged,
            answered: m.answered,
            forwarded: false,
            draft: m.draft,
            has_attachments: parsed
                .as_ref()
                .map(|p| p.attachments.iter().any(|a| !a.inline))
                .unwrap_or(false),
            importance: if m.high { Importance::High } else { Importance::Normal },
            focused: !m.bulk,
            list_unsubscribe: if m.bulk {
                Some(format!("https://{}/unsubscribe", util::domain_of(m.from.1)))
            } else {
                None
            },
            raw: Some(raw),
            body_text: text,
        };
        let (mid, _) = store.insert_message(&nm)?;
        if m.pinned {
            pinned.push(mid);
        }
        if !m.categories.is_empty() {
            cats.push((mid, m.categories.iter().map(|s| s.to_string()).collect()));
        }
        if m.flagged {
            store.set_flag_due(mid, Some(now + 2 * 86400))?;
        }
    }
    store.set_bool(&pinned, "pinned", true)?;
    for (mid, c) in cats {
        store.set_categories(mid, &c)?;
    }
    store.conn.execute("UPDATE messages SET dirty = NULL", [])?;

    // Contacts.
    let people = [
        (PRIYA, "Director of Product", "Example Corp", "+1 555 0101", true),
        (MARCUS, "Engineering Manager", "Example Corp", "+1 555 0102", true),
        (SOFIA, "Product Designer", "Studio Rossi", "+39 555 0103", false),
        (KIM, "Design Lead", "Example Corp", "+1 555 0104", false),
        (DANIEL, "Finance Partner", "Example Corp", "+1 555 0105", false),
        (HANNAH, "Corporate Counsel", "Weber Legal", "+49 555 0106", false),
        (JAMAL, "Developer Advocate", "Example Corp", "+1 555 0107", false),
        (ELENA, "Customer Success Lead", "Example Corp", "+34 555 0108", true),
        (TOM, "Chief Operating Officer", "Example Corp", "+1 555 0109", false),
        (AIKO, "People Partner", "Example Corp", "+81 555 0110", false),
    ];
    for (i, ((name, email), title, company, phone, fav)) in people.iter().enumerate() {
        let birthday = if i == 2 {
            let d = today + Duration::days(9);
            format!("1991-{:02}-{:02}", d.month(), d.day())
        } else {
            String::new()
        };
        store.save_contact(&Contact {
            name: name.to_string(),
            emails: vec![email.to_string()],
            phones: vec![phone.to_string()],
            company: company.to_string(),
            job_title: title.to_string(),
            favorite: *fav,
            birthday,
            notes: if i == 0 { "Prefers async updates. Q4 roadmap owner.".into() } else { String::new() },
            ..Default::default()
        })?;
    }

    // Calendar.
    let week = util::start_of_week(today);
    let monday = if util::first_day_monday() { week } else { week + Duration::days(1) };
    let at = |d: chrono::NaiveDate, h: u32, m: u32| util::ts_from_date_time(d, h, m);
    let events = vec![
        CalEvent {
            title: "Daily standup".into(),
            location: "Online".into(),
            start: at(monday - Duration::days(14), 9, 30),
            end: at(monday - Duration::days(14), 9, 45),
            repeat: RepeatFreq::Weekdays,
            category: "Work".into(),
            reminder_min: Some(5),
            ..Default::default()
        },
        CalEvent {
            title: "1:1 Priya / Alex".into(),
            start: at(monday + Duration::days(1) - Duration::days(7), 11, 0),
            end: at(monday + Duration::days(1) - Duration::days(7), 11, 30),
            repeat: RepeatFreq::Weekly,
            category: "Work".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Focus time".into(),
            start: at(today, 13, 0),
            end: at(today, 15, 0),
            show_as: ShowAs::Busy,
            ..Default::default()
        },
        CalEvent {
            title: "Customer call — Blue Harbor Logistics".into(),
            location: "Online".into(),
            start: at(today, 15, 30),
            end: at(today, 16, 0),
            category: "Important".into(),
            reminder_min: Some(10),
            ..Default::default()
        },
        CalEvent {
            title: "Lunch with Sofia".into(),
            location: "Café Lumen, 5th Street".into(),
            start: at(thu, 12, 30),
            end: at(thu, 13, 30),
            category: "Personal".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Q4 planning workshop".into(),
            location: "Juniper room".into(),
            start: at(monday + Duration::days(4), 10, 0),
            end: at(monday + Duration::days(4), 12, 0),
            category: "Work".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Gym".into(),
            start: at(monday - Duration::days(7), 18, 0),
            end: at(monday - Duration::days(7), 19, 0),
            repeat: RepeatFreq::Weekly,
            show_as: ShowAs::Free,
            category: "Personal".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Dentist".into(),
            location: "Bright Smile Dental".into(),
            start: at(monday + Duration::days(7), 16, 0),
            end: at(monday + Duration::days(7), 17, 0),
            reminder_min: Some(60),
            ..Default::default()
        },
        CalEvent {
            title: "Team offsite".into(),
            location: "Mountain Lodge".into(),
            start: util::local_midnight(monday + Duration::days(9)),
            end: util::local_midnight(monday + Duration::days(11)),
            all_day: true,
            category: "Work".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Flight SL 482 to Lisbon".into(),
            location: "Terminal 2".into(),
            start: at(monday + Duration::days(11), 7, 45),
            end: at(monday + Duration::days(11), 11, 30),
            category: "Travel".into(),
            reminder_min: Some(180),
            ..Default::default()
        },
        CalEvent {
            title: "Apollo launch 🚀".into(),
            start: util::local_midnight(monday + Duration::days(15)),
            end: util::local_midnight(monday + Duration::days(16)),
            all_day: true,
            category: "Important".into(),
            ..Default::default()
        },
        CalEvent {
            title: "Mom's birthday".into(),
            start: util::local_midnight(today + Duration::days(5)),
            end: util::local_midnight(today + Duration::days(6)),
            all_day: true,
            repeat: RepeatFreq::Yearly,
            show_as: ShowAs::Free,
            category: "Personal".into(),
            ..Default::default()
        },
    ];
    for e in &events {
        store.save_event(e)?;
    }

    // To Do.
    let groceries = store.save_task_list(0, "Groceries")?;
    let apollo = store.save_task_list(0, "Apollo launch")?;
    let today_s = today.format("%Y-%m-%d").to_string();
    let due = |days: i64| Some(util::local_midnight(today + Duration::days(days)));
    let tasks = vec![
        Task { list_id: 1, title: "Review Q4 roadmap slides".into(), due: due(0), important: true, my_day: today_s.clone(), ..Default::default() },
        Task { list_id: 1, title: "Book hotel in Lisbon".into(), due: due(1), notes: "Near Alfama if possible".into(), ..Default::default() },
        Task { list_id: 1, title: "Prepare offsite agenda".into(), due: due(3), important: true, ..Default::default() },
        Task { list_id: 1, title: "Send expense receipts".into(), completed: true, completed_at: Some(now - 86400), ..Default::default() },
        Task { list_id: groceries, title: "Oat milk".into(), my_day: today_s.clone(), ..Default::default() },
        Task { list_id: groceries, title: "Coffee beans".into(), ..Default::default() },
        Task { list_id: groceries, title: "Avocados".into(), ..Default::default() },
        Task { list_id: apollo, title: "Finalize release notes".into(), due: due(4), important: true, ..Default::default() },
        Task { list_id: apollo, title: "QA sign-off".into(), due: due(6), ..Default::default() },
        Task { list_id: apollo, title: "Update status page copy".into(), due: due(5), my_day: today_s, ..Default::default() },
    ];
    for t in &tasks {
        store.save_task(t)?;
    }

    cfg.favorites.push(crate::config::FavoriteRef {
        account_id: id.clone(),
        path: "INBOX".into(),
    });
    cfg.favorites.push(crate::config::FavoriteRef {
        account_id: id.clone(),
        path: "Sent".into(),
    });
    cfg.favorites.push(crate::config::FavoriteRef {
        account_id: id.clone(),
        path: "Drafts".into(),
    });
    if cfg.default_account.is_empty() {
        cfg.default_account = id.clone();
    }
    cfg.accounts.push(acc);
    Ok(id)
}
