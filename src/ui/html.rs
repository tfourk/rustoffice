//! A small, safe HTML e-mail renderer. HTML is parsed with `scraper` and
//! flattened into blocks of styled runs (tables become stacked blocks, as on
//! mobile mail clients). Nothing is executed and remote images are only
//! fetched when the user allows it.

use std::collections::HashMap;

use egui::text::LayoutJob;
use egui::{
    vec2, Align, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Pos2, Rect, Sense, Stroke,
    TextFormat, Ui,
};
use scraper::{ElementRef, Html, Node};

use crate::icons;
use crate::theme::{self, font, icon_font, mix, pal};
use crate::util;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub scale: f32,
    pub color: Option<Color32>,
    pub bg: Option<Color32>,
    pub link: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
}

#[derive(Clone, Debug)]
pub enum Block {
    Text {
        runs: Vec<Run>,
        align: Align,
        indent: u8,
        quote: u8,
        heading: u8,
        marker: Option<String>,
    },
    Image {
        src: String,
        alt: String,
        width: Option<f32>,
        height: Option<f32>,
        link: Option<String>,
        align: Align,
        indent: u8,
        quote: u8,
    },
    Rule {
        quote: u8,
    },
    Gap,
}

#[derive(Clone, Debug, Default)]
pub struct HtmlDoc {
    pub blocks: Vec<Block>,
    pub remote_images: usize,
}

const SKIP: &[&str] = &[
    "head", "style", "script", "title", "meta", "link", "noscript", "template", "iframe", "object",
    "embed", "svg", "math", "select", "option", "input", "textarea", "button", "form-hidden", "map",
    "area", "audio", "video", "canvas",
];

fn parse_color(v: &str) -> Option<Color32> {
    let v = v.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let h = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            3 => {
                let c: Vec<char> = hex.chars().collect();
                Some(Color32::from_rgb(
                    h(&format!("{0}{0}", c[0]))?,
                    h(&format!("{0}{0}", c[1]))?,
                    h(&format!("{0}{0}", c[2]))?,
                ))
            }
            6 | 8 => Some(Color32::from_rgb(h(&hex[0..2])?, h(&hex[2..4])?, h(&hex[4..6])?)),
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")) {
        let nums: Vec<f32> = inner
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.trim().trim_end_matches('%').parse().ok())
            .collect();
        if nums.len() >= 3 {
            if nums.len() == 4 && nums[3] < 0.1 {
                return None;
            }
            return Some(Color32::from_rgb(nums[0] as u8, nums[1] as u8, nums[2] as u8));
        }
        return None;
    }
    let named = match v.as_str() {
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "green" => 0x008000,
        "blue" => 0x0000ff,
        "gray" | "grey" => 0x808080,
        "silver" => 0xc0c0c0,
        "maroon" => 0x800000,
        "navy" => 0x000080,
        "purple" => 0x800080,
        "teal" => 0x008080,
        "olive" => 0x808000,
        "orange" => 0xffa500,
        "yellow" => 0xffff00,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "darkblue" => 0x00008b,
        "darkred" => 0x8b0000,
        "darkgreen" => 0x006400,
        "crimson" => 0xdc143c,
        "royalblue" => 0x4169e1,
        "steelblue" => 0x4682b4,
        "dimgray" | "dimgrey" => 0x696969,
        "whitesmoke" => 0xf5f5f5,
        "gainsboro" => 0xdcdcdc,
        _ => return None,
    };
    Some(theme::hex(named))
}

pub(crate) fn luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

pub(crate) fn saturation(c: Color32) -> f32 {
    let mx = c.r().max(c.g()).max(c.b()) as f32;
    let mn = c.r().min(c.g()).min(c.b()) as f32;
    if mx == 0.0 {
        0.0
    } else {
        (mx - mn) / mx
    }
}

fn parse_len(v: &str) -> Option<f32> {
    let v = v.trim().trim_end_matches("!important").trim();
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f32 = num.parse().ok()?;
    if v.ends_with('%') {
        None
    } else {
        Some(n)
    }
}

#[derive(Default)]
struct Css {
    color: Option<Color32>,
    bg: Option<Color32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<f32>,
    align: Option<Align>,
    hidden: bool,
    mono: bool,
    width: Option<f32>,
    height: Option<f32>,
}

fn parse_css(s: &str) -> Css {
    let mut c = Css::default();
    let mut max_h_zero = false;
    let mut overflow_hidden = false;
    for decl in s.split(';') {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        let lv = v.to_ascii_lowercase();
        match k.as_str() {
            "color" => c.color = parse_color(v),
            "background-color" => c.bg = parse_color(v),
            "background" => {
                if let Some(col) = lv.split_whitespace().find_map(parse_color) {
                    c.bg = Some(col);
                }
            }
            "font-weight" => {
                c.bold = Some(lv.contains("bold") || lv.parse::<u32>().map(|w| w >= 600).unwrap_or(false))
            }
            "font-style" => c.italic = Some(lv.contains("italic")),
            "text-decoration" | "text-decoration-line" => {
                if lv.contains("underline") {
                    c.underline = Some(true);
                }
                if lv.contains("line-through") {
                    c.strike = Some(true);
                }
                if lv.contains("none") {
                    c.underline = Some(false);
                }
            }
            "font-size" => {
                let px = if lv.ends_with("pt") {
                    parse_len(&lv.replace("pt", "")).map(|p| p * 1.333)
                } else if lv.ends_with("em") || lv.ends_with("rem") {
                    parse_len(&lv.replace("rem", "").replace("em", "")).map(|e| e * 14.0)
                } else if lv.ends_with('%') {
                    lv.trim_end_matches('%').trim().parse::<f32>().ok().map(|p| p / 100.0 * 14.0)
                } else {
                    match lv.as_str() {
                        "small" | "x-small" | "xx-small" => Some(12.0),
                        "large" => Some(17.0),
                        "x-large" => Some(21.0),
                        "xx-large" => Some(26.0),
                        _ => parse_len(&lv.replace("px", "")),
                    }
                };
                if let Some(px) = px {
                    if px <= 1.0 {
                        c.hidden = true;
                    }
                    c.size = Some(px);
                }
            }
            "text-align" => {
                c.align = match lv.as_str() {
                    "center" => Some(Align::Center),
                    "right" => Some(Align::Max),
                    "left" | "justify" => Some(Align::Min),
                    _ => None,
                }
            }
            "display" if lv.starts_with("none") => c.hidden = true,
            "visibility" if lv.starts_with("hidden") => c.hidden = true,
            "mso-hide" if lv.starts_with("all") => c.hidden = true,
            "opacity" if lv.parse::<f32>().map(|o| o < 0.05).unwrap_or(false) => c.hidden = true,
            "max-height" if parse_len(&lv.replace("px", "")) == Some(0.0) => max_h_zero = true,
            "height" => c.height = parse_len(&lv.replace("px", "")),
            "width" => c.width = parse_len(&lv.replace("px", "")),
            "overflow" if lv.starts_with("hidden") => overflow_hidden = true,
            "font-family" if lv.contains("mono") || lv.contains("courier") || lv.contains("consolas") => {
                c.mono = true
            }
            _ => {}
        }
    }
    if max_h_zero && overflow_hidden {
        c.hidden = true;
    }
    c
}

struct Builder {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    align: Align,
    indent: u8,
    quote: u8,
    heading: u8,
    pre: bool,
    marker: Option<String>,
    last_space: bool,
    lists: Vec<(bool, usize)>,
    remote_images: usize,
}

impl Builder {
    fn new() -> Self {
        Builder {
            blocks: Vec::new(),
            runs: Vec::new(),
            align: Align::Min,
            indent: 0,
            quote: 0,
            heading: 0,
            pre: false,
            marker: None,
            last_space: true,
            lists: Vec::new(),
            remote_images: 0,
        }
    }

    fn push_str(&mut self, s: &str, style: &RunStyle) {
        if s.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(r) if r.style == *style => r.text.push_str(s),
            _ => self.runs.push(Run {
                text: s.to_string(),
                style: style.clone(),
            }),
        }
    }

    fn text(&mut self, t: &str, style: &RunStyle) {
        if self.pre {
            self.push_str(t, style);
            self.last_space = t.ends_with(char::is_whitespace);
            return;
        }
        let mut out = String::with_capacity(t.len());
        for ch in t.chars() {
            // Zero-width characters used as preheader padding.
            if matches!(ch, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{034f}' | '\u{00ad}') {
                continue;
            }
            if ch.is_whitespace() && ch != '\u{a0}' {
                if !self.last_space {
                    out.push(' ');
                    self.last_space = true;
                }
            } else {
                out.push(if ch == '\u{a0}' { ' ' } else { ch });
                self.last_space = false;
            }
        }
        self.push_str(&out, style);
    }

    fn newline(&mut self, style: &RunStyle) {
        // Trim the trailing space before a forced break.
        if let Some(r) = self.runs.last_mut() {
            while r.text.ends_with(' ') {
                r.text.pop();
            }
        }
        self.push_str("\n", style);
        self.last_space = true;
    }

    fn flush(&mut self) {
        let mut runs = std::mem::take(&mut self.runs);
        // Trim whitespace at both ends of the block.
        while let Some(r) = runs.first_mut() {
            let t = r.text.trim_start_matches([' ', '\n']).to_string();
            if t.is_empty() {
                runs.remove(0);
            } else {
                r.text = t;
                break;
            }
        }
        while let Some(r) = runs.last_mut() {
            let t = r.text.trim_end_matches([' ', '\n']).to_string();
            if t.is_empty() {
                runs.pop();
            } else {
                r.text = t;
                break;
            }
        }
        self.last_space = true;
        if runs.iter().all(|r| r.text.trim().is_empty()) {
            return;
        }
        self.blocks.push(Block::Text {
            runs,
            align: self.align,
            indent: self.indent,
            quote: self.quote,
            heading: self.heading,
            marker: self.marker.take(),
        });
    }

    fn gap(&mut self) {
        if !matches!(self.blocks.last(), Some(Block::Gap) | None) {
            self.blocks.push(Block::Gap);
        }
    }

    fn walk(&mut self, el: ElementRef<'_>, style: &RunStyle) {
        for child in el.children() {
            if self.blocks.len() > 6000 {
                return;
            }
            match child.value() {
                Node::Text(t) => {
                    let s: &str = t;
                    self.text(s, style);
                }
                Node::Element(_) => {
                    if let Some(e) = ElementRef::wrap(child) {
                        self.element(e, style);
                    }
                }
                _ => {}
            }
        }
    }

    fn element(&mut self, el: ElementRef<'_>, parent: &RunStyle) {
        let v = el.value();
        let tag = v.name().to_ascii_lowercase();
        if SKIP.contains(&tag.as_str()) {
            return;
        }
        if v.attr("hidden").is_some() || v.attr("aria-hidden") == Some("true") && tag != "img" {
            if tag != "td" && tag != "table" {
                return;
            }
        }
        let css = v.attr("style").map(parse_css).unwrap_or_default();
        if css.hidden {
            return;
        }
        let mut style = parent.clone();
        if let Some(c) = css.color.or_else(|| v.attr("color").and_then(parse_color)) {
            style.color = Some(c);
        }
        if let Some(b) = css.bg.or_else(|| v.attr("bgcolor").and_then(parse_color)) {
            style.bg = Some(b);
        }
        if let Some(b) = css.bold {
            style.bold = b;
        }
        if let Some(i) = css.italic {
            style.italic = i;
        }
        if let Some(u) = css.underline {
            style.underline = u;
        }
        if let Some(s) = css.strike {
            style.strike = s;
        }
        if css.mono {
            style.mono = true;
        }
        if let Some(px) = css.size {
            style.scale = (px / 14.0).clamp(0.72, 2.3);
        }
        let attr_align = v.attr("align").and_then(|a| match a.to_ascii_lowercase().as_str() {
            "center" => Some(Align::Center),
            "right" => Some(Align::Max),
            "left" => Some(Align::Min),
            _ => None,
        });
        let block_align = css.align.or(attr_align);

        let block = |b: &mut Builder, style: &RunStyle, el: ElementRef<'_>, gap_after: bool| {
            b.flush();
            let old = b.align;
            if let Some(a) = block_align {
                b.align = a;
            }
            b.walk(el, style);
            b.flush();
            b.align = old;
            if gap_after {
                b.gap();
            }
        };

        match tag.as_str() {
            "br" => self.newline(&style),
            "img" => self.image(el, &style, &css),
            "hr" => {
                self.flush();
                self.blocks.push(Block::Rule { quote: self.quote });
            }
            "p" => block(self, &style, el, true),
            "div" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside"
            | "address" | "figure" | "figcaption" | "form" | "fieldset" | "dl" | "dt" | "dd"
            | "table" | "tbody" | "thead" | "tfoot" | "tr" | "caption" | "body" | "html" => {
                block(self, &style, el, false)
            }
            "td" | "th" => {
                if tag == "th" {
                    style.bold = true;
                }
                block(self, &style, el, false)
            }
            "center" => {
                self.flush();
                let old = self.align;
                self.align = Align::Center;
                self.walk(el, &style);
                self.flush();
                self.align = old;
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<u8>().unwrap_or(3);
                self.flush();
                let old = self.heading;
                self.heading = level;
                style.bold = true;
                block(self, &style, el, true);
                self.heading = old;
            }
            "ul" | "ol" | "menu" => {
                self.flush();
                let start = v.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
                self.lists.push((tag == "ol", start));
                self.indent = self.indent.saturating_add(1);
                self.walk(el, &style);
                self.flush();
                self.indent = self.indent.saturating_sub(1);
                self.lists.pop();
                if self.lists.is_empty() {
                    self.gap();
                }
            }
            "li" => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        let m = format!("{n}.");
                        *n += 1;
                        m
                    }
                    _ => "•".to_string(),
                };
                self.marker = Some(marker);
                // Bullets next to centered text look broken; list items are left aligned.
                let old = self.align;
                self.align = Align::Min;
                self.walk(el, &style);
                self.flush();
                self.align = old;
            }
            "blockquote" => {
                self.flush();
                self.quote = self.quote.saturating_add(1);
                self.walk(el, &style);
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                self.gap();
            }
            "pre" => {
                self.flush();
                self.pre = true;
                style.mono = true;
                self.walk(el, &style);
                self.flush();
                self.pre = false;
                self.gap();
            }
            "a" => {
                if let Some(href) = v.attr("href") {
                    let h = href.trim();
                    if !h.is_empty() && !h.starts_with('#') && !h.to_ascii_lowercase().starts_with("javascript:") {
                        style.link = Some(h.to_string());
                    }
                }
                self.walk(el, &style);
            }
            "b" | "strong" => {
                style.bold = true;
                self.walk(el, &style);
            }
            "i" | "em" | "cite" | "dfn" | "var" => {
                style.italic = true;
                self.walk(el, &style);
            }
            "u" | "ins" => {
                style.underline = true;
                self.walk(el, &style);
            }
            "s" | "strike" | "del" => {
                style.strike = true;
                self.walk(el, &style);
            }
            "code" | "kbd" | "samp" | "tt" => {
                style.mono = true;
                self.walk(el, &style);
            }
            "small" | "sup" | "sub" => {
                style.scale = (if style.scale > 0.0 { style.scale } else { 1.0 }) * 0.85;
                self.walk(el, &style);
            }
            "big" => {
                style.scale = (if style.scale > 0.0 { style.scale } else { 1.0 }) * 1.2;
                self.walk(el, &style);
            }
            "mark" => {
                style.bg = Some(theme::hex(0xFFF100));
                self.walk(el, &style);
            }
            "font" => {
                if let Some(sz) = v.attr("size").and_then(|s| s.trim().parse::<i32>().ok()) {
                    style.scale = match sz {
                        1 => 0.72,
                        2 => 0.86,
                        3 => 1.0,
                        4 => 1.2,
                        5 => 1.5,
                        6 => 1.8,
                        _ => 2.2,
                    };
                }
                self.walk(el, &style);
            }
            _ => self.walk(el, &style),
        }
    }

    fn image(&mut self, el: ElementRef<'_>, style: &RunStyle, css: &Css) {
        let v = el.value();
        let src = v.attr("src").unwrap_or("").trim().to_string();
        if src.is_empty() {
            return;
        }
        let width = v.attr("width").and_then(parse_len).or(css.width);
        let height = v.attr("height").and_then(parse_len).or(css.height);
        if width.map(|w| w <= 3.0).unwrap_or(false) || height.map(|h| h <= 3.0).unwrap_or(false) {
            return; // tracking pixel
        }
        if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("//") {
            self.remote_images += 1;
        }
        self.flush();
        self.blocks.push(Block::Image {
            src,
            alt: v.attr("alt").unwrap_or("").trim().to_string(),
            width,
            height,
            link: style.link.clone(),
            align: self.align,
            indent: self.indent,
            quote: self.quote,
        });
    }
}

pub fn parse_html(html: &str) -> HtmlDoc {
    let doc = Html::parse_document(html);
    let mut b = Builder::new();
    b.walk(doc.root_element(), &RunStyle::default());
    b.flush();
    while matches!(b.blocks.last(), Some(Block::Gap)) {
        b.blocks.pop();
    }
    HtmlDoc {
        blocks: b.blocks,
        remote_images: b.remote_images,
    }
}

/// Plain text with quote levels (`>`) and auto-linked URLs.
pub fn from_plain_text(text: &str) -> HtmlDoc {
    let mut blocks = Vec::new();
    let mut para: Vec<Run> = Vec::new();
    let mut para_quote = 0u8;
    let flush = |blocks: &mut Vec<Block>, para: &mut Vec<Run>, quote: u8| {
        if !para.is_empty() {
            if let Some(last) = para.last_mut() {
                while last.text.ends_with('\n') {
                    last.text.pop();
                }
            }
            blocks.push(Block::Text {
                runs: std::mem::take(para),
                align: Align::Min,
                indent: 0,
                quote,
                heading: 0,
                marker: None,
            });
        }
    };
    for raw in text.replace("\r\n", "\n").lines() {
        let mut line = raw;
        let mut q = 0u8;
        loop {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix('>') {
                q += 1;
                line = rest.strip_prefix(' ').unwrap_or(rest);
            } else {
                break;
            }
        }
        if q != para_quote {
            flush(&mut blocks, &mut para, para_quote);
            para_quote = q;
        }
        if line.trim().is_empty() {
            flush(&mut blocks, &mut para, para_quote);
            if !matches!(blocks.last(), Some(Block::Gap) | None) {
                blocks.push(Block::Gap);
            }
            continue;
        }
        let mut pos = 0;
        for (s, e) in util::find_links(line) {
            if s > pos {
                push_run(&mut para, &line[pos..s], RunStyle::default());
            }
            let url = &line[s..e];
            let href = if url.starts_with("www.") { format!("https://{url}") } else { url.to_string() };
            push_run(
                &mut para,
                url,
                RunStyle {
                    link: Some(href),
                    ..Default::default()
                },
            );
            pos = e;
        }
        if pos < line.len() {
            push_run(&mut para, &line[pos..], RunStyle::default());
        }
        push_run(&mut para, "\n", RunStyle::default());
    }
    flush(&mut blocks, &mut para, para_quote);
    HtmlDoc {
        blocks,
        remote_images: 0,
    }
}

fn push_run(runs: &mut Vec<Run>, text: &str, style: RunStyle) {
    match runs.last_mut() {
        Some(r) if r.style == style => r.text.push_str(text),
        _ => runs.push(Run {
            text: text.to_string(),
            style,
        }),
    }
}

// ----------------------------------------------------------------- rendering

pub struct RenderOpts<'a> {
    pub show_images: bool,
    /// content-id -> egui image uri
    pub cid: &'a HashMap<String, String>,
    pub base_size: f32,
}

pub enum HtmlAction {
    OpenLink(String),
    ShowImages,
}

pub(crate) fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.6,
        2 => 1.35,
        3 => 1.18,
        4 => 1.06,
        _ => 1.0,
    }
}

fn adapt(style: &RunStyle) -> (Color32, Color32) {
    let p = pal();
    let link = style.link.is_some();
    let bg = style.bg;
    let bg_kept = match bg {
        Some(b) => {
            let lum = luminance(b);
            if p.dark {
                if lum < 0.6 {
                    Some(b)
                } else if saturation(b) > 0.4 {
                    Some(mix(b, p.card, 0.65))
                } else {
                    None
                }
            } else if lum < 0.72 || saturation(b) > 0.45 {
                Some(b)
            } else {
                None
            }
        }
        None => None,
    };
    let dark_bg = bg_kept.map(|b| luminance(b) < 0.5).unwrap_or(p.dark);
    let mut fg = match style.color {
        Some(c) => c,
        None if link => p.accent_text,
        None => {
            if bg_kept.is_some() && dark_bg && !p.dark {
                Color32::WHITE
            } else {
                p.text
            }
        }
    };
    let lum = luminance(fg);
    if dark_bg && lum < 0.5 {
        fg = if p.dark { mix(fg, Color32::WHITE, 0.72) } else { Color32::WHITE };
    } else if !dark_bg && lum > 0.82 {
        fg = p.text;
    }
    (fg, bg_kept.unwrap_or(Color32::TRANSPARENT))
}

fn format_for(style: &RunStyle, base: f32, heading: u8) -> TextFormat {
    // An explicit CSS size wins over the default heading size.
    let scale = if style.scale > 0.0 { style.scale } else { heading_scale(heading) };
    let size = (base * scale).clamp(9.0, 40.0);
    let family = if style.mono {
        FontFamily::Monospace
    } else if style.bold {
        FontFamily::Name(theme::BOLD.into())
    } else {
        FontFamily::Proportional
    };
    let (color, background) = adapt(style);
    let line = if style.underline || style.link.is_some() {
        Stroke::new(1.0, color)
    } else {
        Stroke::NONE
    };
    TextFormat {
        font_id: FontId::new(size, family),
        color,
        background,
        italics: style.italic,
        underline: line,
        strikethrough: if style.strike { Stroke::new(1.0, color) } else { Stroke::NONE },
        line_height: Some(size * 1.42),
        ..Default::default()
    }
}

fn quote_bars(ui: &Ui, rect: Rect, quote: u8) {
    let p = pal();
    for q in 0..quote {
        let x = rect.left() + 2.0 + q as f32 * 14.0;
        ui.painter().vline(
            x,
            rect.y_range(),
            Stroke::new(3.0, if q % 2 == 0 { mix(p.accent, p.card, 0.55) } else { p.stroke_strong }),
        );
    }
}

/// Renders a document. Returns a requested action (clicked link…).
pub fn show(ui: &mut Ui, doc: &HtmlDoc, opts: &RenderOpts) -> Option<HtmlAction> {
    let p = pal();
    let mut action = None;
    let full_w = ui.available_width();
    ui.spacing_mut().item_spacing.y = 4.0;
    for (i, block) in doc.blocks.iter().enumerate() {
        match block {
            Block::Gap => ui.add_space(8.0),
            Block::Rule { quote } => {
                let (rect, _) = ui.allocate_exact_size(vec2(full_w, 13.0), Sense::hover());
                let left = rect.left() + *quote as f32 * 14.0;
                ui.painter().hline(left..=rect.right(), rect.center().y, Stroke::new(1.0, p.stroke));
                quote_bars(ui, rect, *quote);
            }
            Block::Text {
                runs,
                align,
                indent,
                quote,
                heading,
                marker,
            } => {
                let left = *quote as f32 * 14.0 + *indent as f32 * 22.0 + if marker.is_some() { 4.0 } else { 0.0 };
                let marker_w = if marker.is_some() { 18.0 } else { 0.0 };
                let wrap_w = (full_w - left - marker_w).max(60.0);
                let mut job = LayoutJob::default();
                job.wrap.max_width = wrap_w;
                job.halign = *align;
                let mut links: Vec<(usize, usize, String)> = Vec::new();
                let mut chars = 0usize;
                for r in runs {
                    let fmt = format_for(&r.style, opts.base_size, *heading);
                    let n = r.text.chars().count();
                    if let Some(l) = &r.style.link {
                        links.push((chars, chars + n, l.clone()));
                    }
                    chars += n;
                    job.append(&r.text, 0.0, fmt);
                }
                let galley = ui.painter().layout_job(job);
                let height = galley.size().y.max(4.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(full_w, height), Sense::click_and_drag());
                let text_left = rect.left() + left + marker_w;
                let galley_pos = match align {
                    Align::Center => Pos2::new(text_left + wrap_w / 2.0, rect.top()),
                    Align::Max => Pos2::new(text_left + wrap_w, rect.top()),
                    Align::Min => Pos2::new(text_left, rect.top()),
                };
                if let Some(m) = marker {
                    ui.painter().text(
                        Pos2::new(rect.left() + left + marker_w - 6.0, rect.top()),
                        egui::Align2::RIGHT_TOP,
                        m,
                        font(opts.base_size),
                        p.text2,
                    );
                }
                quote_bars(ui, rect, *quote);
                // Link hover / click.
                let mut hovered_link: Option<String> = None;
                if !links.is_empty() {
                    if let Some(pos) = resp.hover_pos() {
                        let rel = pos - galley_pos;
                        if galley.rect.expand(2.0).contains(Pos2::new(rel.x, rel.y)) {
                            let cursor = galley.cursor_from_pos(rel);
                            let crect = galley.pos_from_cursor(cursor);
                            let idx = if rel.x < crect.left() {
                                cursor.index.0.saturating_sub(1)
                            } else {
                                cursor.index.0
                            };
                            hovered_link = links
                                .iter()
                                .find(|(s, e, _)| idx >= *s && idx < *e)
                                .map(|(_, _, l)| l.clone());
                        }
                    }
                }
                let resp = if let Some(l) = &hovered_link {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    if resp.clicked() {
                        action = Some(HtmlAction::OpenLink(l.clone()));
                    }
                    resp.on_hover_text_at_pointer(util::truncate_chars(l, 120))
                } else {
                    resp
                };
                if ui.is_rect_visible(rect) {
                    egui::text_selection::LabelSelectionState::label_text_selection(
                        ui,
                        &resp,
                        galley_pos,
                        galley,
                        p.text,
                        Stroke::NONE,
                    );
                }
            }
            Block::Image {
                src,
                alt,
                width,
                height,
                link,
                align,
                indent,
                quote,
            } => {
                let left = *quote as f32 * 14.0 + *indent as f32 * 22.0;
                let avail = (full_w - left).max(40.0);
                let is_cid = src.to_ascii_lowercase().starts_with("cid:");
                let is_data = src.starts_with("data:");
                let uri = if is_cid {
                    opts.cid.get(&src[4..]).cloned()
                } else if is_data {
                    opts.cid.get(&format!("data:{i}")).cloned()
                } else if opts.show_images {
                    Some(if src.starts_with("//") { format!("https:{src}") } else { src.clone() })
                } else {
                    None
                };
                let target_w = width.unwrap_or(avail).min(avail);
                let target_h = match (width, height) {
                    (Some(w), Some(h)) if *w > 0.0 => Some(h * target_w / w),
                    (None, Some(h)) => Some(*h),
                    _ => None,
                };
                match uri {
                    Some(uri) => {
                        let mut img = egui::Image::new(uri)
                            .fit_to_original_size(1.0)
                            .max_width(target_w)
                            .corner_radius(CornerRadius::same(2))
                            .show_loading_spinner(true)
                            .sense(Sense::click());
                        if let Some(h) = target_h {
                            img = img.fit_to_exact_size(vec2(target_w, h));
                        }
                        // Lay out against the full pane width (a horizontal row
                        // would clamp the image to the row height).
                        let size = img
                            .load_and_calc_size(ui, vec2(target_w, f32::INFINITY))
                            .unwrap_or(vec2(target_w.min(320.0), target_h.unwrap_or(120.0)));
                        let (row, _) = ui.allocate_exact_size(vec2(full_w, size.y), Sense::hover());
                        let x = match align {
                            Align::Center => row.left() + left + (avail - size.x) / 2.0,
                            Align::Max => row.right() - size.x,
                            Align::Min => row.left() + left,
                        };
                        let r = ui.put(Rect::from_min_size(Pos2::new(x, row.top()), size), img);
                        if let Some(l) = link {
                            let r = r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(l.as_str());
                            if r.clicked() {
                                action = Some(HtmlAction::OpenLink(l.clone()));
                            }
                        } else if !alt.is_empty() {
                            r.on_hover_text(alt.as_str());
                        }
                    }
                    None => {
                        // Blocked remote image: show a small placeholder.
                        let h = target_h.unwrap_or(34.0).clamp(28.0, 120.0);
                        let w = if width.is_some() { target_w } else { avail.min(260.0) };
                        let (rect, resp) = ui.allocate_exact_size(vec2(left + w, h), Sense::click());
                        let r = Rect::from_min_size(Pos2::new(rect.left() + left, rect.top()), vec2(w, h));
                        ui.painter().rect_filled(r, CornerRadius::same(4), p.sunken);
                        ui.painter().rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, p.stroke), egui::StrokeKind::Inside);
                        let label = if alt.is_empty() { "Image blocked".to_string() } else { alt.clone() };
                        ui.painter().text(
                            Pos2::new(r.left() + 12.0, r.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{}  {}", icons::IMAGE, util::truncate_chars(&label, 60)),
                            icon_font(12.5),
                            p.text3,
                        );
                        if resp.on_hover_text("Click to show images in this message").clicked() {
                            action = Some(HtmlAction::ShowImages);
                        }
                    }
                }
            }
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(doc: &HtmlDoc) -> Vec<String> {
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { runs, .. } => Some(runs.iter().map(|r| r.text.as_str()).collect::<String>()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_structure() {
        let html = r#"<html><head><style>p{color:red}</style><title>x</title></head><body>
            <div style="display:none">hidden preheader</div>
            <h1>Title</h1>
            <p>Hello   <b>bold</b> and <a href="https://x.org/a">a link</a>.<br>Next line</p>
            <ul><li>One</li><li>Two</li></ul>
            <blockquote>Quoted</blockquote>
            <img src="https://t.example/p.gif" width="1" height="1">
            <img src="https://img.example/banner.png" alt="Banner" width="600">
            <script>alert(1)</script>
            </body></html>"#;
        let doc = parse_html(html);
        let texts = text_of(&doc);
        assert_eq!(texts[0], "Title");
        assert_eq!(texts[1], "Hello bold and a link.\nNext line");
        assert_eq!(texts[2], "One");
        assert!(texts.iter().all(|t| !t.contains("hidden") && !t.contains("alert")));
        assert_eq!(doc.remote_images, 1, "tracking pixel is ignored");
        let link = doc.blocks.iter().find_map(|b| match b {
            Block::Text { runs, .. } => runs.iter().find_map(|r| r.style.link.clone()),
            _ => None,
        });
        assert_eq!(link.as_deref(), Some("https://x.org/a"));
        assert!(doc.blocks.iter().any(|b| matches!(b, Block::Text { quote: 1, .. })));
    }

    #[test]
    fn plain_text_quotes_and_links() {
        let doc = from_plain_text("Hi,\nsee https://example.com.\n\n> quoted\n>> deeper");
        let texts = text_of(&doc);
        assert_eq!(texts[0], "Hi,\nsee https://example.com.");
        assert!(doc.blocks.iter().any(|b| matches!(b, Block::Text { quote: 2, .. })));
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color32::WHITE));
        assert_eq!(parse_color("rgb(255, 0, 0)"), Some(Color32::from_rgb(255, 0, 0)));
        assert_eq!(parse_color("Navy"), Some(theme::hex(0x000080)));
    }
}
