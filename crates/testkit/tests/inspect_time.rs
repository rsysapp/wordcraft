//! embed: how long the save inspector takes for the work it charges. Each shape is built first, then
//! inspected alone and timed; the slowest nanoseconds per unit of work, times `MAX_WORK`, is the
//! longest any inspection can take on this machine. Slow and machine-dependent, so ignored by the
//! gates: run it in release with `cargo test --release -p wordcraft-testkit --test inspect_time --
//! --ignored --nocapture`.

use std::io::{Cursor, Write};
use std::time::Instant;

use wordcraft_docx::inspect::{MAX_WORK, inspect_measured};

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
const TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const DOCUMENT: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Rent.</w:t></w:r></w:p></w:body></w:document>"#;

/// A part just under 64 MiB, so three fill a side's 192 MiB.
const PART: usize = (64 << 20) - 4_096;

fn package(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(false);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in [
        ("[Content_Types].xml".to_string(), TYPES.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), ROOT_RELS.as_bytes().to_vec()),
        ("word/document.xml".to_string(), DOCUMENT.as_bytes().to_vec()),
    ]
    .iter()
    .chain(parts)
    {
        writer.start_file(name.as_str(), if name.starts_with("stored/") { stored } else { options }).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// A pseudo-random stream, the same on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, length: usize, alphabet: &[u8]) -> Vec<u8> {
        (0..length).map(|_| alphabet[(self.next() % alphabet.len() as u64) as usize]).collect()
    }
}

/// Three parts of `content` under `folder`, the last byte of the last differing between sides when
/// `differ`.
fn side(folder: &str, extension: &str, content: &[u8], last: u8) -> Vec<u8> {
    let parts: Vec<(String, Vec<u8>)> = (0..3)
        .map(|n| {
            let mut bytes = content.to_vec();
            if n == 2
                && let Some(byte) = bytes.last_mut()
            {
                *byte = last;
            }
            (format!("{folder}/part{n}.{extension}"), bytes)
        })
        .collect();
    package(&parts)
}

fn xml(text: &[u8]) -> Vec<u8> {
    let mut out = b"<data>".to_vec();
    out.extend_from_slice(text.get(..PART - 13).unwrap_or(text));
    out.extend_from_slice(b"</data>");
    out
}

/// Deflate's bits, least significant first; Huffman codes are written most significant first.
struct Bits {
    out: Vec<u8>,
    pending: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u64, count: u32) {
        self.pending |= value << self.count;
        self.count += count;
        while self.count >= 8 {
            self.out.push(self.pending as u8);
            self.pending >>= 8;
            self.count -= 8;
        }
    }

    fn code(&mut self, (code, length): (u64, u32)) {
        let reversed = (0..length).fold(0, |reversed, bit| reversed | (((code >> bit) & 1) << (length - 1 - bit)));
        self.put(reversed, length);
    }
}

/// Canonical Huffman codes from code lengths.
fn canonical(lengths: &[u32]) -> Vec<(u64, u32)> {
    let longest = *lengths.iter().max().unwrap() as usize;
    let mut counts = vec![0u64; longest + 1];
    for &length in lengths.iter().filter(|length| **length > 0) {
        counts[length as usize] += 1;
    }
    let mut next = vec![0u64; longest + 2];
    let mut code = 0;
    for bits in 1..=longest {
        code = (code + counts[bits - 1]) << 1;
        next[bits] = code;
    }
    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return (0, 0);
            }
            let code = next[length as usize];
            next[length as usize] += 1;
            (code, length)
        })
        .collect()
}

/// A deflate stream of about `size` bytes that gives nothing: empty blocks, each declaring code
/// tables that run to 15 bits (so the reader builds sub-tables), the dearest input per byte
/// measured.
fn empty_dynamic_blocks(size: usize) -> Vec<u8> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let mut literals = vec![0u32; 286];
    let mut distances = vec![0u32; 30];
    for n in 0..15 {
        literals[n] = n as u32 + 1;
        distances[n] = n as u32 + 1;
    }
    literals[256] = 15;
    distances[15] = 15;
    let mut lengths_code = [0u32; 19];
    for (n, symbol) in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 18].into_iter().enumerate() {
        lengths_code[symbol] = if n < 15 { 4 } else { 5 };
    }
    let codes = canonical(&lengths_code);
    let end_of_block = canonical(&literals)[256];
    let all: Vec<u32> = literals.iter().chain(&distances).copied().collect();
    let mut bits = Bits { out: Vec::with_capacity(size + 64), pending: 0, count: 0 };
    while bits.out.len() < size {
        bits.put(0, 1);
        bits.put(2, 2);
        bits.put(286 - 257, 5);
        bits.put(30 - 1, 5);
        bits.put(19 - 4, 4);
        for symbol in ORDER {
            bits.put(u64::from(lengths_code[symbol]), 3);
        }
        let mut at = 0;
        while at < all.len() {
            let run = all[at..].iter().take(138).take_while(|length| **length == 0).count();
            if run >= 11 {
                bits.code(codes[18]);
                bits.put(run as u64 - 11, 7);
                at += run;
            } else {
                bits.code(codes[all[at] as usize]);
                at += 1;
            }
        }
        bits.code(end_of_block);
    }
    // A final empty fixed block, padded to the byte.
    bits.put(1, 1);
    bits.put(1, 2);
    bits.put(0, 7);
    bits.put(0, 7);
    bits.out
}

/// `zip` with each entry under `stored/` turned into a deflate entry of its stored bytes that
/// inflates to nothing (its CRC and size zero, in its local header and its directory record).
fn deflating_to_nothing(mut zip: Vec<u8>) -> Vec<u8> {
    let u16_at = |zip: &[u8], at: usize| usize::from(u16::from_le_bytes([zip[at], zip[at + 1]]));
    let u32_at = |zip: &[u8], at: usize| u32::from_le_bytes([zip[at], zip[at + 1], zip[at + 2], zip[at + 3]]) as usize;
    let end = zip.len() - 22;
    let (count, mut at) = (u16_at(&zip, end + 10), u32_at(&zip, end + 16));
    for _ in 0..count {
        let name_length = u16_at(&zip, at + 28);
        let local = u32_at(&zip, at + 42);
        if zip[at + 46..at + 46 + name_length].starts_with(b"stored/") {
            for (header, method, crc, size) in [(at, 10, 16, 24), (local, 8, 14, 22)] {
                zip[header + method] = 8;
                zip[header + crc..header + crc + 4].fill(0);
                zip[header + size..header + size + 4].fill(0);
            }
        }
        at += 46 + name_length + u16_at(&zip, at + 30) + u16_at(&zip, at + 32);
    }
    zip
}

/// An XML part of about `PART` bytes: `head`, then `unit` repeated, then `tail`.
fn fill(head: &str, unit: &str, tail: &str) -> Vec<u8> {
    let room = PART.saturating_sub(head.len() + tail.len());
    let mut out = head.as_bytes().to_vec();
    out.extend_from_slice(&unit.as_bytes().repeat(room / unit.len().max(1)));
    out.extend_from_slice(tail.as_bytes());
    out
}

/// The same XML in three custom XML parts a side.
fn same_xml(content: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (side("customXml", "xml", content, *content.last().unwrap_or(&b'>')), side("customXml", "xml", content, *content.last().unwrap_or(&b'>')))
}

/// Shapes that each make one loop of the XML reader do the most it can for the bytes it is given.
fn xml_loops() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut shapes = Vec::new();
    let mut add = |name: &'static str, content: Vec<u8>| {
        let (base, edited) = same_xml(&content);
        shapes.push((name, base, edited));
    };
    add("XML: short comments", fill("<data>", "<!---->", "</data>"));
    add("XML: one long comment", fill("<data><!--", "x", "--></data>"));
    add("XML: processing instructions", fill("<data>", "<?p?>", "</data>"));
    add("XML: white space after the root", fill("<data/>", " ", ""));
    add("XML: line ends in text", fill("<data>", "\r\n", "</data>"));
    add("XML: empty CDATA pieces", fill("<data>", "<![CDATA[]]>", "</data>"));
    add("XML: CDATA pieces", fill("<data>", "<![CDATA[x]]>", "</data>"));
    add("XML: entity references", fill("<data>", "&lt;", "</data>"));
    add("XML: references in pieces", fill("<data>", "x&lt;", "</data>"));
    add("XML: long numeric reference", fill("<data>&#x", "0", "41;</data>"));
    add("XML: document type", fill("<!DOCTYPE data [", " ", "]><data/>"));
    add("XML: long declaration", fill("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"", "y", "\"?><data/>"));
    let attributes: String = (0..500).map(|n| format!(" a{n}=\"\"")).collect();
    add("XML: many short attributes", fill("<data>", &format!("<e{attributes}/>"), "</data>"));
    let keys: String = (0..500).map(|n| format!(" {}{n:03}=\"\"", "k".repeat(253))).collect();
    add("XML: long attribute names", fill("<data>", &format!("<e{keys}/>"), "</data>"));
    add("XML: references in an attribute value", fill("<data><e a=\"", "&lt;", "\"/></data>"));
    add("XML: line ends in an attribute value", fill("<data><e a=\"", "\r\n", "\"/></data>"));
    let mc = r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;
    add("XML: white space in a prefix list", fill(&format!("<data {mc}><e mc:Ignorable=\""), " ", "\"/></data>"));
    add(
        "XML: prefix list tokens",
        fill(&format!("<data {mc} xmlns:p=\"urn:p\">"), r#"<e mc:Ignorable="p p p p p p p p p p p p p p p p"/>"#, "</data>"),
    );
    let declarations = |from: usize, to: usize| -> String { (from..to).map(|n| format!(r#" xmlns:p{n}="urn:{n}""#)).collect() };
    add(
        "XML: names walking 1,024 declarations",
        fill(&format!("<data{}><s{}><t{}>", declarations(0, 500), declarations(500, 1_000), declarations(1_000, 1_024)), "<e/>", "</t></s></data>"),
    );
    add(
        "XML: attributes walking 1,024 declarations",
        fill(
            &format!("<data{}><s{}><t{}>", declarations(0, 500), declarations(500, 1_000), declarations(1_000, 1_024)),
            r#"<e p0:a="" p0:b="" p0:c=""/>"#,
            "</t></s></data>",
        ),
    );
    add("XML: nested start and end tags", fill("<data>", "<e><e><e><e></e></e></e></e>", "</data>"));
    shapes
}

fn shapes() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut shapes = Vec::new();
    let zeros = vec![0u8; PART];
    shapes.push(("zeros, binary", side("parts", "bin", &zeros, 0), side("parts", "bin", &zeros, 1)));
    let spaces = xml(&vec![b' '; PART]);
    shapes.push(("spaces, XML", side("customXml", "xml", &spaces, b'>'), side("customXml", "xml", &spaces, b'>')));
    let noise = random.bytes(PART, &(0..=255).collect::<Vec<u8>>());
    shapes.push(("random, stored, binary", side("stored", "bin", &noise, 0), side("stored", "bin", &noise, 1)));
    // Compressed data that gives nothing: charged for what it consumes.
    let empty = empty_dynamic_blocks(PART);
    shapes.push((
        "empty Huffman blocks, binary",
        deflating_to_nothing(side("stored", "bin", &empty, *empty.last().unwrap())),
        deflating_to_nothing(side("stored", "bin", &empty, *empty.last().unwrap())),
    ));
    let literals = random.bytes(PART, b"abcdefghijklmnop");
    shapes.push(("16-symbol literals, binary", side("parts", "bin", &literals, b'a'), side("parts", "bin", &literals, b'b')));
    let literal_xml = xml(&literals);
    shapes.push(("16-symbol literals, XML", side("customXml", "xml", &literal_xml, b'>'), side("customXml", "xml", &literal_xml, b'>')));
    let words: Vec<u8> = (0..PART / 4).flat_map(|_| random.bytes(3, b"abcdefghijklmnopqrstuvwxyz").into_iter().chain([b' '])).collect();
    shapes.push(("3-letter tokens, binary", side("parts", "bin", &words, b'a'), side("parts", "bin", &words, b'b')));
    let word_xml = xml(&words);
    shapes.push(("3-letter tokens, XML", side("customXml", "xml", &word_xml, b'>'), side("customXml", "xml", &word_xml, b'>')));
    let references: Vec<u8> = (0..(PART - 13) / 6).flat_map(|_| format!("&#x{:02X};", 0x41 + random.next() % 26).into_bytes()).collect();
    let reference_xml = xml(&references);
    shapes.push((
        "random hexadecimal references, XML",
        side("customXml", "xml", &reference_xml, b'>'),
        side("customXml", "xml", &reference_xml, b'>'),
    ));
    shapes
}

#[test]
#[ignore = "slow and machine-dependent: run in release, by hand"]
fn the_work_limit_bounds_the_time() {
    let mut worst = (0.0f64, "");
    let mut longest = (0.0f64, "");
    let only = std::env::var("INSPECT_TIME_ONLY").ok();
    for (name, base, edited) in shapes().into_iter().chain(xml_loops()).filter(|(name, _, _)| only.as_deref().is_none_or(|only| name.contains(only)))
    {
        let started = Instant::now();
        let (result, _, work) = inspect_measured(&base, &edited);
        let seconds = started.elapsed().as_secs_f64();
        let per_unit = seconds * 1e9 / work.max(1) as f64;
        eprintln!(
            "{name:<36} upload {:>6.1} MB  work {:>4} M  {seconds:>6.2} s  {per_unit:>6.1} ns/unit  {}",
            (base.len() + edited.len()) as f64 / 1e6,
            work / 1_000_000,
            match &result {
                Ok(_) => "Ok".to_string(),
                Err(error) => format!("{error:?}").chars().take(50).collect(),
            }
        );
        if per_unit > worst.0 {
            worst = (per_unit, name);
        }
        if seconds > longest.0 {
            longest = (seconds, name);
        }
    }
    eprintln!("slowest per unit: {:.1} ns ({}), so MAX_WORK takes at most {:.2} s", worst.0, worst.1, worst.0 * MAX_WORK as f64 / 1e9);
    eprintln!("longest inspection: {:.2} s ({})", longest.0, longest.1);
}
