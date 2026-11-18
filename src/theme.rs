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
    /// Monospace text, square boxes and terminal colours, as if the app
    /// were running in a terminal window. Inspired by BS-Mail by IronWolve
    /// (<https://github.com/IronWolve/BS-Mail>).
    Terminal,
    /// A synthwave dusk: deep purple, a hot pink and orange sunset, teal.
    NeonSunset,
    /// Near-black with acid yellow, electric cyan and hot magenta, glowing
    /// edges and cut corners.
    Cyberpunk,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 6] = [
        ThemeMode::System,
        ThemeMode::Light,
        ThemeMode::Dark,
        ThemeMode::Terminal,
        ThemeMode::NeonSunset,
        ThemeMode::Cyberpunk,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "Use system setting",
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::Terminal => "Terminal",
            ThemeMode::NeonSunset => "Neon Sunset",
            ThemeMode::Cyberpunk => "Cyberpunk",
        }
    }

    /// The family of visuals this mode uses.
    pub fn skin(self) -> Skin {
        match self {
            ThemeMode::System | ThemeMode::Light | ThemeMode::Dark => Skin::Standard,
            ThemeMode::Terminal => Skin::Terminal,
            ThemeMode::NeonSunset => Skin::NeonSunset,
            ThemeMode::Cyberpunk => Skin::Cyberpunk,
        }
    }

    /// Themes with colours of their own, which the accent colour setting
    /// doesn't change.
    pub fn own_colors(self) -> bool {
        self.skin() != Skin::Standard
    }
}

/// The family of visuals a palette belongs to. Beyond its colours, each
/// non-standard skin adds a few touches of its own in the UI code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Skin {
    /// Light and Dark, with the chosen accent colour.
    #[default]
    Standard,
    Terminal,
    NeonSunset,
    Cyberpunk,
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
    pub skin: Skin,
    /// The Terminal theme: monospace text, square corners, no shadows.
    /// Same as `skin == Skin::Terminal`.
    pub term: bool,
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
    /// Extra terminal colours. The light and dark themes map them to their
    /// nearest existing colours.
    pub cyan: Color32,
    pub magenta: Color32,
    pub yellow: Color32,
    /// Column dividers and other faint rules.
    pub rule: Color32,
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
                skin: Skin::Standard,
                term: false,
                cyan: base,
                magenta: hex(0xAD2D8C),
                yellow: hex(0x8A5300),
                rule: hex(0xE0E0E0),
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
                skin: Skin::Standard,
                term: false,
                cyan: mix(base, Color32::WHITE, 0.45),
                magenta: hex(0xE08AD0),
                yellow: hex(0xF4BF4F),
                rule: hex(0x3D3D3D),
            }
        }
    }

    /// The Terminal theme: a near-black screen with cyan box lines, green
    /// headings, yellow counts, magenta tags and a blue selection bar. It
    /// has its own colours, so the accent colour setting doesn't apply.
    /// The look was inspired by BS-Mail by IronWolve; no BS-Mail code or
    /// files are used.
    pub fn terminal() -> Self {
        let bg = hex(0x03080F);
        let green = hex(0x66E655);
        let cyan = hex(0x3EE6E8);
        let blue = hex(0x00227A);
        Palette {
            dark: true,
            skin: Skin::Terminal,
            term: true,
            bg,
            card: bg,
            card_alt: hex(0x070E18),
            sunken: hex(0x070E18),
            hover: hex(0x0E1A2B),
            pressed: hex(0x16263D),
            stroke: hex(0x1F8E91),
            stroke_strong: hex(0x2FD9DC),
            text: hex(0xF4F8FC),
            text2: hex(0xC9D1DA),
            text3: hex(0x83888D),
            disabled: hex(0x474C52),
            accent: hex(0x3A9560),
            accent_hover: hex(0x45AD70),
            accent_pressed: hex(0x2F7A4F),
            on_accent: bg,
            accent_text: green,
            selected: blue,
            selected_hover: hex(0x0A2F94),
            accent_soft: hex(0x0B2216),
            danger: hex(0xFF5F5F),
            danger_soft: hex(0x2A0C10),
            warning: hex(0xFFD028),
            warning_soft: hex(0x261F05),
            success: hex(0x3FE56A),
            success_soft: hex(0x0A2412),
            flag: hex(0xFFD028),
            flagged_row: bg,
            pinned_row: bg,
            shadow: Color32::TRANSPARENT,
            cyan,
            magenta: hex(0xD77ADE),
            yellow: hex(0xFFD028),
            rule: hex(0x3B4046),
        }
    }
}

/// Builds a palette from `#RRGGBB` values in field order (the shadow takes
/// `#RRGGBBAA`), so the hand-picked themes below read as a colour table.
fn picked(skin: Skin, c: &[u32; 33], shadow: Color32) -> Palette {
    let h = |i: usize| hex(c[i]);
    Palette {
        dark: true,
        skin,
        term: skin == Skin::Terminal,
        bg: h(0),
        card: h(1),
        card_alt: h(2),
        sunken: h(3),
        hover: h(4),
        pressed: h(5),
        stroke: h(6),
        stroke_strong: h(7),
        text: h(8),
        text2: h(9),
        text3: h(10),
        disabled: h(11),
        accent: h(12),
        accent_hover: h(13),
        accent_pressed: h(14),
        on_accent: h(15),
        accent_text: h(16),
        selected: h(17),
        selected_hover: h(18),
        accent_soft: h(19),
        danger: h(20),
        danger_soft: h(21),
        warning: h(22),
        warning_soft: h(23),
        success: h(24),
        success_soft: h(25),
        flag: h(26),
        flagged_row: h(27),
        pinned_row: h(28),
        shadow,
        cyan: h(29),
        magenta: h(30),
        yellow: h(31),
        rule: h(32),
    }
}

impl Palette {
    /// Neon Sunset: a synthwave dusk. Calm indigo surfaces for reading,
    /// hot pink fading into sunset orange on the marks that mean "act
    /// here", teal for links and focus, a striped sun and a glowing
    /// horizon in the title bar (see [`sunset`]). Every text colour meets
    /// WCAG AA on the surfaces it is used on.
    pub fn neon_sunset() -> Self {
        picked(
            Skin::NeonSunset,
            &[
                0x130C23, 0x221939, 0x281E42, 0x1A1230, 0x2E2249, 0x3A2C57, 0x3C2E5E, 0x7360A2, // bg … stroke_strong
                0xF6EAF3, 0xD6C7E3, 0xAFA0C8, 0x6E5E8A, // text … disabled
                0xFF4F9E, 0xFF6DAF, 0xE83F8B, 0x1A0B2C, 0xFF94C8, // accent … accent_text
                0x472049, 0x55244F, 0x381C44, // selected, selected_hover, accent_soft
                0xFF6F69, 0x3D1832, 0xFFB84D, 0x38242E, 0x62E6A2, 0x163428, // danger … success_soft
                0xFF9A4D, 0x37233A, 0x232845, // flag, flagged_row, pinned_row
                0x52D9D1, 0xD77BFF, 0xFFCB6B, 0x33264F, // cyan, magenta, yellow, rule
            ],
            Color32::from_rgba_unmultiplied(0xF0, 0x48, 0x9E, 0x40),
        )
    }

    /// Cyberpunk: near-black panels with cut corners, acid yellow for
    /// controls, electric cyan for live text and edges, hot magenta for the
    /// selection and the glow around dialogs (see [`cut_corners`] and
    /// `cyber_visuals`). Every text colour meets WCAG AA on the surfaces it
    /// is used on.
    pub fn cyberpunk() -> Self {
        picked(
            Skin::Cyberpunk,
            &[
                0x06070B, 0x0D0F16, 0x12151E, 0x08090E, 0x171B26, 0x1F2433, 0x26404B, 0x24818E, // bg … stroke_strong
                0xDFE8EE, 0xB4C1CB, 0x919EAC, 0x5C6474, // text … disabled
                0xE6F032, 0xF2F87A, 0xBAC41E, 0x06070B, 0x58E1EC, // accent … accent_text
                0x34163B, 0x3E1C46, 0x0F2229, // selected, selected_hover, accent_soft
                0xFF5A52, 0x2C1018, 0xFFB224, 0x2A1F08, 0x39F2A0, 0x0C2A1E, // danger … success_soft
                0xFF3DA8, 0x1D0E15, 0x0F1A21, // flag, flagged_row, pinned_row
                0x1FDDEB, 0xFF3DA8, 0xE6F032, 0x1E2A36, // cyan, magenta, yellow, rule
            ],
            Color32::from_rgba_unmultiplied(0xFF, 0x3D, 0xA8, 0x38),
        )
    }
}

/// Neon Sunset's sun and sky colours, used for its gradients.
pub mod sunset {
    use super::{hex, mix};
    use egui::Color32;

    pub const GOLD: Color32 = hex(0xFFC861);
    pub const ORANGE: Color32 = hex(0xFF8A4C);
    pub const PINK: Color32 = hex(0xFF4F9E);
    pub const VIOLET: Color32 = hex(0x8E5CFF);
    /// The sky at the bottom of the title bar: at its ends, and warmer in
    /// the middle behind the search box.
    pub const SKY_EDGE: Color32 = hex(0x1C1032);
    pub const SKY_GLOW: Color32 = hex(0x2C1242);
    /// Where the pink-to-orange accent gradient ends when hovered / pressed.
    pub const HOVER_END: Color32 = hex(0xFF9C66);
    pub const PRESSED_END: Color32 = hex(0xF07A3E);

    /// The sun's colour `t` of the way down it: gold, orange, then pink.
    pub fn sun_at(t: f32) -> Color32 {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            mix(GOLD, ORANGE, t * 2.0)
        } else {
            mix(ORANGE, PINK, t * 2.0 - 1.0)
        }
    }
}

/// Text colour for a label drawn on `fill`: white, or dark ink where white
/// would be hard to read (light or bright fills).
pub fn ink_on(fill: Color32) -> Color32 {
    // Near-black, so the better of the two is at least 4.5:1 on any fill.
    let ink = hex(0x08060C);
    if contrast(Color32::WHITE, fill) >= contrast(ink, fill) {
        Color32::WHITE
    } else {
        ink
    }
}

/// WCAG 2 contrast ratio between two opaque colours.
fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let (la, lb) = (lum(a), lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

static PALETTE: RwLock<Option<Palette>> = RwLock::new(None);
static DENSITY: RwLock<Density> = RwLock::new(Density::Cozy);

/// Whether the Terminal theme is in effect.
pub fn terminal() -> bool {
    pal().term
}

/// Monospace faces look larger than the UI font at the same size, so the
/// Terminal theme draws text a little smaller.
const TERM_TEXT_SCALE: f32 = 0.9;

fn text_size(size: f32) -> f32 {
    if terminal() {
        (size * TERM_TEXT_SCALE).round()
    } else {
        size
    }
}

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
    FontId::new(text_size(size), FontFamily::Proportional)
}
pub fn font_semibold(size: f32) -> FontId {
    FontId::new(text_size(size), FontFamily::Name(SEMIBOLD.into()))
}
pub fn font_bold(size: f32) -> FontId {
    FontId::new(text_size(size), FontFamily::Name(BOLD.into()))
}
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn icon_fill_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS_FILL.into()))
}

/// Install the palette and derive egui's style from it.
pub fn apply(ctx: &egui::Context, dark: bool, skin: Skin, accent: Accent, dens: Density) {
    let p = match skin {
        Skin::Standard => Palette::new(dark, accent),
        Skin::Terminal => Palette::terminal(),
        Skin::NeonSunset => Palette::neon_sunset(),
        Skin::Cyberpunk => Palette::cyberpunk(),
    };
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
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });
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

    if p.term {
        terminal_visuals(&mut v, p);
    }
    if p.skin == Skin::NeonSunset {
        sunset_visuals(&mut v, p);
    }
    if p.skin == Skin::Cyberpunk {
        cyber_visuals(&mut v, p);
    }

    style.visuals = v;
    style.animation_time = 0.12;
    style.interaction.tooltip_delay = 0.45;
    style.url_in_tooltip = false;
    style
}

/// Neon Sunset: teal (the ocean) for links, the text cursor and focus, so
/// they never look like the pink unread marks; a pink neon edge and glow
/// on dialogs and menus.
fn sunset_visuals(v: &mut Visuals, p: &Palette) {
    v.hyperlink_color = p.cyan;
    v.text_cursor.stroke = Stroke::new(2.0, p.cyan);
    v.selection.stroke = Stroke::new(1.5, p.cyan);
    v.window_stroke = window_stroke_for(p);
    v.popup_shadow.color = Color32::from_rgba_unmultiplied(0xF0, 0x48, 0x9E, 0x28);
}

/// Cyberpunk: neon edges instead of drop shadows (a cyan rim with a
/// magenta haze around dialogs, a cyan haze around menus), cyan text on a
/// violet selection, a yellow caret and cyan hover rims. The corner radii
/// stay, so [`cut_corners`] cuts them.
fn cyber_visuals(v: &mut Visuals, p: &Palette) {
    v.window_stroke = Stroke::new(1.0, p.stroke_strong);
    v.window_shadow = Shadow { offset: [0, 0], blur: 24, spread: 0, color: p.shadow };
    v.popup_shadow = Shadow { offset: [0, 0], blur: 12, spread: 0, color: with_alpha(p.cyan, 0x30) };
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.extreme_bg_color = p.sunken;
    v.text_edit_bg_color = Some(p.sunken);
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.cyan);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
}

/// Square corners, box lines and no shadows for the Terminal theme.
fn terminal_visuals(v: &mut Visuals, p: &Palette) {
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.window_stroke = Stroke::new(1.0, p.stroke_strong);
    v.window_fill = p.bg;
    v.extreme_bg_color = p.sunken;
    v.text_edit_bg_color = Some(p.sunken);
    v.hyperlink_color = p.cyan;
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.6 };
    v.text_cursor.stroke = Stroke::new(2.0, p.success);
    let w = &mut v.widgets;
    for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = CornerRadius::ZERO;
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.inactive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.hovered.bg_stroke = Stroke::new(1.0, p.stroke_strong);
    w.hovered.fg_stroke = Stroke::new(1.0, p.cyan);
    w.active.bg_stroke = Stroke::new(1.0, p.cyan);
    w.open.bg_stroke = Stroke::new(1.0, p.stroke_strong);
}

/// Runs `f` on every shape painted so far this frame, in every layer of the
/// current viewport, with the index of the layer it is in (layers are
/// visited one after another). Themes use it once the frame is drawn to
/// restyle shapes across the whole app without changing the code that
/// paints them.
fn restyle_shapes(ctx: &egui::Context, f: &dyn Fn(&mut egui::Shape, usize)) {
    fn visit(shape: &mut egui::Shape, layer: usize, f: &dyn Fn(&mut egui::Shape, usize)) {
        if let egui::Shape::Vec(list) = shape {
            list.iter_mut().for_each(|s| visit(s, layer, f));
        } else {
            f(shape, layer);
        }
    }
    let mut layers: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    layers.push(egui::LayerId::background());
    ctx.graphics_mut(|g| {
        for (n, id) in layers.into_iter().enumerate() {
            if let Some(list) = g.get_mut(id) {
                for i in 0..list.next_idx().0 {
                    list.mutate_shape(egui::layers::ShapeIdx(i), |s| visit(&mut s.shape, n, f));
                }
            }
        }
    });
}

/// Applies the theme's reshaping of boxes (square or cut corners) to
/// everything painted this frame in the current viewport. Call it once per
/// frame in every viewport, after all its drawing: the main window and
/// pop-out compose windows.
pub fn restyle_frame(ctx: &egui::Context) {
    match pal().skin {
        Skin::Terminal => square_corners(ctx),
        Skin::Cyberpunk => cut_corners(ctx),
        Skin::Standard | Skin::NeonSunset => {}
    }
}

/// Makes every rectangle painted this frame square. The Terminal theme
/// calls it once the frame is drawn, so the many rounded cards, buttons
/// and highlights across the app need no changes of their own.
pub fn square_corners(ctx: &egui::Context) {
    restyle_shapes(ctx, &|shape, _| {
        if let egui::Shape::Rect(r) = shape {
            r.corner_radius = CornerRadius::ZERO;
        }
    });
}

/// The Cyberpunk theme's shapes, applied to everything painted this frame:
/// - a rounded top-left or bottom-right corner is cut off diagonally (the
///   other two corners, and corners that were square, become square);
/// - the outermost large framed panels get an acid-yellow bracket lit along
///   their cut top-left corner and a cyan tick on the other;
/// - anything filled with the selection colour gets a magenta outline;
/// - circles of radius 5 or more (avatars, round buttons) become cut tiles.
///
/// Square rectangles (backgrounds, lines), images, shadows, rotated
/// rectangles and meshes (gradients) stay as they are, and small dots stay
/// round.
pub fn cut_corners(ctx: &egui::Context) {
    let p = pal();
    let ppp = ctx.pixels_per_point();
    let selection_line = Stroke::new(1.0, mix(p.selected, p.magenta, 0.6));
    let is_selection = |c: Color32| c == p.selected || c == p.selected_hover;
    // Panels given a bracket so far, in the layer being visited: panels
    // inside them (cards in a dialog, a message in the reading pane) don't
    // get one of their own.
    let bracketed: std::cell::RefCell<(usize, Vec<egui::Rect>)> = Default::default();
    restyle_shapes(ctx, &|shape, layer| {
        if let egui::Shape::Circle(c) = shape {
            if c.radius >= 5.0 {
                // A tile 1.8 r wide, inside the circle's box.
                let (h, k) = (c.radius * 0.9, c.radius * 0.45);
                let o = c.center;
                let stroke = if c.stroke.width == 0.0 && is_selection(c.fill) { selection_line } else { c.stroke };
                let points = vec![
                    o + egui::vec2(-h + k, -h),
                    o + egui::vec2(h, -h),
                    o + egui::vec2(h, h - k),
                    o + egui::vec2(h - k, h),
                    o + egui::vec2(-h, h),
                    o + egui::vec2(-h, -h + k),
                ];
                *shape = egui::Shape::convex_polygon(points, c.fill, stroke);
            }
            return;
        }
        let egui::Shape::Rect(r) = shape else { return };
        if r.corner_radius == CornerRadius::ZERO || r.blur_width > 0.0 || r.brush.is_some() || r.angle != 0.0 {
            return;
        }
        // Framed before the selection outline is added, so selected rows
        // don't count as framed panels.
        let framed = r.stroke.width >= 1.0;
        if r.stroke.width == 0.0 && is_selection(r.fill) {
            r.stroke = selection_line;
            r.stroke_kind = egui::StrokeKind::Inside;
        }
        // On whole pixels, as egui draws rectangles, so 1 px borders stay
        // crisp at any zoom; then where the stroke sits.
        let snapped = if r.round_to_pixels == Some(false) { r.rect } else { egui::emath::GuiRounding::round_to_pixels(r.rect, ppp) };
        let rect = match r.stroke_kind {
            egui::StrokeKind::Inside => snapped.shrink(r.stroke.width / 2.0),
            egui::StrokeKind::Outside => snapped.expand(r.stroke.width / 2.0),
            egui::StrokeKind::Middle => snapped,
        };
        // Each cut follows its own corner's radius. Narrow bars (an event's
        // colour stripe) are cut across nearly their whole width, so they
        // line up with the cut of the box they sit on.
        let cut_for = |radius: u8| {
            let cut = (radius as f32 * 1.25).min(rect.width() * 0.9).min(rect.height() / 3.0);
            if cut >= 2.0 { cut } else { 0.0 }
        };
        let (nw, se) = (cut_for(r.corner_radius.nw), cut_for(r.corner_radius.se));
        if nw == 0.0 && se == 0.0 {
            r.corner_radius = CornerRadius::ZERO;
            return;
        }
        let mut points = Vec::with_capacity(6);
        if nw > 0.0 {
            points.push(egui::pos2(rect.left() + nw, rect.top()));
        } else {
            points.push(rect.left_top());
        }
        points.push(rect.right_top());
        if se > 0.0 {
            points.push(egui::pos2(rect.right(), rect.bottom() - se));
            points.push(egui::pos2(rect.right() - se, rect.bottom()));
        } else {
            points.push(rect.right_bottom());
        }
        points.push(rect.left_bottom());
        if nw > 0.0 {
            points.push(egui::pos2(rect.left(), rect.top() + nw));
        }
        let panel = egui::Shape::Path(egui::epaint::PathShape::convex_polygon(points, r.fill, r.stroke));
        let lit = framed && nw > 0.0 && rect.width() >= 120.0 && rect.height() >= 40.0 && {
            let mut b = bracketed.borrow_mut();
            if b.0 != layer {
                *b = (layer, Vec::new());
            }
            let nested = b.1.iter().any(|outer| outer.expand(1.0).contains_rect(rect));
            if !nested {
                b.1.push(rect);
            }
            !nested
        };
        if !lit {
            *shape = panel;
            return;
        }
        let t = rect.shrink(0.5);
        let mut parts = vec![
            panel,
            egui::Shape::line(
                vec![
                    egui::pos2(t.left(), t.top() + nw + 12.0),
                    egui::pos2(t.left(), t.top() + nw),
                    egui::pos2(t.left() + nw, t.top()),
                    egui::pos2(t.left() + nw + 32.0, t.top()),
                ],
                Stroke::new(2.0, p.accent),
            ),
        ];
        if se > 0.0 {
            parts.push(egui::Shape::line(
                vec![
                    egui::pos2(t.right(), t.bottom() - se - 8.0),
                    egui::pos2(t.right(), t.bottom() - se),
                    egui::pos2(t.right() - se, t.bottom()),
                    egui::pos2(t.right() - se - 8.0, t.bottom()),
                ],
                Stroke::new(1.0, p.cyan),
            ));
        }
        *shape = egui::Shape::Vec(parts);
    });
}

/// How far Cyberpunk cuts a corner of `radius` on a box of `rect`'s size
/// (0 when it doesn't), for things drawn along a box's edge.
pub fn corner_cut(radius: u8, rect: egui::Rect) -> f32 {
    if pal().skin != Skin::Cyberpunk {
        return 0.0;
    }
    let cut = (radius as f32 * 1.25).min(rect.width() * 0.9).min(rect.height() / 3.0);
    if cut >= 2.0 { cut } else { 0.0 }
}

/// Border of dialogs: none, or a box line in the Terminal theme, where
/// dialogs have no shadow to set them apart.
pub fn window_stroke() -> Stroke {
    window_stroke_for(&pal())
}

fn window_stroke_for(p: &Palette) -> Stroke {
    match p.skin {
        Skin::Terminal | Skin::Cyberpunk => Stroke::new(1.0, p.stroke_strong),
        // A soft pink neon edge.
        Skin::NeonSunset => Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xFF, 0x4F, 0x9E, 90)),
        _ => Stroke::NONE,
    }
}

pub fn window_shadow() -> Shadow {
    let p = pal();
    if p.term {
        return Shadow::NONE;
    }
    if p.skin == Skin::Cyberpunk {
        // A magenta haze all round instead of a drop shadow.
        return Shadow { offset: [0, 0], blur: 24, spread: 0, color: p.shadow };
    }
    Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: p.shadow,
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
