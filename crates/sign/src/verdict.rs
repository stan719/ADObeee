//! Validated states of layered signature appearances, drawn for display only.
//!
//! Some signer software builds a signature appearance from nested form XObjects:
//! background, signer text, status icon, border and status caption. The file bakes
//! the unverified state (a "Signature Not Verified" caption, a question-mark icon);
//! validating viewers generate the validated state into those two slots once the
//! signature verifies, which is why the same bytes read "Signature valid" with a
//! green tick there. PdfCraft does the same in the display copy: the signed bytes
//! are never touched, so saving keeps the original appearance and the signature
//! stays valid.
//!
//! Slots are identified by content role, never by XObject name: the caption is the
//! text layer still carrying the unverified template, and the icon is the one
//! vector drawing that is not framing. Frames, borders and panels are rectangles
//! however they are stroked, while the status mark is not, so a drawing made only
//! of axis-aligned rectangles is decoration, never the icon. Only appearances
//! that still carry the unverified caption are rewritten, so a file that already
//! shows its verdict is left alone, as is anything with an unexpected shape.
//! Every anomaly degrades to the previous behaviour (the raw appearance plus the
//! UI's own verdict mark) rather than failing.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};
use pdfcraft_fonts::{helvetica_width, literal, win_ansi};
use std::collections::{HashMap, HashSet};

use crate::pdf::{SignatureInfo, Status, fmt, text};

/// Show validated verdicts inside layered signature appearances of the display copy.
/// Marks each substituted signature so the page needs no overlay badge for it.
/// Best-effort and infallible: anything unexpected is left as it is.
/// Returns whether any appearance changed.
pub fn show_valid_appearances(doc: &mut Document, signatures: &mut [SignatureInfo]) -> bool {
    // Layer references claimed by more than one field are never rewritten: sharing
    // one stream between fields with different verdicts must not paint either wrong.
    // Every field counts, not just the valid ones: a shared caption with an
    // unvalidated field would otherwise be rewritten for the valid one alone.
    // Refs are deduped per field first, so two widgets of one field sharing a
    // stream still rewrite it once for their shared verdict.
    let mut seen_fields = HashSet::new();
    let mut owners: HashMap<ObjRef, usize> = HashMap::new();
    for sig in signatures.iter() {
        if !seen_fields.insert(sig.field.clone()) {
            continue;
        }
        let unique: HashSet<ObjRef> = appearance_layer_refs(doc, &sig.field).into_iter().collect();
        for r in unique {
            *owners.entry(r).or_default() += 1;
        }
    }
    let mut changed = false;
    for sig in signatures.iter_mut().filter(|s| s.signed && s.status == Status::Valid) {
        if substitute_valid_state(doc, &sig.field, &owners) {
            sig.verdict_shown = true;
            changed = true;
        }
    }
    changed
}

/// Whether any valid signature has its icon and caption slots: so callers only
/// build a writable display copy when there is something it could change. Weaker
/// than the rewrite gates on purpose (no bounding-box, matrix or font checks):
/// a false positive wastes one copy, while a false negative would miss the fix.
pub fn has_substitutable_appearances(doc: &Document, signatures: &[SignatureInfo]) -> bool {
    signatures
        .iter()
        .filter(|s| s.signed && s.status == Status::Valid)
        .any(|sig| widget_layers(doc, &sig.field).is_some_and(|widgets| widgets.iter().any(|layers| find_slots(doc, layers).is_some())))
}

/// The most XObject levels an appearance tree is followed down.
const MAX_APPEARANCE_DEPTH: usize = 4;

/// The most layers collected from one appearance tree.
const MAX_APPEARANCE_LAYERS: usize = 64;

/// The most field nodes visited looking for one field: hostile AcroForms still
/// terminate, while real forms (tens of thousands of fields at most) never notice.
const MAX_FIELD_NODES: usize = 100_000;

/// Decoded bytes read from a layer at most: appearances are kilobytes; anything
/// bigger is damage, and failing to read it only skips the rewrite.
const MAX_LAYER_DECODED: usize = 1 << 20;

/// Rewrite the status icon and caption layers of `field`'s widgets with the
/// validated state. Every widget that carries the pair must rewrite cleanly, or
/// nothing does: a half-rewritten field would be worse than the original.
/// Streams claimed by another field (`shared`) are left alone.
fn substitute_valid_state(doc: &mut Document, field: &str, shared: &HashMap<ObjRef, usize>) -> bool {
    let Some(widgets) = widget_layers(doc, field) else { return false };
    if widgets.is_empty() {
        return false;
    }
    // Plan first, write after: no stream changes until every widget checks out.
    let mut plan = Vec::new();
    for layers in &widgets {
        let Some((icon, caption)) = find_slots(doc, layers) else {
            return false;
        };
        if shared.get(&icon.r).is_some_and(|&n| n > 1) || shared.get(&caption.r).is_some_and(|&n| n > 1) {
            return false;
        }
        let (Some(icon_bbox), Some(caption_bbox)) = (stream_bbox(doc, &icon.dict), stream_bbox(doc, &caption.dict)) else {
            return false;
        };
        if !matrix_is_identity(doc, &icon.dict) || !matrix_is_identity(doc, &caption.dict) {
            return false;
        }
        let Some(font) = fresh_helvetica_name(doc, &caption.dict) else { return false };
        let Some(check) = checkmark_stream(&icon_bbox) else { return false };
        let Some(text) = caption_stream(&caption_bbox, &font) else { return false };
        plan.push((icon.r, icon.dict.clone(), check, caption.r, caption.dict.clone(), text, font));
    }
    for (icon_r, icon_dict, check, caption_r, caption_dict, text, font) in plan {
        doc.set(icon_r, Object::Stream(Stream::from_raw(stripped(&icon_dict), check)));
        let mut caption_dict = stripped(&caption_dict);
        caption_dict.set(b"Resources".to_vec(), font_resources(doc, &caption_dict, &font));
        doc.set(caption_r, Object::Stream(Stream::from_raw(caption_dict, text)));
    }
    true
}

/// The icon and caption layers of one widget, found by content role: the caption is
/// the text layer still carrying the unverified template, and the icon is the
/// one vector drawing that is not framing. Names are never consulted: signers arrange
/// `n0`–`n4` either way round, and only the content tells which slot is which.
/// Singularity alone never qualifies a layer: backgrounds, borders and panels are
/// rectangles however they are stroked, so a drawing made only of axis-aligned
/// rectangles is decoration, not the status mark. When either slot is missing,
/// repeated or framing-only, there is no slot and the caller falls back to the
/// overlay badge.
fn find_slots(doc: &Document, layers: &[Layer]) -> Option<(LayerRef, LayerRef)> {
    let mut captions: Vec<&Layer> = Vec::new();
    let mut icons: Vec<&Layer> = Vec::new();
    for layer in layers {
        let (probe, decoded) = probe_layer(doc, layer);
        if probe.has_text && layer_contains(&decoded, b"Not Verified") {
            captions.push(layer);
        }
        if probe.is_form && !probe.has_text && !is_only_rectangles(&decoded) {
            icons.push(layer);
        }
    }
    let ([caption], [icon]) = (captions.as_slice(), icons.as_slice()) else { return None };
    Some((LayerRef::of(icon), LayerRef::of(caption)))
}

// A layer reference with its dictionary, for rewriting.
struct LayerRef {
    r: ObjRef,
    dict: Dict,
}

impl LayerRef {
    fn of(layer: &Layer) -> Self {
        Self { r: layer.r, dict: layer.dict.clone() }
    }
}

/// What a layer draws, probed from capped decoded content: whether it shows text
/// and whether it is a form (image and other streams never qualify).
/// Returns the decoded bytes alongside, so callers never decode twice.
struct Probe {
    is_form: bool,
    has_text: bool,
}

fn probe_layer(doc: &Document, layer: &Layer) -> (Probe, Vec<u8>) {
    let empty = Probe { is_form: false, has_text: false };
    let resolved = doc.resolve(&Object::Ref(layer.r));
    let stream = match &*resolved {
        Object::Stream(s) => s,
        _ => return (empty, Vec::new()),
    };
    let Ok(decoded) = stream.decoded_within(MAX_LAYER_DECODED) else { return (empty, Vec::new()) };
    let is_form = stream.dict.get(b"Subtype").map(|s| doc.resolve(s)).is_none_or(|s| s.as_name().is_none_or(|n| n == b"Form"));
    (Probe { is_form, has_text: shows_text(&decoded) }, decoded)
}

/// Whether decoded content shows text (`Tj TJ ' "`), tokenised so operator-like
/// bytes inside literal strings never count (hex payloads stay glued to their
/// `<…>` brackets, so they never exact-match a bare operator).
fn shows_text(decoded: &[u8]) -> bool {
    const TEXT_OPS: &[&[u8]] = &[b"Tj", b"TJ", b"'", b"\""];
    blank_strings(decoded)
        .split(|c| matches!(c, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0'))
        .filter(|tok| !tok.is_empty())
        .any(|tok| TEXT_OPS.contains(&tok))
}

/// Decoded content with literal strings blanked: `(a Tj)` draws an "a", not an
/// operator. The closing delimiter is consumed too, or it would glue onto the
/// next operator (`(a)Tj` is one string followed by `Tj`, since `)` delimits).
fn blank_strings(decoded: &[u8]) -> Vec<u8> {
    let mut flat = Vec::with_capacity(decoded.len());
    let mut b = decoded;
    while let Some(i) = b.iter().position(|c| *c == b'(') {
        flat.extend_from_slice(&b[..i]);
        flat.push(b' ');
        let mut depth = 0usize;
        let mut j = i;
        while j < b.len() {
            match b[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b'\\' => j += 1,
                _ => {}
            }
            j += 1;
        }
        b = &b[(j + 1).min(b.len())..];
    }
    flat.extend_from_slice(b);
    flat
}

/// Whether a vector drawing is only axis-aligned rectangles: frames, borders and
/// panels, stroked with lines or `re` alike. Curves, irregular or open segments
/// and unparseable numbers are content, not framing, so this returns false for
/// them; an empty or degenerate drawing counts as framing, since there is no
/// mark in it to identify. Callers exclude framing from the icon slot and fall
/// back to the overlay badge instead of repainting decoration.
fn is_only_rectangles(decoded: &[u8]) -> bool {
    /// A closed loop of segments is a rectangle when its corners turn exactly
    /// four times between horizontal and vertical. Collinear splits contribute
    /// no turn, so mid-edge points need no tolerance arithmetic; identical
    /// literals compare exactly, so axis checks need none either.
    fn is_rect(pts: &[[f64; 2]]) -> bool {
        let Some(first) = pts.first() else {
            return true;
        };
        let (mut x0, mut y0, mut x1, mut y1) = (first[0], first[1], first[0], first[1]);
        for p in &pts[1..] {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        if x1 <= x0 || y1 <= y0 {
            return true;
        }
        let mut dirs = Vec::new();
        for w in pts.windows(2) {
            let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
            if dx == 0.0 && dy == 0.0 {
                continue;
            }
            if dx != 0.0 && dy != 0.0 {
                return false;
            }
            dirs.push(dx != 0.0);
        }
        if dirs.is_empty() {
            return true;
        }
        let mut turns = 0;
        for i in 0..dirs.len() {
            if dirs[i] != dirs[(i + 1) % dirs.len()] {
                turns += 1;
            }
        }
        if turns != 4 {
            return false;
        }
        true
    }
    // The subpath under construction and whether `h` closed it: paint ends the
    // subpath, and anything left open is content, not framing.
    fn flush(current: &mut Vec<[f64; 2]>, closed: &mut bool) -> bool {
        let mut pts = std::mem::take(current);
        let was_closed = std::mem::replace(closed, false);
        let Some(first) = pts.first().copied() else {
            return true;
        };
        if was_closed {
            pts.push(first);
        }
        if pts.first() != pts.last() {
            return false;
        }
        is_rect(&pts)
    }
    let flat = blank_strings(decoded);
    let mut stack: Vec<f64> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    let mut closed = false;
    for tok in flat.split(|c| matches!(c, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0')) {
        if tok.is_empty() {
            continue;
        }
        let Ok(word) = std::str::from_utf8(tok) else {
            return true;
        };
        if let Ok(n) = word.parse::<f64>() {
            stack.push(n);
            if stack.len() > 64 {
                return true;
            }
            continue;
        }
        match word {
            "m" => {
                if stack.len() < 2 {
                    return true;
                }
                if !flush(&mut current, &mut closed) {
                    return false;
                }
                let n = stack.len();
                current.push([stack[n - 2], stack[n - 1]]);
                stack.clear();
            }
            "l" => {
                if stack.len() < 2 {
                    return true;
                }
                if current.is_empty() {
                    return true;
                }
                let n = stack.len();
                current.push([stack[n - 2], stack[n - 1]]);
                stack.clear();
            }
            "c" | "v" | "y" => return false,
            "h" => closed = true,
            "re" => {
                if stack.len() < 4 {
                    return true;
                }
                if !flush(&mut current, &mut closed) {
                    return false;
                }
                stack.clear();
            }
            "S" | "s" | "f" | "F" | "f*" | "B" | "b*" | "n" | "W" | "W*" => {
                if !flush(&mut current, &mut closed) {
                    return false;
                }
                stack.clear();
            }
            _ => stack.clear(),
        }
    }
    flush(&mut current, &mut closed)
}

// A form layer: its reference (for rewriting) and stream dictionary.
struct Layer {
    r: ObjRef,
    dict: Dict,
}

/// The form XObjects of each widget of `field`, one list per widget.
fn widget_layers(doc: &Document, field: &str) -> Option<Vec<Vec<Layer>>> {
    if field.is_empty() {
        return None;
    }
    let mut widgets = Vec::new();
    collect_widgets(doc, field, &mut widgets)?;
    let mut out = Vec::new();
    for w in &widgets {
        let mut layers = Vec::new();
        let mut seen = HashSet::new();
        let Some(apd) = w.get(b"AP").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()) else { continue };
        let Some(n) = apd.get(b"N") else { continue };
        walk_appearance(doc, n, 0, &mut seen, &mut layers);
        out.push(layers);
    }
    Some(out)
}

// Every layer reference reachable from `field`'s appearance trees, for sharing checks.
fn appearance_layer_refs(doc: &Document, field: &str) -> Vec<ObjRef> {
    widget_layers(doc, field).unwrap_or_default().into_iter().flatten().map(|l| l.r).collect()
}

/// Form widgets of the named signature field: the field dictionary itself when it is
/// merged with its widget, else its kid widgets. Mirrors the traversal of the
/// validator's field list, so a field validation can see is a field this can find.
fn collect_widgets(doc: &Document, field: &str, widgets: &mut Vec<Dict>) -> Option<()> {
    let root = doc.root()?;
    let af = doc.get(root).as_dict()?.get(b"AcroForm").map(|a| doc.resolve(a))?;
    let af = af.as_dict()?;
    let fields = doc.resolve(af.get(b"Fields")?).as_array().cloned()?;
    let mut seen = HashSet::new();
    let mut stack: Vec<(Object, String)> = fields.into_iter().rev().map(|f| (f, String::new())).collect();
    let mut visited = 0usize;
    while let Some((o, prefix)) = stack.pop() {
        visited += 1;
        if visited > MAX_FIELD_NODES {
            return None;
        }
        if let Some(r) = o.as_ref()
            && !seen.insert(r)
        {
            continue;
        }
        let d = doc.resolve(&o).as_dict().cloned()?;
        let part = text(doc, &d, b"T");
        let name = match (&part, prefix.is_empty()) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{prefix}.{p}"),
            (None, _) => prefix.clone(),
        };
        if name == field {
            let kids: Vec<Object> = d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
            for k in &kids {
                if let Some(wd) = doc.resolve(k).as_dict().cloned()
                    && wd.name(b"Subtype") == Some(b"Widget")
                {
                    widgets.push(wd);
                }
            }
            if d.name(b"Subtype") == Some(b"Widget") {
                widgets.push(d);
            }
            return Some(());
        }
        let kids: Vec<Object> = d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default();
        for k in kids.into_iter().rev() {
            if doc.resolve(&k).as_dict().is_some_and(|kd| kd.contains(b"T")) {
                stack.push((k, name.clone()));
            }
        }
    }
    None
}

/// Named form XObjects under `o`, following nested resource trees. Only indirect
/// streams are collected (only those can be rewritten); everything else is just
/// descended into. Bounded by depth, layer count and a seen-set, so hostile graphs
/// terminate with bounded work.
fn walk_appearance(doc: &Document, o: &Object, depth: usize, seen: &mut HashSet<ObjRef>, out: &mut Vec<Layer>) {
    if depth > MAX_APPEARANCE_DEPTH || out.len() >= MAX_APPEARANCE_LAYERS {
        return;
    }
    let resolved = doc.resolve(o);
    let Some(res) = resolved.as_dict().and_then(|d| d.get(b"Resources")).map(|r| doc.resolve(r)) else {
        // Not a form with resources (the /N stream itself has none, say): still look
        // through it when it is a dictionary of named states.
        if let Some(d) = resolved.as_dict() {
            for (k, v) in d.iter() {
                if matches!(k.as_slice(), b"N" | b"AP") {
                    continue;
                }
                walk_appearance(doc, v, depth + 1, seen, out);
            }
        }
        return;
    };
    let Some(xod) = res.as_dict() else { return };
    let Some(xobjects) = xod.get(b"XObject").map(|x| doc.resolve(x)) else { return };
    let Some(xobjects) = xobjects.as_dict() else { return };
    for (_, v) in xobjects.iter() {
        if out.len() >= MAX_APPEARANCE_LAYERS {
            return;
        }
        let r = doc.resolve(v);
        if let (Some(oref), Object::Stream(s)) = (v.as_ref(), &*r)
            && seen.insert(oref)
        {
            out.push(Layer { r: oref, dict: s.dict.clone() });
        }
        walk_appearance(doc, v, depth + 1, seen, out);
    }
}

// Whether decoded content holds `needle`.
fn layer_contains(decoded: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && decoded.windows(needle.len()).any(|w| w == needle)
}

/// A stream's `/BBox` as exactly four finite numbers enclosing positive, bounded
/// user-space coordinates.
fn stream_bbox(doc: &Document, dict: &Dict) -> Option<[f64; 4]> {
    let bbox = dict.get(b"BBox").map(|b| doc.resolve(b))?;
    let bbox = bbox.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    let mut v = [0.0; 4];
    for (slot, x) in v.iter_mut().zip(bbox.iter()) {
        *slot = doc.resolve(x).as_f64()?;
    }
    let (w, h) = (v[2] - v[0], v[3] - v[1]);
    (v.iter().all(|x| x.is_finite()) && w > 0.0 && h > 0.0 && w <= 1e6 && h <= 1e6).then_some(v)
}

/// A stream's `/Matrix`, if any, must be the identity: generated content is laid
/// out in bounding-box coordinates, which a transforming matrix would displace.
fn matrix_is_identity(doc: &Document, dict: &Dict) -> bool {
    let Some(m) = dict.get(b"Matrix").map(|m| doc.resolve(m)) else { return true };
    let Some(m) = m.as_array() else { return false };
    if m.len() != 6 {
        return false;
    }
    let v: Vec<f64> = m.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    v.len() == 6 && v.iter().all(|x| x.is_finite()) && v[0] == 1.0 && v[1] == 0.0 && v[2] == 0.0 && v[3] == 1.0 && v[4] == 0.0 && v[5] == 0.0
}

/// The stream dictionary without its encoding: replaced content is stored plain
/// (the writer sets `/Length` at write time).
fn stripped(dict: &Dict) -> Dict {
    let mut d = dict.clone();
    d.remove(b"Filter");
    d.remove(b"DecodeParms");
    d.remove(b"Length");
    d
}

/// The caption "Signature valid", in the signer's own template language (English):
/// only English-template appearances pass the gate, so a translated caption would
/// be the one thing in the box speaking another language. Plain ASCII, so any
/// WinAnsi font draws it.
fn caption_stream(bbox: &[f64; 4], font: &[u8]) -> Option<Vec<u8>> {
    const CAPTION: &str = "Signature valid";
    let (w, h) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
    let size = (h * 0.62).min(w * 0.94 / helvetica_width(CAPTION, 1.0).max(0.01)).max(3.0);
    if !size.is_finite() {
        return None;
    }
    let x = bbox[0] + w * 0.03;
    let y = bbox[1] + ((h - size * 0.72) / 2.0).max(0.0);
    let mut out = b"BT /".to_vec();
    out.extend_from_slice(font);
    out.extend_from_slice(format!(" {} Tf 0 g 1 0 0 1 {} {} Tm ", fmt(size), fmt(x), fmt(y)).as_bytes());
    out.extend(literal(&win_ansi(CAPTION)));
    out.extend_from_slice(b" Tj ET\n");
    Some(out)
}

/// The appearance's resources with a fresh Helvetica for the caption, under an
/// unused name. Additive only: nothing the original content references is renamed
/// or removed.
fn font_resources(doc: &Document, dict: &Dict, font: &[u8]) -> Dict {
    let mut res = dict.get(b"Resources").map(|r| doc.resolve(r)).and_then(|r| r.as_dict().cloned()).unwrap_or_default();
    let mut fonts = res.get(b"Font").map(|f| doc.resolve(f)).and_then(|f| f.as_dict().cloned()).unwrap_or_default();
    if !fonts.contains(font) {
        let mut helv = Dict::new();
        helv.set(b"Type".to_vec(), Object::name("Font"));
        helv.set(b"Subtype".to_vec(), Object::name("Type1"));
        helv.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
        helv.set(b"Encoding".to_vec(), Object::name("WinAnsiEncoding"));
        fonts.set(font.to_vec(), Object::Dict(helv));
    }
    res.set(b"Font".to_vec(), Object::Dict(fonts));
    res
}

/// A fresh Helvetica resource name (`Helv`, `Helv2`, …). A face of our own rather
/// than the appearance's on purpose: embedded subsets carry only the glyphs the
/// original caption used, and "valid" needs an `l` that "Not Verified" never did.
/// `None` when hundreds are taken; then the field is left alone.
fn fresh_helvetica_name(doc: &Document, dict: &Dict) -> Option<Vec<u8>> {
    let fonts = dict
        .get(b"Resources")
        .map(|r| doc.resolve(r))
        .and_then(|r| r.as_dict().cloned())
        .and_then(|res| res.get(b"Font").map(|f| doc.resolve(f)))
        .and_then(|f| f.as_dict().cloned())
        .unwrap_or_default();
    for n in 1..1000u32 {
        let name = if n == 1 { b"Helv".to_vec() } else { format!("Helv{n}").into_bytes() };
        if !fonts.contains(&name) {
            return Some(name);
        }
    }
    None
}

/// A green check drawn in the icon slot, in the appearance's own coordinate frame,
/// wrapped so its graphics state cannot leak into surrounding content. The UI's own
/// valid green, so banner, panel and page agree with each other.
fn checkmark_stream(bbox: &[f64; 4]) -> Option<Vec<u8>> {
    let (x0, y0, w, h) = (bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]);
    let lw = (w.min(h) * 0.13).max(0.5);
    if !(w > 0.0 && h > 0.0 && lw.is_finite()) {
        return None;
    }
    let mut out = b"q 0.18 0.62 0.47 RG ".to_vec();
    out.extend_from_slice(format!("{} w 1 J 1 j ", fmt(lw)).as_bytes());
    out.extend_from_slice(
        format!(
            "{} {} m {} {} l {} {} l S Q\n",
            fmt(x0 + 0.30 * w),
            fmt(y0 + 0.44 * h),
            fmt(x0 + 0.45 * w),
            fmt(y0 + 0.29 * h),
            fmt(x0 + 0.72 * w),
            fmt(y0 + 0.71 * h),
        )
        .as_bytes(),
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn stream(bbox: &str, resources: &str, ops: &str) -> String {
        format!("<< /BBox {bbox} /Length {} /Resources {resources} >>\nstream\n{ops}\nendstream", ops.len())
    }

    /// Object numbers in every fixture below: 1 catalog, 2 pages, 3 page, 4 field,
    /// 5 form, 6 /N, 7 frame, 8 background, 9 words, 10 glyph, 11 caption, 12 border,
    /// 13 Helvetica. Layer names are deliberately undistinguished: selection works
    /// by content role, never by name.
    fn assemble(objs: Vec<String>) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (k, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", k + 1).as_bytes());
        }
        let start = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for o in &offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF", objs.len() + 1).as_bytes());
        out
    }

    /// A one-page file whose signature field carries a layered appearance: background,
    /// words, a vector glyph, border, and a caption that still says "Not Verified".
    fn layered_fixture() -> Vec<u8> {
        let helv_font = "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>";
        assemble(vec![
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 5 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string(),
            "<< /FT /Sig /T (Signature1) /Subtype /Widget /Rect [10 10 110 60] /AP << /N 6 0 R >> >>".to_string(),
            "<< /Fields [4 0 R] >>".to_string(),
            stream("[0 0 50 30]", "<< /XObject << /frame 7 0 R >> >>", "q /frame Do Q"),
            stream(
                "[0 0 50 30]",
                "<< /XObject << /bg 8 0 R /words 9 0 R /glyph 10 0 R /bd 12 0 R /cap 11 0 R >> >>",
                "q /bg Do /words Do /glyph Do /bd Do /cap Do Q",
            ),
            stream("[0 0 100 100]", "<< >>", "0 g 0 0 100 100 re f"),
            stream("[0 0 100 100]", helv_font, "BT /F1 10 Tf 5 50 Td (Signed by Test) Tj ET"),
            stream("[0 0 50 30]", "<< >>", "0.8 0.1 0.1 RG 3 w 5 5 m 20 5 l 12 22 l h S"),
            stream("[0 21 50 30]", helv_font, "BT /F1 9 Tf 2 22 Td (Signature Not Verified) Tj ET"),
            stream("[0 0 100 100]", "<< >>", "0 g 0 0 100 100 re f"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ])
    }

    fn verdict(field: &str, status: Status) -> SignatureInfo {
        SignatureInfo {
            field: field.into(),
            signed: true,
            page: Some(0),
            rect: Some([10.0, 10.0, 110.0, 60.0]),
            visible: true,
            signer: None,
            certificate: None,
            chain: Vec::new(),
            date: None,
            signing_time: None,
            reason: None,
            location: None,
            contact: None,
            sub_filter: None,
            doc_timestamp: false,
            certify: None,
            revision: 1,
            signed_len: 0,
            digest: None,
            algorithm: None,
            timestamp: false,
            timestamp_time: None,
            status,
            modification: crate::pdf::Modification::None,
            verdict_shown: false,
            details: Vec::new(),
        }
    }

    /// The (icon, caption) references of the first widget, through the production
    /// selection. Callers keep the references: after a substitution the marker the
    /// selection looks for is gone by design.
    fn slots(doc: &Document, field: &str) -> (ObjRef, ObjRef) {
        let mut widgets = Vec::new();
        collect_widgets(doc, field, &mut widgets).unwrap();
        let mut seen = HashSet::new();
        let mut layers = Vec::new();
        let apd = widgets[0].get(b"AP").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap();
        walk_appearance(doc, apd.get(b"N").unwrap(), 0, &mut seen, &mut layers);
        let (icon, caption) = find_slots(doc, &layers).unwrap();
        (icon.r, caption.r)
    }

    fn decoded(doc: &Document, r: ObjRef) -> Vec<u8> {
        match &*doc.resolve(&Object::Ref(r)) {
            Object::Stream(s) => s.decoded().unwrap(),
            _ => panic!("not a stream"),
        }
    }

    /// The decoded bytes of every layer of the field, by reference.
    fn all_layers(doc: &Document, field: &str) -> Vec<(ObjRef, Vec<u8>)> {
        let mut widgets = Vec::new();
        collect_widgets(doc, field, &mut widgets).unwrap();
        let mut seen = HashSet::new();
        let mut layers = Vec::new();
        for w in &widgets {
            let apd = w.get(b"AP").map(|a| doc.resolve(a)).and_then(|a| a.as_dict().cloned()).unwrap();
            walk_appearance(doc, apd.get(b"N").unwrap(), 0, &mut seen, &mut layers);
        }
        layers.into_iter().map(|l| (l.r, decoded(doc, l.r))).collect()
    }

    fn has(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn open(bytes: Vec<u8>) -> Document {
        Document::open_with_password(Arc::new(bytes), None).unwrap()
    }

    /// The decoded content of a layer, with `from` replaced by `to` and written back.
    fn replace_layer(doc: &mut Document, r: ObjRef, from: &[u8], to: &[u8]) {
        let (dict, raw) = match &*doc.resolve(&Object::Ref(r)) {
            Object::Stream(s) => (s.dict.clone(), s.decoded().unwrap()),
            _ => panic!("not a stream"),
        };
        let mut raw = raw;
        let at = raw.windows(from.len()).position(|w| w == from).unwrap();
        raw.splice(at..at + from.len(), to.iter().copied());
        doc.set(r, Object::Stream(Stream::from_raw(stripped(&dict), raw)));
    }

    #[test]
    fn shows_text_reads_operators_not_string_contents() {
        assert!(shows_text(b"BT /F1 10 Tf 5 50 Td (Signed by Test) Tj ET"));
        assert!(!shows_text(b"0.8 0.1 0.1 RG 3 w 5 5 m 20 5 l S"));
        assert!(!shows_text(b"0 g 0 0 100 100 re f"));
        // Operator-like bytes inside strings never count, in either direction.
        assert!(!shows_text(b"(a Tj of irony)"));
        assert!(shows_text(b"BT (m) Tj ET"));
        assert!(!shows_text(b"(re)"));
        assert!(!shows_text(b""));
        // `)` always delimits, so a string glued to its operator still tokenises.
        assert!(shows_text(b"(a)Tj"));
        assert!(shows_text(b"((a) b \\( c) Tj"));
        assert!(!shows_text(b"(abc"));
    }

    #[test]
    fn only_non_framing_drawings_count_as_the_icon() {
        // Rectangles however stroked: fills, single and nested borders, panels.
        assert!(is_only_rectangles(b"0 g 0 0 100 100 re f"));
        assert!(is_only_rectangles(b"0 G 2 w 0 0 100 100 re S 10 10 80 80 re S"));
        assert!(is_only_rectangles(b"1 1 m 99 1 l 99 99 l 1 99 l h S"));
        assert!(is_only_rectangles(b"1 1 m 99 1 l 99 99 l 1 99 l 1 1 l S"));
        assert!(is_only_rectangles(b"0 0 m 50 0 l 100 0 l 100 100 l 0 100 l h S"));
        assert!(is_only_rectangles(b"0 0 m 50 0 l 50 30 l 0 30 l 0 0 l 60 0 m 100 0 l 100 30 l 60 30 l 60 0 l S"));
        // Curves, diagonals and open paths are content, not framing.
        assert!(!is_only_rectangles(b"5 5 m 20 5 l 12 22 l h S"));
        assert!(!is_only_rectangles(b"0 0 m 50 30 l S 0 30 m 50 0 l S"));
        assert!(!is_only_rectangles(b"0 0 m 10 0 l 10 0 20 10 20 20 c S"));
        assert!(!is_only_rectangles(b"0 G 1 w 0 0 m 50 30 l S"));
        assert!(!is_only_rectangles(b"0 0 m 100 0 l S"));
        // Operator words inside strings never count: without blanking, the `c`
        // below would read as a curve.
        assert!(is_only_rectangles(b"0 0 10 10 re S (x c y)"));
        // Text-only layers carry no drawing, and malformed numbers exclude the
        // layer rather than guessing: both fall back, never misidentify.
        assert!(is_only_rectangles(b"BT (99 99) Tj ET"));
        assert!(is_only_rectangles(b"5 m"));
        assert!(is_only_rectangles(b"a b m"));
        assert!(is_only_rectangles(b"q 0 G 2 w Q"));
        assert!(is_only_rectangles(b""));
    }

    #[test]
    fn the_predicate_tracks_slots_not_verdicts() {
        let doc = &open(layered_fixture());
        assert!(has_substitutable_appearances(doc, &[verdict("Signature1", Status::Valid)]));
        assert!(!has_substitutable_appearances(doc, &[verdict("Signature1", Status::Unknown)]));
        assert!(!has_substitutable_appearances(doc, &[verdict("NoSuchField", Status::Valid)]));
        assert!(!has_substitutable_appearances(doc, &[]));
    }

    #[test]
    fn a_valid_verdict_rewrites_the_icon_and_caption_layers() {
        let doc = &mut open(layered_fixture());
        let (icon, caption) = slots(doc, "Signature1");
        let icon_before = decoded(doc, icon);
        // Read the untouched layers before substituting: afterwards the markers
        // the selection looks for are gone, and re-reading would compare the
        // rewritten bytes to themselves.
        let before: Vec<(ObjRef, Vec<u8>)> = all_layers(doc, "Signature1").into_iter().filter(|(r, _)| *r != icon && *r != caption).collect();
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(show_valid_appearances(doc, &mut sigs));
        assert!(sigs[0].verdict_shown);
        let after = decoded(doc, caption);
        assert!(has(&after, b"Signature valid"));
        assert!(has(&after, b"1 0 0 1"), "the caption positions with a full text matrix");
        assert!(!has(&after, b"Not Verified"));
        let check = decoded(doc, icon);
        assert_ne!(check, icon_before);
        assert!(check.starts_with(b"q "), "graphics state stays wrapped");
        assert!(has(&check, b"RG"));
        // Everything else keeps its bytes.
        for (r, bytes) in &before {
            assert_eq!(&decoded(doc, *r), bytes, "layer {r:?} untouched");
        }
    }

    #[test]
    fn an_unvalidated_signature_keeps_its_appearance() {
        let doc = &mut open(layered_fixture());
        let (_, caption) = slots(doc, "Signature1");
        let mut sigs = vec![verdict("Signature1", Status::Unknown)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"Not Verified"));
    }

    #[test]
    fn an_appearance_without_layers_is_left_alone() {
        let doc = &mut open(layered_fixture());
        let mut sigs = vec![verdict("NoSuchField", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown);
    }

    #[test]
    fn a_caption_that_already_shows_its_verdict_is_left_alone() {
        let doc = &mut open(layered_fixture());
        let (_, caption) = slots(doc, "Signature1");
        replace_layer(doc, caption, b"(Signature Not Verified)", b"(Signature valid)");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "nothing left to rewrite");
    }

    #[test]
    fn a_caption_in_another_language_is_left_alone() {
        let doc = &mut open(layered_fixture());
        let (_, caption) = slots(doc, "Signature1");
        replace_layer(doc, caption, b"(Signature Not Verified)", b"(Signature non v\xe9rifi\xe9e)");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"v\xe9rifi\xe9e"));
    }

    #[test]
    fn a_large_photo_background_does_not_win_the_icon_slot() {
        let doc = &mut open(layered_fixture());
        // A photo far bigger than the icon, referenced from the frame.
        let photo = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"Type".to_vec(), Object::name("XObject"));
                d.set(b"Subtype".to_vec(), Object::name("Image"));
                d.set(b"Width".to_vec(), Object::Int(200));
                d.set(b"Height".to_vec(), Object::Int(200));
                d.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
                d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
                d
            },
            vec![0u8; 200 * 200 * 3],
        )));
        doc.update_dict(ObjRef::new(7, 0), |d| {
            let res =
                d.get_mut(b"Resources").and_then(|r| r.as_dict_mut()).and_then(|r| r.get_mut(b"XObject")).and_then(|x| x.as_dict_mut()).unwrap();
            res.set(b"img".to_vec(), Object::Ref(photo));
        })
        .unwrap();
        let (_, caption) = slots(doc, "Signature1");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(show_valid_appearances(doc, &mut sigs));
        assert!(sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"Signature valid"));
    }

    #[test]
    fn layers_shared_with_another_field_are_left_alone() {
        let doc = &mut open(layered_fixture());
        // A second field whose appearance reuses Signature1's icon and caption.
        let (icon, caption) = slots(doc, "Signature1");
        let frm = doc.add(form_with_ops(vec![(b"glyph", icon), (b"cap", caption)], "[0 0 50 30]", "q /glyph Do /cap Do Q"));
        let top = doc.add(form_with_ops(vec![(b"frame", frm)], "[0 0 50 30]", "q /frame Do Q"));
        let widget = doc.add(Object::Dict({
            let mut d = Dict::new();
            d.set(b"Type".to_vec(), Object::name("Annot"));
            d.set(b"Subtype".to_vec(), Object::name("Widget"));
            d.set(b"FT".to_vec(), Object::name("Sig"));
            d.set(b"T".to_vec(), Object::String(pdfcraft_cos::PdfString::literal(b"Signature2".to_vec())));
            d.set(b"Rect".to_vec(), Object::Array(vec![Object::Int(10), Object::Int(70), Object::Int(110), Object::Int(120)]));
            let mut ap = Dict::new();
            ap.set(b"N".to_vec(), Object::Ref(top));
            d.set(b"AP".to_vec(), Object::Dict(ap));
            d
        }));
        let mut fields = doc
            .get(ObjRef::new(5, 0))
            .as_dict()
            .and_then(|d| d.get(b"Fields"))
            .map(|f| doc.resolve(f))
            .and_then(|f| f.as_array().cloned())
            .unwrap_or_default();
        fields.push(Object::Ref(widget));
        doc.update_dict(ObjRef::new(5, 0), |d| {
            d.set(b"Fields".to_vec(), Object::Array(fields));
        })
        .unwrap();
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let mut sigs = vec![verdict("Signature1", Status::Valid), verdict("Signature2", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown && !sigs[1].verdict_shown, "shared streams stay untouched");
        assert_eq!(decoded(doc, caption), caption_before);
    }

    #[test]
    fn a_shared_caption_with_a_mixed_verdict_is_left_alone() {
        let doc = &mut open(layered_fixture());
        // Same sharing as above, but only the first field validated: rewriting the
        // shared caption for it would repaint the other field's unverified state.
        let (icon, caption) = slots(doc, "Signature1");
        let frm = doc.add(form_with_ops(vec![(b"glyph", icon), (b"cap", caption)], "[0 0 50 30]", "q /glyph Do /cap Do Q"));
        let top = doc.add(form_with_ops(vec![(b"frame", frm)], "[0 0 50 30]", "q /frame Do Q"));
        let widget = doc.add(Object::Dict({
            let mut d = Dict::new();
            d.set(b"Type".to_vec(), Object::name("Annot"));
            d.set(b"Subtype".to_vec(), Object::name("Widget"));
            d.set(b"FT".to_vec(), Object::name("Sig"));
            d.set(b"T".to_vec(), Object::String(pdfcraft_cos::PdfString::literal(b"Signature2".to_vec())));
            d.set(b"Rect".to_vec(), Object::Array(vec![Object::Int(10), Object::Int(70), Object::Int(110), Object::Int(120)]));
            let mut ap = Dict::new();
            ap.set(b"N".to_vec(), Object::Ref(top));
            d.set(b"AP".to_vec(), Object::Dict(ap));
            d
        }));
        let mut fields = doc
            .get(ObjRef::new(5, 0))
            .as_dict()
            .and_then(|d| d.get(b"Fields"))
            .map(|f| doc.resolve(f))
            .and_then(|f| f.as_array().cloned())
            .unwrap_or_default();
        fields.push(Object::Ref(widget));
        doc.update_dict(ObjRef::new(5, 0), |d| {
            d.set(b"Fields".to_vec(), Object::Array(fields));
        })
        .unwrap();
        let caption_before = decoded(doc, caption);
        let icon_before = decoded(doc, icon);
        let mut sigs = vec![verdict("Signature1", Status::Valid), verdict("Signature2", Status::Unknown)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown && !sigs[1].verdict_shown, "a shared stream serves both verdicts");
        assert_eq!(decoded(doc, caption), caption_before);
        assert_eq!(decoded(doc, icon), icon_before);
    }

    #[test]
    fn a_decorative_border_longer_than_the_icon_is_preserved() {
        let doc = &mut open(layered_fixture());
        // Four nested stroked rectangles, deliberately longer than the glyph stream:
        // length alone must not promote decoration to the icon slot.
        let border = ObjRef::new(12, 0);
        let rects = b"0 G 2 w 0 0 100 100 re S 10 10 80 80 re S 20 20 60 60 re S 30 30 40 40 re S".to_vec();
        let dict = match &*doc.resolve(&Object::Ref(border)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("border is a stream"),
        };
        doc.set(border, Object::Stream(Stream::from_raw(stripped(&dict), rects)));
        let border_before = decoded(doc, border);
        assert!(border_before.len() > decoded(doc, slots(doc, "Signature1").0).len(), "the decoy is longer than the glyph");
        let (_, caption) = slots(doc, "Signature1");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(show_valid_appearances(doc, &mut sigs));
        assert!(sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"Signature valid"));
        assert_eq!(decoded(doc, border), border_before, "decoration untouched");
    }

    #[test]
    fn two_captions_with_the_template_are_left_alone() {
        let doc = &mut open(layered_fixture());
        // A second caption still carrying the unverified template: rewriting only the
        // first would leave the widget showing both states at once, so there is no
        // slot and the page keeps the file's appearance plus the overlay badge.
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let extra = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(21), Object::Int(50), Object::Int(30)]));
                let mut res = Dict::new();
                let mut fonts = Dict::new();
                fonts.set(
                    b"F1".to_vec(),
                    Object::Dict({
                        let mut f = Dict::new();
                        f.set(b"Type".to_vec(), Object::name("Font"));
                        f.set(b"Subtype".to_vec(), Object::name("Type1"));
                        f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
                        f
                    }),
                );
                res.set(b"Font".to_vec(), Object::Dict(fonts));
                d.set(b"Resources".to_vec(), Object::Dict(res));
                d
            },
            b"BT /F1 9 Tf 2 22 Td (Signature Not Verified) Tj ET".to_vec(),
        )));
        doc.update_dict(ObjRef::new(7, 0), |d| {
            let res =
                d.get_mut(b"Resources").and_then(|r| r.as_dict_mut()).and_then(|r| r.get_mut(b"XObject")).and_then(|x| x.as_dict_mut()).unwrap();
            res.set(b"second-caption", Object::Ref(extra));
        })
        .unwrap();
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "two templates leave no single caption");
        assert_eq!(decoded(doc, caption), caption_before);
        assert!(has(&decoded(doc, extra), b"Not Verified"), "the other caption is untouched too");
    }

    #[test]
    fn an_icon_slot_with_no_drawing_is_left_alone() {
        let doc = &mut open(layered_fixture());
        // The glyph redrawn as a bare rectangle: with no drawn line or curve left,
        // no layer qualifies as the icon, so nothing rewrites.
        let glyph = ObjRef::new(10, 0);
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let dict = match &*doc.resolve(&Object::Ref(glyph)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("glyph is a stream"),
        };
        doc.set(glyph, Object::Stream(Stream::from_raw(stripped(&dict), b"0 G 2 w 0 0 50 30 re S".to_vec())));
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "no drawing leaves no icon slot");
        assert_eq!(decoded(doc, caption), caption_before);
    }

    #[test]
    fn nested_line_rectangles_are_not_the_icon() {
        let doc = &mut open(layered_fixture());
        // Borders drawn as nested line rectangles rather than `re`: every piece
        // is still a frame, so a glyph reduced to a bare rectangle alongside
        // still leaves no icon slot.
        let glyph = ObjRef::new(10, 0);
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let dict = match &*doc.resolve(&Object::Ref(glyph)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("glyph is a stream"),
        };
        doc.set(glyph, Object::Stream(Stream::from_raw(stripped(&dict), b"0 G 2 w 0 0 50 30 re S".to_vec())));
        let glyph_before = decoded(doc, glyph);
        let border = ObjRef::new(12, 0);
        let dict = match &*doc.resolve(&Object::Ref(border)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("border is a stream"),
        };
        doc.set(
            border,
            Object::Stream(Stream::from_raw(stripped(&dict), b"0 G 1 w 5 5 m 95 5 l 95 95 l 5 95 l h 15 15 m 85 15 l 85 85 l 15 85 l h S".to_vec())),
        );
        let border_before = decoded(doc, border);
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "nested frames are still frames");
        assert_eq!(decoded(doc, border), border_before);
        assert_eq!(decoded(doc, glyph), glyph_before);
        assert_eq!(decoded(doc, caption), caption_before);
    }

    #[test]
    fn a_line_drawn_border_is_not_the_icon() {
        let doc = &mut open(layered_fixture());
        // The reviewer's case: the glyph becomes a form invoking an image, and the
        // border a line-drawn rectangle. The border is then the only line/curve
        // drawing, but a frame-spanning rectangle is decoration, not the status
        // mark, so nothing rewrites.
        let image = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"Type".to_vec(), Object::name("XObject"));
                d.set(b"Subtype".to_vec(), Object::name("Image"));
                d.set(b"Width".to_vec(), Object::Int(8));
                d.set(b"Height".to_vec(), Object::Int(8));
                d.set(b"ColorSpace".to_vec(), Object::name("DeviceRGB"));
                d.set(b"BitsPerComponent".to_vec(), Object::Int(8));
                d
            },
            vec![0u8; 8 * 8 * 3],
        )));
        let glyph = ObjRef::new(10, 0);
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        doc.set(
            glyph,
            Object::Stream(Stream::from_raw(
                {
                    let mut d = Dict::new();
                    d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(50), Object::Int(30)]));
                    let mut res = Dict::new();
                    let mut xo = Dict::new();
                    xo.set(b"im".to_vec(), Object::Ref(image));
                    res.set(b"XObject".to_vec(), Object::Dict(xo));
                    d.set(b"Resources".to_vec(), Object::Dict(res));
                    d
                },
                b"q /im Do Q".to_vec(),
            )),
        );
        let border = ObjRef::new(12, 0);
        let dict = match &*doc.resolve(&Object::Ref(border)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("border is a stream"),
        };
        doc.set(border, Object::Stream(Stream::from_raw(stripped(&dict), b"q 0 G 1 w 1 1 m 99 1 l 99 99 l 1 99 l h S Q".to_vec())));
        let border_before = decoded(doc, border);
        let glyph_before = decoded(doc, glyph);
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "a lone frame-spanning drawing is decoration");
        assert_eq!(decoded(doc, border), border_before);
        assert_eq!(decoded(doc, glyph), glyph_before);
        assert_eq!(decoded(doc, caption), caption_before);
    }

    #[test]
    fn a_line_drawn_border_with_a_rectangle_glyph_is_not_the_icon() {
        let doc = &mut open(layered_fixture());
        // Belt and braces on the above: even with the glyph reduced to a bare
        // rectangle, the line-drawn frame-spanning border must not become the icon.
        let glyph = ObjRef::new(10, 0);
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let dict = match &*doc.resolve(&Object::Ref(glyph)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("glyph is a stream"),
        };
        doc.set(glyph, Object::Stream(Stream::from_raw(stripped(&dict), b"0 G 2 w 0 0 50 30 re S".to_vec())));
        let glyph_before = decoded(doc, glyph);
        let border = ObjRef::new(12, 0);
        let dict = match &*doc.resolve(&Object::Ref(border)) {
            Object::Stream(s) => s.dict.clone(),
            _ => panic!("border is a stream"),
        };
        doc.set(border, Object::Stream(Stream::from_raw(stripped(&dict), b"q 0 G 1 w 1 1 m 99 1 l 99 99 l 1 99 l h S Q".to_vec())));
        let border_before = decoded(doc, border);
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "spanning the frame disqualifies the drawing");
        assert_eq!(decoded(doc, border), border_before);
        assert_eq!(decoded(doc, glyph), glyph_before);
        assert_eq!(decoded(doc, caption), caption_before);
    }

    #[test]
    fn competing_drawn_layers_fall_back_to_the_overlay() {
        let doc = &mut open(layered_fixture());
        // A second drawn layer (lines, not rectangles) alongside the glyph: with two
        // icon candidates neither is unambiguous, so nothing rewrites and the page
        // keeps the file's appearance plus the overlay badge.
        let (_, caption) = slots(doc, "Signature1");
        let caption_before = decoded(doc, caption);
        let extra = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(50), Object::Int(30)]));
                d
            },
            b"0 G 1 w 0 0 m 50 30 l S 0 30 m 50 0 l S".to_vec(),
        )));
        doc.update_dict(ObjRef::new(7, 0), |d| {
            let res =
                d.get_mut(b"Resources").and_then(|r| r.as_dict_mut()).and_then(|r| r.get_mut(b"XObject")).and_then(|x| x.as_dict_mut()).unwrap();
            res.set(b"decoy", Object::Ref(extra));
        })
        .unwrap();
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(!show_valid_appearances(doc, &mut sigs));
        assert!(!sigs[0].verdict_shown, "ambiguity falls back to the badge");
        assert_eq!(decoded(doc, caption), caption_before);
    }

    /// A form XObject naming `entries`, with `ops` as its content.
    fn form_with_ops(entries: Vec<(&[u8], ObjRef)>, bbox: &str, ops: &str) -> Object {
        let mut d = Dict::new();
        d.set(b"BBox".to_vec(), Object::Array(bbox[1..bbox.len() - 1].split(' ').map(|n| Object::Int(n.parse().unwrap())).collect()));
        let mut res = Dict::new();
        let mut xo = Dict::new();
        for (k, r) in entries {
            xo.set(k.to_vec(), Object::Ref(r));
        }
        res.set(b"XObject".to_vec(), Object::Dict(xo));
        d.set(b"Resources".to_vec(), Object::Dict(res));
        Object::Stream(Stream::from_raw(d, ops.as_bytes().to_vec()))
    }

    #[test]
    fn every_widget_of_a_field_rewrites_together() {
        let doc = &mut open(layered_fixture());
        // A second widget on the same field with its own icon and caption layers.
        let icon2 = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(50), Object::Int(30)]));
                d
            },
            b"0.8 0.1 0.1 RG 3 w 1 1 m 10 10 l S".to_vec(),
        )));
        let caption2 = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(21), Object::Int(50), Object::Int(30)]));
                let mut res = Dict::new();
                let mut fonts = Dict::new();
                fonts.set(
                    b"F1".to_vec(),
                    Object::Dict({
                        let mut f = Dict::new();
                        f.set(b"Type".to_vec(), Object::name("Font"));
                        f.set(b"Subtype".to_vec(), Object::name("Type1"));
                        f.set(b"BaseFont".to_vec(), Object::name("Helvetica"));
                        f
                    }),
                );
                res.set(b"Font".to_vec(), Object::Dict(fonts));
                d.set(b"Resources".to_vec(), Object::Dict(res));
                d
            },
            b"BT /F1 9 Tf 2 22 Td (Signature Not Verified) Tj ET".to_vec(),
        )));
        let frm2 = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(50), Object::Int(30)]));
                let mut res = Dict::new();
                let mut xo = Dict::new();
                xo.set(b"second-icon".to_vec(), Object::Ref(icon2));
                xo.set(b"second-caption".to_vec(), Object::Ref(caption2));
                res.set(b"XObject".to_vec(), Object::Dict(xo));
                d.set(b"Resources".to_vec(), Object::Dict(res));
                d
            },
            b"q /second-icon Do /second-caption Do Q".to_vec(),
        )));
        let top2 = doc.add(Object::Stream(Stream::from_raw(
            {
                let mut d = Dict::new();
                d.set(b"BBox".to_vec(), Object::Array(vec![Object::Int(0), Object::Int(0), Object::Int(50), Object::Int(30)]));
                let mut res = Dict::new();
                let mut xo = Dict::new();
                xo.set(b"second-frame".to_vec(), Object::Ref(frm2));
                res.set(b"XObject".to_vec(), Object::Dict(xo));
                d.set(b"Resources".to_vec(), Object::Dict(res));
                d
            },
            b"q /second-frame Do Q".to_vec(),
        )));
        let widget2 = doc.add(Object::Dict({
            let mut d = Dict::new();
            d.set(b"Type".to_vec(), Object::name("Annot"));
            d.set(b"Subtype".to_vec(), Object::name("Widget"));
            d.set(b"Rect".to_vec(), Object::Array(vec![Object::Int(10), Object::Int(70), Object::Int(110), Object::Int(120)]));
            let mut ap = Dict::new();
            ap.set(b"N".to_vec(), Object::Ref(top2));
            d.set(b"AP".to_vec(), Object::Dict(ap));
            d
        }));
        let mut kids = doc
            .get(ObjRef::new(4, 0))
            .as_dict()
            .and_then(|d| d.get(b"Kids"))
            .map(|k| doc.resolve(k))
            .and_then(|k| k.as_array().cloned())
            .unwrap_or_default();
        kids.push(Object::Ref(widget2));
        doc.update_dict(ObjRef::new(4, 0), |d| {
            d.set(b"Kids".to_vec(), Object::Array(kids));
        })
        .unwrap();
        let (_, caption) = slots(doc, "Signature1");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(show_valid_appearances(doc, &mut sigs));
        assert!(sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"Signature valid"));
    }

    #[test]
    fn substitution_survives_document_encryption() {
        use pdfcraft_cos::{Algorithm, NewEncryption, SaveOptions, write_full, write_incremental};
        let mut doc = open(layered_fixture());
        doc.set_encryption(&NewEncryption {
            algorithm: Algorithm::Aes128,
            user_password: "user",
            owner_password: "owner",
            permissions: -1,
            encrypt_metadata: true,
            seed: [29; 32],
        })
        .unwrap();
        let bytes = Arc::new(write_full(&doc, &SaveOptions::default()).unwrap());
        let doc = &mut Document::open_with_password(bytes, Some("user")).unwrap();
        let (_, caption) = slots(doc, "Signature1");
        let mut sigs = vec![verdict("Signature1", Status::Valid)];
        assert!(show_valid_appearances(doc, &mut sigs));
        assert!(sigs[0].verdict_shown);
        assert!(has(&decoded(doc, caption), b"Signature valid"));
        // The display copy writes back out as an openable, still-encrypted file
        // carrying the substituted caption: this is the exact display path.
        let shown = write_incremental(doc, &SaveOptions::default()).unwrap();
        let shown = Document::open_with_password(Arc::new(shown), Some("user")).unwrap();
        let caption_bytes = decoded(&shown, caption);
        assert!(has(&caption_bytes, b"Signature valid"));
        assert!(!has(&caption_bytes, b"Not Verified"));
    }
}
