//! Properties of reachable indirect structure elements.

use pdfcraft_cos::{CosError, Document, ObjRef, Object, PdfString};

use crate::alt;

/// A tag that can be addressed by an edit. Inline dictionaries have no stable object reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagInfo {
    pub object: ObjRef,
    /// Nearest reachable indirect parent, if any. Inline ancestors are skipped.
    pub parent: Option<ObjRef>,
    /// Indentation level among reachable indirect tags.
    pub depth: usize,
    pub kind: String,
    pub title: Option<String>,
    pub actual_text: Option<String>,
    pub alt: Option<String>,
    pub language: Option<String>,
    /// Zero-based page when `/Pg` is known.
    pub page: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagProperty {
    Type,
    Title,
    ActualText,
    AlternateText,
    Language,
}

/// Where to place a structure tag relative to one of its direct siblings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagPlacement {
    Before,
    After,
}

/// A custom structure type and the standard type to which it maps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleMapEntry {
    pub custom: String,
    pub standard: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RoleMapError {
    #[error("the document has no tags")]
    Untagged,
    #[error("the structure tree must be an indirect object")]
    DirectStructureTree,
    #[error("the role map is not a dictionary")]
    InvalidRoleMap,
    #[error("a role name must be a nonempty PDF name using letters, digits, colon, period, underscore or hyphen")]
    InvalidName,
    #[error("a role map target must be a standard PDF structure type")]
    InvalidTarget,
    #[error(transparent)]
    Cos(#[from] CosError),
}

impl TagProperty {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "type" => Some(Self::Type),
            "title" => Some(Self::Title),
            "actual_text" => Some(Self::ActualText),
            "alternate_text" => Some(Self::AlternateText),
            "language" => Some(Self::Language),
            _ => None,
        }
    }

    fn key(self) -> &'static [u8] {
        match self {
            Self::Type => b"S",
            Self::Title => b"T",
            Self::ActualText => b"ActualText",
            Self::AlternateText => b"Alt",
            Self::Language => b"Lang",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TagError {
    #[error("object {0} is not a reachable indirect structure tag")]
    NotATag(ObjRef),
    #[error("a tag type must be a nonempty PDF name using letters, digits, colon, period, underscore or hyphen")]
    InvalidType,
    #[error("tag property text is too long (maximum 65536 bytes)")]
    TooLong,
    #[error("both tags must be distinct, reachable, direct children of the same structure parent")]
    NotSiblings,
    #[error("the parent must list each tag exactly once in its /K array")]
    AmbiguousChildren,
    #[error("the tag is already at the requested position")]
    AlreadyThere,
    #[error(transparent)]
    Cos(#[from] CosError),
}

/// Reorder two direct sibling tags in their parent's `/K` array. Content and page appearance
/// stay untouched; tagged reading order follows the new array order.
pub fn move_tag(doc: &mut Document, object: ObjRef, anchor: ObjRef, placement: TagPlacement) -> Result<(), TagError> {
    if object == anchor {
        return Err(TagError::NotSiblings);
    }
    let reachable = alt::elements(doc);
    if !reachable.iter().any(|e| e.object == object) || !reachable.iter().any(|e| e.object == anchor) {
        return Err(TagError::NotSiblings);
    }
    let parent_of = |r| doc.get(r).as_dict().and_then(|d| d.get(b"P").and_then(Object::as_ref));
    let Some(parent) = parent_of(object) else { return Err(TagError::NotSiblings) };
    if parent_of(anchor) != Some(parent) {
        return Err(TagError::NotSiblings);
    }
    let owner = doc.get(parent);
    let Some(children) = owner.as_dict().and_then(|d| d.get(b"K").and_then(Object::as_array)) else {
        return Err(TagError::AmbiguousChildren);
    };
    let one_position = |child| {
        let mut matches = children.iter().enumerate().filter(|(_, item)| item.as_ref() == Some(child));
        matches.next().and_then(|(index, _)| matches.next().is_none().then_some(index))
    };
    let (Some(from), Some(to)) = (one_position(object), one_position(anchor)) else { return Err(TagError::AmbiguousChildren) };
    let mut reordered = children.clone();
    let moved = reordered.remove(from);
    let target = if from < to { to - 1 } else { to };
    let destination = target + usize::from(placement == TagPlacement::After);
    if destination == from {
        return Err(TagError::AlreadyThere);
    }
    reordered.insert(destination, moved);
    doc.update_dict(parent, |d| d.set(b"K".to_vec(), Object::Array(reordered)))?;
    Ok(())
}

fn text(doc: &Document, d: &pdfcraft_cos::Dict, key: &[u8]) -> Option<String> {
    d.get(key).map(|o| doc.resolve(o)).and_then(|o| o.as_string().map(PdfString::to_text))
}

/// List reachable, indirect tags in structure-tree order.
pub fn tags(doc: &Document) -> Vec<TagInfo> {
    let pages = pdfcraft_model::pages(doc);
    let page_index: std::collections::HashMap<ObjRef, usize> = pages.iter().enumerate().map(|(i, p)| (p.obj, i)).collect();
    alt::elements(doc)
        .into_iter()
        .filter_map(|e| {
            Some(TagInfo {
                object: e.object,
                parent: e.parent,
                depth: e.depth,
                kind: String::from_utf8_lossy(e.dict.name(b"S")?).into_owned(),
                title: text(doc, &e.dict, b"T"),
                actual_text: text(doc, &e.dict, b"ActualText"),
                alt: text(doc, &e.dict, b"Alt"),
                language: text(doc, &e.dict, b"Lang"),
                page: e.page.and_then(|p| page_index.get(&p).copied()),
            })
        })
        .collect()
}

/// Change one property of a reachable tag; `None` removes a text property.
pub fn set_tag_property(doc: &mut Document, object: ObjRef, property: TagProperty, value: Option<&str>) -> Result<(), TagError> {
    if !alt::elements(doc).iter().any(|e| e.object == object) {
        return Err(TagError::NotATag(object));
    }
    // ActualText may intentionally contain leading or trailing whitespace; preserve it.
    let value = value.filter(|s| !s.is_empty());
    if value.is_some_and(|s| s.len() > 65_536) {
        return Err(TagError::TooLong);
    }
    if property == TagProperty::Type
        && !value.is_some_and(|s| s.len() <= 127 && s.as_bytes().iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')))
    {
        return Err(TagError::InvalidType);
    }
    doc.update_dict(object, |d| match value {
        Some(s) if property == TagProperty::Type => d.set(property.key().to_vec(), Object::name(s)),
        Some(s) => d.set(property.key().to_vec(), Object::String(PdfString::text(s))),
        None => {
            d.remove(property.key());
        }
    })?;
    Ok(())
}

fn name_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 127 && value.as_bytes().iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-'))
}

/// List custom structure roles in lexicographic order. Invalid map values are ignored so a
/// malformed role map can still be repaired without guessing at its non-name entries.
pub fn role_map(doc: &Document) -> Vec<RoleMapEntry> {
    let Ok((_, _, map)) = editable_role_map(doc) else { return Vec::new() };
    let mut entries: Vec<_> = map
        .iter()
        .filter_map(|(custom, standard)| {
            doc.resolve(standard).as_name().map(|standard| RoleMapEntry {
                custom: String::from_utf8_lossy(custom).into_owned(),
                standard: String::from_utf8_lossy(standard).into_owned(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.custom.cmp(&b.custom));
    entries
}

/// Add, replace or remove an entry in `/StructTreeRoot/RoleMap`.
///
/// The structure tree itself must be indirect, as it is in normal tagged PDFs. Direct trees are
/// left untouched because rewriting an inline catalog subtree can discard producer-specific data.
pub fn set_role_map(doc: &mut Document, custom: &str, standard: Option<&str>) -> Result<(), RoleMapError> {
    if !name_ok(custom) {
        return Err(RoleMapError::InvalidName);
    }
    if let Some(standard) = standard
        && (!name_ok(standard) || !crate::structure::is_standard(standard.as_bytes()))
    {
        return Err(RoleMapError::InvalidTarget);
    }
    let (tree, map_ref, mut map) = editable_role_map(doc)?;
    match standard {
        Some(standard) => map.set(custom.as_bytes().to_vec(), Object::name(standard)),
        None => {
            map.remove(custom.as_bytes());
        }
    }
    if let Some(map_ref) = map_ref {
        doc.set(map_ref, Object::Dict(map));
    } else {
        doc.update_dict(tree, |tree| {
            if map.is_empty() {
                tree.remove(b"RoleMap");
            } else {
                tree.set(b"RoleMap".to_vec(), Object::Dict(map));
            }
        })?;
    }
    Ok(())
}

/// The indirect structure tree and its role map, which may be direct or indirect.
fn editable_role_map(doc: &Document) -> Result<(ObjRef, Option<ObjRef>, pdfcraft_cos::Dict), RoleMapError> {
    let catalog = doc.root().ok_or(RoleMapError::Untagged)?;
    let catalog_object = doc.get(catalog);
    let catalog = catalog_object.as_dict().ok_or(RoleMapError::Untagged)?;
    let tree = match catalog.get(b"StructTreeRoot") {
        None => return Err(RoleMapError::Untagged),
        Some(Object::Ref(tree)) => *tree,
        Some(_) => return Err(RoleMapError::DirectStructureTree),
    };
    let tree_object = doc.get(tree);
    let tree_dict = tree_object.as_dict().ok_or(RoleMapError::Untagged)?;
    match tree_dict.get(b"RoleMap") {
        None => Ok((tree, None, pdfcraft_cos::Dict::new())),
        Some(Object::Ref(map_ref)) => {
            let map = doc.get(*map_ref).as_dict().cloned().ok_or(RoleMapError::InvalidRoleMap)?;
            Ok((tree, Some(*map_ref), map))
        }
        Some(map) => doc.resolve(map).as_dict().cloned().map(|map| (tree, None, map)).ok_or(RoleMapError::InvalidRoleMap),
    }
}
