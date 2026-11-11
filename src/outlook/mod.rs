//! Import of Outlook data files (`.pst` and `.ost`): mail with its folders,
//! contacts, calendar events and tasks.
//!
//! `reader` opens the file (read-only) and hands out items as bags of MAPI
//! properties, `convert` turns those into the app's own types, and
//! `import` writes them into the store.

use std::collections::HashMap;

pub mod convert;
mod import;
mod reader;
pub mod rtf;

pub use import::{apply, mailbox_account, run, start, Job, Progress, PROVIDER};

/// A MAPI property value, reduced to what the import needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Text(String),
    /// Text in the item's code page (ANSI files).
    Text8(Vec<u8>),
    Bin(Vec<u8>),
    /// Seconds since 1970 (UTC).
    Time(i64),
    List(Vec<String>),
}

/// The properties of an item, a recipient or an attachment, by property id.
#[derive(Clone, Debug, Default)]
pub struct Props {
    map: HashMap<u16, Value>,
    codepage: u32,
}

impl Props {
    pub fn new(map: HashMap<u16, Value>, codepage: u32) -> Props {
        Props { map, codepage }
    }

    pub fn text(&self, id: u16) -> Option<String> {
        match self.map.get(&id)? {
            Value::Text(s) => Some(s.clone()),
            Value::Text8(b) => Some(decode_codepage(b, self.codepage)),
            _ => None,
        }
    }

    pub fn int(&self, id: u16) -> Option<i64> {
        match self.map.get(&id)? {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn bool(&self, id: u16) -> Option<bool> {
        match self.map.get(&id)? {
            Value::Bool(v) => Some(*v),
            Value::Int(v) => Some(*v != 0),
            _ => None,
        }
    }

    pub fn bin(&self, id: u16) -> Option<&[u8]> {
        match self.map.get(&id)? {
            Value::Bin(v) => Some(v),
            _ => None,
        }
    }

    pub fn time(&self, id: u16) -> Option<i64> {
        match self.map.get(&id)? {
            Value::Time(v) => Some(*v),
            _ => None,
        }
    }

    pub fn list(&self, id: u16) -> Vec<String> {
        match self.map.get(&id) {
            Some(Value::List(v)) => v.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            Some(Value::Text(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
            _ => Vec::new(),
        }
    }
}

/// Property ids of this file's named properties: by (property set, numeric
/// id) and by (property set, name). The set is the first field of its GUID.
#[derive(Clone, Debug, Default)]
pub struct Named {
    pub ids: HashMap<(u32, u32), u16>,
    pub names: HashMap<(u32, String), u16>,
}

impl Named {
    pub fn by_id(&self, set: u32, lid: u32) -> Option<u16> {
        self.ids.get(&(set, lid)).copied()
    }

    pub fn by_name(&self, set: u32, name: &str) -> Option<u16> {
        self.names.get(&(set, name.to_string())).copied()
    }
}

pub enum AttachmentData {
    Bytes(Vec<u8>),
    /// An attached message.
    Message(Box<RawItem>),
    /// A link to a file, or data in a form that can't be carried over.
    None,
}

pub struct RawAttachment {
    pub props: Props,
    pub data: AttachmentData,
}

/// One item of an Outlook data file.
pub struct RawItem {
    /// Message class, e.g. `IPM.Note`.
    pub class: String,
    pub props: Props,
    pub recipients: Vec<Props>,
    pub attachments: Vec<RawAttachment>,
}

/// Text in a Windows code page (as numbered by Windows).
pub fn decode_codepage(bytes: &[u8], codepage: u32) -> String {
    use encoding_rs::*;
    let encoding: &'static Encoding = match codepage {
        65001 => UTF_8,
        1200 => UTF_16LE,
        1201 => UTF_16BE,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 | 51949 => EUC_KR,
        950 => BIG5,
        866 => IBM866,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 | 28599 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        10000 => MACINTOSH,
        20866 => KOI8_R,
        21866 => KOI8_U,
        28592 => ISO_8859_2,
        28593 => ISO_8859_3,
        28594 => ISO_8859_4,
        28595 => ISO_8859_5,
        28596 => ISO_8859_6,
        28597 => ISO_8859_7,
        28598 | 38598 => ISO_8859_8,
        28603 => ISO_8859_13,
        28605 => ISO_8859_15,
        50220..=50222 => ISO_2022_JP,
        51932 => EUC_JP,
        54936 => GB18030,
        // Unknown or unset: valid UTF-8 as such, anything else as Western.
        0 => return String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| WINDOWS_1252.decode(bytes).0.into_owned()),
        _ => WINDOWS_1252,
    };
    encoding.decode(bytes).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_pages_decode() {
        assert_eq!(decode_codepage(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2], 1251), "Привет");
        assert_eq!(decode_codepage(&[0x93, 0xFA, 0x96, 0x7B], 932), "日本");
        assert_eq!(decode_codepage("café".as_bytes(), 65001), "café");
        assert_eq!(decode_codepage(&[b'c', b'a', b'f', 0xE9], 20127), "café");
        assert_eq!(decode_codepage("né".as_bytes(), 0), "né");
        assert_eq!(decode_codepage(&[b'n', 0xE9], 0), "né");
    }
}
