//! Saving a message in formats other programs open: PDF, Word (`.docx`),
//! a web page, plain text and Markdown.
//!
//! All of them are written from the same block model the reading pane
//! shows ([`HtmlDoc`]), with a header block (subject, people, date,
//! attachment names) on top. Nothing is fetched from the network: images the
//! message carries are included, remote ones are named by their description.

use std::fmt::Write as _;
use std::io::Write as _;

use anyhow::Result;
use base64::Engine;
use egui::Align;

use crate::pdf::{self, colors, external_link, PageSize, PdfMessage, Rgb};
use crate::ui::html::{heading_scale, Block, Run, RunStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Pdf,
    Word,
    Html,
    Text,
    Markdown,
}

impl Format {
    pub const ALL: [Format; 5] = [Format::Pdf, Format::Word, Format::Html, Format::Text, Format::Markdown];

    /// Name in menus and in the file dialog's type list.
    pub fn label(self) -> &'static str {
        match self {
            Format::Pdf => "PDF",
            Format::Word => "Word document",
            Format::Html => "Web page",
            Format::Text => "Plain text",
            Format::Markdown => "Markdown",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Word => "docx",
            Format::Html => "html",
            Format::Text => "txt",
            Format::Markdown => "md",
        }
    }
}

/// The message `raw` in `format`. `subject` and `date` are the ones the
/// message list shows; the message's own are used where they are missing.
pub fn export(format: Format, raw: &[u8], subject: Option<&str>, date: Option<i64>) -> Result<Vec<u8>> {
    let page = PageSize::for_region();
    match format {
        Format::Pdf => pdf::message_pdf(raw, subject, date, page),
        Format::Word => pdf::with_message(raw, subject, date, |m| docx(m, page)),
        Format::Html => pdf::with_message(raw, subject, date, |m| Ok(html(m).into_bytes())),
        Format::Text => pdf::with_message(raw, subject, date, |m| Ok(text(m).into_bytes())),
        Format::Markdown => pdf::with_message(raw, subject, date, |m| Ok(markdown(m).into_bytes())),
    }
}

// ------------------------------------------------------------------- shared

const TEXT: Rgb = [0x24, 0x24, 0x24];
/// Body text size in points (the reading pane's 14 px).
const BASE_PT: f32 = 10.5;

fn subject_of<'a>(m: &'a PdfMessage) -> &'a str {
    if m.subject.trim().is_empty() {
        "(no subject)"
    } else {
        m.subject.trim()
    }
}

/// The header lines that have something in them.
fn header_rows(m: &PdfMessage) -> Vec<(&'static str, String)> {
    [
        ("From", m.from.clone()),
        ("To", m.to.clone()),
        ("Cc", m.cc.clone()),
        ("Date", m.date.clone()),
        ("Attachments", m.attachments.join("; ")),
    ]
    .into_iter()
    .map(|(k, v)| (k, v.trim().to_string()))
    .filter(|(_, v)| !v.is_empty())
    .collect()
}

/// The bytes of an image block, if the message carries the image itself.
fn image_bytes<'a>(m: &'a PdfMessage, block: usize, src: &str) -> Option<&'a Vec<u8>> {
    if src.to_ascii_lowercase().starts_with("cid:") {
        m.images.get(&src[4..])
    } else if src.starts_with("data:") {
        m.images.get(&format!("data:{block}"))
    } else {
        None
    }
}

/// Text for XML and HTML: escaped, without the control characters XML forbids.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() || matches!(c, '\u{FFFE}' | '\u{FFFF}') => {}
            c => out.push(c),
        }
    }
    out
}

fn hex(c: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Size of a run in points.
fn size_pt(style: &RunStyle, heading: u8) -> f32 {
    let scale = if style.scale > 0.0 { style.scale } else { heading_scale(heading) };
    (BASE_PT * scale).clamp(6.75, 30.0)
}

// --------------------------------------------------------------------- text

/// The text of a paragraph, with the targets of links that say something
/// other than their own address.
fn plain_runs(runs: &[Run]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < runs.len() {
        // Adjacent runs of one link read as one.
        let link = runs[i].style.link.as_deref().and_then(external_link);
        let mut j = i;
        let mut label = String::new();
        while j < runs.len() && runs[j].style.link.as_deref().and_then(external_link) == link {
            label.push_str(&runs[j].text);
            j += 1;
            if link.is_none() {
                break;
            }
        }
        out.push_str(&label);
        if let Some(url) = link {
            let shown = label.trim().trim_end_matches('/');
            let bare = url.trim_start_matches("mailto:").trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
            if !shown.is_empty() && shown != url.trim_end_matches('/') && shown != bare {
                let _ = write!(out, " <{url}>");
            }
        }
        i = j;
    }
    out
}

/// Adds a paragraph's lines, each behind `first` / `rest`. Markdown keeps
/// the spaces at line ends (two of them are a line break).
fn push_lines(out: &mut String, text: &str, first: &str, rest: &str, keep_trailing_spaces: bool) {
    let lines: Vec<&str> = text.trim_end().split('\n').collect();
    for (n, line) in lines.iter().enumerate() {
        out.push_str(if n == 0 { first } else { rest });
        out.push_str(if keep_trailing_spaces && n + 1 < lines.len() { line } else { line.trim_end() });
        out.push('\n');
    }
}

fn blank_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

pub fn text(m: &PdfMessage) -> String {
    let mut out = format!("Subject: {}\n", subject_of(m));
    for (label, value) in header_rows(m) {
        let _ = writeln!(out, "{label}: {value}");
    }
    out.push('\n');
    for (i, block) in m.doc.blocks.iter().enumerate() {
        match block {
            Block::Gap => blank_line(&mut out),
            Block::Rule { quote } => {
                let _ = writeln!(out, "{}{}", "> ".repeat(*quote as usize), "-".repeat(40));
            }
            Block::Text {
                runs,
                indent,
                quote,
                marker,
                ..
            } => {
                let body = plain_runs(runs);
                if body.trim().is_empty() {
                    continue;
                }
                // A list's own indentation is its marker.
                let depth = (*indent as usize).saturating_sub(marker.is_some() as usize);
                let lead = format!("{}{}", "> ".repeat(*quote as usize), "  ".repeat(depth));
                let bullet = marker.as_ref().map(|m| format!("{m} ")).unwrap_or_default();
                let first = format!("{lead}{bullet}");
                let rest = format!("{lead}{}", " ".repeat(bullet.chars().count()));
                push_lines(&mut out, &body, &first, &rest, false);
            }
            Block::Image { src, alt, quote, .. } => {
                let known = image_bytes(m, i, src).is_some();
                let label = match (alt.trim(), known) {
                    ("", true) => "[image]".to_string(),
                    ("", false) => continue,
                    (alt, _) => format!("[image: {alt}]"),
                };
                let _ = writeln!(out, "{}{label}", "> ".repeat(*quote as usize));
            }
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('\n');
    out
}

// ----------------------------------------------------------------- markdown

/// Characters Markdown would read as formatting.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One run as Markdown. Emphasis marks must hug the text, so the white
/// space around it stays outside them.
fn md_run(run: &Run) -> String {
    let style = &run.style;
    let mut out = String::new();
    // Line breaks inside a paragraph are hard breaks.
    for (n, line) in run.text.split('\n').enumerate() {
        if n > 0 {
            out.push_str("  \n");
        }
        let core = line.trim();
        if core.is_empty() {
            out.push_str(line);
            continue;
        }
        let (before, after) = (&line[..line.len() - line.trim_start().len()], &line[line.trim_end().len()..]);
        let mut piece = if style.mono {
            format!("`{}`", core.replace('`', "'"))
        } else {
            md_escape(core)
        };
        if style.strike {
            piece = format!("~~{piece}~~");
        }
        if style.italic {
            piece = format!("*{piece}*");
        }
        if style.bold {
            piece = format!("**{piece}**");
        }
        if let Some(url) = style.link.as_deref().and_then(external_link) {
            piece = format!("[{piece}]({})", url.replace(' ', "%20").replace(')', "%29"));
        }
        out.push_str(before);
        out.push_str(&piece);
        out.push_str(after);
    }
    out
}

pub fn markdown(m: &PdfMessage) -> String {
    let mut out = format!("# {}\n\n", md_escape(subject_of(m)));
    for (label, value) in header_rows(m) {
        let _ = writeln!(out, "**{label}:** {}  ", md_escape(&value));
    }
    out.push_str("\n---\n\n");
    for (i, block) in m.doc.blocks.iter().enumerate() {
        match block {
            Block::Gap => blank_line(&mut out),
            Block::Rule { quote } => {
                blank_line(&mut out);
                let _ = writeln!(out, "{}---\n", "> ".repeat(*quote as usize));
            }
            Block::Text {
                runs,
                indent,
                quote,
                heading,
                marker,
                ..
            } => {
                let quote_marks = "> ".repeat(*quote as usize);
                if *heading > 0 {
                    let title: String = runs.iter().map(|r| md_escape(r.text.trim_matches('\n'))).collect::<String>().replace('\n', " ");
                    if title.trim().is_empty() {
                        continue;
                    }
                    blank_line(&mut out);
                    // The subject is the document's first-level heading.
                    let _ = writeln!(out, "{quote_marks}{} {}\n", "#".repeat((*heading as usize + 1).min(6)), title.trim());
                    continue;
                }
                let body: String = runs.iter().map(md_run).collect();
                if body.trim().is_empty() {
                    continue;
                }
                match marker {
                    Some(mark) => {
                        // "1." stays a numbered item; any bullet becomes "-".
                        let bullet = if mark.trim_end_matches(['.', ')']).chars().all(|c| c.is_ascii_digit()) && mark.len() > 1 {
                            format!("{}. ", mark.trim_end_matches(['.', ')']))
                        } else {
                            "- ".to_string()
                        };
                        let nest = "    ".repeat((*indent as usize).saturating_sub(1));
                        let first = format!("{quote_marks}{nest}{bullet}");
                        let rest = format!("{quote_marks}{nest}{}", " ".repeat(bullet.len()));
                        push_lines(&mut out, &body, &first, &rest, true);
                    }
                    None => {
                        // Separate blocks are separate lines on screen.
                        if !out.ends_with("\n\n") && !out.ends_with("  \n") && out.ends_with('\n') {
                            let len = out.len() - 1;
                            out.truncate(len);
                            out.push_str("  \n");
                        }
                        push_lines(&mut out, &body, &quote_marks, &quote_marks, true);
                    }
                }
            }
            Block::Image { src, alt, quote, .. } => {
                let known = image_bytes(m, i, src).is_some();
                let label = match (alt.trim(), known) {
                    ("", true) => "*[image]*".to_string(),
                    ("", false) => continue,
                    (alt, _) => format!("*[image: {}]*", md_escape(alt)),
                };
                let _ = writeln!(out, "{}{label}", "> ".repeat(*quote as usize));
            }
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('\n');
    out
}

// --------------------------------------------------------------------- html

const HTML_STYLE: &str = "body{font:14px/1.45 'Segoe UI','Open Sans',Helvetica,Arial,sans-serif;color:#242424;background:#fff;max-width:820px;margin:28px auto;padding:0 18px}\
h1.subject{font-size:22px;font-weight:600;margin:0 0 10px}\
table.header{border-collapse:collapse;font-size:13px;margin:0 0 12px}\
table.header th{text-align:left;font-weight:normal;color:#616161;padding:1px 14px 1px 0;vertical-align:top;white-space:nowrap}\
table.header td{padding:1px 0}\
hr{border:0;border-top:1px solid #d1d1d1;margin:10px 0}\
p,h2,h3,h4,h5{margin:0 0 4px}\
img{max-width:100%;height:auto;border-radius:2px}\
code{font-family:Consolas,'Cascadia Mono',Menlo,monospace}\
a{color:#0f6cbd}\
.gap{height:8px}\
.missing{color:#616161;font-style:italic}";

/// One run as HTML.
fn html_run(run: &Run, heading: u8) -> String {
    let style = &run.style;
    let (fg, bg) = colors(style);
    let link = style.link.as_deref().and_then(external_link);
    let mut css = String::new();
    // A link keeps the page's link colour unless the message set its own.
    if fg != TEXT && !(link.is_some() && style.color.is_none()) {
        let _ = write!(css, "color:#{};", hex(fg));
    }
    if let Some(bg) = bg {
        let _ = write!(css, "background:#{};", hex(bg));
    }
    if style.scale > 0.0 {
        let _ = write!(css, "font-size:{:.1}pt;", size_pt(style, heading));
    }
    if style.underline && link.is_none() {
        css.push_str("text-decoration:underline;");
    }
    let mut out = esc(&run.text).replace('\n', "<br>\n");
    if style.mono {
        out = format!("<code>{out}</code>");
    }
    if style.strike {
        out = format!("<s>{out}</s>");
    }
    if style.italic {
        out = format!("<i>{out}</i>");
    }
    if style.bold && heading == 0 {
        out = format!("<b>{out}</b>");
    }
    if !css.is_empty() {
        out = format!("<span style=\"{css}\">{out}</span>");
    }
    if let Some(url) = link {
        out = format!("<a href=\"{}\">{out}</a>", esc(&url));
    }
    out
}

/// Left edge of a block: quote bars and indentation, as CSS.
fn html_edge(quote: u8, indent: u8, marker: bool) -> String {
    let mut css = String::new();
    let indent_px = indent as u32 * 22 + if marker { 18 } else { 0 };
    if quote > 0 {
        let _ = write!(css, "border-left:3px solid #93bee2;padding-left:{}px;margin-left:{}px;", 9 + indent_px, (quote as u32 - 1) * 14);
    } else if indent_px > 0 {
        let _ = write!(css, "padding-left:{indent_px}px;");
    }
    css
}

fn image_mime(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Jpeg) => "image/jpeg",
        Ok(image::ImageFormat::Gif) => "image/gif",
        Ok(image::ImageFormat::WebP) => "image/webp",
        Ok(image::ImageFormat::Bmp) => "image/bmp",
        _ => "image/png",
    }
}

/// A page that stands on its own: styles and images are inside the file.
pub fn html(m: &PdfMessage) -> String {
    let mut out = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{HTML_STYLE}</style>\n</head>\n<body>\n<h1 class=\"subject\">{}</h1>\n<table class=\"header\">\n",
        esc(subject_of(m)),
        esc(subject_of(m))
    );
    for (label, value) in header_rows(m) {
        let _ = writeln!(out, "<tr><th>{label}:</th><td>{}</td></tr>", esc(&value));
    }
    out.push_str("</table>\n<hr>\n");
    for (i, block) in m.doc.blocks.iter().enumerate() {
        match block {
            Block::Gap => out.push_str("<div class=\"gap\"></div>\n"),
            Block::Rule { quote } => {
                let _ = writeln!(out, "<hr style=\"margin-left:{}px\">", *quote as u32 * 14);
            }
            Block::Text {
                runs,
                align,
                indent,
                quote,
                heading,
                marker,
            } => {
                let body: String = runs.iter().map(|r| html_run(r, *heading)).collect();
                if body.trim().is_empty() {
                    continue;
                }
                let mut css = html_edge(*quote, *indent, marker.is_some());
                match align {
                    Align::Center => css.push_str("text-align:center;"),
                    Align::Max => css.push_str("text-align:right;"),
                    Align::Min => {}
                }
                if marker.is_some() {
                    css.push_str("text-indent:-18px;");
                }
                // The subject is the page's <h1>.
                let tag = if *heading > 0 { format!("h{}", (*heading + 1).min(5)) } else { "p".to_string() };
                let style = if css.is_empty() { String::new() } else { format!(" style=\"{css}\"") };
                let bullet = marker
                    .as_ref()
                    .map(|m| format!("<span style=\"display:inline-block;width:18px;text-indent:0;color:#616161\">{}</span>", esc(m)))
                    .unwrap_or_default();
                let _ = writeln!(out, "<{tag}{style}>{bullet}{body}</{tag}>");
            }
            Block::Image {
                src,
                alt,
                width,
                link,
                align,
                indent,
                quote,
                ..
            } => {
                let mut css = html_edge(*quote, *indent, false);
                match align {
                    Align::Center => css.push_str("text-align:center;"),
                    Align::Max => css.push_str("text-align:right;"),
                    Align::Min => {}
                }
                let style = if css.is_empty() { String::new() } else { format!(" style=\"{css}\"") };
                let link = link.as_deref().and_then(external_link);
                let mut inner = match image_bytes(m, i, src) {
                    Some(bytes) => {
                        let width = width.filter(|w| *w > 0.0).map(|w| format!(" width=\"{}\"", w.round())).unwrap_or_default();
                        format!(
                            "<img src=\"data:{};base64,{}\" alt=\"{}\"{width}>",
                            image_mime(bytes),
                            base64::engine::general_purpose::STANDARD.encode(bytes),
                            esc(alt.trim())
                        )
                    }
                    None if alt.trim().is_empty() => continue,
                    None => format!("<span class=\"missing\">[{}]</span>", esc(alt.trim())),
                };
                if let Some(url) = link {
                    inner = format!("<a href=\"{}\">{inner}</a>", esc(&url));
                }
                let _ = writeln!(out, "<p{style}>{inner}</p>");
            }
        }
    }
    out.push_str("</body>\n</html>\n");
    out
}

// ---------------------------------------------------------------------- zip

/// A minimal ZIP archive writer (deflate), enough for a `.docx`.
struct Zip {
    out: Vec<u8>,
    central: Vec<u8>,
    entries: u16,
}

impl Zip {
    fn new() -> Zip {
        Zip {
            out: Vec::new(),
            central: Vec::new(),
            entries: 0,
        }
    }

    fn add(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data)?;
        let deflated = encoder.finish()?;
        // Already compressed data (images) is stored as it is.
        let (method, body): (u16, &[u8]) = if deflated.len() < data.len() { (8, &deflated) } else { (0, data) };
        let crc = crc32fast::hash(data);
        let offset = self.out.len() as u32;
        // Shared by the local and the central header: version needed,
        // flags (UTF-8 names), method, time, date (1980-01-01), CRC, sizes,
        // name length, extra length.
        let mut common = Vec::new();
        for v in [20u16, 0x0800, method, 0, 0x0021] {
            common.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, body.len() as u32, data.len() as u32] {
            common.extend_from_slice(&v.to_le_bytes());
        }
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());

        self.out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        self.out.extend_from_slice(&common);
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(body);

        self.central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        self.central.extend_from_slice(&common);
        // Comment length, disk number, internal and external attributes.
        self.central.extend_from_slice(&[0u8; 10]);
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.entries += 1;
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        let start = self.out.len() as u32;
        let size = self.central.len() as u32;
        self.out.extend_from_slice(&self.central);
        self.out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        for v in [0u16, 0, self.entries, self.entries] {
            self.out.extend_from_slice(&v.to_le_bytes());
        }
        self.out.extend_from_slice(&size.to_le_bytes());
        self.out.extend_from_slice(&start.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out
    }
}

// --------------------------------------------------------------------- docx

const DOCX_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#;

const DOCX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;

const DOCX_STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:color w:val="242424"/><w:sz w:val="21"/><w:szCs w:val="21"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="60" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:after="120"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="160" w:after="80"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="140" w:after="80"/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="120" w:after="60"/><w:outlineLvl w:val="2"/></w:pPr><w:rPr><w:b/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading4"><w:name w:val="heading 4"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="100" w:after="60"/><w:outlineLvl w:val="3"/></w:pPr><w:rPr><w:b/></w:rPr></w:style><w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:rPr><w:color w:val="0F6CBD"/><w:u w:val="single"/></w:rPr></w:style></w:styles>"#;

const DOCX_NAMESPACES: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#;

/// Twentieths of a point per CSS pixel (0.75 pt).
const TWIPS_PER_PX: u32 = 15;
/// English Metric Units per CSS pixel.
const EMU_PER_PX: f32 = 9525.0;
const EMU_PER_TWIP: u32 = 635;
const MARGIN_TWIPS: u32 = 1080;

/// Paragraph properties, written in the order the format prescribes.
#[derive(Default)]
struct ParaProps<'a> {
    style: Option<&'a str>,
    quote: bool,
    rule_below: bool,
    tab: Option<u32>,
    before: Option<u32>,
    after: Option<u32>,
    left: u32,
    hanging: u32,
    align: Option<&'a str>,
}

impl ParaProps<'_> {
    fn xml(&self) -> String {
        let mut p = String::new();
        if let Some(s) = self.style {
            let _ = write!(p, "<w:pStyle w:val=\"{s}\"/>");
        }
        if self.quote || self.rule_below {
            p.push_str("<w:pBdr>");
            if self.quote {
                p.push_str("<w:left w:val=\"single\" w:sz=\"18\" w:space=\"6\" w:color=\"93BEE2\"/>");
            }
            if self.rule_below {
                p.push_str("<w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"D1D1D1\"/>");
            }
            p.push_str("</w:pBdr>");
        }
        if let Some(pos) = self.tab {
            let _ = write!(p, "<w:tabs><w:tab w:val=\"left\" w:pos=\"{pos}\"/></w:tabs>");
        }
        if self.before.is_some() || self.after.is_some() {
            p.push_str("<w:spacing");
            if let Some(v) = self.before {
                let _ = write!(p, " w:before=\"{v}\"");
            }
            if let Some(v) = self.after {
                let _ = write!(p, " w:after=\"{v}\"");
            }
            p.push_str("/>");
        }
        if self.left > 0 || self.hanging > 0 {
            let _ = write!(p, "<w:ind w:left=\"{}\"", self.left);
            if self.hanging > 0 {
                let _ = write!(p, " w:hanging=\"{}\"", self.hanging);
            }
            p.push_str("/>");
        }
        if let Some(a) = self.align {
            let _ = write!(p, "<w:jc w:val=\"{a}\"/>");
        }
        if p.is_empty() {
            p
        } else {
            format!("<w:pPr>{p}</w:pPr>")
        }
    }
}

/// Run properties for the given look.
fn run_props(style: &RunStyle, heading: u8) -> String {
    let (fg, bg) = colors(style);
    let mut p = String::new();
    if style.mono {
        p.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/>");
    }
    if style.bold {
        p.push_str("<w:b/>");
    }
    if style.italic {
        p.push_str("<w:i/>");
    }
    if style.strike {
        p.push_str("<w:strike/>");
    }
    if fg != TEXT {
        let _ = write!(p, "<w:color w:val=\"{}\"/>", hex(fg));
    }
    let half_points = (size_pt(style, heading) * 2.0).round() as u32;
    if half_points != 21 {
        let _ = write!(p, "<w:sz w:val=\"{half_points}\"/><w:szCs w:val=\"{half_points}\"/>");
    }
    if style.underline || style.link.as_deref().and_then(external_link).is_some() {
        p.push_str("<w:u w:val=\"single\"/>");
    }
    if let Some(bg) = bg {
        let _ = write!(p, "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>", hex(bg));
    }
    if p.is_empty() {
        p
    } else {
        format!("<w:rPr>{p}</w:rPr>")
    }
}

/// Text as runs: line breaks and tabs are elements of their own.
fn text_runs(text: &str, props: &str) -> String {
    let mut out = String::new();
    let mut piece = String::new();
    let flush = |piece: &mut String, out: &mut String| {
        if !piece.is_empty() {
            let _ = write!(out, "<w:r>{props}<w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(piece));
            piece.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\n' => {
                flush(&mut piece, &mut out);
                let _ = write!(out, "<w:r>{props}<w:br/></w:r>");
            }
            '\t' => {
                flush(&mut piece, &mut out);
                let _ = write!(out, "<w:r>{props}<w:tab/></w:r>");
            }
            '\r' => {}
            c => piece.push(c),
        }
    }
    flush(&mut piece, &mut out);
    out
}

/// The parts of a `.docx` that grow while the body is written.
struct Docx {
    body: String,
    /// (id, type, target, external)
    relations: Vec<(String, &'static str, String, bool)>,
    media: Vec<(String, Vec<u8>)>,
    content_width: u32,
}

const REL_STYLES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const REL_LINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

impl Docx {
    fn relation(&mut self, kind: &'static str, target: String, external: bool) -> String {
        // The same link or picture is referred to once.
        if let Some((id, ..)) = self.relations.iter().find(|(_, k, t, _)| *k == kind && *t == target) {
            return id.clone();
        }
        let id = format!("rId{}", self.relations.len() + 1);
        self.relations.push((id.clone(), kind, target, external));
        id
    }

    fn paragraph(&mut self, props: &ParaProps, content: &str) {
        let _ = write!(self.body, "<w:p>{}{content}</w:p>", props.xml());
    }

    /// Runs of a paragraph, with links around the runs they cover.
    fn runs(&mut self, runs: &[Run], heading: u8) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < runs.len() {
            let link = runs[i].style.link.as_deref().and_then(external_link);
            let mut inner = String::new();
            let mut j = i;
            while j < runs.len() && runs[j].style.link.as_deref().and_then(external_link) == link {
                inner.push_str(&text_runs(&runs[j].text, &run_props(&runs[j].style, heading)));
                j += 1;
                if link.is_none() {
                    break;
                }
            }
            match link {
                Some(url) => {
                    let id = self.relation(REL_LINK, url, true);
                    let _ = write!(out, "<w:hyperlink r:id=\"{id}\" w:history=\"1\">{inner}</w:hyperlink>");
                }
                None => out.push_str(&inner),
            }
            i = j;
        }
        out
    }

    /// A picture as an inline drawing, or `None` if it can't be read.
    fn picture(&mut self, bytes: &[u8], alt: &str, width: Option<f32>, height: Option<f32>) -> Option<String> {
        // Word reads PNG, JPEG and GIF; anything else is converted.
        let (ext, data) = match image::guess_format(bytes).ok()? {
            image::ImageFormat::Png => ("png", bytes.to_vec()),
            image::ImageFormat::Jpeg => ("jpeg", bytes.to_vec()),
            image::ImageFormat::Gif => ("gif", bytes.to_vec()),
            _ => {
                let mut png = std::io::Cursor::new(Vec::new());
                image::load_from_memory(bytes).ok()?.write_to(&mut png, image::ImageFormat::Png).ok()?;
                ("png", png.into_inner())
            }
        };
        let (nw, nh) = image::ImageReader::new(std::io::Cursor::new(&data)).with_guessed_format().ok()?.into_dimensions().ok()?;
        let (nw, nh) = (nw.max(1) as f32, nh.max(1) as f32);
        let (w, h) = match (width.filter(|w| *w > 0.0), height.filter(|h| *h > 0.0)) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, nh * w / nw),
            (None, Some(h)) => (nw * h / nh, h),
            (None, None) => (nw, nh),
        };
        // Within the text column and the height of a page.
        let max_w = (self.content_width * EMU_PER_TWIP) as f32;
        let max_h = 9.0 * 914_400.0;
        let fit = (max_w / (w * EMU_PER_PX)).min(max_h / (h * EMU_PER_PX)).min(1.0);
        let (cx, cy) = ((w * EMU_PER_PX * fit).round().max(1.0) as u64, (h * EMU_PER_PX * fit).round().max(1.0) as u64);

        let n = self.media.len() + 1;
        let name = format!("image{n}.{ext}");
        let id = self.relation(REL_IMAGE, format!("media/{name}"), false);
        self.media.push((name, data));
        let alt = esc(alt.trim());
        Some(format!(
            "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{n}\" name=\"Picture {n}\" descr=\"{alt}\"/>\
<wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
<pic:pic><pic:nvPicPr><pic:cNvPr id=\"{n}\" name=\"Picture {n}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
        ))
    }
}

pub fn docx(m: &PdfMessage, page: PageSize) -> Result<Vec<u8>> {
    // Page size in twentieths of a point.
    let (page_w, page_h) = match page {
        PageSize::A4 => (11906u32, 16838u32),
        PageSize::Letter => (12240, 15840),
    };
    let mut d = Docx {
        body: String::new(),
        relations: vec![("rId1".into(), REL_STYLES, "styles.xml".into(), false)],
        media: Vec::new(),
        content_width: page_w - 2 * MARGIN_TWIPS,
    };

    // Header block.
    d.paragraph(
        &ParaProps {
            style: Some("Title"),
            ..Default::default()
        },
        &text_runs(subject_of(m), ""),
    );
    let small = "<w:rPr><w:sz w:val=\"19\"/><w:szCs w:val=\"19\"/></w:rPr>";
    let small_muted = "<w:rPr><w:color w:val=\"616161\"/><w:sz w:val=\"19\"/><w:szCs w:val=\"19\"/></w:rPr>";
    for (label, value) in header_rows(m) {
        let content = format!("{}{}", text_runs(&format!("{label}:\t"), small_muted), text_runs(&value, small));
        d.paragraph(
            &ParaProps {
                tab: Some(1300),
                after: Some(20),
                left: 1300,
                hanging: 1300,
                ..Default::default()
            },
            &content,
        );
    }
    d.paragraph(
        &ParaProps {
            rule_below: true,
            after: Some(200),
            ..Default::default()
        },
        "",
    );

    // Body. A gap is space above the block after it.
    let mut gap = false;
    for (i, block) in m.doc.blocks.iter().enumerate() {
        let before = if std::mem::take(&mut gap) { Some(160) } else { None };
        match block {
            Block::Gap => gap = true,
            Block::Rule { quote } => d.paragraph(
                &ParaProps {
                    rule_below: true,
                    quote: *quote > 0,
                    before,
                    after: Some(120),
                    left: *quote as u32 * 14 * TWIPS_PER_PX,
                    ..Default::default()
                },
                "",
            ),
            Block::Text {
                runs,
                align,
                indent,
                quote,
                heading,
                marker,
            } => {
                let mut content = d.runs(runs, *heading);
                if content.is_empty() {
                    gap = before.is_some();
                    continue;
                }
                let mut left = (*quote as u32 * 14 + *indent as u32 * 22) * TWIPS_PER_PX;
                let mut props = ParaProps {
                    quote: *quote > 0,
                    before,
                    ..Default::default()
                };
                let style;
                if (1..=4).contains(heading) {
                    style = format!("Heading{heading}");
                    props.style = Some(&style);
                }
                if let Some(mark) = marker {
                    // The marker hangs to the left of the item's text.
                    left += 360;
                    props.hanging = 360;
                    props.tab = Some(left);
                    content = format!("{}{content}", text_runs(&format!("{mark}\t"), "<w:rPr><w:color w:val=\"616161\"/></w:rPr>"));
                }
                props.left = left;
                props.align = match align {
                    Align::Center => Some("center"),
                    Align::Max => Some("right"),
                    Align::Min => None,
                };
                d.paragraph(&props, &content);
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
                let link = link.as_deref().and_then(external_link);
                let content = match image_bytes(m, i, src).and_then(|b| d.picture(b, alt, *width, *height)) {
                    Some(drawing) => drawing,
                    None if alt.trim().is_empty() => {
                        gap = before.is_some();
                        continue;
                    }
                    // Not part of the message: say what was there.
                    None => text_runs(&format!("[{}]", alt.trim()), "<w:rPr><w:i/><w:color w:val=\"616161\"/></w:rPr>"),
                };
                let content = match link {
                    Some(url) => {
                        let id = d.relation(REL_LINK, url, true);
                        format!("<w:hyperlink r:id=\"{id}\" w:history=\"1\">{content}</w:hyperlink>")
                    }
                    None => content,
                };
                d.paragraph(
                    &ParaProps {
                        quote: *quote > 0,
                        before,
                        left: (*quote as u32 * 14 + *indent as u32 * 22) * TWIPS_PER_PX,
                        align: match align {
                            Align::Center => Some("center"),
                            Align::Max => Some("right"),
                            Align::Min => None,
                        },
                        ..Default::default()
                    },
                    &content,
                );
            }
        }
    }

    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document {DOCX_NAMESPACES}><w:body>{}<w:sectPr><w:pgSz w:w=\"{page_w}\" w:h=\"{page_h}\"/>\
<w:pgMar w:top=\"{MARGIN_TWIPS}\" w:right=\"{MARGIN_TWIPS}\" w:bottom=\"{MARGIN_TWIPS}\" w:left=\"{MARGIN_TWIPS}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        d.body
    );
    let mut rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
    for (id, kind, target, external) in &d.relations {
        let _ = write!(
            rels,
            "<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{}\"{}/>",
            esc(target),
            if *external { " TargetMode=\"External\"" } else { "" }
        );
    }
    rels.push_str("</Relationships>");
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
<dc:title>{}</dc:title><dc:creator>{}</dc:creator></cp:coreProperties>",
        esc(subject_of(m)),
        esc(m.from.trim())
    );

    let mut zip = Zip::new();
    zip.add("[Content_Types].xml", DOCX_TYPES.as_bytes())?;
    zip.add("_rels/.rels", DOCX_RELS.as_bytes())?;
    zip.add("docProps/core.xml", core.as_bytes())?;
    zip.add("word/document.xml", document.as_bytes())?;
    zip.add("word/styles.xml", DOCX_STYLES.as_bytes())?;
    zip.add("word/_rels/document.xml.rels", rels.as_bytes())?;
    for (name, data) in &d.media {
        zip.add(&format!("word/media/{name}"), data)?;
    }
    Ok(zip.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::html as page;
    use std::collections::HashMap;
    use std::io::Read as _;

    const SAMPLE: &str = "<h2>Roadmap</h2><p>Hi Alex, the <b>slides</b> for <i>Thursday</i> are <a href=\"https://example.com/deck?a=1&b=2\">here</a>, see \
        <a href=\"https://example.com/\">https://example.com</a>.<br>Second line &lt;with&gt; marks*.</p>\
        <ul><li>Onboarding flow</li><li>Sync <code>engine</code></li></ul><ol><li>First</li><li>Second</li></ol>\
        <blockquote>Thanks — see you <s>then</s>.</blockquote><hr>\
        <p><img src=\"cid:photo1\" width=\"200\" alt=\"Sunset\"><img src=\"https://example.com/banner.png\" alt=\"Rust banner\"></p>\
        <p style=\"text-align:center\"><a href=\"#top\">Top</a> 日本語 😀</p>";

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 160, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn message(doc: &page::HtmlDoc) -> PdfMessage<'_> {
        PdfMessage {
            subject: "Q4 roadmap <review> & plans".into(),
            from: "Priya Shah <priya.shah@example.com>".into(),
            to: "Alex Morgan <alex@example.com>".into(),
            cc: String::new(),
            date: "Tue 11/10/2026 11:22 AM".into(),
            attachments: vec!["Q4-roadmap.pptx (2.1 MB)".into()],
            doc,
            images: HashMap::from([("photo1".to_string(), png(400, 200))]),
        }
    }

    /// Set `RUSTMAIL_TEST_EXPORT_DIR` to keep the files for a look.
    fn keep(name: &str, bytes: &[u8]) {
        if let Ok(dir) = std::env::var("RUSTMAIL_TEST_EXPORT_DIR") {
            std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
        }
    }

    /// Reads an archive written by [`Zip`]: name -> contents, checking sizes and checksums.
    fn unzip(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]) as usize;
        let u32at = |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap()) as usize;
        let end = zip.len() - 22;
        assert_eq!(u32at(end), 0x0605_4B50, "end record");
        let (count, mut at) = (u16at(end + 10), u32at(end + 16));
        assert_eq!(u32at(end + 12), end - at, "central directory size");
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32at(at), 0x0201_4B50, "central header");
            let (method, crc, csize, usize_, name_len, local) = (u16at(at + 10), u32at(at + 16), u32at(at + 20), u32at(at + 24), u16at(at + 28), u32at(at + 42));
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32at(local), 0x0403_4B50, "local header of {name}");
            assert_eq!(&zip[local + 30..local + 30 + name_len], name.as_bytes());
            let body = &zip[local + 30 + name_len..local + 30 + name_len + csize];
            let data = match method {
                0 => body.to_vec(),
                8 => {
                    let mut data = Vec::new();
                    flate2::read::DeflateDecoder::new(body).read_to_end(&mut data).unwrap();
                    data
                }
                other => panic!("method {other}"),
            };
            assert_eq!(data.len(), usize_, "size of {name}");
            assert_eq!(crc32fast::hash(&data) as usize, crc, "checksum of {name}");
            out.push((name, data));
            at += 46 + name_len;
        }
        out
    }

    #[test]
    fn word_documents_are_well_formed_packages() {
        let doc = page::parse_html(SAMPLE);
        let bytes = docx(&message(&doc), PageSize::Letter).unwrap();
        keep("message.docx", &bytes);
        let files = unzip(&bytes);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["[Content_Types].xml", "_rels/.rels", "docProps/core.xml", "word/document.xml", "word/styles.xml", "word/_rels/document.xml.rels", "word/media/image1.png"]
        );
        let part = |name: &str| String::from_utf8(files.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap();
        // Every XML part parses.
        for (name, data) in files.iter().filter(|(n, _)| n.ends_with(".xml") || n.ends_with(".rels")) {
            let text = std::str::from_utf8(data).unwrap();
            roxmltree::Document::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let document = part("word/document.xml");
        let xml = roxmltree::Document::parse(&document).unwrap();
        let text: Vec<String> = xml
            .descendants()
            .filter(|n| n.has_tag_name(("http://schemas.openxmlformats.org/wordprocessingml/2006/main", "p")))
            .map(|p| p.descendants().filter(|n| n.tag_name().name() == "t").filter_map(|t| t.text()).collect::<String>())
            .collect();
        assert_eq!(text[0], "Q4 roadmap <review> & plans");
        assert!(text.contains(&"From:Priya Shah <priya.shah@example.com>".to_string()), "{text:?}");
        assert!(text.contains(&"Attachments:Q4-roadmap.pptx (2.1 MB)".to_string()));
        assert!(!text.iter().any(|t| t.starts_with("Cc:")));
        for expected in ["Roadmap", "•Onboarding flow", "•Sync engine", "1.First", "2.Second", "Thanks — see you then.", "[Rust banner]", "Top 日本語 😀"] {
            assert!(text.iter().any(|t| t == expected), "missing {expected:?} in {text:?}");
        }
        assert!(text.iter().any(|t| t.starts_with("Hi Alex, the slides for Thursday are here, see https://example.com.")));
        assert!(document.contains("<w:pStyle w:val=\"Heading2\"/>") && document.contains("<w:strike/>") && document.contains("w:ascii=\"Consolas\""));
        assert!(document.contains("<w:jc w:val=\"center\"/>") && document.contains("<w:br/>"));
        assert!(document.contains("<wp:extent cx=\"1905000\" cy=\"952500\"/>"), "200 px wide, aspect kept");
        assert!(document.contains("w:w=\"12240\" w:h=\"15840\""));

        let rels = part("word/_rels/document.xml.rels");
        assert!(rels.contains("Target=\"https://example.com/deck?a=1&amp;b=2\" TargetMode=\"External\""));
        assert!(rels.contains("Target=\"media/image1.png\"") && !rels.contains("#top"), "in-page links are dropped");
        // Each relation the document uses exists.
        for id in document.split("r:id=\"").chain(document.split("r:embed=\"")).skip(1).filter_map(|s| s.split('"').next()) {
            if id.starts_with("rId") {
                assert!(rels.contains(&format!("Id=\"{id}\"")), "{id}");
            }
        }
    }

    #[test]
    fn web_pages_stand_alone() {
        let doc = page::parse_html(SAMPLE);
        let out = html(&message(&doc));
        keep("message.html", out.as_bytes());
        assert!(out.starts_with("<!DOCTYPE html>") && out.contains("<meta charset=\"utf-8\">"));
        assert!(out.contains("<title>Q4 roadmap &lt;review&gt; &amp; plans</title>"));
        assert!(out.contains("<tr><th>From:</th><td>Priya Shah &lt;priya.shah@example.com&gt;</td></tr>") && !out.contains("<th>Cc:</th>"));
        assert!(out.contains("<h3>Roadmap</h3>"), "headings sit below the subject");
        assert!(out.contains("<b>slides</b>") && out.contains("<i>Thursday</i>") && out.contains("<code>engine</code>") && out.contains("<s>then</s>"));
        assert!(out.contains("<a href=\"https://example.com/deck?a=1&amp;b=2\">here</a>"));
        assert!(out.contains("Second line &lt;with&gt; marks*.") && out.contains("<br>"));
        assert!(out.contains("<img src=\"data:image/png;base64,iVBOR") && out.contains("alt=\"Sunset\" width=\"200\""));
        assert!(out.contains("<span class=\"missing\">[Rust banner]</span>"));
        assert!(out.contains("border-left:3px solid #93bee2") && out.contains("text-align:center"));
        // Nothing points outside the file except links to click.
        assert!(!out.contains("src=\"http") && !out.contains("<script") && !out.contains("href=\"#top\""));
    }

    #[test]
    fn plain_text_keeps_structure() {
        let doc = page::parse_html(SAMPLE);
        let out = text(&message(&doc));
        keep("message.txt", out.as_bytes());
        let expected = "Subject: Q4 roadmap <review> & plans\n\
From: Priya Shah <priya.shah@example.com>\n\
To: Alex Morgan <alex@example.com>\n\
Date: Tue 11/10/2026 11:22 AM\n\
Attachments: Q4-roadmap.pptx (2.1 MB)\n\
\n\
Roadmap\n\
\n\
Hi Alex, the slides for Thursday are here <https://example.com/deck?a=1&b=2>, see https://example.com.\n\
Second line <with> marks*.\n\
\n\
  • Onboarding flow\n\
  • Sync engine\n\
\n\
  1. First\n\
  2. Second\n\
\n\
> Thanks — see you then.\n\
\n\
----------------------------------------\n\
[image: Sunset]\n\
[image: Rust banner]\n\
\n\
Top 日本語 😀\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn markdown_reads_as_markdown() {
        let doc = page::parse_html(SAMPLE);
        let out = markdown(&message(&doc));
        keep("message.md", out.as_bytes());
        assert!(out.starts_with("# Q4 roadmap \\<review\\> & plans\n\n**From:** Priya Shah \\<priya.shah@example.com\\>  \n"), "{out}");
        assert!(out.contains("\n---\n\n### Roadmap\n"), "{out}");
        assert!(out.contains("the **slides** for *Thursday* are [here](https://example.com/deck?a=1&b=2), see [https://example.com](https://example.com/)."), "{out}");
        assert!(out.contains("  \nSecond line \\<with\\> marks\\*."), "a line break inside a paragraph is a hard break: {out}");
        assert!(out.contains("\n- Onboarding flow\n- Sync `engine`\n") && out.contains("\n1. First\n2. Second\n"), "{out}");
        assert!(out.contains("\n> Thanks — see you ~~then~~.\n") && out.contains("*[image: Sunset]*") && out.contains("Top 日本語 😀"), "{out}");
        assert!(!out.contains("#top"));
        // Emphasis marks hug their text.
        let run = Run {
            text: " spaced \n".into(),
            style: RunStyle {
                bold: true,
                ..Default::default()
            },
        };
        assert_eq!(md_run(&run), " **spaced**   \n");
    }

    #[test]
    fn every_format_exports_every_demo_message() {
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
            for format in [Format::Word, Format::Html, Format::Text, Format::Markdown] {
                let bytes = export(format, &raw, Some(&m.subject), Some(m.date)).unwrap_or_else(|e| panic!("{:?} as {format:?}: {e}", m.subject));
                assert!(!bytes.is_empty());
                if format == Format::Word {
                    for (name, data) in unzip(&bytes).iter().filter(|(n, _)| n.ends_with(".xml") || n.ends_with(".rels")) {
                        roxmltree::Document::parse(std::str::from_utf8(data).unwrap()).unwrap_or_else(|e| panic!("{:?} {name}: {e}", m.subject));
                    }
                }
                keep(&format!("demo-{}.{}", m.id, format.extension()), &bytes);
            }
            exported += 1;
        }
        assert!(exported >= 20, "only {exported} demo messages had a body");
        assert_eq!(Format::ALL.map(Format::extension), ["pdf", "docx", "html", "txt", "md"]);
    }
}
