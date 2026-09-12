//! A system font for the glyphs egui does not bundle.
//!
//! egui ships Ubuntu-Light plus two emoji fonts: Latin, Greek, Cyrillic and
//! emoji — but no CJK. A script whose controls contain Chinese therefore draws
//! as empty boxes. Bundling a CJK font would add megabytes to this crate, so we
//! use one the machine already has:
//!
//! * `AU3_GUI_FONT` (and `AU3_GUI_FONT_INDEX` for a `.ttc` collection) point at
//!   a specific file;
//! * otherwise a short list of well-known paths is tried, then a shallow scan of
//!   the system font directories.
//!
//! The font is installed at the *lowest* priority, so Latin text keeps egui's
//! own font and only the characters it lacks come from here.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily, FontId};

/// Where a CJK-capable font usually lives, most specific first.
const CANDIDATES: &[&str] = &[
    // Linux
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Light.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    // Windows (SystemRoot is the reliable way to spell C:\Windows)
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
    r"C:\Windows\Fonts\Deng.ttf",
    // macOS
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
];

/// Directories a last-resort scan walks.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/System/Library/Fonts",
];

/// Name fragments that mark a font as worth trying, used by the scan.
const HINTS: &[&str] = &[
    "cjk",
    "wqy",
    "sourcehan",
    "source-han",
    "notosanssc",
    "msyh",
    "simhei",
    "simsun",
    "pingfang",
    "hiragino",
];

struct Fallback {
    path: PathBuf,
    data: FontData,
}

/// The font this machine can render Chinese with, found once per process.
fn fallback() -> Option<&'static Fallback> {
    static FOUND: OnceLock<Option<Fallback>> = OnceLock::new();
    FOUND.get_or_init(find).as_ref()
}

fn find() -> Option<Fallback> {
    if let Some(path) = std::env::var_os("AU3_GUI_FONT") {
        let path = PathBuf::from(path);
        let index = std::env::var("AU3_GUI_FONT_INDEX")
            .ok()
            .and_then(|index| index.parse().ok())
            .unwrap_or(0);
        match load(&path, index) {
            Some(found) => return Some(found),
            None => eprintln!(
                "[gui] AU3_GUI_FONT={} is not a readable font file; falling back to the search",
                path.display()
            ),
        }
    }
    for candidate in CANDIDATES {
        let path = windows_fonts_path(candidate);
        if let Some(found) = load(&path, 0) {
            return Some(found);
        }
    }
    let found = FONT_DIRS
        .iter()
        .find_map(|dir| scan(Path::new(dir), 3).and_then(|path| load(&path, 0)));
    if found.is_none() {
        eprintln!(
            "[gui] no CJK font found: Chinese text will draw as boxes. \
             Set AU3_GUI_FONT to a .ttf/.ttc to fix that."
        );
    }
    found
}

/// `SystemRoot` is where Windows actually lives; the `C:\Windows` in
/// [`CANDIDATES`] is only the usual spelling.
fn windows_fonts_path(candidate: &str) -> PathBuf {
    match (
        candidate.strip_prefix(r"C:\Windows"),
        std::env::var_os("SystemRoot"),
    ) {
        (Some(rest), Some(root)) => Path::new(&root).join(rest.trim_start_matches('\\')),
        _ => PathBuf::from(candidate),
    }
}

fn load(path: &Path, index: u32) -> Option<Fallback> {
    let bytes = std::fs::read(path).ok()?;
    if !looks_like_a_font(&bytes) {
        return None;
    }
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    Some(Fallback {
        path: path.to_path_buf(),
        data,
    })
}

/// The sfnt magic numbers: TrueType, OpenType/CFF, `true`, and a collection.
fn looks_like_a_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0x00, 0x01, 0x00, 0x00])
            | Some(b"OTTO")
            | Some(b"true")
            | Some(b"ttcf")
            | Some(b"wOFF")
    )
}

/// A shallow walk for a font whose name looks like it covers CJK.
fn scan(dir: &Path, depth: usize) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirectories = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                subdirectories.push(path);
            }
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if HINTS.iter().any(|hint| name.contains(hint)) {
            return Some(path);
        }
    }
    subdirectories
        .into_iter()
        .find_map(|dir| scan(&dir, depth - 1))
}

/// Add a CJK-capable font as the last fallback of both font families.
///
/// Returns the file it used, or `None` when this machine has none — the caller
/// then keeps egui's fonts and Chinese text draws as boxes.
pub fn install_cjk_font(ctx: &egui::Context) -> Option<&'static Path> {
    let found = fallback()?;
    ctx.add_font(FontInsert::new(
        "autoitv3-cjk",
        found.data.clone(),
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
    Some(found.path.as_path())
}

/// Whether `ctx` can draw `text` with the font sizes the GUI layer uses.
///
/// egui builds its fonts on the first `Context::run`, so this panics on a
/// context that has never drawn a frame.
pub fn has_glyphs(ctx: &egui::Context, text: &str) -> bool {
    let font = FontId::proportional(14.0);
    ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, text))
}
