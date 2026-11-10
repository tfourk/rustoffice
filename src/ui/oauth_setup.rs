//! One-time "Set up Google sign-in" guide. Google only lets an app open Gmail
//! with its own app key (an OAuth client from Google Cloud). The guide opens
//! each Google Cloud page in the browser and picks up the downloaded key file
//! by itself, so nobody has to copy a client ID by hand.

use std::time::{Duration, SystemTime};

use egui::{vec2, Align2, CornerRadius, Frame, Margin, RichText, Sense, Stroke, Ui};

use crate::config::OAuthClientKey;
use crate::engine::oauth;
use crate::icons;
use crate::theme::{font, font_semibold, icon_fill_font, icon_font, mix, pal};
use crate::ui::widgets;

struct Step {
    title: &'static str,
    body: &'static str,
    url: &'static str,
    button: &'static str,
}

const STEPS: &[Step] = &[
    Step {
        title: "Create a project",
        body: "Sign in with your Google account. Google may ask you to turn on 2-Step Verification and to accept the Google Cloud terms; skip any free-trial or billing offer (nothing needs paying). Name the project RustMail and click Create.",
        url: "https://console.cloud.google.com/projectcreate",
        button: "Open Google Cloud",
    },
    Step {
        title: "Turn on Gmail",
        body: "Check that RustMail is the project shown at the top of the page, then click Enable.",
        url: "https://console.cloud.google.com/apis/library/gmail.googleapis.com",
        button: "Open Gmail API",
    },
    Step {
        title: "Turn on Google Calendar",
        body: "Same again for your calendars: check the project at the top, then click Enable.",
        url: "https://console.cloud.google.com/apis/library/calendar-json.googleapis.com",
        button: "Open Calendar API",
    },
    Step {
        title: "Describe the app",
        body: "Click Get started. App name: RustMail. User support email: your address. Audience: External. Contact email: your address. Tick the agreement, then click Continue and Create.",
        url: "https://console.cloud.google.com/auth/overview",
        button: "Open sign-in setup",
    },
    Step {
        title: "Let yourself in",
        body: "Under Test users, click Add users, enter your Gmail address and click Save. (Google then asks you to sign in again every 7 days; publishing the app instead needs a website with a privacy policy.)",
        url: "https://console.cloud.google.com/auth/audience",
        button: "Open Audience",
    },
    Step {
        title: "Create the key",
        body: "Click Create client. Application type: Desktop app. Name: RustMail. Leave the other options as they are and click Create. In the window that appears, click Download JSON right away: Google shows the secret only once.",
        url: "https://console.cloud.google.com/auth/clients",
        button: "Open Clients",
    },
];

pub enum SetupResult {
    Working,
    Done(OAuthClientKey),
    Cancel,
}

pub struct GoogleSetup {
    /// Only key files saved after the guide opened are taken automatically;
    /// older ones could be stale or planted, so they need choosing by hand.
    since: SystemTime,
    next_scan: f64,
    opened: [bool; STEPS.len()],
    paste: String,
    /// A client ID pasted on its own, waiting for its secret.
    pending_id: String,
    error: Option<String>,
    /// Last problem found with a downloaded key file (shown once per file).
    scan_error: Option<String>,
}

impl GoogleSetup {
    pub fn new() -> Self {
        GoogleSetup {
            since: SystemTime::now() - Duration::from_secs(10),
            next_scan: 0.0,
            opened: [false; STEPS.len()],
            paste: String::new(),
            pending_id: String::new(),
            error: None,
            scan_error: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> SetupResult {
        let p = pal();
        let mut result = SetupResult::Working;

        ui.label(RichText::new("Set up Google sign-in").font(font_semibold(22.0)));
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Google only lets an app open Gmail if the app has its own key, called a client ID. \
                 Apps from big companies have one built in; RustMail doesn't, so you make one for yourself. \
                 It's free, takes about 5 minutes, and you only do it once on this computer.",
            )
            .font(font(14.0))
            .color(p.text2),
        );
        ui.add_space(14.0);

        for (i, step) in STEPS.iter().enumerate() {
            Frame::NONE
                .fill(p.card_alt)
                .stroke(Stroke::new(1.0, mix(p.stroke, p.card, 0.3)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                        if self.opened[i] {
                            ui.painter().circle_filled(r.center(), 13.0, p.success_soft);
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::CHECK, icon_font(14.0), p.success);
                        } else {
                            ui.painter().circle_filled(r.center(), 13.0, p.accent_soft);
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, (i + 1).to_string(), font_semibold(13.0), p.accent_text);
                        }
                        ui.add_space(6.0);
                        let button_w = 170.0;
                        ui.vertical(|ui| {
                            ui.set_width((ui.available_width() - button_w).max(160.0));
                            ui.label(RichText::new(step.title).font(font_semibold(14.5)));
                            ui.label(RichText::new(step.body).font(font(13.0)).color(p.text2));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::secondary_button(ui, Some(icons::ARROW_SQUARE_OUT), step.button).clicked() {
                                let _ = open::that_detached(step.url);
                                self.opened[i] = true;
                            }
                        });
                    });
                });
            ui.add_space(8.0);
        }

        // Watch the downloads folder for the key file.
        let now = ui.input(|i| i.time);
        if now >= self.next_scan {
            self.next_scan = now + 1.5;
            match oauth::scan_downloads_for_google_key(self.since) {
                Ok(Some(key)) => result = SetupResult::Done(key),
                Ok(None) => {}
                Err(e) => {
                    if self.scan_error.as_ref() != Some(&e) {
                        self.error = Some(e.clone());
                        self.scan_error = Some(e);
                    }
                }
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(1500));

        // A key file dropped on the window.
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path();
            let key = if path.is_file() {
                oauth::load_google_key_file(path)
            } else {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                match f.bytes() {
                    Ok(bytes) if bytes.len() <= 64 * 1024 => key_from_text(&String::from_utf8_lossy(&bytes), &name),
                    _ => Err("Could not read the dropped file.".to_string()),
                }
            };
            match key {
                Ok(k) => result = SetupResult::Done(k),
                Err(e) => self.error = Some(e),
            }
        }

        ui.add_space(4.0);
        Frame::NONE
            .fill(p.accent_soft)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::spinner(ui, 16.0);
                    ui.label(
                        RichText::new("Waiting for the key… RustMail picks up the downloaded file from your Downloads folder by itself.")
                            .font(font(13.5))
                            .color(p.accent_text),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Saved it somewhere else?").font(font(13.0)).color(p.text2));
                    if widgets::link(ui, "Choose the file").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Google key file", &["json"]).pick_file() {
                            match oauth::load_google_key_file(&path) {
                                Ok(k) => result = SetupResult::Done(k),
                                Err(e) => self.error = Some(e),
                            }
                        }
                    }
                    ui.label(RichText::new("or drop it on this window. You can also paste the client ID and secret here:").font(font(13.0)).color(p.text2));
                });
                ui.add_space(4.0);
                let hint = if self.pending_id.is_empty() { "Paste the client ID and client secret" } else { "Now paste the client secret (starts with GOCSPX-)" };
                ui.horizontal(|ui| {
                    let r = widgets::text_input(ui, &mut self.paste, hint, (ui.available_width() - 160.0).max(200.0));
                    if r.changed() {
                        self.error = None;
                    }
                    let ready = !self.paste.trim().is_empty();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (widgets::primary_button_ex(ui, None, "Use this key", ready).clicked() || (enter && ready)) && ready {
                        match self.key_from_paste() {
                            Ok(Some(k)) => result = SetupResult::Done(k),
                            Ok(None) => {}
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
                if !self.pending_id.is_empty() {
                    ui.label(
                        RichText::new(format!("{} Got the client ID. Now paste the client secret.", icons::CHECK))
                            .font(icon_font(12.5))
                            .color(p.success),
                    );
                }
            });
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{} {e}", icons::WARNING)).font(icon_font(13.0)).color(p.danger));
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new("When you sign in, Google warns that the app is being tested or hasn't been verified: click Continue (or Advanced, then \"Go to … (unsafe)\"). It's your own key, so that's safe.")
                .font(font(12.5))
                .color(p.text3),
        );
        ui.add_space(10.0);
        if widgets::secondary_button(ui, None, "Back").clicked() {
            result = SetupResult::Cancel;
        }
        let _ = icon_fill_font;
        result
    }

    /// Reads the pasted text. A client ID on its own is remembered until the
    /// secret is pasted (`Ok(None)`); the secret must look complete.
    fn key_from_paste(&mut self) -> Result<Option<OAuthClientKey>, String> {
        let text = std::mem::take(&mut self.paste);
        // New text with its own client ID replaces any remembered one; only a
        // secret on its own is paired with the remembered ID.
        let has_own_id = text.trim_start().starts_with('{') || text.contains(".apps.googleusercontent.com");
        let key = if has_own_id || self.pending_id.is_empty() {
            oauth::parse_google_key(&text)
        } else {
            oauth::parse_google_key(&format!("{} {}", self.pending_id, text))
        };
        let mut key = key.inspect_err(|_| self.paste = text.clone())?;
        if key.client_secret.is_empty() {
            self.pending_id = key.client_id;
            return Ok(None);
        }
        // Google secrets are "GOCSPX-" plus 28 characters; a shorter one is a typo.
        if key.client_secret.len() < "GOCSPX-".len() + 20 {
            self.paste = text;
            return Err("That client secret looks incomplete. Copy it again from Google Cloud.".into());
        }
        self.pending_id.clear();
        key.source = "pasted by you".into();
        Ok(Some(key))
    }
}

/// A key from pasted or dropped text; needs both the ID and the secret.
fn key_from_text(text: &str, source: &str) -> Result<OAuthClientKey, String> {
    let mut key = oauth::parse_google_key(text)?;
    if key.client_secret.is_empty() {
        return Err("Got the client ID. Now paste the client secret too (it starts with GOCSPX-).".into());
    }
    key.source = if source.is_empty() { "pasted by you".into() } else { source.to_string() };
    Ok(key)
}

/// Short description of a key for Settings, e.g.
/// "123456789012-… (Google Cloud project rustmail-123456)".
pub fn describe(key: &OAuthClientKey) -> String {
    let id = key.client_id.trim();
    let short = match id.split_once('-') {
        Some((num, _)) => format!("{num}-…"),
        None => crate::util::truncate_chars(id, 16),
    };
    let origin = if !key.project_id.is_empty() {
        format!("Google Cloud project {}", key.project_id)
    } else if key.source.starts_with("client_secret") {
        "downloaded key file".to_string()
    } else {
        key.source.clone()
    };
    if origin.is_empty() {
        short
    } else {
        format!("{short} ({origin})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "123456789012-abcdefghijklmnop0123456789abcdef.apps.googleusercontent.com";
    const SECRET: &str = "GOCSPX-abcdefghijklmnopqrstuvwx1234";

    #[test]
    fn paste_id_then_secret() {
        let mut g = GoogleSetup::new();
        g.paste = ID.into();
        assert!(g.key_from_paste().unwrap().is_none(), "ID alone waits for the secret");
        assert_eq!(g.pending_id, ID);
        // Half-typed secret is refused and kept for fixing.
        g.paste = "GOCSPX-a".into();
        assert!(g.key_from_paste().is_err());
        assert_eq!(g.paste, "GOCSPX-a");
        g.paste = SECRET.into();
        let k = g.key_from_paste().unwrap().unwrap();
        assert_eq!((k.client_id.as_str(), k.client_secret.as_str()), (ID, SECRET));
        assert!(g.pending_id.is_empty());
    }

    #[test]
    fn pasted_id_can_be_replaced() {
        let other = "999999999999-zzzzzzzzzzzzzzzz0123456789abcdef.apps.googleusercontent.com";
        let mut g = GoogleSetup::new();
        g.paste = other.into();
        assert!(g.key_from_paste().unwrap().is_none());
        // The right ID pasted afterwards replaces the wrong one.
        g.paste = ID.into();
        assert!(g.key_from_paste().unwrap().is_none());
        assert_eq!(g.pending_id, ID);
        g.paste = SECRET.into();
        assert_eq!(g.key_from_paste().unwrap().unwrap().client_id, ID);
    }

    #[test]
    fn website_key_after_pending_id_is_refused() {
        let mut g = GoogleSetup::new();
        g.paste = ID.into();
        assert!(g.key_from_paste().unwrap().is_none());
        g.paste = format!(r#"{{"web":{{"client_id":"{ID}","client_secret":"{SECRET}"}}}}"#);
        assert!(g.key_from_paste().unwrap_err().contains("Desktop app"));
    }

    #[test]
    fn paste_both_at_once() {
        let mut g = GoogleSetup::new();
        g.paste = format!("Client ID {ID}  Client secret {SECRET}");
        let k = g.key_from_paste().unwrap().unwrap();
        assert_eq!(k.client_secret, SECRET);
        assert_eq!(k.source, "pasted by you");
    }
}
