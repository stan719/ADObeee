//! OFD → PDF on synthetic packages built here: no binary fixtures, no real documents.

use std::sync::Arc;

use pdfcraft_cos::{SaveOptions, write_full};

use super::*;

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// A stored (uncompressed) ZIP of `files`.
fn zip(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let offset = out.len() as u32;
        let header = |central: bool| {
            let mut h = Vec::new();
            h.extend(if central { 0x0201_4b50u32 } else { 0x0403_4b50 }.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend(0x0800u16.to_le_bytes());
            h.extend(0u16.to_le_bytes()); // stored
            h.extend([0u8; 4]);
            h.extend(crc32(data).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                h.extend([0u8; 10]);
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(false));
        out.extend(data);
        central.extend(header(true));
    }
    let cd = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 2, 2);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[90u8; 12]).unwrap();
    }
    out
}

const NS: &str = r#"xmlns:ofd="http://www.ofdspec.org/2016""#;

fn package(pages: &[&str], signed: bool) -> Vec<u8> {
    package_with_font(pages, signed, b"not a font")
}

/// `font` is the file behind font 5 (an embedded OFD font).
fn package_with_font(pages: &[&str], signed: bool, font: &[u8]) -> Vec<u8> {
    package_full(pages, signed, font, &[], "")
}

/// `templates` are (ID, z-order, body) template pages; `res` goes into the resource file.
fn package_full(pages: &[&str], signed: bool, font: &[u8], templates: &[(u32, &str, &str)], res: &str) -> Vec<u8> {
    let signatures = if signed { "<ofd:Signatures>Doc_0/Signs/Signatures.xml</ofd:Signatures>" } else { "" };
    let ofd = format!(
        r#"<ofd:OFD {NS} Version="1.0" DocType="OFD"><ofd:DocBody><ofd:DocInfo><ofd:Title>Test</ofd:Title></ofd:DocInfo><ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>{signatures}</ofd:DocBody></ofd:OFD>"#
    );
    let list: String = (0..pages.len()).map(|i| format!(r#"<ofd:Page ID="{}" BaseLoc="Pages/Page_{i}/Content.xml"/>"#, 100 + i)).collect();
    let tpls: String =
        templates.iter().map(|(id, z, _)| format!(r#"<ofd:TemplatePage ID="{id}" ZOrder="{z}" BaseLoc="Tpls/Tpl_{id}/Content.xml"/>"#)).collect();
    let document = format!(
        r#"<ofd:Document {NS}><ofd:CommonData><ofd:MaxUnitID>50</ofd:MaxUnitID><ofd:PageArea><ofd:PhysicalBox>0 0 210 297</ofd:PhysicalBox></ofd:PageArea>
<ofd:PublicRes>PublicRes.xml</ofd:PublicRes>{tpls}</ofd:CommonData><ofd:Pages>{list}</ofd:Pages></ofd:Document>"#
    );
    let res = format!(
        r#"<ofd:Res {NS} BaseLoc="Res"><ofd:Fonts><ofd:Font ID="4" FontName="宋体" FamilyName="宋体"/><ofd:Font ID="5" FontName="Fake"><ofd:FontFile>fake.ttf</ofd:FontFile></ofd:Font></ofd:Fonts>
<ofd:MultiMedias><ofd:MultiMedia ID="6" Type="Image" Format="PNG"><ofd:MediaFile>image_6.png</ofd:MediaFile></ofd:MultiMedia>
<ofd:MultiMedia ID="7" Type="Image" Format="JBIG2"><ofd:MediaFile>image_7.jb2</ofd:MediaFile></ofd:MultiMedia></ofd:MultiMedias>{res}</ofd:Res>"#
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("OFD.xml".into(), ofd.into_bytes()),
        ("Doc_0/Document.xml".into(), document.into_bytes()),
        ("Doc_0/PublicRes.xml".into(), res.into_bytes()),
        ("Doc_0/Res/image_6.png".into(), png_bytes()),
        ("Doc_0/Res/image_7.jb2".into(), b"\x97JB2\r\n\x1a\n".to_vec()),
        ("Doc_0/Res/fake.ttf".into(), font.to_vec()),
    ];
    for (i, body) in pages.iter().enumerate() {
        files.push((format!("Doc_0/Pages/Page_{i}/Content.xml"), body.as_bytes().to_vec()));
    }
    for (id, _, body) in templates {
        files.push((format!("Doc_0/Tpls/Tpl_{id}/Content.xml"), page(body).into_bytes()));
    }
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    zip(&refs)
}

fn page(body: &str) -> String {
    format!(r#"<ofd:Page {NS}><ofd:Content><ofd:Layer ID="10">{body}</ofd:Layer></ofd:Content></ofd:Page>"#)
}

/// Write, reparse (with hayro-syntax too) and return each page's dictionary and content.
fn convert(bytes: &[u8]) -> (Vec<(Dict, String)>, OfdReport, Document) {
    let (doc, report) = from_ofd(bytes).unwrap();
    let out = write_full(&doc, &SaveOptions::default()).unwrap();
    hayro_syntax::Pdf::new(out.clone()).expect("parses");
    let doc = Document::open(Arc::new(out)).unwrap();
    let pages = doc.get(doc.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap();
    let kids = doc.get(pages).as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();
    let pages = kids
        .iter()
        .map(|k| {
            let d = doc.resolve(k).as_dict().cloned().unwrap();
            let c = doc.resolve(d.get(b"Contents").unwrap());
            let Object::Stream(s) = &*c else { panic!("contents are not a stream") };
            let content = s.decoded().unwrap();
            (d, String::from_utf8_lossy(&content).into_owned())
        })
        .collect();
    (pages, report, doc)
}

fn media(d: &Dict) -> Vec<f64> {
    d.get(b"MediaBox").unwrap().as_array().unwrap().iter().map(|o| o.as_f64().unwrap()).collect()
}

fn font_names(doc: &Document, page: &Dict) -> Vec<String> {
    let res = doc.resolve(page.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let fonts = doc.resolve(res.get(b"Font").unwrap()).as_dict().cloned().unwrap();
    let mut names: Vec<String> = fonts
        .iter()
        .map(|(_, f)| {
            let f = doc.resolve(f).as_dict().cloned().unwrap();
            String::from_utf8_lossy(f.get(b"BaseFont").unwrap().as_name().unwrap()).into_owned()
        })
        .collect();
    names.sort();
    names
}

const PATH: &str = r#"<ofd:PathObject ID="11" Boundary="10 20 50 30" LineWidth="0.5" Fill="true">
<ofd:FillColor Value="255 0 0"/><ofd:StrokeColor Value="0 0 255"/>
<ofd:AbbreviatedData>M 0 0 L 50 0 Q 50 30 0 30 A 5 5 0 0 1 0 10 C</ofd:AbbreviatedData></ofd:PathObject>"#;

const TEXT: &str = r#"<ofd:TextObject ID="12" Boundary="20 40 100 10" Font="4" Size="5">
<ofd:TextCode X="0" Y="5" DeltaX="3 g 2 5">Fé发票</ofd:TextCode></ofd:TextObject>"#;

#[test]
fn pages_get_the_ofd_page_size() {
    let custom = r#"<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Area><ofd:PhysicalBox>0 0 100 50</ofd:PhysicalBox></ofd:Area><ofd:Content/></ofd:Page>"#;
    let (pages, report, _) = convert(&package(&[&page(""), custom], false));
    assert_eq!((pages.len(), report.pages), (2, 2));
    let a4 = media(&pages[0].0);
    assert!((a4[2] - 595.2756).abs() < 1e-3 && (a4[3] - 841.8898).abs() < 1e-3, "{a4:?}");
    let small = media(&pages[1].0);
    assert!((small[2] - 283.4646).abs() < 1e-3 && (small[3] - 141.7323).abs() < 1e-3, "{small:?}");
    // Millimetres, y down → points, y up.
    assert!(pages[0].1.starts_with("2.8346 0 0 -2.8346 0 841.8898 cm\n"), "{}", pages[0].1);
}

#[test]
fn paths_keep_colours_width_and_curves() {
    let (pages, report, _) = convert(&package(&[&page(PATH)], false));
    let ops = &pages[0].1;
    assert!(ops.contains("1 0 0 1 10 20 cm"), "placed at the boundary: {ops}");
    assert!(ops.contains("0.5 w") && ops.contains("1 0 0 rg") && ops.contains("0 0 1 RG"), "{ops}");
    assert!(ops.contains("0 0 m") && ops.contains("50 0 l") && ops.contains(" c\n") && ops.contains("\nh\n"), "{ops}");
    assert!(ops.contains("\nB\nQ"), "filled and stroked: {ops}");
    assert_eq!(report.skipped, 0, "{:?}", report.notes);
}

#[test]
fn text_is_real_text_with_standard_and_chinese_fallback_fonts() {
    let (pages, report, doc) = convert(&package(&[&page(TEXT)], false));
    let ops = &pages[0].1;
    // "F" and "é" through WinAnsi Helvetica (é is 0xE9), the Chinese through STSong-Light.
    assert!(ops.contains("BT\n") && ops.contains("<46> Tj") && ops.contains("<E9> Tj"), "{ops}");
    assert!(ops.contains("<53D1> Tj") && ops.contains("<7968> Tj"), "{ops}");
    assert!(
        ops.contains("5 0 0 -5 0 5 Tm") && ops.contains("5 0 0 -5 3 5 Tm") && ops.contains("5 0 0 -5 8 5 Tm") && ops.contains("5 0 0 -5 13 5 Tm"),
        "DeltaX positions: {ops}"
    );
    assert_eq!(font_names(&doc, &pages[0].0), ["Helvetica", "STSong-Light"]);
    assert_eq!(report.skipped, 0);
}

#[test]
fn a_font_file_that_is_not_a_font_falls_back() {
    let body = r#"<ofd:TextObject ID="12" Boundary="0 0 50 10" Font="5" Size="4"><ofd:TextCode X="1" Y="4">Hi</ofd:TextCode></ofd:TextObject>"#;
    let (pages, _, doc) = convert(&package(&[&page(body)], false));
    assert!(pages[0].1.contains("<48> Tj"), "{}", pages[0].1);
    assert_eq!(font_names(&doc, &pages[0].0), ["Helvetica"]);
}

#[test]
fn embedded_fonts_are_embedded_with_widths_and_to_unicode() {
    // The project's own Inter stands in for a font an OFD file carries.
    let body = r#"<ofd:TextObject ID="12" Boundary="0 0 50 10" Font="5" Size="4"><ofd:TextCode X="1" Y="4">Hi发</ofd:TextCode></ofd:TextObject>"#;
    let (pages, _, doc) = convert(&package_with_font(&[&page(body)], false, pdfcraft_fonts::INTER_REGULAR));
    let ops = &pages[0].1;
    assert!(ops.contains("/F5 1 Tf"), "{ops}");
    // Inter has no Chinese: that glyph falls back to STSong-Light.
    assert!(ops.contains("/FCJK 1 Tf") && ops.contains("<53D1> Tj"), "{ops}");
    assert_eq!(font_names(&doc, &pages[0].0), ["Fake", "STSong-Light"]);
    let res = doc.resolve(pages[0].0.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let fonts = doc.resolve(res.get(b"Font").unwrap()).as_dict().cloned().unwrap();
    let t0 = doc.resolve(fonts.get(b"F5").unwrap()).as_dict().cloned().unwrap();
    assert!(t0.get(b"ToUnicode").is_some());
    let kids = t0.get(b"DescendantFonts").unwrap().as_array().unwrap().clone();
    let cid = doc.resolve(&kids[0]).as_dict().cloned().unwrap();
    assert_eq!(cid.get(b"W").unwrap().as_array().unwrap().len(), 4, "two glyphs with widths");
    let fd = doc.resolve(cid.get(b"FontDescriptor").unwrap()).as_dict().cloned().unwrap();
    assert!(fd.get(b"FontFile2").is_some());
}

#[test]
fn images_are_drawn_and_undecodable_ones_are_reported() {
    let body = r#"<ofd:ImageObject ID="13" Boundary="150 10 40 40" CTM="40 0 0 40 0 0" ResourceID="6"/>
<ofd:ImageObject ID="14" Boundary="10 10 40 40" CTM="40 0 0 40 0 0" ResourceID="7"/>
<ofd:CompositeObject ID="15" Boundary="0 0 10 10" ResourceID="16"/>"#;
    let (pages, report, _) = convert(&package(&[&page(body)], false));
    let ops = &pages[0].1;
    assert!(ops.contains("40 0 0 40 150 10 cm") && ops.contains("1 0 0 -1 0 1 cm\n/Im6 Do"), "{ops}");
    assert!(!ops.contains("/Im7"), "{ops}");
    assert_eq!(report.skipped, 2, "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("JBIG2")) && report.notes.iter().any(|n| n.contains("composite")), "{:?}", report.notes);
    assert!(!report.signed);
}

#[test]
fn signed_files_say_seals_are_not_carried_over() {
    let (_, report, _) = convert(&package(&[&page(PATH)], true));
    assert!(report.signed);
    assert!(report.notes.iter().any(|n| n.contains("seals")), "{:?}", report.notes);
}

#[test]
fn files_that_are_not_ofd_are_errors_not_panics() {
    assert!(from_ofd(b"%PDF-1.7").is_err());
    assert!(from_ofd(b"PK\x03\x04garbage").is_err());
    let mut bytes = package(&[&page(PATH)], false);
    bytes.truncate(bytes.len() / 2);
    assert!(from_ofd(&bytes).is_err());
}

#[test]
fn source_kind_recognises_ofd() {
    assert_eq!(crate::source_kind("a.ofd", &package(&[&page("")], false)), Some(crate::SourceKind::Ofd));
}

#[test]
fn a_template_is_written_once_for_all_its_pages() {
    // A template used by every page is one form XObject, not a copy per page: a small file with
    // a large template and many pages must not multiply the template by its page count.
    let background = r#"<ofd:PathObject ID="61" Boundary="0 0 10 10"><ofd:AbbreviatedData>M 0 0 L 77 0</ofd:AbbreviatedData></ofd:PathObject>"#;
    let foreground = r#"<ofd:PathObject ID="63" Boundary="0 0 10 10"><ofd:AbbreviatedData>M 0 0 L 88 0</ofd:AbbreviatedData></ofd:PathObject>"#;
    let body = format!(
        r#"<ofd:Page {NS}><ofd:Template TemplateID="60"/><ofd:Template TemplateID="62"/><ofd:Content><ofd:Layer ID="10">{PATH}</ofd:Layer></ofd:Content></ofd:Page>"#
    );
    let pages: Vec<&str> = vec![body.as_str(); 30];
    let bytes = package_full(&pages, false, b"not a font", &[(60, "Background", background), (62, "Foreground", foreground)], "");
    let (pages, report, doc) = convert(&bytes);
    assert_eq!(report.pages, 30);
    let mut forms = Vec::new();
    for (dict, ops) in &pages {
        assert!(!ops.contains("77 0 l") && !ops.contains("88 0 l"), "the template is not copied into the page: {ops}");
        let (bg, own, fg) = (ops.find("/Tpl60 Do").unwrap(), ops.find("50 0 l").unwrap(), ops.find("/Tpl62 Do").unwrap());
        assert!(bg < own && own < fg, "background, page, foreground: {ops}");
        let res = doc.resolve(dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
        let xobjects = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
        forms.push((xobjects.get(b"Tpl60").unwrap().clone(), xobjects.get(b"Tpl62").unwrap().clone()));
    }
    assert!(forms.windows(2).all(|w| w[0] == w[1]), "every page draws the same two forms");
    let form = doc.resolve(&forms[0].0);
    let Object::Stream(form) = &*form else { panic!("the template is not a stream") };
    assert_eq!(form.dict.get(b"Subtype").and_then(Object::as_name), Some(&b"Form"[..]));
    assert!(String::from_utf8_lossy(&form.decoded().unwrap()).contains("77 0 l"));
}

/// A clip made of one path area, in the object's boundary space: the path's own boundary corner
/// (1, 1), then the area's CTM (×2), then the object's boundary corner (10, 20).
const CLIP: &str = r#"<ofd:Clips><ofd:Clip><ofd:Area CTM="2 0 0 2 0 0"><ofd:Path Boundary="1 1 5 5"><ofd:AbbreviatedData>M 0 0 L 5 0 L 5 5 L 0 5 C</ofd:AbbreviatedData></ofd:Path></ofd:Area></ofd:Clip></ofd:Clips>"#;

#[test]
fn paths_text_and_images_are_clipped_in_page_space() {
    let path = format!(
        r#"<ofd:PathObject ID="11" Boundary="10 20 50 30" CTM="3 0 0 3 0 0">{CLIP}<ofd:AbbreviatedData>M 0 0 L 9 9</ofd:AbbreviatedData></ofd:PathObject>"#
    );
    // TransFlag: the object's CTM (×2) applies to the clip too; an even-odd clip path.
    let text = r#"<ofd:TextObject ID="12" Boundary="20 40 100 10" Font="4" Size="5" CTM="2 0 0 2 0 0"><ofd:Clips TransFlag="true"><ofd:Clip><ofd:Area>
<ofd:Path Boundary="0 0 1 1" Rule="Even-Odd"><ofd:AbbreviatedData>M 0 0 L 1 0 L 1 1 C</ofd:AbbreviatedData></ofd:Path></ofd:Area></ofd:Clip></ofd:Clips>
<ofd:TextCode X="0" Y="5">A</ofd:TextCode></ofd:TextObject>"#;
    // An image clipped to its own unit square, through its CTM (×40).
    let image = r#"<ofd:ImageObject ID="13" Boundary="150 10 40 40" CTM="40 0 0 40 0 0" ResourceID="6"><ofd:Clips TransFlag="true"><ofd:Clip><ofd:Area>
<ofd:Path Boundary="0 0 1 1"><ofd:AbbreviatedData>M 0 0 L 1 0 L 1 1 C</ofd:AbbreviatedData></ofd:Path></ofd:Area></ofd:Clip></ofd:Clips></ofd:ImageObject>"#;
    let (pages, report, _) = convert(&package(&[&page(&format!("{path}{text}{image}"))], false));
    let ops = &pages[0].1;
    let clipped_path = "q\n12 22 m\n22 22 l\n22 32 l\n12 32 l\nh\nW n\n3 0 0 3 10 20 cm\n";
    assert!(ops.contains(clipped_path), "the clip comes before the object's own space: {ops}");
    let clipped_text = "q\n20 40 m\n22 40 l\n22 42 l\nh\nW* n\n2 0 0 2 20 40 cm\n";
    assert!(ops.contains(clipped_text), "{ops}");
    let clipped_image = "q\n150 10 m\n190 10 l\n190 50 l\nh\nW n\n40 0 0 40 150 10 cm\n";
    assert!(ops.contains(clipped_image), "{ops}");
    assert_eq!(report.skipped, 0, "{:?}", report.notes);
}

#[test]
fn several_clips_intersect_and_curves_are_mapped() {
    let second = r#"<ofd:Clip><ofd:Area><ofd:Path Boundary="0 0 4 4"><ofd:AbbreviatedData>M 0 0 Q 4 0 4 4 A 2 2 0 0 1 0 0 C</ofd:AbbreviatedData></ofd:Path></ofd:Area></ofd:Clip>"#;
    let clips = CLIP.replace("</ofd:Clips>", &format!("{second}</ofd:Clips>"));
    let body =
        format!(r#"<ofd:PathObject ID="11" Boundary="10 20 50 30">{clips}<ofd:AbbreviatedData>M 0 0 L 9 9</ofd:AbbreviatedData></ofd:PathObject>"#);
    let (pages, report, _) = convert(&package(&[&page(&body)], false));
    let ops = &pages[0].1;
    assert_eq!(ops.matches("W n\n").count(), 2, "{ops}");
    // The quadratic curve becomes a cubic, moved to the boundary corner: (0,0)→(10,20) and
    // control points ⅔ of the way to (4,0), ending at (4,4)→(14,24).
    assert!(ops.contains("10 20 m\n12.6667 20 14 21.3333 14 24 c\n"), "{ops}");
    assert_eq!(report.skipped, 0, "{:?}", report.notes);
}

#[test]
fn clips_that_cannot_be_drawn_are_skipped_and_reported() {
    let text_area = r#"<ofd:Clips><ofd:Clip><ofd:Area><ofd:Text Boundary="0 0 5 5" Font="4" Size="3"><ofd:TextCode X="0" Y="3">X</ofd:TextCode></ofd:Text></ofd:Area></ofd:Clip></ofd:Clips>"#;
    let two_areas = CLIP.replace(
        "</ofd:Area>",
        "</ofd:Area><ofd:Area><ofd:Path Boundary=\"0 0 1 1\"><ofd:AbbreviatedData>M 0 0 L 1 1</ofd:AbbreviatedData></ofd:Path></ofd:Area>",
    );
    let empty = r#"<ofd:Clips><ofd:Clip/><ofd:Clip><ofd:Area/></ofd:Clip></ofd:Clips>"#;
    let objects = [text_area, two_areas.as_str(), empty]
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                r#"<ofd:PathObject ID="{}" Boundary="0 0 9 9">{c}<ofd:AbbreviatedData>M 0 0 L 9 {i}</ofd:AbbreviatedData></ofd:PathObject>"#,
                20 + i
            )
        })
        .collect::<String>();
    let (pages, report, _) = convert(&package(&[&page(&objects)], false));
    let ops = &pages[0].1;
    // Everything is still drawn, without the clips.
    assert!(!ops.contains(" W"), "{ops}");
    assert!(ops.contains("9 0 l") && ops.contains("9 1 l") && ops.contains("9 2 l"), "{ops}");
    let by_kind: Vec<(&str, usize)> = report.skipped_by_kind.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(by_kind, [("clipping regions made of several areas", 1), ("empty clipping regions", 2), ("text used as a clipping region", 1)]);
    assert_eq!(report.skipped, 4);
    assert!(report.notes.iter().any(|n| n.contains("text used as a clipping region")), "{:?}", report.notes);
}

#[test]
fn composite_units_are_drawn_once_as_forms() {
    // Unit 40 draws a path and places unit 43; unit 43 places itself.
    let units = r#"<ofd:CompositeGraphicUnits><ofd:CompositeGraphicUnit ID="40" Width="20" Height="10"><ofd:Content>
<ofd:PathObject ID="41" Boundary="0 0 20 10" Fill="true"><ofd:FillColor Value="0 0 255"/><ofd:AbbreviatedData>M 0 0 L 66 0 L 66 10 C</ofd:AbbreviatedData></ofd:PathObject>
<ofd:CompositeObject ID="42" Boundary="0 0 1 1" ResourceID="43"/></ofd:Content></ofd:CompositeGraphicUnit>
<ofd:CompositeGraphicUnit ID="43" Width="1" Height="1"><ofd:Content><ofd:CompositeObject ID="44" Boundary="0 0 1 1" ResourceID="43"/></ofd:Content></ofd:CompositeGraphicUnit></ofd:CompositeGraphicUnits>"#;
    let body = r#"<ofd:CompositeObject ID="50" Boundary="10 20 20 10" CTM="0.5 0 0 0.5 0 0" ResourceID="40"/>
<ofd:CompositeObject ID="51" Boundary="100 20 20 10" Alpha="102" ResourceID="40"/><ofd:CompositeObject ID="52" Boundary="0 0 1 1" ResourceID="99"/>"#;
    let (pages, report, doc) = convert(&package_full(&[&page(body)], false, b"not a font", &[], units));
    let (dict, ops) = &pages[0];
    assert!(ops.contains("q\n0.5 0 0 0.5 10 20 cm\n/Cu40 Do\nQ"), "placed by its CTM and boundary: {ops}");
    assert!(ops.contains("q\n1 0 0 1 100 20 cm\n/GS102_102 gs\n/Cu40 Do\nQ"), "faded as a whole: {ops}");
    assert!(!ops.contains("66 0 l"), "the unit is drawn in its form, not copied: {ops}");
    let res = doc.resolve(dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let xobjects = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    let form = doc.resolve(xobjects.get(b"Cu40").unwrap());
    let Object::Stream(form) = &*form else { panic!("the unit is not a stream") };
    let group = form.dict.get(b"Group").and_then(Object::as_dict).unwrap();
    assert_eq!(group.get(b"S").and_then(Object::as_name), Some(&b"Transparency"[..]));
    let unit_ops = String::from_utf8_lossy(&form.decoded().unwrap()).into_owned();
    assert!(unit_ops.contains("66 0 l") && unit_ops.contains("0 0 1 rg") && unit_ops.contains("/Cu43 Do"), "{unit_ops}");
    // A unit placing itself and an undefined unit: reported, never followed round.
    let by_kind: Vec<(&str, usize)> = report.skipped_by_kind.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(by_kind, [("composite graphic units that aren't defined", 1), ("composite graphic units that contain themselves", 1)]);
}

#[test]
fn deeply_nested_composite_units_stop() {
    // A chain of 40 units, each placing the next: drawn to MAX_UNIT_DEPTH, the rest reported.
    let units: String = (0..40)
        .map(|i| {
            format!(
                r#"<ofd:CompositeGraphicUnit ID="{}" Width="1" Height="1"><ofd:Content><ofd:CompositeObject Boundary="0 0 1 1" ResourceID="{}"/></ofd:Content></ofd:CompositeGraphicUnit>"#,
                1000 + i,
                1001 + i
            )
        })
        .collect();
    let units = format!("<ofd:CompositeGraphicUnits>{units}</ofd:CompositeGraphicUnits>");
    let body = r#"<ofd:CompositeObject ID="50" Boundary="0 0 1 1" ResourceID="1000"/>"#;
    let (_, report, _) = convert(&package_full(&[&page(body)], false, b"not a font", &[], &units));
    assert_eq!(report.skipped_by_kind.get("composite graphic units nested too deep"), Some(&1), "{:?}", report.skipped_by_kind);
}

/// Four segments as real writers send them: two pairs at nearly the same place.
const AXIAL: &str = r#"<ofd:AxialShd Extend="3" StartPoint="0 5" EndPoint="40 5"><ofd:Segment Position="0"><ofd:Color Value="100 192 171"/></ofd:Segment>
<ofd:Segment Position="0.0001"><ofd:Color Value="100 192 171"/></ofd:Segment><ofd:Segment Position="0.9835"><ofd:Color Value="0 140 207"/></ofd:Segment>
<ofd:Segment Position="1"><ofd:Color Value="0 140 207"/></ofd:Segment></ofd:AxialShd>"#;

#[test]
fn axial_and_radial_shadings_fill_paths_and_text() {
    let radial = r#"<ofd:RadialShd Extend="2" StartPoint="5 5" EndPoint="5 5" EndRadius="6"><ofd:Segment><ofd:Color Value="255"/></ofd:Segment><ofd:Segment><ofd:Color Value="0"/></ofd:Segment></ofd:RadialShd>"#;
    let path = format!(
        r#"<ofd:PathObject ID="11" Boundary="10 20 40 10" CTM="2 0 0 2 0 0" Fill="true" Stroke="false"><ofd:FillColor>{AXIAL}</ofd:FillColor><ofd:AbbreviatedData>M 0 0 L 20 0 L 20 5 C</ofd:AbbreviatedData></ofd:PathObject>"#
    );
    let ring = format!(
        r#"<ofd:PathObject ID="12" Boundary="60 20 10 10" Fill="true" Rule="Even-Odd"><ofd:FillColor>{radial}</ofd:FillColor><ofd:AbbreviatedData>M 0 0 L 10 0 L 10 10 C</ofd:AbbreviatedData></ofd:PathObject>"#
    );
    let text = format!(
        r#"<ofd:TextObject ID="13" Boundary="20 40 100 10" Font="4" Size="5"><ofd:FillColor>{AXIAL}</ofd:FillColor><ofd:TextCode X="0" Y="5">Hi</ofd:TextCode></ofd:TextObject>"#
    );
    let (pages, report, doc) = convert(&package(&[&page(&format!("{path}{ring}{text}"))], false));
    let (dict, ops) = &pages[0];
    // The outline clips; the shading paints in the boundary's space (the CTM undone).
    assert!(ops.contains("2 0 0 2 10 20 cm\n"), "{ops}");
    assert!(ops.contains("q\n0 0 m\n20 0 l\n20 5 l\nh\nW n\n0.5 0 0 0.5 0 0 cm\n/Sh1 sh\nQ\nQ\n"), "{ops}");
    // Even-odd outline, then the default black stroke over the shading.
    assert!(ops.contains("h\nW* n\n/Sh2 sh\nQ\n0 0 m\n10 0 l\n10 10 l\nh\nS\nQ\n"), "{ops}");
    // Text: the glyphs clip (mode 7), the shading paints them; still real text.
    assert!(ops.contains("BT\n7 Tr\n") && ops.contains("<69> Tj\nET\n/Sh3 sh\nQ\n"), "{ops}");
    assert_eq!(report.skipped, 0, "{:?}", report.notes);

    let res = doc.resolve(dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let shadings = doc.resolve(res.get(b"Shading").unwrap()).as_dict().cloned().unwrap();
    let get = |name: &[u8]| doc.resolve(shadings.get(name).unwrap()).as_dict().cloned().unwrap();
    let nums = |o: &Object| o.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect::<Vec<_>>();
    let axial = get(b"Sh1");
    assert_eq!(axial.get(b"ShadingType").and_then(Object::as_int), Some(2));
    assert_eq!(axial.get(b"ColorSpace").and_then(Object::as_name), Some(&b"DeviceRGB"[..]));
    assert_eq!(nums(axial.get(b"Coords").unwrap()), [0.0, 5.0, 40.0, 5.0]);
    assert_eq!(axial.get(b"Extend").unwrap().as_array().unwrap(), &[Object::Bool(true), Object::Bool(true)]);
    let f = axial.get(b"Function").and_then(Object::as_dict).unwrap();
    assert_eq!(f.get(b"FunctionType").and_then(Object::as_int), Some(3));
    assert_eq!(nums(f.get(b"Bounds").unwrap()), [0.0001, 0.9835]);
    let pieces = f.get(b"Functions").unwrap().as_array().unwrap();
    assert_eq!(pieces.len(), 3);
    let first = pieces[0].as_dict().unwrap();
    assert!((nums(first.get(b"C0").unwrap())[1] - 192.0 / 255.0).abs() < 1e-9);
    let radial = get(b"Sh2");
    assert_eq!(radial.get(b"ShadingType").and_then(Object::as_int), Some(3));
    assert_eq!(radial.get(b"ColorSpace").and_then(Object::as_name), Some(&b"DeviceGray"[..]));
    assert_eq!(nums(radial.get(b"Coords").unwrap()), [5.0, 5.0, 0.0, 5.0, 5.0, 6.0]);
    assert_eq!(radial.get(b"Extend").unwrap().as_array().unwrap(), &[Object::Bool(false), Object::Bool(true)]);
    let f = radial.get(b"Function").and_then(Object::as_dict).unwrap();
    assert_eq!((f.get(b"FunctionType").and_then(Object::as_int), nums(f.get(b"C1").unwrap())), (Some(2), vec![0.0]));
}

#[test]
fn shadings_that_cannot_be_drawn_are_reported() {
    let fill = |c: &str, i: usize| {
        format!(
            r#"<ofd:PathObject ID="{i}" Boundary="0 0 9 9" Fill="true"><ofd:FillColor>{c}</ofd:FillColor><ofd:AbbreviatedData>M 0 0 L 9 {i}</ofd:AbbreviatedData></ofd:PathObject>"#
        )
    };
    let repeat =
        r#"<ofd:AxialShd MapType="Repeat" StartPoint="0 0" EndPoint="1 1"><ofd:Segment><ofd:Color Value="0 0 0"/></ofd:Segment></ofd:AxialShd>"#;
    let ellipse = r#"<ofd:RadialShd Eccentricity="0.5" StartPoint="0 0" EndPoint="1 1" EndRadius="2"><ofd:Segment><ofd:Color Value="0 0 0"/></ofd:Segment></ofd:RadialShd>"#;
    let gouraud = r#"<ofd:GouraudShd><ofd:Point X="0" Y="0"/></ofd:GouraudShd>"#;
    let unreadable = r#"<ofd:AxialShd StartPoint="0" EndPoint="1 1"><ofd:Segment><ofd:Color Value="0 0 0"/></ofd:Segment></ofd:AxialShd>"#;
    let stroked = format!(
        r#"<ofd:PathObject ID="30" Boundary="0 0 9 9"><ofd:StrokeColor>{AXIAL}</ofd:StrokeColor><ofd:AbbreviatedData>M 0 0 L 9 9</ofd:AbbreviatedData></ofd:PathObject>"#
    );
    let body = format!("{}{}{}{}{stroked}", fill(repeat, 20), fill(ellipse, 21), fill(gouraud, 22), fill(unreadable, 23));
    let (pages, report, _) = convert(&package(&[&page(&body)], false));
    assert!(!pages[0].1.contains(" sh\n"), "{}", pages[0].1);
    let by_kind: Vec<(&str, usize)> = report.skipped_by_kind.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        by_kind,
        [
            ("elliptical radial shadings", 1),
            ("pattern and shading colours", 2),
            ("repeating or reflecting shadings", 1),
            ("shading colours on strokes", 1)
        ]
    );
    assert!(report.notes.iter().any(|n| n.contains("<AxialShd> could not be read")), "{:?}", report.notes);
}

#[test]
fn annotations_are_drawn_over_the_page() {
    // The annotation list sits in Document.xml after Pages; one page's file holds a watermark,
    // a print-only stamp, a hidden one and a link.
    let mut bytes_files: Vec<(String, Vec<u8>)> = Vec::new();
    let annots = r#"<ofd:Annotations xmlns:ofd="http://www.ofdspec.org/2016"><ofd:Page PageID="100"><ofd:FileLoc>Page_0.xml</ofd:FileLoc></ofd:Page></ofd:Annotations>"#;
    let page_annots = r#"<ofd:PageAnnot xmlns:ofd="http://www.ofdspec.org/2016">
<ofd:Annot Type="Watermark" ID="70"><ofd:Appearance Boundary="90 8 30 20"><ofd:PathObject ID="71" Boundary="0 0 30 20"><ofd:AbbreviatedData>M 0 0 L 77 0</ofd:AbbreviatedData></ofd:PathObject></ofd:Appearance></ofd:Annot>
<ofd:Annot Type="Stamp" ID="72" NoView="true"><ofd:Appearance Boundary="0 0 9 9"><ofd:PathObject ID="73" Boundary="0 0 9 9"><ofd:AbbreviatedData>M 0 0 L 88 0</ofd:AbbreviatedData></ofd:PathObject></ofd:Appearance></ofd:Annot>
<ofd:Annot Type="Stamp" ID="74" Visible="false"><ofd:Appearance Boundary="0 0 9 9"><ofd:PathObject ID="75" Boundary="0 0 9 9"><ofd:AbbreviatedData>M 0 0 L 99 0</ofd:AbbreviatedData></ofd:PathObject></ofd:Appearance></ofd:Annot>
<ofd:Annot Type="Link" ID="76"><ofd:Appearance Boundary="0 0 9 9"><ofd:PathObject ID="77" Visible="false" Boundary="0 0 9 9"><ofd:AbbreviatedData>M 0 0 L 9 9</ofd:AbbreviatedData></ofd:PathObject></ofd:Appearance></ofd:Annot>
</ofd:PageAnnot>"#;
    let base = package(&[&page(PATH)], false);
    // Rebuild the package with the annotation files and the list in Document.xml.
    let pkg = pdfcraft_ofd::zip::Package::open(&base, &pdfcraft_ofd::Limits::default()).unwrap();
    for name in pkg.names() {
        let mut data = pkg.read(name).unwrap();
        if name == "Doc_0/Document.xml" {
            let doc =
                String::from_utf8(data).unwrap().replace("</ofd:Pages>", "</ofd:Pages><ofd:Annotations>Annots/Annotations.xml</ofd:Annotations>");
            data = doc.into_bytes();
        }
        bytes_files.push((name.to_string(), data));
    }
    bytes_files.push(("Doc_0/Annots/Annotations.xml".into(), annots.as_bytes().to_vec()));
    bytes_files.push(("Doc_0/Annots/Page_0.xml".into(), page_annots.as_bytes().to_vec()));
    let refs: Vec<(&str, Vec<u8>)> = bytes_files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    let (pages, report, _) = convert(&zip(&refs));
    let ops = &pages[0].1;
    // After the page's own path, from the appearance's corner.
    let find = |needle: &str| ops.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {ops}"));
    let (own, watermark) = (find("50 0 l"), find("q\n1 0 0 1 90 8 cm\nq\n1 0 0 1 0 0 cm\n"));
    assert!(own < watermark && ops.contains("77 0 l"), "{ops}");
    assert!(!ops.contains("88 0 l") && !ops.contains("99 0 l"), "print-only and hidden annotations are not drawn: {ops}");
    assert_eq!(report.skipped_by_kind.get("annotations shown only when printed"), Some(&1));
    assert!(report.notes.iter().any(|n| n.contains("not clickable")), "{:?}", report.notes);
}

#[test]
fn tiling_patterns_repeat_their_cell_inside_the_outline() {
    let pattern = |attrs: &str| {
        format!(
            r#"<ofd:Pattern Width="10" Height="5" {attrs}><ofd:CellContent><ofd:PathObject ID="91" Boundary="0 0 10 5" Fill="true"><ofd:FillColor Value="255 0 0"/><ofd:AbbreviatedData>M 0 0 L 7 0 L 7 3 C</ofd:AbbreviatedData></ofd:PathObject></ofd:CellContent></ofd:Pattern>"#
        )
    };
    let filled = |i: usize, attrs: &str| {
        format!(
            r#"<ofd:PathObject ID="{i}" Boundary="0 0 20 10" Fill="true" Stroke="false"><ofd:FillColor>{}</ofd:FillColor><ofd:AbbreviatedData>M 0 0 L 20 0 L 20 10 C</ofd:AbbreviatedData></ofd:PathObject>"#,
            pattern(attrs)
        )
    };
    let (pages, report, doc) = convert(&package(&[&page(&filled(20, ""))], false));
    let (dict, ops) = &pages[0];
    // Clipped to the outline; cells every 10 × 5 over the 20 × 10 boundary, one beyond each edge.
    assert!(ops.contains("h\nW n\nq\n1 0 0 1 -10 -5 cm\n/Pc1 Do\nQ\n"), "{ops}");
    assert_eq!(ops.matches("/Pc1 Do").count(), 4 * 4, "{ops}");
    assert!(ops.contains("q\n1 0 0 1 10 5 cm\n/Pc1 Do\nQ\n"), "{ops}");
    let res = doc.resolve(dict.get(b"Resources").unwrap()).as_dict().cloned().unwrap();
    let xobjects = doc.resolve(res.get(b"XObject").unwrap()).as_dict().cloned().unwrap();
    let cell = doc.resolve(xobjects.get(b"Pc1").unwrap());
    let Object::Stream(cell) = &*cell else { panic!("the cell is not a stream") };
    let bbox: Vec<f64> = cell.dict.get(b"BBox").unwrap().as_array().unwrap().iter().map(|o| o.as_f64().unwrap()).collect();
    assert_eq!(bbox, [0.0, 0.0, 10.0, 5.0], "a cell is clipped to its size");
    assert!(String::from_utf8_lossy(&cell.decoded().unwrap()).contains("7 3 l"));
    assert_eq!(report.skipped, 0, "{:?}", report.notes);

    // What isn't drawn is reported.
    let body = [filled(21, r#"ReflectMethod="Row""#), filled(22, r#"RelativeTo="Page""#), filled(23, r#"XStep="0.0001" YStep="0.0001""#)].concat();
    let (pages, report, _) = convert(&package(&[&page(&body)], false));
    assert!(!pages[0].1.contains(" Do"), "{}", pages[0].1);
    let by_kind: Vec<(&str, usize)> = report.skipped_by_kind.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(by_kind, [("mirrored patterns", 1), ("patterns aligned to the page", 1), ("patterns with too many cells", 1)]);
}

#[test]
fn pattern_cells_are_bounded_for_the_whole_document() {
    // 30 objects of 3,844 cells each (600 × 300 at 10 × 5, one beyond each edge): 26 fit in the
    // document's 100,000 cells, the other 4 are reported instead of growing the PDF.
    let cell = r#"<ofd:Pattern Width="10" Height="5"><ofd:CellContent><ofd:PathObject Boundary="0 0 1 1"><ofd:AbbreviatedData>M 0 0 L 1 1</ofd:AbbreviatedData></ofd:PathObject></ofd:CellContent></ofd:Pattern>"#;
    let body: String = (0..30)
        .map(|i| {
            format!(r#"<ofd:PathObject ID="{}" Boundary="0 0 600 300" Fill="true" Stroke="false"><ofd:FillColor>{cell}</ofd:FillColor><ofd:AbbreviatedData>M 0 0 L 1 1</ofd:AbbreviatedData></ofd:PathObject>"#, 200 + i)
        })
        .collect();
    let (pages, report, _) = convert(&package(&[&page(&body)], false));
    assert_eq!(pages[0].1.matches(" Do\n").count(), 26 * 3844);
    assert_eq!(report.skipped_by_kind.get("patterns with too many cells"), Some(&4), "{:?}", report.skipped_by_kind);
}

#[test]
fn standard_font_text_advances_by_the_faces_own_widths() {
    // Without DeltaX the next character follows the font's advance: Times-Roman's "f" is 333/1000
    // em (Helvetica's is 278), Courier's every character 600.
    let fonts = r#"<ofd:Fonts><ofd:Font ID="8" FontName="SerifFace" Serif="true"/><ofd:Font ID="9" FontName="Mono" FixedWidth="true"/></ofd:Fonts>"#;
    let body = r#"<ofd:TextObject ID="11" Boundary="0 0 50 10" Font="8" Size="10"><ofd:TextCode X="0" Y="5">fi</ofd:TextCode></ofd:TextObject>
<ofd:TextObject ID="12" Boundary="0 20 50 10" Font="9" Size="10"><ofd:TextCode X="0" Y="5">fi</ofd:TextCode></ofd:TextObject>"#;
    let (pages, _, doc) = convert(&package_full(&[&page(body)], false, b"not a font", &[], fonts));
    let ops = &pages[0].1;
    assert!(ops.contains("/TimesRoman 1 Tf\n10 0 0 -10 3.33 5 Tm\n<69> Tj"), "{ops}");
    assert!(ops.contains("/Courier 1 Tf\n10 0 0 -10 6 5 Tm\n<69> Tj"), "{ops}");
    assert_eq!(font_names(&doc, &pages[0].0), ["Courier", "Times-Roman"]);
}

#[test]
fn glyph_positions_at_the_end_of_the_index_range_do_not_overflow() {
    // A CGTransform at the largest CodePosition: on a 32-bit target (wasm32) position + index
    // used to overflow. The glyphs that can't be placed are ignored; the text still converts.
    let body = r#"<ofd:TextObject ID="11" Boundary="0 0 50 10" Font="5" Size="4"><ofd:CGTransform CodePosition="4294967295" CodeCount="2" GlyphCount="2"><ofd:Glyphs>3 4</ofd:Glyphs></ofd:CGTransform><ofd:TextCode X="1" Y="4">Hi</ofd:TextCode></ofd:TextObject>"#;
    let (pages, _, _) = convert(&package_with_font(&[&page(body)], false, pdfcraft_fonts::INTER_REGULAR));
    assert!(pages[0].1.contains("/F5 1 Tf"), "{}", pages[0].1);
}
