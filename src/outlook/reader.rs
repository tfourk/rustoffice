//! Reads an Outlook data file with the `outlook-pst` crate. The file is only
//! ever opened for reading.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::rc::Rc;

use anyhow::{anyhow, Context, Result};
use outlook_pst::ltp::prop_context::PropertyValue;
use outlook_pst::ltp::table_context::TableContext;
use outlook_pst::messaging::attachment::{AnsiAttachment, Attachment, AttachmentData as PstAttachmentData, UnicodeAttachment};
use outlook_pst::messaging::folder::Folder;
use outlook_pst::messaging::message::{AnsiMessage, Message, UnicodeMessage};
use outlook_pst::messaging::named_prop::{NamedPropertyGuid, NamedPropertyId};
use outlook_pst::messaging::store::{AnsiStore, Store, UnicodeStore};
use outlook_pst::ndb::node_id::NodeId;
use outlook_pst::{AnsiPstFile, PstReader, UnicodePstFile};

use super::{decode_codepage, AttachmentData, Named, Props, RawAttachment, RawItem, Value};

/// The root folder every data file has (`NID_ROOT_FOLDER`).
const ROOT_FOLDER: u32 = 0x122;
/// Guards against damaged files whose folders refer to each other.
const MAX_DEPTH: usize = 40;
/// Windows FILETIME (100 ns since 1601) of the Unix epoch.
const EPOCH_FILETIME: i64 = 116_444_736_000_000_000;

enum AnyStore {
    Unicode(Rc<UnicodeStore>),
    Ansi(Rc<AnsiStore>),
}

pub struct Archive {
    store: AnyStore,
    pub named: Named,
    /// The data file's own name ("Personal Folders"), if it has one.
    pub name: String,
}

pub struct FolderInfo {
    /// Names from below the mailbox root down to this folder.
    pub path: Vec<String>,
    /// Container class, e.g. `IPF.Note`; empty for plain mail folders.
    pub class: String,
    pub count: usize,
    node: u32,
}

fn convert(v: &PropertyValue) -> Option<Value> {
    Some(match v {
        PropertyValue::Integer16(n) => Value::Int(*n as i64),
        PropertyValue::Integer32(n) => Value::Int(*n as i64),
        PropertyValue::Integer64(n) => Value::Int(*n),
        PropertyValue::Boolean(b) => Value::Bool(*b),
        // Strings may carry their terminating NUL.
        PropertyValue::Unicode(s) => Value::Text(String::from_utf16_lossy(s.buffer()).trim_end_matches('\0').to_string()),
        PropertyValue::String8(s) => {
            let b = s.buffer();
            Value::Text8(b[..b.iter().rposition(|c| *c != 0).map_or(0, |i| i + 1)].to_vec())
        }
        PropertyValue::Binary(b) => Value::Bin(b.buffer().to_vec()),
        PropertyValue::Time(t) => Value::Time((*t - EPOCH_FILETIME).div_euclid(10_000_000)),
        PropertyValue::MultipleUnicode(list) => Value::List(list.iter().map(|s| String::from_utf16_lossy(s.buffer())).collect()),
        PropertyValue::MultipleString8(list) => Value::List(list.iter().map(|s| decode_codepage(s.buffer(), 0)).collect()),
        _ => return None,
    })
}

fn props_of<'a>(values: impl Iterator<Item = (&'a u16, &'a PropertyValue)>, codepage: Option<u32>) -> Props {
    let map: HashMap<u16, Value> = values.filter_map(|(id, v)| Some((*id, convert(v)?))).collect();
    // PidTagMessageCodepage, else PidTagInternetCodepage.
    let own = [0x3FFDu16, 0x3FDE].iter().find_map(|id| match map.get(id) {
        Some(Value::Int(cp)) if *cp > 0 => Some(*cp as u32),
        _ => None,
    });
    Props::new(map, codepage.or(own).unwrap_or(0))
}

/// The rows of a table (recipients, attachments) as property bags.
fn rows(table: &dyn TableContext, codepage: u32) -> Vec<(u32, Props)> {
    let context = table.context();
    table
        .rows_matrix()
        .filter_map(|row| {
            let columns = row.columns(context).ok()?;
            let map: HashMap<u16, Value> = context
                .columns()
                .iter()
                .zip(columns)
                .filter_map(|(col, value)| {
                    let value = table.read_column(&value?, col.prop_type()).ok()?;
                    Some((col.prop_id(), convert(&value)?))
                })
                .collect();
            Some((u32::from(row.id()), Props::new(map, codepage)))
        })
        .collect()
}

/// An item without its attachments (the part every message type shares).
fn item_of(message: &dyn Message) -> RawItem {
    let props = props_of(message.properties().iter(), None);
    let recipients = message
        .recipient_table()
        .map(|t| rows(t.as_ref(), props.codepage).into_iter().map(|(_, p)| p).collect())
        .unwrap_or_default();
    RawItem {
        class: message.properties().message_class().unwrap_or_default(),
        props,
        recipients,
        attachments: Vec::new(),
    }
}

fn attachment_of(attachment: &dyn Attachment, row: Props) -> RawAttachment {
    let mut props = props_of(attachment.properties().iter(), Some(row.codepage));
    // The table row has the names even when the attachment object lacks them.
    for (id, value) in row.map {
        props.map.entry(id).or_insert(value);
    }
    let data = match attachment.data() {
        Some(PstAttachmentData::Binary(b)) => AttachmentData::Bytes(b.buffer().to_vec()),
        Some(PstAttachmentData::Message(m)) => AttachmentData::Message(Box::new(item_of(m.as_ref()))),
        None => AttachmentData::None,
    };
    RawAttachment { props, data }
}

/// Reads an item with the concrete message and attachment types of one
/// file format (the library has no format-independent way to open
/// attachments).
macro_rules! read_item {
    ($store:expr, $node:expr, $message:ty, $attachment:ty) => {{
        let entry_id = $store.properties().make_entry_id(NodeId::from($node))?;
        let message = <$message>::read($store.clone(), &entry_id, None)?;
        let mut item = item_of(message.as_ref());
        if let Some(table) = message.attachment_table() {
            for (row_id, row) in rows(table.as_ref(), item.props.codepage) {
                // An attachment that can't be read costs only itself.
                if let Ok(a) = <$attachment>::read(message.clone(), NodeId::from(row_id), None) {
                    item.attachments.push(attachment_of(a.as_ref(), row));
                }
            }
        }
        io::Result::Ok(item)
    }};
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive> {
        // Every PST and OST starts with "!BDN".
        let mut magic = [0u8; 4];
        File::open(path)
            .and_then(|mut f| io::Read::read_exact(&mut f, &mut magic))
            .or_else(|e| if e.kind() == io::ErrorKind::UnexpectedEof { Ok(()) } else { Err(e) })
            .with_context(|| format!("could not open {}", path.display()))?;
        if &magic != b"!BDN" {
            return Err(anyhow!("this is not an Outlook data file (.pst or .ost)"));
        }
        let reader = || -> io::Result<Box<dyn PstReader>> { Ok(Box::new(BufReader::new(File::open(path)?))) };
        let unicode = reader()
            .and_then(UnicodePstFile::read_from)
            .and_then(|f| UnicodeStore::read(Rc::new(f)));
        let store = match unicode {
            Ok(s) => AnyStore::Unicode(s),
            Err(unicode_error) => {
                // Outlook 97-2002 files.
                let ansi = reader().and_then(AnsiPstFile::read_from).and_then(|f| AnsiStore::read(Rc::new(f)));
                match ansi {
                    Ok(s) => AnyStore::Ansi(s),
                    Err(_) => return Err(anyhow!("the file is damaged or in a format RustMail can't read ({unicode_error})")),
                }
            }
        };
        let mut archive = Archive {
            store,
            named: Named::default(),
            name: String::new(),
        };
        archive.name = archive.dyn_store().properties().display_name().unwrap_or_default().trim().to_string();
        archive.named = archive.read_named().unwrap_or_default();
        Ok(archive)
    }

    fn dyn_store(&self) -> Rc<dyn Store> {
        match &self.store {
            AnyStore::Unicode(s) => s.clone(),
            AnyStore::Ansi(s) => s.clone(),
        }
    }

    fn read_named(&self) -> io::Result<Named> {
        let map = self.dyn_store().named_property_map()?;
        let properties = map.properties();
        let guids = properties.stream_guid().unwrap_or_default();
        let mut named = Named::default();
        for entry in properties.stream_entry()? {
            let set = match entry.guid() {
                NamedPropertyGuid::Mapi => 0x0002_0328,
                NamedPropertyGuid::PublicStrings => 0x0002_0329,
                NamedPropertyGuid::GuidIndex(i) => match guids.get(i as usize) {
                    Some(g) => g.data1(),
                    None => continue,
                },
                NamedPropertyGuid::None => continue,
            };
            match entry.id() {
                NamedPropertyId::Number(lid) => {
                    named.ids.insert((set, lid), entry.prop_id());
                }
                NamedPropertyId::StringOffset(offset) => {
                    if let Ok(s) = properties.lookup_string(offset) {
                        let units: Vec<u16> = s.buffer().chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                        named.names.insert((set, String::from_utf16_lossy(&units)), entry.prop_id());
                    }
                }
            }
        }
        Ok(named)
    }

    fn open_folder(&self, node: u32) -> io::Result<Rc<dyn Folder>> {
        let store = self.dyn_store();
        let entry_id = store.properties().make_entry_id(NodeId::from(node))?;
        store.open_folder(&entry_id)
    }

    fn children(folder: &dyn Folder) -> Vec<u32> {
        folder.hierarchy_table().map(|t| t.rows_matrix().map(|r| u32::from(r.id())).collect()).unwrap_or_default()
    }

    /// The folder mail clients show as the top of the mailbox. A PST names
    /// it; an OST has it as "IPM_SUBTREE" below the root (one per store it
    /// caches, of which the one with content is wanted).
    fn mailbox_root(&self) -> Result<u32> {
        let store = self.dyn_store();
        if let Ok(entry_id) = store.properties().ipm_sub_tree_entry_id() {
            if store.open_folder(&entry_id).is_ok() {
                return Ok(u32::from(entry_id.node_id()));
            }
        }
        let root = self.open_folder(ROOT_FOLDER).context("the file has no root folder")?;
        let mut best: Option<(usize, u32)> = None;
        let mut queue: Vec<(u32, usize)> = Self::children(root.as_ref()).into_iter().map(|n| (n, 1)).collect();
        let mut seen = HashSet::new();
        while let Some((node, depth)) = queue.pop() {
            if depth > 3 || !seen.insert(node) {
                continue;
            }
            let Ok(folder) = self.open_folder(node) else { continue };
            if folder.properties().display_name().unwrap_or_default() == "IPM_SUBTREE" {
                let size = Self::children(folder.as_ref()).len();
                if best.is_none_or(|(s, _)| size > s) {
                    best = Some((size, node));
                }
            } else {
                queue.extend(Self::children(folder.as_ref()).into_iter().map(|n| (n, depth + 1)));
            }
        }
        Ok(best.map(|(_, node)| node).unwrap_or(ROOT_FOLDER))
    }

    /// Every folder of the mailbox, parents before their children.
    pub fn folders(&self) -> Result<Vec<FolderInfo>> {
        let root = self.mailbox_root()?;
        let top = self.open_folder(root).context("the mailbox's top folder can't be read")?;
        let mut out = Vec::new();
        let mut seen = HashSet::from([root]);
        // Depth-first, keeping the file's order.
        let mut stack: Vec<(u32, Vec<String>)> = Self::children(top.as_ref()).into_iter().rev().map(|n| (n, Vec::new())).collect();
        while let Some((node, parent)) = stack.pop() {
            if parent.len() >= MAX_DEPTH || !seen.insert(node) {
                continue;
            }
            let Ok(folder) = self.open_folder(node) else { continue };
            let properties = folder.properties();
            let name = properties.display_name().unwrap_or_default().trim().to_string();
            let mut path = parent;
            path.push(if name.is_empty() { format!("Folder {node:X}") } else { name });
            let class = match properties.get(0x3613).and_then(convert) {
                Some(Value::Text(s)) => s,
                Some(Value::Text8(b)) => decode_codepage(&b, 0),
                _ => String::new(),
            };
            let count = folder.contents_table().map(|t| t.rows_matrix().count()).unwrap_or(0);
            stack.extend(Self::children(folder.as_ref()).into_iter().rev().map(|n| (n, path.clone())));
            out.push(FolderInfo { path, class, count, node });
        }
        Ok(out)
    }

    /// Ids of the items in a folder, for [`Archive::item`].
    pub fn item_ids(&self, folder: &FolderInfo) -> Result<Vec<u32>> {
        let f = self.open_folder(folder.node)?;
        Ok(f.contents_table().map(|t| t.rows_matrix().map(|r| u32::from(r.id())).collect()).unwrap_or_default())
    }

    pub fn item(&self, id: u32) -> Result<RawItem> {
        Ok(match &self.store {
            AnyStore::Unicode(s) => read_item!(s, id, UnicodeMessage, UnicodeAttachment)?,
            AnyStore::Ansi(s) => read_item!(s, id, AnsiMessage, AnsiAttachment)?,
        })
    }
}
