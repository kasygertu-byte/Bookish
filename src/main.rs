mod app;
mod book;
mod covers;
mod library;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Bookshelf")
            .with_app_id("bookshelf")
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([420.0, 360.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Bookshelf",
        options,
        Box::new(|cc| Ok(Box::new(app::BookshelfApp::new(cc)))),
    )
}
