//! Opening a host page's file: Word documents only, with a plain message for anything that cannot
//! be shown. The editor never asks for a password in view mode.
//!
//! Nothing here may panic: a panic aborts the WebAssembly module (embed API, "Crashes"). The cheap
//! checks run before any parse, and the DOCX reader bounds what it decompresses and parses.

use wordcraft_doc::Document;
use wordcraft_engine::io::{DocxError, ReadLimits};

use super::bridge::HostCommand;

/// Largest file a host may open.
pub const MAX_OPEN_BYTES: usize = 100 * 1024 * 1024;

/// How much of a Word package the embed build decompresses and parses. A failed allocation aborts a
/// WebAssembly module, and its memory never shrinks, so these sit well below the desktop's limits
/// (256 MiB a part, 512 MiB in all, 8 million elements a part). Reading peaks at about 190 to 630
/// bytes per XML element (measured: empty-paragraph floods cost the most), so 2 million elements
/// stay near 1.3 GB at worst, leaving the rest of the 4 GiB address space for layout and pages. A
/// formatted page holds roughly a thousand elements, so real documents of a couple of thousand
/// pages fit. Pictures make up most of a package's bytes: 64 MiB for one is generous, and 192 MiB
/// in all leaves room beside the up-to-100 MB file itself. The file is held twice: the host's copy
/// while it opens, and the copy the document keeps for its lifetime so a save can keep the package
/// (made after the decompressed parts are freed). The archive is also capped at 192 MiB on its
/// own, which `MAX_OPEN_BYTES` already keeps it under.
///
/// Elements nest at most 64 deep (the desktop allows 256). The reader recurses once per level, and a
/// stack overflow traps the module just as a panic does. Measured in Chromium on WebAssembly's
/// 1 MiB stack, the deepest reading path (block-level `w:customXml` or `w:ins`, nested) peaks at
/// about 2 KiB a level with the frame around it: 132 KiB at 60 levels, 503 KiB at 250. So 64 levels
/// stay near 140 KiB. Real documents nest far less: text in a shape's text box inside a table cell
/// inside a content control is 21 levels, and each further nested table adds 3.
pub const EMBED_LIMITS: ReadLimits = ReadLimits { max_part_bytes: 64 << 20, max_total_bytes: 192 << 20, max_elements: 2_000_000, max_depth: 64 };
const WORD_EXTENSIONS: &[&str] = &["docx", "docm", "dotx"];
/// OLE compound files: password-protected Word packages (and legacy binary files) start with this.
const COMPOUND_FILE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

// The host shows these as they are, so they read like pdfcraft's: plain words, one sentence.
pub const UNSUPPORTED: &str = "Only Word documents (.docx, .docm, .dotx) can be shown here.";
pub const EMPTY: &str = "This file is empty and can't be shown.";
/// For a file over `MAX_OPEN_BYTES` and for one whose contents exceed `EMBED_LIMITS` alike.
pub const TOO_LARGE: &str = "This file is too large or complex to be shown here.";
pub const ENCRYPTED: &str = "This file is password-protected and can't be shown here.";
pub const DAMAGED: &str = "This file is damaged or isn't a Word document and can't be shown.";

/// Why a file named `name` of `len` bytes starting with `head` cannot be opened, before parsing it.
pub fn precheck(name: &str, len: usize, head: &[u8]) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !WORD_EXTENSIONS.contains(&ext.as_str()) {
        return Some(UNSUPPORTED);
    }
    if len == 0 {
        return Some(EMPTY);
    }
    if len > MAX_OPEN_BYTES {
        return Some(TOO_LARGE);
    }
    if head.starts_with(&COMPOUND_FILE) {
        return Some(ENCRYPTED);
    }
    if !head.starts_with(b"PK") {
        return Some(DAMAGED);
    }
    None
}

/// The command that opens a host's file of `len` bytes, where `copy(len)` copies the whole file
/// into this module's memory. The size is judged first: a file over [`MAX_OPEN_BYTES`] is
/// [`TOO_LARGE`] whatever it holds, so none of it is copied (a whole copy could take more memory
/// than the module can get; a failed allocation aborts it, and its memory never shrinks). An empty
/// file, which is also what a detached buffer reads as, is never touched; it opens as empty and is
/// refused in order like any other.
#[doc(hidden)]
pub fn open_command<E>(name: String, len: usize, copy: impl FnOnce(usize) -> Result<Vec<u8>, E>) -> Result<HostCommand, E> {
    if len > MAX_OPEN_BYTES {
        return Ok(HostCommand::OpenTooLarge { name, len });
    }
    let bytes = if len == 0 { Vec::new() } else { copy(len)? };
    Ok(HostCommand::Open { name, bytes })
}

/// Parse a host's file, or answer with a message the host can show as is.
pub fn open_document(name: &str, bytes: &[u8]) -> Result<Document, String> {
    match precheck(name, bytes.len(), bytes.get(..COMPOUND_FILE.len()).unwrap_or(bytes)) {
        Some(ENCRYPTED) => return Err(compound_file_problem(bytes).to_string()),
        Some(problem) => return Err(problem.to_string()),
        None => {}
    }
    // The parser's own error names zip entries and XML details, which mean nothing to the reader;
    // it goes to the log for whoever investigates.
    wordcraft_engine::io::open_docx_with_limits(bytes, &EMBED_LIMITS).map_err(|e| {
        log::warn!("embed: a host file could not be opened: {e}");
        match e {
            DocxError::Limit(_) => TOO_LARGE,
            _ => DAMAGED,
        }
        .to_string()
    })
}

/// An OLE compound file is either a password-protected Word package (an `EncryptedPackage` stream)
/// or a legacy binary file such as a `.doc` renamed to `.docx` (a `WordDocument` stream). Stream
/// names are stored in UTF-16LE in the file's directory, which may sit anywhere in the file.
fn compound_file_problem(bytes: &[u8]) -> &'static str {
    let contains = |name: &str| {
        let needle: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        bytes.windows(needle.len()).any(|w| w == needle.as_slice())
    };
    if !contains("EncryptedPackage") && contains("WordDocument") { UNSUPPORTED } else { ENCRYPTED }
}

#[cfg(test)]
mod tests {
    use wordcraft_doc::StoryRef;

    use super::*;

    fn utf16(name: &str) -> Vec<u8> {
        name.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn word_extensions_only_in_any_case() {
        assert_eq!(precheck("A.DOCX", 10, b"PK\x03\x04"), None);
        assert_eq!(precheck("a.dotx", 10, b"PK"), None);
        assert_eq!(precheck("a.DocM", 10, b"PK"), None);
        for name in ["a.doc", "a.pdf", "a.txt", "a.json", "a.md", "docx", "", ".", "a.docx.", "a.docx\u{FFFD}", "\u{FFFD}.\u{FFFD}"] {
            assert_eq!(precheck(name, 10, b"PK"), Some(UNSUPPORTED), "{name}");
        }
        // Replacement characters stand in for a name that was not valid Unicode.
        assert_eq!(precheck("\u{FFFD}\u{1F4C4}.docx", 10, b"PK"), None);
    }

    #[test]
    fn size_limits() {
        assert_eq!(precheck("a.docx", 0, b""), Some(EMPTY));
        assert_eq!(precheck("a.docx", MAX_OPEN_BYTES, b"PK"), None);
        assert_eq!(precheck("a.docx", MAX_OPEN_BYTES + 1, b"PK"), Some(TOO_LARGE));
        assert_eq!(precheck("a.docx", usize::MAX, b"PK"), Some(TOO_LARGE));
    }

    #[test]
    fn short_or_foreign_heads_are_damaged() {
        for head in [&b"P"[..], b"\0", b"%PDF-1.7", b"{\\rtf1", &COMPOUND_FILE[..7]] {
            assert_eq!(precheck("a.docx", 10, head), Some(DAMAGED), "{head:?}");
        }
    }

    #[test]
    fn compound_files_are_reported_as_encrypted() {
        assert_eq!(precheck("a.docx", 4096, &COMPOUND_FILE), Some(ENCRYPTED));
        assert_eq!(open_document("a.docx", &COMPOUND_FILE).unwrap_err(), ENCRYPTED);
    }

    #[test]
    fn a_legacy_word_file_renamed_to_docx_is_not_called_password_protected() {
        let mut legacy = COMPOUND_FILE.to_vec();
        legacy.extend(vec![0; 500]);
        legacy.extend(utf16("WordDocument"));
        assert_eq!(open_document("old.docx", &legacy).unwrap_err(), UNSUPPORTED);

        let mut encrypted = COMPOUND_FILE.to_vec();
        encrypted.extend(utf16("EncryptionInfo"));
        encrypted.extend(utf16("EncryptedPackage"));
        encrypted.extend(utf16("WordDocument"));
        assert_eq!(open_document("locked.docx", &encrypted).unwrap_err(), ENCRYPTED);
        assert_eq!(open_document("old.doc", &legacy).unwrap_err(), UNSUPPORTED);
    }

    #[test]
    fn messages_are_one_plain_sentence() {
        for message in [UNSUPPORTED, EMPTY, TOO_LARGE, ENCRYPTED, DAMAGED] {
            assert!(message.ends_with('.') && message.matches(". ").count() == 0, "{message}");
            assert!(!message.contains("zip") && !message.contains("XML") && !message.contains('/'), "{message}");
        }
    }

    #[test]
    fn a_real_docx_opens() {
        let bytes = wordcraft_engine::io::save_bytes("x.docx", &Document::from_text("Hello")).unwrap();
        assert_eq!(open_document("x.docx", &bytes).unwrap().plain_text(StoryRef::Body), "Hello");
    }

    #[test]
    fn a_truncated_docx_is_damaged_at_every_length() {
        let bytes = wordcraft_engine::io::save_bytes("x.docx", &Document::from_text("Hello")).unwrap();
        // Every cut that removes the zip's central directory leaves no readable package.
        for len in (1..bytes.len()).step_by(7) {
            if let Err(message) = open_document("x.docx", bytes.get(..len).unwrap_or_default()) {
                assert!([DAMAGED, EMPTY].contains(&message.as_str()), "{len}: {message}");
            }
        }
    }

    #[test]
    fn an_oversized_file_is_too_large_and_never_copied() {
        let mut copied = false;
        for (name, len) in [("big.docx", MAX_OPEN_BYTES + 1), ("locked.docx", MAX_OPEN_BYTES + 1), ("big.pdf", usize::MAX)] {
            let command = open_command(name.into(), len, |_| {
                copied = true;
                Ok::<_, ()>(Vec::new())
            });
            assert_eq!(command, Ok(HostCommand::OpenTooLarge { name: name.into(), len }));
        }
        assert!(!copied, "nothing of an oversized file is copied");
    }

    #[test]
    fn a_file_within_the_cap_is_copied_whole_and_an_empty_one_not_at_all() {
        let mut asked = Vec::new();
        let mut copy = |n: usize| {
            asked.push(n);
            Ok::<_, ()>(vec![b'P'; n.min(4)])
        };
        assert_eq!(open_command("a.docx".into(), 3, &mut copy), Ok(HostCommand::Open { name: "a.docx".into(), bytes: b"PPP".to_vec() }));
        assert!(matches!(open_command("a.docx".into(), MAX_OPEN_BYTES, &mut copy), Ok(HostCommand::Open { .. })));
        assert_eq!(open_command("a.docx".into(), 0, &mut copy), Ok(HostCommand::Open { name: "a.docx".into(), bytes: Vec::new() }));
        assert_eq!(asked, [3, MAX_OPEN_BYTES], "an empty file is never read");
    }

    #[test]
    fn a_failed_copy_queues_nothing() {
        assert_eq!(open_command("a.docx".into(), 10, |_| Err("detached")), Err("detached"));
    }
}
