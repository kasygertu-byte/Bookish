//! Reading book files: metadata for the library, and the text content for the reader.
//! Supported formats: EPUB, plain text (.txt) and PDF (PDFs are shown as rendered pages,
//! see `pdf.rs`; only their metadata is read here).

use std::path::Path;

pub struct Chapter {
    pub title: String,
    pub text: String,
}

pub struct Metadata {
    pub title: String,
    pub author: String,
}

pub fn is_supported(path: &Path) -> bool {
    matches!(extension(path).as_str(), "epub" | "txt" | "pdf")
}

pub fn is_pdf(path: &Path) -> bool {
    extension(path) == "pdf"
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
}

pub fn read_metadata(path: &Path) -> Result<Metadata, String> {
    match extension(path).as_str() {
        "epub" => {
            let doc = epub::doc::EpubDoc::new(path).map_err(|e| e.to_string())?;
            let title = doc
                .get_title()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| file_stem(path));
            let author = doc
                .mdata("creator")
                .map(|m| m.value.clone())
                .unwrap_or_default();
            Ok(Metadata {
                title: title.trim().to_string(),
                author: author.trim().to_string(),
            })
        }
        "pdf" => {
            let (title, author) = crate::pdf::read_info(path)?;
            Ok(Metadata {
                title: if title.is_empty() {
                    file_stem(path)
                } else {
                    title
                },
                author,
            })
        }
        "txt" => {
            // Common naming: "Author - Title.txt".
            let stem = file_stem(path);
            match stem.split_once(" - ") {
                Some((author, title)) => Ok(Metadata {
                    title: title.trim().to_string(),
                    author: author.trim().to_string(),
                }),
                None => Ok(Metadata {
                    title: stem,
                    author: String::new(),
                }),
            }
        }
        other => Err(format!("unsupported file type: .{other}")),
    }
}

pub fn load_chapters(path: &Path) -> Result<Vec<Chapter>, String> {
    let chapters = match extension(path).as_str() {
        "epub" => load_epub(path)?,
        "txt" => load_txt(path)?,
        other => return Err(format!("unsupported file type: .{other}")),
    };
    if chapters.is_empty() {
        return Err("the book has no readable text".into());
    }
    Ok(chapters)
}

fn load_epub(path: &Path) -> Result<Vec<Chapter>, String> {
    let mut doc = epub::doc::EpubDoc::new(path).map_err(|e| e.to_string())?;

    // Table-of-contents labels by resource path, to name the chapters.
    let mut toc_labels = Vec::new();
    fn flatten(points: &[epub::doc::NavPoint], out: &mut Vec<(String, String)>) {
        for p in points {
            let content = p.content.to_string_lossy();
            let file = content.split('#').next().unwrap_or("").to_string();
            out.push((file, p.label.trim().to_string()));
            flatten(&p.children, out);
        }
    }
    flatten(&doc.toc, &mut toc_labels);

    let spine: Vec<String> = doc.spine.iter().map(|s| s.idref.clone()).collect();
    let mut chapters = Vec::new();
    for (i, idref) in spine.iter().enumerate() {
        let Some(resource) = doc.resources.get(idref) else {
            continue;
        };
        let res_path = resource.path.to_string_lossy().into_owned();
        let Some((html, _mime)) = doc.get_resource_str(idref) else {
            continue;
        };
        let text = html_to_text(&html);
        if text.trim().is_empty() {
            continue; // e.g. a cover page that is only an image
        }
        let title = toc_labels
            .iter()
            .find(|(file, _)| !file.is_empty() && res_path.ends_with(file.as_str()))
            .map(|(_, label)| label.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| format!("Section {}", i + 1));
        chapters.push(Chapter { title, text });
    }
    Ok(chapters)
}

/// Converts a chapter's XHTML to plain text: one line per paragraph, blank lines between
/// paragraphs. Wrapping is left to the reader, so the converter's width is very large.
fn html_to_text(html: &str) -> String {
    let raw = html2text::config::plain_no_decorate()
        .string_from_read(html.as_bytes(), 100_000)
        .unwrap_or_default();
    // The converter marks headings with "# ", "## ", …; drop those marks.
    let raw: Vec<&str> = raw
        .lines()
        .map(|line| {
            let hashes = line.len() - line.trim_start_matches('#').len();
            match line[hashes..].strip_prefix(' ') {
                Some(rest) if (1..=6).contains(&hashes) => rest,
                _ => line,
            }
        })
        .collect();
    tidy(&raw.join("\n"))
}

/// Trims trailing spaces and collapses runs of blank lines into one.
fn tidy(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank = 0;
    for line in raw.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

/// Many text files (e.g. Project Gutenberg) break lines at ~70 characters inside paragraphs.
/// If the file looks like that (short lines, paragraphs separated by blank lines), the lines
/// of each paragraph are joined so the reader can wrap them to the window width.
fn unwrap_hard_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() || !text.contains("\n\n") {
        return text.to_string();
    }
    let avg = lines.iter().map(|l| l.chars().count()).sum::<usize>() / lines.len();
    if avg >= 85 {
        return text.to_string();
    }
    text.split("\n\n")
        .map(|para| para.lines().map(str::trim).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Long text files are split into parts of roughly this many bytes (at paragraph breaks),
/// so the reader never has to lay out the whole book at once.
const TXT_PART_SIZE: usize = 60_000;

fn load_txt(path: &Path) -> Result<Vec<Chapter>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let text = unwrap_hard_breaks(&tidy(&text));

    let mut chapters = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let cut = if rest.len() <= TXT_PART_SIZE {
            rest.len()
        } else {
            // Prefer a paragraph break, then a line break, after the target size.
            let mut target = TXT_PART_SIZE;
            while !rest.is_char_boundary(target) {
                target += 1;
            }
            rest[target..]
                .find("\n\n")
                .or_else(|| rest[target..].find('\n'))
                .map(|i| target + i)
                .unwrap_or(rest.len())
        };
        let part = rest[..cut].trim_matches('\n');
        if !part.is_empty() {
            chapters.push(Chapter {
                title: format!("Part {}", chapters.len() + 1),
                text: part.to_string(),
            });
        }
        rest = &rest[cut..];
    }
    Ok(chapters)
}
