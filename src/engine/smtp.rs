//! Outgoing mail over SMTP (lettre).

use std::time::Duration;

use anyhow::{anyhow, Result};
use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials as SmtpCredentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use super::imap::Credentials;
use crate::config::{Security, ServerConfig};

fn transport(server: &ServerConfig, creds: &Credentials) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let host = server.host.trim();
    if host.is_empty() {
        return Err(anyhow!("No outgoing (SMTP) server is configured"));
    }
    let params = TlsParameters::builder(host.to_string())
        .dangerous_accept_invalid_certs(server.accept_invalid_certs)
        .dangerous_accept_invalid_hostnames(server.accept_invalid_certs)
        .build_rustls()
        .map_err(|e| anyhow!("TLS setup failed: {e}"))?;
    let tls = match server.security {
        Security::Tls => Tls::Wrapper(params),
        Security::StartTls => Tls::Required(params),
        Security::None => Tls::None,
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(server.port)
        .tls(tls)
        .timeout(Some(Duration::from_secs(60)));
    if !creds.username.is_empty() {
        builder = builder
            .credentials(SmtpCredentials::new(creds.username.clone(), creds.secret.clone()))
            .authentication(if creds.oauth {
                vec![Mechanism::Xoauth2]
            } else {
                vec![Mechanism::Plain, Mechanism::Login]
            });
    }
    Ok(builder.build())
}

fn friendly(e: lettre::transport::smtp::Error) -> anyhow::Error {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("535") || lower.contains("authentication") || lower.contains("credentials") {
        anyhow!("The outgoing server rejected your sign-in. Check the SMTP user name and password. ({msg})")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        anyhow!("The outgoing server did not respond in time ({msg})")
    } else if lower.contains("connection refused") {
        anyhow!("Could not connect to the outgoing server ({msg})")
    } else {
        anyhow!("Sending failed: {msg}")
    }
}

pub async fn send(
    server: &ServerConfig,
    creds: &Credentials,
    from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    let t = transport(server, creds)?;
    let from_addr: lettre::Address = from
        .trim()
        .parse()
        .map_err(|e| anyhow!("Invalid sender address \"{from}\": {e}"))?;
    let to: Vec<lettre::Address> = recipients
        .iter()
        .filter_map(|r| r.trim().parse().ok())
        .collect();
    if to.is_empty() {
        return Err(anyhow!("The message has no valid recipients"));
    }
    let envelope = Envelope::new(Some(from_addr), to).map_err(|e| anyhow!("{e}"))?;
    t.send_raw(&envelope, raw).await.map_err(friendly)?;
    Ok(())
}

pub async fn test(server: &ServerConfig, creds: &Credentials) -> Result<()> {
    let t = transport(server, creds)?;
    match t.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(anyhow!("The outgoing server did not accept the connection")),
        Err(e) => Err(friendly(e)),
    }
}
