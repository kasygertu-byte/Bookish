# Bookshelf

A small e-book library and reader written in Rust (egui), in the spirit of Calibre's
library + viewer.

## Features

- **Library** grid of book covers with title, author and reading progress (%)
  - **+ Add books** → *Book files…* or *Folder…* (subfolders included), or drag files /
    folders onto the window
  - Search by title or author
  - Recently opened books first
  - Right-click a book: *Open*, *Fetch cover again*, *Remove from library*
    (removing only forgets the book, the file on disk is not touched)
- **Automatic covers** from [Open Library](https://openlibrary.org) (free, no API key):
  when a book is added, its title and author are searched on Open Library and the cover
  is downloaded and stored locally. Downloads run in the background, one at a time
- **Reader** with real pages (no scrolling): the chapter is wrapped to the window width and
  cut into pages that fit the window height
  - Next page: → / ↓ / Page Down / Space, mouse wheel down, click the right side, or ▶
  - Previous page: ← / ↑ / Page Up, mouse wheel up, click the left side, or ◀
  - Chapter list in the top bar, **A− / A+** text size, Esc or **⬅ Library** to go back
- **Remembers where you stopped**: the position is saved on every page turn. It's stored as
  a character position in the chapter, so it stays right when you resize the window or
  change the text size
- Light / dark theme

## Supported formats

| Format | Notes |
|---|---|
| EPUB | Title, author and chapter names from the book's metadata and table of contents. Text only (images and styling are not shown) |
| TXT  | UTF-8. A file named `Author - Title.txt` gets that author and title. Hard-wrapped text (e.g. Project Gutenberg) is re-flowed to the window width. Long files are split into parts |

## Build and run

```bash
cd bookshelf
cargo run --release
```

On Linux, building needs the usual eframe dependencies (e.g. on Debian/Ubuntu:
`libxkbcommon-dev libwayland-dev libx11-dev libgtk-3-dev` for the file dialog).

## Where data is stored

| | Linux | Windows | macOS |
|---|---|---|---|
| Library, settings, positions | `~/.local/share/bookshelf/library.json` | `%APPDATA%\bookshelf\library.json` | `~/Library/Application Support/bookshelf/library.json` |
| Covers | `…/bookshelf/covers/` | same folder | same folder |
