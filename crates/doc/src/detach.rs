//! embed: content brought into a document from elsewhere (another file, the clipboard, a stored
//! Quick Part, a merge record) claims no place in the package the document was read from. An
//! [`Origin`](crate::Origin) names a position in one package; a paragraph carrying one that is not
//! its own would let a writer that keeps the package rewrite the wrong paragraph. Every such path
//! goes through [`Paragraph::detached`], which the block and fragment forms below walk to.

use std::sync::Arc;

use crate::edit::Fragment;
use crate::{Block, Paragraph};

impl Paragraph {
    /// The paragraph as content new to whichever document it goes into: no origin.
    pub fn detached(mut self) -> Paragraph {
        self.origin = None;
        self
    }
}

impl Block {
    /// The block with every paragraph in it, table cells included, [`Paragraph::detached`].
    pub fn detached(self) -> Block {
        match self {
            Block::Para(paragraph) => Block::Para(paragraph.detached()),
            Block::Table(mut table) => {
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        for block in &mut cell.blocks {
                            let inner = Arc::make_mut(block);
                            *inner = std::mem::replace(inner, Block::Para(Paragraph::new())).detached();
                        }
                    }
                }
                Block::Table(table)
            }
        }
    }
}

impl Fragment {
    /// The fragment with every block [`Block::detached`], the text box stories it carries included.
    pub fn detached(self) -> Fragment {
        let Fragment { blocks, mut parts } = self;
        for part in parts.values_mut() {
            for block in &mut part.blocks {
                let inner = Arc::make_mut(block);
                *inner = std::mem::replace(inner, Block::Para(Paragraph::new())).detached();
            }
        }
        Fragment { blocks: blocks.into_iter().map(Block::detached).collect(), parts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Cell, Table};
    use crate::{CharProps, Origin, para_block};

    fn read_from(index: u32, text: &str) -> Paragraph {
        let mut paragraph = Paragraph::with_text(text, CharProps::default());
        paragraph.origin = Some(Origin { part: 3, index });
        paragraph
    }

    #[test]
    fn detaches_paragraphs_inside_nested_tables() {
        let mut inner = Table::new(1, 1, 100.0);
        inner.rows[0].cells[0] = Cell { blocks: vec![para_block(read_from(2, "Inner"))], ..Cell::empty() };
        let mut outer = Table::new(1, 1, 200.0);
        outer.rows[0].cells[0] = Cell { blocks: vec![para_block(read_from(1, "Outer")), Arc::new(Block::Table(inner))], ..Cell::empty() };
        let fragment = Fragment { blocks: vec![Block::Para(read_from(0, "Before")), Block::Table(outer)], ..Default::default() }.detached();
        let mut doc = crate::Document::new();
        doc.body = fragment.blocks.into_iter().map(Arc::new).collect();
        let paths = doc.para_paths(crate::StoryRef::Body);
        let texts: Vec<_> = paths.iter().filter_map(|path| doc.para(crate::StoryRef::Body, path)).map(|p| (p.plain_text(), p.origin)).collect();
        assert_eq!(texts, [("Before".to_string(), None), ("Outer".to_string(), None), ("Inner".to_string(), None)]);
    }

    // embed: a fragment's text box stories become new parts too.
    #[test]
    fn detaches_the_text_box_stories_a_fragment_carries() {
        let story = crate::Part { kind: crate::PartKind::TextBox, blocks: vec![para_block(read_from(4, "Boxed"))], body: Default::default() };
        let fragment = Fragment { blocks: vec![Block::Para(read_from(0, "Before"))], parts: [(7, story)].into_iter().collect() }.detached();
        let origins: Vec<_> =
            fragment.parts.values().flat_map(|part| part.blocks.iter()).filter_map(|block| block.as_para()).map(|p| p.origin).collect();
        assert_eq!(origins, [None]);
        assert_eq!(fragment.blocks.first().and_then(Block::as_para).map(|p| p.origin), Some(None));
    }
}
