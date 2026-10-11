//! embed: comments by meaning (Ruling W1-4). Edit mode adds comments and replies, so the comments
//! parts change on a legitimate save; what matters is which comments were added, by whom, and
//! whether any comment of the base was removed or altered.

use std::collections::{BTreeMap, HashMap};

use super::opc::Canon;
use super::xml::{XEl, XNode, is_space};

/// One comment as compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Comment {
    pub author: String,
    /// Every attribute but the id (author, date, initials…), canonical.
    pub meta: String,
    /// Its paragraphs, canonical.
    pub content: String,
    /// Marked done (`w15:done`).
    pub done: bool,
    /// The comment this one answers (`w15:paraIdParent`).
    pub parent: Option<String>,
}

/// Comments by id.
pub(super) type Comments = BTreeMap<String, Comment>;

/// The comments of `w:comments`, with their state from `w15:commentsEx`.
pub(super) fn read(comments: &XEl, extended: Option<&XEl>, canon: &Canon<'_>) -> Result<Comments, String> {
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
        let id = el.attr("w:id").ok_or("a comment without an id")?.to_string();
        let without_id =
            XEl { name: el.name.clone(), attrs: el.attrs.iter().filter(|(key, _)| &**key != "w:id").cloned().collect(), kids: Vec::new() };
        let mut meta = String::new();
        canon.start_tag(&without_id, &mut meta);
        let mut content = String::new();
        for node in &el.kids {
            match node {
                XNode::El(child) => canon.element(child, &mut content),
                XNode::Text(text) if is_space(text) => {}
                XNode::Text(_) => return Err("text directly inside a comment".into()),
            }
        }
        if let Some(paragraph) = el.els().filter(|child| &*child.name == "w:p").last().and_then(|p| p.attr("w14:paraId")) {
            by_paragraph.entry(paragraph.to_string()).and_modify(|entry| *entry = None).or_insert_with(|| Some(id.clone()));
        }
        let comment = Comment { author: el.attr("w:author").unwrap_or("").to_string(), meta, content, done: false, parent: None };
        if out.insert(id.clone(), comment).is_some() {
            return Err(format!("two comments with id {id}"));
        }
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
