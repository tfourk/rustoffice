//! "Add account" wizard: e-mail first, settings discovered automatically.

use egui::{vec2, Align, Align2, Color32, CornerRadius, Frame, Id, Key, Layout, Margin, Modal, RichText, Sense, Ui};

use crate::app::{App, ToastKind};
use crate::config::{AccountConfig, AuthKind, Security, ServerConfig};
use crate::engine::autoconfig::Discovered;
use crate::engine::Command;
use crate::icons;
use crate::model::FolderRole;
use crate::secrets::{self, SecretKind};
use crate::theme::{font, font_semibold, icon_fill_font, icon_font, mix, pal};
use crate::ui::settings::{account_form, AccountEditor};
use crate::ui::widgets;
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Welcome,
    Email,
    Discovering,
    Credentials,
    Testing,
    Done,
}

pub struct WizardState {
    pub step: Step,
    pub first_run: bool,
    pub email: String,
    pub name: String,
    pub found: Option<Discovered>,
    pub editor: AccountEditor,
    pub manual: bool,
    pub error: Option<String>,
    pub imap: Option<Result<String, String>>,
    pub smtp: Option<Result<String, String>>,
    pub warning: Option<String>,
    pub start_test_pending: bool,
    focus: bool,
}

fn guess_servers(email: &str) -> (ServerConfig, ServerConfig) {
    let domain = util::domain_of(email);
    (
        ServerConfig {
            host: format!("imap.{domain}"),
            port: 993,
            security: Security::Tls,
            username: email.to_string(),
            accept_invalid_certs: false,
        },
        ServerConfig {
            host: format!("smtp.{domain}"),
            port: 587,
            security: Security::StartTls,
            username: email.to_string(),
            accept_invalid_certs: false,
        },
    )
}

impl WizardState {
    pub fn new(first_run: bool) -> Self {
        WizardState {
            step: if first_run { Step::Welcome } else { Step::Email },
            first_run,
            email: String::new(),
            name: String::new(),
            found: None,
            editor: AccountEditor {
                acc: AccountConfig::default(),
                password: String::new(),
                smtp_password: String::new(),
                has_password: false,
                testing: false,
                test: None,
                oauth_waiting: false,
                oauth_result: None,
                show_password: false,
            },
            manual: false,
            error: None,
            imap: None,
            smtp: None,
            warning: None,
            start_test_pending: false,
            focus: true,
        }
    }

    pub fn on_discovered(&mut self, email: &str, result: Result<Discovered, String>) {
        if self.step != Step::Discovering || !email.eq_ignore_ascii_case(self.email.trim()) {
            return;
        }
        let acc = &mut self.editor.acc;
        acc.email = self.email.trim().to_string();
        acc.display_name = if self.name.trim().is_empty() {
            self.email.split('@').next().unwrap_or("").replace(['.', '_'], " ")
        } else {
            self.name.trim().to_string()
        };
        match result {
            Ok(d) => {
                acc.imap = d.imap.clone();
                acc.smtp = d.smtp.clone();
                acc.provider = d.provider.clone();
                acc.save_sent_copy = d.save_sent_copy;
                if let Some(o) = &d.oauth {
                    acc.oauth = o.clone();
                    // Microsoft no longer accepts passwords for IMAP.
                    acc.auth = if o.provider == "microsoft" { AuthKind::OAuth2 } else { AuthKind::Password };
                }
                self.manual = d.source.contains("guess");
                self.found = Some(d);
                self.error = None;
            }
            Err(e) => {
                let (i, s) = guess_servers(&acc.email);
                acc.imap = i;
                acc.smtp = s;
                acc.provider = util::domain_of(&acc.email);
                self.manual = true;
                self.error = Some(e);
            }
        }
        self.step = Step::Credentials;
        self.focus = true;
    }

    pub fn on_test_result(&mut self, imap: Result<String, String>, smtp: Result<String, String>) {
        if self.step == Step::Testing {
            self.imap = Some(imap);
            self.smtp = Some(smtp);
        }
    }

    pub fn on_oauth(&mut self, account_id: &str, result: Result<(), String>) {
        if self.editor.acc.id == account_id {
            self.editor.oauth_waiting = false;
            self.editor.oauth_result = Some(result.clone());
            if result.is_ok() && self.step == Step::Credentials {
                self.start_test_pending = true;
            }
        }
    }
}

impl App {
    pub fn wizard_ui(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.wizard.take() else { return };
        let p = pal();
        let mut close = false;
        let modal = Modal::new(Id::new("wizard")).frame(
            Frame::NONE
                .fill(p.card)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(32))
                .shadow(crate::theme::window_shadow()),
        );
        // Kick off the connection test after OAuth sign-in succeeded.
        if w.start_test_pending {
            w.start_test_pending = false;
            self.wizard_start_test(&mut w);
        }
        // Finish when both tests reported back.
        if w.step == Step::Testing {
            if let (Some(imap), Some(smtp)) = (w.imap.clone(), w.smtp.clone()) {
                match imap {
                    Ok(_) => {
                        self.wizard_finish(&mut w);
                        if let Err(e) = smtp {
                            w.warning = Some(format!("You can read mail, but sending failed: {e}. Check the outgoing server in Settings → Accounts."));
                        }
                        w.step = Step::Done;
                    }
                    Err(e) => {
                        w.error = Some(e);
                        w.manual = true;
                        w.step = Step::Credentials;
                    }
                }
            }
        }
        let resp = modal.show(ctx, |ui| {
            ui.set_width(if w.step == Step::Credentials && w.manual { 640.0 } else { 520.0 });
            match w.step {
                Step::Welcome => {
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(88.0, 88.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(20), p.accent);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::ENVELOPE_SIMPLE, icon_fill_font(48.0), Color32::WHITE);
                        ui.add_space(18.0);
                        ui.label(RichText::new("Welcome to RustMail").font(font_semibold(26.0)));
                        ui.add_space(6.0);
                        ui.label(RichText::new("Your mail, calendar, contacts and tasks — fast, private, and in one place.").font(font(14.5)).color(p.text2));
                        ui.add_space(24.0);
                        if widgets::primary_button(ui, Some(icons::USER_PLUS), "Add your e-mail account").clicked() {
                            w.step = Step::Email;
                            w.focus = true;
                        }
                        ui.add_space(10.0);
                        if widgets::secondary_button(ui, Some(icons::SPARKLE), "Explore with a demo mailbox").clicked() {
                            self.add_demo();
                            close = true;
                        }
                        ui.add_space(18.0);
                        ui.label(RichText::new("Works with Gmail, Outlook.com, iCloud, Yahoo, Fastmail and any IMAP/SMTP provider.").font(font(12.5)).color(p.text3));
                    });
                }
                Step::Email => {
                    header(ui, "Add your account", "Enter your e-mail address. We'll find the right settings automatically.");
                    ui.add_space(14.0);
                    let r = widgets::text_input(ui, &mut w.email, "you@example.com", ui.available_width());
                    if w.focus {
                        r.request_focus();
                        w.focus = false;
                    }
                    ui.add_space(8.0);
                    widgets::text_input(ui, &mut w.name, "Your name (shown to people you e-mail)", ui.available_width());
                    if let Some(e) = &w.error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(e).color(p.danger));
                    }
                    ui.add_space(20.0);
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        let valid = util::is_valid_email(&w.email);
                        if (widgets::primary_button_ex(ui, None, "Continue", valid).clicked() || (enter && valid)) && valid {
                            w.error = None;
                            w.step = Step::Discovering;
                            w.editor.acc = AccountConfig::default();
                            self.engine.send(Command::Discover { email: w.email.trim().to_string() });
                        }
                        if widgets::link(ui, "Set up manually").clicked() {
                            let email = w.email.trim().to_string();
                            let (i, s) = guess_servers(&email);
                            let acc = &mut w.editor.acc;
                            acc.email = email.clone();
                            acc.display_name = w.name.trim().to_string();
                            acc.imap = i;
                            acc.smtp = s;
                            acc.provider = util::domain_of(&email);
                            w.manual = true;
                            w.step = Step::Credentials;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::secondary_button(ui, None, "Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                }
                Step::Discovering => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        widgets::spinner(ui, 36.0);
                        ui.add_space(16.0);
                        ui.label(RichText::new("Looking up your account settings…").font(font_semibold(17.0)));
                        ui.label(RichText::new(w.email.trim()).color(p.text3));
                        ui.add_space(20.0);
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            w.step = Step::Email;
                        }
                    });
                }
                Step::Credentials => {
                    let provider = w.found.as_ref().map(|f| f.provider.clone()).unwrap_or_else(|| w.editor.acc.provider.clone());
                    header(ui, &format!("Sign in to {}", if provider.is_empty() { "your account" } else { &provider }), &w.editor.acc.email.clone());
                    ui.add_space(10.0);
                    if let Some(e) = &w.error {
                        widgets::banner(ui, icons::WARNING, &util::truncate_chars(e, 300), p.danger, p.danger_soft, |_| {});
                        ui.add_space(8.0);
                    }
                    if let Some(f) = &w.found {
                        if let Some(note) = &f.note {
                            let url = f.password_help_url.clone();
                            widgets::banner(ui, icons::INFO, note, p.accent_text, p.accent_soft, |ui| {
                                if let Some(u) = &url {
                                    if widgets::link(ui, "Open help page").clicked() {
                                        let _ = open::that(u);
                                    }
                                }
                            });
                            ui.add_space(8.0);
                        }
                        if !w.manual {
                            ui.label(
                                RichText::new(format!("Found settings ({}): {} · {}", f.source, f.imap.summary(), f.smtp.summary()))
                                    .font(font(12.0))
                                    .color(p.text3),
                            );
                            ui.add_space(8.0);
                        }
                    }
                    let mut start_oauth = false;
                    if w.manual {
                        egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.6).show(ui, |ui| {
                            if account_form(ui, &mut w.editor, false) == Some("oauth") {
                                start_oauth = true;
                            }
                        });
                    } else {
                        match w.editor.acc.auth {
                            AuthKind::Password => {
                                ui.horizontal(|ui| {
                                    let r = widgets::text_input_ex(ui, &mut w.editor.password, "Password or app password", ui.available_width() - 40.0, !w.editor.show_password);
                                    if w.focus {
                                        r.request_focus();
                                        w.focus = false;
                                    }
                                    if widgets::icon_button(ui, if w.editor.show_password { icons::EYE_SLASH } else { icons::EYE }, "Show password").clicked() {
                                        w.editor.show_password = !w.editor.show_password;
                                    }
                                });
                            }
                            AuthKind::OAuth2 => {
                                if account_form_oauth_only(ui, &mut w.editor) {
                                    start_oauth = true;
                                }
                            }
                        }
                        ui.add_space(6.0);
                        if widgets::link(ui, "Change server settings").clicked() {
                            w.manual = true;
                        }
                    }
                    if start_oauth {
                        w.editor.oauth_waiting = true;
                        w.editor.oauth_result = None;
                        self.engine.send(Command::OAuthSignIn { account: Box::new(w.editor.acc.clone()) });
                    }
                    ui.add_space(18.0);
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        let ready = match w.editor.acc.auth {
                            AuthKind::Password => !w.editor.password.is_empty(),
                            AuthKind::OAuth2 => matches!(w.editor.oauth_result, Some(Ok(()))),
                        };
                        if (widgets::primary_button_ex(ui, Some(icons::SIGN_IN), "Sign in", ready).clicked() || (enter && ready && !w.manual)) && ready {
                            self.wizard_start_test(&mut w);
                        }
                        if widgets::secondary_button(ui, None, "Back").clicked() {
                            w.step = Step::Email;
                            w.error = None;
                            w.found = None;
                        }
                    });
                }
                Step::Testing => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        widgets::spinner(ui, 36.0);
                        ui.add_space(14.0);
                        ui.label(RichText::new("Signing in and checking your settings…").font(font_semibold(17.0)));
                        ui.add_space(12.0);
                    });
                    status_line(ui, "Incoming mail", &w.imap);
                    status_line(ui, "Outgoing mail", &w.smtp);
                }
                Step::Done => {
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 40.0, p.success_soft);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::CHECK_CIRCLE, icon_fill_font(52.0), p.success);
                        ui.add_space(12.0);
                        ui.label(RichText::new("You're all set!").font(font_semibold(24.0)));
                        ui.label(RichText::new(format!("{} was added. Your mail is downloading now.", w.editor.acc.email)).color(p.text2));
                        if let Some(warn) = &w.warning {
                            ui.add_space(10.0);
                            widgets::banner(ui, icons::WARNING, warn, p.warning, p.warning_soft, |_| {});
                        }
                        ui.add_space(22.0);
                        if widgets::primary_button(ui, Some(icons::TRAY), "Go to Inbox").clicked() {
                            close = true;
                        }
                        ui.add_space(8.0);
                        if widgets::link(ui, "Add another account").clicked() {
                            w = WizardState::new(false);
                        }
                    });
                }
            }
        });
        let dismissable = !(w.first_run && self.cfg.accounts.is_empty());
        if resp.should_close() && dismissable && !matches!(w.step, Step::Testing | Step::Discovering) {
            close = true;
        }
        if close {
            self.cfg.onboarded = true;
            self.mark_config_dirty();
        } else if self.wizard.is_none() {
            self.wizard = Some(w);
        }
        let _ = mix;
        let _ = icon_font;
    }

    fn wizard_start_test(&mut self, w: &mut WizardState) {
        let acc = &mut w.editor.acc;
        if acc.label.trim().is_empty() {
            acc.label = acc.email.clone();
        }
        if acc.imap.username.trim().is_empty() {
            acc.imap.username = acc.email.clone();
        }
        if acc.smtp.username.trim().is_empty() {
            acc.smtp.username = acc.email.clone();
        }
        w.imap = None;
        w.smtp = None;
        w.error = None;
        w.step = Step::Testing;
        self.engine.send(Command::TestAccount {
            account: Box::new(w.editor.acc.clone()),
            password: w.editor.password.clone(),
            smtp_password: w.editor.smtp_password.clone(),
        });
    }

    fn wizard_finish(&mut self, w: &mut WizardState) {
        let acc = w.editor.acc.clone();
        if acc.auth == AuthKind::Password {
            secrets::set(&acc.id, SecretKind::Password, &w.editor.password);
            if !acc.smtp_same_password && !w.editor.smtp_password.is_empty() {
                secrets::set(&acc.id, SecretKind::SmtpPassword, &w.editor.smtp_password);
            }
        }
        match self.cfg.account_mut(&acc.id) {
            Some(slot) => *slot = acc.clone(),
            None => self.cfg.accounts.push(acc.clone()),
        }
        if self.cfg.default_account.is_empty() || self.cfg.account(&self.cfg.default_account).map(|a| a.is_local()).unwrap_or(true) {
            self.cfg.default_account = acc.id.clone();
        }
        self.cfg.favorites.push(crate::config::FavoriteRef { account_id: acc.id.clone(), path: "INBOX".into() });
        self.cfg.onboarded = true;
        self.save_config_now();
        self.mail.folders_dirty = true;
        self.mail.list_dirty = true;
        self.toast(format!("{} added", acc.email), ToastKind::Success);
        // Show the new inbox as soon as it exists.
        if let Ok(Some(f)) = self.store.folder_by_role(&acc.id, FolderRole::Inbox) {
            self.select_view(crate::ui::mail::MailView::Folder(f.id));
        } else {
            self.mail.view = crate::ui::mail::MailView::AllInboxes;
        }
        self.module = crate::app::Module::Mail;
    }
}

fn header(ui: &mut Ui, title: &str, subtitle: &str) {
    let p = pal();
    ui.label(RichText::new(title).font(font_semibold(22.0)).color(p.text));
    if !subtitle.is_empty() {
        ui.add_space(2.0);
        ui.label(RichText::new(subtitle).font(font(14.0)).color(p.text2));
    }
}

fn status_line(ui: &mut Ui, label: &str, r: &Option<Result<String, String>>) {
    let p = pal();
    ui.horizontal_wrapped(|ui| {
        match r {
            None => {
                widgets::spinner(ui, 14.0);
            }
            Some(Ok(_)) => {
                ui.label(RichText::new(icons::CHECK_CIRCLE).font(icon_fill_font(16.0)).color(p.success));
            }
            Some(Err(_)) => {
                ui.label(RichText::new(icons::WARNING_CIRCLE).font(icon_fill_font(16.0)).color(p.danger));
            }
        }
        ui.label(RichText::new(label).font(font_semibold(14.0)));
        if let Some(r) = r {
            let text = match r {
                Ok(m) => m.clone(),
                Err(e) => util::truncate_chars(e, 200),
            };
            ui.label(RichText::new(text).font(font(13.0)).color(p.text2));
        }
    });
}

/// Compact OAuth section for the quick (non-manual) path. Returns true when
/// the user asked to start the browser sign-in.
fn account_form_oauth_only(ui: &mut Ui, ed: &mut AccountEditor) -> bool {
    let p = pal();
    let mut start = false;
    ui.label(RichText::new("This provider requires signing in with your browser (OAuth 2.0).").color(p.text2));
    ui.add_space(6.0);
    widgets::text_input(ui, &mut ed.acc.oauth.client_id, "OAuth client ID (from your app registration)", ui.available_width());
    ui.add_space(4.0);
    widgets::text_input_ex(ui, &mut ed.acc.oauth.client_secret, "Client secret (if your provider issued one)", ui.available_width(), true);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ed.oauth_waiting {
            widgets::spinner(ui, 16.0);
            ui.label("Finish signing in in your browser…");
        } else if widgets::secondary_button_ex(ui, Some(icons::SIGN_IN), "Sign in with browser", !ed.acc.oauth.client_id.trim().is_empty()).clicked() {
            start = true;
        }
        match &ed.oauth_result {
            Some(Ok(())) => {
                ui.label(RichText::new(format!("{} Signed in", icons::CHECK_CIRCLE)).font(icon_font(13.0)).color(p.success));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(util::truncate_chars(e, 140)).font(font(12.5)).color(p.danger));
            }
            None => {}
        }
    });
    start
}
