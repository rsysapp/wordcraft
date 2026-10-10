//! View mode's window: the document canvas (the view bar arrives with page navigation, zoom and find).

use crate::WordApp;
use crate::theme::Tokens;

pub fn show(app: &mut WordApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        crate::canvas::show(app, ui);
    });
}
