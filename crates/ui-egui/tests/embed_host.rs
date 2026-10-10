//! The embed build's host bridge: commands from a host page, events back.

mod common;

use common::*;
use wordcraft_ui_egui::host::{HostCommand, HostEvent};

#[test]
fn opening_a_word_document_answers_opened_and_page_one() {
    let (mut h, bridge) = embed_app();
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let count = pages(&mut h);
    assert!(count >= 3, "fixture too short: {count} pages");
    assert_eq!(events.first(), Some(&HostEvent::Opened { pages: count as u32 }), "{events:?}");
    assert_eq!(events, vec![HostEvent::Opened { pages: count as u32 }, HostEvent::PageChanged { page: 1 }]);
    assert!(h.state().session.view_only, "an opened document stays view-only");
    assert!(!h.state().session.dirty);
}

#[test]
fn files_that_cannot_be_shown_answer_open_failed_and_keep_the_open_document() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let shown = doc_json(&h);
    let good = docx(&clauses(2));
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("empty.docx", Vec::new(), "empty"),
        ("locked.docx", vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0], "password-protected"),
        ("truncated.docx", good[..good.len() / 2].to_vec(), "damaged"),
        ("noise.docx", b"PK\x03\x04 not really a zip archive".to_vec(), "damaged"),
        ("notes.txt", b"plain text".to_vec(), "Only Word documents"),
        ("contract.pdf", b"%PDF-1.7".to_vec(), "Only Word documents"),
    ];
    for (name, bytes, says) in cases {
        bridge.send(HostCommand::Open { name: name.into(), bytes });
        frames(&mut h, 2);
        let events = bridge.take_events();
        assert!(matches!(events.as_slice(), [HostEvent::OpenFailed { message }] if message.contains(says)), "{name}: {events:?}");
        assert_eq!(doc_json(&h), shown, "{name} replaced the open document");
    }
}

#[test]
fn scrolling_to_another_page_reports_it_once() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    h.state_mut().canvas.scroll_to_doc = Some((2, 0.0));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 3 }]);
    assert_eq!(h.state().canvas.visible_page, Some(2));
    frames(&mut h, 3);
    assert!(bridge.take_events().is_empty(), "pageChanged repeated without a change");
}

#[test]
fn a_second_open_replaces_the_document_and_reports_page_one_again() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    h.state_mut().canvas.scroll_to_doc = Some((2, 0.0));
    frames(&mut h, 4);
    bridge.take_events();
    // Several pages, so the previous document's scroll position still lands on a page of this one.
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let count = pages(&mut h) as u32;
    assert!(count >= 3, "fixture too short: {count} pages");
    assert_eq!(events, vec![HostEvent::Opened { pages: count }, HostEvent::PageChanged { page: 1 }]);
    assert_eq!(h.state().canvas.visible_page, Some(0));
    assert!(h.state().session.view_only);
}

#[test]
fn a_newer_jump_replaces_a_waiting_one() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    h.state_mut().canvas.scroll_to_doc = Some((2, 0.0));
    h.state_mut().canvas.scroll_to_doc = Some((1, 0.0));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 2 }]);
}

#[test]
fn a_jump_past_the_last_page_or_with_a_wild_offset_moves_nothing_and_never_panics() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    for target in [(usize::MAX, 0.0), (0, f32::NAN), (0, f32::INFINITY), (0, -1e30), (1, f32::MAX)] {
        h.state_mut().canvas.scroll_to_doc = Some(target);
        frames(&mut h, 3);
        assert!(h.state().canvas.scroll_to_doc.is_none(), "{target:?} stayed queued");
    }
    // Only real pages are ever reported.
    let count = pages(&mut h) as u32;
    for event in bridge.take_events() {
        assert!(matches!(event, HostEvent::PageChanged { page } if (1..=count).contains(&page)), "{event:?}");
    }
}

// --- Malformed input: every class answers `openFailed` and leaves the open document showing. ---

/// CRC-32 (IEEE), table-driven so the large fixtures below stay quick; real CRCs mean a refusal
/// can only come from the limit being tested, never from a checksum.
fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
            *slot = c;
        }
        table
    });
    let mut crc = !crc;
    for byte in data {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

struct Entry {
    name: &'static str,
    method: u16,
    data: Vec<u8>,
    crc: u32,
    declared_size: u32,
}

impl Entry {
    fn stored(name: &'static str, data: &[u8]) -> Entry {
        Entry { name, method: 0, data: data.to_vec(), crc: crc32(data), declared_size: data.len() as u32 }
    }

    /// `prefix`, then `len` copies of `fill`, then `suffix`, deflated to about `len / 160` bytes,
    /// with its real CRC; `declared` is the size its headers claim.
    fn bomb(name: &'static str, prefix: &[u8], fill: u8, len: usize, suffix: &[u8], declared: u32) -> Entry {
        let mut crc = crc32_update(0, prefix);
        let chunk = vec![fill; 1 << 20];
        let mut left = len;
        while left > 0 {
            let take = left.min(chunk.len());
            crc = crc32_update(crc, &chunk[..take]);
            left -= take;
        }
        crc = crc32_update(crc, suffix);
        Entry { name, method: 8, data: deflate_fill(prefix, fill, len, suffix), crc, declared_size: declared }
    }

    fn real_size(&self) -> usize {
        self.declared_size as usize
    }
}

/// A zip archive written by hand, so its headers can say what a hostile file would say.
fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let fields = |out: &mut Vec<u8>| {
            out.extend(20u16.to_le_bytes());
            out.extend(0u16.to_le_bytes());
            out.extend(entry.method.to_le_bytes());
            out.extend(0u32.to_le_bytes());
            out.extend(entry.crc.to_le_bytes());
            out.extend((entry.data.len() as u32).to_le_bytes());
            out.extend(entry.declared_size.to_le_bytes());
            out.extend((entry.name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes());
        };
        out.extend(0x0403_4b50u32.to_le_bytes());
        fields(&mut out);
        out.extend(entry.name.as_bytes());
        out.extend(&entry.data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        fields(&mut central);
        // Comment length, start disk, internal attributes; then external attributes.
        central.extend([0u8; 6]);
        central.extend(0u32.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(entry.name.as_bytes());
    }
    let central_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(central_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// A raw deflate stream (one fixed-Huffman block): `prefix` as literals, `len` bytes of `fill` as
/// one literal and copies of 258 bytes at distance 1, then `suffix` as literals. Every byte must be
/// below 144, which covers ASCII.
fn deflate_fill(prefix: &[u8], fill: u8, len: usize, suffix: &[u8]) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, value: u32, count: u32) {
            for i in 0..count {
                self.acc |= ((value >> i) & 1) << self.n;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        /// Huffman codes go most significant bit first.
        fn code(&mut self, code: u32, count: u32) {
            for i in (0..count).rev() {
                self.put((code >> i) & 1, 1);
            }
        }
        fn literal(&mut self, byte: u8) {
            assert!(byte < 144, "8-bit literal codes only");
            self.code(0x30 + u32::from(byte), 8);
        }
    }
    let mut bits = Bits { out: Vec::new(), acc: 0, n: 0 };
    bits.put(1, 1); // final block
    bits.put(1, 2); // fixed Huffman codes
    prefix.iter().for_each(|b| bits.literal(*b));
    if len > 0 {
        bits.literal(fill);
        let copies = (len - 1) / 258;
        for _ in 0..copies {
            bits.code(0xC5, 8); // length 258 (code 285)
            bits.code(0, 5); // distance 1
        }
        for _ in 0..(len - 1) % 258 {
            bits.literal(fill);
        }
    }
    suffix.iter().for_each(|b| bits.literal(*b));
    bits.code(0, 7); // end of block
    bits.put(0, 7);
    bits.out
}

fn content_types() -> Entry {
    Entry::stored("[Content_Types].xml", br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#)
}

const DOCUMENT_START: &[u8] =
    br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#;
const DOCUMENT_END: &[u8] = b"</w:body></w:document>";

/// A valid document whose `document.xml` is padded with `spaces` spaces between paragraphs, so
/// without a size limit it would open; `declared` is what its headers claim.
fn padded_document(spaces: usize, declared: u32) -> Vec<u8> {
    zip(&[content_types(), Entry::bomb("word/document.xml", DOCUMENT_START, b' ', spaces, DOCUMENT_END, declared)])
}

/// A valid document whose body holds `elements` empty unknown elements, which the reader parses
/// and then ignores: the package holds `elements + 2` elements in all.
fn document_with_elements(elements: usize) -> Vec<u8> {
    let mut xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#.to_vec();
    xml.extend(b"<w:body>");
    xml.extend(b"<w:x/>".repeat(elements));
    xml.extend(b"</w:body></w:document>");
    zip(&[content_types(), Entry::stored("word/document.xml", &xml)])
}

/// One file per class of input the embed build must refuse without crashing.
fn malformed_files() -> Vec<(String, Vec<u8>, &'static str)> {
    use wordcraft_ui_egui::host::open::{DAMAGED, EMBED_LIMITS, EMPTY, ENCRYPTED, MAX_OPEN_BYTES, TOO_LARGE, UNSUPPORTED};
    let good = docx(&clauses(2));
    let mut legacy_word = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    legacy_word.extend([0u8; 504]);
    legacy_word.extend("WordDocument".encode_utf16().flat_map(u16::to_le_bytes));
    let mut encrypted = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    encrypted.extend([0u8; 504]);
    encrypted.extend("EncryptedPackage".encode_utf16().flat_map(u16::to_le_bytes));
    let mut too_large = vec![0u8; MAX_OPEN_BYTES + 1];
    too_large[..2].copy_from_slice(b"PK");
    // Past the embed build's per-part limit, honest about it or not.
    let spaces = EMBED_LIMITS.max_part_bytes as usize + 16 * 1024 * 1024;
    let honest = (spaces + DOCUMENT_START.len() + DOCUMENT_END.len()) as u32;
    vec![
        ("empty.docx".into(), Vec::new(), EMPTY),
        ("one-byte.docx".into(), b"P".to_vec(), DAMAGED),
        ("noise.docx".into(), b"PK\x03\x04 not really a zip archive".to_vec(), DAMAGED),
        ("truncated.docx".into(), good[..good.len() / 2].to_vec(), DAMAGED),
        ("headless.docx".into(), good[..good.len() - 10].to_vec(), DAMAGED),
        ("encrypted.docx".into(), encrypted, ENCRYPTED),
        ("renamed-legacy.docx".into(), legacy_word.clone(), UNSUPPORTED),
        ("legacy.doc".into(), legacy_word, UNSUPPORTED),
        ("too-large.docx".into(), too_large, TOO_LARGE),
        ("\u{FFFD}\u{FFFD}.\u{FFFD}".into(), good.clone(), UNSUPPORTED),
        ("no-word-part.docx".into(), zip(&[content_types()]), DAMAGED),
        ("not-xml.docx".into(), zip(&[content_types(), Entry::stored("word/document.xml", b"<w:document><w:body><w:p>")]), DAMAGED),
        ("other-xml.docx".into(), zip(&[content_types(), Entry::stored("word/document.xml", b"<html/>")]), DAMAGED),
        ("huge-part.docx".into(), padded_document(spaces, honest), TOO_LARGE),
        ("lying-bomb.docx".into(), padded_document(spaces, 1024), TOO_LARGE),
    ]
}

#[test]
fn every_malformed_file_answers_open_failed_without_a_crash() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let shown = doc_json(&h);
    for (name, bytes, message) in malformed_files() {
        bridge.send(HostCommand::Open { name: name.clone(), bytes });
        frames(&mut h, 2);
        assert_eq!(bridge.take_events(), vec![HostEvent::OpenFailed { message: message.into() }], "{name}");
        assert_eq!(doc_json(&h), shown, "{name} replaced the open document");
        assert!(h.state().session.view_only && !h.state().session.dirty, "{name}");
    }
    // The editor still opens a good file afterwards.
    assert!(matches!(open_doc(&mut h, &bridge, &clauses(3)).first(), Some(HostEvent::Opened { pages: 1 })));
}

#[test]
fn the_hand_written_zip_is_a_real_package() {
    // Guards the fixtures above: a refusal must come from the hostile part, not a broken writer.
    let xml = br#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
    let doc = wordcraft_ui_egui::host::open::open_document("hand.docx", &zip(&[content_types(), Entry::stored("word/document.xml", xml)]));
    assert_eq!(doc.map(|d| d.plain_text(wordcraft_doc::StoryRef::Body)), Ok("Hello".to_string()));
}

#[test]
fn a_jump_offset_stays_within_its_page() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // Past the bottom of the second page: the view's top lands at that page's bottom edge, so the
    // page filling the view is the third, never one further down.
    for offset in [f32::MAX, 1e30, 1e9] {
        h.state_mut().canvas.scroll_to_doc = Some((0, 0.0));
        frames(&mut h, 3);
        h.state_mut().canvas.scroll_to_doc = Some((1, offset));
        frames(&mut h, 3);
        assert_eq!(h.state().canvas.visible_page, Some(2), "offset {offset}");
    }
    // Above the top of a page is its top.
    h.state_mut().canvas.scroll_to_doc = Some((1, -1e30));
    frames(&mut h, 3);
    assert_eq!(h.state().canvas.visible_page, Some(1));
    bridge.take_events();
}

#[test]
fn a_host_jump_wins_over_a_caret_scroll_from_the_same_frame() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // The caret is on page one; both scrolls are asked for in one frame.
    h.state_mut().canvas.scroll_to_caret = true;
    h.state_mut().canvas.scroll_to_doc = Some((4, 0.0));
    frames(&mut h, 6);
    assert_eq!(h.state().canvas.visible_page, Some(4));
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 5 }]);
}

// --- The embed build's decompression and parse limits ---

#[test]
fn a_part_just_over_the_embed_limit_is_too_large_and_one_at_it_opens() {
    use wordcraft_ui_egui::host::open::{EMBED_LIMITS, TOO_LARGE, open_document};
    let room = EMBED_LIMITS.max_part_bytes as usize - DOCUMENT_START.len() - DOCUMENT_END.len();
    for declared in [None, Some(1024)] {
        let fits = padded_document(room, declared.unwrap_or((room + DOCUMENT_START.len() + DOCUMENT_END.len()) as u32));
        let over = padded_document(room + 1, declared.unwrap_or((room + 1 + DOCUMENT_START.len() + DOCUMENT_END.len()) as u32));
        assert_eq!(open_document("big.docx", &over).err().as_deref(), Some(TOO_LARGE), "declared {declared:?}");
        // A lying size is caught by the decompression cap; an honest one at the limit is read.
        if declared.is_none() {
            let doc = open_document("big.docx", &fits).expect("a part at the limit opens");
            assert!(doc.plain_text(wordcraft_doc::StoryRef::Body).starts_with("Hello"));
        }
    }
}

#[test]
fn parts_just_over_the_embed_total_are_too_large() {
    use wordcraft_ui_egui::host::open::{EMBED_LIMITS, TOO_LARGE, open_document};
    let part = EMBED_LIMITS.max_part_bytes as usize;
    let parts = (EMBED_LIMITS.max_total_bytes / EMBED_LIMITS.max_part_bytes) as usize;
    let filler = |name: &'static str, len: usize| Entry::bomb(name, b"", b'0', len, b"", len as u32);
    let package = |last: usize| {
        let mut entries = vec![content_types(), Entry::stored("word/document.xml", &[DOCUMENT_START, DOCUMENT_END].concat())];
        let used: usize = entries.iter().map(Entry::real_size).sum();
        let names = ["word/media/a.bin", "word/media/b.bin", "word/media/c.bin", "word/media/d.bin"];
        for name in names.iter().take(parts - 1) {
            entries.push(filler(name, part));
        }
        // The last part brings the total to exactly the limit, plus `last`.
        let total_before = used + part * (parts - 1);
        entries.push(filler(names[parts - 1], EMBED_LIMITS.max_total_bytes as usize - total_before + last));
        zip(&entries)
    };
    assert_eq!(open_document("many.docx", &package(1)).err().as_deref(), Some(TOO_LARGE));
    assert!(open_document("many.docx", &package(0)).is_ok(), "exactly the total opens");
}

#[test]
fn elements_just_over_the_embed_budget_are_too_large_and_at_it_open() {
    use wordcraft_ui_egui::host::open::{EMBED_LIMITS, TOO_LARGE, open_document};
    let budget = EMBED_LIMITS.max_elements;
    assert_eq!(open_document("dense.docx", &document_with_elements(budget - 2 + 1)).err().as_deref(), Some(TOO_LARGE));
    assert!(open_document("dense.docx", &document_with_elements(budget - 2)).is_ok());
}

#[test]
fn a_normal_document_opens_within_the_embed_limits() {
    use wordcraft_ui_egui::host::open::open_document;
    let doc = open_document("agreement.docx", &docx(&clauses(400))).expect("opens");
    assert!(doc.plain_text(wordcraft_doc::StoryRef::Body).contains("Clause 400."));
}
