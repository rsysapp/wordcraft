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
    /// The fragment with every block [`Block::detached`].
    pub fn detached(self) -> Fragment {
        Fragment { blocks: self.blocks.into_iter().map(Block::detached).collect() }
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
        let fragment = Fragment { blocks: vec![Block::Para(read_from(0, "Before")), Block::Table(outer)] }.detached();
        let mut doc = crate::Document::new();
        doc.body = fragment.blocks.into_iter().map(Arc::new).collect();
        let paths = doc.para_paths(crate::StoryRef::Body);
        let texts: Vec<_> = paths.iter().filter_map(|path| doc.para(crate::StoryRef::Body, path)).map(|p| (p.plain_text(), p.origin)).collect();
        assert_eq!(texts, [("Before".to_string(), None), ("Outer".to_string(), None), ("Inner".to_string(), None)]);
    }
}
