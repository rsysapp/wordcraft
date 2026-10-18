//! The embed build's host bridge: commands from a host page, events back.

mod common;

use common::*;
use wordcraft_doc::{Document, Pos};
use wordcraft_engine::Session;
use wordcraft_ui_egui::host::text_match::MAX_NEEDLE_CHARS;
use wordcraft_ui_egui::host::{EmbedProfile, HostBridge, HostCommand, HostEvent, JumpTarget};
use wordcraft_ui_egui::{Services, WordApp};

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

/// The embed API sends no `pageChanged` before the first `opened`: the empty document the editor
/// starts with is not one the host opened.
#[test]
fn no_page_changed_comes_before_the_first_opened() {
    // As right after mount: no frame drained, nothing opened. A page jump would land on the empty
    // document's one page.
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = harness(app, 1.0);
    bridge.set_context(h.ctx.clone());
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 3);
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }));
    frames(&mut h, 3);
    let before = bridge.take_events();
    assert!(!before.iter().any(|e| matches!(e, HostEvent::PageChanged { .. })), "{before:?}");
    // A failed open is not an open either.
    bridge.send(HostCommand::Open { name: "notes.txt".into(), bytes: b"text".to_vec(), options: Default::default() });
    frames(&mut h, 3);
    let failed = bridge.take_events();
    assert!(matches!(failed.as_slice(), [HostEvent::OpenFailed { .. }]), "{failed:?}");
    // The first open: `opened` first, then its page.
    let events = open_doc(&mut h, &bridge, &clauses(120));
    let count = pages(&mut h) as u32;
    assert_eq!(events, vec![HostEvent::Opened { pages: count }, HostEvent::PageChanged { page: 1 }]);
    // And after it, page changes are reported as before.
    h.state_mut().canvas.scroll_to_doc = Some((2, 0.0));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 3 }]);
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 }));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 2 }]);
    // A second open: its `opened` comes before its page, which is reported again even though the
    // previous document was on another one.
    let events = open_doc(&mut h, &bridge, &clauses(120));
    assert_eq!(events, vec![HostEvent::Opened { pages: count }, HostEvent::PageChanged { page: 1 }]);
}

/// The same for a one-page document, whose page 1 is also the empty starting document's only page.
#[test]
fn a_one_page_document_reports_its_page_only_after_opened() {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = harness(app, 1.0);
    bridge.set_context(h.ctx.clone());
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 3);
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }));
    frames(&mut h, 3);
    assert!(!bridge.take_events().iter().any(|e| matches!(e, HostEvent::PageChanged { .. })));
    let events = open_doc(&mut h, &bridge, &clauses(1));
    assert_eq!(pages(&mut h), 1);
    assert_eq!(events, vec![HostEvent::Opened { pages: 1 }, HostEvent::PageChanged { page: 1 }]);
    // Opening it again reports its page again, after `opened`.
    let events = open_doc(&mut h, &bridge, &clauses(1));
    assert_eq!(events, vec![HostEvent::Opened { pages: 1 }, HostEvent::PageChanged { page: 1 }]);
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
        bridge.send(HostCommand::Open { name: name.into(), bytes, options: Default::default() });
        frames(&mut h, 2);
        let events = bridge.take_events();
        assert!(matches!(events.as_slice(), [HostEvent::OpenFailed { message }] if message.contains(says)), "{name}: {events:?}");
        assert_eq!(doc_json(&h), shown, "{name} replaced the open document");
    }
}

#[test]
fn an_open_over_the_cap_answers_too_large_in_order_without_copying_the_file() {
    use wordcraft_ui_egui::host::open::{self, MAX_OPEN_BYTES, open_command};
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(10));
    let shown = doc_json(&h);
    let mut copied = 0;
    let mut copy = |n: usize| {
        copied += n;
        Ok::<_, ()>(Vec::new())
    };
    bridge.send(HostCommand::Open { name: "empty.docx".into(), bytes: Vec::new(), options: Default::default() });
    // Too large whatever the name or contents would have said.
    for (name, len) in [("big.docx", MAX_OPEN_BYTES + 1), ("big.pdf", usize::MAX)] {
        let command = open_command(name.into(), len, Default::default(), &mut copy).expect("no copy to fail");
        assert_eq!(command, HostCommand::OpenTooLarge { name: name.into(), len });
        bridge.send(command);
    }
    bridge.send(open_command("detached.docx".into(), 0, Default::default(), &mut copy).expect("no copy to fail"));
    bridge.send(HostCommand::Open { name: "notes.txt".into(), bytes: b"text".to_vec(), options: Default::default() });
    assert_eq!(copied, 0, "nothing of an oversized or empty file was copied");
    frames(&mut h, 2);
    let messages: Vec<String> = bridge
        .take_events()
        .into_iter()
        .map(|event| match event {
            HostEvent::OpenFailed { message } => message,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(messages, [open::EMPTY, open::TOO_LARGE, open::TOO_LARGE, open::EMPTY, open::UNSUPPORTED], "each open answered once, in order");
    assert_eq!(doc_json(&h), shown, "a refused open keeps the document");
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
    // The view really was on page 3, so page 1 below is the open's doing.
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 3 }]);
    assert_eq!(h.state().canvas.visible_page, Some(2));
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
        bridge.send(HostCommand::Open { name: name.clone(), bytes, options: Default::default() });
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

#[test]
fn a_scroll_animation_already_running_does_not_carry_the_view_off_a_host_jump() {
    // At 60 frames a second, so egui's scroll animation spans several frames (the harness's default
    // step finishes it in one).
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = egui_kittest::Harness::builder().with_size(WINDOW).with_step_dt(1.0 / 60.0).with_max_steps(12).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    bridge.set_context(h.ctx.clone());
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 2);
    open_doc(&mut h, &bridge, &clauses(120));
    assert!(pages(&mut h) >= 6, "fixture too short");
    // The harness turns egui's scroll animation off; the browser has it on.
    h.ctx.all_styles_mut(|style| style.scroll_animation = egui::style::ScrollAnimation::default());
    // An animated scroll towards the caret near the end, as a find step starts one; a frame in, it
    // is still running when the host jumps.
    let far = Pos::body(119, 0);
    h.state_mut().session.sel = wordcraft_engine::Selection { anchor: far.clone(), focus: far };
    h.state_mut().canvas.scroll_to_caret = true;
    h.step();
    h.step();
    assert_eq!(h.state().canvas.visible_page, Some(0), "the animation should still be running");
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 }));
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(h.state().canvas.visible_page, Some(1), "the animation moved the view off the jump");
    let events = bridge.take_events();
    assert_eq!(events.last(), Some(&HostEvent::PageChanged { page: 2 }), "{events:?}");
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

/// A document whose paragraph sits inside `levels` nested block-level `w:customXml` elements, the
/// reader's deepest path per level: document, body, the levels, then paragraph, run and text.
fn document_nested(levels: usize) -> Vec<u8> {
    let mut xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#.to_vec();
    xml.extend(b"<w:customXml>".repeat(levels));
    xml.extend(b"<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    xml.extend(b"</w:customXml>".repeat(levels));
    xml.extend(b"</w:body></w:document>");
    zip(&[content_types(), Entry::stored("word/document.xml", &xml)])
}

#[test]
fn hostile_nesting_is_refused_as_too_complex_without_recursing() {
    use wordcraft_ui_egui::host::open::{EMBED_LIMITS, TOO_LARGE, open_document};
    // 10,000 levels: the parser stops at the limit, long before the reader could recurse that deep.
    assert_eq!(open_document("nested.docx", &document_nested(10_000)).err().as_deref(), Some(TOO_LARGE));
    // At the limit (document, body and the paragraph's three levels around the nesting) it opens.
    let at_limit = EMBED_LIMITS.max_depth - 5;
    let doc = open_document("nested.docx", &document_nested(at_limit)).expect("nesting at the limit opens");
    assert_eq!(doc.plain_text(wordcraft_doc::StoryRef::Body), "deep");
    assert_eq!(open_document("nested.docx", &document_nested(at_limit + 1)).err().as_deref(), Some(TOO_LARGE));
    // Through the host, on a thread with WebAssembly's 1 MiB of stack: a clean `openFailed`, and the
    // open document stays.
    let (events, kept) = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(|| {
            let (mut h, bridge) = embed_app();
            open_doc(&mut h, &bridge, &clauses(3));
            bridge.send(HostCommand::Open { name: "nested.docx".into(), bytes: document_nested(10_000), options: Default::default() });
            frames(&mut h, 3);
            (bridge.take_events(), h.state().session.doc.plain_text(wordcraft_doc::StoryRef::Body).starts_with("Clause 1."))
        })
        .expect("thread starts")
        .join()
        .expect("no stack overflow");
    assert_eq!(events, vec![HostEvent::OpenFailed { message: TOO_LARGE.to_string() }]);
    assert!(kept, "the open document stays");
}

#[test]
fn a_normal_document_opens_within_the_embed_limits() {
    use wordcraft_ui_egui::host::open::open_document;
    let doc = open_document("agreement.docx", &docx(&clauses(400))).expect("opens");
    assert!(doc.plain_text(wordcraft_doc::StoryRef::Body).contains("Clause 400."));
}

// --- jumpTo: pages and passages ---

fn text_target(text: &str, page: Option<u32>, occurrence: u32) -> JumpTarget {
    JumpTarget::Text { text: text.into(), page, occurrence }
}

fn page_target(h: &egui_kittest::Harness<'_, WordApp>) -> Option<usize> {
    h.state().host.as_ref().and_then(|host| host.page_target())
}

/// Where the selection's start is drawn this frame, if its page is on screen.
fn selection_start_on_screen(h: &mut egui_kittest::Harness<'_, WordApp>) -> Option<egui::Pos2> {
    let start = h.state().session.sel.ordered().0;
    let app = h.state_mut();
    let caret = app.session.layout().caret_on(&start, app.session.page_hint)?;
    wordcraft_ui_egui::canvas::page_to_screen(app, caret.page, caret.x, caret.top + caret.height / 2.0)
}

/// The paragraph positions of "The Supplier" in each clause that starts on page index `page`.
fn clause_starts_on(h: &mut egui_kittest::Harness<'_, WordApp>, clauses: usize, page: usize) -> Vec<Pos> {
    let app = h.state_mut();
    let layout = app.session.layout();
    (0..clauses).map(|i| Pos::body(i, format!("Clause {}. ", i + 1).len())).filter(|p| layout.page_of(p) == Some(page)).collect()
}

#[test]
fn jumping_to_a_page_puts_its_top_at_the_top_of_the_view() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 3 }]);
    let app = h.state();
    let (page, view) = (app.canvas.page_rects[2], app.canvas.canvas_rect.expect("canvas"));
    assert!((page.min.y - view.min.y).abs() < 2.0, "page 3 top at {}, view top at {}", page.min.y, view.min.y);
    assert_eq!(page_target(&h), Some(2));
}

#[test]
fn jumping_to_text_selects_the_folded_match_and_scrolls_to_it() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(text_target("CLAUSE 77. the  supplier", None, 0)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.selected_text(), "Clause 77. The Supplier");
    let page = {
        let app = h.state_mut();
        app.session.layout().page_of(&app.session.sel.ordered().0).expect("laid out")
    };
    let events = bridge.take_events();
    assert_eq!(events, vec![HostEvent::PageChanged { page: page as u32 + 1 }], "the passage's page is reported");
    assert!(!events.iter().any(|e| matches!(e, HostEvent::Selection { .. })), "a jump is not a person's selection");
    assert_eq!(page_target(&h), Some(page));
    // The passage is on screen, below the top of the view by no more than the jump's margin.
    let view = h.state().canvas.canvas_rect.expect("canvas");
    let at = selection_start_on_screen(&mut h).expect("the passage's page is on screen");
    assert!(view.contains(at), "the passage at {at:?} is outside the view {view:?}");
    let margin = 60.0 * h.state().canvas.scale;
    assert!(at.y - view.min.y <= margin + 20.0, "the passage sits {} below the view's top", at.y - view.min.y);
}

#[test]
fn page_and_occurrence_narrow_a_text_jump() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let on_page_two = clause_starts_on(&mut h, 120, 1);
    assert!(on_page_two.len() >= 2, "fixture has too few clauses on page 2");
    bridge.send(HostCommand::JumpTo(text_target("the supplier shall deliver", Some(2), 1)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.sel.ordered().0, on_page_two[1]);
    assert_eq!(h.state().session.selected_text(), "The Supplier shall deliver");
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: 2 }]);
    // Without a page, the occurrence counts through the whole document.
    bridge.send(HostCommand::JumpTo(text_target("the supplier shall deliver", None, 76)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.sel.ordered().0, Pos::body(76, "Clause 77. ".len()));
    // The last match on a page is found; one past it is not.
    let last = on_page_two.len() as u32 - 1;
    bridge.take_events();
    bridge.send(HostCommand::JumpTo(text_target("the supplier shall deliver", Some(2), last)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.sel.ordered().0, on_page_two[last as usize]);
    bridge.take_events();
    let past = text_target("the supplier shall deliver", Some(2), last + 1);
    bridge.send(HostCommand::JumpTo(past.clone()));
    frames(&mut h, 2);
    // Finding nothing drops the target, so the host hears the page that is in view from then on.
    let events = bridge.take_events();
    assert_eq!(events.first(), Some(&HostEvent::JumpNotFound { target: past }), "{events:?}");
    let visible = h.state().canvas.visible_page.expect("a visible page");
    assert!(events[1..].iter().all(|e| *e == HostEvent::PageChanged { page: visible as u32 + 1 }), "{events:?}");
}

#[test]
fn a_match_may_cross_lines_but_not_paragraphs() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    // Clause 3 wraps: its whole text spans more than one line.
    let clause = "Clause 3. The Supplier shall deliver the goods listed in Schedule 3 within thirty days of the order.";
    bridge.send(HostCommand::JumpTo(text_target(clause, None, 0)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.selected_text(), clause);
    let (start, end) = h.state().session.sel.ordered();
    let app = h.state_mut();
    let layout = app.session.layout();
    let (top, bottom) = (layout.caret_on(&start, 0).expect("start").top, layout.caret_on(&end, 0).expect("end").top);
    assert!(bottom > top, "the fixture's clause fits on one line, so this proves nothing");
    bridge.take_events();
    let across = text_target("of the order. Clause 4.", None, 0);
    bridge.send(HostCommand::JumpTo(across.clone()));
    frames(&mut h, 2);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: across }]);
}

#[test]
fn a_jump_that_finds_nothing_answers_jump_not_found_with_its_target() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    for target in [
        text_target("no such passage", None, 0),
        text_target("Clause 7.", Some(99), 0),
        text_target("Clause 7.", None, 5),
        JumpTarget::Page { page: 99 },
    ] {
        bridge.send(HostCommand::JumpTo(target.clone()));
        frames(&mut h, 2);
        assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target }]);
    }
}

#[test]
fn saturated_zero_and_empty_targets_answer_jump_not_found() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let before = h.state().session.sel.clone();
    let targets = [
        JumpTarget::Page { page: 0 },
        JumpTarget::Page { page: u32::MAX },
        text_target("Clause 1.", Some(0), 0),
        text_target("Clause 1.", Some(u32::MAX), 0),
        text_target("Clause 1.", None, u32::MAX),
        text_target("Clause 1.", Some(1), u32::MAX),
        text_target("", None, 0),
        text_target(" \t\n ", None, 0),
        text_target("\u{FFFC}", None, 0),
        text_target(&"a".repeat(MAX_NEEDLE_CHARS + 1), None, 0),
    ];
    for target in targets {
        bridge.send(HostCommand::JumpTo(target.clone()));
        frames(&mut h, 2);
        assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: target.clone() }], "{:.40?}", target);
    }
    assert_eq!(h.state().session.sel, before);
    assert_eq!(page_target(&h), None);
}

#[test]
fn a_passage_at_the_length_limit_is_found_and_one_past_it_is_not() {
    let (mut h, bridge) = embed_app();
    let long = "x".repeat(MAX_NEEDLE_CHARS + 5);
    open_doc(&mut h, &bridge, &Document::from_text(&format!("Start\n{long}")));
    bridge.send(HostCommand::JumpTo(text_target(&"X".repeat(MAX_NEEDLE_CHARS), None, 0)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.selected_text().len(), MAX_NEEDLE_CHARS);
    assert!(!bridge.take_events().iter().any(|e| matches!(e, HostEvent::JumpNotFound { .. })));
    let over = text_target(&"x".repeat(MAX_NEEDLE_CHARS + 1), None, 0);
    bridge.send(HostCommand::JumpTo(over.clone()));
    frames(&mut h, 2);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: over }]);
}

#[test]
fn commands_sent_before_the_first_frame_apply_in_order() {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    // Queued before the app ran a single frame, as a host may do right after mount.
    bridge.send(HostCommand::Open { name: "agreement.docx".into(), bytes: docx(&clauses(120)), options: Default::default() });
    bridge.send(HostCommand::JumpTo(text_target("Clause 90.", None, 0)));
    let mut h = harness(app, 1.0);
    frames(&mut h, 4);
    let events = bridge.take_events();
    assert!(matches!(events.first(), Some(HostEvent::Opened { .. })), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, HostEvent::JumpNotFound { .. })), "{events:?}");
    assert_eq!(h.state().session.selected_text(), "Clause 90.");
    let page = {
        let app = h.state_mut();
        app.session.layout().page_of(&app.session.sel.ordered().0).expect("laid out")
    };
    assert_eq!(events.last(), Some(&HostEvent::PageChanged { page: page as u32 + 1 }), "{events:?}");
    assert!(selection_start_on_screen(&mut h).is_some_and(|at| h.state().canvas.canvas_rect.is_some_and(|view| view.contains(at))));
}

#[test]
fn a_newer_host_jump_replaces_a_waiting_one() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // Both arrive before a frame: only the second lands, and the first is not answered.
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 4 }));
    bridge.send(HostCommand::JumpTo(text_target("Clause 30.", None, 0)));
    frames(&mut h, 4);
    let page = {
        let app = h.state_mut();
        app.session.layout().page_of(&Pos::body(29, 0)).expect("laid out")
    };
    assert_ne!(page, 3, "the fixture puts clause 30 on page 4, so this proves nothing");
    assert_eq!(bridge.take_events(), vec![HostEvent::PageChanged { page: page as u32 + 1 }]);
    assert_eq!(h.state().session.selected_text(), "Clause 30.");
    // A newer jump that finds nothing replaces a waiting one too: the view stays where it was.
    let before = h.state().canvas.page_rects.clone();
    let missing = text_target("no such passage", None, 0);
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 5 }));
    bridge.send(HostCommand::JumpTo(missing.clone()));
    frames(&mut h, 4);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: missing }]);
    assert_eq!(h.state().canvas.page_rects, before, "the replaced jump still moved the view");
    assert_eq!(page_target(&h), None);
}

#[test]
fn a_host_jump_replaces_the_page_target() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    // The view bar set a target first.
    wordcraft_ui_egui::host::view::go_to_page(h.state_mut(), 1);
    frames(&mut h, 4);
    assert_eq!(page_target(&h), Some(1));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 4 }));
    frames(&mut h, 4);
    assert_eq!(page_target(&h), Some(3));
    bridge.send(HostCommand::JumpTo(text_target("Clause 3.", None, 0)));
    frames(&mut h, 4);
    assert_eq!(page_target(&h), Some(0));
    bridge.take_events();
    // Not found: the target is dropped, and the host hears the page that is in view.
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 99 }));
    frames(&mut h, 4);
    assert_eq!(page_target(&h), None);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: JumpTarget::Page { page: 99 } }]);
}

#[test]
fn a_jump_waiting_when_an_open_replaces_the_document_answers_jump_not_found() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let second = docx(&clauses(30));
    for target in [JumpTarget::Page { page: 4 }, text_target("Clause 90.", None, 0)] {
        // The jump is found in the open document but has not landed when the next one replaces it.
        bridge.send(HostCommand::JumpTo(target.clone()));
        bridge.send(HostCommand::Open { name: "next.docx".into(), bytes: second.clone(), options: Default::default() });
        frames(&mut h, 4);
        let count = pages(&mut h) as u32;
        assert_eq!(
            bridge.take_events(),
            vec![HostEvent::JumpNotFound { target: target.clone() }, HostEvent::Opened { pages: count }, HostEvent::PageChanged { page: 1 }]
        );
        assert_eq!(h.state().canvas.visible_page, Some(0), "{target:?} was applied to the new document");
        assert_eq!(h.state().session.sel.ordered().0, Pos::body(0, 0));
        assert_eq!(page_target(&h), None);
        open_doc(&mut h, &bridge, &clauses(120));
    }
    // Also when all three are queued before the app ran a single frame.
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let early = text_target("Clause 90.", None, 0);
    bridge.send(HostCommand::Open { name: "a.docx".into(), bytes: docx(&clauses(120)), options: Default::default() });
    bridge.send(HostCommand::JumpTo(early.clone()));
    bridge.send(HostCommand::Open { name: "b.docx".into(), bytes: second.clone(), options: Default::default() });
    let mut h = harness(app, 1.0);
    frames(&mut h, 4);
    let events = bridge.take_events();
    let (first, second) = (HostEvent::Opened { pages: 8 }, HostEvent::Opened { pages: pages(&mut h) as u32 });
    assert_eq!(events, vec![first, HostEvent::JumpNotFound { target: early }, second, HostEvent::PageChanged { page: 1 }]);
}

/// Embed API order for one open: the reported selection's `selectionCleared`, then the waiting
/// jump's `jumpNotFound`, then `opened` (as the other editor sends them).
#[test]
fn an_open_clears_the_reported_selection_before_answering_the_waiting_jump() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let (_, released) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    frames(&mut h, 2);
    let reported = [released, bridge.take_events()].concat();
    assert!(reported.iter().any(is_selection), "no selection was reported: {reported:?}");
    let target = JumpTarget::Page { page: 4 };
    bridge.send(HostCommand::JumpTo(target.clone()));
    bridge.send(HostCommand::Open { name: "next.docx".into(), bytes: docx(&clauses(30)), options: Default::default() });
    frames(&mut h, 4);
    let count = pages(&mut h) as u32;
    assert_eq!(
        bridge.take_events(),
        vec![HostEvent::SelectionCleared, HostEvent::JumpNotFound { target }, HostEvent::Opened { pages: count }, HostEvent::PageChanged { page: 1 }]
    );
}

#[test]
fn a_jump_waiting_when_an_open_fails_still_lands() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 4 }));
    bridge.send(HostCommand::Open { name: "broken.docx".into(), bytes: b"PK\x03\x04 broken".to_vec(), options: Default::default() });
    frames(&mut h, 4);
    let events = bridge.take_events();
    assert!(matches!(events.as_slice(), [HostEvent::OpenFailed { .. }, HostEvent::PageChanged { page: 4 }]), "{events:?}");
}

#[test]
fn a_resize_keeps_the_jump_target_in_view() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
    frames(&mut h, 4);
    bridge.take_events();
    // A narrower window rescales the fitted pages: page 3 goes back to the top of the view.
    for size in [egui::vec2(700.0, 800.0), egui::vec2(1400.0, 600.0)] {
        h.set_size(size);
        frames(&mut h, 4);
        let app = h.state();
        let (page, view) = (app.canvas.page_rects[2], app.canvas.canvas_rect.expect("canvas"));
        assert!((page.min.y - view.min.y).abs() < 2.0, "{size:?}: page 3 top at {}, view top at {}", page.min.y, view.min.y);
        assert_eq!(page_target(&h), Some(2), "{size:?}: a resize is not a person scrolling");
    }
    assert!(bridge.take_events().is_empty(), "the reported page stays 3");
    // A passage stays in view too.
    h.set_size(WINDOW);
    bridge.send(HostCommand::JumpTo(text_target("Clause 77.", None, 0)));
    frames(&mut h, 4);
    for size in [egui::vec2(600.0, 800.0), egui::vec2(1400.0, 700.0)] {
        h.set_size(size);
        frames(&mut h, 4);
        let view = h.state().canvas.canvas_rect.expect("canvas");
        let at = selection_start_on_screen(&mut h);
        assert!(at.is_some_and(|at| view.contains(at)), "{size:?}: the passage at {at:?} left the view {view:?}");
    }
}

#[test]
fn a_selection_drag_that_scrolls_the_view_drops_the_page_target() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
    frames(&mut h, 4);
    assert_eq!(page_target(&h), Some(2));
    let view = h.state().canvas.canvas_rect.expect("canvas");
    // The start of the first clause whose first line is drawn in `band` (screen y).
    let clause_start_in = |h: &mut egui_kittest::Harness<'_, WordApp>, band: std::ops::Range<f32>| {
        (0..120).map(|i| screen_of(h, &Pos::body(i, 0))).find(|at| band.contains(&at.y)).expect("a clause in the band")
    };
    // A drag across the middle of the view selects without scrolling: the target stays.
    let middle = clause_start_in(&mut h, view.center().y - 100.0..view.center().y + 100.0) + egui::vec2(4.0, 0.0);
    h.event(egui::Event::PointerMoved(middle));
    h.event(press(middle, egui::PointerButton::Primary));
    h.event(egui::Event::PointerMoved(middle + egui::vec2(80.0, 0.0)));
    frames(&mut h, 3);
    h.event(release(middle + egui::vec2(80.0, 0.0), egui::PointerButton::Primary));
    frames(&mut h, 4);
    assert!(!h.state().session.sel.is_collapsed(), "the drag selected nothing");
    assert_eq!(page_target(&h), Some(2), "a drag that did not scroll dropped the target");
    // One that starts near the bottom edge scrolls the caret it put there into view while the person
    // drags: the person moved the view, so what is visible is reported from now on.
    let top_before = h.state().canvas.page_rects[2].min.y;
    let low = egui::pos2(middle.x, view.max.y - 40.0);
    h.event(egui::Event::PointerMoved(low));
    h.event(press(low, egui::PointerButton::Primary));
    h.event(egui::Event::PointerMoved(low + egui::vec2(80.0, 0.0)));
    frames(&mut h, 8);
    assert_ne!(h.state().canvas.page_rects[2].min.y, top_before, "the drag did not scroll, so this proves nothing");
    assert_eq!(page_target(&h), None);
    h.event(release(low + egui::vec2(80.0, 0.0), egui::PointerButton::Primary));
    frames(&mut h, 2);
}

// --- setMarks: host highlights ---

use serde_json::json;
use wordcraft_engine::Selection;
use wordcraft_ui_egui::host::MarkSpec;
use wordcraft_ui_egui::host::wire::MAX_MARKS;

const AMBER: &str = "#ffd60080";

fn amber() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xff, 0xd6, 0x00, 0x80)
}

fn mark(id: &str, text: &str) -> MarkSpec {
    MarkSpec { id: id.into(), text: text.into(), page: None, color: AMBER.into() }
}

fn mark_on(id: &str, text: &str, page: u32) -> MarkSpec {
    MarkSpec { page: Some(page), ..mark(id, text) }
}

fn mark_rects(h: &egui_kittest::Harness<'_, WordApp>) -> Vec<(egui::Rect, String)> {
    h.state().host.as_ref().expect("embed host").mark_rects()
}

fn marks_pending(h: &egui_kittest::Harness<'_, WordApp>) -> bool {
    h.state().host.as_ref().expect("embed host").marks_pending()
}

/// Run frames until every mark is resolved (a long list resolves over several frames).
fn settle_marks(h: &mut egui_kittest::Harness<'_, WordApp>) {
    for _ in 0..2_000 {
        frames(h, 1);
        if !marks_pending(h) {
            frames(h, 1);
            return;
        }
    }
    panic!("marks never finished resolving");
}

fn mark_ids(h: &egui_kittest::Harness<'_, WordApp>) -> Vec<String> {
    let mut ids: Vec<String> = mark_rects(h).into_iter().map(|(_, id)| id).collect();
    ids.dedup();
    ids
}

/// Whether a painted mark rectangle covers the start of the passage at `pos` (just right of it).
fn marked_at(h: &mut egui_kittest::Harness<'_, WordApp>, pos: &Pos) -> Vec<String> {
    let at = screen_of(h, pos) + egui::vec2(2.0, 0.0);
    mark_rects(h).into_iter().filter(|(rect, _)| rect.contains(at)).map(|(_, id)| id).collect()
}

fn activated(events: &[HostEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            HostEvent::MarkActivated { id } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn marks_paint_found_passages_and_skip_the_rest() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "clause 2. the supplier"), mark("gone", "not in this document")]));
    frames(&mut h, 3);
    let rects = mark_rects(&h);
    assert!(!rects.is_empty(), "found mark not painted");
    assert!(rects.iter().all(|(_, id)| id == "t1"), "{rects:?}");
    assert!(shapes(&h).iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == amber())), "no rectangle in the mark's colour");
    assert!(bridge.take_events().is_empty(), "unfound marks are skipped silently");
}

#[test]
fn clicking_a_mark_sends_mark_activated() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 3. The Supplier")]));
    frames(&mut h, 3);
    let (rect, _) = mark_rects(&h).first().cloned().expect("mark painted");
    click(&mut h, rect.center(), egui::PointerButton::Primary);
    assert_eq!(activated(&bridge.take_events()), vec!["t1".to_string()]);
    // A click beside every mark activates nothing.
    let elsewhere = screen_of(&mut h, &Pos::body(10, 12));
    click(&mut h, elsewhere, egui::PointerButton::Primary);
    assert!(activated(&bridge.take_events()).is_empty());
}

#[test]
fn marks_sit_above_the_page_and_below_the_selection() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 2.")]));
    h.state_mut().session.sel = Selection { anchor: Pos::body(1, 0), focus: Pos::body(1, 9) };
    frames(&mut h, 3);
    let painted = shapes(&h);
    let selection = wordcraft_ui_egui::theme::Tokens::light().selection;
    let at = |fill: egui::Color32| painted.iter().position(|s| matches!(s, egui::Shape::Rect(r) if r.fill == fill));
    let (page, mark, sel) = (at(egui::Color32::WHITE).expect("page"), at(amber()).expect("mark"), at(selection).expect("selection"));
    assert!(page < mark && mark < sel, "paint order page {page}, mark {mark}, selection {sel}");
}

#[test]
fn an_empty_list_or_a_new_document_clears_marks() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 2.")]));
    frames(&mut h, 2);
    assert!(!mark_rects(&h).is_empty());
    bridge.send(HostCommand::SetMarks(Vec::new()));
    frames(&mut h, 2);
    assert!(mark_rects(&h).is_empty());
    assert!(!shapes(&h).iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == amber())), "a cleared mark is still painted");
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 2.")]));
    frames(&mut h, 2);
    open_doc(&mut h, &bridge, &clauses(5));
    assert!(mark_rects(&h).is_empty(), "marks belong to the document they were set on");
}

#[test]
fn marks_ignore_comment_anchors_inside_the_passage() {
    let mut editing = Session::new(clauses(5));
    editing.run("select.text", &json!({"text": "Supplier", "occurrence": 3})).unwrap();
    editing.run("review.newComment", &json!({"text": "Check the supplier"})).unwrap();
    let anchored = editing.doc.para_at(&Pos::body(2, 0)).is_some_and(|p| p.text.contains('\u{FFFC}'));
    assert!(anchored, "the fixture needs comment anchors inside clause 3");
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &editing.doc);
    bridge.send(HostCommand::SetMarks(vec![mark("c3", "Clause 3. The Supplier shall deliver")]));
    frames(&mut h, 2);
    assert!(!mark_rects(&h).is_empty(), "comment anchors broke the match");
}

#[test]
fn where_marks_overlap_the_topmost_wins_the_click() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let inside = Pos::body(2, "Clause 3. The ".len());
    for (order, winner) in [(["wide", "narrow"], "narrow"), (["narrow", "wide"], "wide")] {
        let marks = order.iter().map(|id| if *id == "wide" { mark("wide", "Clause 3. The Supplier") } else { mark("narrow", "3. The Supplier") });
        bridge.send(HostCommand::SetMarks(marks.collect()));
        frames(&mut h, 3);
        // Both cover the point; the one sent last is painted last.
        let mut covering = marked_at(&mut h, &inside);
        covering.dedup();
        assert_eq!(covering, order.to_vec(), "both marks cover the click, in paint order");
        let painted: Vec<usize> =
            shapes(&h).iter().enumerate().filter(|(_, s)| matches!(s, egui::Shape::Rect(r) if r.fill == amber())).map(|(index, _)| index).collect();
        assert!(painted.len() >= 2, "{painted:?}");
        let at = screen_of(&mut h, &inside) + egui::vec2(2.0, 0.0);
        click(&mut h, at, egui::PointerButton::Primary);
        assert_eq!(activated(&bridge.take_events()), vec![winner.to_string()], "one event, for the topmost mark");
    }
}

#[test]
fn a_drag_across_a_mark_selects_and_activates_nothing() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 3. The Supplier shall deliver")]));
    frames(&mut h, 3);
    let (rect, _) = mark_rects(&h).first().cloned().expect("mark painted");
    let (from, to) = (rect.left_center() + egui::vec2(4.0, 0.0), rect.right_center() - egui::vec2(4.0, 0.0));
    assert!(to.x - from.x > 40.0, "the mark is too narrow to drag across");
    bridge.take_events();
    h.event(egui::Event::PointerMoved(from));
    h.event(press(from, egui::PointerButton::Primary));
    h.event(egui::Event::PointerMoved(from + egui::vec2(20.0, 0.0)));
    h.event(egui::Event::PointerMoved(to));
    h.event(release(to, egui::PointerButton::Primary));
    frames(&mut h, 4);
    assert!(!h.state().session.sel.is_collapsed(), "the drag selected nothing, so this proves nothing");
    assert!(activated(&bridge.take_events()).is_empty(), "a drag ending in a mark activated it");
}

#[test]
fn a_mark_shows_only_its_first_match_on_its_page_or_in_the_document() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let passage = "the supplier shall deliver";
    let at = |clause: usize| Pos::body(clause, format!("Clause {}. ", clause + 1).len());
    // No page: the first match in document order, and no other.
    bridge.send(HostCommand::SetMarks(vec![mark("any", passage)]));
    frames(&mut h, 3);
    assert_eq!(marked_at(&mut h, &at(0)), vec!["any".to_string()]);
    for clause in 1..4 {
        assert!(marked_at(&mut h, &at(clause)).is_empty(), "clause {} is marked too", clause + 1);
    }
    let lines: std::collections::BTreeSet<i32> = mark_rects(&h).iter().map(|(rect, _)| rect.center().y.round() as i32).collect();
    assert_eq!(lines.len(), 1, "one match, on one line: {lines:?}");
    // A page: the first match on that page, even with earlier ones on page 1.
    let on_page_two = clause_starts_on(&mut h, 120, 1);
    assert!(on_page_two.len() >= 2, "fixture has too few clauses on page 2");
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 }));
    bridge.send(HostCommand::SetMarks(vec![mark_on("paged", passage, 2)]));
    frames(&mut h, 4);
    assert_eq!(marked_at(&mut h, &on_page_two[0]), vec!["paged".to_string()]);
    assert!(marked_at(&mut h, &on_page_two[1]).is_empty(), "a later match on the page is marked too");
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 1 }));
    frames(&mut h, 4);
    assert!(marked_at(&mut h, &at(0)).is_empty(), "a paged mark lies on page 1");
    // A page the document lacks, or one without a match, finds nothing.
    bridge.take_events();
    bridge.send(HostCommand::SetMarks(vec![mark_on("far", passage, 999), mark_on("max", passage, u32::MAX), mark_on("zero", passage, 0)]));
    frames(&mut h, 3);
    assert!(mark_rects(&h).is_empty());
    assert!(bridge.take_events().is_empty(), "unfound marks are skipped silently");
}

#[test]
fn marks_beyond_the_cap_are_dropped_and_the_first_kept() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let mut marks: Vec<MarkSpec> = (0..MAX_MARKS).map(|index| mark(&format!("m{index}"), "Clause 1.")).collect();
    marks.push(mark("late", "Clause 2."));
    bridge.send(HostCommand::SetMarks(marks));
    settle_marks(&mut h);
    let ids = mark_ids(&h);
    assert_eq!(ids.len(), MAX_MARKS, "every kept mark is painted");
    assert_eq!(ids.first().map(String::as_str), Some("m0"));
    assert_eq!(ids.last().map(String::as_str), Some(format!("m{}", MAX_MARKS - 1).as_str()));
    assert!(!ids.iter().any(|id| id == "late"), "the mark past the cap was kept");
    assert!(marked_at(&mut h, &Pos::body(1, 0)).is_empty());
    // The last one sent is on top.
    let (rect, _) = mark_rects(&h).last().cloned().expect("painted");
    click(&mut h, rect.center(), egui::PointerButton::Primary);
    assert_eq!(activated(&bridge.take_events()), vec![format!("m{}", MAX_MARKS - 1)]);
}

#[test]
fn a_passage_longer_than_the_cap_is_skipped_silently() {
    let (mut h, bridge) = embed_app();
    let long = "x".repeat(MAX_NEEDLE_CHARS + 5);
    open_doc(&mut h, &bridge, &Document::from_text(&format!("Start\n{long}")));
    bridge.send(HostCommand::SetMarks(vec![mark("over", &"x".repeat(MAX_NEEDLE_CHARS + 1)), mark("start", "Start")]));
    frames(&mut h, 3);
    assert_eq!(mark_ids(&h), vec!["start".to_string()]);
    assert!(bridge.take_events().is_empty());
    bridge.send(HostCommand::SetMarks(vec![mark("at", &"X".repeat(MAX_NEEDLE_CHARS))]));
    frames(&mut h, 3);
    assert_eq!(mark_ids(&h), vec!["at".to_string()], "a passage at the cap is found");
}

#[test]
fn marks_follow_a_changed_document_and_a_new_layout() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let clause = "Clause 3. The Supplier shall deliver the goods listed in Schedule 3 within thirty days of the order.";
    bridge.send(HostCommand::SetMarks(vec![mark("c3", clause)]));
    frames(&mut h, 3);
    assert_eq!(marked_at(&mut h, &Pos::body(2, 0)), vec!["c3".to_string()]);
    let lines_before = mark_rects(&h).len();
    // The same document without the host's open (rev and layout change): the mark moves with its text.
    let mut moved = clauses(20);
    moved.body.insert(0, wordcraft_doc::para_block(wordcraft_doc::Paragraph::with_text("Preamble.", Default::default())));
    h.state_mut().session.set_document(moved);
    frames(&mut h, 3);
    assert_eq!(marked_at(&mut h, &Pos::body(3, 0)), vec!["c3".to_string()], "the mark stayed on the old text");
    assert!(marked_at(&mut h, &Pos::body(2, 0)).is_empty(), "the stale rectangle is still painted");
    // A wider page in web view rewraps the clause onto fewer lines: the rectangles follow the layout.
    h.state_mut().session.view.mode = wordcraft_layout::ViewMode::Web;
    h.set_size(egui::vec2(2400.0, 800.0));
    frames(&mut h, 4);
    assert_eq!(marked_at(&mut h, &Pos::body(3, 0)), vec!["c3".to_string()]);
    let lines_after = mark_rects(&h).len();
    assert!(lines_after < lines_before, "the clause wraps as before ({lines_before} lines, now {lines_after})");
    let app = h.state_mut();
    let layout = app.session.layout();
    let end = Pos::body(3, clause.len());
    let expected = layout.selection_rects(&app.session.doc, &Pos::body(3, 0), &end, 0).len();
    assert_eq!(lines_after, expected, "the rectangles are the new layout's");
}

#[test]
fn comment_balloons_stay_clickable_beside_marked_text() {
    let mut editing = Session::new(clauses(5));
    editing.run("select.text", &json!({"text": "Supplier", "occurrence": 3})).unwrap();
    editing.run("review.newComment", &json!({"text": "Check the supplier"})).unwrap();
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &editing.doc);
    bridge.send(HostCommand::SetMarks(vec![mark("c3", "Clause 3. The Supplier shall deliver")]));
    frames(&mut h, 3);
    assert!(!mark_rects(&h).is_empty());
    let balloon = shapes(&h).iter().find_map(|s| match s {
        egui::Shape::Text(text) if text.galley.text().contains("Check the supplier") => Some(text.visual_bounding_rect()),
        _ => None,
    });
    let balloon = balloon.expect("the comment's balloon is drawn with marks set");
    assert!(mark_rects(&h).iter().all(|(rect, _)| !rect.intersects(balloon)), "a mark covers the balloon");
    // The balloon's leader line runs from the anchor, inside the marked passage, to the page edge:
    // it is painted after the marks, so they do not cover it.
    let painted = shapes(&h);
    let marked: Vec<egui::Rect> = mark_rects(&h).into_iter().map(|(rect, _)| rect.expand(1.0)).collect();
    let last_mark = painted.iter().rposition(|s| matches!(s, egui::Shape::Rect(r) if r.fill == amber())).expect("mark painted");
    let leader_in_mark = painted.iter().position(|s| match s {
        egui::Shape::LineSegment { points, .. } => points.iter().any(|at| marked.iter().any(|rect| rect.contains(*at))),
        _ => false,
    });
    let leader_in_mark = leader_in_mark.expect("the leader line starts inside the marked passage");
    assert!(leader_in_mark > last_mark, "the leader line (shape {leader_in_mark}) is under the marks (last at {last_mark})");
    h.state_mut().session.sel = Selection::caret(Pos::body(0, 0));
    frames(&mut h, 2);
    bridge.take_events();
    click(&mut h, balloon.center(), egui::PointerButton::Primary);
    assert!(activated(&bridge.take_events()).is_empty(), "a balloon click activated a mark");
    assert_eq!(h.state().session.sel.focus.path, Pos::body(2, 0).path, "the balloon's click moves to its comment");
}

#[test]
fn a_large_mark_set_finds_what_a_search_per_mark_finds() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let marks: Vec<MarkSpec> = (0..MAX_MARKS)
        .map(|k| {
            let id = format!("m{k}");
            match k % 4 {
                0 => mark(&id, &format!("Clause {}. The Supplier", k % 130)),
                1 => mark_on(&id, "the supplier shall deliver", (k % 5) as u32),
                2 => mark(&id, &format!("schedule {} within", k % 130)),
                _ => mark_on(&id, "within thirty days of the order", 1),
            }
        })
        .collect();
    bridge.send(HostCommand::SetMarks(marks.clone()));
    settle_marks(&mut h);
    // What each mark finds when searched for alone, as jumps do.
    let expected: Vec<Option<Pos>> = {
        let app = h.state_mut();
        let layout = app.session.layout();
        let mut index = wordcraft_ui_egui::host::text_match::TextIndex::default();
        index.refresh(&app.session.doc, app.session.rev());
        let pages = layout.pages.len();
        marks
            .iter()
            .map(|m| {
                let on_page = match m.page {
                    Some(page) => Some(wordcraft_ui_egui::host::wire::page_index(page, pages)?),
                    None => None,
                };
                index.find_where(&m.text, |start, _| on_page.is_none_or(|p| layout.page_of(start) == Some(p)), 0).map(|(start, _)| start)
            })
            .collect()
    };
    let painted = mark_ids(&h);
    assert!(painted.len() > 100, "the fixture paints too little: {}", painted.len());
    for id in &painted {
        let k: usize = id.trim_start_matches('m').parse().expect("mark id");
        assert!(expected[k].is_some(), "{id} is painted but a search for it alone finds nothing");
    }
    // Every mark whose own match is on page 1 is painted there, over its match.
    let view = h.state().canvas.canvas_rect.expect("canvas");
    let mut checked = 0;
    for (k, start) in expected.iter().enumerate() {
        let Some(start) = start else { continue };
        let on_first_page = h.state_mut().session.layout().page_of(start) == Some(0);
        if !on_first_page || !view.contains(screen_of(&mut h, start)) {
            continue;
        }
        assert!(marked_at(&mut h, start).contains(&format!("m{k}")), "m{k} is not painted over its match");
        checked += 1;
    }
    assert!(checked > 100, "too few marks checked: {checked}");
}

#[test]
fn passages_longer_than_the_raw_cap_are_skipped_and_not_found() {
    use wordcraft_ui_egui::host::text_match::MAX_RAW_PASSAGE_BYTES;
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    // Mostly whitespace: each folds to "clause 2.", so only the raw length tells them apart.
    let filler = " \t\n".repeat(MAX_RAW_PASSAGE_BYTES / 3 + 1);
    let at_cap = format!("{}Clause 2.", &filler[..MAX_RAW_PASSAGE_BYTES - "Clause 2.".len()]);
    assert_eq!(at_cap.len(), MAX_RAW_PASSAGE_BYTES);
    let over = format!("{}{at_cap}", " ".repeat(300 * 1024 - MAX_RAW_PASSAGE_BYTES));
    assert_eq!(over.len(), 300 * 1024);
    bridge.send(HostCommand::SetMarks(vec![mark("over", &over), mark("at", &at_cap)]));
    frames(&mut h, 3);
    assert_eq!(mark_ids(&h), vec!["at".to_string()], "the mark over the raw cap was kept");
    assert!(bridge.take_events().is_empty(), "a mark over the cap is skipped silently");
    let target = text_target(&over, None, 0);
    bridge.send(HostCommand::JumpTo(target.clone()));
    frames(&mut h, 2);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target }]);
    bridge.send(HostCommand::JumpTo(text_target(&at_cap, None, 0)));
    frames(&mut h, 4);
    assert_eq!(h.state().session.selected_text(), "Clause 2.");
}

// --- Selection events ---

use egui_kittest::kittest::Queryable;

fn assert_selection_at(h: &mut egui_kittest::Harness<'_, WordApp>, events: &[HostEvent], end: &Pos, expect_text: &str) {
    let [HostEvent::Selection { text, x, y, .. }] = events else { panic!("expected one selection event: {events:?}") };
    assert_eq!(text, expect_text);
    let (ex, ey) = selection_end_on_screen(h, end);
    assert!((x - ex).abs() < 1.0 && (y - ey).abs() < 6.0, "selection at ({x}, {y}), its end is at ({ex}, {ey})");
}

fn is_selection(event: &HostEvent) -> bool {
    matches!(event, HostEvent::Selection { .. })
}

/// Drag-select "Clause" in the first clause drawn in the middle of the view (a press near an edge
/// scrolls the caret into view, which would move the drag's end); asserts it was reported.
fn select_a_visible_clause(h: &mut egui_kittest::Harness<'_, WordApp>, bridge: &HostBridge, clauses: usize) {
    let view = h.state().canvas.canvas_rect.expect("canvas");
    let band = view.center().y - 150.0..view.center().y + 150.0;
    let clause = (0..clauses).find(|i| band.contains(&screen_of(h, &Pos::body(*i, 0)).y)).expect("a clause in view");
    bridge.take_events();
    let (_, after) = drag_select(h, bridge, &Pos::body(clause, 0), &Pos::body(clause, 6));
    assert!(matches!(after.as_slice(), [HostEvent::Selection { text, .. }] if text == "Clause"), "{after:?}");
}

#[test]
fn a_drag_sends_one_selection_when_released() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let (during, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    assert!(during.is_empty(), "selection sent before the pointer was released: {during:?}");
    assert!(matches!(after.as_slice(), [HostEvent::Selection { copy_text, page: Some(1), .. }] if copy_text == "Clause 1."), "{after:?}");
    assert_selection_at(&mut h, &after, &Pos::body(0, 9), "Clause 1.");
    frames(&mut h, 3);
    assert!(bridge.take_events().is_empty(), "selection re-sent without a change");
}

#[test]
fn selecting_the_same_passage_again_reports_it_again() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    for round in 1..=2 {
        let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
        assert_selection_at(&mut h, &after, &Pos::body(0, 9), "Clause 1.");
        frames(&mut h, 3);
        assert!(bridge.take_events().is_empty(), "round {round}: re-sent without a new press");
    }
}

/// The same drag over the same passage, right after the first (a browser's frame rate, not
/// kittest's quarter-second steps): a press that started a drag is no click, so the second press
/// is not a double-click selecting a word, and the passage is reported again.
#[test]
fn dragging_over_the_same_passage_again_at_once_reports_it_again() {
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let mut h = egui_kittest::Harness::builder().with_size(WINDOW).with_step_dt(1.0 / 60.0).with_max_steps(12).build_ui_state(
        |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    bridge.set_context(h.ctx.clone());
    wordcraft_ui_egui::host::prepare_context(&h.ctx);
    frames(&mut h, 2);
    open_doc(&mut h, &bridge, &clauses(20));
    for round in 1..=3 {
        let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
        let [HostEvent::Selection { text, .. }] = after.as_slice() else { panic!("round {round}: {after:?}") };
        assert_eq!(text, "Clause 1.", "round {round}");
    }
}

#[test]
fn a_selection_across_paragraphs_keeps_line_breaks_in_copy_text_only() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(1, 9));
    let [HostEvent::Selection { text, copy_text, .. }] = after.as_slice() else { panic!("{after:?}") };
    assert!(copy_text.contains('\n') && copy_text.ends_with("\nClause 2."), "{copy_text:?}");
    assert!(!text.contains('\n') && text.ends_with(" Clause 2."), "{text:?}");
}

#[test]
fn selection_coordinates_are_points_at_double_density() {
    let (mut h, bridge) = view_app(Document::new(), 2.0);
    open_doc(&mut h, &bridge, &clauses(20));
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    assert_selection_at(&mut h, &after, &Pos::body(0, 9), "Clause 1.");
}

#[test]
fn selection_coordinates_are_css_pixels_under_an_egui_zoom() {
    let (mut h, bridge) = embed_app();
    // Since the upstream merge the app sets egui's zoom factor from its interface size every frame.
    h.state_mut().ui.interface_size = wordcraft_ui_egui::theme::InterfaceSize::from_percent(150.0).unwrap();
    frames(&mut h, 2);
    assert_eq!(h.ctx.zoom_factor(), 1.5);
    open_doc(&mut h, &bridge, &clauses(20));
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    let [HostEvent::Selection { x, y, .. }] = after.as_slice() else { panic!("{after:?}") };
    let (ex, ey) = selection_end_on_screen(&mut h, &Pos::body(0, 9));
    // One egui point is `zoom` CSS pixels.
    assert!((x - ex * 1.5).abs() < 1.5 && (y - ey * 1.5).abs() < 9.0, "selection at ({x}, {y}), its end in points ({ex}, {ey})");
}

#[test]
fn scrolled_selection_coordinates_follow_the_page() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
    frames(&mut h, 4);
    let clause = {
        let app = h.state_mut();
        let layout = app.session.layout();
        (0..120).find(|i| layout.page_of(&Pos::body(*i, 0)) == Some(2)).expect("a clause starts on page 3")
    };
    bridge.take_events();
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(clause, 0), &Pos::body(clause, 6));
    assert_selection_at(&mut h, &after, &Pos::body(clause, 6), "Clause");
    let [HostEvent::Selection { y, page, .. }] = after.as_slice() else { panic!("{after:?}") };
    assert_eq!(*page, Some(3));
    assert!(*y > 0.0 && *y < WINDOW.y, "y {y} is outside the canvas");
}

#[test]
fn keyboard_selection_is_reported_and_a_click_clears_it() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let start = screen_of(&mut h, &Pos::body(0, 0));
    click(&mut h, start, egui::PointerButton::Primary);
    bridge.take_events();
    // A person's key presses arrive in frames of their own.
    for _ in 0..3 {
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowRight);
        frames(&mut h, 1);
    }
    frames(&mut h, 2);
    let texts: Vec<String> = bridge
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            HostEvent::Selection { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["C", "Cl", "Cla"]);
    let elsewhere = screen_of(&mut h, &Pos::body(1, 4));
    click(&mut h, elsewhere, egui::PointerButton::Primary);
    assert!(bridge.take_events().contains(&HostEvent::SelectionCleared));
}

#[test]
fn jumps_and_finds_send_no_selection_but_clear_a_reported_one() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(30));
    drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    bridge.send(HostCommand::JumpTo(text_target("Clause 5.", None, 0)));
    frames(&mut h, 3);
    let events = bridge.take_events();
    assert!(events.contains(&HostEvent::SelectionCleared), "{events:?}");
    assert!(!events.iter().any(is_selection), "{events:?}");
    h.get_by(find_field()).focus();
    frames(&mut h, 2);
    h.get_by(find_field()).type_text("schedule 7");
    frames(&mut h, 3);
    assert_eq!(h.state().session.selected_text(), "Schedule 7");
    assert!(!bridge.take_events().iter().any(is_selection));
}

#[test]
fn every_selection_the_editor_makes_clears_a_reported_one_and_is_never_reported() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    for action in ["host page jump", "host text jump", "next page button", "find", "open"] {
        select_a_visible_clause(&mut h, &bridge, 120);
        match action {
            "host page jump" => bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 2 })),
            "host text jump" => bridge.send(HostCommand::JumpTo(text_target("Clause 9.", None, 0))),
            "next page button" => h.get_by_label("Next page").click(),
            "find" => {
                h.get_by(find_field()).focus();
                frames(&mut h, 2);
                h.get_by(find_field()).type_text("goods listed");
            }
            _ => bridge.send(HostCommand::Open { name: "next.docx".into(), bytes: docx(&clauses(120)), options: Default::default() }),
        }
        frames(&mut h, 4);
        let events = bridge.take_events();
        assert_eq!(events.iter().filter(|e| **e == HostEvent::SelectionCleared).count(), 1, "{action}: {events:?}");
        assert!(!events.iter().any(is_selection), "{action}: {events:?}");
        assert!(!h.state().session.sel.is_collapsed() || action != "host text jump", "{action}: the passage is selected");
        if action == "open" {
            let cleared = events.iter().position(|e| *e == HostEvent::SelectionCleared);
            let opened = events.iter().position(|e| matches!(e, HostEvent::Opened { .. }));
            assert!(cleared < opened && opened.is_some(), "{events:?}");
        }
        frames(&mut h, 3);
        assert!(bridge.take_events().iter().all(|e| !is_selection(e) && *e != HostEvent::SelectionCleared), "{action}: sent later");
    }
}

#[test]
fn a_jump_that_finds_nothing_keeps_a_reported_selection() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(30));
    drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    let missing = text_target("no such passage", None, 0);
    bridge.send(HostCommand::JumpTo(missing.clone()));
    frames(&mut h, 3);
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: missing }]);
    assert_eq!(h.state().session.selected_text(), "Clause 1.");
}

#[test]
fn a_newer_jump_that_finds_nothing_puts_back_the_selection_the_replaced_jump_made() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    let (sel, hint) = (h.state().session.sel.clone(), h.state().session.page_hint);
    let before = h.state().canvas.page_rects.clone();
    for first in [text_target("Clause 90.", None, 0), JumpTarget::Page { page: 5 }] {
        let missing = text_target("no such passage", None, 0);
        bridge.send(HostCommand::JumpTo(first.clone()));
        bridge.send(HostCommand::JumpTo(missing.clone()));
        frames(&mut h, 4);
        // The replaced jump changed nothing: the person's selection is still the one the host knows.
        assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: missing }], "{first:?}");
        assert_eq!(h.state().session.sel, sel, "{first:?}: the selection was not put back");
        assert_eq!(h.state().session.page_hint, hint, "{first:?}: the page hint was not put back");
        assert_eq!(h.state().canvas.page_rects, before, "{first:?}: the view moved");
    }
    // It is still the reported one: a click clears it.
    let elsewhere = screen_of(&mut h, &Pos::body(1, 4));
    click(&mut h, elsewhere, egui::PointerButton::Primary);
    assert_eq!(bridge.take_events(), vec![HostEvent::SelectionCleared]);
}

#[test]
fn a_whitespace_only_selection_counts_as_cleared() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &Document::from_text("Alpha     beta\nGamma"));
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 5));
    assert!(matches!(after.as_slice(), [HostEvent::Selection { text, .. }] if text == "Alpha"), "{after:?}");
    for (from, to) in [(Pos::body(0, 5), Pos::body(0, 10)), (Pos::body(0, 6), Pos::body(0, 8)), (Pos::body(0, 14), Pos::body(1, 0))] {
        h.state_mut().session.sel = Selection { anchor: from, focus: to };
        frames(&mut h, 2);
    }
    assert_eq!(bridge.take_events(), vec![HostEvent::SelectionCleared], "cleared once");
}

#[test]
fn holding_the_button_without_moving_keeps_the_page_target() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    bridge.send(HostCommand::JumpTo(JumpTarget::Page { page: 3 }));
    frames(&mut h, 4);
    assert_eq!(page_target(&h), Some(2));
    let view = h.state().canvas.canvas_rect.expect("canvas");
    // A press near the bottom edge puts the caret there, and the canvas scrolls to keep it in sight.
    let low = egui::pos2(view.center().x, view.max.y - 40.0);
    let top_before = h.state().canvas.page_rects[2].min.y;
    h.event(egui::Event::PointerMoved(low));
    h.event(press(low, egui::PointerButton::Primary));
    frames(&mut h, 8);
    assert_ne!(h.state().canvas.page_rects[2].min.y, top_before, "the press did not scroll, so this proves nothing");
    assert_eq!(page_target(&h), Some(2), "a press without a drag is not a person scrolling");
    h.event(release(low, egui::PointerButton::Primary));
    frames(&mut h, 2);
}

#[test]
fn text_jump_occurrences_count_only_laid_out_matches_with_or_without_a_page() {
    use std::sync::Arc;
    use wordcraft_doc::props::{CharProps, VMerge};
    use wordcraft_doc::{Block, Paragraph, StoryRef, Table, para_block};
    let line = |text: &str| Paragraph::with_text(text, CharProps::default());
    // The second match sits in a cell that continues a vertical merge: it is never laid out.
    let mut table = Table::new(2, 1, 300.0);
    table.rows[0].cells[0].blocks = vec![para_block(line("Merged"))];
    table.rows[0].cells[0].props.vmerge = VMerge::Restart;
    table.rows[1].cells[0].blocks = vec![para_block(line("Notice period"))];
    table.rows[1].cells[0].props.vmerge = VMerge::Continue;
    let mut doc = Document::new();
    doc.body = vec![para_block(line("Notice period")), Arc::new(Block::Table(table)), para_block(line("Notice period"))];
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    let unplaced = {
        let app = h.state_mut();
        let layout = app.session.layout();
        let doc = &app.session.doc;
        doc.para_paths(StoryRef::Body)
            .into_iter()
            .filter(|path| doc.para(StoryRef::Body, path).is_some_and(|p| p.text == "Notice period"))
            .filter(|path| layout.page_of(&Pos { story: StoryRef::Body, path: path.clone(), off: 0 }).is_none())
            .count()
    };
    assert_eq!(unplaced, 1, "the fixture has no unplaced match, so this proves nothing");
    for page in [None, Some(1)] {
        bridge.send(HostCommand::JumpTo(text_target("notice period", page, 1)));
        frames(&mut h, 4);
        assert_eq!(h.state().session.sel.ordered().0, Pos::body(2, 0), "page {page:?}");
        bridge.take_events();
        let past = text_target("notice period", page, 2);
        bridge.send(HostCommand::JumpTo(past.clone()));
        frames(&mut h, 4);
        assert!(bridge.take_events().contains(&HostEvent::JumpNotFound { target: past }), "page {page:?}");
    }
}

// --- Selection events: visible text and the editor's own selections ---

/// A paragraph of `(text, props)` parts.
fn paragraph_of(parts: &[(&str, wordcraft_doc::props::CharProps)]) -> wordcraft_doc::Paragraph {
    let mut paragraph = wordcraft_doc::Paragraph::new();
    for (text, props) in parts {
        paragraph.text.push_str(text);
        paragraph.runs.push(wordcraft_doc::Run { len: text.len(), props: props.clone() });
    }
    paragraph
}

#[test]
fn selection_text_leaves_out_tracked_deletions_and_hidden_text_and_is_found_again() {
    use wordcraft_doc::para_block;
    use wordcraft_doc::props::CharProps;
    let plain = CharProps::default();
    let deleted = CharProps { del: Some(0), ..CharProps::default() };
    let hidden = CharProps { hidden: Some(true), ..CharProps::default() };
    let mut doc = Document::new();
    doc.body = vec![
        para_block(paragraph_of(&[("The fee is ", plain.clone()), ("ten ", deleted), ("five dollars.", plain.clone())])),
        para_block(paragraph_of(&[("Alpha ", plain.clone()), ("SECRET ", hidden), ("Beta.", plain)])),
    ];
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    let end_of = |h: &egui_kittest::Harness<'_, WordApp>, i: usize| {
        let path = Pos::body(i, 0).path;
        Pos::body(i, h.state().session.doc.para(wordcraft_doc::StoryRef::Body, &path).map_or(0, |p| p.text.len()))
    };
    assert_eq!(h.state().session.doc.para_at(&Pos::body(1, 0)).map(|p| p.text.clone()).as_deref(), Some("Alpha SECRET Beta."));
    // Across both paragraphs, by a drag: the deleted and hidden words are not there, the break is.
    let last = end_of(&h, 1);
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(0, 0), &last);
    let [HostEvent::Selection { text, copy_text, .. }] = after.as_slice() else { panic!("{after:?}") };
    assert_eq!(copy_text, "The fee is five dollars.\nAlpha Beta.");
    assert_eq!(text, "The fee is five dollars. Alpha Beta.");
    // Each paragraph's reported text is found again as a jump and as a mark.
    for i in 0..2 {
        let end = end_of(&h, i);
        h.state_mut().session.sel = Selection { anchor: Pos::body(i, 0), focus: end.clone() };
        frames(&mut h, 2);
        let events = bridge.take_events();
        let [HostEvent::Selection { text, .. }] = events.as_slice() else { panic!("{events:?}") };
        let text = text.clone();
        bridge.send(HostCommand::JumpTo(text_target(&text, None, 0)));
        frames(&mut h, 3);
        assert!(!bridge.take_events().iter().any(|e| matches!(e, HostEvent::JumpNotFound { .. })), "{text:?} was not found by a jump");
        assert_eq!(h.state().session.sel.ordered(), (Pos::body(i, 0), end), "{text:?}");
        bridge.send(HostCommand::SetMarks(vec![mark("again", &text)]));
        settle_marks(&mut h);
        assert_eq!(mark_ids(&h), vec!["again".to_string()], "{text:?} was not found as a mark");
    }
}

#[test]
fn a_selection_inside_deleted_and_hidden_runs_reports_and_copies_only_visible_text() {
    use wordcraft_doc::para_block;
    use wordcraft_doc::props::CharProps;
    let plain = CharProps::default();
    let deleted = CharProps { del: Some(0), ..CharProps::default() };
    let hidden = CharProps { hidden: Some(true), ..CharProps::default() };
    let mut doc = Document::new();
    doc.body = vec![
        para_block(paragraph_of(&[("The fee is ", plain.clone()), ("ten ", deleted), ("five dollars.", plain.clone())])),
        para_block(paragraph_of(&[("Alpha ", plain.clone()), ("SECRET ", hidden), ("Beta.", plain)])),
    ];
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &doc);
    // The canvas takes the keyboard on a click.
    let at = screen_of(&mut h, &Pos::body(0, 3));
    click(&mut h, at, egui::PointerButton::Primary);
    bridge.take_events();
    // Starting inside the deleted "ten " and ending inside the hidden "SECRET ", either way round.
    let (inside_deleted, inside_hidden) = (Pos::body(0, 12), Pos::body(1, 9));
    for (anchor, focus) in [(inside_deleted.clone(), inside_hidden.clone()), (inside_hidden, inside_deleted)] {
        h.state_mut().session.sel = wordcraft_engine::Selection { anchor, focus };
        frames(&mut h, 2);
        let events = bridge.take_events();
        let [HostEvent::Selection { text, copy_text, .. }] = events.as_slice() else { panic!("{events:?}") };
        assert_eq!((text.as_str(), copy_text.as_str()), ("five dollars. Alpha", "five dollars.\nAlpha "));
        // Ctrl/⌘+C copies exactly the reported `copyText`.
        h.event(egui::Event::Copy);
        h.step();
        let commands = &h.output().platform_output.commands;
        let copied: Vec<_> = commands.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }).collect();
        assert_eq!(copied, vec![copy_text.clone()]);
        h.state_mut().session.sel = wordcraft_engine::Selection::default();
        frames(&mut h, 2);
        bridge.take_events();
    }
}

/// A harness whose `ui` can be skipped for a frame (`logic` still runs), as eframe skips it when the
/// view is not shown.
fn skippable_app() -> (egui_kittest::Harness<'static, WordApp>, HostBridge, std::rc::Rc<std::cell::Cell<bool>>) {
    let skip = std::rc::Rc::new(std::cell::Cell::new(false));
    let bridge = HostBridge::new();
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let skipping = skip.clone();
    let mut h = egui_kittest::Harness::builder().with_size(WINDOW).with_max_steps(12).build_ui_state(
        move |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            if !skipping.get() {
                app.ui(ui);
            }
        },
        app,
    );
    bridge.set_context(h.ctx.clone());
    frames(&mut h, 2);
    bridge.take_events();
    (h, bridge, skip)
}

#[test]
fn a_jump_held_across_a_frame_is_not_undone_by_a_newer_one_that_finds_nothing() {
    let (mut h, bridge, skip) = skippable_app();
    open_doc(&mut h, &bridge, &clauses(120));
    drag_select(&mut h, &bridge, &Pos::body(0, 0), &Pos::body(0, 9));
    // The first jump is found in a frame that draws nothing, so it is still waiting a frame later.
    skip.set(true);
    bridge.send(HostCommand::JumpTo(text_target("Clause 90.", None, 0)));
    frames(&mut h, 1);
    skip.set(false);
    let missing = text_target("no such passage", None, 0);
    bridge.send(HostCommand::JumpTo(missing.clone()));
    frames(&mut h, 3);
    assert_eq!(h.state().session.selected_text(), "Clause 90.", "a selection from an earlier frame was put back");
    assert_eq!(bridge.take_events(), vec![HostEvent::JumpNotFound { target: missing }, HostEvent::SelectionCleared]);
}

#[test]
fn a_find_that_leaves_the_selection_alone_keeps_a_reported_one() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(30));
    h.get_by(find_field()).focus();
    frames(&mut h, 2);
    h.get_by(find_field()).type_text("zzz");
    frames(&mut h, 3);
    select_a_visible_clause(&mut h, &bridge, 30);
    // Next and Previous find no match: nothing moved, so the host's selection stands.
    for button in ["Next match", "Previous match"] {
        h.get_by_label(button).click();
        frames(&mut h, 3);
        assert_eq!(bridge.take_events(), vec![], "{button}");
    }
    // Escape in the field clears the search but selects nothing.
    h.get_by(find_field()).focus();
    frames(&mut h, 2);
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    assert_eq!(bridge.take_events(), vec![]);
    assert_eq!(h.state().session.selected_text(), "Clause");
}

#[test]
fn shift_click_extends_a_selection_and_reports_it() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    let start = screen_of(&mut h, &Pos::body(0, 0));
    click(&mut h, start, egui::PointerButton::Primary);
    bridge.take_events();
    let end = screen_of(&mut h, &Pos::body(0, 9));
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.event(egui::Event::PointerMoved(end));
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
    h.event(egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::SHIFT });
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    frames(&mut h, 6);
    let events = bridge.take_events();
    assert_selection_at(&mut h, &events, &Pos::body(0, 9), "Clause 1.");
}

#[test]
fn a_backward_selection_reports_its_document_order_end() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    // Dragged from the end of clause 2 back to the start of clause 1: the focus comes first.
    let (_, after) = drag_select(&mut h, &bridge, &Pos::body(1, 9), &Pos::body(0, 0));
    assert!(h.state().session.sel.focus < h.state().session.sel.anchor, "the drag did not select backwards");
    let [HostEvent::Selection { copy_text, .. }] = after.as_slice() else { panic!("{after:?}") };
    assert!(copy_text.starts_with("Clause 1.") && copy_text.ends_with("\nClause 2."), "{copy_text:?}");
    let text = copy_text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_selection_at(&mut h, &after, &Pos::body(1, 9), &text);
}

// --- setTheme: dark or light chrome and the host's accent; pages stay white. ---

use wordcraft_ui_egui::host::ThemeSpec;
use wordcraft_ui_egui::theme::{AccentOverride, Tokens};

fn theme(dark: bool, accent: &str) -> ThemeSpec {
    ThemeSpec { dark, accent: accent.into(), accent_content: "#ffffff".into() }
}

#[test]
fn set_theme_applies_dark_mode_and_the_accent_while_pages_stay_white() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(5));
    bridge.send(HostCommand::SetTheme(theme(true, "#0f766e")));
    frames(&mut h, 3);
    let teal = egui::Color32::from_rgb(0x0f, 0x76, 0x6e);
    let tokens = Tokens::get(&h.ctx);
    assert!(tokens.dark);
    assert_eq!(tokens.accent, teal);
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert_eq!(h.ctx.global_style().visuals.selection.bg_fill, egui::Color32::from_rgba_unmultiplied(0x0f, 0x76, 0x6e, 0x55));
    let painted = shapes(&h);
    let page = h.state().canvas.page_rects.first().copied().expect("a page is on screen");
    assert!(
        painted.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == egui::Color32::WHITE && r.rect.expand(0.5).contains_rect(page) && page.expand(0.5).contains_rect(r.rect))),
        "the page itself must stay white"
    );
    assert!(painted.iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == teal)), "the page indicator uses the accent");
}

#[test]
fn switching_back_to_light_keeps_the_accent() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(5));
    bridge.send(HostCommand::SetTheme(theme(true, "#0f766e")));
    frames(&mut h, 2);
    bridge.send(HostCommand::SetTheme(theme(false, "#7c3aed")));
    frames(&mut h, 2);
    let tokens = Tokens::get(&h.ctx);
    assert!(!tokens.dark);
    assert_eq!(tokens.accent, egui::Color32::from_rgb(0x7c, 0x3a, 0xed));
    assert!(!h.ctx.global_style().visuals.dark_mode);
}

#[test]
fn a_system_theme_change_never_replaces_the_hosts_choice() {
    let other = |theme: egui::Theme| if theme == egui::Theme::Dark { egui::Theme::Light } else { egui::Theme::Dark };
    // Every pairing of the host's choice with the system theme in force when it chose.
    for host_dark in [true, false] {
        for system_then in [egui::Theme::Dark, egui::Theme::Light] {
            let (mut h, bridge) = embed_app();
            // The test harness pins egui's theme; the page's browser starts out following the system.
            h.ctx.set_theme(egui::ThemePreference::System);
            open_doc(&mut h, &bridge, &clauses(5));
            h.input_mut().system_theme = Some(system_then);
            frames(&mut h, 2);
            bridge.send(HostCommand::SetTheme(theme(host_dark, "#0f766e")));
            frames(&mut h, 2);
            // The system's theme changes after the host chose, and back again.
            for changed in [other(system_then), system_then, other(system_then)] {
                h.input_mut().system_theme = Some(changed);
                frames(&mut h, 3);
                let case = format!("host dark {host_dark}, system {system_then:?} then {changed:?}");
                let tokens = Tokens::get(&h.ctx);
                assert_eq!(tokens.dark, host_dark, "{case}: the host's choice was replaced");
                assert_eq!(tokens.accent, egui::Color32::from_rgb(0x0f, 0x76, 0x6e), "{case}");
                // The app's own style, not egui's default for the other theme.
                let style = h.ctx.global_style();
                assert_eq!(style.visuals.dark_mode, host_dark, "{case}");
                assert_eq!(style.visuals.panel_fill, tokens.ribbon, "{case}: chrome lost the app's style");
                assert_eq!(style.spacing.item_spacing, egui::vec2(6.0, 4.0), "{case}: chrome lost the app's spacing");
            }
        }
    }
}

#[test]
fn a_host_theme_sends_no_window_theme_command() {
    // eframe's web backend cannot act on `ViewportCommand::SetTheme` and logs a warning for each one
    // into the host page's console; a canvas has no window decorations to theme anyway.
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(5));
    let mut sent = Vec::new();
    for dark in [true, false, true, false] {
        bridge.send(HostCommand::SetTheme(theme(dark, "#0f766e")));
        for _ in 0..3 {
            frames(&mut h, 1);
            sent.extend(viewport_commands(&h));
        }
        assert_eq!(Tokens::get(&h.ctx).dark, dark);
    }
    assert!(!sent.iter().any(|command| matches!(command, egui::ViewportCommand::SetTheme(_))), "{sent:?}");
}

#[test]
fn the_hosts_theme_wins_over_the_apps_own_dark_setting() {
    let (mut h, bridge) = embed_app();
    h.state_mut().ui.theme = wordcraft_ui_egui::theme::Appearance::Dark;
    h.state_mut().session.view.dark_mode = true;
    frames(&mut h, 2);
    bridge.send(HostCommand::SetTheme(theme(false, "#0f766e")));
    frames(&mut h, 3);
    assert!(!Tokens::get(&h.ctx).dark);
}

#[test]
fn set_theme_before_open_applies_to_the_first_frame() {
    let bridge = HostBridge::new();
    bridge.send(HostCommand::SetTheme(theme(true, "#7c3aed")));
    let app = WordApp::new(Session::new(Document::new()), Services::default()).with_host(bridge.clone(), EmbedProfile::View);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorder = seen.clone();
    let _harness = egui_kittest::Harness::builder().with_size(WINDOW).with_max_steps(12).build_ui_state(
        move |ui, app: &mut WordApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
            let tokens = Tokens::get(&ctx);
            recorder.borrow_mut().push((tokens.dark, tokens.accent, ctx.global_style().visuals.dark_mode));
        },
        app,
    );
    let violet = egui::Color32::from_rgb(0x7c, 0x3a, 0xed);
    assert_eq!(seen.borrow().first(), Some(&(true, violet, true)), "the first frame was drawn with the default theme");
}

#[test]
fn a_theme_with_a_bad_colour_changes_nothing_and_does_not_panic() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::SetTheme(theme(true, "#0f766e")));
    frames(&mut h, 2);
    for (accent, content) in [("teal", "#ffffff"), ("#0f766e", ""), ("#0f766e80", "#ffffff"), ("#12345", "#fff"), ("\u{1F600}\u{1F600}", "#ffffff")] {
        bridge.send(HostCommand::SetTheme(ThemeSpec { dark: false, accent: accent.into(), accent_content: content.into() }));
        frames(&mut h, 2);
        let tokens = Tokens::get(&h.ctx);
        assert!(tokens.dark, "{accent:?}/{content:?} changed the theme");
        assert_eq!(tokens.accent, egui::Color32::from_rgb(0x0f, 0x76, 0x6e));
    }
}

#[test]
fn the_page_indicator_keeps_its_text_and_uses_the_hosts_text_colour_on_the_accent() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(120));
    let pages = pages(&mut h);
    bridge.send(HostCommand::SetTheme(ThemeSpec { dark: true, accent: "#fde047".into(), accent_content: "#1c1917".into() }));
    frames(&mut h, 3);
    assert!(drawn_text(&h).contains(&format!("Page 1 of {pages}")), "{}", drawn_text(&h));
    let ink = egui::Color32::from_rgb(0x1c, 0x19, 0x17);
    let colour = shapes(&h).iter().find_map(|s| match s {
        egui::Shape::Text(t) if t.galley.text().starts_with("Page 1 of") => t.galley.job.sections.first().map(|section| section.format.color),
        _ => None,
    });
    assert_eq!(colour, Some(ink));
}

#[test]
fn the_theme_leaves_the_hosts_mark_colours_alone() {
    let (mut h, bridge) = embed_app();
    open_doc(&mut h, &bridge, &clauses(20));
    bridge.send(HostCommand::SetMarks(vec![mark("t1", "Clause 2.")]));
    frames(&mut h, 3);
    assert!(shapes(&h).iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == amber())), "fixture: the mark is painted");
    for dark in [true, false] {
        bridge.send(HostCommand::SetTheme(theme(dark, "#0f766e")));
        frames(&mut h, 3);
        assert!(shapes(&h).iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == amber())), "dark {dark}: the mark changed colour");
    }
}

fn luminance(colour: egui::Color32) -> f32 {
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.03928 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(colour.r()) + 0.7152 * linear(colour.g()) + 0.0722 * linear(colour.b())
}

fn contrast(first: egui::Color32, second: egui::Color32) -> f32 {
    let (high, low) = (luminance(first).max(luminance(second)), luminance(first).min(luminance(second)));
    (high + 0.05) / (low + 0.05)
}

/// `over` painted on the opaque `under`. `Color32` holds premultiplied channels.
fn blended(over: egui::Color32, under: egui::Color32) -> egui::Color32 {
    let keep = 1.0 - f32::from(over.a()) / 255.0;
    let channel = |top: u8, bottom: u8| (f32::from(top) + f32::from(bottom) * keep).round().min(255.0) as u8;
    egui::Color32::from_rgb(channel(over.r(), under.r()), channel(over.g(), under.g()), channel(over.b(), under.b()))
}

#[test]
fn the_selection_stays_visible_on_the_white_page_for_any_accent_in_both_themes() {
    // Pale accents (which come with dark text) included: the page is white in both themes.
    let accents = ["#0f766e", "#7c3aed", "#dc2626", "#1d4ed8", "#fde047", "#fef9c3", "#e0f2fe", "#a7f3d0", "#ffffff", "#000000"];
    for dark in [false, true] {
        for accent in accents {
            let spec = ThemeSpec {
                dark,
                accent: accent.into(),
                accent_content: if luminance(wordcraft_ui_egui::host::wire::parse_color(accent).unwrap()) > 0.4 {
                    "#111111".into()
                } else {
                    "#ffffff".into()
                },
            };
            let (accent_colour, content) = spec.colors().expect("opaque colours");
            let tokens = Tokens::resolve(dark, Some(AccentOverride { accent: accent_colour, content }));
            let on_page = blended(tokens.selection, egui::Color32::WHITE);
            assert!(contrast(on_page, egui::Color32::WHITE) >= 1.25, "dark {dark}, accent {accent}: selection {on_page:?} vanishes on the page");
            assert!(contrast(on_page, egui::Color32::BLACK) >= 7.0, "dark {dark}, accent {accent}: page text is hard to read when selected");
        }
    }
}

#[test]
fn clearing_the_accent_restores_the_apps_own_colours() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::SetTheme(theme(false, "#0f766e")));
    frames(&mut h, 2);
    assert!(wordcraft_ui_egui::theme::accent_override(&h.ctx).is_some());
    wordcraft_ui_egui::theme::set_accent_override(&h.ctx, None);
    assert_eq!(Tokens::get(&h.ctx).accent, Tokens::light().accent);
    assert_eq!(wordcraft_ui_egui::theme::accent_content(&h.ctx), egui::Color32::WHITE);
}

#[test]
fn opening_a_document_after_set_theme_keeps_the_theme() {
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::SetTheme(theme(true, "#0f766e")));
    frames(&mut h, 2);
    for doc in [clauses(5), clauses(120)] {
        open_doc(&mut h, &bridge, &doc);
        let tokens = Tokens::get(&h.ctx);
        assert!(tokens.dark && h.ctx.global_style().visuals.dark_mode, "an open reset the chrome to light");
        assert_eq!(tokens.accent, egui::Color32::from_rgb(0x0f, 0x76, 0x6e));
        assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    }
}

#[test]
fn a_mid_tone_accent_gets_a_dark_selection_and_wash_not_a_pale_one() {
    // #22c55e is 2.3:1 on white; white text on it would be the host's choice, and must not pull the wash toward white.
    let green = egui::Color32::from_rgb(0x22, 0xc5, 0x5e);
    for dark in [false, true] {
        let over = AccentOverride { accent: green, content: egui::Color32::WHITE };
        let tokens = Tokens::resolve(dark, Some(over));
        let on_page = blended(tokens.selection, egui::Color32::WHITE);
        assert!(contrast(on_page, egui::Color32::WHITE) >= 1.25, "dark {dark}: the selection vanishes on the page");
        assert!(luminance(over.on_white()) < luminance(green), "the page colour must be darker than the accent");
        assert!(contrast(over.on_white(), egui::Color32::WHITE) >= 3.0, "{:?}", over.on_white());
        // The wash is the same colour at a lower alpha.
        let tint = over.on_white();
        assert_eq!(tokens.checked, egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 0x30));
    }
    // An accent that is dark enough is used as it is.
    let teal = egui::Color32::from_rgb(0x0f, 0x76, 0x6e);
    assert_eq!(AccentOverride { accent: teal, content: egui::Color32::WHITE }.on_white(), teal);
}

#[test]
fn a_dark_chrome_keeps_comment_balloons_readable_on_their_white_cards() {
    let mut editing = Session::new(clauses(5));
    editing.run("select.text", &json!({"text": "Supplier", "occurrence": 3})).unwrap();
    editing.run("review.newComment", &json!({"text": "Check the supplier"})).unwrap();
    let (mut h, bridge) = embed_app();
    bridge.send(HostCommand::SetTheme(theme(true, "#0f766e")));
    open_doc(&mut h, &bridge, &editing.doc);
    frames(&mut h, 2);
    assert!(Tokens::get(&h.ctx).dark);
    let painted = shapes(&h);
    let area = egui::Color32::from_rgb(0xF3, 0xF3, 0xF3);
    let texts: Vec<_> = painted
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text().contains("Check the supplier") => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1, "the comment's text is drawn once");
    let body = &texts[0];
    let at = body.visual_bounding_rect().center();
    // The card is the white rectangle under the text.
    let card = painted
        .iter()
        .find_map(|s| match s {
            egui::Shape::Rect(r) if r.fill == egui::Color32::WHITE && r.rect.contains(at) && r.rect.width() < 400.0 && r.rect.height() < 200.0 => {
                Some(r.rect)
            }
            _ => None,
        })
        .expect("a white card under the balloon text");
    // Every text on the card (the author's name and the comment) is page ink.
    let mut on_card = 0;
    for shape in &painted {
        if let egui::Shape::Text(text) = shape
            && card.contains(text.visual_bounding_rect().center())
        {
            let colour = text.galley.job.sections.first().expect("a section").format.color;
            assert!(contrast(colour, egui::Color32::WHITE) >= 7.0, "{:?} on its white card: {colour:?}", text.galley.text());
            assert!(contrast(colour, area) >= 4.5, "{:?} on the markup area: {colour:?}", text.galley.text());
            on_card += 1;
        }
    }
    assert!(on_card >= 2, "the author and the comment: {on_card}");
    let bars: Vec<egui::Color32> = painted
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if (r.rect.width() - 4.0).abs() < 0.01 && r.rect.height() > 10.0 => Some(r.fill),
            _ => None,
        })
        .collect();
    assert!(!bars.is_empty(), "the author bar is drawn");
    for bar in bars {
        assert!(contrast(bar, egui::Color32::WHITE) >= 4.5 && contrast(bar, area) >= 4.5, "author colour {bar:?}");
    }
}

/// Contrast of every text on the selected comment balloon in `h` against the card it sits on.
fn selected_balloon_contrasts(h: &mut egui_kittest::Harness<'_, WordApp>) -> Vec<f32> {
    h.state_mut().session.sel = Selection::caret(Pos::body(2, 0));
    frames(h, 3);
    let painted = shapes(h);
    let body = painted
        .iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text().contains("Check the supplier") => Some(t.visual_bounding_rect()),
            _ => None,
        })
        .expect("the balloon is drawn");
    // The card: the last filled card-sized rectangle under the text (its shadow is painted first;
    // the markup area, under a translucent fill, is page-sized).
    let (card, fill) = painted
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.fill.a() > 0 && r.rect.contains(body.center()) && r.rect.width() < 400.0 && r.rect.height() < 200.0 => {
                Some((r.rect, r.fill))
            }
            _ => None,
        })
        .next_back()
        .expect("the balloon's card");
    let card_fill = blended(fill, egui::Color32::from_rgb(0xF3, 0xF3, 0xF3));
    painted
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) if card.contains(t.visual_bounding_rect().center()) => {
                let ink = t.galley.job.sections.first().expect("a section").format.color;
                Some(contrast(ink, card_fill))
            }
            _ => None,
        })
        .collect()
}

fn commented_document() -> Document {
    let mut editing = Session::new(clauses(5));
    editing.run("select.text", &json!({"text": "Supplier", "occurrence": 3})).unwrap();
    editing.run("review.newComment", &json!({"text": "Check the supplier"})).unwrap();
    editing.doc
}

#[test]
fn a_selected_balloon_is_readable_in_the_embed_and_in_the_full_apps_dark_mode() {
    // The embed, dark chrome, the host's accent (a pale and a dark one).
    for accent in ["#0f766e", "#fde047"] {
        let (mut h, bridge) = embed_app();
        bridge.send(HostCommand::SetTheme(theme(true, accent)));
        open_doc(&mut h, &bridge, &commented_document());
        let contrasts = selected_balloon_contrasts(&mut h);
        assert!(contrasts.len() >= 2, "{accent}: {contrasts:?}");
        assert!(contrasts.iter().all(|c| *c >= 4.5), "embed {accent}: {contrasts:?}");
    }
    // The full app in dark mode, no host accent: the card must not take the chrome's navy.
    let mut h = full_app(commented_document());
    h.state_mut().ui.theme = wordcraft_ui_egui::theme::Appearance::Dark;
    frames(&mut h, 3);
    assert!(Tokens::get(&h.ctx).dark);
    let contrasts = selected_balloon_contrasts(&mut h);
    assert!(contrasts.len() >= 2, "{contrasts:?}");
    assert!(contrasts.iter().all(|c| *c >= 4.5), "full app dark: {contrasts:?}");
}

#[test]
fn header_and_footer_label_text_reaches_normal_text_contrast_on_its_wash() {
    // Pale, mid-tone and dark accents; #ffffff and #f0f0f0 halve to 3:1 to 4.5:1, not enough for text.
    for accent in ["#ffffff", "#f0f0f0", "#fde047", "#22c55e", "#e0f2fe", "#0f766e", "#7c3aed", "#000000"] {
        let (accent_colour, content) = ThemeSpec { dark: false, accent: accent.into(), accent_content: "#ffffff".into() }.colors().unwrap();
        let over = AccentOverride { accent: accent_colour, content };
        let wash = over.wash();
        let text = over.on_white_text();
        assert!(contrast(text, wash) >= 4.5, "{accent}: label {text:?} on its wash {wash:?} is {}", contrast(text, wash));
        assert!(contrast(over.on_white(), egui::Color32::WHITE) >= 3.0, "{accent}: the line");
        assert!(luminance(text) <= luminance(over.on_white()) + 1e-6, "{accent}: the text variant is never paler than the line");
        // The wash is what the app paints behind the label, over the white page.
        let tokens = Tokens::resolve(false, Some(over));
        assert_eq!(blended(tokens.checked, egui::Color32::WHITE), wash, "{accent}");
    }
    // A teal that already clears 3:1 for the line is darkened for the text.
    let teal = AccentOverride { accent: egui::Color32::from_rgb(0x0f, 0x76, 0x6e), content: egui::Color32::WHITE };
    assert_eq!(teal.on_white(), teal.accent);
    assert_ne!(teal.on_white_text(), teal.accent, "teal is 4.2:1 on its own wash");
}
