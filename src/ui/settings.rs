//! Settings dialog.

use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, DragValue, Frame, Id, Layout, Margin,
    Modal, Pos2, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::app::{App, Dialog, ToastKind};
use crate::config::*;
use crate::engine::Command;
use crate::icons;
use crate::model::SyncStatus;
use crate::secrets::{self, SecretKind};
use crate::theme::{self, font, font_semibold, icon_fill_font, icon_font, mix, pal, Accent, Density, ThemeMode};
use crate::ui::widgets::{self, setting_row};
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Accounts,
    Appearance,
    Layout,
    Compose,
    Rules,
    Categories,
    Privacy,
    Calendar,
    Notifications,
    Shortcuts,
    About,
}

impl Section {
    const ALL: [Section; 11] = [
        Section::Accounts,
        Section::Appearance,
        Section::Layout,
        Section::Compose,
        Section::Rules,
        Section::Categories,
        Section::Privacy,
        Section::Calendar,
        Section::Notifications,
        Section::Shortcuts,
        Section::About,
    ];
    fn label(self) -> &'static str {
        match self {
            Section::Accounts => "Accounts",
            Section::Appearance => "Appearance",
            Section::Layout => "Mail layout",
            Section::Compose => "Compose and reply",
            Section::Rules => "Rules",
            Section::Categories => "Categories",
            Section::Privacy => "Privacy & junk",
            Section::Calendar => "Calendar",
            Section::Notifications => "Notifications",
            Section::Shortcuts => "Keyboard shortcuts",
            Section::About => "About",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Section::Accounts => icons::USER_CIRCLE,
            Section::Appearance => icons::PALETTE,
            Section::Layout => icons::SQUARES_FOUR,
            Section::Compose => icons::NOTE_PENCIL,
            Section::Rules => icons::LIGHTNING,
            Section::Categories => icons::TAG,
            Section::Privacy => icons::SHIELD_CHECK,
            Section::Calendar => icons::CALENDAR_BLANK,
            Section::Notifications => icons::BELL,
            Section::Shortcuts => icons::KEYBOARD,
            Section::About => icons::INFO,
        }
    }
}

pub struct AccountEditor {
    pub acc: AccountConfig,
    pub password: String,
    pub smtp_password: String,
    pub has_password: bool,
    pub testing: bool,
    pub test: Option<(Result<String, String>, Result<String, String>)>,
    pub oauth_waiting: bool,
    pub oauth_result: Option<Result<(), String>>,
    /// The browser sign-in page, offered for copying if no browser opened.
    pub oauth_url: Option<String>,
    pub link_copied: bool,
    pub has_oauth_token: bool,
    /// The remembered Microsoft client ID was offered once already.
    pub ms_prefilled: bool,
    pub show_password: bool,
}

impl AccountEditor {
    pub fn blank(acc: AccountConfig) -> Self {
        AccountEditor {
            acc,
            password: String::new(),
            smtp_password: String::new(),
            has_password: false,
            testing: false,
            test: None,
            oauth_waiting: false,
            oauth_result: None,
            oauth_url: None,
            link_copied: false,
            has_oauth_token: false,
            ms_prefilled: false,
            show_password: false,
        }
    }

    fn new(acc: &AccountConfig) -> Self {
        AccountEditor {
            has_password: secrets::get(&acc.id, SecretKind::Password).is_some(),
            has_oauth_token: acc.auth == AuthKind::OAuth2 && secrets::get(&acc.id, SecretKind::OAuthRefresh).is_some(),
            oauth_url: None,
            link_copied: false,
            ms_prefilled: false,
            acc: acc.clone(),
            password: String::new(),
            smtp_password: String::new(),
            testing: false,
            test: None,
            oauth_waiting: false,
            oauth_result: None,
            show_password: false,
        }
    }
}

pub struct SettingsState {
    pub section: Section,
    pub account: Option<AccountEditor>,
    pub rule_edit: Option<Rule>,
    pub new_safe: String,
    pub new_blocked: String,
    pub new_category: String,
    pub google_setup: Option<crate::ui::oauth_setup::GoogleSetup>,
    /// Start the browser sign-in once Google setup is finished.
    pub sign_in_after_setup: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        SettingsState {
            section: Section::Appearance,
            account: None,
            rule_edit: None,
            new_safe: String::new(),
            new_blocked: String::new(),
            new_category: String::new(),
            google_setup: None,
            sign_in_after_setup: false,
        }
    }
}

impl SettingsState {
    pub fn accounts() -> Self {
        Self::section(Section::Accounts)
    }
    pub fn section(s: Section) -> Self {
        SettingsState {
            section: s,
            ..Default::default()
        }
    }
    pub fn edit_account(acc: &AccountConfig) -> Self {
        SettingsState {
            section: Section::Accounts,
            account: Some(AccountEditor::new(acc)),
            ..Default::default()
        }
    }
    pub fn on_test_result(&mut self, imap: Result<String, String>, smtp: Result<String, String>) {
        if let Some(a) = self.account.as_mut() {
            if a.testing {
                a.testing = false;
                a.test = Some((imap, smtp));
            }
        }
    }
    pub fn on_oauth(&mut self, account_id: &str, result: &Result<Box<OAuthConfig>, String>) {
        if let Some(a) = self.account.as_mut() {
            if a.acc.id == account_id {
                a.oauth_waiting = false;
                if let Ok(oauth) = result {
                    // Keep the key the new token belongs to, so Save can't undo it.
                    a.acc.oauth = (**oauth).clone();
                    a.acc.auth = AuthKind::OAuth2;
                    a.has_oauth_token = true;
                }
                a.oauth_result = Some(result.as_ref().map(|_| ()).map_err(|e| e.clone()));
            }
        }
    }

    pub fn on_oauth_url(&mut self, account_id: &str, url: &str) {
        if let Some(a) = self.account.as_mut().filter(|a| a.acc.id == account_id) {
            a.oauth_url = Some(url.to_string());
        }
    }
}

/// Browser sign-in controls for an OAuth account. Returns `"oauth"` to start
/// signing in, `"oauth-cancel"` or `"google-setup"`.
pub fn oauth_section(ui: &mut Ui, ed: &mut AccountEditor, apps: &OAuthApps) -> Option<&'static str> {
    let p = pal();
    let mut action = None;
    let provider = ed.acc.oauth.provider.clone();
    let name = crate::engine::oauth::provider_name(&provider);
    let key = crate::engine::oauth::key_for_sign_in(apps, &ed.acc.oauth);
    if provider == "google" && key.is_none() {
        Frame::NONE
            .fill(p.accent_soft)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::KEY).font(icon_font(18.0)).color(p.accent_text));
                    ui.label(RichText::new("One-time setup needed").font(font_semibold(15.0)).color(p.accent_text));
                });
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Google asks every mail app for its own key. RustMail walks you through making yours: about 5 minutes, and only once on this computer.")
                        .font(font(13.5))
                        .color(p.text),
                );
                ui.add_space(10.0);
                if widgets::primary_button(ui, Some(icons::KEY), "Set up Google sign-in").clicked() {
                    action = Some("google-setup");
                }
            });
        return action;
    }
    if provider == "microsoft" && crate::engine::oauth::builtin_key("microsoft").is_none() {
        if !ed.ms_prefilled {
            ed.ms_prefilled = true;
            if ed.acc.oauth.client_id.trim().is_empty() && apps.microsoft.is_set() {
                ed.acc.oauth.client_id = apps.microsoft.client_id.clone();
            }
        }
        ui.label(
            RichText::new("Microsoft requires an app registration for mail apps. Paste its Application (client) ID; see the README for how to create one.")
                .font(font(13.0))
                .color(p.text2),
        );
        ui.add_space(4.0);
        widgets::text_input(ui, &mut ed.acc.oauth.client_id, "Application (client) ID", ui.available_width());
        ui.add_space(6.0);
    }
    if ed.oauth_waiting {
        ui.horizontal(|ui| {
            widgets::spinner(ui, 16.0);
            ui.label(RichText::new("Finish signing in in your browser…").font(font(14.0)));
            ui.add_space(8.0);
            if widgets::secondary_button(ui, None, "Cancel").clicked() {
                action = Some("oauth-cancel");
            }
        });
        if let Some(url) = &ed.oauth_url {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Browser didn't open?").font(font(12.5)).color(p.text3));
                let label = if ed.link_copied { "Link copied: paste it into your browser" } else { "Copy the sign-in link" };
                if widgets::link(ui, label).clicked() {
                    ui.ctx().copy_text(url.clone());
                    ed.link_copied = true;
                }
            });
        }
        if provider == "google" {
            ui.label(
                RichText::new("If Google warns that the app is being tested or hasn't been verified, click Continue (or Advanced, then \"Go to … (unsafe)\"). It's your own key, so that's safe. If Google shows an error page instead, close it and press Cancel.")
                    .font(font(12.5))
                    .color(p.text3),
            );
        }
    } else {
        let label = if ed.has_oauth_token { format!("Sign in again with {name}") } else { format!("Sign in with {name}") };
        let ready = key.is_some() || !ed.acc.oauth.client_id.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            if widgets::primary_button_ex(ui, Some(icons::SIGN_IN), &label, ready).clicked() {
                action = Some("oauth");
            }
            if let Some(Ok(())) = &ed.oauth_result {
                ui.label(RichText::new(format!("{} Signed in", icons::CHECK_CIRCLE)).font(icon_font(13.5)).color(p.success));
            }
        });
        if let Some(Err(e)) = &ed.oauth_result {
            ui.add_space(4.0);
            ui.label(RichText::new(util::truncate_chars(e, 400)).font(font(12.5)).color(p.danger));
        }
    }
    if provider == "google" {
        if let Some(k) = &key {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("App key: {}", crate::ui::oauth_setup::describe(k))).font(font(12.0)).color(p.text3));
                if !ed.oauth_waiting && widgets::link(ui, "Change").clicked() {
                    action = Some("google-setup");
                }
            });
        }
    }
    action
}

fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    let p = pal();
    ui.label(RichText::new(title).font(font_semibold(15.0)).color(p.text));
    ui.add_space(6.0);
    Frame::NONE
        .fill(p.card_alt)
        .stroke(Stroke::new(1.0, mix(p.stroke, p.card, 0.3)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(18.0);
}

fn radio_row<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) {
    for (v, label) in options {
        ui.radio_value(value, *v, *label);
    }
}

fn server_fields(ui: &mut Ui, id: &str, s: &mut ServerConfig, width: f32) {
    let p = pal();
    egui::Grid::new(id).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(RichText::new("Server").color(p.text2));
        widgets::text_input(ui, &mut s.host, "mail.example.com", width);
        ui.end_row();
        ui.label(RichText::new("Port").color(p.text2));
        ui.horizontal(|ui| {
            ui.add(DragValue::new(&mut s.port).range(1..=65535));
            ui.add_space(12.0);
            ui.label(RichText::new("Security").color(p.text2));
            let before = s.security;
            egui::ComboBox::from_id_salt((id, "sec")).selected_text(s.security.label()).show_ui(ui, |ui| {
                for sec in Security::ALL {
                    ui.selectable_value(&mut s.security, sec, sec.label());
                }
            });
            if before != s.security {
                // Suggest the matching standard port.
                let imap = id.contains("imap");
                s.port = match (imap, s.security) {
                    (true, Security::Tls) => 993,
                    (true, _) => 143,
                    (false, Security::Tls) => 465,
                    (false, Security::StartTls) => 587,
                    (false, Security::None) => 25,
                };
            }
        });
        ui.end_row();
        ui.label(RichText::new("User name").color(p.text2));
        widgets::text_input(ui, &mut s.username, "Usually your e-mail address", width);
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut s.accept_invalid_certs, "Allow untrusted (self-signed) certificates");
        ui.end_row();
    });
}

/// Shared by the settings dialog and the setup wizard.
pub fn account_form(ui: &mut Ui, ed: &mut AccountEditor, show_identity: bool, apps: &OAuthApps) -> Option<&'static str> {
    let p = pal();
    let mut action = None;
    let w = (ui.available_width() - 140.0).max(200.0);
    if show_identity {
        group(ui, "Identity", |ui| {
            egui::Grid::new("identity").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("Your name").color(p.text2));
                widgets::text_input(ui, &mut ed.acc.display_name, "Shown to people you write to", w);
                ui.end_row();
                ui.label(RichText::new("E-mail address").color(p.text2));
                widgets::text_input(ui, &mut ed.acc.email, "you@example.com", w);
                ui.end_row();
                ui.label(RichText::new("Account name").color(p.text2));
                widgets::text_input(ui, &mut ed.acc.label, "e.g. Work, Personal", w);
                ui.end_row();
            });
        });
    }
    group(ui, "Sign-in", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut ed.acc.auth, AuthKind::Password, "Password / app password");
            ui.radio_value(&mut ed.acc.auth, AuthKind::OAuth2, "OAuth 2.0 (sign in with browser)");
        });
        ui.add_space(6.0);
        match ed.acc.auth {
            AuthKind::Password => {
                ui.horizontal(|ui| {
                    let hint = if ed.has_password { "•••••••• (saved — leave blank to keep)" } else { "Password" };
                    widgets::text_input_ex(ui, &mut ed.password, hint, w, !ed.show_password);
                    if widgets::icon_button(ui, if ed.show_password { icons::EYE_SLASH } else { icons::EYE }, "Show password").clicked() {
                        ed.show_password = !ed.show_password;
                    }
                });
                ui.add_space(4.0);
                ui.checkbox(&mut ed.acc.smtp_same_password, "Use the same password for sending (SMTP)");
                if !ed.acc.smtp_same_password {
                    widgets::text_input_ex(ui, &mut ed.smtp_password, "SMTP password", w, true);
                }
            }
            AuthKind::OAuth2 => {
                if ed.acc.oauth.auth_url.is_empty() {
                    let guess = if util::domain_of(&ed.acc.email).contains("gmail") { "google" } else { "microsoft" };
                    if let Some(p) = crate::engine::oauth::preset(guess) {
                        ed.acc.oauth = p;
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Provider").color(p.text2));
                    let label = match ed.acc.oauth.provider.as_str() {
                        "google" => "Google",
                        "microsoft" => "Microsoft",
                        _ => "Custom",
                    };
                    egui::ComboBox::from_id_salt("oauth-prov").selected_text(label).show_ui(ui, |ui| {
                        // Only a real change of provider resets the key: the account's
                        // refresh token belongs to the key it has now.
                        for (key, name) in [("google", "Google"), ("microsoft", "Microsoft")] {
                            if ui.selectable_label(ed.acc.oauth.provider == key, name).clicked() && ed.acc.oauth.provider != key {
                                if let Some(mut p) = crate::engine::oauth::preset(key) {
                                    // Back from "Custom": keep the key typed there if it belongs
                                    // to this provider (Microsoft public clients have no secret).
                                    if ed.acc.oauth.provider == "custom" {
                                        let id = ed.acc.oauth.client_id.trim();
                                        let google_id = id.ends_with(".apps.googleusercontent.com");
                                        if key == "google" && google_id {
                                            p.client_id = id.to_string();
                                            p.client_secret = ed.acc.oauth.client_secret.clone();
                                        } else if key == "microsoft" && !google_id {
                                            p.client_id = id.to_string();
                                        }
                                    }
                                    ed.acc.oauth = p;
                                }
                            }
                        }
                        if ui.selectable_label(ed.acc.oauth.provider == "custom", "Custom").clicked() {
                            ed.acc.oauth.provider = "custom".into();
                        }
                    });
                });
                ui.add_space(6.0);
                if ed.acc.oauth.provider == "custom" {
                    egui::Grid::new("oauth").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new("Client ID").color(p.text2));
                        widgets::text_input(ui, &mut ed.acc.oauth.client_id, "From your app registration", w);
                        ui.end_row();
                        ui.label(RichText::new("Client secret").color(p.text2));
                        widgets::text_input_ex(ui, &mut ed.acc.oauth.client_secret, "If your provider issued one", w, true);
                        ui.end_row();
                        ui.label(RichText::new("Authorize URL").color(p.text2));
                        widgets::text_input(ui, &mut ed.acc.oauth.auth_url, "https://…/authorize", w);
                        ui.end_row();
                        ui.label(RichText::new("Token URL").color(p.text2));
                        widgets::text_input(ui, &mut ed.acc.oauth.token_url, "https://…/token", w);
                        ui.end_row();
                        ui.label(RichText::new("Scopes").color(p.text2));
                        widgets::text_input(ui, &mut ed.acc.oauth.scopes, "space separated", w);
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                }
                action = oauth_section(ui, ed, apps);
            }
        }
    });
    group(ui, "Incoming mail (IMAP)", |ui| server_fields(ui, "imap-fields", &mut ed.acc.imap, w));
    group(ui, "Outgoing mail (SMTP)", |ui| server_fields(ui, "smtp-fields", &mut ed.acc.smtp, w));
    action
}

impl App {
    pub fn settings_ui(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.settings.take() else { return };
        let p = pal();
        let before = self.cfg.clone();
        let mut open = true;
        let screen = ctx.content_rect();
        let size = vec2(screen.width().min(980.0) - 40.0, screen.height().min(720.0) - 60.0);
        let modal = Modal::new(Id::new("settings")).frame(
            Frame::NONE
                .fill(p.card)
                .corner_radius(CornerRadius::same(10))
                .shadow(crate::theme::window_shadow()),
        );
        let resp = modal.show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_height(size.y);
            ui.horizontal_top(|ui| {
                // Navigation
                Frame::NONE
                    .fill(if p.dark { p.card_alt } else { mix(p.bg, p.card, 0.5) })
                    .corner_radius(CornerRadius { nw: 10, sw: 10, ne: 0, se: 0 })
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                      ui.vertical(|ui| {
                        ui.set_width(210.0);
                        ui.set_height(size.y - 28.0);
                        ui.label(RichText::new("Settings").font(font_semibold(20.0)));
                        ui.add_space(12.0);
                        for s in Section::ALL {
                            let sel = st.section == s;
                            let (rect, r) = ui.allocate_exact_size(vec2(210.0, 34.0), Sense::click());
                            if sel || r.hovered() {
                                ui.painter().rect_filled(rect, CornerRadius::same(4), if sel { p.selected } else { p.hover });
                            }
                            if sel {
                                ui.painter().rect_filled(egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 9.0), vec2(3.0, 16.0)), CornerRadius::same(2), p.accent);
                            }
                            ui.painter().text(Pos2::new(rect.left() + 20.0, rect.center().y), Align2::CENTER_CENTER, s.icon(), if sel { icon_fill_font(16.0) } else { icon_font(16.0) }, if sel { p.accent } else { p.text2 });
                            ui.painter().text(Pos2::new(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, s.label(), if sel { font_semibold(14.0) } else { font(14.0) }, p.text);
                            if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                st.section = s;
                                st.account = None;
                                st.rule_edit = None;
                            }
                        }
                      });
                    });
                // Content
                ui.vertical(|ui| {
                    ui.set_width(size.x - 238.0);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        widgets::heading(ui, st.section.label(), 20.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(12.0);
                            if widgets::icon_button(ui, icons::X, "Close").clicked() {
                                open = false;
                            }
                        });
                    });
                    ui.add_space(8.0);
                    ScrollArea::vertical().id_salt(("settings", st.section as u8)).auto_shrink([false, false]).show(ui, |ui| {
                        Frame::NONE.inner_margin(Margin { left: 20, right: 22, top: 4, bottom: 20 }).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            match st.section {
                                Section::Accounts => self.settings_accounts(ui, &mut st),
                                Section::Appearance => self.settings_appearance(ui),
                                Section::Layout => self.settings_layout(ui),
                                Section::Compose => self.settings_compose(ui),
                                Section::Rules => self.settings_rules(ui, &mut st),
                                Section::Categories => self.settings_categories(ui, &mut st),
                                Section::Privacy => self.settings_privacy(ui, &mut st),
                                Section::Calendar => self.settings_calendar(ui),
                                Section::Notifications => self.settings_notifications(ui),
                                Section::Shortcuts => {
                                    egui::Grid::new("sc").num_columns(2).spacing([28.0, 10.0]).striped(true).show(ui, |ui| {
                                        for (k, d) in crate::ui::shell::SHORTCUTS {
                                            ui.label(RichText::new(*k).font(font_semibold(13.5)));
                                            ui.label(RichText::new(*d).color(p.text2));
                                            ui.end_row();
                                        }
                                    });
                                }
                                Section::About => self.settings_about(ui),
                            }
                        });
                    });
                });
            });
        });
        if resp.should_close() && st.account.is_none() && st.rule_edit.is_none() && st.google_setup.is_none() {
            open = false;
        }
        if self.cfg != before {
            self.mark_config_dirty();
            self.mail.list_dirty = true;
            self.mail.folders_dirty = true;
            self.cal.dirty = true;
            util::set_time_24h(self.cfg.appearance.time_24h);
            util::set_first_day_monday(self.cfg.calendar.first_day_monday);
        }
        if open && self.settings.is_none() {
            self.settings = Some(st);
        }
    }

    /// Starts the browser sign-in for the account in `ed` with the app key
    /// RustMail should use for it.
    /// The account itself only switches to that key once the sign-in has
    /// succeeded (see the `OAuthResult` handling), because its current refresh
    /// token belongs to its current key.
    pub fn start_oauth(&mut self, ed: &mut AccountEditor) {
        let mut acc = ed.acc.clone();
        if let Some(k) = crate::engine::oauth::key_for_sign_in(&self.cfg.oauth_apps, &acc.oauth) {
            crate::engine::oauth::apply_key(&mut acc.oauth, &k);
        }
        // Older accounts only asked for mail; a new sign-in asks for calendar too.
        crate::engine::oauth::upgrade_scopes(&mut acc.oauth);
        ed.oauth_waiting = true;
        ed.oauth_result = None;
        ed.oauth_url = None;
        ed.link_copied = false;
        self.engine.send(Command::OAuthSignIn { account: Box::new(acc) });
    }

    /// Saves the key from the Google setup guide for every Google account.
    pub fn save_google_key(&mut self, key: OAuthClientKey) {
        self.toast(
            if key.source.is_empty() { "Google sign-in is ready".to_string() } else { format!("Google sign-in is ready (key from {})", key.source) },
            ToastKind::Success,
        );
        self.cfg.oauth_apps.google = key;
        self.save_config_now();
    }

    fn settings_accounts(&mut self, ui: &mut Ui, st: &mut SettingsState) {
        let p = pal();
        if let Some(setup) = st.google_setup.as_mut() {
            match setup.ui(ui) {
                crate::ui::oauth_setup::SetupResult::Working => {}
                crate::ui::oauth_setup::SetupResult::Cancel => {
                    st.google_setup = None;
                    st.sign_in_after_setup = false;
                }
                crate::ui::oauth_setup::SetupResult::Done(key) => {
                    st.google_setup = None;
                    self.save_google_key(key);
                    if std::mem::take(&mut st.sign_in_after_setup) {
                        if let Some(ed) = st.account.as_mut().filter(|e| e.acc.oauth.provider == "google") {
                            self.start_oauth(ed);
                        }
                    }
                }
            }
            return;
        }
        if let Some(ed) = st.account.as_mut() {
            let mut close = false;
            ui.horizontal(|ui| {
                if widgets::subtle_button(ui, icons::ARROW_LEFT, "All accounts").clicked() {
                    close = true;
                }
            });
            ui.add_space(8.0);
            if ed.acc.is_archive() {
                group(ui, "Outlook mailbox", |ui| {
                    ui.label("This mailbox was imported from an Outlook data file and lives only on this computer. You can read, search and organize it; to send mail, add an e-mail account.");
                });
            } else if ed.acc.is_local() {
                group(ui, "Demo mailbox", |ui| {
                    ui.label("This offline account lives only on this computer. It's great for trying things out.");
                });
            }
            let action = if ed.acc.is_local() { None } else { account_form(ui, ed, true, &self.cfg.oauth_apps) };
            match action {
                Some("oauth") => self.start_oauth(ed),
                Some("oauth-cancel") => {
                    ed.oauth_waiting = false;
                    self.engine.send(Command::OAuthCancel { account_id: ed.acc.id.clone() });
                }
                Some("google-setup") => {
                    st.google_setup = Some(crate::ui::oauth_setup::GoogleSetup::new());
                    st.sign_in_after_setup = true;
                    return;
                }
                _ => {}
            }
            group(ui, "Sync", |ui| {
                setting_row(ui, "Check for new mail every", "Minutes between automatic syncs", |ui| {
                    ui.add(DragValue::new(&mut ed.acc.sync_interval_min).range(1..=240).suffix(" min"));
                });
                setting_row(ui, "Messages to download initially", "Newest messages per folder on first sync", |ui| {
                    ui.add(DragValue::new(&mut ed.acc.initial_sync_count).range(50..=20000).speed(50));
                });
                setting_row(ui, "Get new mail instantly", "Keeps a connection open with IMAP IDLE (push)", |ui| {
                    widgets::toggle(ui, &mut ed.acc.push);
                });
                setting_row(ui, "Download message bodies for offline reading", "", |ui| {
                    widgets::toggle(ui, &mut ed.acc.prefetch_bodies);
                });
                setting_row(ui, "Save a copy of sent messages", "Turn off for Gmail and Outlook.com, which do this automatically", |ui| {
                    widgets::toggle(ui, &mut ed.acc.save_sent_copy);
                });
                setting_row(ui, "Account enabled", "", |ui| {
                    widgets::toggle(ui, &mut ed.acc.enabled);
                });
            });
            group(ui, "Signature", |ui| {
                ui.add(
                    TextEdit::multiline(&mut ed.acc.signature)
                        .hint_text("Your name\nTitle · Company\nPhone")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::same(8)),
                );
                ui.checkbox(&mut ed.acc.signature_on_reply, "Add signature to replies and forwards");
            });
            if let Some((imap, smtp)) = &ed.test {
                group(ui, "Connection test", |ui| {
                    for (label, r) in [("Incoming", imap), ("Outgoing", smtp)] {
                        ui.horizontal_wrapped(|ui| {
                            let (icon, color, text) = match r {
                                Ok(m) => (icons::CHECK_CIRCLE, p.success, m.clone()),
                                Err(e) => (icons::WARNING_CIRCLE, p.danger, e.clone()),
                            };
                            ui.label(RichText::new(icon).font(icon_fill_font(16.0)).color(color));
                            ui.label(RichText::new(format!("{label}: ")).font(font_semibold(13.5)));
                            ui.label(RichText::new(text).font(font(13.0)));
                        });
                    }
                });
            }
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, Some(icons::CHECK), "Save").clicked() {
                    let acc = ed.acc.clone();
                    if !ed.password.is_empty() {
                        secrets::set(&acc.id, SecretKind::Password, &ed.password);
                        ed.has_password = true;
                    }
                    if !ed.smtp_password.is_empty() {
                        secrets::set(&acc.id, SecretKind::SmtpPassword, &ed.smtp_password);
                    }
                    match self.cfg.account_mut(&acc.id) {
                        Some(slot) => *slot = acc.clone(),
                        None => self.cfg.accounts.push(acc.clone()),
                    }
                    self.save_config_now();
                    self.engine.send(Command::SyncAccount { account_id: acc.id.clone(), full: false });
                    self.mail.folders_dirty = true;
                    self.toast("Account saved", ToastKind::Success);
                    close = true;
                }
                if !ed.acc.is_local() {
                    if ed.testing {
                        widgets::spinner(ui, 18.0);
                        ui.label("Testing…");
                    } else if widgets::secondary_button(ui, Some(icons::PLUGS_CONNECTED), "Test connection").clicked() {
                        ed.testing = true;
                        ed.test = None;
                        self.engine.send(Command::TestAccount {
                            account: Box::new(ed.acc.clone()),
                            password: ed.password.clone(),
                            smtp_password: ed.smtp_password.clone(),
                        });
                    }
                }
                if widgets::secondary_button(ui, None, "Cancel").clicked() {
                    close = true;
                }
            });
            if close {
                st.account = None;
            }
            return;
        }

        ui.label(RichText::new("Add, remove and manage your e-mail accounts.").color(p.text2));
        ui.add_space(12.0);
        let accounts = self.cfg.accounts.clone();
        for a in &accounts {
            Frame::NONE
                .fill(p.card_alt)
                .stroke(Stroke::new(1.0, mix(p.stroke, p.card, 0.3)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::avatar(ui, 44.0, &a.display_name, &a.email);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(a.name()).font(font_semibold(15.0)));
                                if self.cfg.default_account == a.id {
                                    widgets::chip(ui, "Default", p.accent);
                                }
                                if !a.enabled {
                                    widgets::chip(ui, "Disabled", p.text3);
                                }
                            });
                            ui.label(RichText::new(&a.email).font(font(13.0)).color(p.text2));
                            let status = match self.statuses.get(&a.id).filter(|_| !a.is_archive()) {
                                Some(SyncStatus::Error(e)) => (format!("{} {}", icons::WARNING, util::truncate_chars(e, 90)), p.danger),
                                Some(SyncStatus::Syncing(s)) => (s.clone(), p.text3),
                                Some(SyncStatus::Idle { last }) if *last > 0 => (format!("Synced {}", util::time_ago(*last)), p.text3),
                                _ if a.is_archive() => ("Imported from Outlook · stored on this computer".to_string(), p.text3),
                                _ => (if a.is_local() { "Offline demo account".to_string() } else { a.provider.clone() }, p.text3),
                            };
                            ui.label(RichText::new(status.0).font(icon_font(12.5)).color(status.1));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let more = widgets::icon_button(ui, icons::DOTS_THREE, "More");
                            egui::Popup::menu(&more).show(|ui| {
                                if widgets::menu_item(ui, icons::STAR, "Set as default").clicked() {
                                    self.cfg.default_account = a.id.clone();
                                }
                                if widgets::menu_item(ui, icons::ARROWS_CLOCKWISE, "Sync now").clicked() {
                                    self.engine.send(Command::SyncAccount { account_id: a.id.clone(), full: true });
                                }
                                if widgets::menu_item(ui, icons::TRASH, "Remove account").clicked() {
                                    self.dialog = Some(Dialog::RemoveAccount { account_id: a.id.clone(), name: a.name().to_string() });
                                }
                            });
                            if widgets::secondary_button(ui, Some(icons::GEAR), "Manage").clicked() {
                                st.account = Some(AccountEditor::new(a));
                            }
                        });
                    });
                });
            ui.add_space(10.0);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, Some(icons::USER_PLUS), "Add account").clicked() {
                self.wizard = Some(crate::ui::wizard::WizardState::new(false));
            }
            if widgets::secondary_button(ui, Some(icons::DOWNLOAD_SIMPLE), "Import from Outlook…")
                .on_hover_text("Bring in mail, contacts, calendar and tasks from an Outlook data file (.pst or .ost)")
                .clicked()
            {
                self.import_outlook();
            }
            if !self.cfg.accounts.iter().any(|a| a.is_demo()) && widgets::secondary_button(ui, Some(icons::SPARKLE), "Add demo mailbox").clicked() {
                self.add_demo();
            }
        });
        ui.add_space(20.0);
        let google = self.cfg.oauth_apps.google.clone();
        let builtin = crate::engine::oauth::builtin_key("google");
        group(ui, "Google sign-in", |ui| {
            let (text, color) = if google.is_set() {
                (format!("Ready. Every Google account signs in with the key {}.", crate::ui::oauth_setup::describe(&google)), p.text)
            } else if builtin.is_some() {
                ("Ready. This copy of RustMail has a Google key built in.".to_string(), p.text)
            } else {
                ("Not set up yet. You'll be guided through it the first time you add a Google account.".to_string(), p.text2)
            };
            ui.label(RichText::new(text).font(font(13.5)).color(color));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if google.is_set() { "Use a different key" } else { "Set up now" };
                if widgets::secondary_button(ui, Some(icons::KEY), label).clicked() {
                    st.google_setup = Some(crate::ui::oauth_setup::GoogleSetup::new());
                    st.sign_in_after_setup = false;
                }
                if google.is_set() {
                    if !google.project_id.is_empty() && widgets::secondary_button(ui, Some(icons::ARROW_SQUARE_OUT), "Open in Google Cloud").clicked() {
                        let _ = open::that_detached(format!("https://console.cloud.google.com/auth/clients?project={}", google.project_id));
                    }
                    if widgets::secondary_button(ui, Some(icons::TRASH), "Forget key").clicked() {
                        self.cfg.oauth_apps.google = OAuthClientKey::default();
                        self.save_config_now();
                    }
                }
            });
            if google.is_set() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Accounts that already signed in keep working if you forget or change the key.")
                        .font(font(12.0))
                        .color(p.text3),
                );
            }
        });
        let microsoft = self.cfg.oauth_apps.microsoft.clone();
        if microsoft.is_set() && crate::engine::oauth::builtin_key("microsoft").is_none() {
            group(ui, "Microsoft sign-in", |ui| {
                ui.label(
                    RichText::new(format!("New Microsoft accounts start with the client ID {}.", microsoft.client_id))
                        .font(font(13.5))
                        .color(p.text),
                );
                ui.add_space(8.0);
                if widgets::secondary_button(ui, Some(icons::TRASH), "Forget client ID").clicked() {
                    self.cfg.oauth_apps.microsoft = OAuthClientKey::default();
                    self.save_config_now();
                }
            });
        }
        ui.label(
            RichText::new(format!("Passwords are stored in your {}.", secrets::backend_name()))
                .font(font(12.0))
                .color(p.text3),
        );
    }

    pub fn add_demo(&mut self) {
        match crate::demo::create(&self.store, &mut self.cfg) {
            Ok(_) => {
                self.cfg.onboarded = true;
                self.save_config_now();
                self.mail.select_default_folder(&self.store, &self.cfg);
                self.people.dirty = true;
                self.todo.dirty = true;
                self.cal.dirty = true;
                self.toast("Demo mailbox added", ToastKind::Success);
            }
            Err(e) => {
                self.toast(format!("Couldn't create demo: {e}"), ToastKind::Error);
            }
        }
    }

    fn settings_appearance(&mut self, ui: &mut Ui) {
        let p = pal();
        group(ui, "Theme", |ui| {
            ui.horizontal(|ui| {
                for t in ThemeMode::ALL {
                    let sel = self.cfg.appearance.theme == t;
                    let (rect, r) = ui.allocate_exact_size(vec2(150.0, 110.0), Sense::click());
                    ui.painter().rect_filled(rect, CornerRadius::same(8), p.card);
                    ui.painter().rect_stroke(rect, CornerRadius::same(8), Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { p.accent } else { p.stroke }), egui::StrokeKind::Inside);
                    let preview = egui::Rect::from_min_size(rect.left_top() + vec2(12.0, 12.0), vec2(126.0, 62.0));
                    let (bg, card) = match t {
                        ThemeMode::Light => (theme::hex(0xF0F0F0), Color32::WHITE),
                        ThemeMode::Dark => (theme::hex(0x1A1A1A), theme::hex(0x292929)),
                        ThemeMode::System => (theme::hex(0x8A8A8A), theme::hex(0xD0D0D0)),
                    };
                    ui.painter().rect_filled(preview, CornerRadius::same(4), bg);
                    if t == ThemeMode::System {
                        let half = egui::Rect::from_min_max(preview.center_top(), preview.right_bottom());
                        ui.painter().rect_filled(half, CornerRadius { ne: 4, se: 4, nw: 0, sw: 0 }, theme::hex(0x1A1A1A));
                    }
                    let c1 = egui::Rect::from_min_size(preview.left_top() + vec2(30.0, 8.0), vec2(40.0, 46.0));
                    let c2 = egui::Rect::from_min_size(preview.left_top() + vec2(74.0, 8.0), vec2(44.0, 46.0));
                    ui.painter().rect_filled(c1, CornerRadius::same(3), card);
                    ui.painter().rect_filled(c2, CornerRadius::same(3), card);
                    ui.painter().rect_filled(egui::Rect::from_min_size(preview.left_top() + vec2(8.0, 10.0), vec2(14.0, 4.0)), CornerRadius::same(2), p.accent);
                    ui.painter().text(Pos2::new(rect.center().x, rect.bottom() - 18.0), Align2::CENTER_CENTER, t.label(), if sel { font_semibold(13.0) } else { font(13.0) }, p.text);
                    if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        self.cfg.appearance.theme = t;
                    }
                }
            });
        });
        group(ui, "Accent color", |ui| {
            ui.horizontal_wrapped(|ui| {
                for a in Accent::ALL {
                    let sel = self.cfg.appearance.accent == a;
                    let (rect, r) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::click());
                    let c = a.base();
                    if sel {
                        ui.painter().circle_stroke(rect.center(), 18.0, Stroke::new(2.0, c));
                    }
                    ui.painter().circle_filled(rect.center(), 14.0, c);
                    if sel {
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icons::CHECK, icon_fill_font(14.0), Color32::WHITE);
                    }
                    if r.on_hover_text(a.label()).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        self.cfg.appearance.accent = a;
                    }
                }
            });
        });
        group(ui, "Density", |ui| {
            ui.label(RichText::new("How much space appears between items in lists").font(font(12.5)).color(p.text3));
            ui.horizontal(|ui| {
                for d in Density::ALL {
                    ui.radio_value(&mut self.cfg.appearance.density, d, d.label());
                }
            });
        });
        group(ui, "Text and window", |ui| {
            setting_row(ui, "Zoom", "Scales the whole interface (Ctrl + / Ctrl −)", |ui| {
                let mut pct = (self.cfg.appearance.zoom * 100.0).round() as i32;
                if ui.add(egui::Slider::new(&mut pct, 70..=200).suffix("%")).changed() {
                    self.cfg.appearance.zoom = pct as f32 / 100.0;
                }
            });
            setting_row(ui, "Use 24-hour time", "", |ui| {
                widgets::toggle(ui, &mut self.cfg.appearance.time_24h);
            });
            setting_row(ui, "Slim toolbar", "One compact row with the everyday commands instead of the ribbon; everything else is in its ⋯ menu", |ui| {
                widgets::toggle(ui, &mut self.cfg.appearance.slim);
            });
            setting_row(ui, "Modern title bar", "Draws the search box in the title bar (takes effect after restart)", |ui| {
                widgets::toggle(ui, &mut self.cfg.appearance.custom_titlebar);
            });
        });
    }

    fn settings_layout(&mut self, ui: &mut Ui) {
        let m = &mut self.cfg.mail;
        group(ui, "Focused Inbox", |ui| {
            setting_row(ui, "Sort messages into Focused and Other", "Newsletters, notifications and bulk mail go to Other", |ui| {
                widgets::toggle(ui, &mut m.focused_inbox);
            });
        });
        group(ui, "Reading pane", |ui| {
            radio_row(ui, &mut m.reading_pane, &[
                (ReadingPanePos::Right, ReadingPanePos::Right.label()),
                (ReadingPanePos::Bottom, ReadingPanePos::Bottom.label()),
                (ReadingPanePos::Off, ReadingPanePos::Off.label()),
            ]);
        });
        group(ui, "Message list", |ui| {
            setting_row(ui, "Group messages by conversation", "", |ui| {
                widgets::toggle(ui, &mut m.conversation_view);
            });
            setting_row(ui, "Show preview text", "", |ui| {
                widgets::toggle(ui, &mut m.show_preview);
            });
            setting_row(ui, "Show sender images", "", |ui| {
                widgets::toggle(ui, &mut m.show_avatars);
            });
            setting_row(ui, "Group by date (Today, Yesterday…)", "", |ui| {
                widgets::toggle(ui, &mut m.group_by_date);
            });
            setting_row(ui, "Show \"All inboxes\" when you have several accounts", "", |ui| {
                widgets::toggle(ui, &mut m.unified_inbox);
            });
        });
        group(ui, "Mark as read", |ui| {
            radio_row(ui, &mut m.mark_read, &[
                (MarkReadMode::OnSelect, "When the selection changes"),
                (MarkReadMode::AfterDelay, "When viewed in the reading pane for a few seconds"),
                (MarkReadMode::Never, "Don't automatically mark items as read"),
            ]);
            if m.mark_read == MarkReadMode::AfterDelay {
                ui.add(egui::Slider::new(&mut m.mark_read_delay_s, 1..=30).suffix(" seconds"));
            }
        });
    }

    fn settings_compose(&mut self, ui: &mut Ui) {
        let c = &mut self.cfg.compose;
        group(ui, "Message format", |ui| {
            setting_row(ui, "Compose messages with formatting (HTML)", "Use **bold**, *italic*, lists and links; a plain-text copy is always included", |ui| {
                widgets::toggle(ui, &mut c.html);
            });
            setting_row(ui, "Open new messages in a separate window", "", |ui| {
                widgets::toggle(ui, &mut c.pop_out);
            });
            setting_row(ui, "Request read receipts by default", "", |ui| {
                widgets::toggle(ui, &mut c.read_receipt);
            });
            setting_row(ui, "Make \"Reply all\" the default (Ctrl+R)", "", |ui| {
                widgets::toggle(ui, &mut c.default_reply_all);
            });
        });
        group(ui, "Undo send", |ui| {
            ui.label(RichText::new("Wait this long before sending, so you can cancel.").font(font(13.0)).color(pal().text2));
            ui.add(egui::Slider::new(&mut c.undo_send_s, 0..=15).suffix(" seconds"));
        });
        ui.label(RichText::new("Signatures are set per account under Accounts → Manage.").font(font(12.5)).color(pal().text3));
    }

    fn settings_rules(&mut self, ui: &mut Ui, st: &mut SettingsState) {
        let p = pal();
        if let Some(rule) = st.rule_edit.as_mut() {
            let mut done: Option<bool> = None;
            group(ui, "Rule", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Name").color(p.text2));
                    widgets::text_input(ui, &mut rule.name, "Rule name", 360.0);
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Apply to").color(p.text2));
                    let label = if rule.account_id.is_empty() { "All accounts".to_string() } else { self.account_name(&rule.account_id) };
                    egui::ComboBox::from_id_salt("rule-acc").selected_text(label).show_ui(ui, |ui| {
                        ui.selectable_value(&mut rule.account_id, String::new(), "All accounts");
                        for a in &self.cfg.accounts {
                            ui.selectable_value(&mut rule.account_id, a.id.clone(), a.name());
                        }
                    });
                    ui.checkbox(&mut rule.enabled, "Enabled");
                });
            });
            group(ui, "When a new message arrives in the Inbox and", |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut rule.match_all, true, "all conditions match");
                    ui.radio_value(&mut rule.match_all, false, "any condition matches");
                });
                let mut remove = None;
                for (i, c) in rule.conditions.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("cf", i)).selected_text(c.field.label()).width(140.0).show_ui(ui, |ui| {
                            for f in RuleField::ALL {
                                ui.selectable_value(&mut c.field, f, f.label());
                            }
                        });
                        if c.field != RuleField::HasAttachment {
                            egui::ComboBox::from_id_salt(("co", i)).selected_text(c.op.label()).width(140.0).show_ui(ui, |ui| {
                                for o in RuleOp::ALL {
                                    ui.selectable_value(&mut c.op, o, o.label());
                                }
                            });
                            widgets::text_input(ui, &mut c.value, "value", 220.0);
                        }
                        if widgets::icon_button(ui, icons::TRASH, "Remove condition").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    rule.conditions.remove(i);
                }
                if widgets::subtle_button(ui, icons::PLUS, "Add condition").clicked() {
                    rule.conditions.push(RuleCondition::default());
                }
            });
            group(ui, "Do the following", |ui| {
                let folders: Vec<(String, String)> = self
                    .mail
                    .folders
                    .iter()
                    .filter(|(acc, _)| rule.account_id.is_empty() || **acc == rule.account_id)
                    .flat_map(|(_, v)| v.iter().filter(|f| f.selectable).map(|f| (f.path.clone(), f.label())))
                    .collect();
                let mut remove = None;
                for (i, a) in rule.actions.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let kind = match a {
                            RuleAction::MoveTo(_) => "Move to folder",
                            RuleAction::MarkRead => "Mark as read",
                            RuleAction::Flag => "Flag",
                            RuleAction::Pin => "Pin",
                            RuleAction::Categorize(_) => "Categorize",
                            RuleAction::Delete => "Delete",
                            RuleAction::MoveToOther => "Move to Other",
                        };
                        egui::ComboBox::from_id_salt(("ra", i)).selected_text(kind).width(160.0).show_ui(ui, |ui| {
                            let options = [
                                ("Move to folder", RuleAction::MoveTo(folders.first().map(|f| f.0.clone()).unwrap_or_default())),
                                ("Mark as read", RuleAction::MarkRead),
                                ("Flag", RuleAction::Flag),
                                ("Pin", RuleAction::Pin),
                                ("Categorize", RuleAction::Categorize(self.cfg.categories.first().map(|c| c.name.clone()).unwrap_or_default())),
                                ("Delete", RuleAction::Delete),
                                ("Move to Other", RuleAction::MoveToOther),
                            ];
                            for (label, v) in options {
                                if ui.selectable_label(kind == label, label).clicked() {
                                    *a = v;
                                }
                            }
                        });
                        match a {
                            RuleAction::MoveTo(path) => {
                                let label = folders.iter().find(|f| f.0 == *path).map(|f| f.1.clone()).unwrap_or_else(|| util::decode_imap_utf7(path));
                                egui::ComboBox::from_id_salt(("rf", i)).selected_text(label).width(220.0).show_ui(ui, |ui| {
                                    for (fp, fl) in &folders {
                                        ui.selectable_value(path, fp.clone(), fl);
                                    }
                                });
                            }
                            RuleAction::Categorize(cat) => {
                                egui::ComboBox::from_id_salt(("rc", i)).selected_text(cat.clone()).width(180.0).show_ui(ui, |ui| {
                                    for c in &self.cfg.categories {
                                        ui.selectable_value(cat, c.name.clone(), &c.name);
                                    }
                                });
                            }
                            _ => {}
                        }
                        if widgets::icon_button(ui, icons::TRASH, "Remove action").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    rule.actions.remove(i);
                }
                if widgets::subtle_button(ui, icons::PLUS, "Add action").clicked() {
                    rule.actions.push(RuleAction::MarkRead);
                }
                ui.checkbox(&mut rule.stop_processing, "Stop processing more rules");
            });
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, Some(icons::CHECK), "Save rule").clicked() {
                    done = Some(true);
                }
                if widgets::secondary_button(ui, None, "Cancel").clicked() {
                    done = Some(false);
                }
            });
            if let Some(save) = done {
                let rule = st.rule_edit.take().unwrap();
                if save {
                    match self.cfg.rules.iter_mut().find(|r| r.id == rule.id) {
                        Some(slot) => *slot = rule,
                        None => self.cfg.rules.push(rule),
                    }
                }
            }
            return;
        }
        ui.label(RichText::new("Rules run on new messages as they arrive in your Inbox.").color(p.text2));
        ui.add_space(10.0);
        if self.cfg.rules.is_empty() {
            ui.label(RichText::new("You haven't created any rules yet.").color(p.text3));
            ui.add_space(8.0);
        }
        let mut remove = None;
        let mut edit = None;
        for (i, r) in self.cfg.rules.iter_mut().enumerate() {
            Frame::NONE
                .fill(p.card_alt)
                .stroke(Stroke::new(1.0, mix(p.stroke, p.card, 0.3)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::toggle(ui, &mut r.enabled);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&r.name).font(font_semibold(14.0)));
                            let conds: Vec<String> = r
                                .conditions
                                .iter()
                                .map(|c| {
                                    if c.field == RuleField::HasAttachment {
                                        "has attachment".to_string()
                                    } else {
                                        format!("{} {} \"{}\"", c.field.label().to_lowercase(), c.op.label(), c.value)
                                    }
                                })
                                .collect();
                            let acts: Vec<String> = r.actions.iter().map(|a| a.label()).collect();
                            ui.label(
                                RichText::new(format!(
                                    "If {} → {}",
                                    conds.join(if r.match_all { " and " } else { " or " }),
                                    acts.join(", ")
                                ))
                                .font(font(12.5))
                                .color(p.text3),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::TRASH, "Delete rule").clicked() {
                                remove = Some(i);
                            }
                            if widgets::icon_button(ui, icons::PENCIL_SIMPLE, "Edit rule").clicked() {
                                edit = Some(i);
                            }
                        });
                    });
                });
            ui.add_space(8.0);
        }
        if let Some(i) = remove {
            self.cfg.rules.remove(i);
        }
        if let Some(i) = edit {
            st.rule_edit = Some(self.cfg.rules[i].clone());
        }
        if widgets::primary_button(ui, Some(icons::PLUS), "Add new rule").clicked() {
            st.rule_edit = Some(Rule::default());
        }
    }

    fn settings_categories(&mut self, ui: &mut Ui, st: &mut SettingsState) {
        let p = pal();
        ui.label(RichText::new("Categories color-code messages and events.").color(p.text2));
        ui.add_space(10.0);
        let mut remove = None;
        for (i, c) in self.cfg.categories.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let color = theme::category_color(&c.color);
                let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                ui.painter().circle_filled(r.center(), 11.0, color);
                egui::Popup::menu(&resp).show(|ui| {
                    ui.horizontal(|ui| {
                        for name in theme::CATEGORY_COLORS {
                            let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                            ui.painter().circle_filled(r.center(), 10.0, theme::category_color(name));
                            if resp.on_hover_text(name).clicked() {
                                c.color = name.to_string();
                                ui.close();
                            }
                        }
                    });
                });
                widgets::text_input(ui, &mut c.name, "Category name", 260.0);
                if widgets::icon_button(ui, icons::TRASH, "Delete category").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.cfg.categories.remove(i);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            widgets::text_input(ui, &mut st.new_category, "New category", 260.0);
            if widgets::secondary_button(ui, Some(icons::PLUS), "Add").clicked() && !st.new_category.trim().is_empty() {
                let color = theme::CATEGORY_COLORS[self.cfg.categories.len() % theme::CATEGORY_COLORS.len()].to_string();
                self.cfg.categories.push(CategoryDef { name: st.new_category.trim().to_string(), color });
                st.new_category.clear();
            }
        });
    }

    fn settings_privacy(&mut self, ui: &mut Ui, st: &mut SettingsState) {
        let p = pal();
        group(ui, "External images", |ui| {
            ui.label(RichText::new("Remote images can tell senders when you open a message.").font(font(12.5)).color(p.text3));
            for pol in ImagePolicy::ALL {
                ui.radio_value(&mut self.cfg.mail.images, pol, pol.label());
            }
        });
        let list_editor = |ui: &mut Ui, list: &mut Vec<String>, new: &mut String, hint: &str| {
            let mut remove = None;
            for (i, s) in list.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(s);
                    if widgets::icon_button_ex(ui, icons::X, 22.0, 12.0, "Remove", false, None).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            ui.horizontal(|ui| {
                widgets::text_input(ui, new, hint, 300.0);
                if widgets::secondary_button(ui, Some(icons::PLUS), "Add").clicked() && !new.trim().is_empty() {
                    list.push(new.trim().to_lowercase());
                    new.clear();
                }
            });
        };
        group(ui, "Safe senders (images always shown, never junk)", |ui| {
            list_editor(ui, &mut self.cfg.mail.safe_senders, &mut st.new_safe, "name@example.com");
        });
        group(ui, "Blocked senders (marked as read automatically)", |ui| {
            list_editor(ui, &mut self.cfg.mail.blocked_senders, &mut st.new_blocked, "spam@example.com");
        });
        group(ui, "Contacts", |ui| {
            setting_row(ui, "Remember people you write to", "Builds the suggestions list used when you type a recipient", |ui| {
                widgets::toggle(ui, &mut self.cfg.mail.collect_contacts);
            });
        });
    }

    fn settings_calendar(&mut self, ui: &mut Ui) {
        let c = &mut self.cfg.calendar;
        group(ui, "Week", |ui| {
            setting_row(ui, "First day of the week", "", |ui| {
                ui.radio_value(&mut c.first_day_monday, true, "Monday");
                ui.radio_value(&mut c.first_day_monday, false, "Sunday");
            });
            setting_row(ui, "Work hours", "Highlighted in day and week views", |ui| {
                ui.add(DragValue::new(&mut c.work_end).range(1..=24).suffix(":00"));
                ui.label("to");
                ui.add(DragValue::new(&mut c.work_start).range(0..=23).suffix(":00"));
            });
        });
        group(ui, "New events", |ui| {
            setting_row(ui, "Default reminder", "", |ui| {
                ui.add(DragValue::new(&mut c.default_reminder_min).range(0..=10080).suffix(" min before"));
            });
            setting_row(ui, "Default duration", "", |ui| {
                ui.add(DragValue::new(&mut c.default_duration_min).range(5..=1440).speed(5).suffix(" min"));
            });
        });
    }

    fn settings_notifications(&mut self, ui: &mut Ui) {
        let n = &mut self.cfg.notifications;
        let mut test = false;
        group(ui, "Desktop notifications", |ui| {
            setting_row(ui, "Notify me about new messages", "", |ui| {
                widgets::toggle(ui, &mut n.enabled);
            });
            setting_row(ui, "Show a preview of the message", "", |ui| {
                widgets::toggle(ui, &mut n.show_preview);
            });
            setting_row(ui, "Calendar and task reminders", "", |ui| {
                widgets::toggle(ui, &mut n.calendar_reminders);
            });
            ui.add_space(6.0);
            if widgets::secondary_button(ui, Some(icons::BELL_RINGING), "Send a test notification").clicked() {
                test = true;
            }
        });
        if test {
            crate::notify::desktop("RustMail".into(), "Notifications are working.".into());
        }
    }

    fn settings_about(&mut self, ui: &mut Ui) {
        let p = pal();
        let paths = Paths::get();
        group(ui, "RustMail", |ui| {
            ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).font(font_semibold(14.0)));
            ui.label(RichText::new("A fast, private desktop mail client written in Rust.").color(p.text2));
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Settings: {}", paths.config_file().display())).font(font(12.5)).color(p.text3));
            ui.label(RichText::new(format!("Mail data: {}", paths.db_file().display())).font(font(12.5)).color(p.text3));
            ui.label(RichText::new(format!("Passwords: {}", secrets::backend_name())).font(font(12.5)).color(p.text3));
            ui.label(RichText::new(format!("Interface font: {}", self.font_family)).font(font(12.5)).color(p.text3));
        });
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, Some(icons::FOLDER), "Open data folder").clicked() {
                let _ = open::that(&paths.data_dir);
            }
            if widgets::secondary_button(ui, Some(icons::DATABASE), "Optimize search index").clicked() {
                let _ = self.store.rebuild_fts();
                self.toast("Search index optimized", ToastKind::Success);
            }
        });
    }
}
