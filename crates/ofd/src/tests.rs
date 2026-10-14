//! Synthetic OFD packages built here: no binary fixtures, no real documents.

use std::io::Write as _;

use proptest::prelude::*;

use super::*;
use crate::values;

/// One ZIP entry to write. `size`/`crc` override the true values to build lying headers.
struct Raw {
    name: String,
    data: Vec<u8>,
    deflate: bool,
    size: Option<u32>,
    crc: Option<u32>,
    flags: u16,
    method: Option<u16>,
}

fn raw(name: &str, data: &[u8]) -> Raw {
    Raw { name: name.into(), data: data.to_vec(), deflate: true, size: None, crc: None, flags: 0x0800, method: None }
}

fn zip_raw(entries: Vec<Raw>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let n = entries.len() as u16;
    for e in entries {
        let packed = if e.deflate {
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&e.data).unwrap();
            enc.finish().unwrap()
        } else {
            e.data.clone()
        };
        let method = e.method.unwrap_or(if e.deflate { 8 } else { 0 });
        let crc = e.crc.unwrap_or_else(|| crc32fast::hash(&e.data));
        let size = e.size.unwrap_or(e.data.len() as u32);
        let offset = out.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(sig.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend(e.flags.to_le_bytes());
            h.extend(method.to_le_bytes());
            h.extend([0u8; 4]); // time, date
            h.extend(crc.to_le_bytes());
            h.extend((packed.len() as u32).to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend((e.name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend([0u8; 6]); // comment length, disk, internal attributes
                h.extend([0u8; 4]); // external attributes
                h.extend(offset.to_le_bytes());
            }
            h.extend(e.name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(&packed);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(n.to_le_bytes());
    out.extend(n.to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn zip(files: &[(&str, &str)]) -> Vec<u8> {
    zip_raw(files.iter().map(|(n, d)| raw(n, d.as_bytes())).collect())
}

const NS: &str = r#"xmlns:ofd="http://www.ofdspec.org/2016""#;

fn ofd_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD {NS} Version="1.0" DocType="OFD"><ofd:DocBody>
<ofd:DocInfo><ofd:DocID>abc</ofd:DocID><ofd:Title>Fatura İşlem</ofd:Title><ofd:Creator>test</ofd:Creator></ofd:DocInfo>
<ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot><ofd:Signatures>Doc_0/Signs/Signatures.xml</ofd:Signatures>
</ofd:DocBody></ofd:OFD>"#
    )
}

fn document_xml(pages: usize) -> String {
    let list: String = (0..pages).map(|i| format!(r#"<ofd:Page ID="{}" BaseLoc="Pages/Page_{i}/Content.xml"/>"#, 100 + i)).collect();
    format!(
        r#"<ofd:Document {NS}><ofd:CommonData><ofd:MaxUnitID>50</ofd:MaxUnitID>
<ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea>
<ofd:PublicRes>PublicRes.xml</ofd:PublicRes><ofd:DocumentRes>DocumentRes.xml</ofd:DocumentRes>
<ofd:TemplatePage ID="2" BaseLoc="Tpls/Tpl_0/Content.xml" ZOrder="Background"/>
</ofd:CommonData><ofd:Pages>{list}</ofd:Pages></ofd:Document>"#
    )
}

const PUBLIC_RES: &str = r#"<ofd:Res xmlns:ofd="http://www.ofdspec.org/2016" BaseLoc="Res">
<ofd:ColorSpaces><ofd:ColorSpace ID="3" Type="RGB" BitsPerComponent="8"/></ofd:ColorSpaces>
<ofd:Fonts><ofd:Font ID="4" FontName="宋体" FamilyName="宋体"/>
<ofd:Font ID="5" FontName="Embedded" Bold="true"><ofd:FontFile>font_5.ttf</ofd:FontFile></ofd:Font></ofd:Fonts>
</ofd:Res>"#;

const DOC_RES: &str = r##"<ofd:Res xmlns:ofd="http://www.ofdspec.org/2016" BaseLoc="Res">
<ofd:MultiMedias><ofd:MultiMedia ID="6" Type="Image" Format="PNG"><ofd:MediaFile>image_6.png</ofd:MediaFile></ofd:MultiMedia></ofd:MultiMedias>
<ofd:DrawParams><ofd:DrawParam ID="7" LineWidth="0.5"><ofd:StrokeColor Value="#FF 0 0"/></ofd:DrawParam></ofd:DrawParams>
</ofd:Res>"##;

const TEMPLATE: &str = r#"<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Content><ofd:Layer ID="8" Type="Background">
<ofd:PathObject ID="9" Boundary="10 10 190 120" LineWidth="0.25"><ofd:AbbreviatedData>M 0 0 L 190 0 L 190 120 L 0 120 C</ofd:AbbreviatedData></ofd:PathObject>
</ofd:Layer></ofd:Content></ofd:Page>"#;

fn page(body: &str) -> String {
    format!(r#"<ofd:Page {NS}><ofd:Template TemplateID="2"/><ofd:Content><ofd:Layer ID="10">{body}</ofd:Layer></ofd:Content></ofd:Page>"#)
}

const PAGE_BODY: &str = r#"<ofd:TextObject ID="11" Boundary="20 30 100 10" Font="4" Size="3.5" CTM="1 0 0 1 0 0">
<ofd:FillColor Value="0 0 0" ColorSpace="3"/>
<ofd:CGTransform CodePosition="0" CodeCount="2" GlyphCount="2"><ofd:Glyphs>12 34</ofd:Glyphs></ofd:CGTransform>
<ofd:TextCode X="0" Y="3" DeltaX="g 3 3.5 2">发票 号码</ofd:TextCode></ofd:TextObject>
<ofd:ImageObject ID="12" Boundary="150 10 40 40" CTM="40 0 0 40 0 0" ResourceID="6"/>
<ofd:PageBlock ID="13"><ofd:PathObject ID="14" Boundary="0 0 10 10" Fill="true" Stroke="false" Rule="Even-Odd">
<ofd:FillColor Value="255 0 0"/><ofd:AbbreviatedData>S 0 0 Q 5 5 10 0 B 1 2 3 4 5 6 A 2 2 0 1 0 8 8 L 1 1 2 2 C</ofd:AbbreviatedData>
</ofd:PathObject></ofd:PageBlock>"#;

/// A complete two-page document: page 0 uses the document size, page 1 its own.
fn sample_files() -> Vec<(String, String)> {
    let mut files = vec![
        ("OFD.xml".to_string(), ofd_xml()),
        ("Doc_0/Document.xml".into(), document_xml(2)),
        ("Doc_0/PublicRes.xml".into(), PUBLIC_RES.into()),
        ("Doc_0/DocumentRes.xml".into(), DOC_RES.into()),
        ("Doc_0/Tpls/Tpl_0/Content.xml".into(), TEMPLATE.into()),
        ("Doc_0/Pages/Page_0/Content.xml".into(), page(PAGE_BODY)),
    ];
    files.push((
        "Doc_0/Pages/Page_1/Content.xml".into(),
        r#"<Page><Area><PhysicalBox>0 0 100 50</PhysicalBox></Area><Content><Layer><TextObject Boundary="0 0 50 10" Font="5" Size="4"><TextCode X="1" Y="4">Hi</TextCode></TextObject></Layer></Content></Page>"#.to_string(),
    ));
    files
}

fn build(files: &[(String, String)]) -> Vec<u8> {
    let mut entries: Vec<Raw> = files.iter().map(|(n, d)| raw(n, d.as_bytes())).collect();
    entries.push(raw("Doc_0/Res/font_5.ttf", b"not really a font"));
    entries.push(raw("Doc_0/Res/image_6.png", b"\x89PNG\r\n\x1a\n"));
    zip_raw(entries)
}

fn sample() -> Vec<u8> {
    build(&sample_files())
}

fn with(name: &str, content: &str) -> Vec<u8> {
    let files: Vec<(String, String)> = sample_files().into_iter().map(|(n, d)| if n == name { (n, content.to_string()) } else { (n, d) }).collect();
    build(&files)
}

fn invalid(bytes: &[u8]) -> String {
    match parse(bytes) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected an error"),
    }
}

#[test]
fn detects_ofd_and_rejects_other_zips() {
    assert!(is_ofd(&sample()));
    let docx = zip(&[("[Content_Types].xml", "<Types/>"), ("word/document.xml", "<w:document/>")]);
    assert!(!is_ofd(&docx));
    assert!(matches!(parse(&docx), Err(OfdError::NotOfd(_))));
    // OFD.xml must be at the root.
    assert!(!is_ofd(&zip(&[("x/OFD.xml", "<OFD/>")])));
    assert!(!is_ofd(b"%PDF-1.7"));
    assert!(!is_ofd(b""));
    assert!(!is_ofd(b"PK\x03\x04"));
    assert!(matches!(parse(b"hello"), Err(OfdError::NotOfd(_))));
}

#[test]
fn reads_the_document_structure() {
    let doc = parse(&sample()).unwrap();
    assert_eq!((doc.version.as_str(), doc.doc_type.as_str(), doc.doc_bodies), ("1.0", "OFD", 1));
    assert_eq!(doc.info.title.as_deref(), Some("Fatura İşlem"));
    assert_eq!(doc.info.doc_id.as_deref(), Some("abc"));
    assert_eq!(doc.doc_root, "Doc_0/Document.xml");
    assert_eq!(doc.signatures.as_deref(), Some("Doc_0/Signs/Signatures.xml"));
    assert_eq!(doc.pages.len(), 2);
    assert_eq!(doc.pages[0].id, 100);
    assert_eq!(doc.pages[0].location, "Doc_0/Pages/Page_0/Content.xml");
    assert_eq!(doc.pages[0].templates, vec![TemplateRef { template_id: 2, z_order: None }]);
    assert_eq!(doc.templates.len(), 1);
    assert_eq!(doc.templates[0].z_order, ZOrder::Background);
    assert_eq!(doc.templates[0].content.layers[0].kind, LayerKind::Background);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn page_size_comes_from_the_page_or_the_document() {
    let doc = parse(&sample()).unwrap();
    let a4 = doc.pages[0].area.physical;
    assert_eq!(a4, OfdBox { x: 0.0, y: 0.0, w: 210.0, h: 297.0 });
    let (w, h) = a4.size_pt();
    assert!((w - 595.2756).abs() < 1e-3 && (h - 841.8898).abs() < 1e-3, "{w} × {h}");
    assert_eq!(doc.pages[1].area.physical, OfdBox { x: 0.0, y: 0.0, w: 100.0, h: 50.0 });
}

#[test]
fn ofd_page_space_maps_to_pdf_space() {
    let m = OfdBox { x: 0.0, y: 0.0, w: 210.0, h: 297.0 }.page_to_pdf();
    let close = |(a, b): (f64, f64), (c, d): (f64, f64)| (a - c).abs() < 1e-9 && (b - d).abs() < 1e-9;
    let (w, h) = (mm_to_pt(210.0), mm_to_pt(297.0));
    assert!(close(m.apply(0.0, 0.0), (0.0, h)), "top left is the PDF page's top left");
    assert!(close(m.apply(210.0, 297.0), (w, 0.0)));
    // A page box that doesn't start at 0 0.
    let m = OfdBox { x: 5.0, y: 10.0, w: 100.0, h: 50.0 }.page_to_pdf();
    assert!(close(m.apply(5.0, 10.0), (0.0, mm_to_pt(50.0))));
    assert!(close(m.apply(105.0, 60.0), (mm_to_pt(100.0), 0.0)));
    // An object's own space: its CTM, then its boundary's corner.
    let g = Graphic {
        id: None,
        boundary: OfdBox { x: 20.0, y: 30.0, w: 10.0, h: 10.0 },
        ctm: Some(Matrix { a: 2.0, b: 0.0, c: 0.0, d: 2.0, e: 1.0, f: 0.0 }),
        draw_param: None,
        line_width: None,
        alpha: 255,
        visible: true,
        clips: Vec::new(),
        clips_follow_ctm: false,
    };
    assert!(close(g.to_page().apply(1.0, 1.0), (23.0, 32.0)));
}

#[test]
fn reads_objects_and_resources() {
    let doc = parse(&sample()).unwrap();
    let objs = &doc.pages[0].content.layers[0].objects;
    let Object::Text(t) = &objs[0] else { panic!("{:?}", objs[0]) };
    assert_eq!((t.font, t.size, t.fill, t.stroke, t.weight), (4, 3.5, true, false, 400));
    assert_eq!(t.codes[0].text, "发票 号码");
    assert_eq!(t.codes[0].delta_x, vec![3.5, 3.5, 3.5, 2.0]);
    assert_eq!(t.cg_transforms[0].glyphs, vec![12, 34]);
    assert_eq!(t.fill_color.as_ref().unwrap().color_space, Some(3));
    let Object::Image(i) = &objs[1] else { panic!() };
    assert_eq!(i.resource_id, 6);
    assert_eq!(doc.resources.media[&6].file, "Doc_0/Res/image_6.png");
    let Object::Block(b) = &objs[2] else { panic!() };
    let Object::Path(p) = &b.objects[0] else { panic!() };
    assert_eq!((p.fill, p.stroke, p.rule), (true, false, FillRule::EvenOdd));
    assert_eq!(
        p.path,
        vec![
            PathCmd::Move(0.0, 0.0),
            PathCmd::Quad(5.0, 5.0, 10.0, 0.0),
            PathCmd::Cubic(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
            PathCmd::Arc { rx: 2.0, ry: 2.0, angle: 0.0, large: true, sweep: false, x: 8.0, y: 8.0 },
            PathCmd::Line(1.0, 1.0),
            PathCmd::Line(2.0, 2.0),
            PathCmd::Close,
        ]
    );
    let fonts = &doc.resources.fonts;
    assert_eq!((fonts[&4].name.as_str(), fonts[&4].file.as_deref()), ("宋体", None));
    assert_eq!(fonts[&5].file.as_deref(), Some("Doc_0/Res/font_5.ttf"));
    assert!(fonts[&5].bold);
    assert_eq!(doc.resources.color_spaces[&3].kind, ColorSpaceKind::Rgb);
    let dp = &doc.resources.draw_params[&7];
    assert_eq!((dp.line_width, dp.stroke_color.as_ref().unwrap().value.clone()), (Some(0.5), vec![255.0, 0.0, 0.0]));
    // Files referenced by resources are in the package and readable.
    let bytes = sample();
    let pkg = zip::Package::open(&bytes, &Limits::default()).unwrap();
    assert_eq!(pkg.read(fonts[&5].file.as_deref().unwrap()).unwrap(), b"not really a font");
}

#[test]
fn reads_files_without_the_namespace_prefix() {
    // Page_1 in the sample is written without `ofd:`.
    let doc = parse(&sample()).unwrap();
    let Object::Text(t) = &doc.pages[1].content.layers[0].objects[0] else { panic!() };
    assert_eq!(t.codes[0].text, "Hi");
}

#[test]
fn many_pages_keep_their_order() {
    let mut files = sample_files();
    files.retain(|(n, _)| !n.contains("Pages/"));
    files.retain(|(n, _)| n != "Doc_0/Document.xml");
    files.push(("Doc_0/Document.xml".into(), document_xml(5)));
    for i in 0..5 {
        files.push((
            format!("Doc_0/Pages/Page_{i}/Content.xml"),
            page(&format!(r#"<ofd:TextObject Boundary="0 0 9 9" Font="4" Size="3"><ofd:TextCode X="0" Y="0">{i}</ofd:TextCode></ofd:TextObject>"#)),
        ));
    }
    let doc = parse(&build(&files)).unwrap();
    let texts: Vec<String> = doc
        .pages
        .iter()
        .map(|p| match &p.content.layers[0].objects[0] {
            Object::Text(t) => t.codes[0].text.clone(),
            o => panic!("{o:?}"),
        })
        .collect();
    assert_eq!(texts, ["0", "1", "2", "3", "4"]);
    assert_eq!(doc.pages.iter().map(|p| p.id).collect::<Vec<_>>(), [100, 101, 102, 103, 104]);
}

#[test]
fn delta_x_g_repeats_expand() {
    let l = Limits::default();
    assert_eq!(values::deltas("g 3 1.5 2", 4, &l).unwrap(), vec![1.5, 1.5, 1.5, 2.0]);
    assert_eq!(values::deltas("1 g 2 0.5 3", 4, &l).unwrap(), vec![1.0, 0.5, 0.5, 3.0]);
    // Steps past the text's end are dropped; a huge count costs nothing.
    assert_eq!(values::deltas("g 4000000000 1", 2, &l).unwrap(), vec![1.0, 1.0]);
    assert_eq!(values::deltas("g 18446744073709551615 1", 3, &l).unwrap(), vec![1.0; 3]);
    assert!(values::deltas("g 3", 4, &l).is_err());
    assert!(values::deltas("g x 1", 4, &l).is_err());
    assert!(values::deltas("g -1 1", 4, &l).is_err());
    assert!(values::deltas("1 NaN", 4, &l).is_err());
    let code = TextCode { x: Some(0.0), y: Some(0.0), delta_x: vec![2.0, 3.0], delta_y: vec![], text: "abcd".into() };
    assert_eq!(
        code.glyph_offsets(),
        vec![('a', Some(0.0), Some(0.0)), ('b', Some(2.0), Some(0.0)), ('c', Some(5.0), Some(0.0)), ('d', None, Some(0.0))]
    );
    let vertical = TextCode { x: None, y: None, delta_x: vec![], delta_y: vec![4.0], text: "上下".into() };
    assert_eq!(vertical.glyph_offsets(), vec![('上', Some(0.0), Some(0.0)), ('下', None, Some(4.0))]);
}

#[test]
fn path_data_errors() {
    let l = Limits::default();
    assert_eq!(values::path("M 1 2 L 3 4 Z", &l).unwrap(), vec![PathCmd::Move(1.0, 2.0), PathCmd::Line(3.0, 4.0), PathCmd::Close]);
    assert_eq!(values::path("M1,2L3,4", &l).unwrap(), vec![PathCmd::Move(1.0, 2.0), PathCmd::Line(3.0, 4.0)]);
    assert_eq!(values::path("", &l).unwrap(), vec![]);
    for bad in ["L 1", "1 2", "M 1 x", "X 1 2", "B 1 2 3 4 5", "M 1e400 0"] {
        assert!(values::path(bad, &l).is_err(), "{bad}");
    }
}

#[test]
fn malformed_parts_are_errors_naming_the_part() {
    let page0 = "Doc_0/Pages/Page_0/Content.xml";
    assert!(invalid(&with(page0, "<Page><Content>")).contains(page0));
    assert!(invalid(&with(page0, &page(r#"<TextObject Boundary="0 0 1 1" Size="3"/>"#))).contains("Font"));
    assert!(invalid(&with(page0, &page(r#"<PathObject Boundary="0 0 1 NaN"/>"#))).contains("NaN"));
    // A missing or negative Boundary is tolerated (common_writer_mistakes_are_tolerated_and_recorded),
    // a malformed one is not.
    assert!(invalid(&with(page0, &page(r#"<PathObject Boundary="0 0 1"/>"#))).contains("four numbers"));
    assert!(invalid(&with(page0, &page(r#"<PathObject Boundary="0 0 1e20 1"/>"#))).contains("range"));
    assert!(invalid(&with(page0, &page(r#"<PathObject Boundary="0 0 1 1" Stroke="maybe"/>"#))).contains("Stroke"));
    assert!(invalid(&with(page0, &page(r#"<ImageObject Boundary="0 0 1 1" ResourceID="x"/>"#))).contains("ResourceID"));
    assert!(invalid(&with("OFD.xml", "<OFD><DocBody/></OFD>")).contains("DocRoot"));
    assert!(invalid(&with("OFD.xml", "<Other/>")).contains("<Other>"));
    assert!(invalid(&with("Doc_0/Document.xml", &document_xml(0))).contains("no pages"));
    // A DTD (entity expansion) is refused.
    let dtd = r#"<?xml version="1.0"?><!DOCTYPE r [<!ENTITY a "aaaa">]><OFD/>"#;
    assert!(matches!(parse(&with("OFD.xml", dtd)), Err(OfdError::Xml { .. })));
    // Not UTF-8; a BOM is fine.
    let latin1 = zip_raw(vec![raw("OFD.xml", b"<OFD Version=\"\xE9\"/>")]);
    assert!(matches!(parse(&latin1), Err(OfdError::Xml { .. })));
    assert!(parse(&with("OFD.xml", &format!("\u{FEFF}{}", ofd_xml()))).is_ok());
}

#[test]
fn missing_parts_are_errors() {
    let mut files = sample_files();
    files.retain(|(n, _)| n != "Doc_0/Pages/Page_1/Content.xml");
    assert_eq!(parse(&build(&files)), Err(OfdError::Missing("Doc_0/Pages/Page_1/Content.xml".into())));
    // A page without a size, in a document without one.
    let no_area = document_xml(2).replace("<ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea>", "");
    assert!(invalid(&with("Doc_0/Document.xml", &no_area)).contains("no size"));
}

#[test]
fn unknown_references_are_warnings() {
    let doc = parse(&with("Doc_0/Pages/Page_0/Content.xml", &page(r#"<TextObject Boundary="0 0 1 1" Font="99" Size="3"/>"#))).unwrap();
    assert!(doc.warnings.iter().any(|w| w.contains("font 99")), "{:?}", doc.warnings);
}

#[test]
fn references_cannot_leave_the_package() {
    for root in ["../../etc/passwd", "Doc_0/../../x.xml"] {
        let ofd = ofd_xml().replace("Doc_0/Document.xml", root);
        assert!(invalid(&with("OFD.xml", &ofd)).contains("outside the package"), "{root}");
    }
    assert_eq!(parse::resolve("Doc_0/Pages", "../Res/a.png").unwrap(), "Doc_0/Res/a.png");
    assert_eq!(parse::resolve("Doc_0/Pages", "/Doc_0/x.xml").unwrap(), "Doc_0/x.xml");
    assert_eq!(parse::resolve("Doc_0", r"Res\a.png").unwrap(), "Doc_0/Res/a.png");
}

#[test]
fn unsafe_entry_names_are_refused() {
    for name in ["../evil.xml", "/abs.xml", "C:/x.xml", r"..\x.xml", "a/../../b"] {
        let bytes = zip_raw(vec![raw("OFD.xml", b"<OFD/>"), raw(name, b"x")]);
        assert!(matches!(zip::Package::open(&bytes, &Limits::default()), Err(OfdError::Container(_))), "{name}");
        assert!(!is_ofd(&bytes));
    }
    let dup = zip_raw(vec![raw("OFD.xml", b"<OFD/>"), raw("./OFD.xml", b"<OFD/>")]);
    assert!(invalid(&dup).contains("twice"));
}

#[test]
fn zip_bombs_and_lies_are_refused() {
    // 8 MiB of zeros packs to a few KiB: over the ratio.
    let zeros = vec![0u8; 8 << 20];
    let bomb = zip_raw(vec![raw("OFD.xml", b"<OFD/>"), raw("big.bin", &zeros)]);
    assert!(matches!(zip::Package::open(&bomb, &Limits::default()), Err(OfdError::Limit(m)) if m.contains("ZIP bomb")));
    // Allowed with a looser ratio, and then readable.
    let loose = Limits { max_ratio: 100_000, ..Limits::default() };
    assert_eq!(zip::Package::open(&bomb, &loose).unwrap().read("big.bin").unwrap().len(), 8 << 20);
    // Over the per-entry and total sizes.
    let small = Limits { max_entry_size: 100, ..Limits::default() };
    assert!(matches!(zip::Package::open(&sample(), &small), Err(OfdError::Limit(_))));
    let tight = Limits { max_total_size: 500, ..Limits::default() };
    assert!(matches!(parse_with_limits(&sample(), &tight), Err(OfdError::Limit(_))));
    // Too many entries.
    let few = Limits { max_entries: 3, ..Limits::default() };
    assert!(matches!(parse_with_limits(&sample(), &few), Err(OfdError::Limit(_))));
    // A header that understates the size: the extra bytes are not trusted.
    let mut e = raw("OFD.xml", ofd_xml().as_bytes());
    e.size = Some(10);
    assert!(invalid(&zip_raw(vec![e])).contains("not the 10"));
    // A header that overstates it.
    let mut e = raw("OFD.xml", b"<OFD/>");
    e.size = Some(1000);
    assert!(invalid(&zip_raw(vec![e])).contains("not the 1000"));
    // A wrong checksum.
    let mut e = raw("OFD.xml", ofd_xml().as_bytes());
    e.crc = Some(1);
    assert!(invalid(&zip_raw(vec![e])).contains("checksum"));
}

#[test]
fn unsupported_containers_say_so() {
    let mut e = raw("OFD.xml", b"<OFD/>");
    e.flags |= 1;
    assert!(matches!(parse(&zip_raw(vec![e])), Err(OfdError::Unsupported(m)) if m.contains("encrypted")));
    let mut e = raw("OFD.xml", b"<OFD/>");
    e.method = Some(12);
    assert!(matches!(parse(&zip_raw(vec![e])), Err(OfdError::Unsupported(m)) if m.contains("method 12")));
    let mut z64 = zip(&[("OFD.xml", "<OFD/>")]);
    let at = z64.len() - 22 + 10;
    z64[at..at + 2].copy_from_slice(&[0xFF, 0xFF]);
    assert!(matches!(zip::Package::open(&z64, &Limits::default()), Err(OfdError::Unsupported(m)) if m.contains("ZIP64")));
    // Stored entries read too.
    let mut e = raw("OFD.xml", ofd_xml().as_bytes());
    e.deflate = false;
    assert!(zip::Package::open(&zip_raw(vec![e]), &Limits::default()).unwrap().read("OFD.xml").is_ok());
    // Truncated anywhere: an error, never a panic.
    let full = sample();
    for cut in (0..full.len()).step_by(97) {
        let _ = parse(&full[..cut]);
    }
}

#[test]
fn deep_page_blocks_are_refused() {
    let deep = format!("{}{}", "<PageBlock>".repeat(100), "</PageBlock>".repeat(100));
    let limits = Limits::default();
    assert!(
        matches!(parse_with_limits(&with("Doc_0/Pages/Page_0/Content.xml", &page(&deep)), &limits), Err(OfdError::Limit(m)) if m.contains("nested"))
    );
    let ok = format!("{}{}", "<PageBlock>".repeat(10), "</PageBlock>".repeat(10));
    assert!(parse(&with("Doc_0/Pages/Page_0/Content.xml", &page(&ok))).is_ok());
}

#[test]
fn shared_page_resources_are_read_once() {
    let res = r#"<Res><Fonts><Font ID="40" FontName="KaiTi"/></Fonts></Res>"#;
    let p =
        r#"<Page><PageRes>../../PageRes.xml</PageRes><Content><Layer><TextObject Boundary="0 0 1 1" Font="40" Size="3"/></Layer></Content></Page>"#;
    let mut files = sample_files();
    files.retain(|(n, _)| !n.contains("Pages/"));
    files.push(("Doc_0/PageRes.xml".into(), res.into()));
    files.push(("Doc_0/Pages/Page_0/Content.xml".into(), p.into()));
    files.push(("Doc_0/Pages/Page_1/Content.xml".into(), p.into()));
    let doc = parse(&build(&files)).unwrap();
    assert_eq!(doc.resources.fonts[&40].name, "KaiTi");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Damaged packages: flipped, inserted and removed bytes anywhere.
    #[test]
    fn damaged_packages_never_panic(edits in proptest::collection::vec((any::<usize>(), any::<u8>(), 0u8..3), 1..8)) {
        let mut bytes = sample();
        for (at, byte, kind) in edits {
            let at = at % (bytes.len() + 1);
            match kind {
                0 if at < bytes.len() => bytes[at] = byte,
                1 => bytes.insert(at, byte),
                _ if at < bytes.len() => { bytes.remove(at); }
                _ => {}
            }
        }
        let _ = parse(&bytes);
        let _ = is_ofd(&bytes);
    }

    /// Damaged XML inside a well-formed package.
    #[test]
    fn damaged_page_xml_never_panics(cut in 0usize..2000, junk in "[ -~]{0,40}") {
        let body = page(PAGE_BODY);
        let at = (cut % body.len()).min(body.len());
        let at = (0..=at).rev().find(|&i| body.is_char_boundary(i)).unwrap_or(0);
        let damaged = format!("{}{}{}", &body[..at], junk, &body[at..]);
        let _ = parse(&with("Doc_0/Pages/Page_0/Content.xml", &damaged));
    }

    #[test]
    fn any_path_data_or_deltas_never_panic(s in "[SMLQBACZg0-9 .,eE+-]{0,80}") {
        let l = Limits::default();
        let _ = values::path(&s, &l);
        let _ = values::deltas(&s, 50, &l);
    }
}

/// `pages` pages that all point at one content file holding `body`.
fn shared_pages(pages: usize, body: &str) -> Vec<u8> {
    let mut files = sample_files();
    files.retain(|(n, _)| !n.contains("Pages/") && n != "Doc_0/Document.xml");
    let doc = (0..pages).fold(document_xml(pages), |d, i| d.replace(&format!("Pages/Page_{i}/Content.xml"), "Pages/Shared/Content.xml"));
    files.push(("Doc_0/Document.xml".into(), doc));
    files.push(("Doc_0/Pages/Shared/Content.xml".into(), page(body)));
    build(&files)
}

#[test]
fn a_content_file_shared_by_many_pages_counts_once_per_page() {
    // A few kilobytes of packed XML read for every page: each read counts, so a small file
    // can't make the reader hold the same content thousands of times over.
    let body = r#"<PathObject Boundary="0 0 1 1"><AbbreviatedData>M 0 0 L 1 1 L 2 2 C</AbbreviatedData></PathObject>"#.repeat(40);
    let bytes = shared_pages(60, &body);
    assert_eq!(parse(&bytes).unwrap().pages.len(), 60);
    let xml = Limits { max_xml_total: 100_000, ..Limits::default() };
    assert!(matches!(parse_with_limits(&bytes, &xml), Err(OfdError::Limit(m)) if m.contains("bytes of XML")));
    let objects = Limits { max_objects: 1_000, ..Limits::default() };
    assert!(matches!(parse_with_limits(&bytes, &objects), Err(OfdError::Limit(m)) if m.contains("1000 objects")));
    let memory = Limits { max_model_bytes: 200_000, ..Limits::default() };
    assert!(matches!(parse_with_limits(&bytes, &memory), Err(OfdError::Limit(m)) if m.contains("to hold")));
}

#[test]
fn text_and_path_data_count_against_the_document() {
    // One path of many commands: small in XML, large in the model.
    let path = format!(r#"<PathObject Boundary="0 0 1 1"><AbbreviatedData>M 0 0{}</AbbreviatedData></PathObject>"#, " C".repeat(5_000));
    let bytes = with("Doc_0/Pages/Page_0/Content.xml", &page(&path));
    assert!(parse(&bytes).is_ok());
    let memory = Limits { max_model_bytes: 100_000, ..Limits::default() };
    assert!(matches!(parse_with_limits(&bytes, &memory), Err(OfdError::Limit(m)) if m.contains("to hold")));
    // Text becomes one positioned glyph per character.
    let text = format!(r#"<TextObject Boundary="0 0 1 1" Font="4" Size="3"><TextCode X="0" Y="0">{}</TextCode></TextObject>"#, "a".repeat(500));
    let bytes = with("Doc_0/Pages/Page_0/Content.xml", &page(&text));
    assert!(parse(&bytes).is_ok());
    let chars = Limits { max_text_chars: 400, ..Limits::default() };
    assert!(matches!(parse_with_limits(&bytes, &chars), Err(OfdError::Limit(m)) if m.contains("characters")));
}

#[test]
fn parse_package_shares_the_unpacking_budget() {
    let bytes = sample();
    let limits = Limits::default();
    let pkg = zip::Package::open(&bytes, &limits).unwrap();
    assert_eq!(parse_package(&pkg, &limits).unwrap(), parse(&bytes).unwrap());
    // Exactly the package's size: the reader unpacks every XML part once, and what it unpacked
    // stays counted for the caller's own reads.
    let total = sample_files().iter().map(|(_, d)| d.len() as u64).sum::<u64>() + b"not really a font".len() as u64 + 8;
    let tight = Limits { max_total_size: total, ..Limits::default() };
    let pkg = zip::Package::open(&bytes, &tight).unwrap();
    parse_package(&pkg, &tight).unwrap();
    assert!(matches!(pkg.read("Doc_0/Document.xml"), Err(OfdError::Limit(_))));
    assert!(zip::Package::open(&bytes, &tight).unwrap().read("Doc_0/Document.xml").is_ok());
}

#[test]
fn reads_clips() {
    let body = r#"<PathObject Boundary="10 20 30 40" CTM="2 0 0 2 0 0"><Clips TransFlag="true"><Clip><Area CTM="1 0 0 1 5 5" DrawParam="7">
<Path Boundary="1 2 3 4" Rule="Even-Odd"><Clips><Clip><Area><Path Boundary="0 0 1 1"/></Area></Clip></Clips><AbbreviatedData>M 0 0 L 3 4 C</AbbreviatedData></Path></Area>
<Area><Text Boundary="0 0 1 1" Font="4" Size="3"><TextCode>X</TextCode></Text></Area></Clip><Clip><Area/></Clip></Clips>
<AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject>"#;
    let doc = parse(&with("Doc_0/Pages/Page_0/Content.xml", &page(body))).unwrap();
    let Object::Path(p) = &doc.pages[0].content.layers[0].objects[0] else { panic!() };
    let g = &p.graphic;
    assert!(g.clips_follow_ctm);
    assert_eq!(g.clips.len(), 2);
    let areas = &g.clips[0].areas;
    assert_eq!(areas.len(), 2);
    assert_eq!(areas[0].draw_param, Some(7));
    let ClipShape::Path(clip) = &areas[0].shape else { panic!("{:?}", areas[0].shape) };
    assert_eq!(clip.rule, FillRule::EvenOdd);
    assert_eq!(clip.path, [PathCmd::Move(0.0, 0.0), PathCmd::Line(3.0, 4.0), PathCmd::Close]);
    assert!(clip.graphic.clips.is_empty(), "a clip path's own clips are not read");
    assert_eq!(areas[1].shape, ClipShape::Text);
    assert_eq!(g.clips[1].areas[0].shape, ClipShape::Empty);
    // A clip path point: its boundary corner (1, 2), the area's CTM (+5, +5), the object's CTM
    // (×2, with TransFlag) and its boundary corner (10, 20).
    let m = clip.graphic.to_page().then(&g.clip_to_page(&areas[0]));
    assert_eq!(m.apply(0.0, 0.0), (22.0, 34.0));
    // Without TransFlag only the boundary corner applies.
    let plain = Graphic { clips_follow_ctm: false, ..g.clone() };
    assert_eq!(clip.graphic.to_page().then(&plain.clip_to_page(&areas[0])).apply(0.0, 0.0), (16.0, 27.0));
    // Malformed clips are errors naming the part, like other objects.
    let bad = body.replace(r#"CTM="1 0 0 1 5 5""#, r#"CTM="1 0 0""#);
    assert!(invalid(&with("Doc_0/Pages/Page_0/Content.xml", &page(&bad))).contains("CTM"));
}

#[test]
fn common_writer_mistakes_are_tolerated_and_recorded() {
    let page0 = "Doc_0/Pages/Page_0/Content.xml";
    // A Boundary with a negative width: text written leftwards from the corner. The corner stays.
    let body = r#"<TextObject Boundary="90 58 -3 5" Font="4" Size="3"><TextCode X="-3" Y="3">批</TextCode></TextObject>
<PathObject CTM="2 0 0 2 0 0"><AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject>
<PathObject Boundary="1 2 3 4"><AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject>"#;
    let doc = parse(&with(page0, &page(body))).unwrap();
    let objs = &doc.pages[0].content.layers[0].objects;
    let Object::Text(t) = &objs[0] else { panic!("{:?}", objs[0]) };
    assert_eq!(t.graphic.boundary, OfdBox { x: 90.0, y: 58.0, w: 3.0, h: 5.0 });
    // No Boundary: the object's own coordinates are page coordinates (through its CTM).
    let Object::Path(p) = &objs[1] else { panic!("{:?}", objs[1]) };
    assert_eq!(p.graphic.boundary, OfdBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
    assert_eq!(p.graphic.to_page().apply(1.0, 1.0), (2.0, 2.0));
    // Each mistake is recorded once, however often it is made.
    let three = format!("{body}{body}{body}");
    let doc = parse(&with(page0, &page(&three))).unwrap();
    let count = |needle: &str| doc.warnings.iter().filter(|w| w.contains(needle)).count();
    assert_eq!((count("negative size"), count("no Boundary")), (1, 1), "{:?}", doc.warnings);

    // An image or composite object with no resource: nothing to draw, left out.
    let empty = r#"<ImageObject Boundary="0 0 1 1"/><CompositeObject Boundary="0 0 1 1"/><PathObject Boundary="0 0 1 1"/>"#;
    let doc = parse(&with(page0, &page(empty))).unwrap();
    assert_eq!(doc.pages[0].content.layers[0].objects.len(), 1);
    assert!(doc.warnings.iter().any(|w| w.contains("an <ImageObject> without ResourceID is left out")), "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.contains("an <CompositeObject> without ResourceID is left out")), "{:?}", doc.warnings);
    // A malformed ResourceID is still an error.
    assert!(invalid(&with(page0, &page(r#"<ImageObject Boundary="0 0 1 1" ResourceID="x"/>"#))).contains("ResourceID"));

    // A resource file the document lists but the package lacks: its fonts are undefined.
    let mut files = sample_files();
    files.retain(|(n, _)| n != "Doc_0/PublicRes.xml");
    let doc = parse(&build(&files)).unwrap();
    assert!(doc.warnings.iter().any(|w| w.contains("Doc_0/PublicRes.xml is listed but missing")), "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.contains("font 4 is used but not defined")), "{:?}", doc.warnings);
    // Other damage to a resource file is still an error.
    assert!(invalid(&with("Doc_0/PublicRes.xml", "<Res>")).contains("PublicRes.xml"));

    // An area with an ApplicationBox but no PhysicalBox uses the application box; with neither,
    // the page takes the document's size.
    let app = r#"<Page><Area><ApplicationBox>0 0 100 50</ApplicationBox></Area><Content/></Page>"#;
    let none = r#"<Page><Area><BleedBox>0 0 100 50</BleedBox></Area><Content/></Page>"#;
    let doc = parse(&with("Doc_0/Pages/Page_1/Content.xml", app)).unwrap();
    assert_eq!(doc.pages[1].area.physical, OfdBox { x: 0.0, y: 0.0, w: 100.0, h: 50.0 });
    assert!(doc.warnings.iter().any(|w| w.contains("its <ApplicationBox> is used")), "{:?}", doc.warnings);
    let doc = parse(&with("Doc_0/Pages/Page_1/Content.xml", none)).unwrap();
    assert_eq!(doc.pages[1].area.physical, OfdBox { x: 0.0, y: 0.0, w: 210.0, h: 297.0 });
    assert!(doc.warnings.iter().any(|w| w.contains("has no <PhysicalBox> and is not used")), "{:?}", doc.warnings);
}

#[test]
fn reads_composite_units() {
    let res = r#"<Res><CompositeGraphicUnits><CompositeGraphicUnit ID="40" Width="20" Height="10"><Thumbnail>1</Thumbnail><Content>
<PathObject Boundary="0 0 20 10"><AbbreviatedData>M 0 0 L 20 10</AbbreviatedData></PathObject>
<TextObject Boundary="0 0 5 5" Font="99" Size="3"><TextCode>x</TextCode></TextObject></Content></CompositeGraphicUnit></CompositeGraphicUnits></Res>"#;
    let body = r#"<CompositeObject Boundary="1 2 3 4" ResourceID="40"/><CompositeObject Boundary="1 2 3 4" ResourceID="41"/>"#;
    let files: Vec<(String, String)> = sample_files()
        .into_iter()
        .map(|(n, d)| match n.as_str() {
            "Doc_0/DocumentRes.xml" => (n, res.to_string()),
            "Doc_0/Pages/Page_0/Content.xml" => (n, page(body)),
            _ => (n, d),
        })
        .collect();
    let doc = parse(&build(&files)).unwrap();
    let unit = &doc.resources.composites[&40];
    assert_eq!((unit.width, unit.height, unit.objects.len()), (20.0, 10.0, 2));
    // References inside units are checked too, and so are the units objects place.
    assert!(doc.warnings.iter().any(|w| w.contains("font 99 is used but not defined")), "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.contains("composite graphic unit 41 is used but not defined")), "{:?}", doc.warnings);
    assert!(!doc.warnings.iter().any(|w| w.contains("not read yet")), "{:?}", doc.warnings);
}

#[test]
fn reads_axial_and_radial_shadings() {
    let body = r#"<PathObject Boundary="0 0 9 9" Fill="true"><FillColor><AxialShd Extend="1" MapType="Reflect" StartPoint="1 2" EndPoint="3 4">
<Segment Position="0.25"><Color Value="1 2 3" ColorSpace="3"/></Segment><Segment><Color Value="4 5 6"/></Segment></AxialShd></FillColor>
<StrokeColor><RadialShd StartPoint="1 1" EndPoint="2 2" StartRadius="0.5" EndRadius="3" Eccentricity="0.2"><Segment><Color Value="9"/></Segment></RadialShd></StrokeColor>
<AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject>
<PathObject Boundary="0 0 9 9" Fill="true"><FillColor><AxialShd StartPoint="1 2"><Segment><Color Value="1"/></Segment></AxialShd></FillColor></PathObject>"#;
    let doc = parse(&with("Doc_0/Pages/Page_0/Content.xml", &page(body))).unwrap();
    let objs = &doc.pages[0].content.layers[0].objects;
    let Object::Path(p) = &objs[0] else { panic!("{:?}", objs[0]) };
    let fill = p.fill_color.as_ref().unwrap();
    let shade = fill.shade.as_deref().unwrap();
    assert!(fill.shading);
    assert_eq!((shade.kind, shade.start, shade.end), (ShadeKind::Axial, (1.0, 2.0), (3.0, 4.0)));
    assert_eq!((shade.extend_start, shade.extend_end, shade.repeat), (true, false, true));
    assert_eq!(shade.stops[0], ShadeStop { position: Some(0.25), value: vec![1.0, 2.0, 3.0], color_space: Some(3) });
    assert_eq!(shade.stops[1].position, None);
    let stroke = p.stroke_color.as_ref().unwrap().shade.as_deref().unwrap();
    assert_eq!(stroke.kind, ShadeKind::Radial { start_radius: 0.5, end_radius: 3.0, eccentricity: 0.2 });
    // No end point: left out, with a warning; the colour is still marked as a shading.
    let Object::Path(q) = &objs[1] else { panic!("{:?}", objs[1]) };
    let broken = q.fill_color.as_ref().unwrap();
    assert!(broken.shading && broken.shade.is_none());
    assert!(doc.warnings.iter().any(|w| w.contains("a <AxialShd> could not be read")), "{:?}", doc.warnings);
    // An inverse undoes a matrix; a flat one has none.
    let m = Matrix { a: 2.0, b: 1.0, c: 0.0, d: 3.0, e: 5.0, f: -1.0 };
    let (x, y) = m.inverse().unwrap().apply(m.apply(1.5, -2.0).0, m.apply(1.5, -2.0).1);
    assert!((x - 1.5).abs() < 1e-12 && (y + 2.0).abs() < 1e-12);
    assert!(Matrix { a: 1.0, b: 2.0, c: 2.0, d: 4.0, e: 0.0, f: 0.0 }.inverse().is_none());
}

#[test]
fn reads_annotations() {
    let doc_xml = document_xml(2).replace("</ofd:Pages>", "</ofd:Pages><ofd:Annotations>Annots/Annotations.xml</ofd:Annotations>");
    let list = r#"<Annotations><Page PageID="101"><FileLoc>Page_1/Annotation.xml</FileLoc></Page><Page PageID="999"><FileLoc>Page_9/Annotation.xml</FileLoc></Page></Annotations>"#;
    let annots = r#"<PageAnnot><Annot Type="Stamp" Subtype="Seal" ID="1" NoView="true"><Parameters><Parameter Name="x">y</Parameter></Parameters>
<Appearance Boundary="10 20 30 40"><PathObject Boundary="0 0 1 1"><AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject></Appearance></Annot>
<Annot Type="Link" ID="2"/></PageAnnot>"#;
    let mut files: Vec<(String, String)> =
        sample_files().into_iter().map(|(n, d)| if n == "Doc_0/Document.xml" { (n, doc_xml.clone()) } else { (n, d) }).collect();
    files.push(("Doc_0/Annots/Annotations.xml".into(), list.into()));
    files.push(("Doc_0/Annots/Page_1/Annotation.xml".into(), annots.into()));
    let doc = parse(&build(&files)).unwrap();
    assert!(doc.pages[0].annotations.is_empty());
    let a = &doc.pages[1].annotations;
    assert_eq!(a.len(), 2);
    assert_eq!((a[0].kind.as_str(), a[0].subtype.as_deref(), a[0].visible, a[0].print, a[0].no_view), ("Stamp", Some("Seal"), true, true, true));
    assert_eq!(a[0].boundary, Some(OfdBox { x: 10.0, y: 20.0, w: 30.0, h: 40.0 }));
    assert_eq!(a[0].objects.len(), 1);
    assert_eq!((a[1].kind.as_str(), a[1].boundary, a[1].objects.len()), ("Link", None, 0));
    assert!(doc.warnings.iter().any(|w| w.contains("page 999")), "{:?}", doc.warnings);
    // A listed annotation file that is missing: a warning, not an error.
    files.retain(|(n, _)| n != "Doc_0/Annots/Page_1/Annotation.xml");
    files.push(("Doc_0/Annots/Annotations.xml".into(), list.replace(r#"<Page PageID="999"><FileLoc>Page_9/Annotation.xml</FileLoc></Page>"#, "")));
    files.retain(|(n, d)| !(n == "Doc_0/Annots/Annotations.xml" && d.contains("999")));
    let doc = parse(&build(&files)).unwrap();
    assert!(doc.warnings.iter().any(|w| w.contains("Page_1/Annotation.xml is listed but missing")), "{:?}", doc.warnings);
}

#[test]
fn reads_tiling_patterns_and_bounds_their_nesting() {
    let cell = |inner: &str| {
        format!(
            r#"<Pattern Width="10" Height="5" XStep="12" CTM="1 0 0 1 2 3"><CellContent><PathObject Boundary="0 0 1 1" Fill="true"><FillColor>{inner}</FillColor><AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject></CellContent></Pattern>"#
        )
    };
    // Patterns inside patterns, ten deep: read to four, never followed further.
    let deep = (0..10).fold(String::new(), |inner, _| cell(&inner));
    let body = format!(
        r#"<PathObject Boundary="0 0 9 9" Fill="true"><FillColor>{deep}</FillColor><AbbreviatedData>M 0 0 L 1 1</AbbreviatedData></PathObject>"#
    );
    let doc = parse(&with("Doc_0/Pages/Page_0/Content.xml", &page(&body))).unwrap();
    let Object::Path(p) = &doc.pages[0].content.layers[0].objects[0] else { panic!() };
    let pattern = p.fill_color.as_ref().unwrap().pattern.as_deref().unwrap();
    assert_eq!((pattern.width, pattern.height, pattern.x_step, pattern.y_step), (10.0, 5.0, 12.0, 5.0));
    assert_eq!(pattern.ctm, Some(Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 2.0, f: 3.0 }));
    assert!(!pattern.reflect && !pattern.relative_to_page);
    let mut depth = 0;
    let mut at = Some(pattern);
    while let Some(pt) = at {
        depth += 1;
        let Object::Path(inner) = &pt.objects[0] else { panic!() };
        at = inner.fill_color.as_ref().and_then(|c| c.pattern.as_deref());
    }
    assert_eq!(depth, 4);
    assert!(doc.warnings.iter().any(|w| w.contains("nested more than 4 deep")), "{:?}", doc.warnings);
}
