//! System font discovery. We look for a Segoe-UI-like humanist sans in
//! regular / semibold / bold weights and fall back to egui's bundled fonts.

use std::path::Path;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

use crate::theme::{BOLD, ICONS_FILL, SEMIBOLD};

const UI_FAMILIES: &[&str] = &[
    "Segoe UI",
    "Selawik",
    "Open Sans",
    "Noto Sans",
    "Inter",
    "Helvetica Neue",
    "Fira Sans",
    "Cantarell",
    "Ubuntu",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
];

const MONO_FAMILIES: &[&str] = &[
    "Cascadia Mono",
    "Consolas",
    "JetBrains Mono",
    "Fira Mono",
    "DejaVu Sans Mono",
    "Noto Mono",
    "Liberation Mono",
    "Menlo",
];

const FALLBACK_FAMILIES: &[&str] = &["Droid Sans Fallback", "Microsoft YaHei", "Arial Unicode MS"];

struct Face {
    data: Vec<u8>,
    index: u32,
}

fn is_variable(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains('['))
        .unwrap_or(false)
}

/// Finds the face of `family` closest to `weight` (upright, normal width).
fn find_face(db: &fontdb::Database, family: &str, weight: u16, exact: bool) -> Option<Face> {
    find_face_styled(db, family, weight, exact, false)
}

/// Like [`find_face`], for the upright or the italic/oblique faces.
fn find_face_styled(db: &fontdb::Database, family: &str, weight: u16, exact: bool, italic: bool) -> Option<Face> {
    let mut best: Option<(&fontdb::FaceInfo, u16)> = None;
    for face in db.faces() {
        if !face
            .families
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(family))
        {
            continue;
        }
        if (face.style != fontdb::Style::Normal) != italic || face.stretch != fontdb::Stretch::Normal {
            continue;
        }
        if let fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) = &face.source {
            if is_variable(p) {
                continue;
            }
        }
        let dist = face.weight.0.abs_diff(weight);
        if exact && dist > 50 {
            continue;
        }
        if best.map(|(_, d)| dist < d).unwrap_or(true) {
            best = Some((face, dist));
        }
    }
    let (face, _) = best?;
    db.with_face_data(face.id, |data, index| Face {
        data: data.to_vec(),
        index,
    })
}

fn insert(fonts: &mut FontDefinitions, key: &str, face: Face) {
    let mut fd = FontData::from_owned(face.data);
    fd.index = face.index;
    fonts.font_data.insert(key.to_owned(), fd.into());
}

/// Installs fonts into the context and returns the name of the UI family used.
pub fn install(ctx: &egui::Context) -> String {
    let mut fonts = FontDefinitions::default();
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    let mut used = String::from("egui default");
    let mut ui_regular = None;
    for fam in UI_FAMILIES {
        if let Some(reg) = find_face(&db, fam, 400, true) {
            let semi = find_face(&db, fam, 600, true).or_else(|| find_face(&db, fam, 700, true));
            let bold = find_face(&db, fam, 700, true);
            used = fam.to_string();
            ui_regular = Some((reg, semi, bold));
            break;
        }
    }

    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut prop: Vec<String> = Vec::new();
    let mut semi_fam: Vec<String> = Vec::new();
    let mut bold_fam: Vec<String> = Vec::new();
    if let Some((reg, semi, bold)) = ui_regular {
        insert(&mut fonts, "ui-regular", reg);
        prop.push("ui-regular".into());
        match semi {
            Some(f) => {
                insert(&mut fonts, "ui-semibold", f);
                semi_fam.push("ui-semibold".into());
            }
            None => semi_fam.push("ui-regular".into()),
        }
        match bold {
            Some(f) => {
                insert(&mut fonts, "ui-bold", f);
                bold_fam.push("ui-bold".into());
            }
            None => bold_fam.push(semi_fam[0].clone()),
        }
    }
    // Keep egui's bundled fonts (text + emoji) as fallbacks.
    for k in &defaults {
        prop.push(k.clone());
        semi_fam.push(k.clone());
        bold_fam.push(k.clone());
    }
    for fam in FALLBACK_FAMILIES {
        if let Some(face) = find_face(&db, fam, 400, false) {
            insert(&mut fonts, "fallback-cjk", face);
            prop.push("fallback-cjk".into());
            semi_fam.push("fallback-cjk".into());
            bold_fam.push("fallback-cjk".into());
            break;
        }
    }
    fonts.families.insert(FontFamily::Proportional, prop);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semi_fam.clone());
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), bold_fam.clone());

    for fam in MONO_FAMILIES {
        if let Some(face) = find_face(&db, fam, 400, true) {
            insert(&mut fonts, "ui-mono", face);
            if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
                m.insert(0, "ui-mono".into());
            }
            break;
        }
    }

    // Icons: regular weight is a fallback in every text family so icons can be
    // mixed into labels; the filled variant gets its own family.
    let regular = egui_phosphor::Variant::Regular.font_bytes();
    fonts.font_data.insert(
        "phosphor".into(),
        FontData::from_static(regular).into(),
    );
    for family in [
        FontFamily::Proportional,
        FontFamily::Name(SEMIBOLD.into()),
        FontFamily::Name(BOLD.into()),
    ] {
        if let Some(list) = fonts.families.get_mut(&family) {
            let at = list.len().min(1);
            list.insert(at, "phosphor".into());
        }
    }
    egui_phosphor::add_font_bytes_as_family(
        &mut fonts,
        ICONS_FILL,
        egui_phosphor::Variant::Fill.font_bytes(),
    );

    ctx.set_fonts(fonts);
    used
}

// ------------------------------------------------------------------ export

/// CJK and symbol fonts tried, in order, for characters the text font lacks
/// when a message is saved as a PDF.
const EXPORT_FALLBACK_FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Droid Sans Fallback",
    "WenQuanYi Zen Hei",
    "Microsoft YaHei",
    "PingFang SC",
    "Hiragino Sans",
    "Yu Gothic",
    "Malgun Gothic",
    "Arial Unicode MS",
];

/// A font file and the face inside it.
#[derive(Clone)]
pub struct FaceData {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
}

impl From<Face> for FaceData {
    fn from(f: Face) -> Self {
        FaceData {
            data: Arc::new(f.data),
            index: f.index,
        }
    }
}

/// Fonts for documents written to disk (PDF export). A missing style is
/// `None` and synthesised by the exporter.
pub struct ExportFonts {
    pub regular: FaceData,
    pub bold: Option<FaceData>,
    pub italic: Option<FaceData>,
    pub bold_italic: Option<FaceData>,
    pub mono: FaceData,
    /// Tried in order for characters the fonts above don't have.
    pub fallbacks: Vec<FaceData>,
}

fn bundled(defs: &FontDefinitions, name: &str) -> Option<FaceData> {
    defs.font_data.get(name).map(|d| FaceData {
        data: Arc::new(d.font.to_vec()),
        index: d.index,
    })
}

impl ExportFonts {
    /// Only the fonts shipped inside the app: the same on every computer.
    pub fn bundled() -> ExportFonts {
        let defs = FontDefinitions::default();
        let regular = bundled(&defs, "Ubuntu-Light").expect("bundled text font");
        ExportFonts {
            mono: bundled(&defs, "Hack").unwrap_or_else(|| regular.clone()),
            fallbacks: ["NotoEmoji-Regular", "emoji-icon-font"]
                .iter()
                .filter_map(|n| bundled(&defs, n))
                .collect(),
            regular,
            bold: None,
            italic: None,
            bold_italic: None,
        }
    }

    /// The system's UI font family (the one the app itself uses) with the
    /// bundled fonts and a CJK font as fallbacks.
    pub fn system() -> ExportFonts {
        let mut fonts = ExportFonts::bundled();
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        for fam in UI_FAMILIES {
            if let Some(reg) = find_face(&db, fam, 400, true) {
                // The bundled text font still covers what this family lacks.
                let previous = std::mem::replace(&mut fonts.regular, reg.into());
                fonts.fallbacks.insert(0, previous);
                fonts.bold = find_face(&db, fam, 700, true)
                    .or_else(|| find_face(&db, fam, 600, true))
                    .map(Into::into);
                fonts.italic = find_face_styled(&db, fam, 400, true, true).map(Into::into);
                fonts.bold_italic = find_face_styled(&db, fam, 700, true, true)
                    .or_else(|| find_face_styled(&db, fam, 600, true, true))
                    .map(Into::into);
                break;
            }
        }
        for fam in MONO_FAMILIES {
            if let Some(face) = find_face(&db, fam, 400, true) {
                fonts.mono = face.into();
                break;
            }
        }
        for fam in EXPORT_FALLBACK_FAMILIES {
            if let Some(face) = find_face(&db, fam, 400, false) {
                fonts.fallbacks.push(face.into());
                break;
            }
        }
        fonts
    }
}
