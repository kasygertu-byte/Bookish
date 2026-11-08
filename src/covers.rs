//! Downloads book covers from Open Library (https://openlibrary.org), a free service that
//! needs no API key. Lookups run on a background thread; results come back over a channel.
//!
//! 1. Search:  https://openlibrary.org/search.json?title=…&author=…  → `cover_i` (cover id)
//! 2. Image:   https://covers.openlibrary.org/b/id/{cover_i}-L.jpg

use serde::Deserialize;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

const USER_AGENT: &str = "Bookshelf/0.1 (desktop e-book reader)";

pub struct CoverRequest {
    pub book_id: u64,
    pub title: String,
    pub author: String,
}

pub enum CoverResult {
    /// The cover was saved to the covers directory under this file name.
    Found { book_id: u64, file_name: String },
    NotFound { book_id: u64 },
    Failed { book_id: u64, error: String },
}

/// Starts the background worker. Requests are handled one at a time, which keeps the load
/// on Open Library low when many books are added at once.
pub fn spawn_worker(repaint: impl Fn() + Send + 'static) -> (Sender<CoverRequest>, Receiver<CoverResult>) {
    let (req_tx, req_rx) = mpsc::channel::<CoverRequest>();
    let (res_tx, res_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT)
            .build()
            .into();
        for req in req_rx {
            let result = match fetch_cover(&agent, &req) {
                Ok(Some(file_name)) => CoverResult::Found {
                    book_id: req.book_id,
                    file_name,
                },
                Ok(None) => CoverResult::NotFound {
                    book_id: req.book_id,
                },
                Err(error) => CoverResult::Failed {
                    book_id: req.book_id,
                    error,
                },
            };
            if res_tx.send(result).is_err() {
                break;
            }
            repaint();
        }
    });
    (req_tx, res_rx)
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    docs: Vec<SearchDoc>,
}

#[derive(Deserialize)]
struct SearchDoc {
    cover_i: Option<i64>,
}

fn fetch_cover(agent: &ureq::Agent, req: &CoverRequest) -> Result<Option<String>, String> {
    let Some(cover_id) = search_cover_id(agent, &req.title, &req.author)? else {
        return Ok(None);
    };

    let url = format!("https://covers.openlibrary.org/b/id/{cover_id}-L.jpg");
    let bytes = agent
        .get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;

    // Make sure it really is an image before saving it.
    image::load_from_memory(&bytes).map_err(|e| format!("bad cover image: {e}"))?;

    let dir = crate::library::covers_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file_name = format!("{}.jpg", req.book_id);
    std::fs::write(dir.join(&file_name), bytes).map_err(|e| e.to_string())?;
    Ok(Some(file_name))
}

/// Searches by title and author first, then by title only (author names in e-book metadata
/// are often formatted differently, e.g. "Herbert, Frank").
fn search_cover_id(agent: &ureq::Agent, title: &str, author: &str) -> Result<Option<i64>, String> {
    let title = clean_title(title);
    if title.is_empty() {
        return Ok(None);
    }
    let mut attempts = Vec::new();
    if !author.trim().is_empty() {
        attempts.push(Some(author.trim()));
    }
    attempts.push(None);

    for author in attempts {
        let mut request = agent
            .get("https://openlibrary.org/search.json")
            .query("title", &title)
            .query("fields", "cover_i")
            .query("limit", "5");
        if let Some(author) = author {
            request = request.query("author", author);
        }
        let response: SearchResponse = request
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_json()
            .map_err(|e| e.to_string())?;
        if let Some(id) = response.docs.iter().find_map(|d| d.cover_i).filter(|id| *id > 0) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Drops subtitles and bracketed notes ("Dune (Dune Chronicles, Book 1)" → "Dune"), which
/// otherwise often prevent a match.
fn clean_title(title: &str) -> String {
    let mut t = title;
    for sep in [" (", " [", ": "] {
        if let Some(i) = t.find(sep)
            && i > 0
        {
            t = &t[..i];
        }
    }
    t.trim().to_string()
}
