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
