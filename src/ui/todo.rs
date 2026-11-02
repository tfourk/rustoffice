//! To Do module: My Day, Important, Planned, Flagged email and custom lists.

use chrono::Duration;
use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Key, Layout, Margin, Pos2,
    RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::app::{App, Dialog, ToastKind};
use crate::icons;
use crate::model::*;
use crate::theme::{font, font_semibold, hex, icon_fill_font, icon_font, mix, pal};
use crate::ui::calendar::mini_month;
use crate::ui::widgets::{self, ribbon_button, ribbon_separator};
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TodoView {
    #[default]
    MyDay,
    Important,
    Planned,
    Flagged,
    List(i64),
}

#[derive(Default)]
pub struct TodoState {
    pub view: TodoView,
    pub lists: Vec<TaskList>,
    pub tasks: Vec<Task>,
    pub flagged: Vec<MessageSummary>,
    pub dirty: bool,
    pub loaded: bool,
    pub selected: Option<i64>,
    pub new_title: String,
    pub focus_new: bool,
    pub hide_completed: bool,
    pub completed_open: bool,
    pub picking_due: bool,
    pub picker_month: Option<chrono::NaiveDate>,
    pub renaming: Option<(i64, String)>,
}

fn today_str() -> String {
    util::today().format("%Y-%m-%d").to_string()
}

fn due_label(due: i64) -> (String, bool) {
    let d = util::local(due).date_naive();
    let today = util::today();
    let overdue = d < today;
    let label = if d == today {
        "Today".to_string()
    } else if d == today + Duration::days(1) {
        "Tomorrow".to_string()
    } else if d == today - Duration::days(1) {
        "Yesterday".to_string()
    } else {
        d.format("%a, %b %-d").to_string()
    };
    (label, overdue)
}

impl App {
    fn todo_reload(&mut self) {
        self.todo.lists = self.store.task_lists().unwrap_or_default();
        self.todo.tasks = self.store.tasks().unwrap_or_default();
        self.todo.flagged = self.store.flagged_messages().unwrap_or_default();
        self.todo.dirty = false;
        self.todo.loaded = true;
    }

    fn visible_tasks(&self) -> Vec<Task> {
        let today = today_str();
        let q = self.search.trim().to_lowercase();
        let mut v: Vec<Task> = self
            .todo
            .tasks
            .iter()
            .filter(|t| match self.todo.view {
                TodoView::MyDay => t.my_day == today || t.due.map(|d| util::local(d).date_naive() == util::today()).unwrap_or(false),
                TodoView::Important => t.important,
                TodoView::Planned => t.due.is_some(),
                TodoView::Flagged => false,
                TodoView::List(id) => t.list_id == id,
            })
            .filter(|t| q.is_empty() || t.title.to_lowercase().contains(&q) || t.notes.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if self.todo.view == TodoView::Planned {
            v.sort_by_key(|t| (t.completed, t.due.unwrap_or(i64::MAX)));
        }
        v
    }

    fn save_task(&mut self, t: &Task) {
        if let Err(e) = self.store.save_task(t) {
            self.toast(format!("Couldn't save task: {e}"), ToastKind::Error);
        }
        self.todo.dirty = true;
    }

    fn toggle_complete(&mut self, mut t: Task) {
        t.completed = !t.completed;
        t.completed_at = if t.completed { Some(util::now_ts()) } else { None };
        self.save_task(&t);
        if t.completed {
            self.toast(format!("Completed \"{}\"", util::truncate_chars(&t.title, 40)), ToastKind::Success);
        }
    }

    pub fn ribbon_todo(&mut self, ui: &mut Ui, wide: bool) {
        if widgets::primary_button(ui, Some(icons::PLUS), "New task").clicked() {
            self.todo.focus_new = true;
        }
        if ribbon_button(ui, icons::LIST_PLUS, "New list", wide, true).clicked() {
            self.dialog = Some(Dialog::NewTaskList { name: String::new() });
        }
        ribbon_separator(ui);
        let sel = self.todo.selected.and_then(|id| self.todo.tasks.iter().find(|t| t.id == id).cloned());
        let has = sel.is_some();
        if ribbon_button(ui, icons::CHECK_CIRCLE, "Complete", wide, has).clicked() {
            if let Some(t) = sel.clone() {
                self.toggle_complete(t);
            }
        }
        if widgets::ribbon_button_ex(ui, icons::STAR, "Important", wide, has, false, sel.as_ref().map(|t| t.important).unwrap_or(false)).clicked() {
            if let Some(mut t) = sel.clone() {
                t.important = !t.important;
                self.save_task(&t);
            }
        }
        let in_day = sel.as_ref().map(|t| t.my_day == today_str()).unwrap_or(false);
        if widgets::ribbon_button_ex(ui, icons::SUN, if in_day { "Remove from My Day" } else { "Add to My Day" }, wide, has, false, in_day).clicked() {
            if let Some(mut t) = sel.clone() {
                t.my_day = if in_day { String::new() } else { today_str() };
                self.save_task(&t);
            }
        }
        if ribbon_button(ui, icons::TRASH, "Delete", wide, has).clicked() {
            if let Some(t) = sel {
                let _ = self.store.delete_task(t.id);
                self.todo.selected = None;
                self.todo.dirty = true;
            }
        }
        ribbon_separator(ui);
        let hide = self.todo.hide_completed;
        if widgets::ribbon_button_ex(ui, icons::EYE_SLASH, "Hide completed", wide, true, false, hide).clicked() {
            self.todo.hide_completed = !hide;
        }
    }

    pub fn todo_ui(&mut self, ui: &mut Ui) {
        if self.todo.dirty || !self.todo.loaded {
            self.todo_reload();
        }
        let p = pal();
        let today = today_str();
        let count = |f: &dyn Fn(&Task) -> bool| self.todo.tasks.iter().filter(|t| !t.completed && f(t)).count();
        let n_day = count(&|t| t.my_day == today || t.due.map(|d| util::local(d).date_naive() == util::today()).unwrap_or(false));
        let n_imp = count(&|t| t.important);
        let n_plan = count(&|t| t.due.is_some());
        let n_flag = self.todo.flagged.len();
        let list_counts: Vec<(TaskList, usize)> = self
            .todo
            .lists
            .iter()
            .map(|l| (l.clone(), self.todo.tasks.iter().filter(|t| t.list_id == l.id && !t.completed).count()))
            .collect();
        egui::Panel::left("todo_nav")
            .exact_size(250.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin { left: 8, right: 10, top: 2, bottom: 8 }))
            .show(ui, |ui| {
                ScrollArea::vertical().id_salt("todo_nav").show(ui, |ui| {
                    ui.add_space(4.0);
                    let items = [
                        (TodoView::MyDay, icons::SUN, "My Day", n_day, hex(0xC17C00)),
                        (TodoView::Important, icons::STAR, "Important", n_imp, hex(0xD13438)),
                        (TodoView::Planned, icons::CALENDAR_BLANK, "Planned", n_plan, hex(0x038387)),
                        (TodoView::Flagged, icons::FLAG, "Flagged email", n_flag, hex(0xC50F1F)),
                    ];
                    for (v, icon, label, n, color) in items {
                        if todo_nav(ui, icon, label, n, self.todo.view == v, color).clicked() {
                            self.todo.view = v;
                            self.todo.selected = None;
                        }
                    }
                    ui.add_space(6.0);
                    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));
                    ui.add_space(6.0);
                    for (l, n) in &list_counts {
                        let icon = if l.id == 1 { icons::HOUSE } else { icons::LIST_BULLETS };
                        let r = todo_nav(ui, icon, &l.name, *n, self.todo.view == TodoView::List(l.id), p.accent);
                        if r.clicked() {
                            self.todo.view = TodoView::List(l.id);
                            self.todo.selected = None;
                        }
                        let (lid, lname) = (l.id, l.name.clone());
                        r.context_menu(|ui| {
                            if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Rename list").clicked() {
                                self.todo.renaming = Some((lid, lname.clone()));
                            }
                            if lid != 1 && widgets::menu_item(ui, icons::TRASH, "Delete list").clicked() {
                                let _ = self.store.delete_task_list(lid);
                                if self.todo.view == TodoView::List(lid) {
                                    self.todo.view = TodoView::MyDay;
                                }
                                self.todo.dirty = true;
                            }
                        });
                    }
                    ui.add_space(4.0);
                    if widgets::subtle_button(ui, icons::PLUS, "New list").clicked() {
                        self.dialog = Some(Dialog::NewTaskList { name: String::new() });
                    }
                });
            });
        if self.todo.selected.is_some() {
            egui::Panel::right("todo_detail")
                .exact_size(330.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::NONE.inner_margin(Margin { left: 6, right: 8, top: 0, bottom: 8 }))
                .show(ui, |ui| {
                    widgets::card().show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.task_detail(ui);
                    });
                });
        }
        egui::CentralPanel::no_frame()
            .frame(Frame::NONE.inner_margin(Margin { left: 0, right: 8, top: 0, bottom: 8 }))
            .show(ui, |ui| {
                widgets::card().show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.todo_main(ui);
                });
            });
        self.rename_list_dialog(ui.ctx());
    }

    fn rename_list_dialog(&mut self, ctx: &egui::Context) {
        let Some((id, mut name)) = self.todo.renaming.take() else { return };
        let mut keep = true;
        let r = egui::Modal::new(Id::new("rename-list")).show(ctx, |ui| {
            ui.set_width(360.0);
            widgets::heading(ui, "Rename list", 17.0);
            ui.add_space(10.0);
            let resp = widgets::text_input(ui, &mut name, "List name", 340.0);
            resp.request_focus();
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (widgets::primary_button(ui, None, "Save").clicked() || enter) && !name.trim().is_empty() {
                    let _ = self.store.save_task_list(id, name.trim());
                    self.todo.dirty = true;
                    keep = false;
                }
                if widgets::secondary_button(ui, None, "Cancel").clicked() {
                    keep = false;
                }
            });
        });
        if r.should_close() {
            keep = false;
        }
        if keep {
            self.todo.renaming = Some((id, name));
        }
    }

    fn todo_main(&mut self, ui: &mut Ui) {
        let p = pal();
        let (title, icon, color) = match self.todo.view {
            TodoView::MyDay => ("My Day".to_string(), icons::SUN, hex(0xC17C00)),
            TodoView::Important => ("Important".to_string(), icons::STAR, hex(0xD13438)),
            TodoView::Planned => ("Planned".to_string(), icons::CALENDAR_BLANK, hex(0x038387)),
            TodoView::Flagged => ("Flagged email".to_string(), icons::FLAG, hex(0xC50F1F)),
            TodoView::List(id) => (
                self.todo.lists.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or_default(),
                if id == 1 { icons::HOUSE } else { icons::LIST_BULLETS },
                p.accent,
            ),
        };
        Frame::NONE.inner_margin(Margin { left: 24, right: 24, top: 18, bottom: 8 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).font(icon_fill_font(24.0)).color(color));
                ui.label(RichText::new(&title).font(font_semibold(24.0)).color(p.text));
            });
            if self.todo.view == TodoView::MyDay {
                ui.label(RichText::new(util::fmt_long_date(util::today())).font(font(13.5)).color(p.text3));
            }
        });

        if self.todo.view == TodoView::Flagged {
            self.flagged_list(ui);
            return;
        }

        // Add task row.
        Frame::NONE.inner_margin(Margin { left: 20, right: 20, top: 4, bottom: 8 }).show(ui, |ui| {
            Frame::NONE
                .fill(p.card_alt)
                .stroke(Stroke::new(1.0, p.stroke))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::PLUS).font(icon_font(18.0)).color(p.accent));
                        let id = Id::new("new-task");
                        let r = ui.add(
                            TextEdit::singleline(&mut self.todo.new_title)
                                .id(id)
                                .frame(egui::Frame::NONE)
                                .hint_text(RichText::new("Add a task").color(p.accent_text))
                                .font(font(14.5))
                                .desired_width(ui.available_width())
                                .margin(Margin::symmetric(0, 8)),
                        );
                        if self.todo.focus_new {
                            r.request_focus();
                            self.todo.focus_new = false;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.todo.new_title.trim().is_empty() {
                            let title = std::mem::take(&mut self.todo.new_title);
                            let list_id = match self.todo.view {
                                TodoView::List(id) => id,
                                _ => 1,
                            };
                            let t = Task {
                                list_id,
                                title: title.trim().to_string(),
                                important: self.todo.view == TodoView::Important,
                                my_day: if self.todo.view == TodoView::MyDay { today_str() } else { String::new() },
                                due: if self.todo.view == TodoView::Planned { Some(util::local_midnight(util::today())) } else { None },
                                created: util::now_ts(),
                                ..Default::default()
                            };
                            self.save_task(&t);
                            r.request_focus();
                        }
                    });
                });
        });

        let tasks = self.visible_tasks();
        let (open, done): (Vec<Task>, Vec<Task>) = tasks.into_iter().partition(|t| !t.completed);
        let mut toggle: Option<Task> = None;
        let mut star: Option<Task> = None;
        let mut select: Option<i64> = None;
        ScrollArea::vertical().id_salt("tasks").auto_shrink([false, false]).show(ui, |ui| {
            Frame::NONE.inner_margin(Margin { left: 20, right: 20, top: 0, bottom: 20 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if open.is_empty() && done.is_empty() {
                    let (t, s) = match self.todo.view {
                        TodoView::MyDay => ("Focus on your day", "Add tasks you want to get done today."),
                        TodoView::Important => ("Nothing important yet", "Star tasks to see them here."),
                        TodoView::Planned => ("Nothing planned", "Tasks with due dates show up here."),
                        _ => ("No tasks", "Add one above."),
                    };
                    widgets::empty_state(ui, icons::CHECK_CIRCLE, t, s);
                }
                ui.spacing_mut().item_spacing.y = 4.0;
                for t in &open {
                    if let Some(a) = self.task_row(ui, t) {
                        match a {
                            0 => toggle = Some(t.clone()),
                            1 => star = Some(t.clone()),
                            _ => select = Some(t.id),
                        }
                    }
                }
                if !done.is_empty() && !self.todo.hide_completed {
                    ui.add_space(8.0);
                    let r = ui.add(egui::Button::new(
                        RichText::new(format!("{}  Completed  {}", if self.todo.completed_open { icons::CARET_DOWN } else { icons::CARET_RIGHT }, done.len()))
                            .font(icon_font(13.5))
                            .color(p.text2),
                    ).frame(false));
                    if r.clicked() {
                        self.todo.completed_open = !self.todo.completed_open;
                    }
                    if self.todo.completed_open {
                        for t in &done {
                            if let Some(a) = self.task_row(ui, t) {
                                match a {
                                    0 => toggle = Some(t.clone()),
                                    1 => star = Some(t.clone()),
                                    _ => select = Some(t.id),
                                }
                            }
                        }
                    }
                }
            });
        });
        if let Some(t) = toggle {
            self.toggle_complete(t);
        }
        if let Some(mut t) = star {
            t.important = !t.important;
            self.save_task(&t);
        }
        if let Some(id) = select {
            self.todo.selected = if self.todo.selected == Some(id) { None } else { Some(id) };
            self.todo.picking_due = false;
        }
    }

    /// Returns 0 = toggle complete, 1 = toggle star, 2 = select.
    fn task_row(&self, ui: &mut Ui, t: &Task) -> Option<u8> {
        let p = pal();
        let selected = self.todo.selected == Some(t.id);
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
        let bg = if selected { p.selected } else if resp.hovered() { p.hover } else { p.card_alt };
        ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
        ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, mix(p.stroke, p.card, 0.4)), egui::StrokeKind::Inside);
        let mut action = None;
        let check_c = Pos2::new(rect.left() + 24.0, rect.center().y);
        let check_r = egui::Rect::from_center_size(check_c, vec2(26.0, 26.0));
        let cr = ui.interact(check_r, Id::new(("task-check", t.id)), Sense::click());
        widgets::paint_round_check(ui.painter(), check_c, 9.5, t.completed, cr.hovered(), p.accent, p.text2);
        if cr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            action = Some(0);
        }
        let title_color = if t.completed { p.text3 } else { p.text };
        let galley = ui.painter().layout(t.title.clone(), font(14.5), title_color, rect.width() - 110.0);
        let has_sub = t.due.is_some() || !t.notes.is_empty() || t.my_day == today_str() || !matches!(self.todo.view, TodoView::List(_));
        let ty = if has_sub { rect.top() + 9.0 } else { rect.center().y - galley.size().y / 2.0 };
        let gw = galley.size().x;
        ui.painter().galley(Pos2::new(rect.left() + 46.0, ty), galley, title_color);
        if t.completed {
            ui.painter().hline(rect.left() + 46.0..=rect.left() + 46.0 + gw, ty + 10.0, Stroke::new(1.0, p.text3));
        }
        if has_sub {
            let mut parts: Vec<(String, Color32)> = Vec::new();
            if !matches!(self.todo.view, TodoView::List(_)) {
                if let Some(l) = self.todo.lists.iter().find(|l| l.id == t.list_id) {
                    parts.push((l.name.clone(), p.text3));
                }
            }
            if t.my_day == today_str() && self.todo.view != TodoView::MyDay {
                parts.push((format!("{} My Day", icons::SUN), p.text3));
            }
            if let Some(d) = t.due {
                let (label, overdue) = due_label(d);
                parts.push((format!("{} {label}", icons::CALENDAR_BLANK), if overdue && !t.completed { p.danger } else if label == "Today" { p.accent_text } else { p.text3 }));
            }
            if !t.notes.is_empty() {
                parts.push((format!("{} Note", icons::NOTE), p.text3));
            }
            let mut x = rect.left() + 46.0;
            for (i, (s, c)) in parts.iter().enumerate() {
                if i > 0 {
                    let g = ui.painter().layout_no_wrap(" · ".into(), font(12.0), p.text3);
                    let w = g.size().x;
                    ui.painter().galley(Pos2::new(x, rect.bottom() - 22.0), g, p.text3);
                    x += w;
                }
                let g = ui.painter().layout_no_wrap(s.clone(), icon_font(12.0), *c);
                let w = g.size().x;
                ui.painter().galley(Pos2::new(x, rect.bottom() - 22.0), g, *c);
                x += w;
            }
        }
        let star_c = Pos2::new(rect.right() - 24.0, rect.center().y);
        let sr = ui.interact(egui::Rect::from_center_size(star_c, vec2(28.0, 28.0)), Id::new(("task-star", t.id)), Sense::click());
        ui.painter().text(
            star_c,
            Align2::CENTER_CENTER,
            icons::STAR,
            if t.important { icon_fill_font(17.0) } else { icon_font(17.0) },
            if t.important { p.accent } else if sr.hovered() { p.text } else { p.text3 },
        );
        if sr.on_hover_text(if t.important { "Remove importance" } else { "Mark as important" }).clicked() {
            action = Some(1);
        }
        if action.is_none() && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            action = Some(2);
        }
        action
    }

    fn flagged_list(&mut self, ui: &mut Ui) {
        let p = pal();
        let flagged = self.todo.flagged.clone();
        let mut unflag = None;
        let mut open = None;
        ScrollArea::vertical().id_salt("flagged").auto_shrink([false, false]).show(ui, |ui| {
            Frame::NONE.inner_margin(Margin { left: 20, right: 20, top: 4, bottom: 20 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if flagged.is_empty() {
                    widgets::empty_state(ui, icons::FLAG, "No flagged email", "Flag messages in Mail to follow up on them here.");
                }
                for m in &flagged {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
                    ui.painter().rect_filled(rect, CornerRadius::same(6), if resp.hovered() { p.hover } else { p.card_alt });
                    let c = Pos2::new(rect.left() + 24.0, rect.center().y);
                    let cr = ui.interact(egui::Rect::from_center_size(c, vec2(26.0, 26.0)), Id::new(("flag-done", m.id)), Sense::click());
                    widgets::paint_round_check(ui.painter(), c, 9.5, false, cr.hovered(), p.accent, p.text2);
                    if cr.on_hover_text("Mark complete (clears the flag)").clicked() {
                        unflag = Some(m.id);
                    }
                    ui.painter().text(Pos2::new(rect.left() + 46.0, rect.top() + 18.0), Align2::LEFT_CENTER, util::truncate_chars(m.subject_or_placeholder(), 80), font_semibold(14.0), p.text);
                    let sub = format!("{} · {}", m.sender(), util::fmt_list_date(m.date));
                    ui.painter().text(Pos2::new(rect.left() + 46.0, rect.bottom() - 16.0), Align2::LEFT_CENTER, sub, font(12.5), p.text3);
                    ui.painter().text(Pos2::new(rect.right() - 20.0, rect.center().y), Align2::CENTER_CENTER, icons::FLAG, icon_fill_font(16.0), p.flag);
                    if let Some(d) = m.flag_due {
                        let (label, overdue) = due_label(d);
                        ui.painter().text(Pos2::new(rect.right() - 40.0, rect.center().y), Align2::RIGHT_CENTER, format!("Due {label}"), font(12.0), if overdue { p.danger } else { p.text3 });
                    }
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        open = Some(m.clone());
                    }
                    ui.add_space(4.0);
                }
            });
        });
        if let Some(id) = unflag {
            self.set_flag(&[id], false);
            self.todo.dirty = true;
            self.toast("Flag cleared", ToastKind::Success);
        }
        if let Some(m) = open {
            self.open_in_mail(&m);
        }
    }

    fn task_detail(&mut self, ui: &mut Ui) {
        let p = pal();
        let Some(mut t) = self.todo.selected.and_then(|id| self.todo.tasks.iter().find(|t| t.id == id).cloned()) else {
            self.todo.selected = None;
            return;
        };
        let original = t.clone();
        let mut close = false;
        let mut delete = false;
        ScrollArea::vertical().id_salt("task-detail").auto_shrink([false, false]).show(ui, |ui| {
            Frame::NONE.inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let c = widgets::round_check(ui, t.completed, 24.0, p.accent);
                    if c.clicked() {
                        t.completed = !t.completed;
                        t.completed_at = if t.completed { Some(util::now_ts()) } else { None };
                    }
                    ui.add(
                        TextEdit::multiline(&mut t.title)
                            .frame(egui::Frame::NONE)
                            .font(font_semibold(17.0))
                            .desired_rows(1)
                            .desired_width(ui.available_width() - 34.0),
                    );
                    if widgets::icon_button_ex(ui, icons::STAR, 28.0, 17.0, "Important", t.important, if t.important { Some(p.accent) } else { None }).clicked() {
                        t.important = !t.important;
                    }
                });
                ui.add_space(10.0);
                let in_day = t.my_day == today_str();
                if detail_button(ui, icons::SUN, if in_day { "Added to My Day" } else { "Add to My Day" }, in_day).clicked() {
                    t.my_day = if in_day { String::new() } else { today_str() };
                }
                ui.add_space(6.0);
                let due_text = t.due.map(|d| format!("Due {}", due_label(d).0)).unwrap_or_else(|| "Add due date".into());
                if detail_button(ui, icons::CALENDAR_BLANK, &due_text, t.due.is_some()).clicked() {
                    self.todo.picking_due = !self.todo.picking_due;
                }
                if self.todo.picking_due {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (label, days) in [("Today", 0), ("Tomorrow", 1), ("Next week", 7)] {
                            if widgets::secondary_button(ui, None, label).clicked() {
                                t.due = Some(util::local_midnight(util::today() + Duration::days(days)));
                                self.todo.picking_due = false;
                            }
                        }
                        if t.due.is_some() && widgets::subtle_button(ui, icons::X, "Remove").clicked() {
                            t.due = None;
                            self.todo.picking_due = false;
                        }
                    });
                    let mut month = self.todo.picker_month.unwrap_or_else(util::today);
                    let sel = t.due.map(|d| util::local(d).date_naive()).unwrap_or_else(util::today);
                    if let Some(d) = mini_month(ui, &mut month, sel, &Default::default(), Id::new("due-picker")) {
                        t.due = Some(util::local_midnight(d));
                        self.todo.picking_due = false;
                    }
                    self.todo.picker_month = Some(month);
                }
                ui.add_space(6.0);
                let remind = t.reminder.map(|r| format!("Remind me {}", util::fmt_full_date(r))).unwrap_or_else(|| "Remind me".into());
                let rr = detail_button(ui, icons::BELL, &remind, t.reminder.is_some());
                egui::Popup::menu(&rr).show(|ui| {
                    let now = util::local(util::now_ts());
                    let later = util::now_ts() + 3 * 3600;
                    let tomorrow = util::ts_from_date_time(util::today() + Duration::days(1), 9, 0);
                    let next_week = util::ts_from_date_time(util::today() + Duration::days(7), 9, 0);
                    for (label, ts) in [("Later today", later), ("Tomorrow 9:00", tomorrow), ("Next week", next_week)] {
                        if widgets::menu_item(ui, icons::BELL, label).clicked() {
                            t.reminder = Some(ts);
                        }
                    }
                    if t.reminder.is_some() && widgets::menu_item(ui, icons::X, "Remove reminder").clicked() {
                        t.reminder = None;
                    }
                    let _ = now;
                });
                ui.add_space(6.0);
                let lists = self.todo.lists.clone();
                let current = lists.iter().find(|l| l.id == t.list_id).map(|l| l.name.clone()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::LIST_BULLETS).font(icon_font(16.0)).color(p.text2));
                    egui::ComboBox::from_id_salt("task-list").selected_text(current).width(220.0).show_ui(ui, |ui| {
                        for l in &lists {
                            ui.selectable_value(&mut t.list_id, l.id, &l.name);
                        }
                    });
                });
                ui.add_space(12.0);
                ui.add(
                    TextEdit::multiline(&mut t.notes)
                        .hint_text(RichText::new("Add note").color(p.text3))
                        .desired_rows(6)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::same(10)),
                );
                ui.add_space(12.0);
                ui.label(RichText::new(format!("Created {}", util::fmt_full_date(t.created))).font(font(12.0)).color(p.text3));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if widgets::secondary_button(ui, Some(icons::X), "Close").clicked() {
                        close = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::subtle_button(ui, icons::TRASH, "Delete").clicked() {
                            delete = true;
                        }
                    });
                });
            });
        });
        if delete {
            let _ = self.store.delete_task(t.id);
            self.todo.selected = None;
            self.todo.dirty = true;
            return;
        }
        if t != original {
            if t.completed != original.completed {
                t.completed_at = if t.completed { Some(util::now_ts()) } else { None };
            }
            let _ = self.store.save_task(&t);
            if let Some(slot) = self.todo.tasks.iter_mut().find(|x| x.id == t.id) {
                *slot = t;
            }
        }
        if close {
            self.todo.selected = None;
        }
    }
}

fn todo_nav(ui: &mut Ui, icon: &str, label: &str, count: usize, selected: bool, color: Color32) -> egui::Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), if selected { p.selected } else { p.hover });
    }
    ui.painter().text(Pos2::new(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_font(17.0), color);
    ui.painter().text(Pos2::new(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, label, if selected { font_semibold(14.0) } else { font(14.0) }, p.text);
    if count > 0 {
        ui.painter().text(Pos2::new(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, count.to_string(), font(12.5), p.text3);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn detail_button(ui: &mut Ui, icon: &str, label: &str, active: bool) -> egui::Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(6), if resp.hovered() { p.hover } else { p.card_alt });
    ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, mix(p.stroke, p.card, 0.3)), egui::StrokeKind::Inside);
    let c = if active { p.accent_text } else { p.text2 };
    ui.painter().text(Pos2::new(rect.left() + 20.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_font(16.0), c);
    ui.painter().text(Pos2::new(rect.left() + 40.0, rect.center().y), Align2::LEFT_CENTER, label, font(14.0), if active { p.accent_text } else { p.text });
    resp.on_hover_cursor(CursorIcon::PointingHand)
}
