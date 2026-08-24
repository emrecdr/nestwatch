//! Serves the embedded single-page UI (HTML + built CSS + vendored Alpine.js).
//!
//! Assets in `assets/` are compiled into the binary in release builds via `rust-embed`
//! (in debug builds they're read from disk, so edits show up on refresh). This keeps the
//! shipped artifact a single self-contained `.exe` with no loose files or CDN dependency.

use std::borrow::Cow;

use axum::body::Bytes;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// `GET /` → the app shell.
pub async fn index() -> Response {
    serve_asset("index.html")
}

/// `GET /ask` → the child's "request more time" page (unauthenticated, LAN-gated).
pub async fn ask() -> Response {
    serve_asset("ask.html")
}

/// Fallback → serve any other embedded asset by path (e.g. `/app.css`, `/alpine.min.js`).
/// `/` is handled by [`index`], so this never sees an empty path.
pub async fn static_handler(uri: Uri) -> Response {
    serve_asset(uri.path().trim_start_matches('/'))
}

fn serve_asset(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            // In release builds `data` borrows a `&'static [u8]`, so serve it zero-copy;
            // in debug (assets read from disk) it's owned. Avoid the per-request copy of
            // `into_owned()` on the hot page-load path.
            let body = match file.data {
                Cow::Borrowed(bytes) => Bytes::from_static(bytes),
                Cow::Owned(bytes) => Bytes::from(bytes),
            };
            ([(header::CONTENT_TYPE, mime)], body).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    /// Every column header carries `scope`, on both served pages.
    ///
    /// A `<th>` without `scope` leaves the header-to-cell association to the screen reader's
    /// guesswork. It guesses well on a simple grid and badly on anything else, and the dashboard's
    /// tables are the part a parent reads for facts — which app burned the time, when a request
    /// came in, what the audit trail says. Six tables here had none; this keeps the seventh from
    /// shipping the same way, which is the actual regression mode (a new panel, copied from an
    /// old one).
    ///
    /// Scans the markup rather than asserting a count, so adding a table is caught rather than
    /// merely changing a number nobody updates. Attribute-presence based, so it is indifferent to
    /// line endings — a Windows checkout rewrites these files to CRLF, which has already broken
    /// one source-scanning test in this repo (`tests/spawn_paths.rs`).
    #[test]
    fn every_table_header_says_which_column_it_heads() {
        for (name, html) in [
            ("index.html", include_str!("../assets/index.html")),
            ("ask.html", include_str!("../assets/ask.html")),
        ] {
            let mut rest = html;
            let mut seen = 0usize;
            while let Some(at) = rest.find("<th") {
                rest = &rest[at..];
                let end = rest
                    .find('>')
                    .unwrap_or_else(|| panic!("{name}: unterminated <th"));
                let tag = &rest[..end];
                // `<thead>` shares the prefix. A cell header is `<th>` or `<th ...>`, so the
                // character after the name decides — without this the scan demands `scope` on
                // every `<thead>` and fails on correct markup.
                if !matches!(tag.as_bytes().get(3), None | Some(b' ') | Some(b'\t')) {
                    rest = &rest[end..];
                    continue;
                }
                assert!(
                    tag.contains("scope="),
                    "{name}: this <th> does not say which column it heads — add \
                     scope=\"col\":\n  {tag}>",
                );
                seen += 1;
                rest = &rest[end..];
            }
            // `ask.html` is the child's page and has no tables; only assert coverage where the
            // scan found something, so this cannot silently pass by matching nothing at all.
            if name == "index.html" {
                assert!(
                    seen > 0,
                    "{name}: found no <th> at all — did the scan break?"
                );
            }
        }
    }
}
