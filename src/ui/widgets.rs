//! Fluent-style building blocks drawn with egui primitives.

use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Pos2, Rect,
    Response, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui,
};

use crate::icons;
use crate::theme::{self, font, font_semibold, icon_fill_font, icon_font, mix, pal, with_alpha};
use crate::util;

/// Card surface used by the list and reading panes.
pub fn card() -> Frame {
    let p = pal();
    Frame::NONE
        .fill(p.card)
        .corner_radius(CornerRadius::same(8))
        .stroke(Stroke::new(1.0, if p.dark { p.stroke } else { mix(p.stroke, p.card, 0.4) }))
}

pub fn paint_avatar(painter: &egui::Painter, center: Pos2, radius: f32, name: &str, email: &str) {
    let seed = if email.is_empty() { name } else { email };
    let bg = theme::color_for(seed);
    painter.circle_filled(center, radius, bg);
    let text = util::initials(name, email);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        text,
        font_semibold(radius * 0.78),
        Color32::WHITE,
    );
}

pub fn avatar(ui: &mut Ui, size: f32, name: &str, email: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_avatar(ui.painter(), rect.center(), size / 2.0, name, email);
    resp
}

/// Square, frameless icon button with a hover background.
pub fn icon_button_ex(
    ui: &mut Ui,
    icon: &str,
    size: f32,
    icon_size: f32,
    tooltip: &str,
    active: bool,
    color: Option<Color32>,
) -> Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let bg = if resp.is_pointer_button_down_on() {
            p.pressed
        } else if resp.hovered() || active {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        let fg = color.unwrap_or(if resp.hovered() || active { p.text } else { p.text2 });
        let f = if active { icon_fill_font(icon_size) } else { icon_font(icon_size) };
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, icon, f, fg);
    }
    if tooltip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tooltip)
    }
}

pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    icon_button_ex(ui, icon, 30.0, 17.0, tooltip, false, None)
}

/// Ribbon command: icon (accent coloured) plus optional label.
pub fn ribbon_button(ui: &mut Ui, icon: &str, label: &str, show_label: bool, enabled: bool) -> Response {
    ribbon_button_ex(ui, icon, label, show_label, enabled, false, false)
}

pub fn ribbon_button_ex(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    show_label: bool,
    enabled: bool,
    caret: bool,
    active: bool,
) -> Response {
    let p = pal();
    let h = 32.0;
    let icon_f = icon_font(18.0);
    let label_galley = if show_label {
        Some(ui.painter().layout_no_wrap(label.to_string(), font(14.0), p.text))
    } else {
        None
    };
    let mut w = 10.0 + 18.0 + 10.0;
    if let Some(g) = &label_galley {
        w += 6.0 + g.size().x - 4.0;
    }
    if caret {
        w += 14.0;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };
    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            Color32::TRANSPARENT
        } else if resp.is_pointer_button_down_on() {
            p.pressed
        } else if resp.hovered() || active {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        let icon_color = if !enabled {
            p.disabled
        } else if active {
            p.accent
        } else {
            p.accent_text
        };
        let x = rect.left() + 10.0 + 9.0;
        ui.painter().text(
            Pos2::new(x, rect.center().y),
            Align2::CENTER_CENTER,
            icon,
            if active { icon_fill_font(18.0) } else { icon_f },
            icon_color,
        );
        let mut tx = x + 9.0 + 6.0;
        if let Some(g) = label_galley {
            let color = if enabled { p.text } else { p.disabled };
            let pos = Pos2::new(tx, rect.center().y - g.size().y / 2.0);
            tx += g.size().x;
            ui.painter().galley(pos, g, color);
        }
        if caret {
            ui.painter().text(
                Pos2::new(tx + 8.0, rect.center().y + 1.0),
                Align2::CENTER_CENTER,
                icons::CARET_DOWN,
                icon_font(11.0),
                if enabled { p.text2 } else { p.disabled },
            );
        }
    }
    if show_label {
        resp
    } else {
        resp.on_hover_text(label)
    }
}

pub fn ribbon_separator(ui: &mut Ui) {
    let p = pal();
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 32.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range().shrink(7.0),
        Stroke::new(1.0, p.stroke),
    );
}

/// Ribbon dropdown: a button that opens a menu below itself.
pub fn ribbon_menu(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    show_label: bool,
    enabled: bool,
    add_contents: impl FnOnce(&mut Ui),
) -> Response {
    let resp = ribbon_button_ex(ui, icon, label, show_label, enabled, true, false);
    if enabled {
        egui::Popup::menu(&resp)
            .width(220.0)
            .show(|ui| {
                ui.set_min_width(200.0);
                add_contents(ui)
            });
    }
    resp
}

/// Filled accent button (primary action).
pub fn primary_button(ui: &mut Ui, icon: Option<&str>, label: &str) -> Response {
    primary_button_ex(ui, icon, label, true)
}

pub fn primary_button_ex(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool) -> Response {
    let p = pal();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_semibold(14.0), p.on_accent);
    let icon_w = if icon.is_some() { 24.0 } else { 0.0 };
    let w = 16.0 + icon_w + galley.size().x + 16.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w.max(80.0), 32.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            p.disabled
        } else if resp.is_pointer_button_down_on() {
            p.accent_pressed
        } else if resp.hovered() {
            p.accent_hover
        } else {
            p.accent
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        let content_w = icon_w + galley.size().x;
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(ic) = icon {
            ui.painter().text(
                Pos2::new(x + 8.0, rect.center().y),
                Align2::CENTER_CENTER,
                ic,
                icon_font(17.0),
                p.on_accent,
            );
            x += icon_w;
        }
        ui.painter().galley(
            Pos2::new(x, rect.center().y - galley.size().y / 2.0),
            galley,
            p.on_accent,
        );
    }
    resp
}

/// Outlined neutral button.
pub fn secondary_button(ui: &mut Ui, icon: Option<&str>, label: &str) -> Response {
    secondary_button_ex(ui, icon, label, true)
}

pub fn secondary_button_ex(ui: &mut Ui, icon: Option<&str>, label: &str, enabled: bool) -> Response {
    let p = pal();
    let color = if enabled { p.text } else { p.disabled };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font_semibold(14.0), color);
    let icon_w = if icon.is_some() { 24.0 } else { 0.0 };
    let w = 14.0 + icon_w + galley.size().x + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w.max(64.0), 32.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            p.card
        } else if resp.is_pointer_button_down_on() {
            p.pressed
        } else if resp.hovered() {
            p.hover
        } else {
            p.card
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, p.stroke_strong),
            StrokeKind::Inside,
        );
        let content_w = icon_w + galley.size().x;
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(ic) = icon {
            ui.painter().text(
                Pos2::new(x + 8.0, rect.center().y),
                Align2::CENTER_CENTER,
                ic,
                icon_font(17.0),
                if enabled { p.accent_text } else { p.disabled },
            );
            x += icon_w;
        }
        ui.painter()
            .galley(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, color);
    }
    resp
}

/// Transparent text+icon button (links, toolbar actions inside panes).
pub fn subtle_button(ui: &mut Ui, icon: &str, label: &str) -> Response {
    let p = pal();
    let galley = ui.painter().layout_no_wrap(label.to_string(), font(14.0), p.text);
    let icon_w = if icon.is_empty() { 0.0 } else { 22.0 };
    let w = 8.0 + icon_w + galley.size().x + 8.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let bg = if resp.is_pointer_button_down_on() {
            p.pressed
        } else if resp.hovered() {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        let mut x = rect.left() + 8.0;
        if !icon.is_empty() {
            ui.painter().text(
                Pos2::new(x + 8.0, rect.center().y),
                Align2::CENTER_CENTER,
                icon,
                icon_font(16.0),
                p.accent_text,
            );
            x += icon_w;
        }
        ui.painter()
            .galley(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, p.text);
    }
    resp
}

/// Text link styled with the accent colour.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let p = pal();
    let resp = ui.add(
        egui::Label::new(RichText::new(text).color(p.accent_text))
            .sense(Sense::click())
            .selectable(false),
    );
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.hovered() {
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom() - 1.0, Stroke::new(1.0, p.accent_text));
    }
    resp
}

/// Underlined tab strip ("Focused | Other", "Home | View | Help").
pub fn tabs(ui: &mut Ui, items: &[&str], selected: &mut usize, size: f32) -> bool {
    let p = pal();
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in items.iter().enumerate() {
            let is_sel = *selected == i;
            let f = if is_sel { font_semibold(size) } else { font(size) };
            let galley = ui.painter().layout_no_wrap(label.to_string(), f, p.text);
            let (rect, resp) =
                ui.allocate_exact_size(vec2(galley.size().x + 16.0, size + 18.0), Sense::click());
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            if resp.hovered() && !is_sel {
                ui.painter()
                    .rect_filled(rect.shrink2(vec2(0.0, 3.0)), CornerRadius::same(4), p.hover);
            }
            let color = if is_sel { p.text } else { p.text2 };
            ui.painter().galley(
                Pos2::new(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0 - 1.0),
                galley,
                color,
            );
            if is_sel {
                let bar = Rect::from_min_size(
                    Pos2::new(rect.left() + 8.0, rect.bottom() - 3.0),
                    vec2(rect.width() - 16.0, 3.0),
                );
                ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
            }
            if resp.clicked() && !is_sel {
                *selected = i;
                changed = true;
            }
        }
    });
    changed
}

/// Rounded search field with magnifier and clear button.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32, id: egui::Id) -> Response {
    let p = pal();
    let height = 32.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let focused = ui.memory(|m| m.has_focus(id));
    let fill = if focused { p.card } else if p.dark { p.card } else { mix(p.card, p.bg, 0.25) };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, if focused { p.accent } else { p.stroke }),
        StrokeKind::Inside,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 18.0, rect.center().y),
        Align2::CENTER_CENTER,
        icons::MAGNIFYING_GLASS,
        icon_font(16.0),
        if focused { p.accent_text } else { p.text2 },
    );
    let has_text = !text.is_empty();
    let edit_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 34.0, rect.top() + 4.0),
        Pos2::new(rect.right() - if has_text { 32.0 } else { 8.0 }, rect.bottom() - 4.0),
    );
    let resp = ui
        .scope_builder(egui::UiBuilder::new().max_rect(edit_rect), |ui| {
            ui.add(
                TextEdit::singleline(text)
                    .id(id)
                    .hint_text(RichText::new(hint).color(p.text3))
                    .frame(egui::Frame::NONE)
                    .desired_width(edit_rect.width())
                    .margin(Margin::symmetric(0, 3))
                    .font(font(14.0)),
            )
        })
        .inner;
    if has_text {
        let clear = Rect::from_center_size(Pos2::new(rect.right() - 17.0, rect.center().y), vec2(24.0, 24.0));
        let r = ui.interact(clear, id.with("clear"), Sense::click());
        if r.hovered() {
            ui.painter().rect_filled(clear, CornerRadius::same(4), p.hover);
        }
        ui.painter().text(
            clear.center(),
            Align2::CENTER_CENTER,
            icons::X,
            icon_font(14.0),
            p.text2,
        );
        if r.clicked() {
            text.clear();
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
    resp
}

/// Fluent toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = pal();
    let (rect, mut resp) = ui.allocate_exact_size(vec2(40.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let radius = rect.height() / 2.0;
    let (bg, stroke) = if *on {
        (if resp.hovered() { p.accent_hover } else { p.accent }, Stroke::NONE)
    } else {
        (p.card, Stroke::new(1.0, if resp.hovered() { p.text2 } else { p.text3 }))
    };
    ui.painter().rect_filled(rect, CornerRadius::same(radius as u8), bg);
    if stroke != Stroke::NONE {
        ui.painter().rect_stroke(rect, CornerRadius::same(radius as u8), stroke, StrokeKind::Inside);
    }
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter().circle_filled(
        Pos2::new(x, rect.center().y),
        radius - 5.0,
        if *on { p.on_accent } else { p.text2 },
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A settings row: label (+ description) on the left, control on the right.
pub fn setting_row(ui: &mut Ui, title: &str, desc: &str, add_control: impl FnOnce(&mut Ui)) {
    let p = pal();
    ui.horizontal(|ui| {
        ui.set_min_height(40.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(font(14.0)).color(p.text));
            if !desc.is_empty() {
                ui.label(RichText::new(desc).font(font(12.0)).color(p.text3));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), add_control);
    });
}

pub fn heading(ui: &mut Ui, text: &str, size: f32) {
    ui.label(RichText::new(text).font(font_semibold(size)).color(pal().text));
}

/// Small coloured pill (categories, labels).
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let p = pal();
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(12.0), p.text);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 22.0, 20.0), Sense::click());
    let bg = if p.dark { mix(p.card, color, 0.35) } else { mix(Color32::WHITE, color, 0.22) };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    ui.painter()
        .circle_filled(Pos2::new(rect.left() + 9.0, rect.center().y), 4.0, color);
    ui.painter().galley(
        Pos2::new(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    resp
}

pub fn paint_badge(painter: &egui::Painter, right_center: Pos2, count: i64, strong: bool) -> f32 {
    if count <= 0 {
        return 0.0;
    }
    let p = pal();
    let text = if count > 999 { "999+".to_string() } else { count.to_string() };
    let color = if strong { p.accent_text } else { p.text3 };
    let g = painter.layout_no_wrap(text, font_semibold(12.5), color);
    let w = g.size().x;
    painter.galley(
        Pos2::new(right_center.x - w, right_center.y - g.size().y / 2.0),
        g,
        color,
    );
    w
}

/// Round checkbox used by To Do (and the message list selection mode).
pub fn round_check(ui: &mut Ui, checked: bool, size: f32, color: Color32) -> Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    paint_round_check(ui.painter(), rect.center(), size / 2.0 - 1.0, checked, resp.hovered(), color, p.text2);
    resp
}

pub fn paint_round_check(
    painter: &egui::Painter,
    center: Pos2,
    r: f32,
    checked: bool,
    hovered: bool,
    color: Color32,
    ring: Color32,
) {
    if checked {
        painter.circle_filled(center, r, color);
        painter.text(center, Align2::CENTER_CENTER, icons::CHECK, icon_font(r * 1.15), Color32::WHITE);
    } else {
        painter.circle_stroke(center, r, Stroke::new(1.5, if hovered { color } else { ring }));
        if hovered {
            painter.text(center, Align2::CENTER_CENTER, icons::CHECK, icon_font(r * 1.05), color);
        }
    }
}

/// Centered illustration for empty panes.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, subtitle: &str) {
    let p = pal();
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(20.0));
        let (rect, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 46.0, p.accent_soft);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            icon_fill_font(44.0),
            with_alpha(p.accent, 200),
        );
        ui.add_space(14.0);
        ui.label(RichText::new(title).font(font_semibold(17.0)).color(p.text));
        if !subtitle.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new(subtitle).font(font(13.0)).color(p.text3));
        }
    });
}

/// Context-menu entry with an icon.
pub fn menu_item(ui: &mut Ui, icon: &str, label: &str) -> Response {
    menu_item_ex(ui, icon, label, true)
}

pub fn menu_item_ex(ui: &mut Ui, icon: &str, label: &str, enabled: bool) -> Response {
    let p = pal();
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 30.0), if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        if enabled && resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
        }
        let color = if enabled { p.text } else { p.disabled };
        if !icon.is_empty() {
            ui.painter().text(
                Pos2::new(rect.left() + 16.0, rect.center().y),
                Align2::CENTER_CENTER,
                icon,
                icon_font(16.0),
                if enabled { p.accent_text } else { p.disabled },
            );
        }
        ui.painter().text(
            Pos2::new(rect.left() + 34.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font(14.0),
            color,
        );
    }
    let clicked = enabled && resp.clicked();
    if clicked {
        ui.close();
    }
    resp
}

/// Text input with Fluent styling (bottom accent when focused).
pub fn text_input(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    text_input_ex(ui, text, hint, width, false)
}

pub fn text_input_ex(ui: &mut Ui, text: &mut String, hint: &str, width: f32, password: bool) -> Response {
    let p = pal();
    let resp = ui.add(
        TextEdit::singleline(text)
            .hint_text(RichText::new(hint).color(p.text3))
            .desired_width(width)
            .margin(Margin::symmetric(8, 6))
            .password(password)
            .font(font(14.0)),
    );
    if resp.has_focus() {
        let r = resp.rect;
        ui.painter().hline(
            r.x_range().shrink(1.0),
            r.bottom() - 1.0,
            Stroke::new(2.0, p.accent),
        );
    }
    resp
}

pub fn spinner(ui: &mut Ui, size: f32) {
    ui.add(egui::Spinner::new().size(size).color(pal().accent));
}

/// Info / warning banner inside the reading pane.
pub fn banner(ui: &mut Ui, icon: &str, text: &str, tone: Color32, bg: Color32, add_actions: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icon).font(icon_font(16.0)).color(tone));
                ui.label(RichText::new(text).font(font(13.0)).color(pal().text));
                add_actions(ui);
            });
        });
}

