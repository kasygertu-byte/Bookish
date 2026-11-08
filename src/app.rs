use crate::book::{self, Chapter};
use crate::covers::{self, CoverRequest, CoverResult};
use crate::library::{self, Library};
use crate::pdf::PdfDocument;
use eframe::egui::{self, Color32, FontId, Sense, TextureHandle, TextureOptions, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

const CARD_W: f32 = 140.0;
const COVER_H: f32 = 200.0;
/// Widest the text column gets; wider lines are hard to read.
const MAX_TEXT_WIDTH: f32 = 760.0;

pub struct BookshelfApp {
    library: Library,
    /// Cover textures that have been loaded, by book id.
    cover_textures: HashMap<u64, TextureHandle>,
    /// Books whose cover file could not be loaded (so it isn't retried every frame).
    cover_load_failed: HashSet<u64>,
    cover_tx: Sender<CoverRequest>,
    cover_rx: Receiver<CoverResult>,
    /// Books with a cover download in progress.
    fetching: HashSet<u64>,
    search: String,
    reader: Option<Reader>,
    pdf_reader: Option<PdfReader>,
    /// Message shown at the bottom of the library (errors, import results).
    status: Option<String>,
}

struct Reader {
    book_id: u64,
    title: String,
    chapters: Vec<Chapter>,
    chapter: usize,
    page: usize,
    pages: Vec<Page>,
    /// What the current `pages` were computed for: (chapter, text width, height, font size).
    layout_key: Option<(usize, u32, u32, u32)>,
    /// Where to land once the current chapter has been paginated.
    target: Target,
}

/// Reader state for a PDF: pages are shown as rendered images.
struct PdfReader {
    book_id: u64,
    title: String,
    doc: PdfDocument,
    page: usize,
    /// Fit the page width to the window (scrolls vertically) instead of the whole page.
    fit_width: bool,
    /// Rendered pages near the current one: (texture, rendered width in pixels).
    textures: HashMap<usize, (TextureHandle, u32)>,
    /// Render requests that haven't come back yet: (page, width in pixels).
    requested: HashSet<(usize, u32)>,
}

#[derive(Clone, Copy)]
struct Page {
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
}

#[derive(Clone, Copy)]
enum Target {
    /// The page containing this character offset.
    Offset(usize),
    /// The last page (when going back from the next chapter).
    End,
    /// Stay on the current page.
    Stay,
}

impl BookshelfApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let library = Library::load();
        let ctx = cc.egui_ctx.clone();
        let (cover_tx, cover_rx) = covers::spawn_worker(move || ctx.request_repaint());
        let mut app = Self {
            library,
            cover_textures: HashMap::new(),
            cover_load_failed: HashSet::new(),
            cover_tx,
            cover_rx,
            fetching: HashSet::new(),
            search: String::new(),
            reader: None,
            pdf_reader: None,
            status: None,
        };
        // Look up covers for books that don't have one yet.
        let missing: Vec<u64> = app
            .library
            .books
            .iter()
            .filter(|b| b.cover.is_none() && !b.cover_not_found)
            .map(|b| b.id)
            .collect();
        for id in missing {
            app.request_cover(id);
        }
        app
    }

    fn request_cover(&mut self, book_id: u64) {
        let Some(book) = self.library.get(book_id) else {
            return;
        };
        if !self.fetching.insert(book_id) {
            return;
        }
        let _ = self.cover_tx.send(CoverRequest {
            book_id,
            title: book.title.clone(),
            author: book.author.clone(),
        });
    }

    fn handle_cover_results(&mut self) {
        let results: Vec<CoverResult> = self.cover_rx.try_iter().collect();
        if results.is_empty() {
            return;
        }
        for result in results {
            match result {
                CoverResult::Found { book_id, file_name } => {
                    self.fetching.remove(&book_id);
                    self.cover_textures.remove(&book_id);
                    self.cover_load_failed.remove(&book_id);
                    if let Some(book) = self.library.get_mut(book_id) {
                        book.cover = Some(file_name);
                        book.cover_not_found = false;
                    }
                }
                CoverResult::NotFound { book_id } => {
                    self.fetching.remove(&book_id);
                    if let Some(book) = self.library.get_mut(book_id) {
                        book.cover_not_found = true;
                        self.status = Some(format!("No cover found for \"{}\".", book.title));
                    }
                }
                CoverResult::Failed { book_id, error } => {
                    self.fetching.remove(&book_id);
                    if let Some(book) = self.library.get(book_id) {
                        self.status =
                            Some(format!("Cover download failed for \"{}\": {error}", book.title));
                    }
                }
            }
        }
        self.library.save();
    }

    /// Adds book files, or every supported book inside folders (subfolders included).
    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        let mut files = Vec::new();
        for path in paths {
            collect_books(&path, &mut files);
        }
        let mut added = 0;
        let mut failed = Vec::new();
        for path in files {
            if self.library.contains_path(&path) {
                continue;
            }
            match book::read_metadata(&path) {
                Ok(meta) => {
                    let id = self.library.add(path, meta.title, meta.author);
                    self.request_cover(id);
                    added += 1;
                }
                Err(e) => failed.push(format!(
                    "{}: {e}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )),
            }
        }
        self.library.save();
        let mut msg = format!("Added {added} book{}.", if added == 1 { "" } else { "s" });
        if !failed.is_empty() {
            msg.push_str(&format!(" Could not read: {}", failed.join("; ")));
        }
        self.status = Some(msg);
    }

    fn open_book(&mut self, ctx: &egui::Context, book_id: u64) {
        let Some(book) = self.library.get(book_id) else {
            return;
        };
        if book::is_pdf(&book.path) {
            match PdfDocument::open(&book.path, ctx.clone()) {
                Ok(doc) => {
                    let page = book
                        .progress
                        .as_ref()
                        .map(|p| p.offset.min(doc.page_count() - 1))
                        .unwrap_or(0);
                    doc.set_current(page);
                    self.pdf_reader = Some(PdfReader {
                        book_id,
                        title: book.title.clone(),
                        doc,
                        page,
                        fit_width: false,
                        textures: HashMap::new(),
                        requested: HashSet::new(),
                    });
                    if let Some(book) = self.library.get_mut(book_id) {
                        book.last_opened = Some(library::now());
                    }
                    self.library.save();
                }
                Err(e) => {
                    self.status = Some(format!("Could not open \"{}\": {e}", book.title));
                }
            }
            return;
        }
        match book::load_chapters(&book.path) {
            Ok(chapters) => {
                let (chapter, offset) = book
                    .progress
                    .as_ref()
                    .map(|p| (p.chapter.min(chapters.len() - 1), p.offset))
                    .unwrap_or((0, 0));
                self.reader = Some(Reader {
                    book_id,
                    title: book.title.clone(),
                    chapters,
                    chapter,
                    page: 0,
                    pages: Vec::new(),
                    layout_key: None,
                    target: Target::Offset(offset),
                });
                if let Some(book) = self.library.get_mut(book_id) {
                    book.last_opened = Some(library::now());
                }
                self.library.save();
            }
            Err(e) => {
                self.status = Some(format!("Could not open \"{}\": {e}", book.title));
            }
        }
    }

    /// Stores the reader's current position in the library and writes it to disk.
    fn save_progress(&mut self) {
        if let Some(pdf) = &self.pdf_reader {
            // For PDFs the position is the page number (stored in `offset`).
            let count = pdf.doc.page_count();
            let progress = library::Progress {
                chapter: 0,
                offset: pdf.page,
                percent: (pdf.page + 1) as f32 / count as f32,
            };
            if let Some(book) = self.library.get_mut(pdf.book_id) {
                book.progress = Some(progress);
            }
            self.library.save();
            return;
        }
        let Some(reader) = &self.reader else {
            return;
        };
        let Some(page) = reader.pages.get(reader.page) else {
            return;
        };
        let total: usize = reader.chapters.iter().map(|c| c.text.len()).sum();
        let before: usize = reader.chapters[..reader.chapter]
            .iter()
            .map(|c| c.text.len())
            .sum();
        let percent = if total == 0 {
            0.0
        } else {
            ((before + page.end_byte) as f32 / total as f32).clamp(0.0, 1.0)
        };
        let progress = library::Progress {
            chapter: reader.chapter,
            offset: page.start_char,
            percent,
        };
        if let Some(book) = self.library.get_mut(reader.book_id) {
            book.progress = Some(progress);
        }
        self.library.save();
    }

    // ---------------------------------------------------------------- library page

    fn library_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Drag & drop books or folders onto the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_paths(dropped);
        }

        egui::Panel::top("library_top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("Bookshelf");
                ui.add_space(12.0);
                ui.menu_button("+ Add books", |ui| {
                    let mut picked = None;
                    if ui.button("Book files…").clicked() {
                        ui.close();
                        picked = rfd::FileDialog::new()
                            .set_title("Add books")
                            .add_filter("E-books", &["epub", "EPUB", "pdf", "PDF", "txt", "TXT"])
                            .pick_files();
                    }
                    if ui.button("Folder…").clicked() {
                        ui.close();
                        picked = rfd::FileDialog::new()
                            .set_title("Add a folder of books")
                            .pick_folder()
                            .map(|dir| vec![dir]);
                    }
                    if let Some(paths) = picked {
                        self.add_paths(paths);
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search title or author")
                        .desired_width(220.0),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.library.settings.dark_mode {
                        "☀ Light"
                    } else {
                        "🌙 Dark"
                    };
                    if ui.button(label).clicked() {
                        self.library.settings.dark_mode = !self.library.settings.dark_mode;
                        self.library.save();
                    }
                    if !self.fetching.is_empty() {
                        ui.spinner();
                        ui.label(format!("Fetching {} cover(s)…", self.fetching.len()));
                    }
                });
            });
            ui.add_space(6.0);
        });

        if let Some(status) = self.status.clone() {
            egui::Panel::bottom("library_status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(status);
                    if ui.small_button("✕").clicked() {
                        self.status = None;
                    }
                });
            });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            if self.library.books.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("No books yet. Click \"+ Add books\" or drop EPUB / PDF / TXT files here.");
                });
                return;
            }

            // Recently opened first, then recently added.
            let query = self.search.to_lowercase();
            let mut ids: Vec<(u64, u64, u64)> = self
                .library
                .books
                .iter()
                .filter(|b| {
                    query.is_empty()
                        || b.title.to_lowercase().contains(&query)
                        || b.author.to_lowercase().contains(&query)
                })
                .map(|b| (b.id, b.last_opened.unwrap_or(0), b.added))
                .collect();
            ids.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));

            let mut action = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    let spacing = 16.0;
                    let per_row =
                        (((ui.available_width() + spacing) / (CARD_W + spacing)).floor() as usize)
                            .max(1);
                    for row in ids.chunks(per_row) {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = spacing;
                            for &(id, _, _) in row {
                                if let Some(a) = self.book_card(ui, id) {
                                    action = Some(a);
                                }
                            }
                        });
                        ui.add_space(spacing);
                    }
                });

            match action {
                Some(CardAction::Open(id)) => self.open_book(&ctx, id),
                Some(CardAction::FetchCover(id)) => self.request_cover(id),
                Some(CardAction::Remove(id)) => {
                    self.cover_textures.remove(&id);
                    self.library.remove(id);
                    self.library.save();
                }
                None => {}
            }
        });
    }

    fn cover_texture(&mut self, ctx: &egui::Context, book_id: u64) -> Option<TextureHandle> {
        if let Some(tex) = self.cover_textures.get(&book_id) {
            return Some(tex.clone());
        }
        if self.cover_load_failed.contains(&book_id) {
            return None;
        }
        let file = self.library.get(book_id)?.cover.clone()?;
        let loaded = image::open(library::covers_dir().join(file)).ok().map(|img| {
            // Keep a 2× thumbnail for sharp display on HiDPI screens.
            let img = img
                .thumbnail((CARD_W * 2.0) as u32, (COVER_H * 2.0) as u32)
                .to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            ctx.load_texture(format!("cover-{book_id}"), color, TextureOptions::LINEAR)
        });
        match loaded {
            Some(tex) => {
                self.cover_textures.insert(book_id, tex.clone());
                Some(tex)
            }
            None => {
                self.cover_load_failed.insert(book_id);
                None
            }
        }
    }

    fn book_card(&mut self, ui: &mut egui::Ui, book_id: u64) -> Option<CardAction> {
        let ctx = ui.ctx().clone();
        let texture = self.cover_texture(&ctx, book_id);
        let fetching = self.fetching.contains(&book_id);
        let book = self.library.get(book_id)?;
        let mut action = None;

        let inner = ui.vertical(|ui| {
            ui.set_width(CARD_W);
            let (rect, cover_resp) =
                ui.allocate_exact_size(Vec2::new(CARD_W, COVER_H), Sense::click());
            let visuals = ui.visuals();
            match &texture {
                Some(tex) => {
                    // Fit the cover inside the slot, keeping its aspect ratio.
                    let size = tex.size_vec2();
                    let scale = (CARD_W / size.x).min(COVER_H / size.y);
                    let img_rect =
                        egui::Rect::from_center_size(rect.center(), size * scale);
                    egui::Image::new(tex)
                        .corner_radius(4.0)
                        .paint_at(ui, img_rect);
                }
                None => {
                    // Placeholder: title on a colored background.
                    ui.painter().rect_filled(rect, 4.0, placeholder_color(book_id));
                    let galley = ui.painter().layout(
                        book.title.clone(),
                        FontId::proportional(15.0),
                        Color32::WHITE,
                        CARD_W - 16.0,
                    );
                    let pos = rect.center() - galley.size() / 2.0;
                    ui.painter().galley(pos, galley, Color32::WHITE);
                    if fetching {
                        let spinner_rect = egui::Rect::from_min_size(
                            rect.right_top() + Vec2::new(-24.0, 6.0),
                            Vec2::splat(16.0),
                        );
                        egui::Spinner::new().color(Color32::WHITE).paint_at(ui, spinner_rect);
                    }
                }
            }
            if cover_resp.hovered() {
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    visuals.selection.stroke,
                    egui::StrokeKind::Outside,
                );
            }

            // Reading progress bar along the bottom of the cover.
            if let Some(p) = &book.progress {
                let bar = egui::Rect::from_min_size(
                    rect.left_bottom() - Vec2::new(0.0, 4.0),
                    Vec2::new(CARD_W, 4.0),
                );
                ui.painter().rect_filled(bar, 0.0, Color32::from_black_alpha(160));
                let mut done = bar;
                done.set_width(CARD_W * p.percent);
                ui.painter().rect_filled(done, 0.0, visuals.selection.bg_fill);
            }

            ui.add(egui::Label::new(egui::RichText::new(&book.title).strong()).truncate());
            let mut sub = book.author.clone();
            if let Some(p) = &book.progress {
                if !sub.is_empty() {
                    sub.push_str(" · ");
                }
                sub.push_str(&format!("{:.0}%", p.percent * 100.0));
            }
            ui.add(egui::Label::new(egui::RichText::new(sub).small().weak()).truncate());
            cover_resp
        });

        let resp = inner.inner.on_hover_text(&book.title);
        if resp.clicked() {
            action = Some(CardAction::Open(book_id));
        }
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() {
                action = Some(CardAction::Open(book_id));
                ui.close();
            }
            if ui.button("Fetch cover again").clicked() {
                action = Some(CardAction::FetchCover(book_id));
                ui.close();
            }
            ui.separator();
            if ui.button("Remove from library").clicked() {
                action = Some(CardAction::Remove(book_id));
                ui.close();
            }
        });
        action
    }

    // ---------------------------------------------------------------- reader page

    fn reader_page(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        let mut font_size = self.library.settings.font_size;
        let Some(reader) = &mut self.reader else {
            return;
        };
        let before = (reader.chapter, reader.page);

        egui::Panel::top("reader_top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("⬅ Library").clicked() {
                    close = true;
                }
                ui.add_space(8.0);
                let current = reader.chapters[reader.chapter].title.clone();
                egui::ComboBox::from_id_salt("chapter")
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for (i, ch) in reader.chapters.iter().enumerate() {
                            if ui
                                .selectable_label(i == reader.chapter, &ch.title)
                                .clicked()
                                && i != reader.chapter
                            {
                                reader.chapter = i;
                                reader.target = Target::Offset(0);
                                reader.layout_key = None;
                            }
                        }
                    });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("A+").on_hover_text("Larger text").clicked() {
                        font_size = (font_size + 2.0).min(48.0);
                    }
                    if ui.button("A−").on_hover_text("Smaller text").clicked() {
                        font_size = (font_size - 2.0).max(10.0);
                    }
                    ui.add(egui::Label::new(egui::RichText::new(&reader.title).strong()).truncate());
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("reader_bottom").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("◀").clicked() {
                    prev_page(reader);
                }
                ui.label(format!(
                    "Page {} of {}  ·  {} ({} of {})",
                    reader.page + 1,
                    reader.pages.len().max(1),
                    reader.chapters[reader.chapter].title,
                    reader.chapter + 1,
                    reader.chapters.len()
                ));
                if ui.button("▶").clicked() {
                    next_page(reader);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let area = ui.available_rect_before_wrap();
            let response = ui.allocate_rect(area, Sense::click());

            match page_turn_input(ui, &response, area, true) {
                PageTurn::Next => next_page(reader),
                PageTurn::Prev => prev_page(reader),
                PageTurn::Close => close = true,
                PageTurn::None => {}
            }

            // Text column, centered, with margins.
            let margin = 24.0;
            let text_w = (area.width() - 2.0 * margin).clamp(100.0, MAX_TEXT_WIDTH);
            let text_h = (area.height() - 2.0 * margin).max(50.0);
            let text_rect = egui::Rect::from_min_size(
                egui::pos2(area.center().x - text_w / 2.0, area.top() + margin),
                Vec2::new(text_w, text_h),
            );
            let font = FontId::proportional(font_size);
            let color = ui.visuals().text_color();

            let key = (
                reader.chapter,
                text_w.round() as u32,
                text_h.round() as u32,
                (font_size * 10.0) as u32,
            );
            if reader.layout_key != Some(key) {
                // Keep the reading position when the layout changes (resize, font size).
                if matches!(reader.target, Target::Stay)
                    && let Some(p) = reader.pages.get(reader.page)
                {
                    reader.target = Target::Offset(p.start_char);
                }
                let text = &reader.chapters[reader.chapter].text;
                reader.pages = paginate(ui.ctx(), text, font.clone(), text_w, text_h);
                reader.layout_key = Some(key);
                reader.page = match reader.target {
                    Target::Offset(offset) => reader
                        .pages
                        .iter()
                        .rposition(|p| p.start_char <= offset)
                        .unwrap_or(0),
                    Target::End => reader.pages.len().saturating_sub(1),
                    Target::Stay => reader.page.min(reader.pages.len().saturating_sub(1)),
                };
                reader.target = Target::Stay;
            }

            if let Some(page) = reader.pages.get(reader.page) {
                let text = &reader.chapters[reader.chapter].text[page.start_byte..page.end_byte];
                let text = text.trim_start_matches('\n').to_string();
                let galley = ui.painter().layout(text, font, color, text_w);
                ui.painter_at(area).galley(text_rect.min, galley, color);
            }
        });

        let after = (reader.chapter, reader.page);
        let font_changed = font_size != self.library.settings.font_size;
        self.library.settings.font_size = font_size;
        // Only save once the new chapter is paginated, so the right page is stored.
        if (after != before || font_changed) && reader.layout_key.is_some() {
            self.save_progress();
        }
        if close {
            self.save_progress();
            self.reader = None;
        }
    }

    // ---------------------------------------------------------------- PDF reader page

    fn pdf_reader_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut close = false;
        let Some(r) = &mut self.pdf_reader else {
            return;
        };
        let before = r.page;
        let count = r.doc.page_count();

        // Pick up pages the background thread has finished rendering.
        let rendered: Vec<_> = r.doc.rx.try_iter().collect();
        for done in rendered {
            r.requested.remove(&(done.page, done.width_px));
            let tex = ctx.load_texture(
                format!("pdf-page-{}", done.page),
                done.image,
                TextureOptions::LINEAR,
            );
            r.textures.insert(done.page, (tex, done.width_px));
        }

        egui::Panel::top("pdf_top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("⬅ Library").clicked() {
                    close = true;
                }
                ui.add_space(8.0);
                let label = if r.fit_width { "Fit page" } else { "Fit width" };
                if ui.button(label).clicked() {
                    r.fit_width = !r.fit_width;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&r.title).strong()).truncate());
                });
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("pdf_bottom").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("◀").clicked() && r.page > 0 {
                    r.page -= 1;
                }
                ui.label("Page");
                let mut shown = r.page + 1;
                ui.add(egui::DragValue::new(&mut shown).range(1..=count));
                r.page = shown.clamp(1, count) - 1;
                ui.label(format!("of {count}"));
                if ui.button("▶").clicked() && r.page + 1 < count {
                    r.page += 1;
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let area = ui.available_rect_before_wrap();
            let margin = 12.0;
            let (pw, ph) = r.doc.page_sizes[r.page];
            let avail_w = (area.width() - 2.0 * margin).max(50.0);
            let avail_h = (area.height() - 2.0 * margin).max(50.0);
            let scale = if r.fit_width {
                avail_w / pw.max(1.0)
            } else {
                (avail_w / pw.max(1.0)).min(avail_h / ph.max(1.0))
            };
            let size = Vec2::new(pw * scale, ph * scale);

            // Render at screen resolution, rounded so small resizes don't re-render.
            let ppp = ctx.pixels_per_point();
            let want_px = (((size.x * ppp) / 64.0).ceil() as u32).max(1) * 64;
            r.doc.set_current(r.page);
            for p in [r.page, r.page + 1, r.page.wrapping_sub(1)] {
                if p >= count {
                    continue;
                }
                let up_to_date = r.textures.get(&p).is_some_and(|(_, w)| *w == want_px);
                if !up_to_date && r.requested.insert((p, want_px)) {
                    r.doc.request(p, want_px);
                }
            }
            // Free textures of pages far from the current one.
            let page = r.page;
            r.textures.retain(|&p, _| p + 2 >= page && p <= page + 2);

            let paint = |ui: &mut egui::Ui, rect: egui::Rect| match r.textures.get(&page) {
                Some((tex, _)) => {
                    egui::Image::new(tex).paint_at(ui, rect);
                }
                None => {
                    ui.painter().rect_filled(rect, 0.0, Color32::WHITE);
                    let spinner = egui::Rect::from_center_size(rect.center(), Vec2::splat(32.0));
                    egui::Spinner::new()
                        .color(Color32::GRAY)
                        .paint_at(ui, spinner);
                }
            };

            let turn = if r.fit_width {
                // Tall page: scroll vertically; the wheel scrolls instead of turning pages.
                let mut turn = PageTurn::None;
                egui::ScrollArea::vertical()
                    .id_salt(("pdf_scroll", page))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(margin);
                        let (row, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), size.y),
                            Sense::click(),
                        );
                        let rect = egui::Rect::from_center_size(row.center(), size);
                        paint(ui, rect);
                        ui.add_space(margin);
                        turn = page_turn_input(ui, &response, area, false);
                    });
                turn
            } else {
                let response = ui.allocate_rect(area, Sense::click());
                let rect = egui::Rect::from_center_size(area.center(), size);
                paint(ui, rect);
                page_turn_input(ui, &response, area, true)
            };
            match turn {
                PageTurn::Next if r.page + 1 < count => r.page += 1,
                PageTurn::Prev if r.page > 0 => r.page -= 1,
                PageTurn::Close => close = true,
                _ => {}
            }
        });

        if r.page != before {
            self.save_progress();
        }
        if close {
            self.save_progress();
            self.pdf_reader = None;
        }
    }
}

enum CardAction {
    Open(u64),
    FetchCover(u64),
    Remove(u64),
}

enum PageTurn {
    None,
    Next,
    Prev,
    Close,
}

/// Page-turn input shared by both readers: keys, mouse wheel (if `wheel`) and clicks on the
/// right / left part of `area`.
fn page_turn_input(
    ui: &egui::Ui,
    response: &egui::Response,
    area: egui::Rect,
    wheel: bool,
) -> PageTurn {
    let (next, prev, escape, scroll) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowRight)
                || i.key_pressed(egui::Key::PageDown)
                || i.key_pressed(egui::Key::Space)
                || i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowLeft)
                || i.key_pressed(egui::Key::PageUp)
                || i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::Escape),
            // Raw wheel events (not smoothed), so one notch turns one page.
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                    _ => None,
                })
                .sum::<f32>(),
        )
    });
    let hovered = wheel && response.hovered();
    if escape {
        return PageTurn::Close;
    }
    if next || (hovered && scroll < 0.0) {
        return PageTurn::Next;
    }
    if prev || (hovered && scroll > 0.0) {
        return PageTurn::Prev;
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        if pos.x > area.left() + area.width() * 0.6 {
            return PageTurn::Next;
        } else if pos.x < area.left() + area.width() * 0.4 {
            return PageTurn::Prev;
        }
    }
    PageTurn::None
}

fn next_page(reader: &mut Reader) {
    if reader.page + 1 < reader.pages.len() {
        reader.page += 1;
    } else if reader.chapter + 1 < reader.chapters.len() {
        reader.chapter += 1;
        reader.target = Target::Offset(0);
        reader.layout_key = None;
    }
}

fn prev_page(reader: &mut Reader) {
    if reader.page > 0 {
        reader.page -= 1;
    } else if reader.chapter > 0 {
        reader.chapter -= 1;
        reader.target = Target::End;
        reader.layout_key = None;
    }
}

/// Splits a chapter into pages: lays the whole chapter out at the page width, then cuts the
/// wrapped rows into groups that fit the page height.
fn paginate(ctx: &egui::Context, text: &str, font: FontId, width: f32, height: f32) -> Vec<Page> {
    let galley = ctx.fonts_mut(|f| f.layout(text.to_owned(), font, Color32::WHITE, width));

    // Page boundaries as character offsets.
    let mut starts = vec![0usize];
    let mut page_top = 0.0;
    let mut chars = 0usize;
    for row in &galley.rows {
        let rect = row.rect();
        let page_start = *starts.last().unwrap();
        if rect.max.y - page_top > height && chars > page_start {
            starts.push(chars);
            page_top = rect.min.y;
        }
        chars += row.char_count_including_newline().0;
    }

    // Convert character offsets to byte offsets for slicing (one pass over the text).
    let mut byte_starts = Vec::with_capacity(starts.len());
    let mut wanted = starts.iter().copied().peekable();
    for (char_i, (byte_i, _)) in text.char_indices().enumerate() {
        while wanted.next_if(|&c| c <= char_i).is_some() {
            byte_starts.push(byte_i);
        }
    }
    byte_starts.resize(starts.len(), text.len());

    let mut pages = Vec::with_capacity(starts.len());
    for (i, &start_char) in starts.iter().enumerate() {
        pages.push(Page {
            start_byte: byte_starts[i],
            end_byte: byte_starts.get(i + 1).copied().unwrap_or(text.len()),
            start_char,
        });
    }
    pages
}

fn collect_books(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            collect_books(&entry, out);
        }
    } else if book::is_supported(path) {
        out.push(path.to_path_buf());
    }
}

fn placeholder_color(id: u64) -> Color32 {
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(0x4a, 0x5d, 0x7e),
        Color32::from_rgb(0x7e, 0x4a, 0x5d),
        Color32::from_rgb(0x4a, 0x7e, 0x64),
        Color32::from_rgb(0x7e, 0x6a, 0x4a),
        Color32::from_rgb(0x5d, 0x4a, 0x7e),
        Color32::from_rgb(0x4a, 0x72, 0x7e),
    ];
    COLORS[(id % COLORS.len() as u64) as usize]
}

impl eframe::App for BookshelfApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_theme(if self.library.settings.dark_mode {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        self.handle_cover_results();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.pdf_reader.is_some() {
            self.pdf_reader_page(ui);
        } else if self.reader.is_some() {
            self.reader_page(ui);
        } else {
            self.library_page(ui);
        }
    }

    fn on_exit(&mut self) {
        self.save_progress();
        self.library.save();
    }
}
