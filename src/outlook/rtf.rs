//! Outlook keeps many message bodies only as compressed RTF. This module
//! decompresses it (MS-OXRTFCP) and gets the original HTML or plain text
//! back out of it (MS-OXRTFEX, "encapsulated HTML").

use super::decode_codepage;

/// The dictionary every compressed RTF stream starts with.
const PRELOAD: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

const COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const UNCOMPRESSED: u32 = 0x414C_454D; // "MELA"

/// Bodies larger than this are not worth the memory; Outlook's own limit is far lower.
const MAX_RTF: usize = 64 << 20;

/// Decompresses a `PidTagRtfCompressed` value.
pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let word = |at: usize| Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
    let raw_size = (word(4)? as usize).min(MAX_RTF);
    let body = data.get(16..)?;
    match word(8)? {
        UNCOMPRESSED => Some(body[..raw_size.min(body.len())].to_vec()),
        COMPRESSED => {
            let mut dict = [0u8; 4096];
            dict[..PRELOAD.len()].copy_from_slice(PRELOAD);
            let mut write = PRELOAD.len();
            let mut out = Vec::with_capacity(raw_size);
            let mut i = 0;
            'stream: while i < body.len() {
                let control = body[i];
                i += 1;
                for bit in 0..8 {
                    if control & (1 << bit) != 0 {
                        let (Some(&hi), Some(&lo)) = (body.get(i), body.get(i + 1)) else { break 'stream };
                        i += 2;
                        let reference = u16::from_be_bytes([hi, lo]);
                        let mut offset = (reference >> 4) as usize;
                        let length = (reference & 0xF) as usize + 2;
                        if offset == write {
                            break 'stream;
                        }
                        for _ in 0..length {
                            let b = dict[offset];
                            offset = (offset + 1) % 4096;
                            dict[write] = b;
                            write = (write + 1) % 4096;
                            out.push(b);
                        }
                    } else {
                        let Some(&b) = body.get(i) else { break 'stream };
                        i += 1;
                        dict[write] = b;
                        write = (write + 1) % 4096;
                        out.push(b);
                    }
                    if out.len() >= MAX_RTF {
                        break 'stream;
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
pub enum RtfBody {
    /// The HTML the RTF was made from.
    Html(String),
    Text(String),
}

/// Groups whose contents are never part of the text.
const SKIPPED_DESTINATIONS: &[&str] = &[
    "fonttbl", "colortbl", "stylesheet", "info", "pict", "object", "header", "footer", "headerl", "headerr", "footerl",
    "footerr", "footnote", "listtable", "listoverridetable", "rsidtbl", "generator", "datastore", "themedata",
    "colorschememapping", "latentstyles", "xmlnstbl", "mmathPr", "fldinst", "operator", "author", "bkmkstart", "bkmkend",
];

#[derive(Clone)]
struct Group {
    skip: bool,
    /// Inside `{\*\htmltag ...}`: the contents are HTML source.
    html_tag: bool,
    /// Between `\htmlrtf` and `\htmlrtf0`: RTF that only stands in for the HTML.
    rtf_only: bool,
    /// Characters to skip after a `\u` (its fallback for old readers).
    uc: usize,
    /// The group began with `\*`.
    starred: bool,
    /// No control word seen yet (the first one names the destination).
    fresh: bool,
}

/// The body an RTF document holds: its encapsulated HTML if it has any,
/// otherwise its plain text.
pub fn body(rtf: &[u8]) -> Option<RtfBody> {
    if !rtf.starts_with(b"{\\rtf") {
        return None;
    }
    let head = &rtf[..rtf.len().min(400)];
    let from_html = head.windows(9).any(|w| w == b"\\fromhtml");

    let mut out = String::new();
    // Bytes waiting to be decoded with the document's code page.
    let mut bytes: Vec<u8> = Vec::new();
    let mut high_surrogate: Option<u16> = None;
    let mut codepage = 1252u32;
    let mut skip_after_unicode = 0usize;
    let mut stack = vec![Group {
        skip: false,
        html_tag: false,
        rtf_only: false,
        uc: 1,
        starred: false,
        fresh: true,
    }];

    fn flush(out: &mut String, bytes: &mut Vec<u8>, codepage: u32) {
        if !bytes.is_empty() {
            out.push_str(&decode_codepage(bytes, codepage));
            bytes.clear();
        }
    }
    let visible = |g: &Group| !g.skip && (g.html_tag || !g.rtf_only) && (from_html || !g.html_tag);

    let mut i = 0;
    while i < rtf.len() {
        let c = rtf[i];
        i += 1;
        match c {
            b'{' => {
                let mut g = stack.last().cloned().expect("root group");
                g.starred = false;
                g.fresh = true;
                stack.push(g);
            }
            b'}' => {
                flush(&mut out, &mut bytes, codepage);
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            b'\r' | b'\n' => {}
            b'\\' => {
                let Some(&next) = rtf.get(i) else { break };
                if next.is_ascii_alphabetic() {
                    let start = i;
                    while i < rtf.len() && rtf[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&rtf[start..i]).unwrap_or("");
                    let num_start = i;
                    if rtf.get(i) == Some(&b'-') {
                        i += 1;
                    }
                    while i < rtf.len() && rtf[i].is_ascii_digit() {
                        i += 1;
                    }
                    let param: Option<i32> = std::str::from_utf8(&rtf[num_start..i]).ok().and_then(|s| s.parse().ok());
                    if rtf.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    let g = stack.last_mut().expect("group");
                    let first = std::mem::replace(&mut g.fresh, false);
                    if first {
                        if word == "htmltag" && g.starred {
                            g.html_tag = true;
                        } else if g.starred || SKIPPED_DESTINATIONS.contains(&word) {
                            g.skip = true;
                        }
                    }
                    let show = visible(g);
                    let mut text: Option<&str> = None;
                    match word {
                        "par" | "line" | "sect" | "page" | "row" => text = Some("\r\n"),
                        "tab" | "cell" => text = Some("\t"),
                        "lquote" => text = Some("\u{2018}"),
                        "rquote" => text = Some("\u{2019}"),
                        "ldblquote" => text = Some("\u{201C}"),
                        "rdblquote" => text = Some("\u{201D}"),
                        "bullet" => text = Some("\u{2022}"),
                        "emdash" => text = Some("\u{2014}"),
                        "endash" => text = Some("\u{2013}"),
                        "emspace" | "enspace" | "qmspace" => text = Some(" "),
                        "htmlrtf" => g.rtf_only = param != Some(0),
                        "uc" => g.uc = param.unwrap_or(1).max(0) as usize,
                        "ansicpg" => codepage = param.unwrap_or(1252).max(0) as u32,
                        "bin" => i = (i + param.unwrap_or(0).max(0) as usize).min(rtf.len()),
                        "u" => {
                            let unit = param.unwrap_or(0) as u16;
                            skip_after_unicode = g.uc;
                            if show {
                                flush(&mut out, &mut bytes, codepage);
                                match (high_surrogate.take(), unit) {
                                    (Some(hi), 0xDC00..=0xDFFF) => {
                                        out.extend(char::decode_utf16([hi, unit]).map(|r| r.unwrap_or('\u{FFFD}')));
                                    }
                                    (_, 0xD800..=0xDBFF) => high_surrogate = Some(unit),
                                    (_, unit) => out.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}')),
                                }
                            }
                        }
                        _ => {}
                    }
                    if let (true, Some(t)) = (show, text) {
                        flush(&mut out, &mut bytes, codepage);
                        out.push_str(t);
                    }
                } else {
                    i += 1;
                    let g = stack.last_mut().expect("group");
                    let show = visible(g);
                    match next {
                        b'*' => g.starred = true,
                        b'\'' => {
                            let hex = rtf.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok());
                            let byte = hex.and_then(|h| u8::from_str_radix(h, 16).ok());
                            i = (i + 2).min(rtf.len());
                            g.fresh = false;
                            if skip_after_unicode > 0 {
                                skip_after_unicode -= 1;
                            } else if let (true, Some(b)) = (show, byte) {
                                bytes.push(b);
                            }
                        }
                        b'{' | b'}' | b'\\' => {
                            g.fresh = false;
                            if skip_after_unicode > 0 {
                                skip_after_unicode -= 1;
                            } else if show {
                                bytes.push(next);
                            }
                        }
                        b'~' if show => {
                            flush(&mut out, &mut bytes, codepage);
                            out.push('\u{A0}');
                        }
                        b'_' if show => bytes.push(b'-'),
                        b'\r' | b'\n' if show => {
                            flush(&mut out, &mut bytes, codepage);
                            out.push_str("\r\n");
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                let g = stack.last_mut().expect("group");
                g.fresh = false;
                if skip_after_unicode > 0 {
                    skip_after_unicode -= 1;
                } else if visible(g) {
                    bytes.push(c);
                }
            }
        }
    }
    flush(&mut out, &mut bytes, codepage);
    Some(if from_html { RtfBody::Html(out) } else { RtfBody::Text(out.trim().to_string()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preload_dictionary_is_the_documented_size() {
        assert_eq!(PRELOAD.len(), 207);
    }

    /// The compressed example of MS-OXRTFCP section 3.1.
    #[test]
    fn decompresses_the_specification_example() {
        let data: [u8; 46] = [
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03, 0x00, 0x0a,
            0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20,
            0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a,
        ];
        let mut full = data.to_vec();
        full.extend_from_slice(&[0x80, 0x0f, 0xa0]);
        let out = decompress(&full).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    }

    #[test]
    fn uncompressed_streams_are_passed_through() {
        let rtf = b"{\\rtf1 hi}";
        let mut data = Vec::new();
        data.extend_from_slice(&(rtf.len() as u32 + 12).to_le_bytes());
        data.extend_from_slice(&(rtf.len() as u32).to_le_bytes());
        data.extend_from_slice(&UNCOMPRESSED.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(rtf);
        assert_eq!(decompress(&data).unwrap(), rtf);
        assert_eq!(decompress(b"junk"), None);
    }

    #[test]
    fn gets_the_html_back_out_of_encapsulated_rtf() {
        let rtf = concat!(
            "{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 \\deff0{\\fonttbl{\\f0\\fswiss Arial;}}\r\n",
            "{\\colortbl\\red0\\green0\\blue0;}\r\n",
            "{\\*\\generator Riched20;}\r\n",
            "{\\*\\htmltag19 <html>}{\\*\\htmltag34 <head>}{\\*\\htmltag41 </head>}{\\*\\htmltag50 <body>}\r\n",
            "{\\*\\htmltag64 <p>}\\htmlrtf {\\htmlrtf0 Caf\\'e9 \\u8364?5 & {\\*\\htmltag84 <b>}\\htmlrtf {\\b \\htmlrtf0 bold\\htmlrtf }\\htmlrtf0 ",
            "{\\*\\htmltag92 </b>}\\{x\\}{\\*\\htmltag72 </p>}\\htmlrtf\\par}\\htmlrtf0\r\n",
            "{\\*\\htmltag58 </body>}{\\*\\htmltag27 </html>}}"
        );
        let Some(RtfBody::Html(html)) = body(rtf.as_bytes()) else { panic!("html expected") };
        assert_eq!(html, "<html><head></head><body><p>Caf\u{e9} \u{20ac}5 & <b>bold</b>{x}</p></body></html>");
    }

    #[test]
    fn plain_rtf_becomes_text() {
        let rtf = "{\\rtf1\\ansi\\ansicpg1251{\\fonttbl{\\f0 Arial;}}\\pard Hello\\par \\'cf\\'f0\\'e8\\'e2\\'e5\\'f2\\tab{\\b there}\\par}";
        assert_eq!(body(rtf.as_bytes()), Some(RtfBody::Text("Hello\r\nПривет\tthere".into())));
        assert_eq!(body(b"not rtf"), None);
    }
}
