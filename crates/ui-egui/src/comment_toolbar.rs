//! The floating bar over a selected text markup comment (highlight, underline, strikethrough,
//! squiggly): Reply, Colour and Delete, the quick actions Acrobat shows next to a selected
//! highlight. The same actions stay in the canvas context menu and the Comments panel.

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use pdfcraft_engine::Edit;
use pdfcraft_render::{Annotation, DocInfo};

use crate::QuickTool;
use crate::canvas::DocView;
use crate::comments::{CanvasAction, CommentPrefs, PageCx, color32, is_markup, swatch_grid};
use crate::icons;

/// Side of each button, in points.
const BUTTON: f32 = 28.0;
/// The bar's size as laid out: three buttons, their spacing and the popup frame's margins.
const BAR_SIZE: egui::Vec2 = vec2(3.0 * BUTTON + 30.0, BUTTON + 12.0);
/// Space between the marked text and the bar.
const GAP: f32 = 6.0;
/// The bar keeps this far inside the document view.
const INSET: f32 = 4.0;

/// Where the bar goes for marked text spanning `bounds` (screen space): above its right end, or
/// below it when there is no room above. `None` when the text is off screen.
pub(crate) fn bar_pos(bounds: Rect, viewport: Rect) -> Option<Pos2> {
    if !bounds.is_finite() || !viewport.is_finite() || !viewport.intersects(bounds) {
        return None;
    }
    let max_x = (viewport.right() - BAR_SIZE.x - INSET).max(viewport.left() + INSET);
    let x = (bounds.right() - BAR_SIZE.x).clamp(viewport.left() + INSET, max_x);
    let above = bounds.top() - GAP - BAR_SIZE.y;
    let y = if above >= viewport.top() + INSET { above } else { bounds.bottom() + GAP };
    Some(pos2(x, y))
}

/// The selected comment, when it is a top-level text markup comment.
fn selected_markup(info: &DocInfo, view: &DocView) -> Option<Annotation> {
    let (page, index) = view.comments.selected?;
    info.annotations.iter().find(|a| a.page == page && a.index == index && a.in_reply_to.is_none() && is_markup(&a.subtype)).cloned()
}

/// A round swatch in the comment's colour that opens the colour grid (the quick bar's look).
fn colour_button(ui: &mut egui::Ui, color: Option<[f32; 3]>) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(BUTTON, BUTTON), Sense::click());
    let fill = color.map_or(Color32::TRANSPARENT, |c| color32(c.map(f64::from)));
    ui.painter().circle_filled(r.center(), 8.0, fill);
    ui.painter().circle_stroke(r.center(), 8.0, Stroke::new(1.0, Color32::from_black_alpha(60)));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tl!("Colour")));
    resp.on_hover_text(tl!("Colour"))
}

/// Show the bar over the selected text markup comment. Returns what the canvas should do.
pub(crate) fn show(
    ctx: &egui::Context,
    viewport: Rect,
    view: &mut DocView,
    info: &DocInfo,
    prefs: &CommentPrefs,
    allowed: bool,
) -> Option<CanvasAction> {
    if view.comments.gesture.is_some() || view.comments.composer.is_some() {
        return None;
    }
    let a = selected_markup(info, view)?;
    let xf = view.page_xform(a.page)?;
    let cx = PageCx { page: a.page, xf: &xf, info, tool: QuickTool::Select, prefs, allowed, hidden: false };
    let bounds = cx.screen_rects(&a).into_iter().reduce(|x, y| x.union(y))?;
    let pos = bar_pos(bounds, viewport)?;
    let (page, index) = (a.page, a.index);
    let mut action = None;
    egui::Area::new(egui::Id::new(("markup-toolbar", view.id.0))).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if icons::button(ui, "message-circle-reply", BUTTON, false, tl!("Reply")).clicked() {
                    view.comments.reveal = true;
                    view.comments.reply_focus = true;
                    action = Some(CanvasAction::OpenComments);
                }
                if allowed {
                    let resp = colour_button(ui, a.color);
                    egui::Popup::menu(&resp).show(|ui| {
                        if let Some(c) = swatch_grid(ui, a.color.map(|c| c.map(f64::from))) {
                            let edit = Edit::StyleAnnotation { page, index, color: Some(c), opacity: None, width: None, endings: None };
                            action = Some(CanvasAction::Edit(Box::new(edit)));
                            ui.close();
                        }
                    });
                }
                let delete = ui.add_enabled_ui(allowed && !a.locked, |ui| icons::button(ui, "trash-2", BUTTON, false, tl!("Delete")));
                if delete.inner.clicked() {
                    view.comments.selected = None;
                    action = Some(CanvasAction::Edit(Box::new(Edit::DeleteAnnotation { page, index })));
                }
            });
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0))
    }

    #[test]
    fn the_bar_sits_above_the_right_end_of_the_marked_text() {
        let text = Rect::from_min_max(pos2(100.0, 200.0), pos2(500.0, 260.0));
        let p = bar_pos(text, viewport()).expect("on screen");
        assert_eq!(p.x, 500.0 - BAR_SIZE.x);
        assert_eq!(p.y, 200.0 - GAP - BAR_SIZE.y);
    }

    #[test]
    fn the_bar_drops_below_text_at_the_top_and_stays_in_the_view() {
        let text = Rect::from_min_max(pos2(-50.0, 10.0), pos2(20.0, 30.0));
        let p = bar_pos(text, viewport()).expect("on screen");
        assert_eq!(p, pos2(INSET, 30.0 + GAP));
        let wide = Rect::from_min_max(pos2(700.0, 300.0), pos2(1200.0, 320.0));
        let p = bar_pos(wide, viewport()).expect("on screen");
        assert_eq!(p.x, 800.0 - BAR_SIZE.x - INSET);
    }

    #[test]
    fn no_bar_for_text_off_screen_or_degenerate_geometry() {
        assert_eq!(bar_pos(Rect::from_min_max(pos2(0.0, 700.0), pos2(50.0, 720.0)), viewport()), None);
        assert_eq!(bar_pos(Rect::from_min_max(pos2(f32::NAN, 0.0), pos2(50.0, 20.0)), viewport()), None);
        // A view narrower than the bar still gives a position, at its left edge.
        let narrow = Rect::from_min_max(pos2(0.0, 0.0), pos2(60.0, 600.0));
        assert_eq!(bar_pos(Rect::from_min_max(pos2(10.0, 300.0), pos2(50.0, 320.0)), narrow).map(|p| p.x), Some(INSET));
    }
}
