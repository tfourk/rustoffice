//! Fluent-inspired visual language: palettes, accent colours, density and the
//! egui `Style` derived from them.

use std::sync::RwLock;

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, Style, TextStyle, Vec2,
    Visuals,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "Use system setting",
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Density {
    Compact,
    #[default]
    Cozy,
    Roomy,
}

impl Density {
    pub const ALL: [Density; 3] = [Density::Roomy, Density::Cozy, Density::Compact];
    pub fn label(self) -> &'static str {
        match self {
            Density::Compact => "Compact",
            Density::Cozy => "Cozy",
            Density::Roomy => "Roomy",
        }
    }
    /// Extra vertical padding applied to list rows.
    pub fn pad(self) -> f32 {
        match self {
            Density::Compact => 4.0,
            Density::Cozy => 8.0,
            Density::Roomy => 12.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Accent {
    #[default]
    Blue,
    Teal,
    Green,
    Purple,
    Pink,
    Red,
    Orange,
    Graphite,
}

impl Accent {
    pub const ALL: [Accent; 8] = [
        Accent::Blue,
        Accent::Teal,
        Accent::Green,
        Accent::Purple,
        Accent::Pink,
        Accent::Red,
        Accent::Orange,
        Accent::Graphite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Teal => "Teal",
            Accent::Green => "Green",
            Accent::Purple => "Purple",
            Accent::Pink => "Pink",
            Accent::Red => "Red",
            Accent::Orange => "Orange",
            Accent::Graphite => "Graphite",
        }
    }

    pub fn base(self) -> Color32 {
        match self {
            Accent::Blue => hex(0x0F6CBD),
            Accent::Teal => hex(0x03787C),
            Accent::Green => hex(0x107C41),
            Accent::Purple => hex(0x6B3FA0),
            Accent::Pink => hex(0xAD2D8C),
            Accent::Red => hex(0xC4314B),
            Accent::Orange => hex(0xBC4B09),
            Accent::Graphite => hex(0x4A5868),
        }
    }
}

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Linear blend between two colours (`t = 0` gives `a`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

pub fn with_alpha(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub card: Color32,
    pub card_alt: Color32,
    pub sunken: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub stroke: Color32,
    pub stroke_strong: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub disabled: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    pub on_accent: Color32,
    pub accent_text: Color32,
    pub selected: Color32,
    pub selected_hover: Color32,
    pub accent_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub flag: Color32,
    pub flagged_row: Color32,
    pub pinned_row: Color32,
    pub shadow: Color32,
}

impl Palette {
    pub fn new(dark: bool, accent: Accent) -> Self {
        let base = accent.base();
        if !dark {
            let card = hex(0xFFFFFF);
            Palette {
                dark,
                bg: hex(0xF0F0F0),
                card,
                card_alt: hex(0xFAFAFA),
                sunken: hex(0xF5F5F5),
                hover: hex(0xF0F0F0),
                pressed: hex(0xE0E0E0),
                stroke: hex(0xE0E0E0),
                stroke_strong: hex(0xC7C7C7),
                text: hex(0x242424),
                text2: hex(0x424242),
                text3: hex(0x616161),
                disabled: hex(0xA6A6A6),
                accent: base,
                accent_hover: mix(base, Color32::BLACK, 0.12),
                accent_pressed: mix(base, Color32::BLACK, 0.25),
                on_accent: Color32::WHITE,
                accent_text: base,
                selected: mix(base, card, 0.84),
                selected_hover: mix(base, card, 0.76),
                accent_soft: mix(base, card, 0.92),
                danger: hex(0xC50F1F),
                danger_soft: hex(0xFDF3F4),
                warning: hex(0x8A5300),
                warning_soft: hex(0xFFF9E5),
                success: hex(0x107C10),
                success_soft: hex(0xF1FAF1),
                flag: hex(0xC50F1F),
                flagged_row: hex(0xFFF8E6),
                pinned_row: mix(base, card, 0.95),
                shadow: Color32::from_black_alpha(28),
            }
        } else {
            let card = hex(0x292929);
            let acc = mix(base, Color32::WHITE, 0.12);
            Palette {
                dark,
                bg: hex(0x1A1A1A),
                card,
                card_alt: hex(0x2E2E2E),
                sunken: hex(0x1F1F1F),
                hover: hex(0x333333),
                pressed: hex(0x3D3D3D),
                stroke: hex(0x3D3D3D),
                stroke_strong: hex(0x5C5C5C),
                text: hex(0xFFFFFF),
                text2: hex(0xD6D6D6),
                text3: hex(0xADADAD),
                disabled: hex(0x6E6E6E),
                accent: acc,
                accent_hover: mix(acc, Color32::WHITE, 0.12),
                accent_pressed: mix(acc, Color32::BLACK, 0.2),
                on_accent: Color32::WHITE,
                accent_text: mix(base, Color32::WHITE, 0.45),
                selected: mix(card, base, 0.35),
                selected_hover: mix(card, base, 0.45),
                accent_soft: mix(card, base, 0.18),
                danger: hex(0xF1707B),
                danger_soft: hex(0x3B2326),
                warning: hex(0xF4BF4F),
                warning_soft: hex(0x3A3220),
                success: hex(0x54B054),
                success_soft: hex(0x223322),
                flag: hex(0xF1707B),
                flagged_row: hex(0x3A3422),
                pinned_row: mix(card, base, 0.10),
                shadow: Color32::from_black_alpha(90),
            }
        }
    }
}

static PALETTE: RwLock<Option<Palette>> = RwLock::new(None);
static DENSITY: RwLock<Density> = RwLock::new(Density::Cozy);

/// The palette currently in effect.
pub fn pal() -> Palette {
    PALETTE
        .read()
        .ok()
        .and_then(|p| *p)
        .unwrap_or_else(|| Palette::new(false, Accent::Blue))
}

pub fn density() -> Density {
    DENSITY.read().map(|d| *d).unwrap_or_default()
}

pub const SEMIBOLD: &str = "semibold";
pub const BOLD: &str = "bold";
pub const ICONS_FILL: &str = "icons_fill";

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn font_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
pub fn font_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn icon_fill_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS_FILL.into()))
}

/// Install the palette and derive egui's style from it.
pub fn apply(ctx: &egui::Context, dark: bool, accent: Accent, dens: Density) {
    let p = Palette::new(dark, accent);
    if let Ok(mut w) = PALETTE.write() {
        *w = Some(p);
    }
    if let Ok(mut w) = DENSITY.write() {
        *w = dens;
    }
    let style = build_style(&p);
    // Same style for both egui themes; we choose light/dark ourselves.
    ctx.set_style_of(egui::Theme::Light, style.clone());
    ctx.set_style_of(egui::Theme::Dark, style);
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
}

fn build_style(p: &Palette) -> Style {
    let mut style = Style::default();
    style.text_styles = [
        (TextStyle::Small, font(12.0)),
        (TextStyle::Body, font(14.0)),
        (TextStyle::Button, font(14.0)),
        (TextStyle::Heading, font_semibold(20.0)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 6.0);
    s.button_padding = Vec2::new(10.0, 5.0);
    s.interact_size = Vec2::new(24.0, 30.0);
    s.window_margin = Margin::same(16);
    s.menu_margin = Margin::same(6);
    s.indent = 18.0;
    s.combo_height = 320.0;
    s.text_edit_width = 260.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.tooltip_width = 360.0;
    s.menu_width = 220.0;
    s.scroll = egui::style::ScrollStyle::floating();
    s.scroll.bar_width = 8.0;
    s.scroll.floating_allocated_width = 0.0;

    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.dark_mode = p.dark;
    v.override_text_color = None;
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: p.shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: p.shadow,
    };
    v.extreme_bg_color = p.card;
    v.text_edit_bg_color = Some(p.card);
    v.faint_bg_color = p.card_alt;
    v.code_bg_color = p.sunken;
    v.hyperlink_color = p.accent_text;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = with_alpha(p.accent, if p.dark { 110 } else { 70 });
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.slider_trailing_fill = true;
    v.window_highlight_topmost = false;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    let r = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.card;
    w.inactive.weak_bg_fill = p.card;
    w.inactive.bg_stroke = Stroke::new(1.0, p.stroke_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.hover;
    w.hovered.weak_bg_fill = p.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.stroke_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.pressed;
    w.active.weak_bg_fill = p.pressed;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.bg_stroke = Stroke::new(1.0, p.stroke_strong);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;

    style.visuals = v;
    style.animation_time = 0.12;
    style.interaction.tooltip_delay = 0.45;
    style.url_in_tooltip = false;
    style
}

pub fn window_shadow() -> Shadow {
    Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: pal().shadow,
    }
}

/// Deterministic, pleasant colour for avatars and categories.
pub fn color_for(seed: &str) -> Color32 {
    const COLORS: [u32; 14] = [
        0x0F6CBD, 0x8764B8, 0x038387, 0xCA5010, 0x498205, 0xC239B3, 0x4F6BED, 0x986F0B,
        0xD13438, 0x5C2E91, 0x00827F, 0x637CEF, 0xB146C2, 0x2E7D32,
    ];
    let mut h: u32 = 2166136261;
    for b in seed.to_lowercase().bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    let c = hex(COLORS[(h as usize) % COLORS.len()]);
    if pal().dark {
        mix(c, Color32::WHITE, 0.15)
    } else {
        c
    }
}

/// Outlook-style category colour names.
pub fn category_color(name: &str) -> Color32 {
    match name {
        "Red" => hex(0xE74856),
        "Orange" => hex(0xFF8C00),
        "Yellow" => hex(0xFFD13B),
        "Green" => hex(0x16C60C),
        "Blue" => hex(0x0078D4),
        "Purple" => hex(0x886CE4),
        "Teal" => hex(0x00B7C3),
        "Pink" => hex(0xE3008C),
        "Gray" => hex(0x8A8886),
        _ => color_for(name),
    }
}

pub const CATEGORY_COLORS: [&str; 9] = [
    "Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Teal", "Pink", "Gray",
];
