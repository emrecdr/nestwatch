//! Serves the embedded single-page UI (HTML + built CSS + vendored Alpine.js).
//!
//! Assets in `assets/` are compiled into the binary in release builds via `rust-embed`
//! (in debug builds they're read from disk, so edits show up on refresh). This keeps the
//! shipped artifact a single self-contained `.exe` with no loose files or CDN dependency.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// `GET /` → the app shell.
pub async fn index() -> Response {
    serve_asset("index.html")
}

/// Fallback → serve any other embedded asset by path (e.g. `/app.css`, `/alpine.min.js`).
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        serve_asset("index.html")
    } else {
        serve_asset(path)
    }
}

fn serve_asset(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            ([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}
