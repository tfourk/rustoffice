//! Calendar module: month / week / work week / day views and the event editor.

use std::collections::HashSet;

use chrono::{Datelike, Duration, NaiveDate, Timelike};
use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, Modal, Pos2,
    Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui,
};

use crate::app::{App, ToastKind};
use crate::cal;
use crate::icons;
use crate::mime;
use crate::model::*;
use crate::theme::{self, font, font_semibold, icon_fill_font, icon_font, mix, pal, with_alpha};
use crate::ui::widgets::{self, ribbon_button, ribbon_button_ex, ribbon_separator};
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalView {
    Day,
    WorkWeek,
    Week,
    Month,
}

impl CalView {
    pub fn label(self) -> &'static str {
        match self {
            CalView::Day => "Day",
            CalView::WorkWeek => "Work week",
            CalView::Week => "Week",
            CalView::Month => "Month",
        }
    }
}

pub struct EventEditor {
    pub ev: CalEvent,
    pub start_date: NaiveDate,
    pub start_min: u32,
    pub end_date: NaiveDate,
    pub end_min: u32,
    pub picker: Option<u8>,
    pub picker_month: NaiveDate,
    pub error: Option<String>,
    pub is_new: bool,
}

impl EventEditor {
    fn from_event(ev: CalEvent, is_new: bool) -> Self {
        let s = util::local(ev.start);
        let mut e = util::local(ev.end.max(ev.start));
        if ev.all_day {
            // Stored end is exclusive midnight; show the last day.
            e = util::local(ev.end - 1);
        }
        EventEditor {
            start_date: s.date_naive(),
            start_min: s.hour() * 60 + s.minute(),
            end_date: e.date_naive(),
            end_min: e.hour() * 60 + e.minute(),
            picker: None,
            picker_month: s.date_naive(),
            error: None,
            is_new,
            ev,
        }
    }
}

pub struct CalState {
    pub view: CalView,
    pub date: NaiveDate,
    pub mini: NaiveDate,
    pub editor: Option<EventEditor>,
    pub popup: Option<(Occurrence, Pos2)>,
    pub dirty: bool,
    cache: Option<(i64, i64, Vec<Occurrence>)>,
    pub scroll_to_now: bool,
    pub show_calendar: bool,
    pub show_birthdays: bool,
}

impl CalState {
    pub fn new() -> Self {
        let today = util::today();
        CalState {
            view: CalView::WorkWeek,
            date: today,
            mini: today,
            editor: None,
            popup: None,
            dirty: true,
            cache: None,
            scroll_to_now: true,
            show_calendar: true,
            show_birthdays: true,
        }
    }

    fn days(&self) -> Vec<NaiveDate> {
        match self.view {
            CalView::Day => vec![self.date],
            CalView::Week => {
                let s = util::start_of_week(self.date);
                (0..7).map(|i| s + Duration::days(i)).collect()
            }
            CalView::WorkWeek => {
                let s = util::start_of_week(self.date);
                (0..7)
                    .map(|i| s + Duration::days(i))
                    .filter(|d| !util::is_weekend(*d))
                    .collect()
            }
            CalView::Month => {
                let first = self.date.with_day(1).unwrap_or(self.date);
                let s = util::start_of_week(first);
                (0..42).map(|i| s + Duration::days(i)).collect()
            }
        }
    }

    fn step(&mut self, dir: i32) {
        self.date = match self.view {
            CalView::Day => self.date + Duration::days(dir as i64),
            CalView::WorkWeek | CalView::Week => self.date + Duration::days(7 * dir as i64),
            CalView::Month => util::add_months(self.date, dir),
        };
        self.mini = self.date;
    }

    fn title(&self) -> String {
        let days = self.days();
        match self.view {
            CalView::Day => util::fmt_long_date(self.date),
            CalView::Month => self.date.format("%B %Y").to_string(),
            _ => {
                let (a, b) = (days[0], *days.last().unwrap());
                if a.month() == b.month() {
                    format!("{} {}–{}, {}", a.format("%B"), a.day(), b.day(), a.year())
                } else if a.year() == b.year() {
                    format!("{} – {}, {}", a.format("%B %-d"), b.format("%B %-d"), a.year())
                } else {
                    format!("{} – {}", a.format("%b %-d, %Y"), b.format("%b %-d, %Y"))
                }
            }
        }
    }
}

impl Default for CalState {
    fn default() -> Self {
        Self::new()
    }
}

fn event_color(app: &App, e: &CalEvent) -> Color32 {
    if e.id < 0 {
        return theme::hex(0xC239B3);
    }
    if e.category.is_empty() {
        pal().accent
    } else {
        app.cfg.category_color(&e.category)
    }
}

/// Small month calendar; returns a clicked date.
pub fn mini_month(ui: &mut Ui, month: &mut NaiveDate, selected: NaiveDate, marks: &HashSet<NaiveDate>, id: Id) -> Option<NaiveDate> {
    let p = pal();
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(month.format("%B %Y").to_string()).font(font_semibold(14.0)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_button_ex(ui, icons::CARET_DOWN, 24.0, 13.0, "Next month", false, None).clicked() {
                *month = util::add_months(*month, 1);
            }
            if widgets::icon_button_ex(ui, icons::CARET_UP, 24.0, 13.0, "Previous month", false, None).clicked() {
                *month = util::add_months(*month, -1);
            }
        });
    });
    let first = month.with_day(1).unwrap_or(*month);
    let start = util::start_of_week(first);
    let cw = (ui.available_width() / 7.0).floor().clamp(26.0, 40.0);
    let (rect, _) = ui.allocate_exact_size(vec2(cw * 7.0, 24.0 + 6.0 * 28.0), Sense::hover());
    for (i, n) in util::weekday_names().iter().enumerate() {
        ui.painter().text(
            Pos2::new(rect.left() + cw * (i as f32 + 0.5), rect.top() + 12.0),
            Align2::CENTER_CENTER,
            &n[..1],
            font(12.0),
            p.text3,
        );
    }
    let today = util::today();
    for w in 0..6 {
        for d in 0..7 {
            let date = start + Duration::days(w * 7 + d);
            let c = Pos2::new(rect.left() + cw * (d as f32 + 0.5), rect.top() + 24.0 + 28.0 * (w as f32 + 0.5));
            let cell = Rect::from_center_size(c, vec2(cw, 28.0));
            let resp = ui.interact(cell, id.with((w, d)), Sense::click());
            let in_month = date.month() == month.month();
            if date == today {
                ui.painter().circle_filled(c, 12.0, p.accent);
            } else if date == selected {
                ui.painter().circle_filled(c, 12.0, p.selected);
            } else if resp.hovered() {
                ui.painter().circle_filled(c, 12.0, p.hover);
            }
            let color = if date == today {
                Color32::WHITE
            } else if in_month {
                p.text
            } else {
                p.text3
            };
            ui.painter().text(c, Align2::CENTER_CENTER, date.day().to_string(), font(12.5), color);
            if marks.contains(&date) && date != today {
                ui.painter().circle_filled(c + vec2(0.0, 10.0), 1.8, p.accent);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                clicked = Some(date);
            }
        }
    }
    clicked
}

fn minutes_label(m: u32) -> String {
    let d = util::today().and_hms_opt(m / 60, m % 60, 0).unwrap();
    let dt = chrono::TimeZone::from_local_datetime(&chrono::Local, &d).earliest();
    dt.map(|dt| util::fmt_time(&dt)).unwrap_or_default()
}

fn time_combo(ui: &mut Ui, id: Id, value: &mut u32) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(minutes_label(*value))
        .width(110.0)
        .height(300.0)
        .show_ui(ui, |ui| {
            for m in (0..24 * 60).step_by(15) {
                ui.selectable_value(value, m as u32, minutes_label(m as u32));
            }
        });
}

impl App {
    fn occurrences(&mut self, from: i64, to: i64) -> Vec<Occurrence> {
        if !self.cal.dirty {
            if let Some((a, b, v)) = &self.cal.cache {
                if *a == from && *b == to {
                    return v.clone();
                }
            }
        }
        let mut occ = if self.cal.show_calendar {
            let evs = self.store.events_between(from, to).unwrap_or_default();
            cal::occurrences(&evs, from, to)
        } else {
            Vec::new()
        };
        if self.cal.show_birthdays {
            let from_d = util::local(from).date_naive();
            let to_d = util::local(to).date_naive();
            for c in self.store.contacts().unwrap_or_default() {
                let Ok(b) = NaiveDate::parse_from_str(&c.birthday, "%Y-%m-%d") else { continue };
                for year in from_d.year()..=to_d.year() {
                    let Some(d) = NaiveDate::from_ymd_opt(year, b.month(), b.day()) else { continue };
                    if d < from_d || d > to_d {
                        continue;
                    }
                    let start = util::local_midnight(d);
                    let ev = CalEvent {
                        id: -c.id,
                        title: format!("🎂 {}'s birthday", c.display_name()),
                        start,
                        end: start + 86400,
                        all_day: true,
                        show_as: ShowAs::Free,
                        ..Default::default()
                    };
                    occ.push(Occurrence { start, end: start + 86400, event: ev });
                }
            }
            occ.sort_by_key(|o| (o.start, o.end));
        }
        let q = self.search.trim().to_lowercase();
        if !q.is_empty() {
            occ.retain(|o| {
                o.event.title.to_lowercase().contains(&q)
                    || o.event.location.to_lowercase().contains(&q)
                    || o.event.description.to_lowercase().contains(&q)
            });
        }
        self.cal.cache = Some((from, to, occ.clone()));
        self.cal.dirty = false;
        occ
    }

    pub fn cal_new_event(&mut self, at: Option<i64>) {
        let start = at.unwrap_or_else(|| {
            let now = util::local(util::now_ts());
            let base = if self.cal.date == util::today() { now.date_naive() } else { self.cal.date };
            let mins = if self.cal.date == util::today() { ((now.hour() * 60 + now.minute()) / 30 + 1) * 30 } else { 9 * 60 };
            util::ts_from_date_time(base, (mins / 60).min(23), mins % 60)
        });
        let ev = CalEvent {
            start,
            end: start + self.cfg.calendar.default_duration_min * 60,
            reminder_min: Some(self.cfg.calendar.default_reminder_min),
            ..Default::default()
        };
        self.module = crate::app::Module::Calendar;
        self.cal.editor = Some(EventEditor::from_event(ev, true));
    }

    pub fn ribbon_calendar(&mut self, ui: &mut Ui, wide: bool) {
        if widgets::primary_button(ui, Some(icons::CALENDAR_PLUS), "New event").clicked() {
            self.cal_new_event(None);
        }
        ribbon_separator(ui);
        for v in [CalView::Day, CalView::WorkWeek, CalView::Week, CalView::Month] {
            let icon = match v {
                CalView::Day => icons::CALENDAR_DOTS,
                CalView::WorkWeek => icons::COLUMNS,
                CalView::Week => icons::CALENDAR_BLANK,
                CalView::Month => icons::GRID_FOUR,
            };
            if ribbon_button_ex(ui, icon, v.label(), true, true, false, self.cal.view == v).clicked() {
                self.cal.view = v;
                self.cal.dirty = true;
                self.cal.scroll_to_now = true;
            }
        }
        ribbon_separator(ui);
        if ribbon_button(ui, icons::CALENDAR_CHECK, "Today", wide, true).clicked() {
            self.cal.date = util::today();
            self.cal.mini = self.cal.date;
            self.cal.scroll_to_now = true;
        }
        ribbon_separator(ui);
        if ribbon_button(ui, icons::UPLOAD_SIMPLE, "Import .ics", wide, true).clicked() {
            self.import_ics();
        }
        if ribbon_button(ui, icons::EXPORT, "Export .ics", wide, true).clicked() {
            self.export_ics();
        }
    }

    fn import_ics(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("iCalendar", &["ics", "ical"]).pick_file() else { return };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let imp = cal::parse_ics(&text);
                let mut n = 0;
                for mut e in imp.events {
                    if let Ok(Some(old)) = self.store.event_by_uid(&e.uid) {
                        e.id = old.id;
                    }
                    if self.store.save_event(&e).is_ok() {
                        n += 1;
                    }
                }
                self.cal.dirty = true;
                self.toast(format!("Imported {n} event(s)"), ToastKind::Success);
            }
            Err(e) => {
                self.toast(format!("Couldn't read file: {e}"), ToastKind::Error);
            }
        }
    }

    fn export_ics(&mut self) {
        let events = self.store.events_between(i64::MIN / 2, i64::MAX / 2).unwrap_or_default();
        let Some(path) = rfd::FileDialog::new().set_file_name("calendar.ics").save_file() else { return };
        match std::fs::write(&path, cal::to_ics(&events)) {
            Ok(()) => self.toast(format!("Exported {} event(s)", events.len()), ToastKind::Success),
            Err(e) => self.toast(format!("Couldn't save: {e}"), ToastKind::Error),
        };
    }

    pub fn calendar_ui(&mut self, ui: &mut Ui) {
        let p = pal();
        egui::Panel::left("cal_left")
            .exact_size(270.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin { left: 8, right: 10, top: 2, bottom: 8 }))
            .show(ui, |ui| {
                ScrollArea::vertical().id_salt("cal_left").show(ui, |ui| {
                    // Days with events in the mini month.
                    let first = self.cal.mini.with_day(1).unwrap_or(self.cal.mini);
                    let from = util::local_midnight(util::start_of_week(first));
                    let to = from + 42 * 86400;
                    let evs = self.store.events_between(from, to).unwrap_or_default();
                    let marks: HashSet<NaiveDate> = cal::occurrences(&evs, from, to)
                        .iter()
                        .map(|o| util::local(o.start).date_naive())
                        .collect();
                    let mut month = self.cal.mini;
                    if let Some(d) = mini_month(ui, &mut month, self.cal.date, &marks, Id::new("mini")) {
                        self.cal.date = d;
                        self.cal.dirty = true;
                    }
                    self.cal.mini = month;
                    ui.add_space(16.0);
                    ui.label(RichText::new("My calendars").font(font_semibold(14.0)));
                    ui.add_space(4.0);
                    let mut c1 = self.cal.show_calendar;
                    let mut c2 = self.cal.show_birthdays;
                    calendar_toggle(ui, &mut c1, "Calendar", p.accent);
                    calendar_toggle(ui, &mut c2, "Birthdays", theme::hex(0xC239B3));
                    if c1 != self.cal.show_calendar || c2 != self.cal.show_birthdays {
                        self.cal.show_calendar = c1;
                        self.cal.show_birthdays = c2;
                        self.cal.dirty = true;
                    }
                    ui.add_space(16.0);
                    ui.label(RichText::new("Coming up").font(font_semibold(14.0)));
                    ui.add_space(4.0);
                    let now = util::now_ts();
                    let up_evs = self.store.events_between(now, now + 7 * 86400).unwrap_or_default();
                    let upcoming: Vec<Occurrence> = cal::occurrences(&up_evs, now, now + 7 * 86400).into_iter().take(6).collect();
                    if upcoming.is_empty() {
                        ui.label(RichText::new("Nothing in the next 7 days").font(font(12.5)).color(p.text3));
                    }
                    for o in upcoming {
                        let s = util::local(o.start);
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
                        }
                        let bar = Rect::from_min_size(rect.left_top() + vec2(4.0, 7.0), vec2(3.0, 32.0));
                        ui.painter().rect_filled(bar, CornerRadius::same(2), event_color(self, &o.event));
                        let title = util::truncate_chars(&o.event.title, 30);
                        ui.painter().text(rect.left_top() + vec2(14.0, 9.0), Align2::LEFT_TOP, title, font_semibold(13.0), p.text);
                        let when = if o.event.all_day {
                            format!("{} · All day", s.format("%a %-d"))
                        } else {
                            format!("{} · {}", s.format("%a %-d"), util::fmt_time(&s))
                        };
                        ui.painter().text(rect.left_top() + vec2(14.0, 27.0), Align2::LEFT_TOP, when, font(12.0), p.text3);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            self.cal.date = s.date_naive();
                            self.cal.dirty = true;
                        }
                    }
                });
            });
        egui::CentralPanel::no_frame()
            .frame(Frame::NONE.inner_margin(Margin { left: 0, right: 8, top: 0, bottom: 8 }))
            .show(ui, |ui| {
                widgets::card().show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.calendar_main(ui);
                });
            });
        self.event_popup(ui.ctx());
        self.event_editor(ui.ctx());
    }

    fn calendar_main(&mut self, ui: &mut Ui) {
        let p = pal();
        Frame::NONE.inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 6 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, Some(icons::CALENDAR_CHECK), "Today").clicked() {
                    self.cal.date = util::today();
                    self.cal.mini = self.cal.date;
                    self.cal.scroll_to_now = true;
                }
                if widgets::icon_button(ui, icons::CARET_LEFT, "Previous").clicked() {
                    self.cal.step(-1);
                }
                if widgets::icon_button(ui, icons::CARET_RIGHT, "Next").clicked() {
                    self.cal.step(1);
                }
                ui.add_space(6.0);
                ui.label(RichText::new(self.cal.title()).font(font_semibold(18.0)).color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut idx = match self.cal.view {
                        CalView::Day => 0,
                        CalView::WorkWeek => 1,
                        CalView::Week => 2,
                        CalView::Month => 3,
                    };
                    // Right-to-left: lay the tab strip out as a block.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if widgets::tabs(ui, &["Day", "Work week", "Week", "Month"], &mut idx, 13.5) {
                            self.cal.view = [CalView::Day, CalView::WorkWeek, CalView::Week, CalView::Month][idx];
                            self.cal.dirty = true;
                            self.cal.scroll_to_now = true;
                        }
                    });
                });
            });
        });
        if !self.search.trim().is_empty() {
            self.cal.dirty = true;
        }
        match self.cal.view {
            CalView::Month => self.month_view(ui),
            _ => self.time_grid(ui),
        }
    }

    fn month_view(&mut self, ui: &mut Ui) {
        let p = pal();
        let days = self.cal.days();
        let from = util::local_midnight(days[0]);
        let to = util::local_midnight(*days.last().unwrap() + Duration::days(1));
        let occ = self.occurrences(from, to);
        let rect = ui.available_rect_before_wrap().shrink2(vec2(10.0, 4.0));
        ui.allocate_rect(rect, Sense::hover());
        let header_h = 28.0;
        let cw = rect.width() / 7.0;
        let ch = (rect.height() - header_h) / 6.0;
        for (i, n) in util::weekday_names().iter().enumerate() {
            ui.painter().text(
                Pos2::new(rect.left() + cw * i as f32 + 8.0, rect.top() + header_h / 2.0),
                Align2::LEFT_CENTER,
                *n,
                font_semibold(12.5),
                p.text2,
            );
        }
        let today = util::today();
        let month = self.cal.date.month();
        let mut open: Option<(Occurrence, Pos2)> = None;
        let mut new_at: Option<i64> = None;
        let mut goto_day: Option<NaiveDate> = None;
        for (i, d) in days.iter().enumerate() {
            let (w, c) = (i / 7, i % 7);
            let cell = Rect::from_min_size(
                Pos2::new(rect.left() + cw * c as f32, rect.top() + header_h + ch * w as f32),
                vec2(cw, ch),
            );
            let resp = ui.interact(cell, Id::new(("mcell", *d)), Sense::click());
            let bg = if d.month() != month {
                p.card_alt
            } else if resp.hovered() {
                mix(p.card, p.hover, 0.6)
            } else {
                p.card
            };
            ui.painter().rect_filled(cell, 0.0, bg);
            ui.painter().rect_stroke(cell, 0.0, Stroke::new(0.5, p.stroke), egui::StrokeKind::Inside);
            let num_pos = Pos2::new(cell.left() + 16.0, cell.top() + 14.0);
            if *d == today {
                ui.painter().circle_filled(num_pos, 11.0, p.accent);
            }
            let label = if d.day() == 1 && *d != today { d.format("%b %-d").to_string() } else { d.day().to_string() };
            ui.painter().text(
                if d.day() == 1 && *d != today { Pos2::new(cell.left() + 6.0, num_pos.y) } else { num_pos },
                if d.day() == 1 && *d != today { Align2::LEFT_CENTER } else { Align2::CENTER_CENTER },
                label,
                font_semibold(12.5),
                if *d == today { Color32::WHITE } else if d.month() == month { p.text } else { p.text3 },
            );
            if *d == self.cal.date && *d != today {
                ui.painter().rect_stroke(cell.shrink(1.0), 2.0, Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
            }
            let day_start = util::local_midnight(*d);
            let day_end = day_start + 86400;
            let todays: Vec<&Occurrence> = occ.iter().filter(|o| o.start < day_end && o.end.max(o.start + 1) > day_start).collect();
            let max_rows = (((ch - 30.0) / 20.0).floor() as usize).max(1);
            let mut y = cell.top() + 28.0;
            for (k, o) in todays.iter().enumerate() {
                if k + 1 == max_rows && todays.len() > max_rows {
                    let more = Rect::from_min_size(Pos2::new(cell.left() + 4.0, y), vec2(cw - 8.0, 18.0));
                    let r = ui.interact(more, Id::new(("more", *d)), Sense::click());
                    ui.painter().text(more.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, format!("+{} more", todays.len() - k), font_semibold(12.0), p.accent_text);
                    if r.clicked() {
                        goto_day = Some(*d);
                    }
                    break;
                }
                let chip = Rect::from_min_size(Pos2::new(cell.left() + 4.0, y), vec2(cw - 8.0, 18.0));
                let color = event_color(self, &o.event);
                let r = ui.interact(chip, Id::new(("mchip", o.event.id, o.start, *d)), Sense::click());
                if o.event.all_day || o.end - o.start >= 86400 {
                    ui.painter().rect_filled(chip, CornerRadius::same(4), if r.hovered() { color } else { mix(color, p.card, if p.dark { 0.45 } else { 0.25 }) });
                    let g = egui::text::LayoutJob::simple_singleline(o.event.title.clone(), font_semibold(12.0), Color32::WHITE);
                    let g = ui.painter().layout_job(egui::text::LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(chip.width() - 10.0), ..g });
                    ui.painter().galley(chip.left_center() + vec2(6.0, -g.size().y / 2.0), g, Color32::WHITE);
                } else {
                    if r.hovered() {
                        ui.painter().rect_filled(chip, CornerRadius::same(4), p.hover);
                    }
                    ui.painter().circle_filled(chip.left_center() + vec2(6.0, 0.0), 3.5, color);
                    let text = format!("{} {}", util::fmt_time(&util::local(o.start)), o.event.title);
                    let g = egui::text::LayoutJob::simple_singleline(text, font(12.0), p.text);
                    let g = ui.painter().layout_job(egui::text::LayoutJob { wrap: egui::text::TextWrapping::truncate_at_width(chip.width() - 16.0), ..g });
                    ui.painter().galley(chip.left_center() + vec2(14.0, -g.size().y / 2.0), g, p.text);
                }
                if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    open = Some(((*o).clone(), chip.right_top()));
                }
                y += 20.0;
            }
            if resp.double_clicked() {
                new_at = Some(util::ts_from_date_time(*d, 9, 0));
            } else if resp.clicked() {
                self.cal.date = *d;
            }
        }
        if let Some(o) = open {
            self.cal.popup = Some(o);
        }
        if let Some(t) = new_at {
            self.cal_new_event(Some(t));
        }
        if let Some(d) = goto_day {
            self.cal.date = d;
            self.cal.view = CalView::Day;
            self.cal.dirty = true;
        }
    }

    fn time_grid(&mut self, ui: &mut Ui) {
        let p = pal();
        let days = self.cal.days();
        let from = util::local_midnight(days[0]);
        let to = util::local_midnight(*days.last().unwrap() + Duration::days(1));
        let occ = self.occurrences(from, to);
        let gutter = 58.0;
        let avail = ui.available_rect_before_wrap();
        let col_w = ((avail.width() - gutter - 16.0) / days.len() as f32).max(60.0);
        let today = util::today();
        let mut open: Option<(Occurrence, Pos2)> = None;
        let mut new_at: Option<i64> = None;

        // Day headers.
        let (hrect, _) = ui.allocate_exact_size(vec2(avail.width(), 50.0), Sense::hover());
        for (i, d) in days.iter().enumerate() {
            let x = hrect.left() + gutter + col_w * i as f32;
            let is_today = *d == today;
            ui.painter().text(Pos2::new(x + 10.0, hrect.top() + 14.0), Align2::LEFT_CENTER, d.format("%a").to_string(), font(12.5), if is_today { p.accent_text } else { p.text2 });
            let num_pos = Pos2::new(x + 22.0, hrect.top() + 34.0);
            if is_today {
                ui.painter().circle_filled(num_pos, 13.0, p.accent);
            }
            ui.painter().text(num_pos, Align2::CENTER_CENTER, d.day().to_string(), font_semibold(17.0), if is_today { Color32::WHITE } else { p.text });
        }
        // All-day row.
        let all_day: Vec<&Occurrence> = occ.iter().filter(|o| o.event.all_day || o.end - o.start >= 86400).collect();
        let mut per_day = vec![0usize; days.len()];
        for o in &all_day {
            for (i, d) in days.iter().enumerate() {
                let ds = util::local_midnight(*d);
                if o.start < ds + 86400 && o.end > ds {
                    per_day[i] += 1;
                }
            }
        }
        let rows = per_day.iter().copied().max().unwrap_or(0).max(1);
        let ad_h = 8.0 + rows as f32 * 22.0;
        let (arect, _) = ui.allocate_exact_size(vec2(avail.width(), ad_h), Sense::hover());
        ui.painter().text(Pos2::new(arect.left() + gutter - 8.0, arect.top() + 12.0), Align2::RIGHT_CENTER, "All day", font(11.0), p.text3);
        ui.painter().hline(arect.x_range(), arect.bottom(), Stroke::new(1.0, p.stroke));
        let mut slots = vec![0usize; days.len()];
        for o in &all_day {
            let first_col = days.iter().position(|d| {
                let ds = util::local_midnight(*d);
                o.start < ds + 86400 && o.end > ds
            });
            let Some(c0) = first_col else { continue };
            let c1 = days
                .iter()
                .rposition(|d| {
                    let ds = util::local_midnight(*d);
                    o.start < ds + 86400 && o.end > ds
                })
                .unwrap_or(c0);
            let row = (c0..=c1).map(|c| slots[c]).max().unwrap_or(0);
            for slot in slots.iter_mut().take(c1 + 1).skip(c0) {
                *slot = row + 1;
            }
            let r = Rect::from_min_size(
                Pos2::new(arect.left() + gutter + col_w * c0 as f32 + 2.0, arect.top() + 4.0 + row as f32 * 22.0),
                vec2(col_w * (c1 - c0 + 1) as f32 - 4.0, 20.0),
            );
            let color = event_color(self, &o.event);
            let resp = ui.interact(r, Id::new(("allday", o.event.id, o.start)), Sense::click());
            ui.painter().rect_filled(r, CornerRadius::same(4), if resp.hovered() { color } else { mix(color, p.card, if p.dark { 0.4 } else { 0.2 }) });
            let g = ui.painter().layout_job(egui::text::LayoutJob {
                wrap: egui::text::TextWrapping::truncate_at_width(r.width() - 10.0),
                ..egui::text::LayoutJob::simple_singleline(o.event.title.clone(), font_semibold(12.0), Color32::WHITE)
            });
            ui.painter().galley(r.left_center() + vec2(6.0, -g.size().y / 2.0), g, Color32::WHITE);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                open = Some(((*o).clone(), r.right_bottom()));
            }
        }

        // Scrollable hour grid.
        let hour_h = match theme::density() {
            theme::Density::Compact => 42.0,
            theme::Density::Cozy => 52.0,
            theme::Density::Roomy => 60.0,
        };
        let work = (self.cfg.calendar.work_start, self.cfg.calendar.work_end);
        let mut scroll = ScrollArea::vertical().id_salt(("calgrid", format!("{:?}", self.cal.view))).auto_shrink([false, false]);
        if self.cal.scroll_to_now {
            let now = util::local(util::now_ts());
            let target_h = if days.contains(&today) { now.hour().saturating_sub(1) } else { work.0 };
            scroll = scroll.vertical_scroll_offset(target_h as f32 * hour_h);
            self.cal.scroll_to_now = false;
        }
        scroll.show(ui, |ui| {
            let (grid, _) = ui.allocate_exact_size(vec2(avail.width(), 24.0 * hour_h + 8.0), Sense::hover());
            let top = grid.top() + 4.0;
            for (i, d) in days.iter().enumerate() {
                let x = grid.left() + gutter + col_w * i as f32;
                let col = Rect::from_min_size(Pos2::new(x, top), vec2(col_w, 24.0 * hour_h));
                if !util::is_weekend(*d) {
                    let w = Rect::from_min_max(
                        Pos2::new(col.left(), top + work.0 as f32 * hour_h),
                        Pos2::new(col.right(), top + work.1 as f32 * hour_h),
                    );
                    ui.painter().rect_filled(w, 0.0, if p.dark { mix(p.card, Color32::WHITE, 0.025) } else { mix(p.card, p.accent, 0.03) });
                }
                ui.painter().vline(x, col.y_range(), Stroke::new(1.0, p.stroke));
                let resp = ui.interact(col, Id::new(("col", *d)), Sense::click());
                if resp.double_clicked() || (resp.clicked() && ui.input(|i| i.modifiers.command)) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let mins = (((pos.y - top) / hour_h * 60.0) / 30.0).floor() as u32 * 30;
                        new_at = Some(util::ts_from_date_time(*d, (mins / 60).min(23), mins % 60));
                    }
                } else if resp.clicked() {
                    self.cal.date = *d;
                }
                if resp.hovered() {
                    if let Some(pos) = resp.hover_pos() {
                        let slot = ((pos.y - top) / (hour_h / 2.0)).floor();
                        let r = Rect::from_min_size(Pos2::new(col.left() + 1.0, top + slot * hour_h / 2.0), vec2(col_w - 2.0, hour_h / 2.0));
                        ui.painter().rect_filled(r, CornerRadius::same(3), with_alpha(p.accent, 22));
                    }
                }
            }
            for h in 0..24 {
                let y = top + h as f32 * hour_h;
                ui.painter().hline(grid.left() + gutter - 6.0..=grid.right(), y, Stroke::new(1.0, p.stroke));
                ui.painter().hline(grid.left() + gutter..=grid.right(), y + hour_h / 2.0, Stroke::new(0.5, mix(p.stroke, p.card, 0.5)));
                if h > 0 {
                    ui.painter().text(Pos2::new(grid.left() + gutter - 10.0, y), Align2::RIGHT_CENTER, util::fmt_hour(h), font(11.5), p.text3);
                }
            }
            // Timed events with overlap columns.
            for (i, d) in days.iter().enumerate() {
                let ds = util::local_midnight(*d);
                let de = ds + 86400;
                let mut evs: Vec<&Occurrence> = occ
                    .iter()
                    .filter(|o| !(o.event.all_day || o.end - o.start >= 86400) && o.start < de && o.end > ds)
                    .collect();
                evs.sort_by_key(|o| (o.start, -(o.end - o.start)));
                let mut cols: Vec<i64> = Vec::new();
                let mut assigned: Vec<(usize, &Occurrence)> = Vec::new();
                let mut clusters: Vec<(usize, usize)> = Vec::new(); // (start index, columns)
                let mut cluster_end = i64::MIN;
                let mut cluster_start = 0;
                for o in evs {
                    if o.start >= cluster_end {
                        if !assigned.is_empty() {
                            clusters.push((cluster_start, cols.len()));
                        }
                        cols.clear();
                        cluster_start = assigned.len();
                    }
                    let c = cols.iter().position(|end| *end <= o.start).unwrap_or_else(|| {
                        cols.push(i64::MIN);
                        cols.len() - 1
                    });
                    cols[c] = o.end.max(o.start + 900);
                    cluster_end = cluster_end.max(o.end.max(o.start + 900));
                    assigned.push((c, o));
                }
                if !assigned.is_empty() {
                    clusters.push((cluster_start, cols.len()));
                }
                let x0 = grid.left() + gutter + col_w * i as f32;
                for (ci, (start_idx, ncols)) in clusters.iter().enumerate() {
                    let end_idx = clusters.get(ci + 1).map(|c| c.0).unwrap_or(assigned.len());
                    for (c, o) in &assigned[*start_idx..end_idx] {
                        let s = o.start.max(ds);
                        let e = o.end.min(de).max(s + 900);
                        let y0 = top + ((s - ds) as f32 / 3600.0) * hour_h;
                        let y1 = (top + ((e - ds) as f32 / 3600.0) * hour_h).max(y0 + 22.0);
                        let w = (col_w - 6.0) / (*ncols).max(1) as f32;
                        let r = Rect::from_min_max(
                            Pos2::new(x0 + 3.0 + w * *c as f32, y0 + 1.0),
                            Pos2::new(x0 + 3.0 + w * (*c as f32 + 1.0) - 2.0, y1 - 1.0),
                        );
                        let color = event_color(self, &o.event);
                        let resp = ui.interact(r, Id::new(("ev", o.event.id, o.start)), Sense::click());
                        let fill = if o.event.show_as == ShowAs::Tentative || o.event.show_as == ShowAs::Free {
                            mix(color, p.card, 0.88)
                        } else {
                            mix(color, p.card, if p.dark { 0.62 } else { 0.8 })
                        };
                        ui.painter().rect_filled(r, CornerRadius::same(4), if resp.hovered() { mix(fill, color, 0.15) } else { fill });
                        let bar = Rect::from_min_size(r.left_top(), vec2(4.0, r.height()));
                        ui.painter().rect_filled(bar, CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 }, color);
                        if o.event.show_as == ShowAs::Tentative {
                            ui.painter().rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, color), egui::StrokeKind::Inside);
                        }
                        let text_c = if p.dark { Color32::WHITE } else { p.text };
                        let clip = ui.painter().with_clip_rect(r.shrink(2.0));
                        let compact = r.height() < 30.0;
                        let label = if compact {
                            format!("{} {}", o.event.title, util::fmt_time(&util::local(o.start)))
                        } else {
                            o.event.title.clone()
                        };
                        let title = clip.layout_job(egui::text::LayoutJob {
                            wrap: egui::text::TextWrapping { max_width: r.width() - 12.0, max_rows: if r.height() > 44.0 { 2 } else { 1 }, break_anywhere: false, overflow_character: Some('…') },
                            ..egui::text::LayoutJob::simple(label, font_semibold(if compact { 11.5 } else { 12.5 }), text_c, r.width() - 12.0)
                        });
                        let th = title.size().y;
                        let ty = if compact { (r.height() - th) / 2.0 } else { 3.0 };
                        clip.galley(r.left_top() + vec2(9.0, ty), title, text_c);
                        if r.height() > th + 18.0 {
                            let mut sub = format!("{} – {}", util::fmt_time(&util::local(o.start)), util::fmt_time(&util::local(o.end)));
                            if !o.event.location.is_empty() {
                                sub = format!("{sub} · {}", o.event.location);
                            }
                            clip.text(r.left_top() + vec2(9.0, 5.0 + th), Align2::LEFT_TOP, sub, font(11.5), if p.dark { Color32::from_gray(220) } else { p.text2 });
                        }
                        if o.event.repeat != RepeatFreq::None && r.height() > 30.0 {
                            clip.text(r.right_top() + vec2(-6.0, 4.0), Align2::RIGHT_TOP, icons::REPEAT, icon_font(11.0), p.text3);
                        }
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            open = Some(((*o).clone(), r.right_top()));
                        }
                    }
                }
                // Now line.
                if *d == today {
                    let now = util::now_ts();
                    let y = top + ((now - ds) as f32 / 3600.0) * hour_h;
                    ui.painter().hline(x0..=x0 + col_w, y, Stroke::new(2.0, p.danger));
                    ui.painter().circle_filled(Pos2::new(x0, y), 5.0, p.danger);
                }
            }
        });
        if let Some(o) = open {
            self.cal.popup = Some(o);
        }
        if let Some(t) = new_at {
            self.cal_new_event(Some(t));
        }
    }

    fn event_popup(&mut self, ctx: &egui::Context) {
        let Some((occ, pos)) = self.cal.popup.clone() else { return };
        let p = pal();
        let mut close = false;
        let mut edit = false;
        let mut delete = false;
        let area = egui::Area::new(Id::new("event-popup"))
            .fixed_pos(pos + vec2(6.0, 0.0))
            .constrain(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).inner_margin(Margin::same(16)).show(ui, |ui| {
                    ui.set_width(320.0);
                    let e = &occ.event;
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(4.0, 22.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(2), event_color(self, e));
                        ui.add(egui::Label::new(RichText::new(&e.title).font(font_semibold(16.0))).wrap());
                    });
                    ui.add_space(6.0);
                    let s = util::local(occ.start);
                    let when = if e.all_day {
                        let days = ((occ.end - occ.start) / 86400).max(1);
                        if days > 1 {
                            format!("{} – {}", s.format("%a %b %-d"), util::local(occ.end - 1).format("%a %b %-d"))
                        } else {
                            format!("{} (all day)", s.format("%A, %B %-d"))
                        }
                    } else {
                        format!("{} {} – {}", s.format("%a %b %-d,"), util::fmt_time(&s), util::fmt_time(&util::local(occ.end)))
                    };
                    ui.label(RichText::new(format!("{}  {when}", icons::CLOCK)).font(icon_font(13.0)).color(p.text2));
                    if e.repeat != RepeatFreq::None {
                        ui.label(RichText::new(format!("{}  {}", icons::REPEAT, e.repeat.label())).font(icon_font(13.0)).color(p.text2));
                    }
                    if !e.location.is_empty() {
                        ui.label(RichText::new(format!("{}  {}", icons::MAP_PIN, e.location)).font(icon_font(13.0)).color(p.text2));
                    }
                    if let Some(m) = e.reminder_min {
                        ui.label(RichText::new(format!("{}  {} before", icons::BELL, reminder_label(m))).font(icon_font(13.0)).color(p.text2));
                    }
                    if !e.category.is_empty() {
                        widgets::chip(ui, &e.category, self.cfg.category_color(&e.category));
                    }
                    if !e.organizer.is_empty() {
                        ui.label(RichText::new(format!("Organizer: {}", e.organizer)).font(font(12.5)).color(p.text3));
                    }
                    if !e.description.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(util::truncate_chars(&e.description, 400)).font(font(13.0)));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if e.id > 0 {
                            if widgets::primary_button(ui, Some(icons::PENCIL_SIMPLE), "Edit").clicked() {
                                edit = true;
                            }
                            if widgets::secondary_button(ui, Some(icons::TRASH), "Delete").clicked() {
                                delete = true;
                            }
                        }
                        if widgets::secondary_button(ui, None, "Close").clicked() {
                            close = true;
                        }
                    });
                });
            });
        if area.response.clicked_elsewhere() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if edit {
            if let Ok(Some(ev)) = self.store.event(occ.event.id) {
                self.cal.editor = Some(EventEditor::from_event(ev, false));
            }
            close = true;
        }
        if delete {
            let _ = self.store.delete_event(occ.event.id);
            self.cal.dirty = true;
            self.toast("Event deleted", ToastKind::Info);
            close = true;
        }
        if close {
            self.cal.popup = None;
        }
    }

    fn event_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.cal.editor.take() else { return };
        let p = pal();
        let mut keep = true;
        let mut save = false;
        let mut invite = false;
        let mut delete = false;
        let modal = Modal::new(Id::new("event-editor")).frame(
            Frame::NONE
                .fill(p.card)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(22))
                .shadow(crate::theme::window_shadow()),
        );
        let resp = modal.show(ctx, |ui| {
            ui.set_width(560.0);
            ui.horizontal(|ui| {
                widgets::heading(ui, if ed.is_new { "New event" } else { "Edit event" }, 18.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icons::X, "Close").clicked() {
                        keep = false;
                    }
                });
            });
            ui.add_space(10.0);
            let title = ui.add(
                TextEdit::singleline(&mut ed.ev.title)
                    .hint_text(RichText::new("Add a title").color(p.text3))
                    .font(font_semibold(18.0))
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(8, 6)),
            );
            if ed.is_new && ed.ev.title.is_empty() && !title.has_focus() && ui.memory(|m| m.focused().is_none()) {
                title.request_focus();
            }
            ui.add_space(10.0);
            egui::Grid::new("event-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new(icons::CLOCK).font(icon_font(16.0)).color(p.text2));
                ui.horizontal(|ui| {
                    if date_button(ui, ed.start_date).clicked() {
                        ed.picker = Some(0);
                        ed.picker_month = ed.start_date;
                    }
                    if !ed.ev.all_day {
                        let before = ed.start_min;
                        time_combo(ui, Id::new("start-time"), &mut ed.start_min);
                        if before != ed.start_min && ed.start_date == ed.end_date {
                            let dur = ed.end_min as i64 - before as i64;
                            ed.end_min = (ed.start_min as i64 + dur.max(15)).clamp(0, 24 * 60 - 15) as u32;
                        }
                    }
                    ui.label(RichText::new("to").color(p.text3));
                    if date_button(ui, ed.end_date).clicked() {
                        ed.picker = Some(1);
                        ed.picker_month = ed.end_date;
                    }
                    if !ed.ev.all_day {
                        time_combo(ui, Id::new("end-time"), &mut ed.end_min);
                    }
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    widgets::toggle(ui, &mut ed.ev.all_day);
                    ui.label("All day");
                    ui.add_space(16.0);
                    ui.label(RichText::new(icons::REPEAT).font(icon_font(15.0)).color(p.text2));
                    egui::ComboBox::from_id_salt("repeat")
                        .selected_text(ed.ev.repeat.label())
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for r in RepeatFreq::ALL {
                                ui.selectable_value(&mut ed.ev.repeat, r, r.label());
                            }
                        });
                });
                ui.end_row();
                ui.label(RichText::new(icons::MAP_PIN).font(icon_font(16.0)).color(p.text2));
                widgets::text_input(ui, &mut ed.ev.location, "Add a location", 470.0);
                ui.end_row();
                ui.label(RichText::new(icons::BELL).font(icon_font(16.0)).color(p.text2));
                ui.horizontal(|ui| {
                    let label = ed.ev.reminder_min.map(|m| format!("{} before", reminder_label(m))).unwrap_or_else(|| "No reminder".into());
                    egui::ComboBox::from_id_salt("reminder").selected_text(label).width(150.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut ed.ev.reminder_min, None, "No reminder");
                        for m in [0i64, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080] {
                            ui.selectable_value(&mut ed.ev.reminder_min, Some(m), format!("{} before", reminder_label(m)));
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Show as").color(p.text3));
                    egui::ComboBox::from_id_salt("showas").selected_text(ed.ev.show_as.label()).width(110.0).show_ui(ui, |ui| {
                        for s in ShowAs::ALL {
                            ui.selectable_value(&mut ed.ev.show_as, s, s.label());
                        }
                    });
                });
                ui.end_row();
                ui.label(RichText::new(icons::TAG).font(icon_font(16.0)).color(p.text2));
                let cats: Vec<String> = self.cfg.categories.iter().map(|c| c.name.clone()).collect();
                egui::ComboBox::from_id_salt("category")
                    .selected_text(if ed.ev.category.is_empty() { "No category".to_string() } else { ed.ev.category.clone() })
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut ed.ev.category, String::new(), "No category");
                        for c in cats {
                            ui.selectable_value(&mut ed.ev.category, c.clone(), c);
                        }
                    });
                ui.end_row();
                ui.label(RichText::new(icons::USERS).font(icon_font(16.0)).color(p.text2));
                widgets::text_input(ui, &mut ed.ev.attendees, "Invite attendees (e-mail addresses, comma separated)", 470.0);
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.add(
                TextEdit::multiline(&mut ed.ev.description)
                    .hint_text(RichText::new("Add a description").color(p.text3))
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::same(8)),
            );
            if let Some(e) = &ed.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).color(p.danger));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, Some(icons::CHECK), "Save").clicked() {
                    save = true;
                }
                if !ed.ev.attendees.trim().is_empty() && widgets::secondary_button(ui, Some(icons::PAPER_PLANE_TILT), "Save & send invitation").clicked() {
                    save = true;
                    invite = true;
                }
                if widgets::secondary_button(ui, None, "Cancel").clicked() {
                    keep = false;
                }
                if !ed.is_new {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::subtle_button(ui, icons::TRASH, "Delete").clicked() {
                            delete = true;
                        }
                    });
                }
            });
        });
        if resp.should_close() && ed.picker.is_none() {
            keep = false;
        }
        // Date picker popup.
        if let Some(which) = ed.picker {
            let mut done = false;
            egui::Window::new("Pick a date")
                .collapsible(false)
                .resizable(false)
                .title_bar(false)
                .order(egui::Order::Tooltip)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .frame(Frame::popup(&ctx.global_style()).inner_margin(Margin::same(14)))
                .show(ctx, |ui| {
                    ui.set_width(260.0);
                    let sel = if which == 0 { ed.start_date } else { ed.end_date };
                    let mut month = ed.picker_month;
                    if let Some(d) = mini_month(ui, &mut month, sel, &HashSet::new(), Id::new("picker")) {
                        if which == 0 {
                            let delta = d - ed.start_date;
                            ed.start_date = d;
                            ed.end_date += delta;
                        } else {
                            ed.end_date = d;
                        }
                        done = true;
                    }
                    ed.picker_month = month;
                    if widgets::secondary_button(ui, None, "Cancel").clicked() {
                        done = true;
                    }
                });
            if done {
                ed.picker = None;
            }
        }
        if delete && ed.ev.id > 0 {
            let _ = self.store.delete_event(ed.ev.id);
            self.cal.dirty = true;
            self.toast("Event deleted", ToastKind::Info);
            keep = false;
        }
        if save {
            if ed.ev.title.trim().is_empty() {
                ed.ev.title = "(No title)".into();
            }
            if ed.ev.all_day {
                ed.ev.start = util::local_midnight(ed.start_date);
                ed.ev.end = util::local_midnight(ed.end_date.max(ed.start_date) + Duration::days(1));
            } else {
                ed.ev.start = util::ts_from_date_time(ed.start_date, ed.start_min / 60, ed.start_min % 60);
                ed.ev.end = util::ts_from_date_time(ed.end_date, ed.end_min / 60, ed.end_min % 60);
            }
            if ed.ev.end <= ed.ev.start && !ed.ev.all_day {
                ed.error = Some("The event must end after it starts.".into());
            } else {
                match self.store.save_event(&ed.ev) {
                    Ok(id) => {
                        ed.ev.id = id;
                        if let Ok(Some(saved)) = self.store.event(id) {
                            ed.ev = saved;
                        }
                        self.cal.dirty = true;
                        self.cal.date = ed.start_date;
                        self.toast(if ed.is_new { "Event created" } else { "Event updated" }, ToastKind::Success);
                        if invite {
                            self.send_invitation(&ed.ev);
                        }
                        keep = false;
                    }
                    Err(e) => ed.error = Some(format!("Couldn't save: {e}")),
                }
            }
        }
        if keep && self.cal.editor.is_none() {
            self.cal.editor = Some(ed);
        }
    }

    fn send_invitation(&mut self, ev: &CalEvent) {
        let Some(acc) = self.cfg.default_account().cloned() else { return };
        let attendees = Addr::parse_list(&ev.attendees);
        let mut lines = vec![
            "BEGIN:VCALENDAR".to_string(),
            "VERSION:2.0".to_string(),
            "PRODID:-//RustMail//EN".to_string(),
            "METHOD:REQUEST".to_string(),
        ];
        let body_ics = cal::to_ics(std::slice::from_ref(ev));
        // Insert organizer/attendees into the VEVENT produced by to_ics.
        let mut in_event = false;
        for l in body_ics.lines() {
            let l = l.trim_end_matches('\r');
            if l == "BEGIN:VEVENT" {
                in_event = true;
            }
            if in_event && l == "END:VEVENT" {
                lines.push(format!("ORGANIZER;CN=\"{}\":mailto:{}", acc.display_name.replace('"', ""), acc.email));
                for a in &attendees {
                    lines.push(format!("ATTENDEE;CN=\"{}\";RSVP=TRUE:mailto:{}", a.display().replace('"', ""), a.email));
                }
                lines.push(l.to_string());
                in_event = false;
                continue;
            }
            if in_event || l.starts_with(' ') {
                lines.push(l.to_string());
            }
        }
        lines.push("END:VCALENDAR".into());
        let ics = lines.join("\r\n") + "\r\n";
        let s = util::local(ev.start);
        let when = if ev.all_day {
            util::fmt_long_date(s.date_naive())
        } else {
            format!("{} {} – {}", util::fmt_long_date(s.date_naive()), util::fmt_time(&s), util::fmt_time(&util::local(ev.end)))
        };
        let id = self.next_id();
        let mut c = crate::ui::compose::Compose::new(id, &acc.id, true);
        c.to = attendees;
        c.subject = format!("Invitation: {} @ {}", ev.title, when);
        c.body = format!(
            "You're invited to **{}**\n\n- When: {}\n{}{}",
            ev.title,
            when,
            if ev.location.is_empty() { String::new() } else { format!("- Where: {}\n", ev.location) },
            if ev.description.is_empty() { String::new() } else { format!("\n{}\n", ev.description) }
        );
        c.attachments.push(mime::OutAttachment {
            name: "invite.ics".into(),
            mime: "text/calendar; method=REQUEST".into(),
            data: ics.into_bytes(),
        });
        c.mark_clean();
        c.popped_out = true;
        self.composes.push(c);
    }
}

fn reminder_label(m: i64) -> String {
    match m {
        0 => "At start".into(),
        m if m < 60 => format!("{m} minutes"),
        60 => "1 hour".into(),
        m if m < 1440 => format!("{} hours", m / 60),
        1440 => "1 day".into(),
        m if m < 10080 => format!("{} days", m / 1440),
        _ => "1 week".into(),
    }
}

fn date_button(ui: &mut Ui, d: NaiveDate) -> egui::Response {
    widgets::secondary_button(ui, Some(icons::CALENDAR_BLANK), &d.format("%a %b %-d, %Y").to_string())
}

fn calendar_toggle(ui: &mut Ui, on: &mut bool, label: &str, color: Color32) {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
    }
    let b = Rect::from_center_size(Pos2::new(rect.left() + 16.0, rect.center().y), vec2(16.0, 16.0));
    if *on {
        ui.painter().rect_filled(b, CornerRadius::same(3), color);
        ui.painter().text(b.center(), Align2::CENTER_CENTER, icons::CHECK, icon_fill_font(12.0), Color32::WHITE);
    } else {
        ui.painter().rect_stroke(b, CornerRadius::same(3), Stroke::new(1.5, color), egui::StrokeKind::Inside);
    }
    ui.painter().text(Pos2::new(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, label, font(14.0), p.text);
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        *on = !*on;
    }
}
