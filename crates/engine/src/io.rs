//! File format dispatch by extension. Format crates plug in here.

use wordcraft_doc::Document;

/// Formats WordCraft opens.
pub const OPEN_EXTS: &[&str] = &["docx", "docm", "dotx", "txt", "md", "markdown", "html", "htm", "rtf", "odt", "wcraft.json", "json"];
/// Formats WordCraft saves (Save As).
pub const SAVE_EXTS: &[&str] = &["docx", "pdf", "txt", "md", "html", "rtf", "odt", "png", "json"];

fn ext_of(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.rsplit('.').next().unwrap_or("").to_string()
}

/// Parse a document from bytes; `name` gives the format by extension.
pub fn open_bytes(name: &str, bytes: &[u8]) -> Result<Document, String> {
    let ext = ext_of(name);
    let mut doc = match ext.as_str() {
        "txt" | "text" | "" => Document::from_text(&decode_text(bytes)),
        "json" => serde_json::from_slice::<Document>(bytes).map_err(|e| format!("{name}: {e}"))?,
        other => match crate::io_ext::open(other, bytes) {
            Some(r) => r?,
            None => return Err(format!("{name}: unsupported format `.{other}`")),
        },
    };
    doc.ensure_nonempty();
    Ok(doc)
}

// embed: a caller with less memory than the desktop (the embed build in a browser tab) opens Word
// packages within tighter limits, and tells a refusal for size apart from a broken file.
pub use wordcraft_docx::{DocxError, ReadLimits};

/// A Word package (`.docx`, `.docm`, `.dotx`) read within `limits`, as [`open_bytes`] reads one.
pub fn open_docx_with_limits(bytes: &[u8], limits: &ReadLimits) -> Result<Document, DocxError> {
    let mut doc = wordcraft_docx::read_with_limits(bytes, limits)?;
    doc.ensure_nonempty();
    Ok(doc)
}

/// Serialise a document; `name` gives the format by extension.
pub fn save_bytes(name: &str, doc: &Document) -> Result<Vec<u8>, String> {
    let ext = ext_of(name);
    match ext.as_str() {
        "txt" | "text" => Ok(doc.plain_text(wordcraft_doc::StoryRef::Body).replace('\n', "\r\n").into_bytes()),
        "json" => serde_json::to_vec_pretty(doc).map_err(|e| e.to_string()),
        other => match crate::io_ext::save(other, doc) {
            Some(r) => r,
            None => Err(format!("{name}: can't save as `.{other}`")),
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open_path(path: &std::path::Path) -> Result<Document, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > 2 << 30 {
        return Err(format!("{}: file is larger than 2 GB", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    open_bytes(&path.to_string_lossy(), &bytes)
}

#[cfg(target_arch = "wasm32")]
pub fn open_path(path: &std::path::Path) -> Result<Document, String> {
    Err(format!("{}: files are opened through the browser on the web", path.display()))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_path(path: &std::path::Path, doc: &Document) -> Result<(), String> {
    let bytes = save_bytes(&path.to_string_lossy(), doc)?;
    // Write atomically: temp file next to the target, then rename.
    let tmp = path.with_extension(format!("{}.tmp", ext_of(&path.to_string_lossy())));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(target_arch = "wasm32")]
pub fn save_path(path: &std::path::Path, _doc: &Document) -> Result<(), String> {
    Err(format!("{}: files are saved through the browser on the web", path.display()))
}

/// UTF-8 (with or without BOM), UTF-16 with BOM, else Latin-1.
pub fn decode_text(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if b.len() >= 2 && (b[0] == 0xFF && b[1] == 0xFE || b[0] == 0xFE && b[1] == 0xFF) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> =
            b[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|c| *c as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_package_opens_within_the_callers_limits() {
        let bytes = save_bytes("x.docx", &Document::from_text("Hello\nWorld")).expect("write docx");
        let opened = open_docx_with_limits(&bytes, &ReadLimits::default()).expect("opens with the defaults");
        assert_eq!(
            opened.plain_text(wordcraft_doc::StoryRef::Body),
            open_bytes("x.docx", &bytes).expect("opens").plain_text(wordcraft_doc::StoryRef::Body)
        );
        let tight = ReadLimits { max_elements: 3, ..ReadLimits::default() };
        assert!(matches!(open_docx_with_limits(&bytes, &tight), Err(DocxError::Limit(_))));
        assert!(matches!(open_docx_with_limits(b"not a zip", &ReadLimits::default()), Err(DocxError::Zip(_))));
    }
}
