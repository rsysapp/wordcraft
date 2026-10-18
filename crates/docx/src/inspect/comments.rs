//! embed: comments by meaning. Edit mode adds comments and replies, so the comments
//! parts change on a legitimate save; what matters is which comments were added, by whom, and
//! whether any comment of the base was removed or altered.

use std::collections::{BTreeMap, HashMap};

use super::budget::{Budget, table_room, table_slot, tree_slot, units, vector_slot};

/// Memory charged for each comment kept, besides its strings and identities.
const COMMENT_BYTES: u64 = tree_slot::<String, Comment>();
/// Memory charged for each entry of the paragraph map used while reading, besides its strings.
const MAP_BYTES: u64 = table_slot::<(String, Option<String>)>();
/// Memory charged for each attribute copied while a comment's attributes are compared, besides its
/// value: its entry, in a vector that grows by doubling.
const ATTRIBUTE_COPY_BYTES: u64 = vector_slot::<(std::rc::Rc<str>, String)>();
use super::digest::{Digest, Hasher};
use super::opc::{Canon, finish};
use super::story::kept_value;
use super::xml::{XEl, XNode, is_space};

/// One comment as compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Comment {
    pub author: String,
    /// Every attribute but the id (author, date, initials…), as a digest of their compared form.
    pub meta: Digest,
    /// Its paragraphs, as a digest of their compared form.
    pub content: Digest,
    /// Marked done (`w15:done`).
    pub done: bool,
    /// The comment this one answers (`w15:paraIdParent`).
    pub parent: Option<String>,
    /// embed: streams mode only (`None` for the save check): its date and plain text.
    pub detail: Option<Detail>,
}

/// embed: what streams mode shows of a comment besides what the save check compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Detail {
    pub date: Option<String>,
    /// The text of its paragraphs' `w:t` elements, paragraphs joined by a line end.
    pub text: String,
}

/// Comments by id.
pub(super) type Comments = BTreeMap<String, Comment>;

/// The comments of `w:comments`, with their state from `w15:commentsEx`.
pub(super) fn read(comments: &XEl, extended: Option<&XEl>, canon: &Canon<'_>, budget: &mut Budget) -> Result<Comments, String> {
    read_with(comments, extended, canon, budget, false)
}

/// embed: [`read`], keeping each comment's date and text when `detailed` (streams mode).
pub(super) fn read_with(comments: &XEl, extended: Option<&XEl>, canon: &Canon<'_>, budget: &mut Budget, detailed: bool) -> Result<Comments, String> {
    // The paragraph map lives only while reading: held, then released.
    budget.hold(table_room::<(String, Option<String>)>())?;
    let mut map_held = table_room::<(String, Option<String>)>();
    let read = read_held(comments, extended, canon, budget, &mut map_held, detailed);
    budget.release(map_held);
    read
}

fn read_held(
    comments: &XEl,
    extended: Option<&XEl>,
    canon: &Canon<'_>,
    budget: &mut Budget,
    map_held: &mut u64,
    detailed: bool,
) -> Result<Comments, String> {
    if &*comments.name != "w:comments" {
        return Err("a comments part without w:comments".into());
    }
    let mut out = Comments::new();
    // The last paragraph's id links a comment to its entry in commentsExtended; `None` when two
    // comments share one, which no reader could resolve.
    let mut by_paragraph: HashMap<String, Option<String>> = HashMap::new();
    for node in &comments.kids {
        let el = match node {
            XNode::Text(text) if is_space(text) => continue,
            XNode::Text(_) => return Err("text in the comments part".into()),
            XNode::El(el) if &*el.name == "w:comment" => el,
            XNode::El(el) => return Err(format!("{} in the comments part", el.name)),
        };
        let id = kept_value("a comment's id", el.attr("w:id").ok_or("a comment without an id")?)?;
        let author = kept_value("a comment's w:author", el.attr("w:author").unwrap_or(""))?;
        budget.work(200)?;
        // The attributes copied for the digest, held while it is made.
        let copied = el.attrs.iter().fold(0u64, |held, (_, value)| held.saturating_add(ATTRIBUTE_COPY_BYTES).saturating_add(units(value.len())));
        budget.hold(copied)?;
        let without_id =
            XEl { name: el.name.clone(), attrs: el.attrs.iter().filter(|(key, _)| &**key != "w:id").cloned().collect(), kids: Vec::new() };
        let meta = canon.start_digest(&without_id, budget);
        drop(without_id);
        budget.release(copied);
        let meta = meta?;
        let mut content = Hasher::default();
        for node in &el.kids {
            match node {
                XNode::El(child) => canon.element(child, &mut content),
                XNode::Text(text) if is_space(text) => {}
                XNode::Text(_) => return Err("text directly inside a comment".into()),
            }
        }
        let content = finish(content, budget)?;
        // Kept for the whole inspection: the record, its strings and identities, its map entry.
        let strings = id.len().saturating_add(author.len()).saturating_mul(2);
        budget.hold(COMMENT_BYTES.saturating_add(units(strings)).saturating_add(meta.held()).saturating_add(content.held()))?;
        if let Some(paragraph) = el.els().filter(|child| &*child.name == "w:p").last().and_then(|p| p.attr("w14:paraId")) {
            let paragraph = kept_value("a comment paragraph's w14:paraId", paragraph)?;
            let entry = MAP_BYTES.saturating_add(units(paragraph.len().saturating_add(id.len())));
            budget.hold(entry)?;
            *map_held = map_held.saturating_add(entry);
            by_paragraph.entry(paragraph.to_string()).and_modify(|entry| *entry = None).or_insert_with(|| Some(id.to_string()));
        }
        if out.contains_key(id) {
            return Err(format!("two comments with id {id}"));
        }
        let detail = if detailed { Some(detail_of(el, budget)?) } else { None };
        let comment = Comment { author: author.to_string(), meta, content, done: false, parent: None, detail };
        out.insert(id.to_string(), comment);
    }
    let Some(extended) = extended else { return Ok(out) };
    if &*extended.name != "w15:commentsEx" {
        return Err("a commentsExtended part without w15:commentsEx".into());
    }
    let resolve = |paragraph: &str| -> Result<String, String> {
        match by_paragraph.get(paragraph) {
            Some(Some(id)) => Ok(id.clone()),
            Some(None) => Err(format!("two comments end in paragraph {paragraph}")),
            None => Err(format!("a comment state for no comment ({paragraph})")),
        }
    };
    let mut stated = std::collections::BTreeSet::new();
    for node in &extended.kids {
        let el = match node {
            XNode::Text(text) if is_space(text) => continue,
            XNode::Text(_) => return Err("text in the commentsExtended part".into()),
            XNode::El(el) if &*el.name == "w15:commentEx" => el,
            XNode::El(el) => return Err(format!("{} in the commentsExtended part", el.name)),
        };
        if el.has_element_kids() || el.attrs.iter().any(|(key, _)| !matches!(&**key, "w15:paraId" | "w15:paraIdParent" | "w15:done")) {
            return Err("an unexpected comment state".into());
        }
        let id = resolve(el.attr("w15:paraId").ok_or("a comment state without w15:paraId")?)?;
        if !stated.insert(id.clone()) {
            return Err(format!("two states for comment {id}"));
        }
        let done = match el.attr("w15:done") {
            None | Some("0" | "false" | "off") => false,
            Some("1" | "true" | "on") => true,
            Some(other) => return Err(format!("a comment state done={other:?}")),
        };
        let parent = el.attr("w15:paraIdParent").map(resolve).transpose()?;
        if let Some(comment) = out.get_mut(&id) {
            comment.done = done;
            comment.parent = parent;
        }
    }
    Ok(out)
}

/// embed: a comment's date and plain text, for streams mode, charged as they are copied.
fn detail_of(comment: &XEl, budget: &mut Budget) -> Result<Detail, String> {
    fn texts(el: &XEl, out: &mut String, depth: usize, budget: &mut Budget) -> Result<(), String> {
        if depth > crate::xml::MAX_DEPTH {
            return Ok(());
        }
        for child in el.els() {
            budget.work(1)?;
            if &*child.name == "w:t" {
                for text in child.texts() {
                    budget.hold(units(text.len()))?;
                    out.push_str(text);
                }
            } else {
                texts(child, out, depth + 1, budget)?;
            }
        }
        Ok(())
    }
    let mut text = String::new();
    for (index, paragraph) in comment.els().filter(|child| &*child.name == "w:p").enumerate() {
        if index > 0 {
            text.push('\n');
        }
        texts(paragraph, &mut text, 0, budget)?;
    }
    let date = comment.attr("w:date").map(|date| kept_value("a comment's w:date", date).map(str::to_string)).transpose()?;
    budget.hold(units(date.as_ref().map_or(0, String::len)))?;
    Ok(Detail { date, text })
}
