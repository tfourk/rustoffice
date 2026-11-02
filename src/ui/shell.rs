//! Window chrome: title bar, app bar, ribbon, status bar, toasts, dialogs.

use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, Modal,
    Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder, ViewportCommand,
};

use crate::app::{App, Dialog, Module, ToastAction, ToastKind};
use crate::config::ReadingPanePos;
use crate::engine::Command;
use crate::icons;
use crate::model::*;
use crate::theme::{self, font, font_semibold, icon_fill_font, icon_font, mix, pal, Density, ThemeMode};
use crate::ui::settings::SettingsState;
use crate::ui::widgets::{self, ribbon_button, ribbon_button_ex, ribbon_menu, ribbon_separator};
use crate::util;

impl App {
    pub fn draw(&mut self, ui: &mut Ui) {
        let p = pal();
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(ui.max_rect(), 0.0, p.bg);
        if self.cfg.appearance.custom_titlebar && !self.maximized {
            self.window_resize_borders(&ctx);
        }

        egui::Panel::top("titlebar")
            .exact_size(48.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.bg))
            .show(ui, |ui| self.titlebar(ui));
        egui::Panel::left("appbar")
            .exact_size(56.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.bg))
            .show(ui, |ui| self.app_bar(ui));
        egui::Panel::bottom("statusbar")
            .exact_size(26.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.bg).inner_margin(Margin::symmetric(8, 0)))
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::top("ribbon")
            .exact_size(88.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(p.bg).inner_margin(Margin {
                left: 0,
                right: 8,
                top: 0,
                bottom: 8,
            }))
            .show(ui, |ui| self.ribbon(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| match self.module {
            Module::Mail => self.mail_ui(ui),
            Module::Calendar => self.calendar_ui(ui),
            Module::People => self.people_ui(ui),
            Module::Todo => self.todo_ui(ui),
        });

        self.compose_windows(&ctx);
        self.dialogs(&ctx);
        if self.settings.is_some() {
            self.settings_ui(&ctx);
        }
        if self.wizard.is_some() {
            self.wizard_ui(&ctx);
        }
        self.toasts_ui(&ctx);
        self.drag_preview(&ctx);
    }

    fn window_resize_borders(&mut self, ctx: &egui::Context) {
        use egui::viewport::ResizeDirection as R;
        let rect = ctx.content_rect();
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let m = 5.0;
        let (l, r, t, b) = (
            pos.x - rect.left() < m,
            rect.right() - pos.x < m,
            pos.y - rect.top() < m,
            rect.bottom() - pos.y < m,
        );
        let dir = match (l, r, t, b) {
            (true, _, true, _) => Some((R::NorthWest, CursorIcon::ResizeNorthWest)),
            (_, true, true, _) => Some((R::NorthEast, CursorIcon::ResizeNorthEast)),
            (true, _, _, true) => Some((R::SouthWest, CursorIcon::ResizeSouthWest)),
            (_, true, _, true) => Some((R::SouthEast, CursorIcon::ResizeSouthEast)),
            (true, _, _, _) => Some((R::West, CursorIcon::ResizeWest)),
            (_, true, _, _) => Some((R::East, CursorIcon::ResizeEast)),
            (_, _, true, _) => Some((R::North, CursorIcon::ResizeNorth)),
            (_, _, _, true) => Some((R::South, CursorIcon::ResizeSouth)),
            _ => None,
        };
        if let Some((d, cursor)) = dir {
            ctx.set_cursor_icon(cursor);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(d));
            }
        }
    }

    // --------------------------------------------------------------- title bar

    fn titlebar(&mut self, ui: &mut Ui) {
        let p = pal();
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let custom = self.cfg.appearance.custom_titlebar;
        if custom {
            let drag = ui.interact(rect, Id::new("titlebar-drag"), Sense::click_and_drag());
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
            }
        }

        // Left: logo + name.
        let left = Rect::from_min_size(rect.min + vec2(12.0, 0.0), vec2(220.0, rect.height()));
        ui.scope_builder(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)), |ui| {
            let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(6), p.accent);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::ENVELOPE_SIMPLE, icon_fill_font(17.0), Color32::WHITE);
            ui.add_space(6.0);
            ui.label(RichText::new("RustMail").font(font_semibold(15.0)).color(p.text));
        });

        // Center: search.
        let sw = (rect.width() - 2.0 * 300.0).clamp(240.0, 600.0);
        let search_rect = Rect::from_center_size(rect.center(), vec2(sw, 32.0));
        let search_id = Id::new("global-search");
        let before = self.search.clone();
        ui.scope_builder(UiBuilder::new().max_rect(search_rect), |ui| {
            let hint = match self.module {
                Module::Mail => "Search mail (from:, subject:, has:attachment, is:unread)",
                Module::Calendar => "Search events",
                Module::People => "Search people",
                Module::Todo => "Search tasks",
            };
            widgets::search_box(ui, &mut self.search, hint, sw, search_id);
        });
        if self.focus_search {
            ui.memory_mut(|m| m.request_focus(search_id));
            self.focus_search = false;
        }
        if before != self.search {
            self.mail.list_dirty = true;
            self.mail.limit = 300;
        }

        // Right: commands + window controls.
        let right = Rect::from_min_max(Pos2::new(search_rect.right() + 8.0, rect.top()), rect.right_bottom() - vec2(4.0, 0.0));
        ui.scope_builder(UiBuilder::new().max_rect(right).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if custom {
                self.window_controls(ui);
                ui.add_space(8.0);
            }
            // Account avatar with menu.
            let (name, email) = self
                .cfg
                .default_account()
                .map(|a| (a.display_name.clone(), a.email.clone()))
                .unwrap_or_default();
            let (r, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
            if resp.hovered() {
                ui.painter().circle_filled(r.center(), 17.0, p.hover);
            }
            widgets::paint_avatar(ui.painter(), r.center(), 14.0, if name.is_empty() { "?" } else { &name }, &email);
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Accounts");
            egui::Popup::menu(&resp).width(300.0).show(|ui| {
                ui.set_min_width(280.0);
                for a in self.cfg.accounts.clone() {
                    ui.horizontal(|ui| {
                        widgets::avatar(ui, 32.0, &a.display_name, &a.email);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new(a.name()).font(font_semibold(14.0)));
                            ui.label(RichText::new(&a.email).font(font(12.0)).color(p.text3));
                            let st = match self.statuses.get(&a.id) {
                                Some(SyncStatus::Error(e)) => format!("⚠ {}", util::truncate_chars(e, 40)),
                                Some(SyncStatus::Syncing(s)) => s.clone(),
                                Some(SyncStatus::Connecting) => "Connecting…".into(),
                                _ if !a.enabled => "Disabled".into(),
                                _ => "Connected".into(),
                            };
                            ui.label(RichText::new(st).font(font(11.5)).color(p.text3));
                        });
                    });
                    ui.add_space(4.0);
                }
                ui.separator();
                if widgets::menu_item(ui, icons::USER_PLUS, "Add account").clicked() {
                    self.wizard = Some(crate::ui::wizard::WizardState::new(false));
                }
                if widgets::menu_item(ui, icons::GEAR, "Manage accounts").clicked() {
                    self.settings = Some(SettingsState::accounts());
                }
            });
            ui.add_space(6.0);
            if widgets::icon_button_ex(ui, icons::GEAR_SIX, 34.0, 19.0, "Settings (Ctrl+,)", false, None).clicked() {
                self.settings = Some(SettingsState::default());
            }
            if widgets::icon_button_ex(ui, icons::QUESTION, 34.0, 19.0, "Help & keyboard shortcuts", false, None).clicked() {
                self.dialog = Some(Dialog::Shortcuts);
            }
            let syncing = self
                .statuses
                .values()
                .any(|s| matches!(s, SyncStatus::Syncing(_) | SyncStatus::Connecting));
            if syncing {
                let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                ui.put(Rect::from_center_size(r.center(), vec2(16.0, 16.0)), egui::Spinner::new().size(16.0).color(p.accent));
            } else if widgets::icon_button_ex(ui, icons::ARROWS_CLOCKWISE, 34.0, 18.0, "Send / Receive all folders (F9)", false, None).clicked() {
                self.engine.send(Command::SyncAll { full: true });
                self.engine.send(Command::ProcessOutbox);
            }
        });
    }

    fn window_controls(&mut self, ui: &mut Ui) {
        let p = pal();
        let ctx = ui.ctx().clone();
        let btn = |ui: &mut Ui, icon: &str, close: bool| -> egui::Response {
            let (r, resp) = ui.allocate_exact_size(vec2(44.0, 34.0), Sense::click());
            let hovered = resp.hovered();
            let bg = if close && hovered {
                theme::hex(0xC42B1C)
            } else if hovered {
                p.hover
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(r, CornerRadius::same(4), bg);
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                icon,
                icon_font(15.0),
                if close && hovered { Color32::WHITE } else { p.text2 },
            );
            resp
        };
        if btn(ui, icons::X, true).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        let max_icon = if self.maximized { icons::COPY } else { icons::SQUARE };
        if btn(ui, max_icon, false).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!self.maximized));
        }
        if btn(ui, icons::MINUS, false).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }

    // ----------------------------------------------------------------- app bar

    fn app_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        let unread = self.store.unread_inbox_total().unwrap_or(0);
        let items = [
            (Module::Mail, icons::ENVELOPE_SIMPLE, "Mail (Ctrl+1)"),
            (Module::Calendar, icons::CALENDAR_BLANK, "Calendar (Ctrl+2)"),
            (Module::People, icons::USERS, "People (Ctrl+3)"),
            (Module::Todo, icons::CHECK_SQUARE_OFFSET, "To Do (Ctrl+4)"),
        ];
        ui.add_space(4.0);
        for (module, icon, tip) in items {
            let selected = self.module == module;
            let (rect, resp) = ui.allocate_exact_size(vec2(56.0, 50.0), Sense::click());
            let inner = Rect::from_center_size(rect.center(), vec2(44.0, 44.0));
            if resp.hovered() || selected {
                ui.painter().rect_filled(inner, CornerRadius::same(6), if selected { p.card } else { p.hover });
            }
            if selected {
                let bar = Rect::from_center_size(Pos2::new(rect.left() + 3.0, rect.center().y), vec2(3.0, 20.0));
                ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
            }
            ui.painter().text(
                inner.center(),
                Align2::CENTER_CENTER,
                icon,
                if selected { icon_fill_font(22.0) } else { icon_font(22.0) },
                if selected { p.accent } else { p.text2 },
            );
            if module == Module::Mail && unread > 0 {
                let c = Pos2::new(inner.right() - 9.0, inner.top() + 10.0);
                let txt = if unread > 99 { "99+".to_string() } else { unread.to_string() };
                let g = ui.painter().layout_no_wrap(txt, font_semibold(10.0), Color32::WHITE);
                let w = (g.size().x + 8.0).max(16.0);
                let br = Rect::from_center_size(c, vec2(w, 16.0));
                ui.painter().rect_filled(br, CornerRadius::same(8), p.accent);
                ui.painter().galley(br.center() - g.size() / 2.0, g, Color32::WHITE);
            }
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
            if resp.clicked() {
                self.module = module;
                self.ribbon_tab = 0;
            }
        }
    }

    // ------------------------------------------------------------------ ribbon

    fn ribbon(&mut self, ui: &mut Ui) {
        let p = pal();
        let tabs: &[&str] = &["Home", "View", "Help"];
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            widgets::tabs(ui, tabs, &mut self.ribbon_tab, 14.0);
        });
        ui.add_space(2.0);
        let wide = ui.available_width() > 1560.0;
        let large = ui.available_width() > 1300.0;
        let medium = ui.available_width() > 1000.0;
        Frame::NONE
            .fill(p.card)
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0, if p.dark { p.stroke } else { mix(p.stroke, p.card, 0.4) }))
            .inner_margin(Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    match (self.module, self.ribbon_tab) {
                        (_, 1) => self.ribbon_view(ui, wide),
                        (_, 2) => self.ribbon_help(ui, wide),
                        (Module::Mail, _) => self.ribbon_mail(ui, wide, large, medium),
                        (Module::Calendar, _) => self.ribbon_calendar(ui, medium),
                        (Module::People, _) => self.ribbon_people(ui, medium),
                        (Module::Todo, _) => self.ribbon_todo(ui, medium),
                    }
                });
            });
    }

    fn new_item_button(&mut self, ui: &mut Ui, wide: bool) {
        let label = match self.module {
            Module::Mail => "New mail",
            Module::Calendar => "New event",
            Module::People => "New contact",
            Module::Todo => "New task",
        };
        let icon = match self.module {
            Module::Mail => icons::NOTE_PENCIL,
            Module::Calendar => icons::CALENDAR_PLUS,
            Module::People => icons::USER_PLUS,
            Module::Todo => icons::PLUS,
        };
        let r = if wide || true {
            widgets::primary_button(ui, Some(icon), label)
        } else {
            widgets::primary_button(ui, Some(icon), "")
        };
        if r.clicked() {
            match self.module {
                Module::Mail => self.new_compose(),
                Module::Calendar => self.cal_new_event(None),
                Module::People => self.people_new_contact(),
                Module::Todo => self.todo.focus_new = true,
            }
        }
        let caret = widgets::icon_button_ex(ui, icons::CARET_DOWN, 24.0, 13.0, "New item", false, None);
        egui::Popup::menu(&caret).show(|ui| {
            if widgets::menu_item(ui, icons::ENVELOPE_SIMPLE, "Mail").clicked() {
                self.module = Module::Mail;
                self.new_compose();
            }
            if widgets::menu_item(ui, icons::CALENDAR_BLANK, "Event").clicked() {
                self.module = Module::Calendar;
                self.cal_new_event(None);
            }
            if widgets::menu_item(ui, icons::USER, "Contact").clicked() {
                self.module = Module::People;
                self.people_new_contact();
            }
            if widgets::menu_item(ui, icons::CHECK_CIRCLE, "Task").clicked() {
                self.module = Module::Todo;
                self.todo.focus_new = true;
            }
        });
        ribbon_separator(ui);
    }

    fn ribbon_mail(&mut self, ui: &mut Ui, wide: bool, large: bool, medium: bool) {
        self.new_item_button(ui, wide);
        let sel = self.mail.selection.clone();
        let has = !sel.is_empty();
        let in_junk = self.mail.current_role() == Some(FolderRole::Junk);
        if ribbon_button(ui, icons::TRASH, "Delete", medium, has).clicked() {
            self.delete_messages(&sel, false);
        }
        if ribbon_button(ui, icons::ARCHIVE, "Archive", medium, has).clicked() {
            self.archive_messages(&sel);
        }
        ribbon_menu(ui, icons::WARNING_OCTAGON, "Report", wide, has, |ui| {
            if in_junk {
                if widgets::menu_item(ui, icons::CHECK_CIRCLE, "Not junk").clicked() {
                    self.junk_messages(&sel, true);
                }
            } else if widgets::menu_item(ui, icons::PROHIBIT, "Report junk").clicked() {
                self.junk_messages(&sel, false);
            }
            if widgets::menu_item(ui, icons::SHIELD_CHECK, "Block sender").clicked() {
                self.block_sender(&sel);
            }
        });
        if ribbon_button(ui, icons::BROOM, "Sweep", wide, has).on_hover_text("Delete all messages from this sender in this folder").clicked() {
            self.sweep(&sel);
        }
        let move_resp = ribbon_button_ex(ui, icons::FOLDER_SIMPLE, "Move to", wide, has, true, false);
        if has {
            egui::Popup::menu(&move_resp).width(260.0).show(|ui| {
                self.move_menu(ui, &sel);
            });
        }
        ribbon_separator(ui);
        if ribbon_button(ui, icons::ARROW_BEND_UP_LEFT, "Reply", medium, sel.len() == 1).clicked() {
            self.reply(sel[0], false);
        }
        if ribbon_button(ui, icons::ARROW_BEND_DOUBLE_UP_LEFT, "Reply all", medium, sel.len() == 1).clicked() {
            self.reply(sel[0], true);
        }
        if ribbon_button(ui, icons::ARROW_BEND_UP_RIGHT, "Forward", medium, sel.len() == 1).clicked() {
            self.forward(sel[0]);
        }
        ribbon_separator(ui);
        let msgs = self.store.messages_by_ids(&sel).unwrap_or_default();
        let all_read = !msgs.is_empty() && msgs.iter().all(|m| m.seen);
        if ribbon_button(
            ui,
            if all_read { icons::ENVELOPE_SIMPLE } else { icons::ENVELOPE_SIMPLE_OPEN },
            if all_read { "Unread" } else { "Read" },
            large,
            has,
        )
        .clicked()
        {
            self.set_seen(&sel, !all_read);
        }
        let flagged = !msgs.is_empty() && msgs.iter().all(|m| m.flagged);
        if ribbon_button_ex(ui, icons::FLAG, if flagged { "Unflag" } else { "Flag" }, large, has, false, flagged).clicked() {
            self.toggle_flag(&sel);
        }
        let pinned = !msgs.is_empty() && msgs.iter().all(|m| m.pinned);
        if ribbon_button_ex(ui, icons::PUSH_PIN, if pinned { "Unpin" } else { "Pin" }, large, has, false, pinned).clicked() {
            self.toggle_pin(&sel);
        }
        ribbon_menu(ui, icons::CLOCK, "Snooze", wide, has, |ui| self.snooze_menu(ui, &sel));
        ribbon_menu(ui, icons::TAG, "Categorize", wide, has, |ui| self.categorize_menu(ui, &sel));
        ribbon_separator(ui);
        if ribbon_button(ui, icons::ARROWS_CLOCKWISE, "Sync", wide, true).on_hover_text("Send / Receive (F9)").clicked() {
            self.engine.send(Command::SyncAll { full: true });
            self.engine.send(Command::ProcessOutbox);
        }
    }

    fn ribbon_view(&mut self, ui: &mut Ui, wide: bool) {
        if ribbon_button(ui, icons::GEAR_SIX, "View settings", true, true).clicked() {
            self.settings = Some(SettingsState::section(crate::ui::settings::Section::Layout));
        }
        ribbon_separator(ui);
        let mut changed = false;
        ribbon_menu(ui, icons::SQUARES_FOUR, "Layout", true, true, |ui| {
            for pos in ReadingPanePos::ALL {
                let sel = self.cfg.mail.reading_pane == pos;
                if widgets::menu_item(ui, if sel { icons::CHECK } else { "" }, pos.label()).clicked() {
                    self.cfg.mail.reading_pane = pos;
                    changed = true;
                }
            }
            ui.separator();
            let open = self.cfg.window.folder_pane_open;
            if widgets::menu_item(ui, icons::SIDEBAR_SIMPLE, if open { "Hide folder pane" } else { "Show folder pane" }).clicked() {
                self.cfg.window.folder_pane_open = !open;
                changed = true;
            }
        });
        ribbon_menu(ui, icons::ROWS, "Density", wide, true, |ui| {
            for d in Density::ALL {
                let sel = self.cfg.appearance.density == d;
                if widgets::menu_item(ui, if sel { icons::CHECK } else { "" }, d.label()).clicked() {
                    self.cfg.appearance.density = d;
                    changed = true;
                }
            }
        });
        let toggles: [(&str, &str, bool); 4] = [
            (icons::CHAT_CIRCLE, "Conversations", self.cfg.mail.conversation_view),
            (icons::TRAY, "Focused Inbox", self.cfg.mail.focused_inbox),
            (icons::TEXT_ALIGN_LEFT, "Preview text", self.cfg.mail.show_preview),
            (icons::USER_CIRCLE, "Sender images", self.cfg.mail.show_avatars),
        ];
        for (i, (icon, label, on)) in toggles.into_iter().enumerate() {
            if ribbon_button_ex(ui, icon, label, wide, true, false, on).clicked() {
                match i {
                    0 => self.cfg.mail.conversation_view = !on,
                    1 => self.cfg.mail.focused_inbox = !on,
                    2 => self.cfg.mail.show_preview = !on,
                    _ => self.cfg.mail.show_avatars = !on,
                }
                changed = true;
            }
        }
        ribbon_separator(ui);
        ribbon_menu(ui, icons::CIRCLE_HALF, "Theme", wide, true, |ui| {
            for t in ThemeMode::ALL {
                let sel = self.cfg.appearance.theme == t;
                if widgets::menu_item(ui, if sel { icons::CHECK } else { "" }, t.label()).clicked() {
                    self.cfg.appearance.theme = t;
                    changed = true;
                }
            }
        });
        if ribbon_button(ui, icons::MINUS, "Zoom out", false, true).clicked() {
            self.cfg.appearance.zoom = ((self.cfg.appearance.zoom - 0.1) * 10.0).round() / 10.0;
            changed = true;
        }
        let (r, _) = ui.allocate_exact_size(vec2(46.0, 32.0), Sense::hover());
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            format!("{:.0}%", self.cfg.appearance.zoom * 100.0),
            font(13.0),
            pal().text2,
        );
        if ribbon_button(ui, icons::PLUS, "Zoom in", false, true).clicked() {
            self.cfg.appearance.zoom = ((self.cfg.appearance.zoom + 0.1) * 10.0).round() / 10.0;
            changed = true;
        }
        if changed {
            self.cfg.appearance.zoom = self.cfg.appearance.zoom.clamp(0.7, 2.0);
            self.mail.list_dirty = true;
            self.mail.reading_flags_dirty = true;
            self.mark_config_dirty();
        }
    }

    fn ribbon_help(&mut self, ui: &mut Ui, _wide: bool) {
        if ribbon_button(ui, icons::KEYBOARD, "Keyboard shortcuts", true, true).clicked() {
            self.dialog = Some(Dialog::Shortcuts);
        }
        if ribbon_button(ui, icons::INFO, "About", true, true).clicked() {
            self.dialog = Some(Dialog::About);
        }
        ribbon_separator(ui);
        if ribbon_button(ui, icons::FOLDER, "Open data folder", true, true).clicked() {
            let _ = open::that(crate::config::Paths::get().data_dir);
        }
        if ribbon_button(ui, icons::DATABASE, "Optimize search index", true, true).clicked() {
            match self.store.rebuild_fts() {
                Ok(()) => self.toast("Search index optimized", ToastKind::Success),
                Err(e) => self.toast(format!("Could not optimize: {e}"), ToastKind::Error),
            };
        }
        if ribbon_button(ui, icons::USER_PLUS, "Add account", true, true).clicked() {
            self.wizard = Some(crate::ui::wizard::WizardState::new(false));
        }
    }

    // ------------------------------------------------------------- status bar

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut text = "All folders are up to date.".to_string();
            let mut icon = icons::CHECK_CIRCLE;
            let mut color = p.text3;
            let mut busy = false;
            let mut last = 0;
            for (id, s) in &self.statuses {
                match s {
                    SyncStatus::Syncing(msg) => {
                        text = format!("{} — {msg}", self.account_name(id));
                        busy = true;
                    }
                    SyncStatus::Connecting if !busy => {
                        text = format!("Connecting to {}…", self.account_name(id));
                        busy = true;
                    }
                    SyncStatus::Error(e) if !busy => {
                        text = format!("{}: {}", self.account_name(id), util::truncate_chars(e, 90));
                        icon = icons::WARNING;
                        color = p.danger;
                    }
                    SyncStatus::Idle { last: l } => last = last.max(*l),
                    _ => {}
                }
            }
            if self.cfg.accounts.is_empty() {
                text = "No accounts yet — add one to start syncing.".into();
                icon = icons::INFO;
            }
            if busy {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
            } else {
                ui.label(RichText::new(icon).font(icon_font(13.0)).color(color));
            }
            ui.label(RichText::new(text).font(font(12.0)).color(if color == p.danger { p.danger } else { p.text3 }));
            if !busy && last > 0 && color != p.danger {
                ui.label(
                    RichText::new(format!("· Updated {}", util::fmt_time(&util::local(last))))
                        .font(font(12.0))
                        .color(p.text3),
                );
            }
            let outbox = self.store.outbox().map(|o| o.len()).unwrap_or(0);
            if outbox > 0 {
                ui.label(RichText::new(format!("· Outbox: {outbox}")).font(font(12.0)).color(p.warning));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{:.0}%", self.cfg.appearance.zoom * 100.0))
                        .font(font(12.0))
                        .color(p.text3),
                );
                if self.module == Module::Mail {
                    let unread = self.mail.messages.iter().filter(|m| !m.seen).count();
                    ui.label(
                        RichText::new(format!("Items: {}   Unread: {}   ", self.mail.messages.len(), unread))
                            .font(font(12.0))
                            .color(p.text3),
                    );
                }
            });
        });
    }

    // ------------------------------------------------------------------ toasts

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let p = pal();
        let mut action: Option<(u64, ToastAction)> = None;
        let mut close: Option<u64> = None;
        egui::Area::new(Id::new("toasts"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -40.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for t in &self.toasts {
                    let (icon, tone) = match t.kind {
                        ToastKind::Info => (icons::INFO, p.accent_text),
                        ToastKind::Success => (icons::CHECK_CIRCLE, p.success),
                        ToastKind::Error => (icons::WARNING_OCTAGON, p.danger),
                    };
                    Frame::NONE
                        .fill(if p.dark { p.card_alt } else { theme::hex(0x292929) })
                        .corner_radius(CornerRadius::same(8))
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha(60),
                        })
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(560.0);
                            ui.horizontal(|ui| {
                                let fg = Color32::from_gray(240);
                                let tone = if p.dark { tone } else { mix(tone, Color32::WHITE, 0.35) };
                                ui.label(RichText::new(icon).font(icon_fill_font(17.0)).color(tone));
                                ui.add(egui::Label::new(RichText::new(&t.text).font(font(13.5)).color(fg)).wrap());
                                if let Some(a) = &t.action {
                                    let label = match a {
                                        ToastAction::Undo(_) | ToastAction::UndoSend(_) => "Undo",
                                        ToastAction::OpenAccounts => "Settings",
                                    };
                                    let r = ui.add(
                                        egui::Button::new(RichText::new(label).font(font_semibold(13.0)).color(mix(p.accent, Color32::WHITE, 0.55)))
                                            .frame(false),
                                    );
                                    if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                                        action = Some((t.id, a.clone()));
                                    }
                                }
                                let r = ui.add(egui::Button::new(RichText::new(icons::X).font(icon_font(13.0)).color(Color32::from_gray(200))).frame(false));
                                if r.clicked() {
                                    close = Some(t.id);
                                }
                            });
                        });
                }
            });
        if let Some(id) = close {
            self.toasts.retain(|t| t.id != id);
        }
        if let Some((id, a)) = action {
            self.toasts.retain(|t| t.id != id);
            match a {
                ToastAction::Undo(op) => self.undo(op),
                ToastAction::UndoSend(outbox) => self.undo_send(outbox),
                ToastAction::OpenAccounts => self.settings = Some(SettingsState::accounts()),
            }
        }
    }

    fn drag_preview(&mut self, ctx: &egui::Context) {
        if let Some(payload) = egui::DragAndDrop::payload::<crate::ui::mail::DragMsgs>(ctx) {
            if let Some(pos) = ctx.pointer_interact_pos() {
                let p = pal();
                egui::Area::new(Id::new("drag-preview"))
                    .fixed_pos(pos + vec2(12.0, 8.0))
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .show(ctx, |ui| {
                        Frame::NONE
                            .fill(p.accent)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                let n = payload.0.len();
                                ui.label(
                                    RichText::new(format!(
                                        "{}  Move {} {}",
                                        icons::ENVELOPE_SIMPLE,
                                        n,
                                        if n == 1 { "item" } else { "items" }
                                    ))
                                    .font(icon_font(13.0))
                                    .color(Color32::WHITE),
                                );
                            });
                    });
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
        }
    }

    // ----------------------------------------------------------------- dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let p = pal();
        let mut keep = true;
        let modal = Modal::new(Id::new("dialog")).frame(
            Frame::NONE
                .fill(p.card)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(24))
                .shadow(crate::theme::window_shadow()),
        );
        let resp = modal.show(ctx, |ui| {
            ui.set_width(match &dialog {
                Dialog::Source { .. } => 760.0,
                Dialog::Shortcuts => 560.0,
                Dialog::MoveTo { .. } => 380.0,
                _ => 400.0,
            });
            match &mut dialog {
                Dialog::NewFolder { account_id, parent, name } => {
                    widgets::heading(ui, "Create new folder", 18.0);
                    ui.add_space(12.0);
                    let r = widgets::text_input(ui, name, "Folder name", ui.available_width());
                    r.request_focus();
                    ui.add_space(16.0);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if (widgets::primary_button(ui, None, "Create").clicked() || enter) && !name.trim().is_empty() {
                            self.engine.send(Command::CreateFolder {
                                account_id: account_id.clone(),
                                name: name.trim().to_string(),
                                parent: *parent,
                            });
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::RenameFolder { folder_id, name } => {
                    widgets::heading(ui, "Rename folder", 18.0);
                    ui.add_space(12.0);
                    let r = widgets::text_input(ui, name, "Folder name", ui.available_width());
                    r.request_focus();
                    ui.add_space(16.0);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if (widgets::primary_button(ui, None, "Rename").clicked() || enter) && !name.trim().is_empty() {
                            self.engine.send(Command::RenameFolder {
                                folder_id: *folder_id,
                                name: name.trim().to_string(),
                            });
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::DeleteFolder { folder_id, name } => {
                    widgets::heading(ui, "Delete folder?", 18.0);
                    ui.add_space(8.0);
                    ui.label(format!("\"{name}\" and all messages in it will be permanently deleted from the server."));
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, Some(icons::TRASH), "Delete").clicked() {
                            self.engine.send(Command::DeleteFolder { folder_id: *folder_id });
                            if self.mail.current_folder_id() == Some(*folder_id) {
                                self.mail.select_default_folder(&self.store, &self.cfg);
                            }
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::EmptyFolder { folder_id, name } => {
                    widgets::heading(ui, &format!("Empty \"{name}\"?"), 18.0);
                    ui.add_space(8.0);
                    ui.label("All messages in this folder will be permanently deleted. This can't be undone.");
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, Some(icons::TRASH), "Delete all").clicked() {
                            self.engine.send(Command::EmptyFolder { folder_id: *folder_id });
                            let _ = self.store.clear_folder_messages(*folder_id);
                            self.mail.list_dirty = true;
                            self.mail.folders_dirty = true;
                            self.mail.reading = None;
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::MoveTo { ids, filter } => {
                    widgets::heading(ui, "Move to folder", 18.0);
                    ui.add_space(10.0);
                    let r = widgets::text_input(ui, filter, "Search folders", ui.available_width());
                    r.request_focus();
                    ui.add_space(8.0);
                    let account = self
                        .store
                        .messages_by_ids(ids)
                        .ok()
                        .and_then(|m| m.first().map(|m| m.account_id.clone()))
                        .unwrap_or_default();
                    let folders = self.store.folders(&account).unwrap_or_default();
                    let f = filter.to_lowercase();
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for folder in folders.iter().filter(|x| x.selectable) {
                            let label = folder.label();
                            if !f.is_empty() && !label.to_lowercase().contains(&f) {
                                continue;
                            }
                            if widgets::menu_item(ui, folder.role.icon(), &label).clicked() {
                                let ids = ids.clone();
                                self.move_messages(&ids, folder.id);
                                keep = false;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if widgets::secondary_button(ui, None, "Cancel").clicked() {
                        keep = false;
                    }
                }
                Dialog::RemoveAccount { account_id, name } => {
                    widgets::heading(ui, "Remove account?", 18.0);
                    ui.add_space(8.0);
                    ui.label(format!(
                        "{name} will be removed from RustMail along with its downloaded mail on this computer. Nothing is deleted on the server."
                    ));
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if widgets::primary_button(ui, Some(icons::TRASH), "Remove").clicked() {
                            self.remove_account(&account_id.clone());
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Source { title, text } => {
                    ui.horizontal(|ui| {
                        widgets::heading(ui, title, 17.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::X, "Close").clicked() {
                                keep = false;
                            }
                            if widgets::subtle_button(ui, icons::COPY, "Copy").clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                        });
                    });
                    ui.add_space(8.0);
                    egui::ScrollArea::both().max_height(520.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text.as_str())
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        );
                    });
                }
                Dialog::Shortcuts => {
                    ui.horizontal(|ui| {
                        widgets::heading(ui, "Keyboard shortcuts", 18.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::X, "Close").clicked() {
                                keep = false;
                            }
                        });
                    });
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                        egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 8.0]).striped(true).show(ui, |ui| {
                            for (k, d) in SHORTCUTS {
                                ui.label(RichText::new(*k).font(font_semibold(13.0)));
                                ui.label(RichText::new(*d).font(font(13.0)).color(p.text2));
                                ui.end_row();
                            }
                        });
                    });
                }
                Dialog::About => {
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(12), p.accent);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::ENVELOPE_SIMPLE, icon_fill_font(32.0), Color32::WHITE);
                        ui.add_space(8.0);
                        widgets::heading(ui, "RustMail", 20.0);
                        ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(p.text3));
                        ui.add_space(8.0);
                        ui.label("A fast, private desktop mail client written in Rust.");
                        ui.label(RichText::new(format!("UI font: {}", self.font_family)).font(font(12.0)).color(p.text3));
                        ui.label(
                            RichText::new(format!("Data: {}", crate::config::Paths::get().data_dir.display()))
                                .font(font(12.0))
                                .color(p.text3),
                        );
                        ui.add_space(14.0);
                        if widgets::primary_button(ui, None, "Close").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::NewTaskList { name } => {
                    widgets::heading(ui, "New list", 18.0);
                    ui.add_space(12.0);
                    let r = widgets::text_input(ui, name, "List name", ui.available_width());
                    r.request_focus();
                    ui.add_space(16.0);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if (widgets::primary_button(ui, None, "Create").clicked() || enter) && !name.trim().is_empty() {
                            if let Ok(id) = self.store.save_task_list(0, name.trim()) {
                                self.todo.view = crate::ui::todo::TodoView::List(id);
                            }
                            self.todo.dirty = true;
                            keep = false;
                        }
                        if widgets::secondary_button(ui, None, "Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        if resp.should_close() {
            keep = false;
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    pub fn remove_account(&mut self, account_id: &str) {
        self.cfg.accounts.retain(|a| a.id != account_id);
        self.cfg.favorites.retain(|f| f.account_id != account_id);
        if self.cfg.default_account == account_id {
            self.cfg.default_account = self.cfg.accounts.first().map(|a| a.id.clone()).unwrap_or_default();
        }
        let _ = self.store.delete_account(account_id);
        crate::secrets::delete_all(account_id);
        self.save_config_now();
        self.mail.folders_dirty = true;
        self.mail.list_dirty = true;
        self.mail.reading = None;
        self.mail.selection.clear();
        self.mail.select_default_folder(&self.store, &self.cfg);
        self.statuses.remove(account_id);
        self.toast("Account removed", ToastKind::Info);
    }
}

pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+N", "New message / event / contact / task"),
    ("Ctrl+R", "Reply"),
    ("Ctrl+Shift+R", "Reply all"),
    ("Ctrl+F", "Forward"),
    ("Ctrl+Enter", "Send (while composing)"),
    ("Ctrl+S", "Save draft (while composing)"),
    ("Delete", "Delete selected messages"),
    ("Shift+Delete", "Permanently delete"),
    ("E / Backspace", "Archive"),
    ("Ctrl+Q / Ctrl+U", "Mark as read / unread"),
    ("Insert", "Flag / unflag"),
    ("Ctrl+Shift+V", "Move to folder"),
    ("↑ / ↓ or K / J", "Previous / next message"),
    ("Enter", "Open message"),
    ("Ctrl+A", "Select all messages"),
    ("Ctrl+E or F3", "Search"),
    ("Esc", "Clear search / close full view"),
    ("F9", "Send / receive all folders"),
    ("Ctrl+1 … Ctrl+4", "Mail, Calendar, People, To Do"),
    ("Ctrl+,", "Settings"),
    ("Ctrl + / Ctrl − / Ctrl+0", "Zoom in / out / reset"),
];
