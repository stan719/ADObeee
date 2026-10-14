//! Edit a PDF ▸ Add content in the real shell (egui_kittest): type text onto a page, move it,
//! restyle it from the panel, add an image, delete with the keyboard.

use egui::{Pos2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{PdfCraftApp, QuickTool};

fn harness() -> Harness<'static, PdfCraftApp> {
    harness_bytes(include_bytes!("data/form.pdf").to_vec())
}

fn harness_bytes(bytes: Vec<u8>) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("form.pdf", None, bytes.clone()).unwrap();
        app.set_option("zoom", "150").unwrap();
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h
}

fn at(h: &Harness<'static, PdfCraftApp>, x: f32, y: f32) -> Pos2 {
    let r = h.state().views[0].page_screen_rect(0).expect("on screen");
    let k = r.width() / 300.0;
    r.min + egui::vec2(x * k, y * k)
}

fn click(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
}

fn added(h: &Harness<'static, PdfCraftApp>) -> Vec<pdfcraft_engine::Added> {
    let s = h.state();
    s.session.get(s.views[0].id).unwrap().added.clone()
}

/// A click whose press and release arrive in the same frame, as a quick real mouse click does.
fn quick_click(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    h.run_steps(3);
}

fn texts(h: &Harness<'static, PdfCraftApp>) -> Vec<String> {
    added(h)
        .into_iter()
        .filter_map(|a| match a.content {
            pdfcraft_engine::AddedContent::Text(t) => Some(t.text),
            _ => None,
        })
        .collect()
}

/// A two-page 300 × 400 document (#787).
fn two_pages() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 300 400] >>",
        "<< /Type /Page /Parent 2 0 R >>",
        "<< /Type /Page /Parent 2 0 R >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn an_added_image_drags_onto_the_next_page_and_undo_brings_it_back() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("two.pdf", None, two_pages()).unwrap();
        // Small enough that both pages are on screen.
        app.set_option("zoom", "50").unwrap();
        app
    });
    h.run_steps(4);
    // No Add-text tool: add_image opens the Edit a PDF panel itself, as it does for a user
    // placing a picture from the panel, and the drag must work with the plain Select tool.
    // A 30 × 20 image lands centred on page 1: [135, 190, 165, 210].
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 30, 20);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[200u8, 40, 40].repeat(600)).unwrap();
    }
    h.state_mut().add_image("logo.png".into(), png);
    h.run_steps(3);
    assert_eq!(added(&h).len(), 1);
    let r0 = h.state().views[0].page_screen_rect(0).expect("page 1 on screen");
    let r1 = h.state().views[0].page_screen_rect(1).expect("page 2 on screen");
    // Drag from the picture to the middle of page 2 (#787).
    let from = r0.center();
    let to = r1.center();
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(4);
    {
        let a = added(&h);
        assert_eq!(a.len(), 1, "still one item");
        assert_eq!(a[0].page, 1, "it landed on page 2");
        let r = a[0].content.rect();
        assert!(r.iter().zip([135.0, 190.0, 165.0, 210.0]).all(|(x, y)| (x - y).abs() < 3.0), "centred on page 2: {r:?}");
        let s = h.state();
        assert_eq!(s.session.get(s.views[0].id).unwrap().can_undo(), Some("Move content"));
    }
    // One undo step puts it back.
    h.state_mut().execute("edit.undo");
    h.run_steps(4);
    assert_eq!(added(&h)[0].page, 0, "back on page 1");
}

/// A two-page document with words on each page (#787): "Page N" at 24 pt from (20, 150),
/// so the text layer has glyphs to select.
fn text_pages() -> Vec<u8> {
    let mut objs: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..2).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    objs.push(format!("<< /Type /Pages /Kids [{}] /Count 2 /MediaBox [0 0 200 300] >>", kids.join(" ")));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for i in 0..2 {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i));
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", i + 1);
        objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn an_added_image_sitting_on_words_still_drags_onto_the_next_page() {
    // #787: dragging an attached picture that covers the page's words must move the
    // picture, not start a text selection. A control drag first proves the words are
    // extracted and selectable.
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("text.pdf", None, text_pages()).unwrap();
        app.set_option("zoom", "50").unwrap();
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r0 = h.state().views[0].page_screen_rect(0).expect("page 1 on screen");
    let r1 = h.state().views[0].page_screen_rect(1).expect("page 2 on screen");
    // User space is y-up on the 200 × 300 page; the screen is y-down.
    let sp = |u: [f32; 2]| egui::pos2(r0.left() + r0.width() * u[0] / 200.0, r0.top() + r0.height() * (300.0 - u[1]) / 300.0);
    // Control: with no picture, dragging from on the words out into empty space selects them.
    h.hover_at(sp([80.0, 155.0]));
    h.run_steps(1);
    h.drag_at(sp([80.0, 155.0]));
    h.run_steps(1);
    h.hover_at(sp([150.0, 140.0]));
    h.run_steps(1);
    h.drop_at(sp([150.0, 140.0]));
    h.run_steps(2);
    assert!(h.state().views[0].selected_text().is_some(), "the words are extracted and selectable");
    quick_click(&mut h, sp([150.0, 60.0]));
    assert!(h.state().views[0].selected_text().is_none(), "the control selection is cleared");
    // A 60 × 40 picture centred on the page lands at [70, 130, 130, 170] — on the words.
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 60, 40);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[200u8, 40, 40].repeat(60 * 40)).unwrap();
    }
    h.state_mut().add_image("logo.png".into(), png);
    h.run_steps(3);
    assert_eq!(added(&h).len(), 1);
    // The same drag in Edit text & images: its object and text handlers run ahead of the
    // added-content one, so the picture must still win the press.
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(2);
    // Drag it from on top of the words to the middle of page 2.
    h.hover_at(sp([80.0, 155.0]));
    h.run_steps(1);
    h.drag_at(sp([80.0, 155.0]));
    h.run_steps(1);
    let to = r1.center();
    for k in 1..=4 {
        h.hover_at(sp([80.0, 155.0]) + (to - sp([80.0, 155.0])) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(4);
    {
        let a = added(&h);
        assert_eq!(a.len(), 1, "the picture came along");
        assert_eq!(a[0].page, 1, "the picture landed on page 2");
    }
    assert!(h.state().views[0].selected_text().is_none(), "the words under it were not selected");
}

#[test]
fn an_unselected_added_image_over_words_still_drags_in_edit_text_and_images() {
    // #787: in Edit text & images the object and paragraph handlers run ahead of the
    // added-content one. With the picture covering the words and not selected (as when a
    // document with it is opened), the press must still move the picture.
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("text.pdf", None, text_pages()).unwrap();
        app.set_option("zoom", "50").unwrap();
        app
    });
    for _ in 0..60 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The picture sits on the words, placed the way an opened document has it: unselected.
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 60, 40);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[200u8, 40, 40].repeat(60 * 40)).unwrap();
    }
    let edit = pdfcraft_engine::Edit::AddImage {
        page: 0,
        rect: Some([70.0, 130.0, 130.0, 170.0]),
        name: "logo.png".into(),
        bytes: std::sync::Arc::new(png),
    };
    assert!(h.state_mut().apply_edit(edit));
    h.run_steps(3);
    assert!(h.state().views[0].content.selected.is_none(), "unselected, as opened from a file");
    assert!(h.state_mut().execute("edit.edit_text"));
    h.run_steps(2);
    let r0 = h.state().views[0].page_screen_rect(0).expect("page 1 on screen");
    let r1 = h.state().views[0].page_screen_rect(1).expect("page 2 on screen");
    let sp = |u: [f32; 2]| egui::pos2(r0.left() + r0.width() * u[0] / 200.0, r0.top() + r0.height() * (300.0 - u[1]) / 300.0);
    // User (80, 155) is inside the picture and on the words underneath it.
    let from = sp([80.0, 155.0]);
    let to = r1.center();
    h.hover_at(from);
    h.run_steps(1);
    h.drag_at(from);
    h.run_steps(1);
    for k in 1..=4 {
        h.hover_at(from + (to - from) * (k as f32 / 4.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(4);
    {
        let a = added(&h);
        assert_eq!(a.len(), 1, "the picture came along");
        assert_eq!(a[0].page, 1, "the picture landed on page 2");
    }
    assert!(h.state().views[0].selected_text().is_none(), "the words under it were not selected");
}

#[test]
fn clicking_elsewhere_keeps_the_text_and_starts_a_new_box() {
    // #74: a click elsewhere on the page threw away the text being typed and opened a new box.
    let mut h = harness();
    assert!(h.state_mut().execute("edit.text"));
    h.run_steps(2);
    let p = at(&h, 40.0, 100.0);
    click(&mut h, p);
    h.event(egui::Event::Text("First".into()));
    h.run_steps(2);
    // A slow click (press and release in different frames)…
    let p = at(&h, 40.0, 200.0);
    click(&mut h, p);
    assert_eq!(texts(&h), ["First"]);
    assert!(h.state().views[0].content.draft.is_some(), "a new box opened");
    assert_eq!(h.state().quick_tool, QuickTool::AddText, "still adding text");
    h.event(egui::Event::Text("Second".into()));
    h.run_steps(2);
    // …and a quick one (both in one frame, as a real mouse click usually arrives).
    let p = at(&h, 40.0, 300.0);
    quick_click(&mut h, p);
    assert_eq!(texts(&h), ["First", "Second"]);
    // Discard throws away only the box being typed.
    h.event(egui::Event::Text("Third".into()));
    h.run_steps(2);
    h.get_by_label("Discard this text (Esc)").click();
    h.run_steps(3);
    assert_eq!(texts(&h), ["First", "Second"]);
    assert!(h.state().views[0].content.draft.is_none());
}

#[test]
fn typing_moving_styling_and_deleting_added_content() {
    let mut h = harness();
    assert!(h.state_mut().execute("edit.text"));
    h.run_steps(2);
    h.get_by_label("Edit a PDF");
    let p = at(&h, 40.0, 300.0);
    click(&mut h, p);
    assert!(h.state().views[0].content.draft.is_some(), "the editor opened");
    h.event(egui::Event::Text("Reviewed".into()));
    h.run_steps(2);
    // Done keeps the text and goes back to Select, with the new text selected.
    h.get_by_label("Done adding text").click();
    h.run_steps(3);
    let a = added(&h);
    assert_eq!(a.len(), 1);
    let pdfcraft_engine::AddedContent::Text(t) = &a[0].content else { panic!() };
    assert_eq!(t.text, "Reviewed");
    assert_eq!(h.state().quick_tool, QuickTool::Select);
    h.run_steps(2);
    assert_eq!(h.state().views[0].content.selected, Some((0, 0)), "the new text is selected");
    // Bold from the Format panel: one undoable change.
    h.get_by_label("B").click();
    h.run_steps(3);

    let pdfcraft_engine::AddedContent::Text(t) = &added(&h)[0].content else { panic!() };
    assert!(t.bold);
    {
        let s = h.state();
        assert_eq!(s.session.get(s.views[0].id).unwrap().can_undo(), Some("Edit content"));
    }
    // Delete.
    h.key_press(egui::Key::Delete);
    h.run_steps(3);
    assert!(added(&h).is_empty());
    // An image lands in the middle of the page, selected.
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 30, 20);
        enc.set_color(png::ColorType::Rgb);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[90u8; 1800]).unwrap();
    }
    h.state_mut().add_image("logo.png".into(), png);
    h.run_steps(3);
    let a = added(&h);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].content.rect(), [135.0, 190.0, 165.0, 210.0]);
    assert_eq!(h.state().views[0].content.selected, Some((0, 0)));
    // Edit image: rotate clockwise turns the box around its centre.
    h.get_by_label("Rotate clockwise").click();
    h.run_steps(3);
    let a = added(&h);
    let pdfcraft_engine::AddedContent::Image(img) = &a[0].content else { panic!() };
    assert_eq!((img.rotation, img.rect), (3, [140.0, 185.0, 160.0, 215.0]));
    h.get_by_label("Flip horizontal").click();
    h.run_steps(3);
    let pdfcraft_engine::AddedContent::Image(img) = &added(&h)[0].content else { panic!() };
    assert!(img.flip_h);
}

/// #125: text the standard fonts can't draw stays in the editor with a warning instead of being
/// written to the page as `?` (or lost).
#[test]
fn text_the_standard_fonts_cant_draw_stays_in_the_editor() {
    let mut h = harness();
    assert!(h.state_mut().execute("edit.text"));
    h.run_steps(2);
    let p = at(&h, 40.0, 300.0);
    click(&mut h, p);
    h.event(egui::Event::Text("Hello 世界".into()));
    h.run_steps(2);
    h.get_by_label_contains("can't draw “世” (U+4E16)");
    if let Ok(dir) = std::env::var("PDFCRAFT_SHOTS") {
        h.render().unwrap().save(format!("{dir}/undrawable-text.png")).unwrap();
    }
    h.get_by_label("Done adding text").click();
    h.run_steps(3);
    assert!(texts(&h).is_empty(), "nothing was written to the page");
    let draft = h.state().views[0].content.draft.as_ref().map(|d| d.text.clone());
    assert_eq!(draft.as_deref(), Some("Hello 世界"), "the typed text is kept");
    // A click elsewhere on the page doesn't finish it, or replace it with a new box.
    let elsewhere = at(&h, 40.0, 150.0);
    click(&mut h, elsewhere);
    let draft = h.state().views[0].content.draft.as_ref().map(|d| d.text.clone());
    assert_eq!(draft.as_deref(), Some("Hello 世界"), "clicking elsewhere keeps the typed text");
    assert!(texts(&h).is_empty());
    // Once the text can be drawn, Done adds it (Done took the focus, so edit the draft directly).
    if let Some(d) = h.state_mut().views[0].content.draft.as_mut() {
        d.text = "Hello world".into();
    }
    h.run_steps(2);
    h.get_by_label("Done adding text").click();
    h.run_steps(3);
    assert_eq!(texts(&h), ["Hello world"]);
}

/// Text kept because it can't be drawn goes with its page when the page is deleted, so nothing
/// is left blocking Save out of sight.
#[test]
fn a_kept_draft_goes_with_its_deleted_page() {
    let mut h = harness();
    assert!(h.state_mut().apply_edit(pdfcraft_engine::Edit::InsertBlankPage { at: 1, width: 612.0, height: 792.0 }));
    h.run_steps(2);
    assert!(h.state_mut().execute("edit.text"));
    h.state_mut().views[0].go_to_page(1);
    h.run_steps(4);
    let p = h.state().views[0].page_screen_rect(1).expect("page 2 on screen").min + egui::vec2(40.0, 40.0);
    click(&mut h, p);
    h.event(egui::Event::Text("日本語".into()));
    h.run_steps(2);
    assert_eq!(h.state().views[0].content.draft.as_ref().map(|d| (d.page, d.text.as_str())), Some((1, "日本語")));
    assert!(h.state_mut().apply_edit(pdfcraft_engine::Edit::DeletePages { pages: vec![1] }));
    h.run_steps(2);
    assert!(h.state().views[0].content.draft.is_none());
}

#[test]
fn typing_thai_text_keeps_marks_and_commits_to_page() {
    if pdfcraft_fonts::document_thai_font().is_none() {
        return;
    }
    let fonts = pdfcraft_ui_egui::theme::font_definitions();
    let face = pdfcraft_fonts::document_thai_font().unwrap();
    assert!(fonts.families[&egui::FontFamily::Proportional].contains(&face.name()));
    let mut h = harness();
    assert!(h.state_mut().execute("edit.text"));
    h.run_steps(2);
    let p = at(&h, 40.0, 100.0);
    click(&mut h, p);
    h.event(egui::Event::Text("น้ำ กิ่ ผู้ใช้ Hello".into()));
    h.run_steps(3);
    let p = at(&h, 40.0, 200.0);
    click(&mut h, p);
    assert_eq!(texts(&h), ["น้ำ กิ่ ผู้ใช้ Hello"]);
}

/// The screen rectangle of an added item's display-space box (y up, from the page's bottom).
fn item_on_screen(h: &Harness<'static, PdfCraftApp>, r: [f64; 4]) -> egui::Rect {
    let s = h.state();
    let ph = s.session.get(s.views[0].id).unwrap().info.pages[0].height;
    let page = s.views[0].page_screen_rect(0).expect("on screen");
    let k = page.width() / 300.0;
    let p = |x: f64, y: f64| page.min + egui::vec2(x as f32 * k, (ph - y as f32) * k);
    egui::Rect::from_two_pos(p(r[0], r[1]), p(r[2], r[3]))
}

/// Dark pixels in `r`: glyph ink (the selection outline is blue, the page white).
fn ink(pixels: &image::RgbaImage, r: egui::Rect) -> usize {
    let (x0, y0, x1, y1) = (r.left().max(0.0) as u32, r.top().max(0.0) as u32, r.right() as u32, r.bottom() as u32);
    (y0..y1.min(pixels.height()))
        .flat_map(|y| (x0..x1.min(pixels.width())).map(move |x| (x, y)))
        .filter(|(x, y)| pixels.get_pixel(*x, *y).0[..3].iter().all(|v| *v < 90))
        .count()
}

/// Only the left part of the glyphs: kittest paints its cursor down and right of the pointer.
fn probe(r: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(r.min, egui::vec2(r.width() * 0.6, r.height()))
}

/// Ink in the probes of the text's first and dragged places.
fn ink_at(h: &mut Harness<'static, PdfCraftApp>, from: egui::Rect, to: egui::Rect) -> (usize, usize) {
    let pixels = h.render().unwrap();
    (ink(&pixels, probe(from)), ink(&pixels, probe(to)))
}

/// Types text, finishes it, and drags it up without releasing, until the drag preview shows.
/// Returns the text's box, its screen rectangles before and after, and the pointer.
fn drag_added_text(h: &mut Harness<'static, PdfCraftApp>) -> ([f64; 4], egui::Rect, egui::Rect, Pos2) {
    assert!(h.state_mut().execute("edit.text"));
    h.run_steps(2);
    let p = at(h, 40.0, 300.0);
    click(h, p);
    h.event(egui::Event::Text("MMMMMMMM".into()));
    h.run_steps(2);
    h.get_by_label("Done adding text").click();
    h.run_steps(3);
    let rect = added(h)[0].content.rect();
    let from = item_on_screen(h, rect);
    let mut shown = 0;
    for _ in 0..100 {
        h.run_steps(2);
        shown = ink(&h.render().unwrap(), probe(from));
        if shown > 20 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(shown > 20, "the added text is on the page ({shown} dark pixels)");
    let grab = pos2(from.right() - 12.0, from.center().y);
    let shift = egui::vec2(0.0, -150.0);
    h.hover_at(grab);
    h.run_steps(1);
    h.drag_at(grab);
    h.run_steps(1);
    h.hover_at(grab + shift);
    let to = from.translate(shift);
    for _ in 0..100 {
        h.run_steps(2);
        let (old, new) = ink_at(h, from, to);
        if old == 0 && new > 20 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (rect, from, to, grab + shift)
}

/// #1488: while a finished text box is dragged, its text follows the box instead of staying put
/// until the button is released.
#[test]
fn dragged_text_follows_the_pointer_before_release() {
    let mut h = harness();
    let (rect, from, to, end) = drag_added_text(&mut h);
    if let Ok(dir) = std::env::var("PDFCRAFT_SHOTS") {
        h.render().unwrap().save(format!("{dir}/added-text-drag.png")).unwrap();
    }
    let (old, new) = ink_at(&mut h, from, to);
    assert!(new > 20, "the text is drawn where the box was dragged ({new} dark pixels)");
    assert_eq!(old, 0, "and no longer where it was");
    assert_eq!(added(&h)[0].content.rect(), rect, "nothing is applied before release");
    h.drop_at(end);
    h.run_steps(3);
    assert_ne!(added(&h)[0].content.rect(), rect, "releasing moves it");
}

/// After release the text stays in its new place while the page is re-rendered with the move,
/// instead of jumping back to where it was for those frames.
#[test]
fn released_text_does_not_jump_back_before_the_page_is_redrawn() {
    // A page slow enough to render that the frames before its new raster can be looked at: many
    // pale squares, too light to count as ink.
    let mut content = String::from("0.9 g\n");
    for i in 0..60_000 {
        content.push_str(&format!("{} {} 1 1 re f\n", i % 290, (i / 290) % 390));
    }
    let pdf = format!(
        "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 400] >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /Contents 4 0 R >> endobj\n4 0 obj << /Length {} >> stream\n{content}\nendstream endobj\ntrailer << /Root 1 0 R >>\n%%EOF",
        content.len()
    );
    let mut h = harness_bytes(pdf.into_bytes());
    let (rect, from, to, end) = drag_added_text(&mut h);
    h.drop_at(end);
    // Frame by frame until the page has been re-rendered. The one frame that swaps in the new
    // raster reads back blank here (kittest renders it after its old texture is freed).
    let mut shown = Vec::new();
    while h.state().render_pending() || shown.is_empty() {
        h.run_steps(1);
        assert!(shown.len() < 500, "the page is re-rendered");
        let (old, new) = ink_at(&mut h, from, to);
        assert_eq!(old, 0, "frame {} after release: the text is back where it was", shown.len() + 1);
        shown.push(new);
    }
    assert!(shown.len() > 2, "frames before the new raster: {shown:?}");
    assert!(shown.iter().filter(|n| **n <= 20).count() <= 1, "the text stays where it was released: {shown:?}");
    assert_ne!(added(&h)[0].content.rect(), rect, "the move is applied");
    h.run_steps(2);
    let (old, new) = ink_at(&mut h, from, to);
    assert!(new > 20 && old == 0, "the redrawn page has the text in its new place ({old} old, {new} new)");
}
