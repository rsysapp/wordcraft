//! embed: every structure the save inspector keeps holds no more than it charges for it. Each
//! shape builds one structure on its own (through `wordcraft_docx::inspect::probe`), sized so that
//! structure is what the shape holds, under a counting allocator: removing that structure's charge
//! makes its own shape fail. The shapes are small enough to run at full strength in CI.

use std::io::{Cursor, Write};

use wordcraft_docx::inspect::probe::{self, Entries, Measured, Phase};
use wordcraft_testkit::counting::{Counting, peak, reset_peak};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn zip(entries: &[(String, Vec<u8>)], compression: zip::CompressionMethod) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(compression);
    for (name, bytes) in entries {
        writer.start_file(name.as_str(), options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn story(body: &str) -> Vec<u8> {
    format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#).into_bytes()
}

fn run(text: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

fn inserted(date: usize, content: &str, kind: &str) -> String {
    format!(r#"<w:{kind} w:id="1" w:author="Ana" w:date="2026-01-01T00:00:{date:07}Z">{content}</w:{kind}>"#)
}

type Probe = Box<dyn Fn(&mut dyn FnMut()) -> Result<Measured, String>>;

/// The shapes: a name, the structure it isolates, and its probe.
fn shapes() -> Vec<(&'static str, Probe)> {
    let mut shapes: Vec<(&'static str, Probe)> = Vec::new();
    let tree = |xml: String| -> Probe { Box::new(move |mark| probe::tree(xml.as_bytes(), mark)) };

    shapes.push(("tree: elements", tree(format!("<r>{}</r>", "<e/>".repeat(200_000)))));
    shapes.push(("tree: nested elements", tree(format!("<r>{}</r>", format!("{}{}", "<e>".repeat(60), "</e>".repeat(60)).repeat(3_000)))));
    let attributes: String = (0..500).map(|n| format!(r#" a{n}="1""#)).collect();
    shapes.push(("tree: attributes", tree(format!("<r>{}</r>", format!("<e{attributes}/>").repeat(400)))));
    let values: String = (0..20).map(|n| format!(r#" v{n}="{}""#, "v".repeat(20_000))).collect();
    shapes.push(("tree: attribute values", tree(format!("<r>{}</r>", format!("<e{values}/>").repeat(20)))));
    shapes.push(("tree: text", tree(format!("<r><t>{}</t></r>", "x".repeat(8 << 20)))));
    shapes.push(("tree: CDATA text", tree(format!("<r><t><![CDATA[{}]]></t></r>", "x".repeat(8 << 20)))));
    shapes.push(("tree: text from references", tree(format!("<r><t>{}</t></r>", "x&amp;".repeat(1 << 20)))));
    let names: String = (0..2_000).map(|n| format!("<p:{}{n}/>", "n".repeat(2_000))).collect();
    shapes.push(("tree: names", tree(format!(r#"<r xmlns:p="urn:x">{names}</r>"#))));
    let namespaces: String = (0..2_000).map(|n| format!(r#"<a:e xmlns:a="urn:{}{n}"/>"#, "u".repeat(2_000))).collect();
    shapes.push(("tree: namespaces", tree(format!("<r>{namespaces}</r>"))));
    let declarations: String = (0..500).map(|n| format!(r#" xmlns:d{n}="urn:{}{n}""#, "d".repeat(4_000))).collect();
    shapes.push(("tree: open declarations", tree(format!("<r{declarations}><e/></r>"))));
    let tokens = vec![format!("p:{}", "a".repeat(60)); 500].join(" ");
    let lists = format!(r#"<e mc:ProcessContent="{tokens}"/>"#).repeat(100);
    shapes.push((
        "tree: prefix lists",
        tree(format!(r#"<r xmlns:p="urn:x" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">{lists}</r>"#)),
    ));

    let rels: String = (0..50_000)
        .map(|n| format!(r#"<Relationship Id="rId{n}" Type="http://example.com/t" Target="https://example.com/{n}" TargetMode="External"/>"#))
        .collect();
    let rels = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#);
    shapes.push(("relationships", Box::new(move |mark| probe::relationships(rels.as_bytes(), mark))));
    let types: String = (0..50_000).map(|n| format!(r#"<Override PartName="/x/{n}.xml" ContentType="application/x-{n}+xml"/>"#)).collect();
    let types = format!(r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">{types}</Types>"#);
    shapes.push(("content types", Box::new(move |mark| probe::content_types(types.as_bytes(), mark))));
    let comments: String = (0..20_000)
        .map(|n| format!(r#"<w:comment w:id="{n}" w:author="Ana"><w:p w14:paraId="{n:08X}" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">{}</w:p></w:comment>"#, run("Check this.")))
        .collect();
    let comments = format!(r#"<w:comments {W}>{comments}</w:comments>"#);
    shapes.push(("comments and their paragraph map", Box::new(move |mark| probe::comments(comments.as_bytes(), mark))));
    let styles: String = (0..20_000)
        .map(|n| format!(r#"<w:style w:type="character" w:styleId="S{n}"><w:basedOn w:val="S{}"/><w:rPr><w:vanish/></w:rPr></w:style>"#, n + 1))
        .collect();
    let styles = format!(r#"<w:styles {W}>{styles}</w:styles>"#);
    let copied = format!(
        r#"<w:comments {W}><w:comment w:id="1" w:author="Ana" w:initials="{}"><w:p>{}</w:p></w:comment></w:comments>"#,
        "i".repeat(8 << 20),
        run("Check.")
    );
    shapes.push(("comments: attributes copied", Box::new(move |mark| probe::comments(copied.as_bytes(), mark))));
    shapes.push(("styles", Box::new(move |mark| probe::styles(styles.as_bytes(), mark))));
    shapes.push(("revision pairs", Box::new(|mark| probe::revision_pairs(200_000, mark))));

    shapes.push(("table: short contents", Box::new(|mark| probe::table_entries(Entries::Contents(200), 100_000, mark))));
    shapes.push(("table: long contents", Box::new(|mark| probe::table_entries(Entries::Contents(4_000), 100_000, mark))));
    shapes.push(("table: texts", Box::new(|mark| probe::table_entries(Entries::Texts(200), 100_000, mark))));
    shapes.push(("table: stacks", Box::new(|mark| probe::table_entries(Entries::Stacks, 100_000, mark))));
    shapes.push(("table: format slots", Box::new(|mark| probe::table_entries(Entries::Formats(8), 100_000, mark))));
    shapes.push(("table: marks", Box::new(|mark| probe::table_entries(Entries::Marks(200), 100_000, mark))));
    shapes.push(("table: mark records", Box::new(|mark| probe::table_entries(Entries::Marks(4), 100_000, mark))));
    shapes.push(("table: comment anchor ids", Box::new(|mark| probe::table_entries(Entries::Anchors(2_000), 10_000, mark))));
    shapes.push(("table: contexts", Box::new(|mark| probe::table_entries(Entries::Contexts, 100_000, mark))));

    let long = story(&format!("<w:p>{}</w:p>", run(&"x".repeat(2 << 20))));
    shapes.push(("story items", Box::new(move |mark| probe::story(&long, mark))));
    let wide = story(&format!("<w:p>{}</w:p>", run(&"\u{4E2D}".repeat(1 << 20))));
    shapes.push(("story items moved to fit", Box::new(move |mark| probe::story(&wide, mark))));
    let words = story(&format!("<w:p>{}</w:p>", run(&"word ".repeat(400_000))));
    shapes.push((
        "contexts",
        Box::new({
            let words = words.clone();
            move |mark| probe::contexts(&words, mark)
        }),
    ));
    let changes: String = (0..20_000).map(|n| inserted(n, &run("x"), "ins")).collect();
    let changes = story(&format!("<w:p>{changes}</w:p>"));
    shapes.push((
        "projection caches",
        Box::new({
            let changes = changes.clone();
            move |mark| probe::projection(&changes, false, mark)
        }),
    ));
    let moves: String = (0..20_000)
        .map(|n| {
            format!(
                "{}{}",
                inserted(n, &run("x"), "moveFrom").replace("w:t ", "w:delText ").replace("</w:t>", "</w:delText>"),
                inserted(n, &run("y"), "moveTo")
            )
        })
        .collect();
    let moves = story(&format!("<w:p>{moves}</w:p>"));
    shapes.push(("facts: moves", Box::new(move |mark| probe::facts_of(&moves, mark))));
    shapes.push((
        "facts: stacks",
        Box::new({
            let changes = changes.clone();
            move |mark| probe::facts_of(&changes, mark)
        }),
    ));
    shapes.push((
        "fields: stacks",
        Box::new({
            let changes = changes.clone();
            move |mark| probe::fields_of(&changes, mark)
        }),
    ));
    shapes.push(("comparison: tokens", Box::new(move |mark| probe::comparison(&words, &words, mark))));
    let first: String = (0..1_900).map(|n| format!("a{n} ")).collect();
    let second: String = (0..1_900).map(|n| format!("b{n} ")).collect();
    let (first, second) = (story(&format!("<w:p>{}</w:p>", run(&first))), story(&format!("<w:p>{}</w:p>", run(&second))));
    shapes.push(("comparison: edit script", Box::new(move |mark| probe::comparison(&first, &second, mark))));
    // Steps are the script's own share where edits are few: its trace grows with their square.
    shapes.push(("comparison: edit script steps", Box::new(|mark| probe::edit_script(4, mark))));
    // Each story resolves 100 insertions of its own; the tally keeps them all while the next
    // story is compared.
    let stories: Vec<(Vec<u8>, Vec<u8>)> = (0..200)
        .map(|story_index| {
            let changes: String = (0..100).map(|n| inserted(story_index * 100 + n, &run("x"), "ins")).collect();
            (story(&format!("<w:p>{changes}</w:p>")), story(&format!("<w:p>{}</w:p>", run(&"x".repeat(100)))))
        })
        .collect();
    shapes.push(("comparison: tally", Box::new(move |mark| probe::tally(&stories, mark))));
    // Each story loses 100 elements of names of its own, each a loss of its own.
    let losses: Vec<(Vec<u8>, Vec<u8>)> = (0..200)
        .map(|story_index| {
            let marks: String = (0..100).map(|n| format!("<v:e{}{}/>", "n".repeat(100), story_index * 100 + n)).collect();
            (story(&format!(r#"<w:p xmlns:v="urn:v">{}{marks}</w:p>"#, run("x"))), story(&format!("<w:p>{}</w:p>", run("x"))))
        })
        .collect();
    shapes.push(("comparison: tally losses", Box::new(move |mark| probe::tally(&losses, mark))));

    let entries: Vec<(String, Vec<u8>)> = (0..20_000).map(|n| (format!("parts/part{n}.bin"), vec![b'x'; 8])).collect();
    let many = zip(&entries, zip::CompressionMethod::Deflated);
    shapes.push((
        "package directory",
        Box::new({
            let many = many.clone();
            move |mark| probe::package(&many, mark)
        }),
    ));
    shapes.push(("part names", Box::new(move |mark| probe::part_names(&many, mark))));
    shapes.push(("parts kept", Box::new(|mark| probe::parts_kept(20_000, mark))));
    let one = zip(&[("one.bin".to_string(), vec![b'x'; 16])], zip::CompressionMethod::Deflated);
    shapes.push(("package: one entry's stream", Box::new(move |mark| probe::package(&one, mark))));
    let big = zip(&[("big.bin".to_string(), vec![b'x'; 8 << 20])], zip::CompressionMethod::Deflated);
    shapes.push((
        "part read",
        Box::new({
            let big = big.clone();
            move |mark| probe::read_part(&big, "big.bin", mark)
        }),
    ));
    let small = zip(&[("small.bin".to_string(), vec![b'x'; 16])], zip::CompressionMethod::Deflated);
    shapes.push(("part read: the stream", Box::new(move |mark| probe::read_part(&small, "small.bin", mark))));
    shapes.push(("part compared", Box::new(move |mark| probe::compare_part(&big, &big, "big.bin", mark))));
    shapes
}

/// A hash table's growth point: one entry past the 7/8 of 2^k buckets it holds before it doubles.
const fn resize_point(k: u32) -> usize {
    7 * (1 << k) / 8 + 1
}

/// A vector's or string's growth point: one past 2^k.
const fn doubling_point(k: u32) -> usize {
    (1 << k) + 1
}

/// Shapes that end just past a collection's growth point, where it holds the most for what it
/// holds: a hash table holding its old and new buckets, a vector or string its old and new room.
fn growth_points() -> Vec<(&'static str, Probe)> {
    let mut shapes: Vec<(&'static str, Probe)> = Vec::new();
    let tree = |xml: String| -> Probe { Box::new(move |mark| probe::tree(xml.as_bytes(), mark)) };
    // A text node between elements: its slot in its parent's children, at the children's doubling.
    shapes.push(("tree: text-node slots", tree(format!("<r>{}<a/></r>", "<a/> ".repeat(1 << 16)))));
    shapes.push(("tree: empty CDATA nodes", tree(format!("<r>{}<a/></r>", "<a/><![CDATA[]]>".repeat(1 << 16)))));
    // Text built in pieces (between references), at its string's doubling point.
    shapes.push(("tree: text in pieces", tree(format!("<r><t>{}x</t></r>", "x&amp;".repeat(1 << 20)))));
    shapes.push(("tree: children", tree(format!("<r>{}</r>", "<a/>".repeat(doubling_point(17))))));
    let attributes: String = (0..257).map(|n| format!(r#" a{n}="1""#)).collect();
    shapes.push(("tree: attributes at a doubling point", tree(format!("<r>{}</r>", format!("<e{attributes}/>").repeat(400)))));
    // Distinct names: the root's, then `resize_point - 1` more.
    let names: String = (1..resize_point(15)).map(|n| format!("<e{n}/>")).collect();
    shapes.push(("tree: names at a resize point", tree(format!("<r>{names}</r>"))));
    let namespaces: String = (0..resize_point(15)).map(|n| format!(r#"<a:e xmlns:a="urn:{n}"/>"#)).collect();
    shapes.push(("tree: namespaces at a resize point", tree(format!("<r>{namespaces}</r>"))));
    // Long prefixes in the reader's buffer of open names, nested.
    let prefix = "p".repeat(4_000);
    let nested =
        format!(r#"<{prefix}:e xmlns:{prefix}="urn:x">{}{}</{prefix}:e>"#, format!("<{prefix}:e>").repeat(60), format!("</{prefix}:e>").repeat(60));
    shapes.push(("tree: open names", tree(format!("<r>{}</r>", nested.repeat(20)))));

    for (name, entries, count) in [
        ("table: contents at a resize point", Entries::Contents(8), resize_point(16)),
        ("table: texts at a resize point", Entries::Texts(8), resize_point(16) - 1),
        ("table: texts at a doubling point", Entries::Texts(8), doubling_point(16) - 1),
        ("table: stacks at a resize point", Entries::Stacks, resize_point(16)),
        ("table: stacks at a doubling point", Entries::Stacks, doubling_point(16)),
        ("table: formats at a resize point", Entries::Formats(1), resize_point(16) - 1),
        ("table: formats at a doubling point", Entries::Formats(1), doubling_point(16) - 1),
        ("table: marks at a resize point", Entries::Marks(4), resize_point(16)),
        ("table: marks at a doubling point", Entries::Marks(4), doubling_point(16)),
        ("table: contexts at a resize point", Entries::Contexts, resize_point(16) - 1),
        ("table: contexts at a doubling point", Entries::Contexts, doubling_point(16) - 1),
    ] {
        let count = u32::try_from(count).unwrap();
        shapes.push((name, Box::new(move |mark| probe::table_entries(entries.clone(), count, mark))));
    }

    let styles: String = (0..resize_point(15))
        .map(|n| format!(r#"<w:style w:type="character" w:styleId="S{n}"><w:basedOn w:val="S{}"/><w:rPr><w:vanish/></w:rPr></w:style>"#, n + 1))
        .collect();
    let styles = format!(r#"<w:styles {W}>{styles}</w:styles>"#);
    shapes.push(("styles at a resize point", Box::new(move |mark| probe::styles(styles.as_bytes(), mark))));
    let rels: String = (0..resize_point(15)).map(|n| format!(r#"<Relationship Id="r{n}" Type="t" Target="x" TargetMode="External"/>"#)).collect();
    let rels = format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#);
    shapes.push(("relationships at a resize point", Box::new(move |mark| probe::relationships(rels.as_bytes(), mark))));
    let marks: String = (0..resize_point(15)).map(|n| format!(r#"<w:bookmarkStart w:id="{n}" w:name="b"/>"#)).collect();
    let marks = story(&format!("<w:p>{marks}</w:p>"));
    shapes.push(("bookmarks at a resize point", Box::new(move |mark| probe::bookmarks(&marks, mark))));
    let long_marks: String = (0..2_000).map(|n| format!(r#"<w:bookmarkStart w:id="{n}" w:name="{}"/>"#, "b".repeat(4_000))).collect();
    let long_marks = story(&format!("<w:p>{long_marks}</w:p>"));
    shapes.push(("bookmarks: long names", Box::new(move |mark| probe::bookmarks(&long_marks, mark))));
    let comments: String = (0..resize_point(15))
        .map(|n| format!(r#"<w:comment w:id="{n}" w:author="A"><w:p w14:paraId="{n:08X}" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"/></w:comment>"#))
        .collect();
    let comments = format!(r#"<w:comments {W}>{comments}</w:comments>"#);
    shapes.push(("comments at a resize point", Box::new(move |mark| probe::comments(comments.as_bytes(), mark))));

    let changes: String = (0..resize_point(15)).map(|n| inserted(n, &run("x"), "ins")).collect();
    let changes = story(&format!("<w:p>{changes}</w:p>"));
    for (name, phase) in [("projection caches at a resize point", 0), ("facts: stacks at a resize point", 1), ("fields: stacks at a resize point", 2)]
    {
        let changes = changes.clone();
        shapes.push((
            name,
            Box::new(move |mark| match phase {
                0 => probe::projection(&changes, false, mark),
                1 => probe::facts_of(&changes, mark),
                _ => probe::fields_of(&changes, mark),
            }),
        ));
    }
    // Each insertion by an author of its own: the facts kept per author.
    let authors: String =
        (0..20_000).map(|n| format!(r#"<w:ins w:id="1" w:author="A{n}" w:date="2026-01-01T00:00:00Z">{}</w:ins>"#, run("x"))).collect();
    let authors = story(&format!("<w:p>{authors}</w:p>"));
    shapes.push((
        "facts: authors",
        Box::new({
            let authors = authors.clone();
            move |mark| probe::facts_of(&authors, mark)
        }),
    ));
    let fields: String =
        (0..20_000).map(|n| format!(r#"<w:ins w:id="1" w:author="A{n}" w:date="2026-01-01T00:00:00Z">{}</w:ins>"#, run("x"))).collect();
    let field =
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;
    let fields = story(&format!(r#"<w:p>{field}{fields}<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#));
    shapes.push(("fields: authors", Box::new(move |mark| probe::fields_of(&fields, mark))));
    shapes.push(("comparison: tokens at a doubling point", Box::new(|mark| probe::tokens(doubling_point(18), mark))));
    let entries: Vec<(String, Vec<u8>)> = (0..doubling_point(14)).map(|n| (format!("p/{n}.bin"), vec![b'x'; 1])).collect();
    let doubled = zip(&entries, zip::CompressionMethod::Stored);
    shapes.push(("package directory at a doubling point", Box::new(move |mark| probe::package(&doubled, mark))));
    let entries: Vec<(String, Vec<u8>)> = (0..1_000).map(|n| (format!("p/{}{n}.bin", "n".repeat(30_000)), vec![b'x'; 1])).collect();
    let long = zip(&entries, zip::CompressionMethod::Stored);
    shapes.push(("package directory: long names", Box::new(move |mark| probe::package(&long, mark))));
    shapes
}

const MAIN: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
const RELS: &str = r#"xmlns="http://schemas.openxmlformats.org/package/2006/relationships""#;

/// A package: its main document, the content type overrides and document relationships given,
/// and other entries.
fn docx(document: Vec<u8>, overrides: &str, document_rels: &str, extra: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let types = format!(
        r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="{MAIN}"/>{overrides}</Types>"#
    );
    let root = format!(
        r#"<Relationships {RELS}><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), types.into_bytes()),
        ("_rels/.rels".to_string(), root.into_bytes()),
        ("word/document.xml".to_string(), document),
    ];
    if !document_rels.is_empty() {
        entries.push(("word/_rels/document.xml.rels".to_string(), format!("<Relationships {RELS}>{document_rels}</Relationships>").into_bytes()));
    }
    entries.extend(extra);
    zip(&entries, zip::CompressionMethod::Deflated)
}

fn plain() -> Vec<u8> {
    story(&format!("<w:p>{}</w:p>", run("Rent.")))
}

fn binary_parts(prefix: &str, count: usize) -> Vec<(String, Vec<u8>)> {
    (0..count).map(|n| (format!("parts/{prefix}{n}.bin"), vec![b'x'; 8])).collect()
}

/// A package with a comments part holding these comments (id, author, text).
fn with_comments(comments: &[(String, String, &str)]) -> Vec<u8> {
    let comments: String = comments
        .iter()
        .map(|(id, author, text)| format!(r#"<w:comment w:id="{id}" w:author="{author}"><w:p>{}</w:p></w:comment>"#, run(text)))
        .collect();
    docx(
        plain(),
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#,
        r#"<Relationship Id="rC" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#,
        vec![("word/comments.xml".to_string(), format!(r#"<w:comments {W}>{comments}</w:comments>"#).into_bytes())],
    )
}

fn hyperlinks(host: &str, count: usize) -> String {
    (0..count)
        .map(|n| format!(r#"<Relationship Id="rH{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://{host}.example/{}{n}" TargetMode="External"/>"#, "p".repeat(100)))
        .collect()
}

/// The comparison's steps, each between two packages read first.
fn compared() -> Vec<(&'static str, Probe)> {
    let mut shapes: Vec<(&'static str, Probe)> = Vec::new();
    let phase =
        |base: Vec<u8>, edited: Vec<u8>, phase: fn() -> Phase| -> Probe { Box::new(move |mark| probe::compared(&base, &edited, phase(), mark)) };
    let same = docx(plain(), "", "", binary_parts("same", 19_990));
    shapes.push(("parts compared: unchanged", phase(same.clone(), same, || Phase::Parts)));
    let few = docx(plain(), "", "", Vec::new());
    let many = docx(plain(), "", "", binary_parts("extra", 10_000));
    shapes.push(("parts compared: added", phase(few.clone(), many.clone(), || Phase::Parts)));
    shapes.push(("parts compared: missing", phase(many, few, || Phase::Parts)));
    let altered = |byte: u8| docx(plain(), "", "", (0..10_000).map(|n| (format!("parts/a{n}.bin"), vec![byte; 8])).collect());
    shapes.push(("parts compared: altered", phase(altered(b'x'), altered(b'y'), || Phase::Parts)));
    let typed = |kind: &str| {
        let overrides: String = (0..5_000)
            .map(|n| format!(r#"<Override PartName="/parts/t{n}.bin" ContentType="application/x-{kind}{}-{n}"/>"#, "t".repeat(200)))
            .collect();
        docx(plain(), &overrides, "", binary_parts("t", 5_000))
    };
    shapes.push(("parts compared: content types", phase(typed("a"), typed("b"), || Phase::Parts)));
    let headers = {
        let overrides: String = (0..5_000)
            .map(|n| {
                format!(
                    r#"<Override PartName="/word/h{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#
                )
            })
            .collect();
        let parts = (0..5_000).map(|n| (format!("word/h{n}.xml"), format!(r#"<w:hdr {W}><w:p/></w:hdr>"#).into_bytes())).collect();
        docx(plain(), &overrides, "", parts)
    };
    shapes.push(("parts compared: stories", phase(headers.clone(), headers, || Phase::Parts)));

    let sources = {
        let parts = (0..10_000)
            .map(|n| (format!("parts/_rels/s{n}.bin.rels"), format!(r#"<Relationships {RELS}><Relationship Id="r" Type="urn:t" Target="https://example.com/" TargetMode="External"/></Relationships>"#).into_bytes()))
            .collect();
        docx(plain(), "", "", parts)
    };
    shapes.push(("relationships compared: sources", phase(sources.clone(), sources, || Phase::Relationships)));
    let links = docx(plain(), "", &hyperlinks("a", 20_000), Vec::new());
    shapes.push(("relationships compared: sets", phase(links.clone(), links.clone(), || Phase::Relationships)));
    let other = docx(plain(), "", &hyperlinks("b", 20_000), Vec::new());
    shapes.push(("relationships compared: changes", phase(links, other, || Phase::Relationships)));

    let ids = |prefix: &str| -> Vec<(String, String, &'static str)> {
        (0..10_000).map(|n| (format!("{prefix}{}{n}", "i".repeat(100)), "Ana".to_string(), "Check.")).collect()
    };
    shapes.push(("comments compared: ids", phase(with_comments(&ids("a")), with_comments(&ids("b")), || Phase::Comments)));
    let authors: Vec<(String, String, &str)> = (0..10_000).map(|n| (format!("{n}"), format!("{}{n}", "a".repeat(200)), "Check.")).collect();
    shapes.push(("comments compared: authors", phase(with_comments(&[]), with_comments(&authors), || Phase::Comments)));
    let texts = |text: &'static str| -> Vec<(String, String, &'static str)> {
        (0..10_000).map(|n| (format!("{}{n}", "c".repeat(100)), "Ana".to_string(), text)).collect()
    };
    shapes.push(("comments compared: altered", phase(with_comments(&texts("Check.")), with_comments(&texts("Changed.")), || Phase::Comments)));

    let revised = |author: &dyn Fn(usize) -> String, date: &dyn Fn(usize) -> String| -> Vec<u8> {
        let changes: String =
            (0..10_000).map(|n| format!(r#"<w:ins w:id="1" w:author="{}" w:date="{}">{}</w:ins>"#, author(n), date(n), run("x"))).collect();
        docx(story(&format!("<w:p>{changes}</w:p>")), "", "", Vec::new())
    };
    let base = docx(plain(), "", "", Vec::new());
    let dated = revised(&|_| "Ana".to_string(), &|n| format!("2026-01-01T00:00:{n:07}Z"));
    shapes.push(("revisions compared: pairs", phase(base.clone(), dated, || Phase::Revisions)));
    let authored = revised(&|n| format!("{}{n}", "a".repeat(200)), &|_| "2026-01-01T00:00:00Z".to_string());
    shapes.push(("revisions compared: authors", phase(base.clone(), authored, || Phase::Revisions)));
    let long_dates = revised(&|n| format!("A{n}"), &|n| format!("2026-01-01T{}{n}", "0".repeat(2_000)));
    shapes.push(("revisions compared: dates", phase(base, long_dates, || Phase::Revisions)));
    shapes
}

/// A whole inspection of many small parts: what each part's entries keep.
fn parts_inspection() -> Probe {
    let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut entries = vec![
        ("[Content_Types].xml".to_string(), types.as_bytes().to_vec()),
        ("_rels/.rels".to_string(), root.as_bytes().to_vec()),
        ("word/document.xml".to_string(), story(&format!("<w:p>{}</w:p>", run("Rent.")))),
    ];
    entries.extend((0..19_990).map(|n| (format!("parts/part{n}.bin"), vec![b'x'; 8])));
    let package = zip(&entries, zip::CompressionMethod::Deflated);
    Box::new(move |mark| probe::inspection(&package, &package, mark))
}

#[test]
fn each_structure_holds_no_more_than_its_charge() {
    let mut shapes = shapes();
    shapes.extend(compared());
    shapes.extend(growth_points());
    shapes.push(("inspection: many parts", parts_inspection()));
    let mut over = Vec::new();
    let mut worst = (0.0f64, "");
    for (name, probe) in shapes {
        let mut baseline = 0usize;
        let measured = probe(&mut || baseline = reset_peak()).unwrap_or_else(|error| panic!("{name}: {error}"));
        let held = peak().saturating_sub(baseline) as u64;
        let ratio = held as f64 / measured.charged.max(1) as f64;
        eprintln!("{name:<36} held {:>9.3} MB  charged {:>9.3} MB  ratio {ratio:.4}", held as f64 / 1e6, measured.charged as f64 / 1e6);
        if ratio > worst.0 {
            worst = (ratio, name);
        }
        if held > measured.charged {
            over.push(name);
        }
        drop(measured);
    }
    eprintln!("largest held/charged ratio: {:.4} ({})", worst.0, worst.1);
    assert!(over.is_empty(), "held more than charged: {over:?}");
}
