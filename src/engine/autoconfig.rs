//! Finds IMAP/SMTP settings from an e-mail address:
//! 1. built-in presets for popular providers,
//! 2. the provider's own autoconfig file,
//! 3. Mozilla's ISP database (also via the domain's MX host),
//! 4. probing common host names.

use std::time::Duration;

use anyhow::{anyhow, Result};

use super::net;
use crate::config::{OAuthConfig, Security, ServerConfig};
use crate::util;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Discovered {
    pub provider: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub source: String,
    pub note: Option<String>,
    pub save_sent_copy: bool,
    pub oauth: Option<OAuthConfig>,
    pub password_help_url: Option<String>,
}

#[derive(Clone, Copy)]
enum User {
    Full,
    Local,
}

struct Preset {
    name: &'static str,
    domains: &'static [&'static str],
    mx: &'static [&'static str],
    imap: (&'static str, u16, Security, User),
    smtp: (&'static str, u16, Security, User),
    save_sent: bool,
    note: &'static str,
    oauth: Option<&'static str>,
    help: Option<&'static str>,
    insecure_local: bool,
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "Gmail",
        domains: &["gmail.com", "googlemail.com"],
        mx: &["google.com", "googlemail.com"],
        imap: ("imap.gmail.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.gmail.com", 465, Security::Tls, User::Full),
        save_sent: false,
        note: "",
        oauth: Some("google"),
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Outlook.com",
        domains: &["outlook.com", "hotmail.com", "live.com", "msn.com", "passport.com", "outlook.co.uk", "hotmail.co.uk", "live.co.uk", "hotmail.fr", "live.fr", "hotmail.de", "outlook.de", "hotmail.it", "hotmail.es", "outlook.es", "outlook.fr", "outlook.jp", "live.ca", "live.com.au"],
        mx: &["olc.protection.outlook.com"],
        imap: ("outlook.office365.com", 993, Security::Tls, User::Full),
        smtp: ("smtp-mail.outlook.com", 587, Security::StartTls, User::Full),
        save_sent: false,
        note: "",
        oauth: Some("microsoft"),
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Microsoft 365",
        domains: &[],
        mx: &["mail.protection.outlook.com"],
        imap: ("outlook.office365.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.office365.com", 587, Security::StartTls, User::Full),
        save_sent: false,
        note: "Microsoft 365 requires modern authentication (OAuth). IMAP must be enabled by your administrator.",
        oauth: Some("microsoft"),
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Yahoo Mail",
        domains: &["yahoo.com", "ymail.com", "rocketmail.com", "yahoo.co.uk", "yahoo.fr", "yahoo.de", "yahoo.it", "yahoo.es", "yahoo.ca", "yahoo.com.au", "yahoo.co.jp", "yahoo.in"],
        mx: &["yahoodns.net"],
        imap: ("imap.mail.yahoo.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.mail.yahoo.com", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "Yahoo requires an app password (Account security → Generate app password).",
        oauth: None,
        help: Some("https://login.yahoo.com/account/security"),
        insecure_local: false,
    },
    Preset {
        name: "iCloud Mail",
        domains: &["icloud.com", "me.com", "mac.com"],
        mx: &["mail.icloud.com"],
        imap: ("imap.mail.me.com", 993, Security::Tls, User::Local),
        smtp: ("smtp.mail.me.com", 587, Security::StartTls, User::Full),
        save_sent: true,
        note: "iCloud requires an app-specific password (appleid.apple.com → Sign-In and Security).",
        oauth: None,
        help: Some("https://appleid.apple.com/account/manage"),
        insecure_local: false,
    },
    Preset {
        name: "AOL Mail",
        domains: &["aol.com", "aim.com"],
        mx: &["aol.com"],
        imap: ("imap.aol.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.aol.com", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "AOL requires an app password (Account security → Generate app password).",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Fastmail",
        domains: &["fastmail.com", "fastmail.fm", "fastmail.net", "messagingengine.com"],
        mx: &["messagingengine.com"],
        imap: ("imap.fastmail.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.fastmail.com", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "Fastmail requires an app password (Settings → Privacy & Security → App passwords).",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Zoho Mail",
        domains: &["zoho.com", "zohomail.com"],
        mx: &["zoho.com"],
        imap: ("imap.zoho.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.zoho.com", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "GMX",
        domains: &["gmx.com", "gmx.us", "gmx.co.uk"],
        mx: &[],
        imap: ("imap.gmx.com", 993, Security::Tls, User::Full),
        smtp: ("mail.gmx.com", 587, Security::StartTls, User::Full),
        save_sent: true,
        note: "Enable IMAP access in the GMX web settings first.",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "GMX",
        domains: &["gmx.de", "gmx.net", "gmx.at", "gmx.ch"],
        mx: &["gmx.net"],
        imap: ("imap.gmx.net", 993, Security::Tls, User::Full),
        smtp: ("mail.gmx.net", 587, Security::StartTls, User::Full),
        save_sent: true,
        note: "Enable IMAP access in the GMX web settings first.",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "WEB.DE",
        domains: &["web.de"],
        mx: &["web.de"],
        imap: ("imap.web.de", 993, Security::Tls, User::Full),
        smtp: ("smtp.web.de", 587, Security::StartTls, User::Full),
        save_sent: true,
        note: "Enable IMAP access in the WEB.DE web settings first.",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Yandex Mail",
        domains: &["yandex.com", "yandex.ru", "ya.ru"],
        mx: &["yandex.net", "yandex.ru"],
        imap: ("imap.yandex.com", 993, Security::Tls, User::Full),
        smtp: ("smtp.yandex.com", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "Yandex requires an app password.",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Mail.ru",
        domains: &["mail.ru", "inbox.ru", "list.ru", "bk.ru"],
        mx: &["mail.ru"],
        imap: ("imap.mail.ru", 993, Security::Tls, User::Full),
        smtp: ("smtp.mail.ru", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "Mail.ru requires an app password.",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Proton Mail (Bridge)",
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
        mx: &["protonmail.ch"],
        imap: ("127.0.0.1", 1143, Security::StartTls, User::Full),
        smtp: ("127.0.0.1", 1025, Security::StartTls, User::Full),
        save_sent: false,
        note: "Proton Mail works through Proton Mail Bridge. Start Bridge and use the password it shows.",
        oauth: None,
        help: Some("https://proton.me/mail/bridge"),
        insecure_local: true,
    },
    Preset {
        name: "mailbox.org",
        domains: &["mailbox.org"],
        mx: &["mailbox.org"],
        imap: ("imap.mailbox.org", 993, Security::Tls, User::Full),
        smtp: ("smtp.mailbox.org", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Posteo",
        domains: &["posteo.de", "posteo.net", "posteo.org", "posteo.eu", "posteo.at", "posteo.ch"],
        mx: &["posteo.de"],
        imap: ("posteo.de", 993, Security::Tls, User::Full),
        smtp: ("posteo.de", 465, Security::Tls, User::Full),
        save_sent: true,
        note: "",
        oauth: None,
        help: None,
        insecure_local: false,
    },
    Preset {
        name: "Comcast Xfinity",
        domains: &["comcast.net"],
        mx: &["comcast.net"],
        imap: ("imap.comcast.net", 993, Security::Tls, User::Full),
        smtp: ("smtp.comcast.net", 587, Security::StartTls, User::Full),
        save_sent: true,
        note: "",
        oauth: None,
        help: None,
        insecure_local: false,
    },
];

fn username(kind: User, email: &str) -> String {
    match kind {
        User::Full => email.to_string(),
        User::Local => email.split('@').next().unwrap_or(email).to_string(),
    }
}

fn from_preset(p: &Preset, email: &str, source: &str) -> Discovered {
    let mk = |(host, port, sec, user): (&str, u16, Security, User)| ServerConfig {
        host: host.to_string(),
        port,
        security: sec,
        username: username(user, email),
        accept_invalid_certs: p.insecure_local,
    };
    Discovered {
        provider: p.name.to_string(),
        imap: mk(p.imap),
        smtp: mk(p.smtp),
        source: source.to_string(),
        note: Some(p.note.to_string()).filter(|s| !s.is_empty()),
        save_sent_copy: p.save_sent,
        oauth: p.oauth.and_then(super::oauth::preset),
        password_help_url: p.help.map(|s| s.to_string()),
    }
}

fn preset_for_domain(domain: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.domains.contains(&domain))
}

fn preset_for_mx(mx: &str) -> Option<&'static Preset> {
    PRESETS
        .iter()
        .find(|p| p.mx.iter().any(|suffix| mx == *suffix || mx.ends_with(&format!(".{suffix}"))))
}

fn http_get(url: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .into();
    let mut resp = agent.get(url).call().ok()?;
    resp.body_mut().read_to_string().ok()
}

/// Parses Mozilla's autoconfig format (`clientConfig` v1.1).
pub fn parse_autoconfig(xml: &str, email: &str, source: &str) -> Option<Discovered> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let provider = doc.descendants().find(|n| n.has_tag_name("emailProvider"))?;
    let child_text = |n: roxmltree::Node, tag: &str| {
        n.children()
            .find(|c| c.has_tag_name(tag))
            .and_then(|c| c.text())
            .map(|s| s.trim().to_string())
    };
    let local = email.split('@').next().unwrap_or("");
    let domain = util::domain_of(email);
    let expand = |s: &str| {
        s.replace("%EMAILADDRESS%", email)
            .replace("%EMAILLOCALPART%", local)
            .replace("%EMAILDOMAIN%", &domain)
    };
    let server = |n: roxmltree::Node| -> Option<(ServerConfig, u8)> {
        let host = expand(&child_text(n, "hostname")?);
        let port: u16 = child_text(n, "port")?.parse().ok()?;
        let (security, rank) = match child_text(n, "socketType")
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str()
        {
            "SSL" => (Security::Tls, 3),
            "STARTTLS" => (Security::StartTls, 2),
            _ => (Security::None, 0),
        };
        let auth = child_text(n, "authentication").unwrap_or_default();
        let rank = if auth.contains("OAuth") { rank } else { rank + 1 };
        Some((
            ServerConfig {
                host,
                port,
                security,
                username: expand(&child_text(n, "username").unwrap_or_else(|| "%EMAILADDRESS%".into())),
                accept_invalid_certs: false,
            },
            rank,
        ))
    };
    let imap = provider
        .children()
        .filter(|n| n.has_tag_name("incomingServer") && n.attribute("type") == Some("imap"))
        .filter_map(server)
        .max_by_key(|(_, r)| *r)?
        .0;
    let smtp = provider
        .children()
        .filter(|n| n.has_tag_name("outgoingServer") && n.attribute("type") == Some("smtp"))
        .filter_map(server)
        .max_by_key(|(_, r)| *r)?
        .0;
    let name = child_text(provider, "displayName")
        .or_else(|| child_text(provider, "displayShortName"))
        .unwrap_or_else(|| domain.clone());
    Some(Discovered {
        provider: name,
        imap,
        smtp,
        source: source.to_string(),
        note: None,
        save_sent_copy: true,
        oauth: None,
        password_help_url: None,
    })
}

fn http_autoconfig(email: &str, domain: &str) -> Option<Discovered> {
    let enc = super::oauth::url_encode(email);
    let urls = [
        (
            format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={enc}"),
            "provider autoconfig",
        ),
        (
            format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={enc}"),
            "provider autoconfig",
        ),
        (
            format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
            "Mozilla ISP database",
        ),
    ];
    for (url, src) in urls {
        if let Some(xml) = http_get(&url) {
            if let Some(d) = parse_autoconfig(&xml, email, src) {
                return Some(d);
            }
        }
    }
    None
}

async fn mx_hosts(domain: &str) -> Vec<String> {
    let Ok(builder) = hickory_resolver::TokioResolver::builder_tokio() else {
        return Vec::new();
    };
    let Ok(resolver) = builder.build() else {
        return Vec::new();
    };
    let Ok(Ok(lookup)) =
        tokio::time::timeout(Duration::from_secs(6), resolver.mx_lookup(domain)).await
    else {
        return Vec::new();
    };
    let mut hosts: Vec<(u16, String)> = lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            hickory_resolver::proto::rr::RData::MX(mx) => Some((
                mx.preference,
                mx.exchange.to_ascii().trim_end_matches('.').to_lowercase(),
            )),
            _ => None,
        })
        .collect();
    hosts.sort();
    hosts.into_iter().map(|(_, h)| h).collect()
}

fn base_domain(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() <= 2 {
        return host.to_string();
    }
    // Handle common two-label public suffixes (co.uk, com.au, ...).
    let n = parts.len();
    let two = format!("{}.{}", parts[n - 2], parts[n - 1]);
    if ["co.uk", "com.au", "co.jp", "com.br", "co.nz", "co.za", "org.uk"].contains(&two.as_str()) && n >= 3 {
        format!("{}.{}", parts[n - 3], two)
    } else {
        two
    }
}

async fn guess(email: &str, domain: &str) -> Option<Discovered> {
    let imap_candidates = [
        (format!("imap.{domain}"), 993, Security::Tls),
        (format!("mail.{domain}"), 993, Security::Tls),
        (domain.to_string(), 993, Security::Tls),
        (format!("imap.{domain}"), 143, Security::StartTls),
        (format!("mail.{domain}"), 143, Security::StartTls),
    ];
    let smtp_candidates = [
        (format!("smtp.{domain}"), 465, Security::Tls),
        (format!("smtp.{domain}"), 587, Security::StartTls),
        (format!("mail.{domain}"), 465, Security::Tls),
        (format!("mail.{domain}"), 587, Security::StartTls),
        (domain.to_string(), 587, Security::StartTls),
    ];
    let imap_probe = futures::future::join_all(imap_candidates.iter().map(|(h, p, _)| net::probe(h, *p))).await;
    let smtp_probe = futures::future::join_all(smtp_candidates.iter().map(|(h, p, _)| net::probe(h, *p))).await;
    let imap = imap_candidates
        .iter()
        .zip(imap_probe)
        .find(|(_, ok)| *ok)
        .map(|((h, p, s), _)| (h.clone(), *p, *s))?;
    let smtp = smtp_candidates
        .iter()
        .zip(smtp_probe)
        .find(|(_, ok)| *ok)
        .map(|((h, p, s), _)| (h.clone(), *p, *s))
        .unwrap_or((format!("smtp.{domain}"), 587, Security::StartTls));
    Some(Discovered {
        provider: domain.to_string(),
        imap: ServerConfig {
            host: imap.0,
            port: imap.1,
            security: imap.2,
            username: email.to_string(),
            accept_invalid_certs: false,
        },
        smtp: ServerConfig {
            host: smtp.0,
            port: smtp.1,
            security: smtp.2,
            username: email.to_string(),
            accept_invalid_certs: false,
        },
        source: "best guess (please verify)".into(),
        note: Some("These settings were guessed from common server names. Check them if signing in fails.".into()),
        save_sent_copy: true,
        oauth: None,
        password_help_url: None,
    })
}

pub async fn discover(email: &str) -> Result<Discovered> {
    let email = email.trim().to_lowercase();
    if !util::is_valid_email(&email) {
        return Err(anyhow!("\"{email}\" doesn't look like an e-mail address"));
    }
    let domain = util::domain_of(&email);
    if let Some(p) = preset_for_domain(&domain) {
        return Ok(from_preset(p, &email, "built-in"));
    }
    {
        let (e, d) = (email.clone(), domain.clone());
        if let Ok(Some(found)) = tokio::task::spawn_blocking(move || http_autoconfig(&e, &d)).await {
            return Ok(found);
        }
    }
    let mx = mx_hosts(&domain).await;
    for host in &mx {
        if let Some(p) = preset_for_mx(host) {
            let mut d = from_preset(p, &email, "MX record");
            if p.domains.is_empty() || !p.domains.contains(&domain.as_str()) {
                d.provider = format!("{} ({})", p.name, domain);
            }
            return Ok(d);
        }
    }
    if let Some(first) = mx.first() {
        let base = base_domain(first);
        if base != domain {
            let e = email.clone();
            let url = format!("https://autoconfig.thunderbird.net/v1.1/{base}");
            let found = tokio::task::spawn_blocking(move || {
                http_get(&url).and_then(|xml| parse_autoconfig(&xml, &e, "Mozilla ISP database (MX)"))
            })
            .await
            .ok()
            .flatten();
            if let Some(d) = found {
                return Ok(d);
            }
        }
    }
    guess(&email, &domain).await.ok_or_else(|| {
        anyhow!("Couldn't find settings for {domain}. Enter your server details manually.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_xml() {
        let d = from_preset(preset_for_domain("icloud.com").unwrap(), "jane@icloud.com", "t");
        assert_eq!(d.imap.username, "jane");
        assert_eq!(d.smtp.username, "jane@icloud.com");
        assert!(preset_for_mx("example-com.mail.protection.outlook.com").is_some());
        assert_eq!(base_domain("mx1.mail.example.co.uk"), "example.co.uk");

        let xml = r#"<?xml version="1.0"?>
        <clientConfig version="1.1"><emailProvider id="example.org">
          <domain>example.org</domain><displayName>Example Mail</displayName>
          <incomingServer type="pop3"><hostname>pop.example.org</hostname><port>995</port><socketType>SSL</socketType></incomingServer>
          <incomingServer type="imap"><hostname>imap.example.org</hostname><port>143</port><socketType>STARTTLS</socketType><username>%EMAILLOCALPART%</username></incomingServer>
          <incomingServer type="imap"><hostname>imap.example.org</hostname><port>993</port><socketType>SSL</socketType><username>%EMAILADDRESS%</username></incomingServer>
          <outgoingServer type="smtp"><hostname>smtp.example.org</hostname><port>587</port><socketType>STARTTLS</socketType><username>%EMAILADDRESS%</username></outgoingServer>
        </emailProvider></clientConfig>"#;
        let d = parse_autoconfig(xml, "bob@example.org", "test").unwrap();
        assert_eq!(d.provider, "Example Mail");
        assert_eq!(d.imap.port, 993);
        assert_eq!(d.imap.security, Security::Tls);
        assert_eq!(d.imap.username, "bob@example.org");
        assert_eq!(d.smtp.host, "smtp.example.org");
        assert_eq!(d.smtp.security, Security::StartTls);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Network test: `cargo test -- --ignored live_discover --nocapture`
    #[test]
    #[ignore = "uses the network"]
    fn live_discover() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        for email in ["someone@gmail.com", "someone@mozilla.com", "someone@freenet.de", "someone@t-online.de"] {
            match rt.block_on(discover(email)) {
                Ok(d) => eprintln!(
                    "{email:24} -> {} [{}] imap {} smtp {}",
                    d.provider,
                    d.source,
                    d.imap.summary(),
                    d.smtp.summary()
                ),
                Err(e) => eprintln!("{email:24} -> error: {e:#}"),
            }
        }
    }
}
