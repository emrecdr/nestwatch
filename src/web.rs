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
    /// The served pages contain both HTML files, so both scans below share this list.
    const PAGES: [(&str, &str); 2] = [
        ("index.html", include_str!("../assets/index.html")),
        ("ask.html", include_str!("../assets/ask.html")),
    ];

    /// No `<template>` inside an `<svg>`, on any served page.
    ///
    /// **This one shipped.** The screen-time chart drew its bars with
    /// `<template x-for="(d, i) in screentime.days">` inside its `<svg>`. A `<template>` written
    /// inside `<svg>` is parsed into the SVG namespace, where it is *not* an
    /// `HTMLTemplateElement` and has no `.content` property at all. Alpine's `x-for` reads
    /// `template.content.children`, so it threw `Cannot read properties of undefined` and
    /// rendered **no bars** — thirty days of data drawing an empty chart in 0.2.3, reported
    /// nowhere except the browser console.
    ///
    /// Nothing else in this repository can catch that. It is not a Rust bug, not a type error,
    /// and not visible to any of the 231 tests: it is a DOM namespace rule that only appears when
    /// a browser parses the file. A string scan is a crude instrument, but the defect has a
    /// reliable textual shape, and the alternative is finding it again by eye.
    ///
    /// Deliberately blunt: any `<template>` between an `<svg` and its `</svg>` fails, without
    /// trying to decide whether it is Alpine's. There is no legitimate use of one there.
    ///
    /// Comments are stripped first. The comment explaining this rule, above the chart, naturally
    /// contains the words it forbids — and the first version of this test failed on its own
    /// prose, exactly as the `<th>` scan in `spawn_paths.rs`-style checks has before.
    #[test]
    fn no_alpine_template_inside_svg() {
        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            let html = html.as_str();
            let mut from = 0usize;
            while let Some(open) = html[from..].find("<svg") {
                let open = from + open;
                let close = html[open..]
                    .find("</svg>")
                    .map(|e| open + e)
                    .unwrap_or_else(|| panic!("{name}: <svg at byte {open} is never closed"));
                let inner = &html[open..close];
                assert!(
                    !inner.contains("<template"),
                    "{name}: a <template> inside <svg> is parsed into the SVG namespace, has no \
                     .content, and makes Alpine's x-for render nothing. Build the repeated \
                     elements as HTML, or generate the SVG without x-for.\n  offending <svg> \
                     starts at byte {open}",
                );
                from = close + "</svg>".len();
            }
        }
    }

    /// Replace every `<!-- ... -->` with an equal-length run of spaces.
    ///
    /// Same length so byte offsets in a failure message still point at the real file. An
    /// unterminated comment swallows the rest of the page, which is what a browser does too.
    fn strip_html_comments(html: &str) -> String {
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(start) = rest.find("<!--") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            let end = after.find("-->").map(|e| e + 3).unwrap_or(after.len());
            out.extend(std::iter::repeat_n(' ', end));
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

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
        for (name, html) in PAGES {
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
