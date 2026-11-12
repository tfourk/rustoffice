//! Saves a message as a PDF: a header block (subject, people, date,
//! attachments) followed by the body, laid out from the same block model the
//! reading pane renders ([`HtmlDoc`]). Text stays selectable and searchable,
//! links stay clickable, and inline images are embedded. Remote images are
//! never fetched; their alt text is printed instead.
//!
//! Layout ([`layout`]) and writing ([`write`]) are separate so the layout can
//! be tested without parsing a PDF.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use egui::{Align, Color32};
use krilla::action::LinkAction;
use krilla::annotation::{LinkAnnotation, Target};
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point, Rect, Size, Transform};
use krilla::image::Image;
use krilla::metadata::Metadata;
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::text::{Font, GlyphId, KrillaGlyph};
use krilla::Document;

use crate::fonts::{ExportFonts, FaceData};
use crate::model::Addr;
use crate::ui::html::{heading_scale, luminance, saturation, Block, HtmlDoc, Run, RunStyle};
use crate::util;

/// CSS pixels to PDF points; the reading pane's sizes are in pixels.
const PX: f32 = 0.75;
/// Body text size (the reading pane's 14 px).
const BASE: f32 = 10.5;
const MARGIN: f32 = 54.0;
const LINE_HEIGHT: f32 = 1.42;
const BLOCK_GAP: f32 = 4.0 * PX;
const HEADER_LABEL_W: f32 = 72.0;
/// Slant of synthesised italics (x shift per unit of height).
const SKEW: f32 = 0.2;

pub(crate) type Rgb = [u8; 3];
const TEXT: Rgb = [0x24, 0x24, 0x24];
const MUTED: Rgb = [0x61, 0x61, 0x61];
const LINK: Rgb = [0x0F, 0x6C, 0xBD];
const RULE: Rgb = [0xD1, 0xD1, 0xD1];
const QUOTE_BARS: [Rgb; 2] = [[0x93, 0xBE, 0xE2], [0xC7, 0xC7, 0xC7]];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    A4,
    Letter,
}

impl PageSize {
    /// Letter in the Americas, A4 everywhere else (going by the time zone,
    /// which is the one regional setting available on every platform).
    pub fn for_region() -> PageSize {
        match iana_time_zone::get_timezone() {
            Ok(tz) if tz.starts_with("America/") || tz.starts_with("US/") || tz.starts_with("Canada/") => PageSize::Letter,
            _ => PageSize::A4,
        }
    }

    fn points(self) -> (f32, f32) {
        match self {
            PageSize::A4 => (595.28, 841.89),
            PageSize::Letter => (612.0, 792.0),
        }
    }
}

/// Everything that goes into the PDF of one message.
pub struct PdfMessage<'a> {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: String,
    /// Attachment names, e.g. "slides.pptx (2.1 MB)".
    pub attachments: Vec<String>,
    pub doc: &'a HtmlDoc,
    /// Image bytes by content-id (for `cid:` sources) and by `data:{block
    /// index}` (for `data:` sources); see [`collect_images`].
    pub images: HashMap<String, Vec<u8>>,
}

/// The images a message carries itself: inline (`cid:`) parts and `data:` URIs.
pub fn collect_images(raw: &[u8], doc: &HtmlDoc) -> HashMap<String, Vec<u8>> {
    use base64::Engine;
    let mut map: HashMap<String, Vec<u8>> = crate::mime::inline_parts(raw).into_iter().collect();
    for (i, block) in doc.blocks.iter().enumerate() {
        let Block::Image { src, .. } = block else { continue };
        let Some((meta, data)) = src.strip_prefix("data:").and_then(|rest| rest.split_once(',')) else { continue };
        if meta.ends_with(";base64") {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim()) {
                map.insert(format!("data:{i}"), bytes);
            }
        }
    }
    map
}

/// The PDF of a raw (RFC 5322) message, with the system's fonts. `subject`
/// and `date` are the ones the message list shows; the message's own are
/// used where they are missing.
pub fn message_pdf(raw: &[u8], subject: Option<&str>, date: Option<i64>, size: PageSize) -> Result<Vec<u8>> {
    message_pdf_with(raw, subject, date, size, &ExportFonts::system())
}

pub fn message_pdf_with(raw: &[u8], subject: Option<&str>, date: Option<i64>, size: PageSize, fonts: &ExportFonts) -> Result<Vec<u8>> {
    with_message(raw, subject, date, |msg| render_with(msg, size, fonts))
}

/// Reads a raw message into the form the exporters work from (header
/// fields, body blocks, the images it carries) and hands it to `f`.
pub fn with_message<T>(raw: &[u8], subject: Option<&str>, date: Option<i64>, f: impl FnOnce(&PdfMessage) -> Result<T>) -> Result<T> {
    let parsed = crate::mime::parse_full(raw).ok_or_else(|| anyhow!("the message could not be read"))?;
    let doc = match &parsed.html {
        Some(h) => crate::ui::html::parse_html(h),
        None => crate::ui::html::from_plain_text(parsed.text.as_deref().unwrap_or("")),
    };
    let msg = PdfMessage {
        subject: subject.filter(|s| !s.trim().is_empty()).unwrap_or(&parsed.subject).to_string(),
        from: Addr::list_to_string(&parsed.from),
        to: Addr::list_to_string(&parsed.to),
        cc: Addr::list_to_string(&parsed.cc),
        date: util::fmt_full_date(date.filter(|d| *d > 0).unwrap_or(parsed.date)),
        attachments: parsed
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| format!("{} ({})", a.name, util::human_size(a.size)))
            .collect(),
        images: collect_images(raw, &doc),
        doc: &doc,
    };
    f(&msg)
}

pub fn render_with(msg: &PdfMessage, size: PageSize, fonts: &ExportFonts) -> Result<Vec<u8>> {
    let faces = Faces::new(fonts)?;
    let pages = layout(msg, size, &faces);
    write(msg, size, &faces, &pages)
}

// -------------------------------------------------------------------- fonts

struct Face {
    data: FaceData,
    font: Font,
    bold: bool,
    italic: bool,
}

/// The fonts of one document. `regular` … `mono` index into `all`; a style
/// the system lacks points at the nearest face and is synthesised.
struct Faces {
    all: Vec<Face>,
    regular: usize,
    bold: usize,
    italic: usize,
    bold_italic: usize,
    mono: usize,
    fallbacks: Vec<usize>,
}

impl Faces {
    fn new(fonts: &ExportFonts) -> Result<Faces> {
        let mut all: Vec<Face> = Vec::new();
        let mut add = |fd: &FaceData, bold: bool, italic: bool| -> Option<usize> {
            // Both the shaper and the PDF writer must be able to read it.
            rustybuzz::Face::from_slice(&fd.data, fd.index)?;
            let font = Font::new(fd.data.clone().into(), fd.index)?;
            all.push(Face {
                data: fd.clone(),
                font,
                bold,
                italic,
            });
            Some(all.len() - 1)
        };
        let bundled = ExportFonts::bundled();
        let regular = add(&fonts.regular, false, false)
            .or_else(|| add(&bundled.regular, false, false))
            .ok_or_else(|| anyhow!("no usable text font"))?;
        let bold = fonts.bold.as_ref().and_then(|f| add(f, true, false)).unwrap_or(regular);
        let italic = fonts.italic.as_ref().and_then(|f| add(f, false, true)).unwrap_or(regular);
        let bold_italic = fonts
            .bold_italic
            .as_ref()
            .and_then(|f| add(f, true, true))
            .unwrap_or(if bold != regular { bold } else { italic });
        let mono = add(&fonts.mono, false, false).unwrap_or(regular);
        let fallbacks = fonts.fallbacks.iter().filter_map(|f| add(f, false, false)).collect();
        Ok(Faces {
            all,
            regular,
            bold,
            italic,
            bold_italic,
            mono,
            fallbacks,
        })
    }
}

/// Shapes text and measures it, with font fallback per character.
struct Shaper<'a> {
    faces: &'a Faces,
    rb: Vec<rustybuzz::Face<'a>>,
}

impl<'a> Shaper<'a> {
    fn new(faces: &'a Faces) -> Shaper<'a> {
        let rb = faces
            .all
            .iter()
            .map(|f| rustybuzz::Face::from_slice(&f.data.data, f.data.index).expect("checked in Faces::new"))
            .collect();
        Shaper { faces, rb }
    }

    fn primary(&self, fmt: &Fmt) -> usize {
        match (fmt.mono, fmt.bold, fmt.italic) {
            (true, _, _) => self.faces.mono,
            (false, true, true) => self.faces.bold_italic,
            (false, true, false) => self.faces.bold,
            (false, false, true) => self.faces.italic,
            (false, false, false) => self.faces.regular,
        }
    }

    fn has(&self, face: usize, c: char) -> bool {
        self.rb[face].glyph_index(c).is_some()
    }

    /// The face to draw `c` with: the style's own, else the first fallback
    /// that has the character.
    fn pick(&self, primary: usize, c: char) -> usize {
        if self.has(primary, c) {
            return primary;
        }
        std::iter::once(self.faces.regular)
            .chain(self.faces.fallbacks.iter().copied())
            .find(|&f| self.has(f, c))
            .unwrap_or(primary)
    }

    /// (ascent, descent) of the text font as fractions of the font size.
    fn metrics(&self) -> (f32, f32) {
        let f = &self.rb[self.faces.regular];
        let upem = f.units_per_em() as f32;
        (f.ascender() as f32 / upem, -(f.descender() as f32) / upem)
    }

    fn shape(&self, face: usize, text: &str, fmt: &Fmt) -> Seg {
        let rb = &self.rb[face];
        let upem = rb.units_per_em() as f32;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        let out = rustybuzz::shape(rb, &[], buffer);
        // A glyph covers the text from its cluster to the next higher one.
        let mut clusters: Vec<usize> = out.glyph_infos().iter().map(|i| i.cluster as usize).collect();
        clusters.sort_unstable();
        clusters.dedup();
        let mut advance = 0.0;
        let glyphs = out
            .glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(info, pos)| {
                let start = info.cluster as usize;
                let end = clusters.iter().copied().find(|&c| c > start).unwrap_or(text.len());
                advance += pos.x_advance as f32 / upem;
                KrillaGlyph::new(
                    GlyphId::new(info.glyph_id),
                    pos.x_advance as f32 / upem,
                    pos.x_offset as f32 / upem,
                    pos.y_offset as f32 / upem,
                    pos.y_advance as f32 / upem,
                    start..end,
                    None,
                )
            })
            .collect();
        let f = &self.faces.all[face];
        Seg {
            face,
            text: text.to_string(),
            glyphs,
            width: advance * fmt.size,
            skew: fmt.italic && !f.italic,
            embolden: fmt.bold && !f.bold,
        }
    }

    /// Splits text where the font changes and shapes each piece.
    fn segments(&self, text: &str, fmt: &Fmt) -> Vec<Seg> {
        let primary = self.primary(fmt);
        let mut pieces: Vec<(usize, String)> = Vec::new();
        for c in text.chars() {
            let face = match pieces.last() {
                Some((f, _)) if joins_previous(c) => *f,
                _ => self.pick(primary, c),
            };
            match pieces.last_mut() {
                Some((f, s)) if *f == face => s.push(c),
                _ => pieces.push((face, c.to_string())),
            }
        }
        pieces.iter().map(|(face, s)| self.shape(*face, s, fmt)).collect()
    }
}

/// Combining marks, joiners and variation selectors stay with the character
/// before them (and in its font).
fn joins_previous(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x200D | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0x20D0..=0x20FF)
}

/// Scripts written without spaces: a line may break after any character.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

// ------------------------------------------------------------------- layout

/// Resolved style of a run.
#[derive(Clone, Debug)]
struct Fmt {
    size: f32,
    color: Rgb,
    bg: Option<Rgb>,
    bold: bool,
    italic: bool,
    mono: bool,
    underline: bool,
    strike: bool,
    link: Option<String>,
}

fn rgb_of(c: Color32) -> Rgb {
    [c.r(), c.g(), c.b()]
}

/// Links worth keeping in a document that is read outside the app.
pub(crate) fn external_link(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:", "tel:"]
        .iter()
        .any(|p| lower.starts_with(p))
        .then(|| url.trim().to_string())
}

/// The light-theme colours of a run, as the reading pane picks them: text
/// stays readable whatever the e-mail asked for.
pub(crate) fn colors(style: &RunStyle) -> (Rgb, Option<Rgb>) {
    let bg = style.bg.filter(|b| luminance(*b) < 0.72 || saturation(*b) > 0.45);
    let dark_bg = bg.map(|b| luminance(b) < 0.5).unwrap_or(false);
    let mut fg = match style.color {
        Some(c) => rgb_of(c),
        None if style.link.is_some() => LINK,
        None if dark_bg => [255, 255, 255],
        None => TEXT,
    };
    let lum = luminance(Color32::from_rgb(fg[0], fg[1], fg[2]));
    if dark_bg && lum < 0.5 {
        fg = [255, 255, 255];
    } else if !dark_bg && lum > 0.82 {
        fg = TEXT;
    }
    (fg, bg.map(rgb_of))
}

fn fmt_for(style: &RunStyle, heading: u8) -> Fmt {
    // An explicit CSS size wins over the default heading size.
    let scale = if style.scale > 0.0 { style.scale } else { heading_scale(heading) };
    let (color, bg) = colors(style);
    Fmt {
        size: (BASE * scale).clamp(9.0 * PX, 40.0 * PX),
        color,
        bg,
        bold: style.bold,
        italic: style.italic,
        mono: style.mono,
        underline: style.underline || style.link.is_some(),
        strike: style.strike,
        link: style.link.as_deref().and_then(external_link),
    }
}

/// A shaped piece of text in one font.
#[derive(Clone)]
struct Seg {
    face: usize,
    text: String,
    glyphs: Vec<KrillaGlyph>,
    width: f32,
    skew: bool,
    embolden: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtomKind {
    Word,
    Space,
    Break,
}

/// The unit of line breaking: a word, a run of spaces or a forced break.
#[derive(Clone)]
struct Atom {
    kind: AtomKind,
    /// Index into the paragraph's formats.
    fmt: usize,
    segs: Vec<Seg>,
    width: f32,
    /// No line break is allowed before this word (it continues the previous
    /// one in a different style).
    glue: bool,
}

/// What ends up on a page. Coordinates are points from the top-left corner.
pub(crate) enum Op {
    Text {
        x: f32,
        /// Baseline.
        y: f32,
        face: usize,
        size: f32,
        color: Rgb,
        glyphs: Vec<KrillaGlyph>,
        text: String,
        skew: bool,
        embolden: bool,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgb,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Rgb,
    },
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        image: Image,
    },
    Link {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        url: String,
    },
}

struct Para<'p> {
    runs: &'p [Run],
    align: Align,
    /// Left edge and width of the text column.
    left: f32,
    width: f32,
    heading: u8,
    quote: u8,
    marker: Option<&'p str>,
}

struct Layout<'a> {
    sh: Shaper<'a>,
    pages: Vec<Vec<Op>>,
    y: f32,
    page_w: f32,
    top: f32,
    bottom: f32,
}

impl Layout<'_> {
    fn content_w(&self) -> f32 {
        self.page_w - 2.0 * MARGIN
    }

    fn push(&mut self, op: Op) {
        self.pages.last_mut().expect("a page").push(op);
    }

    /// Starts a new page unless `h` more points fit on this one.
    fn ensure(&mut self, h: f32) {
        if self.y + h > self.bottom && self.y > self.top + 0.1 {
            self.pages.push(Vec::new());
            self.y = self.top;
        }
    }

    fn quote_bars(&mut self, top: f32, height: f32, quote: u8) {
        for q in 0..quote {
            let x = MARGIN + (2.0 + q as f32 * 14.0) * PX;
            self.push(Op::Line {
                x1: x,
                y1: top,
                x2: x,
                y2: top + height,
                width: 3.0 * PX,
                color: QUOTE_BARS[(q % 2) as usize],
            });
        }
    }

    fn atom(&self, kind: AtomKind, text: &str, fmt: usize, fmts: &[Fmt], glue: bool) -> Atom {
        let segs = self.sh.segments(text, &fmts[fmt]);
        Atom {
            kind,
            fmt,
            width: segs.iter().map(|s| s.width).sum(),
            segs,
            glue,
        }
    }

    /// Splits runs into words, spaces and breaks.
    fn atoms(&self, runs: &[Run], heading: u8) -> (Vec<Atom>, Vec<Fmt>) {
        let mut fmts: Vec<Fmt> = Vec::new();
        let mut atoms: Vec<Atom> = Vec::new();
        // A word that directly follows another one continues it (in a new
        // style) unless a break is allowed at this position.
        let mut break_at = usize::MAX;
        for run in runs {
            fmts.push(fmt_for(&run.style, heading));
            let fi = fmts.len() - 1;
            let mut word = String::new();
            let mut spaces = String::new();
            macro_rules! flush {
                () => {
                    if !word.is_empty() {
                        let glue = break_at != atoms.len() && atoms.last().is_some_and(|a| a.kind == AtomKind::Word);
                        atoms.push(self.atom(AtomKind::Word, &word, fi, &fmts, glue));
                        word.clear();
                    }
                    if !spaces.is_empty() {
                        atoms.push(self.atom(AtomKind::Space, &spaces, fi, &fmts, false));
                        spaces.clear();
                    }
                };
            }
            for c in run.text.chars() {
                match c {
                    '\n' => {
                        flush!();
                        atoms.push(Atom {
                            kind: AtomKind::Break,
                            fmt: fi,
                            segs: Vec::new(),
                            width: 0.0,
                            glue: false,
                        });
                    }
                    // Zero-width space: a break opportunity and nothing else.
                    '\u{200B}' => {
                        flush!();
                        break_at = atoms.len();
                    }
                    '\r' | '\u{AD}' | '\u{FEFF}' => {}
                    '\t' => {
                        if !word.is_empty() {
                            flush!();
                        }
                        spaces.push_str("    ");
                    }
                    // Non-breaking spaces stay inside their word.
                    c if c.is_whitespace() && c != '\u{A0}' && c != '\u{202F}' => {
                        if !word.is_empty() {
                            flush!();
                        }
                        spaces.push(' ');
                    }
                    c if c.is_control() => {}
                    c if is_cjk(c) => {
                        flush!();
                        atoms.push(self.atom(AtomKind::Word, c.encode_utf8(&mut [0; 4]), fi, &fmts, false));
                        break_at = atoms.len();
                    }
                    c => {
                        if !spaces.is_empty() {
                            flush!();
                        }
                        word.push(c);
                    }
                }
            }
            flush!();
        }
        (atoms, fmts)
    }

    /// Breaks a word that is wider than the column into single characters.
    fn split_chars(&self, atom: &Atom, fmts: &[Fmt]) -> Vec<Atom> {
        let mut out = Vec::new();
        for seg in &atom.segs {
            let mut cluster = String::new();
            let mut emit = |cluster: &mut String| {
                if !cluster.is_empty() {
                    let s = self.sh.shape(seg.face, cluster, &fmts[atom.fmt]);
                    out.push(Atom {
                        kind: AtomKind::Word,
                        fmt: atom.fmt,
                        width: s.width,
                        segs: vec![s],
                        glue: true,
                    });
                    cluster.clear();
                }
            };
            for c in seg.text.chars() {
                if !joins_previous(c) {
                    emit(&mut cluster);
                }
                cluster.push(c);
            }
            emit(&mut cluster);
        }
        out
    }

    fn paragraph(&mut self, p: &Para) {
        let (atoms, fmts) = self.atoms(p.runs, p.heading);
        if !atoms.iter().any(|a| a.kind == AtomKind::Word) {
            return;
        }
        let mut line: Vec<Atom> = Vec::new();
        let mut line_w = 0.0f32;
        // The current line began because the previous one was full.
        let mut wrapped = false;
        let mut first = true;
        let mut i = 0;
        while i < atoms.len() {
            let a = &atoms[i];
            match a.kind {
                AtomKind::Break => {
                    self.flush_line(&mut line, p, &fmts, a.fmt, &mut first);
                    line_w = 0.0;
                    wrapped = false;
                    i += 1;
                }
                AtomKind::Space => {
                    // Spaces at a wrap point disappear; indentation stays.
                    if !(wrapped && line.is_empty()) {
                        line_w += a.width;
                        line.push(a.clone());
                    }
                    i += 1;
                }
                AtomKind::Word => {
                    let mut j = i + 1;
                    while j < atoms.len() && atoms[j].kind == AtomKind::Word && atoms[j].glue {
                        j += 1;
                    }
                    let unit = &atoms[i..j];
                    let w: f32 = unit.iter().map(|a| a.width).sum();
                    let has_word = line.iter().any(|a| a.kind == AtomKind::Word);
                    if line_w + w > p.width + 0.01 && has_word {
                        self.flush_line(&mut line, p, &fmts, a.fmt, &mut first);
                        line_w = 0.0;
                        wrapped = true;
                    }
                    if w > p.width {
                        let pieces: Vec<Atom> = unit.iter().flat_map(|a| self.split_chars(a, &fmts)).collect();
                        for piece in pieces {
                            if line_w + piece.width > p.width + 0.01 && !line.is_empty() {
                                self.flush_line(&mut line, p, &fmts, piece.fmt, &mut first);
                                line_w = 0.0;
                                wrapped = true;
                            }
                            line_w += piece.width;
                            line.push(piece);
                        }
                    } else {
                        line_w += w;
                        line.extend(unit.iter().cloned());
                    }
                    i = j;
                }
            }
        }
        if !line.is_empty() {
            let fmt = line[0].fmt;
            self.flush_line(&mut line, p, &fmts, fmt, &mut first);
        }
    }

    /// Places one line on the page. `empty_fmt` gives the height of a line
    /// without text.
    fn flush_line(&mut self, line: &mut Vec<Atom>, p: &Para, fmts: &[Fmt], empty_fmt: usize, first: &mut bool) {
        while line.last().is_some_and(|a| a.kind == AtomKind::Space) {
            line.pop();
        }
        let size = line.iter().map(|a| fmts[a.fmt].size).fold(0.0f32, f32::max);
        let size = if size > 0.0 { size } else { fmts[empty_fmt].size };
        let height = size * LINE_HEIGHT;
        self.ensure(height);
        let top = self.y;
        let (ascent, descent) = self.sh.metrics();
        let baseline = top + (height - (ascent + descent) * size) / 2.0 + ascent * size;
        let width: f32 = line.iter().map(|a| a.width).sum();
        let x0 = match p.align {
            Align::Min => p.left,
            Align::Center => p.left + ((p.width - width) / 2.0).max(0.0),
            Align::Max => p.left + (p.width - width).max(0.0),
        };
        self.quote_bars(top, height, p.quote);
        if let (true, Some(marker)) = (*first, p.marker) {
            let fmt = Fmt {
                color: MUTED,
                ..fmt_for(&RunStyle::default(), 0)
            };
            let segs = self.sh.segments(marker, &fmt);
            let w: f32 = segs.iter().map(|s| s.width).sum();
            self.text_segs(&segs, p.left - 6.0 * PX - w, baseline, &fmt);
        }
        *first = false;

        // Backgrounds go below all text of the line.
        let mut x = x0;
        for a in line.iter() {
            if let Some(color) = fmts[a.fmt].bg {
                self.push(Op::Rect {
                    x,
                    y: top,
                    w: a.width,
                    h: height,
                    color,
                });
            }
            x += a.width;
        }
        let mut x = x0;
        // A link spanning several atoms becomes one clickable area.
        let mut link: Option<(String, f32, f32)> = None;
        for a in line.iter() {
            let fmt = &fmts[a.fmt];
            self.text_segs(&a.segs, x, baseline, fmt);
            let line_w = (fmt.size * 0.06).max(0.4);
            for (on, dy) in [(fmt.underline, fmt.size * 0.13), (fmt.strike, -fmt.size * 0.28)] {
                if on {
                    self.push(Op::Line {
                        x1: x,
                        y1: baseline + dy,
                        x2: x + a.width,
                        y2: baseline + dy,
                        width: line_w,
                        color: fmt.color,
                    });
                }
            }
            match (&mut link, &fmt.link) {
                (Some((url, _, end)), Some(l)) if url == l => *end = x + a.width,
                _ => {
                    if let Some((url, start, end)) = link.take() {
                        self.push(Op::Link {
                            x: start,
                            y: top,
                            w: end - start,
                            h: height,
                            url,
                        });
                    }
                    link = fmt.link.clone().map(|l| (l, x, x + a.width));
                }
            }
            x += a.width;
        }
        if let Some((url, start, end)) = link {
            self.push(Op::Link {
                x: start,
                y: top,
                w: end - start,
                h: height,
                url,
            });
        }
        self.y += height;
        line.clear();
    }

    fn text_segs(&mut self, segs: &[Seg], mut x: f32, baseline: f32, fmt: &Fmt) {
        for s in segs {
            self.push(Op::Text {
                x,
                y: baseline,
                face: s.face,
                size: fmt.size,
                color: fmt.color,
                glyphs: s.glyphs.clone(),
                text: s.text.clone(),
                skew: s.skew,
                embolden: s.embolden,
            });
            x += s.width;
        }
    }

    /// One paragraph in a single style.
    fn plain(&mut self, text: &str, style: RunStyle, left: f32, width: f32) {
        let runs = [Run {
            text: text.to_string(),
            style,
        }];
        self.paragraph(&Para {
            runs: &runs,
            align: Align::Min,
            left,
            width,
            heading: 0,
            quote: 0,
            marker: None,
        });
    }

    fn header(&mut self, msg: &PdfMessage) {
        let subject = if msg.subject.trim().is_empty() { "(no subject)" } else { msg.subject.trim() };
        self.plain(
            subject,
            RunStyle {
                bold: true,
                scale: 16.0 / BASE,
                ..Default::default()
            },
            MARGIN,
            self.content_w(),
        );
        self.y += 6.0;
        let small = 9.5 / BASE;
        let attachments = msg.attachments.join("; ");
        let rows = [
            ("From:", msg.from.as_str()),
            ("To:", msg.to.as_str()),
            ("Cc:", msg.cc.as_str()),
            ("Date:", msg.date.as_str()),
            ("Attachments:", attachments.as_str()),
        ];
        for (label, value) in rows {
            if value.trim().is_empty() {
                continue;
            }
            // Label and the value's first line share a line (and a page).
            self.ensure(BASE * small * LINE_HEIGHT);
            let y = self.y;
            self.plain(
                label,
                RunStyle {
                    scale: small,
                    color: Some(Color32::from_rgb(MUTED[0], MUTED[1], MUTED[2])),
                    ..Default::default()
                },
                MARGIN,
                HEADER_LABEL_W,
            );
            self.y = y;
            self.plain(
                value.trim(),
                RunStyle {
                    scale: small,
                    ..Default::default()
                },
                MARGIN + HEADER_LABEL_W,
                self.content_w() - HEADER_LABEL_W,
            );
            self.y += 1.5;
        }
        self.y += 7.0;
        self.rule(0, 1.0);
        self.y += 12.0;
    }

    fn rule(&mut self, quote: u8, height: f32) {
        self.ensure(height);
        self.quote_bars(self.y, height, quote);
        let y = self.y + height / 2.0;
        self.push(Op::Line {
            x1: MARGIN + quote as f32 * 14.0 * PX,
            y1: y,
            x2: MARGIN + self.content_w(),
            y2: y,
            width: 0.75,
            color: RULE,
        });
        self.y += height;
    }

    fn body(&mut self, msg: &PdfMessage) {
        let full = self.content_w();
        for (i, block) in msg.doc.blocks.iter().enumerate() {
            match block {
                Block::Gap => self.y += 8.0 * PX,
                Block::Rule { quote } => {
                    self.rule(*quote, 13.0 * PX);
                    self.y += BLOCK_GAP;
                }
                Block::Text {
                    runs,
                    align,
                    indent,
                    quote,
                    heading,
                    marker,
                } => {
                    let marker_w = if marker.is_some() { 22.0 * PX } else { 0.0 };
                    let left = (*quote as f32 * 14.0 + *indent as f32 * 22.0) * PX + marker_w;
                    self.paragraph(&Para {
                        runs,
                        align: *align,
                        left: MARGIN + left,
                        width: (full - left).max(60.0 * PX),
                        heading: *heading,
                        quote: *quote,
                        marker: marker.as_deref(),
                    });
                    self.y += BLOCK_GAP;
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
                    let left = (*quote as f32 * 14.0 + *indent as f32 * 22.0) * PX;
                    let avail = (full - left).max(40.0 * PX);
                    let bytes = if src.to_ascii_lowercase().starts_with("cid:") {
                        msg.images.get(&src[4..])
                    } else if src.starts_with("data:") {
                        msg.images.get(&format!("data:{i}"))
                    } else {
                        None
                    };
                    let link = link.as_deref().and_then(external_link);
                    match bytes.and_then(|b| load_image(b)) {
                        Some(image) => {
                            let (w, h) = self.image_size(&image, *width, *height, avail);
                            self.ensure(h);
                            let x = MARGIN
                                + match align {
                                    Align::Min => left,
                                    Align::Center => left + (avail - w) / 2.0,
                                    Align::Max => full - w,
                                };
                            self.quote_bars(self.y, h, *quote);
                            self.push(Op::Image {
                                x,
                                y: self.y,
                                w,
                                h,
                                image,
                            });
                            if let Some(url) = link {
                                self.push(Op::Link { x, y: self.y, w, h, url });
                            }
                            self.y += h + BLOCK_GAP;
                        }
                        // Not part of the message (or unreadable): say what
                        // was there rather than leave a hole.
                        None if !alt.trim().is_empty() => {
                            let runs = [Run {
                                text: format!("[{}]", alt.trim()),
                                style: RunStyle {
                                    italic: true,
                                    color: Some(Color32::from_rgb(MUTED[0], MUTED[1], MUTED[2])),
                                    link,
                                    ..Default::default()
                                },
                            }];
                            self.paragraph(&Para {
                                runs: &runs,
                                align: *align,
                                left: MARGIN + left,
                                width: avail,
                                heading: 0,
                                quote: *quote,
                                marker: None,
                            });
                            self.y += BLOCK_GAP;
                        }
                        None => {}
                    }
                }
            }
        }
    }

    /// The size an image is drawn at: the size the e-mail asks for (else its
    /// own), scaled down to fit the column and a page.
    fn image_size(&self, image: &Image, width: Option<f32>, height: Option<f32>, avail: f32) -> (f32, f32) {
        let (nw, nh) = image.size();
        let (nw, nh) = ((nw.max(1) as f32) * PX, (nh.max(1) as f32) * PX);
        let (mut w, mut h) = match (width.filter(|w| *w > 0.0), height.filter(|h| *h > 0.0)) {
            (Some(w), Some(h)) => (w * PX, h * PX),
            (Some(w), None) => (w * PX, nh * w * PX / nw),
            (None, Some(h)) => (nw * h * PX / nh, h * PX),
            (None, None) => (nw, nh),
        };
        let max_h = self.bottom - self.top;
        let fit = (avail / w).min(max_h / h).min(1.0);
        w *= fit;
        h *= fit;
        (w.max(1.0), h.max(1.0))
    }

    fn footers(&mut self) {
        let total = self.pages.len();
        let fmt = Fmt {
            size: 8.0,
            color: MUTED,
            ..fmt_for(&RunStyle::default(), 0)
        };
        let y = self.bottom + (MARGIN - 8.0) / 2.0 + 8.0;
        for n in 0..total {
            let segs = self.sh.segments(&format!("Page {} of {total}", n + 1), &fmt);
            let w: f32 = segs.iter().map(|s| s.width).sum();
            let mut x = (self.page_w - w) / 2.0;
            for s in segs {
                let width = s.width;
                self.pages[n].push(Op::Text {
                    x,
                    y,
                    face: s.face,
                    size: fmt.size,
                    color: fmt.color,
                    glyphs: s.glyphs,
                    text: s.text,
                    skew: false,
                    embolden: false,
                });
                x += width;
            }
        }
    }
}

/// JPEG and PNG data is embedded as it is; other formats are decoded first.
fn load_image(bytes: &[u8]) -> Option<Image> {
    let direct = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Jpeg => Image::from_jpeg(bytes.to_vec().into(), true).ok(),
        image::ImageFormat::Png => Image::from_png(bytes.to_vec().into(), true).ok(),
        _ => None,
    };
    direct.or_else(|| {
        let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w > 0 && h > 0).then(|| Image::from_rgba8(rgba.into_raw(), w, h))
    })
}

fn layout(msg: &PdfMessage, size: PageSize, faces: &Faces) -> Vec<Vec<Op>> {
    let (page_w, page_h) = size.points();
    let mut l = Layout {
        sh: Shaper::new(faces),
        pages: vec![Vec::new()],
        y: MARGIN,
        page_w,
        top: MARGIN,
        bottom: page_h - MARGIN,
    };
    l.header(msg);
    l.body(msg);
    l.footers();
    l.pages
}

// ------------------------------------------------------------------ writing

fn fill(c: Rgb) -> Fill {
    Fill {
        paint: rgb::Color::new(c[0], c[1], c[2]).into(),
        ..Default::default()
    }
}

fn stroke(c: Rgb, width: f32) -> Stroke {
    Stroke {
        paint: rgb::Color::new(c[0], c[1], c[2]).into(),
        width,
        ..Default::default()
    }
}

fn write(msg: &PdfMessage, size: PageSize, faces: &Faces, pages: &[Vec<Op>]) -> Result<Vec<u8>> {
    let (page_w, page_h) = size.points();
    let mut doc = Document::new();
    for ops in pages {
        let settings = PageSettings::from_wh(page_w, page_h).ok_or_else(|| anyhow!("bad page size"))?;
        let mut page = doc.start_page_with(settings);
        let mut surface = page.surface();
        for op in ops {
            match op {
                Op::Rect { x, y, w, h, color } => {
                    let Some(rect) = Rect::from_xywh(*x, *y, *w, *h) else { continue };
                    let mut pb = PathBuilder::new();
                    pb.push_rect(rect);
                    let Some(path) = pb.finish() else { continue };
                    surface.set_fill(Some(fill(*color)));
                    surface.set_stroke(None);
                    surface.draw_path(&path);
                }
                Op::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    width,
                    color,
                } => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(*x1, *y1);
                    pb.line_to(*x2, *y2);
                    let Some(path) = pb.finish() else { continue };
                    surface.set_fill(None);
                    surface.set_stroke(Some(stroke(*color, *width)));
                    surface.draw_path(&path);
                }
                Op::Text {
                    x,
                    y,
                    face,
                    size,
                    color,
                    glyphs,
                    text,
                    skew,
                    embolden,
                } => {
                    surface.set_fill(Some(fill(*color)));
                    // Synthesised bold: thicken the outlines a little.
                    surface.set_stroke(embolden.then(|| stroke(*color, size * 0.035)));
                    if *skew {
                        // Slant around the baseline.
                        surface.push_transform(&Transform::from_row(1.0, 0.0, -SKEW, 1.0, SKEW * y, 0.0));
                    }
                    surface.draw_glyphs(Point::from_xy(*x, *y), glyphs, faces.all[*face].font.clone(), text, *size, false);
                    if *skew {
                        surface.pop();
                    }
                }
                Op::Image { x, y, w, h, image } => {
                    let Some(size) = Size::from_wh(*w, *h) else { continue };
                    surface.push_transform(&Transform::from_translate(*x, *y));
                    surface.draw_image(image.clone(), size);
                    surface.pop();
                }
                Op::Link { .. } => {}
            }
        }
        surface.finish();
        for op in ops {
            if let Op::Link { x, y, w, h, url } = op {
                if let Some(rect) = Rect::from_xywh(*x, *y, *w, *h) {
                    let target = Target::Action(LinkAction::new(url.clone()).into());
                    page.add_annotation(LinkAnnotation::new(rect, target).into());
                }
            }
        }
        page.finish();
    }
    let mut meta = Metadata::new().creator("RustMail".to_string());
    if !msg.subject.trim().is_empty() {
        meta = meta.title(msg.subject.trim().to_string());
    }
    if !msg.from.trim().is_empty() {
        meta = meta.authors(vec![msg.from.trim().to_string()]);
    }
    doc.set_metadata(meta);
    doc.finish().map_err(|e| anyhow!("could not write the PDF: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::html;

    fn message(doc: &HtmlDoc) -> PdfMessage<'_> {
        PdfMessage {
            subject: "Q4 roadmap review — slides attached".into(),
            from: "Priya Shah <priya.shah@example.com>".into(),
            to: "Alex Morgan <alex@example.com>".into(),
            cc: String::new(),
            date: "Tue 11/10/2026 11:22 AM".into(),
            attachments: vec!["Q4-roadmap.pptx (2.1 MB)".into()],
            doc,
            images: HashMap::new(),
        }
    }

    fn pages(msg: &PdfMessage) -> Vec<Vec<Op>> {
        let faces = Faces::new(&ExportFonts::bundled()).unwrap();
        layout(msg, PageSize::A4, &faces)
    }

    fn all_text(pages: &[Vec<Op>]) -> String {
        pages
            .iter()
            .flatten()
            .filter_map(|op| match op {
                Op::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Set `RUSTMAIL_TEST_PDF_DIR` to keep the files for a look.
    fn keep(name: &str, bytes: &[u8]) {
        if let Ok(dir) = std::env::var("RUSTMAIL_TEST_PDF_DIR") {
            std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
        }
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 160, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn writes_a_pdf_with_header_and_body() {
        let doc = html::parse_html(
            "<h2>Roadmap</h2><p>Hi Alex, the <b>slides</b> for <i>Thursday</i> are <a href=\"https://example.com/deck\">here</a>.</p>\
             <ul><li>Onboarding flow</li><li>Sync engine</li></ul><blockquote>Thanks — see you then.</blockquote>\
             <p><code>cargo run</code> <s>old</s> 日本語のテキスト 😀</p>",
        );
        let msg = message(&doc);
        let pages = pages(&msg);
        assert_eq!(pages.len(), 1);
        let text = all_text(&pages);
        for part in ["Q4 roadmap review", "From:", "Priya Shah", "Attachments:", "Q4-roadmap.pptx", "Roadmap", "the slides for Thursday are here.", "•Onboarding flow", "日本語のテキスト 😀", "Page 1 of 1"] {
            assert!(text.contains(part), "missing {part:?} in {text:?}");
        }
        assert!(!text.contains("Cc:"), "an empty Cc row is left out");
        let links: Vec<&str> = pages[0]
            .iter()
            .filter_map(|op| match op {
                Op::Link { url, .. } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(links, ["https://example.com/deck"]);

        let bytes = render_with(&msg, PageSize::A4, &ExportFonts::bundled()).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        keep("message.pdf", &bytes);
    }

    #[test]
    fn long_messages_flow_onto_more_pages_inside_the_margins() {
        let para = "The quick brown fox jumps over the lazy dog and keeps on running through the field. ".repeat(6);
        let body = format!("{para}\n\n").repeat(40);
        let doc = html::from_plain_text(&body);
        let msg = message(&doc);
        let pages = pages(&msg);
        assert!(pages.len() > 2, "{} pages", pages.len());
        let (w, h) = PageSize::A4.points();
        for (n, ops) in pages.iter().enumerate() {
            let mut lines = 0;
            for op in ops {
                if let Op::Text { x, y, glyphs, size, text, .. } = op {
                    let width: f32 = glyphs.iter().map(|g| g.x_advance * size).sum();
                    assert!(*x >= MARGIN - 0.01 && x + width <= w - MARGIN + 0.5, "{text:?} leaves the margins on page {n}");
                    assert!(*y > MARGIN && *y < h - MARGIN / 3.0);
                    lines += 1;
                }
            }
            assert!(lines > 1, "page {n} is empty");
        }
        assert!(all_text(&pages).contains(&format!("Page {0} of {0}", pages.len())));
        let bytes = render_with(&msg, PageSize::Letter, &ExportFonts::bundled()).unwrap();
        keep("long.pdf", &bytes);
    }

    #[test]
    fn a_word_wider_than_the_page_is_broken() {
        let url = format!("https://example.com/{}", "a".repeat(400));
        let doc = html::from_plain_text(&url);
        let pages = pages(&message(&doc));
        let (w, _) = PageSize::A4.points();
        let mut rows = std::collections::BTreeSet::new();
        for op in &pages[0] {
            if let Op::Text { x, y, glyphs, size, text, .. } = op {
                let width: f32 = glyphs.iter().map(|g| g.x_advance * size).sum();
                assert!(x + width <= w - MARGIN + 0.5);
                if text.contains('a') && !text.contains(' ') {
                    rows.insert((*y * 10.0) as i32);
                }
            }
        }
        assert!(rows.len() >= 4, "the long word takes several lines, got {}", rows.len());
    }

    #[test]
    fn inline_images_are_embedded_and_remote_ones_named() {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(png(400, 200));
        let doc = html::parse_html(&format!(
            "<p>Before</p><img src=\"cid:photo1\" width=\"200\"><img src=\"data:image/png;base64,{data}\">\
             <img src=\"https://example.com/banner.png\" alt=\"Rust banner\"><p>After</p>"
        ));
        let mut msg = message(&doc);
        msg.images = collect_images(b"", &doc);
        assert_eq!(msg.images.len(), 1, "the data: image");
        msg.images.insert("photo1".into(), png(800, 400));
        let pages = pages(&msg);
        let sizes: Vec<(f32, f32)> = pages[0]
            .iter()
            .filter_map(|op| match op {
                Op::Image { w, h, .. } => Some((*w, *h)),
                _ => None,
            })
            .collect();
        // width="200" keeps the aspect ratio; the other one is its own size.
        assert_eq!(sizes, [(150.0, 75.0), (300.0, 150.0)]);
        assert!(all_text(&pages).contains("[Rust banner]"));
        let bytes = render_with(&msg, PageSize::A4, &ExportFonts::bundled()).unwrap();
        keep("images.pdf", &bytes);
    }

    /// Every message of the demo mailbox (plain text, HTML newsletter,
    /// inline image, invitation, attachments) goes through the whole path.
    #[test]
    fn every_demo_message_exports() {
        let store = crate::store::Store::memory().unwrap();
        let mut cfg = crate::config::AppConfig::default();
        crate::demo::create(&store, &mut cfg).unwrap();
        let messages = store
            .list_messages(&crate::store::ListQuery {
                limit: 5000,
                ..Default::default()
            })
            .unwrap();
        let mut exported = 0;
        for m in &messages {
            let Some(raw) = store.raw_body(m.id).unwrap() else { continue };
            let bytes = message_pdf_with(&raw, Some(&m.subject), Some(m.date), PageSize::A4, &ExportFonts::bundled())
                .unwrap_or_else(|e| panic!("{:?}: {e}", m.subject));
            assert!(bytes.starts_with(b"%PDF-"), "{:?}", m.subject);
            keep(&format!("demo-{}.pdf", m.id), &bytes);
            exported += 1;
        }
        assert!(exported >= 20, "only {exported} demo messages had a body");
    }

    /// The fonts found on this computer must be usable too.
    #[test]
    fn system_fonts_work() {
        let doc = html::parse_html("<p>Plain, <b>bold</b>, <i>italic</i>, <b><i>both</i></b>, <code>mono</code>, 日本語 😀</p>");
        let bytes = render_with(&message(&doc), PageSize::for_region(), &ExportFonts::system()).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        keep("system-fonts.pdf", &bytes);
    }

    #[test]
    fn colours_stay_readable_on_white() {
        let white_text = RunStyle {
            color: Some(Color32::WHITE),
            ..Default::default()
        };
        assert_eq!(colors(&white_text), (TEXT, None));
        let button = RunStyle {
            color: Some(Color32::WHITE),
            bg: Some(Color32::from_rgb(0xB7, 0x41, 0x0E)),
            ..Default::default()
        };
        assert_eq!(colors(&button), ([255, 255, 255], Some([0xB7, 0x41, 0x0E])));
        assert_eq!(external_link("#show-images"), None);
        assert_eq!(external_link("javascript:alert(1)"), None);
        assert!(external_link("mailto:a@example.com").is_some());
    }
}
