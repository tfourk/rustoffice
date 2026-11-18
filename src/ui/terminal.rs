//! Window chrome for the Terminal theme: a line of account and mailbox
//! facts under the title bar, a bar of clickable shortcut keys, a status
//! line at the bottom and box lines around the window, all drawn as text
//! on a terminal screen.
//!
//! The look was inspired by BS-Mail by IronWolve
//! (<https://github.com/IronWolve/BS-Mail>), a fast, keyboard-first mail
//! client that runs in the terminal. None of BS-Mail's code or files are
//! used.

use std::sync::OnceLock;

use egui::{vec2, Align, Align2, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui};

use crate::app::{App, Dialog, Module, ToastKind};
use crate::config::AccountKind;
use crate::engine::Command;
use crate::icons;
use crate::model::*;
use crate::theme::{font, font_semibold, icon_font, pal};
use crate::ui::mail::MailView;
use crate::ui::settings::SettingsState;
use crate::ui::widgets;
use crate::util;

/// The project whose look inspired the Terminal theme.
pub const INSPIRED_BY_URL: &str = "https://github.com/IronWolve/BS-Mail";

/// The marks at both ends of the title and status lines.
pub const SLASHES: &str = "//////";

/// Ctrl as written on the key caps (⌘ on macOS, where Command stands in).
const CTRL: &str = if cfg!(target_os = "macos") { "⌘" } else { "^" };
/// Ctrl as written in tooltips.
const CTRL_NAME: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyAct {
    New,
    Reply,
    ReplyAll,
    Forward,
    Delete,
    Archive,
    Flag,
    MarkRead,
    Sync,
    Search,
    Settings,
    Help,
    Go(Module),
}

/// One entry of the key bar: the key cap, what it does and its shortcut.
struct KeyItem {
    cap: String,
    label: &'static str,
    tip: String,
    act: KeyAct,
    enabled: bool,
}

impl KeyItem {
    fn new(cap: impl Into<String>, label: &'static str, shortcut: impl Into<String>, act: KeyAct) -> Self {
        let shortcut = shortcut.into();
        KeyItem {
            cap: cap.into(),
            label,
            tip: format!("{label} ({shortcut})"),
            act,
            enabled: true,
        }
    }

    fn ctrl(key: &str, label: &'static str, act: KeyAct) -> Self {
        KeyItem::new(format!("{CTRL}{key}"), label, format!("{CTRL_NAME}+{key}"), act)
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The computer's name, read once.
fn host_name() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "localhost".into())
    })
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

/// A faint vertical bar between segments of a line.
pub fn separator(ui: &mut Ui) {
    let p = pal();
    ui.add_space(10.0);
    ui.label(RichText::new("│").font(font(14.0)).color(p.rule));
    ui.add_space(10.0);
}

fn text(ui: &mut Ui, s: impl Into<String>, color: egui::Color32) {
    ui.label(RichText::new(s.into()).font(font(13.5)).color(color));
}

fn strong(ui: &mut Ui, s: impl Into<String>, color: egui::Color32) {
    ui.label(RichText::new(s.into()).font(font_semibold(13.5)).color(color));
}

fn icon(ui: &mut Ui, ic: &str, color: egui::Color32) {
    ui.label(RichText::new(ic).font(icon_font(14.0)).color(color));
    ui.add_space(6.0);
}

impl App {
    /// Credit for the Terminal theme's look: a link to the project that
    /// inspired it and a note that RustMail isn't affiliated with it, kept
    /// together wherever the credit is shown (the About dialog and Settings
    /// → About).
    pub fn term_credit_link(&mut self, ui: &mut Ui) {
        let p = pal();
        let link = widgets::link(ui, "Terminal theme inspired by BS-Mail by IronWolve").on_hover_text(INSPIRED_BY_URL);
        if link.clicked() {
            if let Err(e) = open::that(INSPIRED_BY_URL) {
                self.toast(format!("Couldn't open the browser: {e}"), ToastKind::Error);
            }
        }
        ui.label(
            RichText::new("RustMail uses none of BS-Mail's code or files, and is not affiliated with or endorsed by BS-Mail or IronWolve.")
                .font(font(12.0))
                .color(p.text3),
        );
    }

    /// Draws the Terminal theme's box lines: around the window and between
    /// the bars at its top and bottom.
    pub fn term_frame(&self, ui: &Ui, rules: &[f32]) {
        let p = pal();
        let line = Stroke::new(1.0, p.stroke_strong);
        let rect = ui.ctx().content_rect().shrink(0.5);
        let painter = ui.painter();
        for &y in rules {
            painter.hline(rect.x_range(), y.round() + 0.5, line);
        }
        painter.rect_stroke(rect, CornerRadius::ZERO, line, egui::StrokeKind::Inside);
    }

    /// The title line: slashes, the app name and the module.
    pub fn term_title(&self, ui: &mut Ui) {
        let p = pal();
        ui.spacing_mut().item_spacing.x = 0.0;
        strong(ui, SLASHES, p.stroke_strong);
        ui.add_space(12.0);
        ui.label(RichText::new(icons::ENVELOPE_SIMPLE).font(icon_font(15.0)).color(p.success));
        ui.add_space(8.0);
        ui.label(RichText::new("RustMail").font(font_semibold(15.5)).color(p.success));
        separator(ui);
        let module = match self.module {
            Module::Mail => "Mail",
            Module::Calendar => "Calendar",
            Module::People => "People",
            Module::Todo => "To Do",
        };
        text(ui, module, p.text);
    }

    /// The version and closing slashes at the right end of the title line,
    /// when there is room for them.
    pub fn term_title_version(&self, ui: &mut Ui) {
        let p = pal();
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let need = ui.painter().layout_no_wrap(format!("{version}  {SLASHES}"), font_semibold(13.5), p.text).size().x + 24.0;
        if ui.available_width() < need {
            return;
        }
        ui.add_space(14.0);
        strong(ui, SLASHES, p.stroke_strong);
        ui.add_space(12.0);
        strong(ui, version, p.success);
    }

    /// Name, unread and total count of the folder shown in Mail.
    pub fn term_mailbox(&self) -> (String, i64, i64) {
        match self.mail.view {
            MailView::AllInboxes => {
                let inboxes = self.mail.folders.values().flatten().filter(|f| f.role == FolderRole::Inbox);
                let (unread, total) = inboxes.fold((0, 0), |(u, t), f| (u + f.unread, t + f.total));
                ("All inboxes".into(), unread, total)
            }
            MailView::Folder(id) => self
                .mail
                .find_folder(id)
                .map(|f| (f.label(), f.unread, f.total))
                .unwrap_or_else(|| ("—".into(), 0, 0)),
        }
    }

    /// The line under the title: account, mailbox, computer and connection.
    pub fn term_info_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        let account = self.cfg.default_account().cloned();
        let (mailbox, unread, total) = self.term_mailbox();
        let connection = account.as_ref().map(|a| {
            if a.kind == AccountKind::Local {
                return ("Local", "On this computer", p.success, None);
            }
            match self.statuses.get(&a.id) {
                Some(SyncStatus::Error(e)) => ("IMAP", "Error", p.danger, Some(e.clone())),
                Some(SyncStatus::Syncing(_)) | Some(SyncStatus::Connecting) => ("IMAP", "Syncing", p.yellow, None),
                _ => ("IMAP", "Online", p.success, None),
            }
        });
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(12.0);
            icon(ui, icons::USER, p.text2);
            text(ui, "Account: ", p.text);
            match &account {
                Some(a) => strong(ui, util::truncate_chars(&a.email, 40), p.cyan),
                None => text(ui, "none", p.text3),
            }
            separator(ui);
            icon(ui, icons::TRAY, p.text2);
            text(ui, "Mailbox: ", p.text);
            text(ui, format!("{} (", util::truncate_chars(&mailbox, 24)), p.text);
            strong(ui, format!("{unread} unread"), if unread > 0 { p.yellow } else { p.text3 });
            text(ui, format!(" / {total} total)"), p.text);
            // The computer's name is the first thing to give way.
            let host = host_name();
            let host_w = ui.painter().layout_no_wrap(format!("Host: {host} ({})", os_name()), font(13.5), p.text).size().x;
            if ui.available_width() > host_w + 260.0 {
                separator(ui);
                icon(ui, icons::DESKTOP, p.text2);
                text(ui, "Host: ", p.text);
                strong(ui, host, p.cyan);
                text(ui, format!(" ({})", os_name()), p.text);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(14.0);
                if let Some((proto, state, color, error)) = connection {
                    let r = ui.label(RichText::new(format!("{proto}: {state}")).font(font_semibold(13.5)).color(color));
                    if let Some(e) = error {
                        r.on_hover_text(e);
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("●").font(font(13.5)).color(color));
                    ui.add_space(4.0);
                }
            });
        });
    }

    fn term_keys(&self) -> Vec<KeyItem> {
        let has_sel = !self.mail.selection.is_empty();
        let mut keys = Vec::new();
        match self.module {
            Module::Mail => {
                keys.push(KeyItem::ctrl("N", "New", KeyAct::New));
                keys.push(KeyItem::ctrl("R", "Reply", KeyAct::Reply).enabled(has_sel));
                keys.push(KeyItem::new(format!("{CTRL}⇧R"), "Reply all", format!("{CTRL_NAME}+Shift+R"), KeyAct::ReplyAll).enabled(has_sel));
                keys.push(KeyItem::ctrl("F", "Forward", KeyAct::Forward).enabled(has_sel));
                keys.push(KeyItem::new("Del", "Delete", "Delete", KeyAct::Delete).enabled(has_sel));
                keys.push(KeyItem::new("E", "Archive", "E or Backspace", KeyAct::Archive).enabled(has_sel));
                keys.push(KeyItem::new("Ins", "Flag", "Insert", KeyAct::Flag).enabled(has_sel));
                keys.push(KeyItem::ctrl("Q", "Read", KeyAct::MarkRead).enabled(has_sel));
            }
            Module::Calendar => keys.push(KeyItem::ctrl("N", "New event", KeyAct::New)),
            Module::People => keys.push(KeyItem::ctrl("N", "New contact", KeyAct::New)),
            Module::Todo => keys.push(KeyItem::ctrl("N", "New task", KeyAct::New)),
        }
        keys.push(KeyItem::new("F9", "Sync", "F9", KeyAct::Sync));
        keys.push(KeyItem::ctrl("E", "Search", KeyAct::Search));
        if self.module != Module::Mail {
            for (n, label, module) in [
                ("1", "Mail", Module::Mail),
                ("2", "Calendar", Module::Calendar),
                ("3", "People", Module::People),
                ("4", "To Do", Module::Todo),
            ] {
                if module != self.module {
                    keys.push(KeyItem::ctrl(n, label, KeyAct::Go(module)));
                }
            }
        }
        keys.push(KeyItem::ctrl(",", "Settings", KeyAct::Settings));
        keys
    }

    fn run_key(&mut self, act: KeyAct) {
        let sel = self.mail.selection.clone();
        let first = sel.first().copied();
        match act {
            KeyAct::New => match self.module {
                Module::Mail => self.new_compose(),
                Module::Calendar => self.cal_new_event(None),
                Module::People => self.people_new_contact(),
                Module::Todo => self.todo.focus_new = true,
            },
            KeyAct::Reply => {
                if let Some(id) = first {
                    self.reply(id, false);
                }
            }
            KeyAct::ReplyAll => {
                if let Some(id) = first {
                    self.reply(id, true);
                }
            }
            KeyAct::Forward => {
                if let Some(id) = first {
                    self.forward(id);
                }
            }
            KeyAct::Delete if !sel.is_empty() => self.delete_messages(&sel, false),
            KeyAct::Archive if !sel.is_empty() => self.archive_messages(&sel),
            KeyAct::Flag if !sel.is_empty() => self.toggle_flag(&sel),
            KeyAct::MarkRead if !sel.is_empty() => self.set_seen(&sel, true),
            KeyAct::Delete | KeyAct::Archive | KeyAct::Flag | KeyAct::MarkRead => {}
            KeyAct::Sync => {
                self.engine.send(Command::SyncAll { full: true });
                self.engine.send(Command::ProcessOutbox);
                self.toast("Checking all folders for new mail…", ToastKind::Info);
            }
            KeyAct::Search => self.focus_search = true,
            KeyAct::Settings => self.settings = Some(SettingsState::default()),
            KeyAct::Help => self.dialog = Some(Dialog::Shortcuts),
            KeyAct::Go(module) => {
                self.module = module;
                self.ribbon_tab = 0;
            }
        }
    }

    /// The bar of shortcut keys above the status line. Each key also works
    /// as a button. Keys that don't fit are left out; Help stays at the end.
    pub fn term_key_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        let rect = ui.max_rect();
        let y = rect.center().y;
        let keys = self.term_keys();
        let help = KeyItem::new("F1", "Help", "F1", KeyAct::Help);
        let help_w = key_width(ui, &help);
        let limit = rect.right() - 12.0 - help_w - 24.0;
        let mut clicked = None;
        let mut x = rect.left() + 10.0;
        for key in &keys {
            let w = key_width(ui, key);
            if x + w > limit {
                break;
            }
            if paint_key(ui, Pos2::new(x, y), key) {
                clicked = Some(key.act);
            }
            x += w;
            ui.painter().text(Pos2::new(x + 12.0, y), Align2::CENTER_CENTER, "│", font(14.0), p.rule);
            x += 24.0;
        }
        if paint_key(ui, Pos2::new(rect.right() - 12.0 - help_w, y), &help) {
            clicked = Some(KeyAct::Help);
        }
        if let Some(act) = clicked {
            self.run_key(act);
        }
    }

    /// The status line at the bottom of the window.
    pub fn term_status_line(&mut self, ui: &mut Ui) {
        let p = pal();
        let sync = self.sync_summary();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(12.0);
            strong(ui, SLASHES, p.stroke_strong);
            ui.add_space(12.0);
            text(ui, format!("RustMail v{}", env!("CARGO_PKG_VERSION")), p.text);
            separator(ui);
            let (mark, color) = if sync.busy {
                (icons::ARROWS_CLOCKWISE, p.yellow)
            } else if sync.error {
                ("✗", p.danger)
            } else {
                ("✓", p.success)
            };
            let state = if sync.busy || sync.error || self.cfg.accounts.is_empty() { sync.text.clone() } else { "Up to date".to_string() };
            ui.label(RichText::new(mark).font(font_semibold(13.5)).color(color));
            ui.add_space(8.0);
            let state = ui.label(RichText::new(util::truncate_chars(&state, 70)).font(font(13.5)).color(if sync.error { p.danger } else { p.text }));
            if sync.error {
                state.on_hover_text(&sync.text);
            }
            if self.module == Module::Mail {
                let unread = self.mail.messages.iter().filter(|m| !m.seen).count();
                separator(ui);
                strong(ui, format!("{unread} unread"), if unread > 0 { p.yellow } else { p.text3 });
                separator(ui);
                text(ui, format!("{} shown", self.mail.messages.len()), p.text);
            }
            if sync.last > 0 && !sync.busy {
                separator(ui);
                text(ui, format!("Last sync: {}", util::fmt_time(&util::local(sync.last))), p.text);
            }
            let outbox = self.store.outbox().map(|o| o.len()).unwrap_or(0);
            if outbox > 0 {
                separator(ui);
                strong(ui, format!("Outbox: {outbox}"), p.yellow);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                strong(ui, SLASHES, p.stroke_strong);
                ui.add_space(12.0);
                text(ui, "for help", p.text);
                ui.add_space(8.0);
                strong(ui, "F1", p.cyan);
                ui.add_space(8.0);
                text(ui, "Press", p.text);
                separator(ui);
                text(ui, format!("{:.0}%", self.cfg.appearance.zoom * 100.0), p.text3);
            });
        });
    }

    /// The Status box at the bottom of the folder pane.
    pub fn term_status_box(&mut self, ui: &mut Ui) {
        let p = pal();
        let sync = self.sync_summary();
        let unread: i64 = self
            .mail
            .folders
            .values()
            .flatten()
            .filter(|f| f.role == FolderRole::Inbox)
            .map(|f| f.unread)
            .sum();
        let outbox = self.store.outbox().map(|o| o.len()).unwrap_or(0);
        let db = std::fs::metadata(crate::config::Paths::get().db_file()).map(|m| m.len()).unwrap_or(0);
        term_heading(ui, icons::CHART_BAR, "Status", |_| {});
        ui.spacing_mut().item_spacing.y = 3.0;
        let row = |ui: &mut Ui, label: &str, value: String, color: egui::Color32| {
            let mut job = egui::text::LayoutJob::default();
            let fmt = |color| egui::TextFormat { font_id: font(13.5), color, ..Default::default() };
            job.append(&format!("{label:<10}"), 6.0, fmt(p.text));
            job.append(&value, 0.0, fmt(color));
            ui.label(job);
        };
        let last = if sync.last > 0 { util::fmt_time(&util::local(sync.last)) } else { "—".into() };
        row(ui, "Last sync", last, p.text);
        row(ui, "Unread", unread.to_string(), if unread > 0 { p.yellow } else { p.text3 });
        let (state, color) = if sync.busy {
            ("Syncing", p.yellow)
        } else if sync.error {
            ("Error", p.danger)
        } else {
            ("Idle", p.text)
        };
        row(ui, "Sync", state.to_string(), color);
        if outbox > 0 {
            row(ui, "Outbox", outbox.to_string(), p.yellow);
        }
        row(ui, "Database", util::human_size(db as usize), p.text);
        ui.add_space(4.0);
    }
}

/// A pane heading in the Terminal theme: green text over a box line, with
/// room for controls at its right end.
pub fn term_heading(ui: &mut Ui, ic: &str, title: &str, add_right: impl FnOnce(&mut Ui)) {
    let p = pal();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.set_min_height(26.0);
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(6.0);
        ui.label(RichText::new(ic).font(icon_font(14.0)).color(p.success));
        ui.add_space(10.0);
        ui.label(RichText::new(title).font(font_semibold(14.5)).color(p.accent_text));
        ui.with_layout(Layout::right_to_left(Align::Center), add_right);
    });
    ui.add_space(2.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, p.stroke));
}

fn key_galleys(ui: &Ui, key: &KeyItem) -> (std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>) {
    let p = pal();
    let cap = ui.painter().layout_no_wrap(key.cap.clone(), font_semibold(13.0), p.bg);
    let label = ui.painter().layout_no_wrap(key.label.to_string(), font(13.5), p.text);
    (cap, label)
}

fn key_width(ui: &Ui, key: &KeyItem) -> f32 {
    let (cap, label) = key_galleys(ui, key);
    cap.size().x + 14.0 + 8.0 + label.size().x
}

/// Paints one key (a reverse-video cap and its label) with its left edge
/// at `left`; returns whether it was clicked.
fn paint_key(ui: &mut Ui, left: Pos2, key: &KeyItem) -> bool {
    let p = pal();
    let (cap, label) = key_galleys(ui, key);
    let cap_rect = Rect::from_min_size(Pos2::new(left.x, left.y - 10.0), vec2(cap.size().x + 14.0, 20.0));
    let rect = Rect::from_min_max(cap_rect.min, Pos2::new(cap_rect.right() + 8.0 + label.size().x, cap_rect.bottom()));
    let resp = ui.interact(rect.expand2(vec2(4.0, 2.0)), Id::new(("term-key", &key.cap)), if key.enabled { Sense::click() } else { Sense::hover() });
    let hovered = key.enabled && resp.hovered();
    if hovered {
        ui.painter().rect_filled(rect.expand2(vec2(4.0, 2.0)), CornerRadius::ZERO, p.hover);
    }
    let cap_bg = if !key.enabled {
        p.disabled
    } else if hovered {
        p.cyan
    } else {
        p.stroke_strong
    };
    ui.painter().rect_filled(cap_rect, CornerRadius::ZERO, cap_bg);
    ui.painter().galley(cap_rect.center() - cap.size() / 2.0, cap, p.bg);
    let label_color = if !key.enabled {
        p.text3
    } else if hovered {
        p.cyan
    } else {
        p.text
    };
    ui.painter().galley(Pos2::new(cap_rect.right() + 8.0, left.y - label.size().y / 2.0), label, label_color);
    let resp = resp.on_hover_text(&key.tip);
    if key.enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
    } else {
        false
    }
}
