//! Reading `OFD.xml`, `Document.xml`, resource files and page content into the model.
//!
//! Elements are matched by local name, so files with or without the `ofd:` namespace prefix read
//! the same. Unknown elements are ignored. A required part, element or attribute that is missing
//! or unreadable is an error naming the part; optional things that can't be used yet (shadings,
//! composite units, more than one `DocBody`) are recorded in [`OfdDocument::warnings`].

use roxmltree::{Document as Xml, Node, ParsingOptions};

use crate::geom::OfdBox;
use crate::model::*;
use crate::values;
use crate::zip::Package;
use crate::{Limits, OfdError};

/// Resolve `loc` (`ST_Loc`) against the directory `base` (both package paths). A leading `/`
/// starts from the package root. Leaving the package with `..` is an error.
pub(crate) fn resolve(base: &str, loc: &str) -> Result<String, OfdError> {
    let loc = loc.trim().replace('\\', "/");
    if loc.is_empty() {
        return Err(OfdError::Invalid { part: base.to_string(), msg: "an empty file reference".into() });
    }
    let mut parts: Vec<&str> = Vec::new();
    let (start, rest) = match loc.strip_prefix('/') {
        Some(rest) => ("", rest),
        None => (base, loc.as_str()),
    };
    for seg in start.split('/').chain(rest.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(OfdError::Invalid { part: base.to_string(), msg: format!("{loc:?} points outside the package") });
                }
            }
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

/// The directory part of a package path ("" for the root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn is(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn child<'a, 'i>(node: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| is(n, name))
}

fn children<'a, 'i: 'a>(node: &Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children().filter(move |n| is(n, name))
}

/// All the text inside an element, trimmed.
fn text(node: &Node) -> String {
    node.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect::<String>().trim().to_string()
}

fn text_opt(node: &Node, name: &str) -> Option<String> {
    child(node, name).map(|n| text(&n)).filter(|s| !s.is_empty())
}

/// One XML part being read: its name for messages, and the limits.
struct Part<'l> {
    name: String,
    limits: &'l Limits,
}

impl Part<'_> {
    fn err(&self, msg: impl Into<String>) -> OfdError {
        OfdError::Invalid { part: self.name.clone(), msg: msg.into() }
    }

    fn at(&self, node: &Node, msg: String) -> OfdError {
        self.err(format!("<{}>: {msg}", node.tag_name().name()))
    }

    fn required<'n>(&self, node: &Node<'n, '_>, attr: &str) -> Result<&'n str, OfdError> {
        node.attribute(attr).ok_or_else(|| self.at(node, format!("the {attr} attribute is missing")))
    }

    fn id(&self, node: &Node, attr: &str) -> Result<Option<u32>, OfdError> {
        node.attribute(attr).map(|v| v.trim().parse::<u32>().map_err(|_| self.at(node, format!("{attr} {v:?} is not an ID")))).transpose()
    }

    fn required_id(&self, node: &Node, attr: &str) -> Result<u32, OfdError> {
        self.id(node, attr)?.ok_or_else(|| self.at(node, format!("the {attr} attribute is missing")))
    }

    fn num(&self, node: &Node, attr: &str) -> Result<Option<f64>, OfdError> {
        node.attribute(attr).map(|v| values::number(v).map_err(|e| self.at(node, format!("{attr}: {e}")))).transpose()
    }

    fn int<T: std::str::FromStr>(&self, node: &Node, attr: &str, default: T) -> Result<T, OfdError> {
        match node.attribute(attr) {
            None => Ok(default),
            Some(v) => v.trim().parse().map_err(|_| self.at(node, format!("{attr} {v:?} is not a whole number in range"))),
        }
    }

    fn flag(&self, node: &Node, attr: &str, default: bool) -> Result<bool, OfdError> {
        match node.attribute(attr).map(str::trim) {
            None => Ok(default),
            Some("true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            Some(v) => Err(self.at(node, format!("{attr} {v:?} is not true or false"))),
        }
    }

    fn boxed(&self, node: &Node, s: &str) -> Result<OfdBox, OfdError> {
        values::ofd_box(s, self.limits).map_err(|e| self.at(node, e))
    }
}

/// Parse an XML part with the DTD refused and the node count bounded.
fn xml<'t>(name: &str, text: &'t str, limits: &Limits) -> Result<Xml<'t>, OfdError> {
    let opt = ParsingOptions { allow_dtd: false, nodes_limit: limits.max_xml_nodes };
    Xml::parse_with_options(text, opt).map_err(|e| OfdError::Xml { part: name.to_string(), msg: e.to_string() })
}

/// The text of an XML entry: UTF-8 (a BOM is skipped), within `max_xml_size`.
fn xml_text(pkg: &Package, name: &str, limits: &Limits) -> Result<String, OfdError> {
    let bytes = pkg.read(name)?;
    if bytes.len() as u64 > limits.max_xml_size {
        return Err(OfdError::Limit(format!("{name} is larger than {} bytes", limits.max_xml_size)));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").map(<[u8]>::to_vec).unwrap_or(bytes);
    String::from_utf8(bytes).map_err(|_| OfdError::Xml { part: name.to_string(), msg: "the file is not UTF-8 text".into() })
}

struct Reader<'p, 'b> {
    pkg: &'p Package<'b>,
    limits: Limits,
    resources: Resources,
    /// Resource files already read (pages often share one).
    res_read: std::collections::HashSet<String>,
    warnings: Vec<String>,
    /// What the document has used so far of the document-wide limits.
    xml_read: u64,
    objects: usize,
    model_bytes: usize,
    text_chars: usize,
    /// Patterns being read, innermost last (a pattern's cells may use patterns too).
    pattern_depth: usize,
}

pub(crate) fn document(pkg: &Package, limits: &Limits) -> Result<OfdDocument, OfdError> {
    let entry = "OFD.xml";
    if !pkg.contains(entry) {
        return Err(OfdError::NotOfd("the ZIP package has no OFD.xml at its root".into()));
    }
    let mut r = Reader {
        pkg,
        limits: limits.clone(),
        resources: Resources::default(),
        res_read: Default::default(),
        warnings: Vec::new(),
        xml_read: 0,
        objects: 0,
        model_bytes: 0,
        text_chars: 0,
        pattern_depth: 0,
    };
    let src = r.read_xml(entry)?;
    let doc = xml(entry, &src, limits)?;
    let part = Part { name: entry.into(), limits };
    let root = doc.root_element();
    if !is(&root, "OFD") {
        return Err(part.err(format!("the root element is <{}>, not <OFD>", root.tag_name().name())));
    }
    let bodies: Vec<Node> = children(&root, "DocBody").collect();
    let body = bodies.first().ok_or_else(|| part.err("there is no <DocBody>"))?;
    if bodies.len() > 1 {
        r.warnings.push(format!("OFD.xml has {} documents; only the first is read", bodies.len()));
    }
    let info = child(body, "DocInfo").map_or_else(DocInfo::default, |i| DocInfo {
        doc_id: text_opt(&i, "DocID"),
        title: text_opt(&i, "Title"),
        author: text_opt(&i, "Author"),
        subject: text_opt(&i, "Subject"),
        creator: text_opt(&i, "Creator"),
        creation_date: text_opt(&i, "CreationDate"),
        mod_date: text_opt(&i, "ModDate"),
    });
    let doc_root = text_opt(body, "DocRoot").ok_or_else(|| part.err("<DocBody> has no <DocRoot>"))?;
    let doc_root = resolve("", &doc_root)?;
    let signatures = text_opt(body, "Signatures").map(|s| resolve("", &s)).transpose()?;
    let version = root.attribute("Version").unwrap_or_default().to_string();
    let doc_type = root.attribute("DocType").unwrap_or_default().to_string();
    let (page_area, templates, pages) = r.doc_xml(&doc_root)?;
    r.check_references(&templates, &pages);
    Ok(OfdDocument {
        version,
        doc_type,
        doc_bodies: bodies.len(),
        info,
        doc_root,
        signatures,
        page_area,
        templates,
        pages,
        resources: r.resources,
        warnings: r.warnings,
    })
}

type DocParts = (Option<PageArea>, Vec<TemplatePage>, Vec<Page>);

impl Reader<'_, '_> {
    /// The text of an XML part, counted against [`Limits::max_xml_total`].
    fn read_xml(&mut self, name: &str) -> Result<String, OfdError> {
        let src = xml_text(self.pkg, name, &self.limits)?;
        self.xml_read = self.xml_read.saturating_add(src.len() as u64);
        if self.xml_read > self.limits.max_xml_total {
            return Err(OfdError::Limit(format!("the document has more than {} bytes of XML to read", self.limits.max_xml_total)));
        }
        Ok(src)
    }

    /// Count what the model keeps against the document-wide limits.
    fn charge(&mut self, part: &Part, objects: usize, bytes: usize, chars: usize) -> Result<(), OfdError> {
        self.objects = self.objects.saturating_add(objects);
        self.model_bytes = self.model_bytes.saturating_add(bytes);
        self.text_chars = self.text_chars.saturating_add(chars);
        let l = &self.limits;
        if self.objects > l.max_objects {
            return Err(OfdError::Limit(format!("{}: the document has more than {} objects", part.name, l.max_objects)));
        }
        if self.model_bytes > l.max_model_bytes {
            return Err(OfdError::Limit(format!("{}: the document's content takes more than {} bytes to hold", part.name, l.max_model_bytes)));
        }
        if self.text_chars > l.max_text_chars {
            return Err(OfdError::Limit(format!("{}: the document has more than {} characters of text", part.name, l.max_text_chars)));
        }
        Ok(())
    }

    fn doc_xml(&mut self, path: &str) -> Result<DocParts, OfdError> {
        let src = self.read_xml(path)?;
        let doc = xml(path, &src, &self.limits)?;
        let limits = self.limits.clone();
        let part = Part { name: path.into(), limits: &limits };
        let root = doc.root_element();
        if !is(&root, "Document") {
            return Err(part.err(format!("the root element is <{}>, not <Document>", root.tag_name().name())));
        }
        let dir = dir_of(path);
        let common = child(&root, "CommonData").ok_or_else(|| part.err("there is no <CommonData>"))?;
        let page_area = match child(&common, "PageArea") {
            Some(a) => self.page_area(&part, &a)?,
            None => None,
        };
        for kind in ["PublicRes", "DocumentRes"] {
            for res in children(&common, kind) {
                let loc = resolve(dir, &text(&res))?;
                self.res_file(&loc)?;
            }
        }
        let mut templates = Vec::new();
        for t in children(&common, "TemplatePage") {
            let id = part.required_id(&t, "ID")?;
            let location = resolve(dir, part.required(&t, "BaseLoc")?)?;
            let z_order = z_order(&part, &t)?.unwrap_or(ZOrder::Background);
            let content = self.page_file(&location)?;
            templates.push(TemplatePage { id, name: t.attribute("Name").map(str::to_string), z_order, location, content });
        }
        let pages_node = child(&root, "Pages").ok_or_else(|| part.err("there is no <Pages>"))?;
        let mut pages = Vec::new();
        for p in children(&pages_node, "Page") {
            if pages.len() >= self.limits.max_pages {
                return Err(OfdError::Limit(format!("the document has more than {} pages", self.limits.max_pages)));
            }
            let id = part.required_id(&p, "ID")?;
            let location = resolve(dir, part.required(&p, "BaseLoc")?)?;
            let (content, templates) = self.page_with_templates(&location)?;
            let area = content.area.or(page_area).ok_or_else(|| OfdError::Invalid {
                part: location.clone(),
                msg: "the page has no size (no <Area> and no document <PageArea>)".into(),
            })?;
            pages.push(Page { id, location, area, templates, content, annotations: Vec::new() });
        }
        if pages.is_empty() {
            return Err(part.err("the document has no pages"));
        }
        if let Some(list) = child(&root, "Annotations").or_else(|| child(&common, "Annotations")) {
            let loc = resolve(dir, &text(&list))?;
            self.annotations(&loc, &mut pages)?;
        }
        Ok((page_area, templates, pages))
    }

    /// The annotation list (`Annotations`): each page's annotation file, by page ID. A listed
    /// file that is missing, or a page ID that no page has, is a warning.
    fn annotations(&mut self, path: &str, pages: &mut [Page]) -> Result<(), OfdError> {
        let src = match self.read_xml(path) {
            Err(OfdError::Missing(_)) => {
                self.warn(format!("{path} is listed but missing from the package; annotations are not read"));
                return Ok(());
            }
            other => other?,
        };
        let doc = xml(path, &src, &self.limits)?;
        let limits = self.limits.clone();
        let part = Part { name: path.into(), limits: &limits };
        let root = doc.root_element();
        if !is(&root, "Annotations") {
            return Err(part.err(format!("the root element is <{}>, not <Annotations>", root.tag_name().name())));
        }
        let dir = dir_of(path);
        for p in children(&root, "Page") {
            let id = part.required_id(&p, "PageID")?;
            let loc = text_opt(&p, "FileLoc").ok_or_else(|| part.at(&p, "there is no <FileLoc>".into()))?;
            let loc = resolve(dir, &loc)?;
            let Some(page) = pages.iter_mut().find(|pg| pg.id == id) else {
                self.warn(format!("{path}: annotations for page {id}, which the document doesn't have, are not read"));
                continue;
            };
            let annots = self.page_annotations(&loc)?;
            page.annotations.extend(annots);
        }
        Ok(())
    }

    /// One page's annotation file (`PageAnnot`).
    fn page_annotations(&mut self, path: &str) -> Result<Vec<Annotation>, OfdError> {
        let src = match self.read_xml(path) {
            Err(OfdError::Missing(_)) => {
                self.warn(format!("{path} is listed but missing from the package; its annotations are not read"));
                return Ok(Vec::new());
            }
            other => other?,
        };
        let doc = xml(path, &src, &self.limits)?;
        let limits = self.limits.clone();
        let part = Part { name: path.into(), limits: &limits };
        let root = doc.root_element();
        if !is(&root, "PageAnnot") {
            return Err(part.err(format!("the root element is <{}>, not <PageAnnot>", root.tag_name().name())));
        }
        let mut out = Vec::new();
        for a in children(&root, "Annot") {
            let appearance = child(&a, "Appearance");
            let boundary = match appearance.and_then(|ap| ap.attribute("Boundary")) {
                Some(b) => Some(values::boundary(b, part.limits).map_err(|e| part.at(&a, e))?.0),
                None => None,
            };
            let objects = match appearance {
                Some(ap) => self.objects(&part, &ap, 0)?,
                None => Vec::new(),
            };
            self.charge(&part, 1, size_of::<Annotation>(), 0)?;
            out.push(Annotation {
                kind: a.attribute("Type").unwrap_or_default().to_string(),
                subtype: a.attribute("Subtype").map(str::to_string),
                visible: part.flag(&a, "Visible", true)?,
                print: part.flag(&a, "Print", true)?,
                no_view: part.flag(&a, "NoView", false)?,
                boundary,
                objects,
            });
        }
        Ok(out)
    }

    /// A resource file (`Res`): its `BaseLoc` is where its files live.
    fn res_file(&mut self, path: &str) -> Result<(), OfdError> {
        if !self.res_read.insert(path.to_string()) {
            return Ok(());
        }
        // Some writers list a resource file they never wrote: what it would define is then
        // undefined (fonts fall back), as a reader would treat it.
        let src = match self.read_xml(path) {
            Err(OfdError::Missing(_)) => {
                self.warn(format!("{path} is listed but missing from the package; its resources are not available"));
                return Ok(());
            }
            other => other?,
        };
        let doc = xml(path, &src, &self.limits)?;
        let limits = self.limits.clone();
        let part = Part { name: path.into(), limits: &limits };
        let root = doc.root_element();
        if !is(&root, "Res") {
            return Err(part.err(format!("the root element is <{}>, not <Res>", root.tag_name().name())));
        }
        let dir = dir_of(path);
        let base = match root.attribute("BaseLoc") {
            Some(b) => resolve(dir, b)?,
            None => dir.to_string(),
        };
        for list in root.children().filter(Node::is_element) {
            match list.tag_name().name() {
                "ColorSpaces" => {
                    for cs in children(&list, "ColorSpace") {
                        let id = part.required_id(&cs, "ID")?;
                        let kind = match part.required(&cs, "Type")? {
                            "GRAY" | "Gray" => ColorSpaceKind::Gray,
                            "RGB" => ColorSpaceKind::Rgb,
                            "CMYK" => ColorSpaceKind::Cmyk,
                            other => ColorSpaceKind::Other(other.to_string()),
                        };
                        let bits = part.int(&cs, "BitsPerComponent", 8u8)?;
                        let profile = cs.attribute("Profile").map(|p| resolve(&base, p)).transpose()?;
                        self.add(id, path, |r| &mut r.color_spaces, ColorSpace { kind, bits_per_component: bits, profile });
                    }
                }
                "DrawParams" => {
                    for dp in children(&list, "DrawParam") {
                        let id = part.required_id(&dp, "ID")?;
                        let value = DrawParam {
                            relative: part.id(&dp, "Relative")?,
                            line_width: part.num(&dp, "LineWidth")?,
                            join: dp.attribute("Join").map(str::to_string),
                            cap: dp.attribute("Cap").map(str::to_string),
                            miter_limit: part.num(&dp, "MiterLimit")?,
                            fill_color: self.color(&part, &dp, "FillColor")?,
                            stroke_color: self.color(&part, &dp, "StrokeColor")?,
                        };
                        self.add(id, path, |r| &mut r.draw_params, value);
                    }
                }
                "Fonts" => {
                    for f in children(&list, "Font") {
                        let id = part.required_id(&f, "ID")?;
                        let file = text_opt(&f, "FontFile").map(|p| resolve(&base, &p)).transpose()?;
                        let value = Font {
                            name: part.required(&f, "FontName")?.to_string(),
                            family: f.attribute("FamilyName").map(str::to_string),
                            charset: f.attribute("Charset").map(str::to_string),
                            italic: part.flag(&f, "Italic", false)?,
                            bold: part.flag(&f, "Bold", false)?,
                            serif: part.flag(&f, "Serif", false)?,
                            fixed_width: part.flag(&f, "FixedWidth", false)?,
                            file,
                        };
                        self.add(id, path, |r| &mut r.fonts, value);
                    }
                }
                "MultiMedias" => {
                    for m in children(&list, "MultiMedia") {
                        let id = part.required_id(&m, "ID")?;
                        let file = text_opt(&m, "MediaFile").ok_or_else(|| part.at(&m, "there is no <MediaFile>".into()))?;
                        let value = MultiMedia {
                            kind: part.required(&m, "Type")?.to_string(),
                            format: m.attribute("Format").map(str::to_string),
                            file: resolve(&base, &file)?,
                        };
                        self.add(id, path, |r| &mut r.media, value);
                    }
                }
                "CompositeGraphicUnits" => {
                    for u in children(&list, "CompositeGraphicUnit") {
                        let id = part.required_id(&u, "ID")?;
                        let objects = match child(&u, "Content") {
                            Some(content) => self.objects(&part, &content, 0)?,
                            None => Vec::new(),
                        };
                        let value =
                            CompositeUnit { width: part.num(&u, "Width")?.unwrap_or(0.0), height: part.num(&u, "Height")?.unwrap_or(0.0), objects };
                        self.add(id, path, |r| &mut r.composites, value);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn add<T>(&mut self, id: u32, part: &str, table: fn(&mut Resources) -> &mut std::collections::BTreeMap<u32, T>, value: T) {
        let t = table(&mut self.resources);
        if let std::collections::btree_map::Entry::Vacant(slot) = t.entry(id) {
            slot.insert(value);
        } else {
            self.warnings.push(format!("{part}: resource ID {id} is defined twice; the first is kept"));
        }
    }

    fn page_with_templates(&mut self, path: &str) -> Result<(PageContent, Vec<TemplateRef>), OfdError> {
        let src = self.read_xml(path)?;
        let doc = xml(path, &src, &self.limits)?;
        let limits = self.limits.clone();
        let part = Part { name: path.into(), limits: &limits };
        let root = doc.root_element();
        if !is(&root, "Page") {
            return Err(part.err(format!("the root element is <{}>, not <Page>", root.tag_name().name())));
        }
        let mut refs = Vec::new();
        for t in children(&root, "Template") {
            refs.push(TemplateRef { template_id: part.required_id(&t, "TemplateID")?, z_order: z_order(&part, &t)? });
        }
        let dir = dir_of(path);
        for res in children(&root, "PageRes") {
            let loc = resolve(dir, &text(&res))?;
            self.res_file(&loc)?;
        }
        let area = match child(&root, "Area") {
            Some(a) => self.page_area(&part, &a)?,
            None => None,
        };
        let mut layers = Vec::new();
        if let Some(content) = child(&root, "Content") {
            for l in children(&content, "Layer") {
                let kind = match l.attribute("Type").unwrap_or("Body") {
                    "Body" => LayerKind::Body,
                    "Background" => LayerKind::Background,
                    "Foreground" => LayerKind::Foreground,
                    "Custom" => LayerKind::Custom,
                    other => return Err(part.at(&l, format!("unknown layer type {other:?}"))),
                };
                let objects = self.objects(&part, &l, 0)?;
                layers.push(Layer { id: part.id(&l, "ID")?, kind, draw_param: part.id(&l, "DrawParam")?, objects });
            }
        }
        Ok((PageContent { area, layers }, refs))
    }

    fn page_file(&mut self, path: &str) -> Result<PageContent, OfdError> {
        self.page_with_templates(path).map(|(c, _)| c)
    }

    fn objects(&mut self, part: &Part, parent: &Node, depth: usize) -> Result<Vec<Object>, OfdError> {
        if depth > self.limits.max_depth {
            return Err(OfdError::Limit(format!("{}: page blocks are nested more than {} deep", part.name, self.limits.max_depth)));
        }
        let mut out = Vec::new();
        for n in parent.children().filter(Node::is_element) {
            let name = n.tag_name().name();
            if matches!(name, "ImageObject" | "CompositeObject") && n.attribute("ResourceID").is_none() {
                // Nothing to draw (some writers leave such placeholders in composite units).
                self.warn(format!("{}: an <{name}> without ResourceID is left out", part.name));
                continue;
            }
            let obj = match name {
                "PathObject" => Object::Path(self.path_object(part, &n, true)?),
                "TextObject" => Object::Text(self.text_object(part, &n)?),
                "ImageObject" => Object::Image(ImageObject {
                    graphic: self.graphic(part, &n)?,
                    resource_id: part.required_id(&n, "ResourceID")?,
                    substitution: part.id(&n, "Substitution")?,
                    image_mask: part.id(&n, "ImageMask")?,
                }),
                "CompositeObject" => {
                    Object::Composite(CompositeObject { graphic: self.graphic(part, &n)?, resource_id: part.required_id(&n, "ResourceID")? })
                }
                "PageBlock" => Object::Block(PageBlock { id: part.id(&n, "ID")?, objects: self.objects(part, &n, depth + 1)? }),
                _ => continue,
            };
            self.charge(part, 1, size_of::<Object>(), 0)?;
            out.push(obj);
        }
        Ok(out)
    }

    fn color(&mut self, part: &Part, node: &Node, name: &str) -> Result<Option<Color>, OfdError> {
        let Some(c) = child(node, name) else { return Ok(None) };
        let value = c.attribute("Value").map(|v| values::color_value(v, part.limits).map_err(|e| part.at(&c, e))).transpose()?;
        self.charge(part, 0, size_of::<Color>().saturating_add(value.as_ref().map_or(0, Vec::len).saturating_mul(size_of::<f64>())), 0)?;
        let shading = c.children().any(|k| k.is_element());
        let shade = match (child(&c, "AxialShd"), child(&c, "RadialShd")) {
            (Some(s), _) | (None, Some(s)) => self.shade(part, &s)?,
            (None, None) => None,
        };
        let pattern = match child(&c, "Pattern") {
            Some(p) => self.pattern(part, &p)?,
            None => None,
        };
        if shading && shade.is_none() && pattern.is_none() {
            self.warn(format!("{}: a pattern or shading colour is not read yet", part.name));
        }
        Ok(Some(Color {
            value: value.unwrap_or_default(),
            color_space: part.id(&c, "ColorSpace")?,
            index: part.id(&c, "Index")?,
            alpha: part.int(&c, "Alpha", 255u8)?,
            shading,
            shade,
            pattern,
        }))
    }

    /// A `Pattern`. One that can't be read, or that nests in other patterns' cells more than 4
    /// deep, is left out with a warning.
    fn pattern(&mut self, part: &Part, p: &Node) -> Result<Option<Box<Pattern>>, OfdError> {
        let num = |attr: &str| p.attribute(attr).map(values::number).transpose();
        let (Ok(Some(width)), Ok(Some(height)), Ok(x_step), Ok(y_step)) = (num("Width"), num("Height"), num("XStep"), num("YStep")) else {
            self.warn(format!("{}: a <Pattern> could not be read and is not drawn", part.name));
            return Ok(None);
        };
        let ctm = match p.attribute("CTM").map(|m| values::matrix(m, part.limits)) {
            None => None,
            Some(Ok(m)) => Some(m),
            Some(Err(_)) => {
                self.warn(format!("{}: a <Pattern> could not be read and is not drawn", part.name));
                return Ok(None);
            }
        };
        if self.pattern_depth >= 4 {
            self.warn(format!("{}: patterns nested more than 4 deep are not drawn", part.name));
            return Ok(None);
        }
        self.pattern_depth += 1;
        let objects = match child(p, "CellContent") {
            Some(cell) => self.objects(part, &cell, 0),
            None => Ok(Vec::new()),
        };
        self.pattern_depth -= 1;
        let objects = objects?;
        self.charge(part, 0, size_of::<Pattern>(), 0)?;
        Ok(Some(Box::new(Pattern {
            width,
            height,
            x_step: x_step.unwrap_or(width),
            y_step: y_step.unwrap_or(height),
            reflect: p.attribute("ReflectMethod").is_some_and(|r| r.trim() != "Normal"),
            relative_to_page: p.attribute("RelativeTo").is_some_and(|r| r.trim() == "Page"),
            ctm,
            objects,
        })))
    }

    /// An `AxialShd` or `RadialShd`. One that can't be read is left out with a warning (the
    /// colour stays a shading that isn't drawn) rather than failing the document.
    fn shade(&mut self, part: &Part, s: &Node) -> Result<Option<Box<Shade>>, OfdError> {
        /// More segments than any real blend has.
        const MAX_STOPS: usize = 256;
        let point = |attr: &str| -> Option<(f64, f64)> {
            match *values::numbers(s.attribute(attr)?, part.limits).ok()?.as_slice() {
                [x, y] => Some((x, y)),
                _ => None,
            }
        };
        let num = |attr: &str, default: f64| s.attribute(attr).map_or(Some(default), |v| values::number(v).ok());
        let unreadable = |r: &mut Self| {
            r.warn(format!("{}: a <{}> could not be read and is not drawn", part.name, s.tag_name().name()));
            Ok(None)
        };
        let (Some(start), Some(end)) = (point("StartPoint"), point("EndPoint")) else { return unreadable(self) };
        let kind = if s.tag_name().name() == "RadialShd" {
            match (num("StartRadius", 0.0), s.attribute("EndRadius").and_then(|v| values::number(v).ok()), num("Eccentricity", 0.0)) {
                (Some(start_radius), Some(end_radius), Some(eccentricity)) if start_radius >= 0.0 && end_radius >= 0.0 => {
                    ShadeKind::Radial { start_radius, end_radius, eccentricity }
                }
                _ => return unreadable(self),
            }
        } else {
            ShadeKind::Axial
        };
        let extend = s.attribute("Extend").map_or(Some(0u8), |v| v.trim().parse::<u8>().ok().filter(|e| *e <= 3));
        let Some(extend) = extend else { return unreadable(self) };
        let mut stops = Vec::new();
        for seg in children(s, "Segment") {
            let color = child(&seg, "Color");
            let value = color.and_then(|c| c.attribute("Value")).and_then(|v| values::color_value(v, part.limits).ok());
            let position = match seg.attribute("Position") {
                None => None,
                Some(p) => match values::number(p) {
                    Ok(p) => Some(p),
                    Err(_) => return unreadable(self),
                },
            };
            let (Some(value), true) = (value, stops.len() < MAX_STOPS) else { return unreadable(self) };
            let color_space = color.and_then(|c| c.attribute("ColorSpace")).and_then(|v| v.trim().parse().ok());
            self.charge(part, 0, size_of::<ShadeStop>().saturating_add(value.len().saturating_mul(size_of::<f64>())), 0)?;
            stops.push(ShadeStop { position, value, color_space });
        }
        if stops.is_empty() {
            return unreadable(self);
        }
        let repeat = s.attribute("MapType").is_some_and(|m| m.trim() != "Direct");
        self.charge(part, 0, size_of::<Shade>(), 0)?;
        Ok(Some(Box::new(Shade { kind, start, end, extend_start: extend & 1 != 0, extend_end: extend & 2 != 0, repeat, stops })))
    }

    /// A `PathObject`, or with `clips` false a clip area's `Path` (whose own clips mean nothing).
    fn path_object(&mut self, part: &Part, n: &Node, clips: bool) -> Result<PathObject, OfdError> {
        let data = child(n, "AbbreviatedData").map(|d| text(&d)).unwrap_or_default();
        let path = values::path(&data, part.limits).map_err(|e| part.at(n, e))?;
        self.charge(part, 0, path.len().saturating_mul(size_of::<PathCmd>()), 0)?;
        let graphic = if clips { self.graphic(part, n)? } else { self.plain_graphic(part, n)? };
        Ok(PathObject {
            graphic,
            stroke: part.flag(n, "Stroke", true)?,
            fill: part.flag(n, "Fill", false)?,
            rule: match n.attribute("Rule") {
                None | Some("NonZero") => FillRule::NonZero,
                Some("Even-Odd" | "EvenOdd") => FillRule::EvenOdd,
                Some(other) => return Err(part.at(n, format!("unknown fill rule {other:?}"))),
            },
            fill_color: self.color(part, n, "FillColor")?,
            stroke_color: self.color(part, n, "StrokeColor")?,
            path,
        })
    }

    fn text_object(&mut self, part: &Part, n: &Node) -> Result<TextObject, OfdError> {
        let size = part.num(n, "Size")?.ok_or_else(|| part.at(n, "the Size attribute is missing".into()))?;
        let mut cg_transforms = Vec::new();
        for t in children(n, "CGTransform") {
            let glyphs = child(&t, "Glyphs").map(|g| text(&g)).unwrap_or_default();
            let mut list = Vec::new();
            for g in glyphs.split_whitespace() {
                if list.len() >= part.limits.max_numbers {
                    return Err(part.at(&t, format!("more than {} glyphs", part.limits.max_numbers)));
                }
                list.push(g.parse::<u32>().map_err(|_| part.at(&t, format!("glyph {g:?} is not a glyph index")))?);
            }
            self.charge(part, 0, size_of::<CgTransform>().saturating_add(list.len().saturating_mul(size_of::<u32>())), 0)?;
            cg_transforms.push(CgTransform {
                code_position: part.int(&t, "CodePosition", 0u32)?,
                code_count: part.int(&t, "CodeCount", 1u32)?,
                glyph_count: part.int(&t, "GlyphCount", 1u32)?,
                glyphs: list,
            });
        }
        let mut codes = Vec::new();
        for c in children(n, "TextCode") {
            // Text is kept as written: spaces inside a run are characters too.
            let text: String = c.descendants().filter(|k| k.is_text()).filter_map(|k| k.text()).collect();
            let needed = text.chars().count().saturating_sub(1);
            let delta = |attr: &str| -> Result<Vec<f64>, OfdError> {
                c.attribute(attr)
                    .map(|v| values::deltas(v, needed, part.limits).map_err(|e| part.at(&c, format!("{attr}: {e}"))))
                    .transpose()
                    .map(Option::unwrap_or_default)
            };
            let (delta_x, delta_y) = (delta("DeltaX")?, delta("DeltaY")?);
            let bytes =
                size_of::<TextCode>().saturating_add(text.len()).saturating_add((delta_x.len() + delta_y.len()).saturating_mul(size_of::<f64>()));
            self.charge(part, 0, bytes, needed.saturating_add(1))?;
            codes.push(TextCode { x: part.num(&c, "X")?, y: part.num(&c, "Y")?, delta_x, delta_y, text });
        }
        Ok(TextObject {
            graphic: self.graphic(part, n)?,
            font: part.required_id(n, "Font")?,
            size,
            stroke: part.flag(n, "Stroke", false)?,
            fill: part.flag(n, "Fill", true)?,
            h_scale: part.num(n, "HScale")?.unwrap_or(1.0),
            read_direction: part.int(n, "ReadDirection", 0u16)?,
            char_direction: part.int(n, "CharDirection", 0u16)?,
            weight: part.int(n, "Weight", 400u16)?,
            italic: part.flag(n, "Italic", false)?,
            fill_color: self.color(part, n, "FillColor")?,
            stroke_color: self.color(part, n, "StrokeColor")?,
            cg_transforms,
            codes,
        })
    }

    /// Record a warning once (writers repeat the same mistake on every object).
    fn warn(&mut self, msg: String) {
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    /// A page area. Without a `PhysicalBox` (some writers give only an `ApplicationBox`), the
    /// application box stands in; with neither, the area is left out, so the page falls back to
    /// the document's size.
    fn page_area(&mut self, part: &Part, node: &Node) -> Result<Option<PageArea>, OfdError> {
        let get = |name: &str| child(node, name).map(|b| part.boxed(&b, &text(&b))).transpose();
        let application = get("ApplicationBox")?;
        let physical = match (get("PhysicalBox")?, application) {
            (Some(p), _) => p,
            (None, Some(a)) => {
                self.warn(format!("{}: <{}> has no <PhysicalBox>; its <ApplicationBox> is used", part.name, node.tag_name().name()));
                a
            }
            (None, None) => {
                self.warn(format!("{}: <{}> has no <PhysicalBox> and is not used", part.name, node.tag_name().name()));
                return Ok(None);
            }
        };
        if physical.w <= 0.0 || physical.h <= 0.0 {
            return Err(part.at(node, "the page has no area (zero width or height)".into()));
        }
        Ok(Some(PageArea { physical, application, content: get("ContentBox")?, bleed: get("BleedBox")? }))
    }

    /// An object's common attributes. A `Boundary` that is missing (its object's coordinates
    /// are then page coordinates) or has a negative size (its corner is kept and the size taken
    /// as positive: text written leftwards from it, as some writers do) is tolerated and
    /// recorded.
    fn plain_graphic(&mut self, part: &Part, n: &Node) -> Result<Graphic, OfdError> {
        let boundary = match n.attribute("Boundary") {
            None => {
                self.warn(format!("{}: <{}> has no Boundary; its position is taken from the page's corner", part.name, n.tag_name().name()));
                OfdBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }
            }
            Some(s) => {
                let (b, negative) = values::boundary(s, part.limits).map_err(|e| part.at(n, e))?;
                if negative {
                    self.warn(format!("{}: a Boundary with a negative size is read with its size made positive", part.name));
                }
                b
            }
        };
        graphic(part, n, boundary)
    }

    /// An object's common attributes and its `Clips`.
    fn graphic(&mut self, part: &Part, n: &Node) -> Result<Graphic, OfdError> {
        let mut g = self.plain_graphic(part, n)?;
        let Some(clips) = child(n, "Clips") else { return Ok(g) };
        g.clips_follow_ctm = part.flag(&clips, "TransFlag", false)?;
        for c in children(&clips, "Clip") {
            let mut areas = Vec::new();
            for a in children(&c, "Area") {
                let shape = match (child(&a, "Path"), child(&a, "Text")) {
                    (Some(p), _) => ClipShape::Path(Box::new(self.path_object(part, &p, false)?)),
                    (None, Some(_)) => ClipShape::Text,
                    (None, None) => ClipShape::Empty,
                };
                self.charge(part, 1, size_of::<ClipArea>(), 0)?;
                let ctm = a.attribute("CTM").map(|m| values::matrix(m, part.limits).map_err(|e| part.at(&a, format!("CTM: {e}")))).transpose()?;
                areas.push(ClipArea { ctm, draw_param: part.id(&a, "DrawParam")?, shape });
            }
            g.clips.push(Clip { areas });
        }
        Ok(g)
    }

    /// Record references to templates, fonts and media that no resource defines.
    fn check_references(&mut self, templates: &[TemplatePage], pages: &[Page]) {
        fn walk(objs: &[Object], res: &Resources, missing: &mut Vec<String>) {
            for o in objs {
                match o {
                    Object::Text(t) if !res.fonts.contains_key(&t.font) => missing.push(format!("font {}", t.font)),
                    Object::Image(i) if !res.media.contains_key(&i.resource_id) => missing.push(format!("image {}", i.resource_id)),
                    Object::Composite(c) if !res.composites.contains_key(&c.resource_id) => {
                        missing.push(format!("composite graphic unit {}", c.resource_id))
                    }
                    Object::Block(b) => walk(&b.objects, res, missing),
                    _ => {}
                }
            }
        }
        let mut missing = Vec::new();
        for p in pages {
            for t in &p.templates {
                if !templates.iter().any(|tp| tp.id == t.template_id) {
                    missing.push(format!("template {}", t.template_id));
                }
            }
        }
        for content in templates.iter().map(|t| &t.content).chain(pages.iter().map(|p| &p.content)) {
            for l in &content.layers {
                walk(&l.objects, &self.resources, &mut missing);
            }
        }
        for unit in self.resources.composites.values() {
            walk(&unit.objects, &self.resources, &mut missing);
        }
        for a in pages.iter().flat_map(|p| &p.annotations) {
            walk(&a.objects, &self.resources, &mut missing);
        }
        missing.sort();
        missing.dedup();
        for m in missing {
            self.warnings.push(format!("{m} is used but not defined"));
        }
    }
}

fn z_order(part: &Part, node: &Node) -> Result<Option<ZOrder>, OfdError> {
    match node.attribute("ZOrder") {
        None => Ok(None),
        Some("Background") => Ok(Some(ZOrder::Background)),
        Some("Foreground") => Ok(Some(ZOrder::Foreground)),
        Some(other) => Err(part.at(node, format!("unknown ZOrder {other:?}"))),
    }
}

fn graphic(part: &Part, n: &Node, boundary: OfdBox) -> Result<Graphic, OfdError> {
    Ok(Graphic {
        id: part.id(n, "ID")?,
        boundary,
        ctm: n.attribute("CTM").map(|m| values::matrix(m, part.limits).map_err(|e| part.at(n, format!("CTM: {e}")))).transpose()?,
        draw_param: part.id(n, "DrawParam")?,
        line_width: part.num(n, "LineWidth")?,
        alpha: part.int(n, "Alpha", 255u8)?,
        visible: part.flag(n, "Visible", true)?,
        clips: Vec::new(),
        clips_follow_ctm: false,
    })
}
