//! Lossless reflection in the displayed CropBox, retaining page streams and interactive annotations.

use std::collections::{BTreeSet, HashMap, HashSet};

use pdfcraft_content::Matrix;
use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::{OrganizeError, check, page_boxes, walk};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipDirection {
    Horizontal,
    Vertical,
}

fn invalid(message: &str) -> OrganizeError {
    OrganizeError::Invalid(format!("cannot flip pages: {message}"))
}

fn numbers(doc: &Document, value: &Object) -> Result<Vec<f64>, OrganizeError> {
    let resolved = doc.resolve(value);
    let array = resolved.as_array().ok_or_else(|| invalid("expected a coordinate array"))?;
    if array.len() > 1_000_000 {
        return Err(invalid("too many annotation coordinates"));
    }
    array.iter().map(|v| doc.resolve(v).as_f64().filter(|v| v.is_finite()).ok_or_else(|| invalid("invalid coordinate"))).collect()
}

fn fixed<const N: usize>(doc: &Document, value: &Object) -> Result<[f64; N], OrganizeError> {
    let resolved = doc.resolve(value);
    let values = resolved.as_array().filter(|v| v.len() == N).ok_or_else(|| invalid("wrong coordinate count"))?;
    let mut result = [0.0; N];
    for (out, value) in result.iter_mut().zip(values) {
        *out = doc.resolve(value).as_f64().filter(|v| v.is_finite()).ok_or_else(|| invalid("invalid coordinate"))?;
    }
    Ok(result)
}

fn array(values: impl IntoIterator<Item = f64>) -> Result<Object, OrganizeError> {
    let values = values
        .into_iter()
        .map(|v| if v.is_finite() { Ok(Object::Real(v)) } else { Err(invalid("transformed coordinate overflow")) })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Object::Array(values))
}

fn reflection(rect: [f64; 4], x: bool) -> Result<Matrix, OrganizeError> {
    let m = if x { Matrix([-1.0, 0.0, 0.0, 1.0, rect[0] + rect[2], 0.0]) } else { Matrix([1.0, 0.0, 0.0, -1.0, 0.0, rect[1] + rect[3]]) };
    if !m.0.iter().all(|v| v.is_finite()) {
        return Err(invalid("invalid page or appearance bounds"));
    }
    Ok(m)
}

/// Reflect selected pages as viewed, after their inherited rotation. Indices are zero-based;
/// duplicates are reflected once. All edits are atomic, including malformed annotation failures.
/// No rasterization, decompression or modification of original page streams is needed.
pub fn flip_pages(doc: &mut Document, indices: &[usize], direction: FlipDirection) -> Result<(), OrganizeError> {
    let all = walk(doc)?;
    check(indices, all.len())?;
    if indices.is_empty() {
        return Err(invalid("select at least one page"));
    }
    let boxes = page_boxes(doc)?;
    let selected: BTreeSet<_> = indices.iter().copied().collect();
    check_annotation_owners(doc, &all, &selected)?;
    let mut next = doc.clone();
    for i in selected {
        let (page, inherited) = all.get(i).ok_or(OrganizeError::NoSuchPage(i))?;
        let crop = boxes.get(i).ok_or(OrganizeError::NoSuchPage(i))?[1];
        flip_page(&mut next, *page, crop, inherited.int(b"Rotate").unwrap_or(0), direction)?;
    }
    *doc = next;
    Ok(())
}

fn check_annotation_owners(doc: &Document, pages: &[(ObjRef, Dict)], selected: &BTreeSet<usize>) -> Result<(), OrganizeError> {
    // An annotation shared between pages cannot have two different page-space geometries.
    // Refuse that malformed case instead of moving an unselected page's widgets or links.
    let mut owners = HashMap::new();
    for (i, (page, _)) in pages.iter().enumerate() {
        let obj = doc.get(*page);
        let Some(annots) = obj.as_dict().and_then(|d| d.get(b"Annots")) else { continue };
        let resolved = doc.resolve(annots);
        let Some(list) = resolved.as_array() else { continue };
        for a in list {
            let popup = doc.resolve(a).as_dict().and_then(|d| d.reference(b"Popup"));
            for r in [a.as_ref(), popup].into_iter().flatten() {
                if let Some(previous) = owners.insert(r, i)
                    && previous != i
                    && (selected.contains(&i) || selected.contains(&previous))
                {
                    return Err(invalid("an annotation is shared by multiple pages"));
                }
            }
        }
    }
    Ok(())
}

fn flip_page(doc: &mut Document, page: ObjRef, crop: [f64; 4], rotation: i64, direction: FlipDirection) -> Result<(), OrganizeError> {
    if !crop.iter().all(|v| v.is_finite()) || crop[2] <= crop[0] || crop[3] <= crop[1] {
        return Err(invalid("empty or invalid CropBox"));
    }
    let rotation = rotation.rem_euclid(360);
    if rotation % 90 != 0 {
        return Err(invalid("page rotation is not a multiple of 90 degrees"));
    }
    let flip_x = (direction == FlipDirection::Horizontal) != (rotation % 180 != 0);
    let matrix = reflection(crop, flip_x)?;
    let obj = doc.get(page);
    let d = obj.as_dict().ok_or_else(|| invalid("invalid page dictionary"))?;
    let contents = reflected_contents(doc, d.get(b"Contents"), matrix)?;
    if let Some(value) = d.get(b"Annots") {
        let annotations = reflected_annotations(doc, value, matrix, flip_x)?;
        doc.update_dict(page, |d| d.set(b"Annots".to_vec(), annotations))?;
    }
    doc.update_dict(page, |d| d.set(b"Contents".to_vec(), contents))?;
    Ok(())
}

fn reflected_contents(doc: &mut Document, value: Option<&Object>, matrix: Matrix) -> Result<Object, OrganizeError> {
    let mut contents = match value {
        None | Some(Object::Null) => Vec::new(),
        Some(value) => match &*doc.resolve(value) {
            Object::Stream(_) => vec![value.clone()],
            Object::Array(items) => {
                if items.iter().any(|v| !matches!(&*doc.resolve(v), Object::Stream(_))) {
                    return Err(invalid("invalid page content stream"));
                }
                items.clone()
            }
            _ => return Err(invalid("invalid page content")),
        },
    };
    // Number Display uses decimal notation (not exponent notation, which PDF forbids).
    let [a, b, c, d, e, f] = matrix.0;
    let prefix = doc.add(Stream::from_raw(Dict::new(), format!("q\n{a} {b} {c} {d} {e} {f} cm\n").into_bytes()));
    let suffix = doc.add(Stream::from_raw(Dict::new(), b"\nQ\n".to_vec()));
    contents.insert(0, Object::Ref(prefix));
    contents.push(Object::Ref(suffix));
    Ok(Object::Array(contents))
}

fn reflected_annotations(doc: &mut Document, value: &Object, matrix: Matrix, flip_x: bool) -> Result<Object, OrganizeError> {
    let resolved = doc.resolve(value);
    let list = resolved.as_array().ok_or_else(|| invalid("invalid annotations array"))?;
    if list.len() > 100_000 {
        return Err(invalid("too many annotations"));
    }
    let mut transformed = Vec::with_capacity(list.len());
    let mut seen = HashSet::new();
    for annot in list {
        transformed.push(annotation(doc, annot, matrix, flip_x, &mut seen)?);
    }
    Ok(Object::Array(transformed))
}

fn annotation(doc: &mut Document, value: &Object, matrix: Matrix, x: bool, seen: &mut HashSet<ObjRef>) -> Result<Object, OrganizeError> {
    if let Some(r) = value.as_ref() {
        if !seen.insert(r) {
            return Ok(value.clone());
        }
        if seen.len() > 100_000 {
            return Err(invalid("too many linked annotations"));
        }
    }
    let mut d = doc.resolve(value).as_dict().cloned().ok_or_else(|| invalid("invalid annotation dictionary"))?;
    if let Some(rect) = d.get(b"Rect") {
        d.set(b"Rect".to_vec(), array(matrix.bbox(fixed(doc, rect)?))?);
    }
    for key in [b"QuadPoints".as_slice(), b"L", b"Vertices", b"CL"] {
        if let Some(v) = d.get(key) {
            d.set(key.to_vec(), points(doc, v, matrix)?);
        }
    }
    if let Some(ink) = d.get(b"InkList") {
        let resolved = doc.resolve(ink);
        let strokes = resolved.as_array().ok_or_else(|| invalid("invalid ink strokes"))?;
        let transformed = strokes.iter().map(|v| points(doc, v, matrix)).collect::<Result<Vec<_>, _>>()?;
        d.set(b"InkList".to_vec(), Object::Array(transformed));
    }
    if let Some(v) = d.get(b"RD") {
        let mut margins: [f64; 4] = fixed(doc, v)?;
        if x {
            margins.swap(0, 2);
        } else {
            margins.swap(1, 3);
        }
        d.set(b"RD".to_vec(), array(margins)?);
    }
    if let Some(ap) = d.get(b"AP") {
        let mut ap = doc.resolve(ap).as_dict().cloned().ok_or_else(|| invalid("invalid annotation appearance"))?;
        for key in [b"N", b"R", b"D"] {
            if let Some(value) = ap.get(key).cloned() {
                ap.set(key.to_vec(), appearance(doc, &value, x, true)?);
            }
        }
        d.set(b"AP".to_vec(), Object::Dict(ap));
    }
    // Popup annotations need their own hit box moved, but never follow Parent/IRT back-links.
    if let Some(popup) = d.get(b"Popup").cloned() {
        // Only a real popup is followed, so a hostile Popup chain cannot recurse indefinitely.
        let obj = doc.resolve(&popup);
        if obj.as_dict().is_some_and(|p| p.name(b"Subtype") == Some(b"Popup") && !p.contains(b"Popup")) {
            let transformed = annotation(doc, &popup, matrix, x, seen)?;
            d.set(b"Popup".to_vec(), transformed);
        } else {
            return Err(invalid("invalid linked popup"));
        }
    }
    match value.as_ref() {
        Some(r) => {
            doc.set(r, Object::Dict(d));
            Ok(value.clone())
        }
        None => Ok(Object::Dict(d)),
    }
}

fn points(doc: &Document, value: &Object, matrix: Matrix) -> Result<Object, OrganizeError> {
    let mut values = numbers(doc, value)?;
    if values.len() % 2 != 0 {
        return Err(invalid("incomplete annotation point"));
    }
    for p in values.as_chunks_mut::<2>().0 {
        let (x, y) = matrix.apply(p[0], p[1]);
        p[0] = x;
        p[1] = y;
    }
    array(values)
}

fn appearance(doc: &mut Document, value: &Object, x: bool, states: bool) -> Result<Object, OrganizeError> {
    let obj = doc.resolve(value);
    match &*obj {
        Object::Stream(original) => {
            let mut stream = original.clone();
            let bbox = fixed(doc, stream.dict.get(b"BBox").ok_or_else(|| invalid("appearance has no bounding box"))?)?;
            let matrix = match stream.dict.get(b"Matrix") {
                Some(v) => Matrix(fixed(doc, v)?),
                None => Matrix::IDENTITY,
            };
            // Reflect inside the appearance's transformed bounds. PDF's appearance-to-Rect
            // placement then moves it into the mirrored Rect without changing its scale.
            let reflected = matrix.then(&reflection(matrix.bbox(bbox), x)?);
            stream.dict.set(b"Matrix".to_vec(), array(reflected.0)?);
            Ok(Object::Ref(doc.add(stream)))
        }
        Object::Dict(d) if states => {
            let mut result = d.clone();
            for (key, value) in d.iter() {
                result.set(key.clone(), appearance(doc, value, x, false)?);
            }
            Ok(Object::Dict(result))
        }
        _ => Err(invalid("invalid appearance stream or state dictionary")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_blank_page, pages};
    use pdfcraft_cos::{SaveOptions, write_full};

    fn fixture() -> (Document, ObjRef, ObjRef, ObjRef) {
        let mut doc = Document::new_empty();
        let first = insert_blank_page(&mut doc, 0, 200.0, 300.0).unwrap();
        let second = insert_blank_page(&mut doc, 1, 200.0, 300.0).unwrap();
        let content = doc.add(Stream::flate(Dict::new(), b"1 0 0 rg 10 20 30 40 re f"));
        for p in [first, second] {
            doc.update_dict(p, |d| d.set(b"Contents".to_vec(), Object::Ref(content))).unwrap();
        }
        (doc, first, second, content)
    }

    #[test]
    fn flip_preserves_shared_compressed_streams_and_unselected_pages() {
        let (mut doc, first, second, content) = fixture();
        let original_stream = doc.get(content);
        let original_page = doc.get(second);
        flip_pages(&mut doc, &[0, 0], FlipDirection::Horizontal).unwrap();
        let obj = doc.get(first);
        let list = obj.as_dict().unwrap().get(b"Contents").unwrap().as_array().unwrap();
        assert_eq!(list.len(), 3, "duplicates are only reflected once");
        assert_eq!(list[1], Object::Ref(content));
        assert_eq!(doc.get(content), original_stream, "compressed bytes and filters remain intact");
        assert_eq!(doc.get(second), original_page);
        assert_eq!(pages(&doc).unwrap().len(), 2);
    }

    #[test]
    fn flip_uses_inherited_rotation_crop_and_indirect_content_arrays() {
        let (mut doc, first, _, content) = fixture();
        let root = crate::pages_root(&doc).unwrap();
        doc.update_dict(root, |d| {
            d.set(b"Rotate".to_vec(), Object::Int(-270));
            d.set(b"CropBox".to_vec(), array([10.0, 20.0, 170.0, 260.0]).unwrap());
        })
        .unwrap();
        let list = doc.add(Object::Array(vec![Object::Ref(content), Object::Ref(content)]));
        doc.update_dict(first, |d| d.set(b"Contents".to_vec(), Object::Ref(list))).unwrap();
        flip_pages(&mut doc, &[0], FlipDirection::Horizontal).unwrap();
        let obj = doc.get(first);
        let contents = obj.as_dict().unwrap().get(b"Contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 4);
        assert_eq!(contents[1..3], [Object::Ref(content), Object::Ref(content)]);
        let prefix = doc.resolve(&contents[0]);
        let Object::Stream(prefix) = &*prefix else { panic!() };
        assert_eq!(prefix.raw.as_ref(), b"q\n1 0 0 -1 0 280 cm\n");
        assert_eq!(crate::page_rotation(&doc, 0).unwrap(), 90);
        assert_eq!(doc.get(list).as_array().unwrap().len(), 2, "shared indirect array is untouched");
        let mut blank = Document::new_empty();
        insert_blank_page(&mut blank, 0, 100.0, 100.0).unwrap();
        flip_pages(&mut blank, &[0], FlipDirection::Vertical).unwrap();
    }

    #[test]
    fn flip_moves_annotation_geometry_and_copies_shared_appearance_states() {
        let (mut doc, first, second, _) = fixture();
        let mut appearance = Dict::new();
        appearance.set(b"BBox".to_vec(), array([10.0, 20.0, 30.0, 40.0]).unwrap());
        appearance.set(b"Matrix".to_vec(), array([2.0, 0.0, 0.0, 3.0, -20.0, -60.0]).unwrap());
        let ap = doc.add(Stream::flate(appearance, b"1 0 0 rg 10 20 10 20 re f"));
        let original = doc.get(ap);
        let mut states = Dict::new();
        states.set(b"Yes".to_vec(), Object::Ref(ap));
        let mut appearances = Dict::new();
        appearances.set(b"N".to_vec(), Object::Dict(states));
        appearances.set(b"R".to_vec(), Object::Ref(ap));
        let mut annot = Dict::new();
        annot.set(b"Subtype".to_vec(), Object::name("Widget"));
        annot.set(b"Rect".to_vec(), array([10.0, 20.0, 50.0, 80.0]).unwrap());
        annot.set(b"L".to_vec(), array([10.0, 20.0, 50.0, 80.0]).unwrap());
        annot.set(b"QuadPoints".to_vec(), array([10.0, 80.0, 50.0, 80.0, 10.0, 20.0, 50.0, 20.0]).unwrap());
        annot.set(b"InkList".to_vec(), Object::Array(vec![array([10.0, 20.0, 30.0, 40.0]).unwrap()]));
        annot.set(b"AP".to_vec(), Object::Dict(appearances));
        annot.set(b"AS".to_vec(), Object::name("Yes"));
        annot.set(b"V".to_vec(), Object::name("Yes"));
        annot.set(b"PrivateData".to_vec(), Object::Int(123));
        let other = doc.add(annot.clone());
        let widget = doc.add(annot);
        for (page, a) in [(first, widget), (second, other)] {
            doc.update_dict(page, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(a)]))).unwrap();
        }
        let before_other = doc.get(other);
        flip_pages(&mut doc, &[0], FlipDirection::Horizontal).unwrap();
        let obj = doc.get(widget);
        let d = obj.as_dict().unwrap();
        assert_eq!(fixed::<4>(&doc, d.get(b"Rect").unwrap()).unwrap(), [150.0, 20.0, 190.0, 80.0]);
        assert_eq!(fixed::<4>(&doc, d.get(b"L").unwrap()).unwrap(), [190.0, 20.0, 150.0, 80.0]);
        assert_eq!(fixed::<8>(&doc, d.get(b"QuadPoints").unwrap()).unwrap(), [190.0, 80.0, 150.0, 80.0, 190.0, 20.0, 150.0, 20.0]);
        assert_eq!(d.name(b"V"), Some(b"Yes".as_slice()));
        assert_eq!(d.int(b"PrivateData"), Some(123));
        assert_eq!(doc.get(other), before_other);
        assert_eq!(doc.get(ap), original, "another page shares this appearance");
        let ap_dict = d.get(b"AP").unwrap().as_dict().unwrap();
        let new_ap = doc.resolve(ap_dict.get(b"N").unwrap().as_dict().unwrap().get(b"Yes").unwrap());
        let Object::Stream(new_ap) = &*new_ap else { panic!() };
        assert_eq!(fixed::<6>(&doc, new_ap.dict.get(b"Matrix").unwrap()).unwrap(), [-2.0, 0.0, 0.0, 3.0, 60.0, -60.0]);
        flip_pages(&mut doc, &[0], FlipDirection::Horizontal).unwrap();
        assert_eq!(fixed::<4>(&doc, doc.get(widget).as_dict().unwrap().get(b"Rect").unwrap()).unwrap(), [10.0, 20.0, 50.0, 80.0]);
    }

    #[test]
    fn flip_is_atomic_for_invalid_selections_annotations_and_shared_widgets() {
        let (mut doc, first, second, _) = fixture();
        let unchanged = |doc: &Document| write_full(doc, &SaveOptions::default()).unwrap();
        for indices in [vec![0, 99], vec![]] {
            let before = unchanged(&doc);
            assert!(flip_pages(&mut doc, &indices, FlipDirection::Vertical).is_err());
            assert_eq!(unchanged(&doc), before);
        }
        let mut malformed = Dict::new();
        malformed.set(b"Rect".to_vec(), Object::Array(vec![Object::Int(1)]));
        let a = doc.add(malformed);
        doc.update_dict(second, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(a)]))).unwrap();
        let before = unchanged(&doc);
        assert!(flip_pages(&mut doc, &[0, 1], FlipDirection::Horizontal).is_err());
        assert_eq!(unchanged(&doc), before, "an error on the second page also rolls back the first");
        doc.update_dict(first, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(a)]))).unwrap();
        let before = unchanged(&doc);
        assert!(flip_pages(&mut doc, &[0], FlipDirection::Vertical).unwrap_err().to_string().contains("shared"));
        assert_eq!(unchanged(&doc), before);
    }

    #[test]
    fn flip_moves_link_and_popup_once_without_changing_their_relationships() {
        let (mut doc, first, _, _) = fixture();
        let mut action = Dict::new();
        action.set(b"S".to_vec(), Object::name("URI"));
        action.set(b"URI".to_vec(), Object::String(pdfcraft_cos::PdfString::text("https://example.org/")));
        let mut link = Dict::new();
        link.set(b"Subtype".to_vec(), Object::name("Link"));
        link.set(b"Rect".to_vec(), array([10.0, 20.0, 50.0, 80.0]).unwrap());
        link.set(b"A".to_vec(), Object::Dict(action.clone()));
        let link = doc.add(link);
        let mut popup = Dict::new();
        popup.set(b"Subtype".to_vec(), Object::name("Popup"));
        popup.set(b"Rect".to_vec(), array([60.0, 20.0, 90.0, 80.0]).unwrap());
        popup.set(b"Parent".to_vec(), Object::Ref(link));
        let popup = doc.add(popup);
        doc.update_dict(link, |d| d.set(b"Popup".to_vec(), Object::Ref(popup))).unwrap();
        doc.update_dict(first, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(link), Object::Ref(popup)]))).unwrap();
        flip_pages(&mut doc, &[0], FlipDirection::Vertical).unwrap();
        assert_eq!(fixed::<4>(&doc, doc.get(link).as_dict().unwrap().get(b"Rect").unwrap()).unwrap(), [10.0, 220.0, 50.0, 280.0]);
        assert_eq!(fixed::<4>(&doc, doc.get(popup).as_dict().unwrap().get(b"Rect").unwrap()).unwrap(), [60.0, 220.0, 90.0, 280.0]);
        assert_eq!(doc.get(link).as_dict().unwrap().get(b"A"), Some(&Object::Dict(action)));
        assert_eq!(doc.get(popup).as_dict().unwrap().reference(b"Parent"), Some(link));
    }

    fn assert_atomic_failure(doc: &mut Document, message: &str) {
        let before = write_full(doc, &SaveOptions::default()).unwrap();
        let error = flip_pages(doc, &[0, 1], FlipDirection::Horizontal).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(write_full(doc, &SaveOptions::default()).unwrap(), before, "the first page must also be rolled back");
    }

    #[test]
    fn flip_rejects_invalid_contents_on_a_later_page_atomically() {
        for indirect in [false, true] {
            for invalid in [Object::Int(7), Object::Array(vec![Object::Int(7)])] {
                let (mut doc, _, second, _) = fixture();
                let value = if indirect { Object::Ref(doc.add(invalid)) } else { invalid };
                doc.update_dict(second, |d| d.set(b"Contents".to_vec(), value)).unwrap();
                assert_atomic_failure(&mut doc, "invalid page content");
            }
        }
    }

    #[test]
    fn flip_rejects_cyclic_appearances_and_malformed_geometry_atomically() {
        let (mut doc, _, second, _) = fixture();
        let cycle = doc.add(Dict::new());
        doc.update_dict(cycle, |d| d.set(b"On".to_vec(), Object::Ref(cycle))).unwrap();
        let missing_bounds = doc.add(Stream::from_raw(Dict::new(), Vec::new()));
        let mut appearance = Dict::new();
        appearance.set(b"BBox".to_vec(), array([0.0, 0.0, 40.0, 60.0]).unwrap());
        appearance.set(b"Matrix".to_vec(), array([1.0, 0.0]).unwrap());
        let truncated_matrix = doc.add(Stream::from_raw(appearance, Vec::new()));
        let normal = |r| {
            let mut ap = Dict::new();
            ap.set(b"N".to_vec(), Object::Ref(r));
            Object::Dict(ap)
        };
        let cases = [
            (b"AP".as_slice(), normal(cycle), "invalid appearance"),
            (b"AP", normal(missing_bounds), "no bounding box"),
            (b"AP", normal(truncated_matrix), "wrong coordinate count"),
            (b"Vertices", array([10.0, 20.0, 30.0]).unwrap(), "incomplete annotation point"),
            (b"InkList", Object::Array(vec![Object::Int(7)]), "expected a coordinate array"),
        ];
        for (key, value, expected) in cases {
            let mut annot = Dict::new();
            annot.set(b"Rect".to_vec(), array([10.0, 20.0, 50.0, 80.0]).unwrap());
            annot.set(key.to_vec(), value);
            let annot = doc.add(annot);
            doc.update_dict(second, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(annot)]))).unwrap();
            assert_atomic_failure(&mut doc, expected);
        }
    }

    #[test]
    fn flip_rejects_shared_linked_popups_even_when_not_listed_on_their_page() {
        for selected in [vec![0], vec![1], vec![0, 1]] {
            let (mut doc, first, second, _) = fixture();
            let mut popup = Dict::new();
            popup.set(b"Subtype".to_vec(), Object::name("Popup"));
            popup.set(b"Rect".to_vec(), array([10.0, 20.0, 50.0, 80.0]).unwrap());
            let popup = doc.add(popup);
            let mut parent = Dict::new();
            parent.set(b"Subtype".to_vec(), Object::name("Text"));
            parent.set(b"Popup".to_vec(), Object::Ref(popup));
            let parent = doc.add(parent);
            for (page, annot) in [(first, parent), (second, popup)] {
                doc.update_dict(page, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(annot)]))).unwrap();
            }
            let before = write_full(&doc, &SaveOptions::default()).unwrap();
            assert!(flip_pages(&mut doc, &selected, FlipDirection::Vertical).unwrap_err().to_string().contains("shared"));
            assert_eq!(write_full(&doc, &SaveOptions::default()).unwrap(), before);
        }
    }

    #[test]
    fn flip_rejects_popup_cycles_and_later_page_rotation_atomically() {
        let (mut doc, _, second, _) = fixture();
        let mut popup = Dict::new();
        popup.set(b"Subtype".to_vec(), Object::name("Popup"));
        let popup = doc.add(popup);
        doc.update_dict(popup, |d| d.set(b"Popup".to_vec(), Object::Ref(popup))).unwrap();
        doc.update_dict(second, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(popup)]))).unwrap();
        assert_atomic_failure(&mut doc, "invalid linked popup");
        doc.update_dict(second, |d| {
            d.remove(b"Annots");
            d.set(b"Rotate".to_vec(), Object::Int(45));
        })
        .unwrap();
        assert_atomic_failure(&mut doc, "not a multiple of 90");
    }
}
