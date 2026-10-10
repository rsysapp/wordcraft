//! View mode's window: the document canvas (the view bar arrives with page navigation, zoom and find).

use crate::WordApp;
use crate::theme::Tokens;

pub fn show(app: &mut WordApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        crate::canvas::show(app, ui);
    });
}

/// The page that fills most of the viewport (`pages` and `viewport` in the scroll area's content space).
pub fn visible_page(pages: &[egui::Rect], viewport: egui::Rect) -> Option<usize> {
    // f32::min and max skip a NaN operand, so a non-finite rect would otherwise count as fully visible.
    if !viewport.is_finite() {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for (i, page) in pages.iter().enumerate().filter(|(_, page)| page.is_finite()) {
        let overlap = (page.max.y.min(viewport.max.y) - page.min.y.max(viewport.min.y)).max(0.0);
        if overlap > 0.0 && best.is_none_or(|(_, b)| overlap > b) {
            best = Some((i, overlap));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2};

    use super::visible_page;

    #[test]
    fn the_page_with_the_most_visible_height_wins() {
        let pages = [Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 100.0)), Rect::from_min_max(pos2(0.0, 110.0), pos2(10.0, 210.0))];
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 80.0))), Some(0));
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 90.0), pos2(10.0, 170.0))), Some(1));
        assert_eq!(visible_page(&pages, Rect::from_min_max(pos2(0.0, 300.0), pos2(10.0, 400.0))), None);
        assert_eq!(visible_page(&[], Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 80.0))), None);
    }

    #[test]
    fn non_finite_geometry_picks_no_page_or_a_real_one() {
        let nan = Rect::from_min_max(pos2(f32::NAN, f32::NAN), pos2(f32::NAN, f32::NAN));
        let page = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 100.0));
        assert_eq!(visible_page(&[nan], page), None);
        assert_eq!(visible_page(&[nan, page], Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 50.0))), Some(1));
        assert_eq!(visible_page(&[page], nan), None);
    }
}
