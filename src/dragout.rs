//! Dragging messages out of the window into another program: the messages
//! are written as `.eml` files and handed to the system's drag-and-drop, so
//! the other program receives ordinary files (an accounting program files
//! them with a customer, a folder window copies them, a mail editor attaches
//! them).
//!
//! Inside the window a drag moves messages between folders; it becomes a
//! file drag when the pointer leaves the window. Only Windows is supported
//! so far (see [`supported`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::model::MsgId;
use crate::store::Store;
use crate::util;

/// Whether messages can be dragged into other programs on this platform.
pub fn supported() -> bool {
    cfg!(windows)
}

/// Where the files of drags are kept. The receiving program may read them
/// some time after the drop, so they stay until the next start.
pub fn folder(data_dir: &Path) -> PathBuf {
    data_dir.join("dragged")
}

/// Removes what earlier runs left behind.
pub fn clean(data_dir: &Path) {
    let _ = std::fs::remove_dir_all(folder(data_dir));
}

/// Longest file name (without ".eml"), in characters.
const MAX_NAME: usize = 80;

/// What became of the messages of a drag.
#[derive(Debug, Default, PartialEq)]
pub struct Exported {
    pub files: Vec<PathBuf>,
    /// Messages whose content hasn't been downloaded yet.
    pub missing: Vec<MsgId>,
}

/// Writes the messages as `.eml` files, named after their subjects, into a
/// new folder below `dir`.
pub fn export(store: &Store, ids: &[MsgId], dir: &Path) -> Result<Exported> {
    // A folder per drag, so equal subjects of different drags don't clash.
    let mut n = 1;
    let target = loop {
        let candidate = dir.join(n.to_string());
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };
    let mut out = Exported::default();
    let mut used: HashSet<String> = HashSet::new();
    for &id in ids {
        let Some(raw) = store.raw_body(id)? else {
            out.missing.push(id);
            continue;
        };
        let subject = store.message(id)?.map(|m| m.subject).unwrap_or_default();
        // Short enough for Windows' limit on the length of a whole path.
        let short: String = subject.trim().chars().take(MAX_NAME).collect();
        let base = util::sanitize_filename(if short.trim().is_empty() { "message" } else { short.trim() });
        // Windows compares names without regard to case.
        let mut name = format!("{base}.eml");
        let mut copy = 2;
        while !used.insert(name.to_lowercase()) {
            name = format!("{base} ({copy}).eml");
            copy += 1;
        }
        std::fs::create_dir_all(&target)?;
        let mut path = target.join(name);
        if std::fs::write(&path, &raw).is_err() {
            // A name this file system won't take: a plain one always works.
            path = target.join(format!("message {}.eml", out.files.len() + 1));
            std::fs::write(&path, &raw)?;
        }
        out.files.push(path);
    }
    Ok(out)
}

/// Starts a system drag of `files` from the app. Returns when the drag is
/// over, with whether the files were dropped somewhere.
#[cfg(windows)]
pub fn start(files: &[PathBuf]) -> Result<bool> {
    windows_impl::drag_files(files)
}

#[cfg(not(windows))]
pub fn start(_files: &[PathBuf]) -> Result<bool> {
    Err(anyhow::anyhow!("dragging into other programs isn't available on this system"))
}

/// The list of files in the form drop targets expect (`CF_HDROP`): a
/// `DROPFILES` header, then the paths as NUL-terminated UTF-16, then one
/// more NUL.
#[cfg_attr(not(windows), allow(dead_code))]
fn hdrop_bytes(files: &[PathBuf]) -> Vec<u8> {
    // DROPFILES: pFiles (offset of the list), pt.x, pt.y, fNC, fWide.
    const HEADER: u32 = 20;
    let mut out = Vec::new();
    for v in [HEADER, 0, 0, 0, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for f in files {
        // Old-style paths: not every program takes the `\\?\` form.
        let text = f.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        for unit in text.encode_utf16().chain(std::iter::once(0)) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(windows)]
mod windows_impl {
    //! An OLE drag source offering a list of files, as Explorer does when
    //! files are dragged out of a folder window.

    use std::mem::ManuallyDrop;
    use std::path::PathBuf;

    use anyhow::{anyhow, Result};
    use windows::core::{implement, Ref, BOOL, HRESULT};
    use windows::Win32::Foundation::{
        DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL, E_OUTOFMEMORY,
        OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{
        IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0,
        TYMED_HGLOBAL,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE};
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;

    fn file_list_format() -> FORMATETC {
        FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    /// The data of the drag: the file list and nothing else.
    #[implement(IDataObject)]
    struct FileList {
        hdrop: Vec<u8>,
    }

    impl FileList_Impl {
        fn offers(&self, format: *const FORMATETC) -> bool {
            // SAFETY: OLE passes a valid FORMATETC (or null).
            let Some(f) = (unsafe { format.as_ref() }) else { return false };
            f.cfFormat == CF_HDROP.0 && f.tymed & TYMED_HGLOBAL.0 as u32 != 0 && f.dwAspect == DVASPECT_CONTENT.0
        }
    }

    impl IDataObject_Impl for FileList_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            if !self.offers(format) {
                return Err(DV_E_FORMATETC.into());
            }
            // The receiver owns (and frees) the copy.
            // SAFETY: the block is allocated with the size that is copied into it.
            unsafe {
                let global = GlobalAlloc(GMEM_MOVEABLE, self.hdrop.len())?;
                let target = GlobalLock(global);
                if target.is_null() {
                    return Err(E_OUTOFMEMORY.into());
                }
                std::ptr::copy_nonoverlapping(self.hdrop.as_ptr(), target.cast::<u8>(), self.hdrop.len());
                let _ = GlobalUnlock(global);
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL.0 as u32,
                    u: STGMEDIUM_0 { hGlobal: global },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            }
        }

        fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
            if self.offers(format) {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }

        fn GetCanonicalFormatEtc(&self, _format: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
            // SAFETY: OLE passes a valid pointer for the result (or null).
            if let Some(out) = unsafe { out.as_mut() } {
                out.ptd = std::ptr::null_mut();
            }
            DATA_S_SAMEFORMATETC
        }

        fn SetData(&self, _format: *const FORMATETC, _medium: *const STGMEDIUM, _release: BOOL) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
            if direction != DATADIR_GET.0 as u32 {
                return Err(E_NOTIMPL.into());
            }
            // SAFETY: the slice outlives the call, which copies it.
            unsafe { SHCreateStdEnumFmtEtc(&[file_list_format()]) }
        }

        fn DAdvise(&self, _format: *const FORMATETC, _flags: u32, _sink: Ref<IAdviseSink>) -> windows::core::Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    /// Ends the drag when the mouse button is released (drop) or Escape is
    /// pressed (cancel).
    #[implement(IDropSource)]
    struct Source;

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape_pressed: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape_pressed.as_bool() {
                DRAGDROP_S_CANCEL
            } else if keys.0 & MK_LBUTTON.0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    pub fn drag_files(files: &[PathBuf]) -> Result<bool> {
        let data: IDataObject = FileList {
            hdrop: super::hdrop_bytes(files),
        }
        .into();
        let source: IDropSource = Source.into();
        // SAFETY: plain OLE calls on the UI thread; both objects live until
        // DoDragDrop returns.
        unsafe {
            // The window toolkit has normally done this already; a second
            // call only counts up and is undone below.
            let initialized = OleInitialize(None);
            let mut effect = DROPEFFECT::default();
            let result = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
            if initialized.is_ok() {
                OleUninitialize();
            }
            match result {
                // Released over something that took the files.
                r if r == DRAGDROP_S_DROP => Ok(effect != DROPEFFECT_NONE),
                r if r == DRAGDROP_S_CANCEL || r == S_OK || r == S_FALSE => Ok(false),
                r => Err(anyhow!("{}", r.message())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FolderRole, Importance};
    use crate::store::NewMessage;

    fn message(store: &Store, folder: i64, uid: i64, subject: &str, raw: Option<&[u8]>) -> MsgId {
        let new = NewMessage {
            account_id: "acc".into(),
            folder_id: folder,
            uid,
            message_id: format!("{uid}@example.com"),
            in_reply_to: String::new(),
            references: Vec::new(),
            subject: subject.into(),
            from_name: "Ann".into(),
            from_addr: "ann@example.com".into(),
            to_list: String::new(),
            cc_list: String::new(),
            date: 1_794_321_000,
            preview: String::new(),
            size: 0,
            seen: true,
            flagged: false,
            answered: false,
            forwarded: false,
            draft: false,
            has_attachments: false,
            importance: Importance::Normal,
            focused: true,
            list_unsubscribe: None,
            raw: raw.map(|r| r.to_vec()),
            body_text: String::new(),
        };
        store.insert_message(&new).unwrap().0
    }

    #[test]
    fn messages_become_eml_files_named_after_their_subjects() {
        let store = Store::memory().unwrap();
        let folder = store.upsert_folder("acc", "INBOX", "Inbox", Some("/"), FolderRole::Inbox, true).unwrap();
        let a = message(&store, folder, 1, "Invoice 1042: March / April?", Some(b"Subject: a\r\n\r\nfirst"));
        let b = message(&store, folder, 2, "invoice 1042: march / april?", Some(b"Subject: b\r\n\r\nsecond"));
        let c = message(&store, folder, 3, "", Some(b"Subject:\r\n\r\nthird"));
        let d = message(&store, folder, 4, "Not downloaded", None);
        let long = message(&store, folder, 5, &"Very long subject ".repeat(40), Some(b"Subject: l\r\n\r\nlong"));

        let dir = std::env::temp_dir().join(format!("rustmail-dragout-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = export(&store, &[a, b, c, d], &dir).unwrap();
        assert_eq!(out.missing, [d]);
        let names: Vec<String> = out.files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.iter().all(|n| n.ends_with(".eml") && !n.contains(['/', ':', '?'])), "{names:?}");
        assert_ne!(names[0].to_lowercase(), names[1].to_lowercase(), "names differ by more than case");
        assert!(names[1].ends_with(" (2).eml"), "{names:?}");
        assert_eq!(names[2], "message.eml");
        assert_eq!(std::fs::read(&out.files[1]).unwrap(), b"Subject: b\r\n\r\nsecond");
        assert!(out.files.iter().all(|p| p.is_absolute() && p.parent() == Some(dir.join("1").as_path())));

        // The next drag gets its own folder; one without content makes none.
        let again = export(&store, &[a, long], &dir).unwrap();
        let long_name = again.files[1].file_name().unwrap().to_string_lossy().into_owned();
        assert!(long_name.chars().count() <= MAX_NAME + 4 && long_name.starts_with("Very long subject"), "{long_name}");
        assert_eq!(again.files[0].parent(), Some(dir.join("2").as_path()));
        let none = export(&store, &[d], &dir).unwrap();
        assert!(none.files.is_empty() && !dir.join("3").exists());

        clean(dir.parent().unwrap().join("no-such-rustmail-dir").as_path());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_file_list_has_the_layout_drop_targets_read() {
        let bytes = hdrop_bytes(&[PathBuf::from(r"C:\a\é.eml"), PathBuf::from(r"\\?\D:\b.eml")]);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!((word(0), word(4), word(8), word(12), word(16)), (20, 0, 0, 0, 1), "header: offset, point, client area flag, wide flag");
        let units: Vec<u16> = bytes[20..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "C:\\a\\é.eml\0D:\\b.eml\0\0");
    }
}
