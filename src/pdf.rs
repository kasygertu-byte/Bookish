//! PDF support: metadata for the library, and rendering pages to images for the reader.
//! Rendering uses hayro (pure Rust, CPU only) on a background thread, because a page can take
//! a noticeable fraction of a second to draw.

use eframe::egui;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

/// Largest rendered page side in pixels (the renderer's limit is u16; this keeps memory sane).
const MAX_RENDER_SIDE: f32 = 6000.0;

fn open(path: &Path) -> Result<Pdf, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    Pdf::new(Arc::new(data)).map_err(|e| format!("cannot read PDF: {e:?}"))
}

/// Title and author from the PDF's document info (either may be empty).
pub fn read_info(path: &Path) -> Result<(String, String), String> {
    let pdf = open(path)?;
    let meta = pdf.metadata();
    let title = meta.title.as_deref().map(decode_text).unwrap_or_default();
    let author = meta.author.as_deref().map(decode_text).unwrap_or_default();
    // Some tools write placeholders instead of leaving the fields empty.
    let clean = |s: String| {
        let s = s.trim().to_string();
        match s.to_lowercase().as_str() {
            "untitled" | "anonymous" | "unknown" => String::new(),
            _ => s,
        }
    };
    Ok((clean(title), clean(author)))
}

/// Decodes a PDF text string: UTF-16BE with a byte order mark, UTF-8 with a BOM, or
/// PDFDocEncoding (treated as Latin-1, which matches it for nearly all characters).
fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

pub struct RenderRequest {
    pub page: usize,
    /// Width of the rendered image in pixels.
    pub width_px: u32,
}

pub struct RenderedPage {
    pub page: usize,
    pub width_px: u32,
    pub image: egui::ColorImage,
}

/// An open PDF: page sizes, plus a background thread that renders pages on request.
pub struct PdfDocument {
    /// Page sizes in PDF points (rotation already applied).
    pub page_sizes: Vec<(f32, f32)>,
    tx: Sender<RenderRequest>,
    pub rx: Receiver<RenderedPage>,
    /// The page the reader is on; the worker skips queued requests far away from it
    /// (e.g. after flipping through many pages quickly).
    current: Arc<AtomicUsize>,
}

impl PdfDocument {
    pub fn open(path: &Path, ctx: egui::Context) -> Result<Self, String> {
        let pdf = open(path)?;
        let page_sizes: Vec<(f32, f32)> =
            pdf.pages().iter().map(|p| p.render_dimensions()).collect();
        if page_sizes.is_empty() {
            return Err("the PDF has no pages".into());
        }

        let (tx, req_rx) = mpsc::channel::<RenderRequest>();
        let (res_tx, rx) = mpsc::channel();
        let current = Arc::new(AtomicUsize::new(0));
        let worker_current = current.clone();
        std::thread::spawn(move || {
            let cache = RenderCache::new();
            let interpreter = InterpreterSettings::default();
            let settings = RenderSettings::default();
            let pages = pdf.pages();
            for req in req_rx {
                let cur = worker_current.load(Ordering::Relaxed);
                if req.page + 1 < cur || req.page > cur + 2 {
                    continue; // no longer needed
                }
                let Some(page) = pages.get(req.page) else {
                    continue;
                };
                let (w, h) = page.render_dimensions();
                let mut scale = req.width_px as f32 / w.max(1.0);
                scale = scale.min(MAX_RENDER_SIDE / w.max(h).max(1.0)).max(0.05);
                let pixmap = hayro::render(
                    page,
                    &cache,
                    &interpreter,
                    &settings,
                    &PixmapSettings {
                        x_scale: scale,
                        y_scale: scale,
                        bg_color: WHITE,
                    },
                );
                let size = [pixmap.width() as usize, pixmap.height() as usize];
                let image =
                    egui::ColorImage::from_rgba_premultiplied(size, pixmap.data_as_u8_slice());
                let rendered = RenderedPage {
                    page: req.page,
                    width_px: req.width_px,
                    image,
                };
                if res_tx.send(rendered).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });

        Ok(Self {
            page_sizes,
            tx,
            rx,
            current,
        })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn set_current(&self, page: usize) {
        self.current.store(page, Ordering::Relaxed);
    }

    pub fn request(&self, page: usize, width_px: u32) {
        let _ = self.tx.send(RenderRequest { page, width_px });
    }
}
