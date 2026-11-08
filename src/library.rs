//! The book library: which books exist, their metadata, covers and reading positions.
//! Stored as JSON in the user's data directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the reader was in a book. The position is stored as a character offset inside a
/// chapter (not a page number), so it stays correct when the window or font size changes
/// the page layout.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub chapter: usize,
    /// Character offset of the first character on the page that was open.
    pub offset: usize,
    /// 0.0 – 1.0, shown on the library card.
    pub percent: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Book {
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    pub author: String,
    /// File name of the cover inside the covers directory, if one was downloaded.
    pub cover: Option<String>,
    /// True once a cover lookup finished without finding a cover, so it isn't retried
    /// automatically on every start (the "Fetch cover" menu item still works).
    #[serde(default)]
    pub cover_not_found: bool,
    #[serde(default)]
    pub progress: Option<Progress>,
    pub added: u64,
    #[serde(default)]
    pub last_opened: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub font_size: f32,
    pub dark_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 20.0,
            dark_mode: true,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Library {
    pub books: Vec<Book>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    next_id: u64,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bookshelf")
}

pub fn covers_dir() -> PathBuf {
    data_dir().join("covers")
}

fn library_file() -> PathBuf {
    data_dir().join("library.json")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Library {
    pub fn load() -> Self {
        std::fs::read_to_string(library_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = data_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("bookshelf: cannot create {}: {e}", dir.display());
            return;
        }
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        // Write to a temp file first so a crash mid-write can't corrupt the library.
        let tmp = dir.join("library.json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, library_file());
        }
    }

    pub fn get(&self, id: u64) -> Option<&Book> {
        self.books.iter().find(|b| b.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Book> {
        self.books.iter_mut().find(|b| b.id == id)
    }

    pub fn contains_path(&self, path: &Path) -> bool {
        self.books.iter().any(|b| b.path == path)
    }

    /// Adds a book and returns its id.
    pub fn add(&mut self, path: PathBuf, title: String, author: String) -> u64 {
        self.next_id = self.next_id.max(self.books.iter().map(|b| b.id + 1).max().unwrap_or(0));
        let id = self.next_id;
        self.next_id += 1;
        self.books.push(Book {
            id,
            path,
            title,
            author,
            cover: None,
            cover_not_found: false,
            progress: None,
            added: now(),
            last_opened: None,
        });
        id
    }

    pub fn remove(&mut self, id: u64) {
        if let Some(book) = self.get(id)
            && let Some(cover) = &book.cover
        {
            let _ = std::fs::remove_file(covers_dir().join(cover));
        }
        self.books.retain(|b| b.id != id);
    }
}
