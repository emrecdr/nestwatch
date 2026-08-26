//! How the embedded dashboard assets are delivered: conditional requests and content negotiation.
//!
//! These are HTTP-level rather than unit tests because the property that matters is what reaches a
//! client, and two of the three mechanisms involved (`security.rs`'s header layer, axum's response
//! assembly) sit between `web::serve_asset` and the wire.
//!
//! # Why the negotiation is tested this hard for one encoding
//!
//! The dashboard is not the only client. `../nestwatch-mobile` is `dart:io` all the way down, and
//! `HttpClient.autoUncompress` un-compresses **gzip and nothing else**. A body it cannot decode
//! does not surface as "bad encoding" — it surfaces as a JSON parse error naming nothing to do with
//! compression, which is a genuinely nasty afternoon. Correct negotiation is what makes that
//! impossible rather than unlikely, so it is pinned from both directions: gzip when asked for, and
//! never anything else, however the request is phrased.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

mod common;
use common::{PASSWORD, login, test_app};

/// `GET /app.js` with an explicit `Accept-Encoding`, and optionally an `If-None-Match`.
async fn fetch(
    accept_encoding: Option<&str>,
    if_none_match: Option<&str>,
) -> axum::response::Response {
    let app = test_app();
    let mut b = Request::builder().uri("/app.js");
    if let Some(ae) = accept_encoding {
        b = b.header(header::ACCEPT_ENCODING, ae);
    }
    if let Some(inm) = if_none_match {
        b = b.header(header::IF_NONE_MATCH, inm);
    }
    app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
}

fn header_of(res: &axum::response::Response, name: header::HeaderName) -> Option<String> {
    res.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn bytes_of(res: axum::response::Response) -> Vec<u8> {
    to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn a_client_that_says_nothing_gets_the_asset_uncompressed() {
    let res = fetch(None, None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header_of(&res, header::CONTENT_ENCODING), None);
    let body = bytes_of(res).await;
    assert!(
        body.starts_with(b"//") || body.len() > 1000,
        "an un-negotiated response must be the plain asset"
    );
}

#[tokio::test]
async fn a_gzip_client_gets_gzip_that_actually_decompresses_to_the_asset() {
    let plain = bytes_of(fetch(None, None).await).await;

    let res = fetch(Some("gzip, deflate, br"), None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header_of(&res, header::CONTENT_ENCODING).as_deref(),
        Some("gzip"),
        "gzip was offered and must be used"
    );
    assert_eq!(
        header_of(&res, header::VARY).as_deref(),
        Some("accept-encoding"),
        "a shared cache must not hand this body to a client that did not ask for gzip"
    );

    let compressed = bytes_of(res).await;
    assert!(
        compressed.len() < plain.len(),
        "compressed {} should be smaller than plain {}",
        compressed.len(),
        plain.len()
    );

    // Decode it rather than trusting the header — the point of the exercise is that a real client
    // can read what we sent.
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&compressed[..])
        .read_to_end(&mut out)
        .expect("the body must be valid gzip");
    assert_eq!(out, plain, "gzip must round-trip to exactly the asset");
}

/// The cross-repo hazard, stated as a test: brotli is never sent, even to a client that asks for
/// only brotli. Such a client gets identity, which every HTTP client can read.
#[tokio::test]
async fn brotli_is_never_sent_however_the_request_is_phrased() {
    for accept in ["br", "br, deflate", "*", "identity, br;q=1.0", "zstd, br"] {
        let res = fetch(Some(accept), None).await;
        let encoding = header_of(&res, header::CONTENT_ENCODING);
        assert!(
            encoding.is_none(),
            "Accept-Encoding: {accept:?} produced Content-Encoding: {encoding:?} — \
             only gzip or identity may ever be sent"
        );
    }
}

#[tokio::test]
async fn gzip_refused_with_q0_is_honoured() {
    let res = fetch(Some("gzip;q=0, identity"), None).await;
    assert_eq!(
        header_of(&res, header::CONTENT_ENCODING),
        None,
        "q=0 means not acceptable, and a client that says so is usually a proxy that means it"
    );
}

#[tokio::test]
async fn gzip_is_recognised_regardless_of_spacing_and_case() {
    for accept in ["gzip", "GZIP", " gzip ", "deflate,gzip", "gzip;q=0.9"] {
        let res = fetch(Some(accept), None).await;
        assert_eq!(
            header_of(&res, header::CONTENT_ENCODING).as_deref(),
            Some("gzip"),
            "Accept-Encoding: {accept:?} should have been recognised as offering gzip"
        );
    }
}

/// The change that matters most in practice: a parent opens the dashboard several times a day, and
/// every one of those visits used to re-download the whole UI because `no-store` forbade keeping it.
#[tokio::test]
async fn a_repeat_visit_revalidates_and_transfers_no_body() {
    let first = fetch(None, None).await;
    let etag = header_of(&first, header::ETAG).expect("assets must carry a validator");
    assert!(
        etag.starts_with('"') && etag.ends_with('"'),
        "a strong validator is quoted: {etag}"
    );

    let second = fetch(None, Some(&etag)).await;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_of(&second, header::ETAG).as_deref(),
        Some(etag.as_str())
    );
    assert!(
        bytes_of(second).await.is_empty(),
        "a 304 must carry no body — that is the entire saving"
    );
}

#[tokio::test]
async fn a_stale_validator_gets_the_asset_again() {
    let res = fetch(None, Some("\"0000000000000000\"")).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!bytes_of(res).await.is_empty());
}

/// Proxies may weaken a validator, and browsers send lists. Neither may turn a hit into a miss.
#[tokio::test]
async fn a_weak_or_listed_validator_still_matches() {
    let etag = header_of(&fetch(None, None).await, header::ETAG).unwrap();
    for candidate in [
        format!("W/{etag}"),
        format!("\"other\", {etag}"),
        format!("{etag}, \"other\""),
    ] {
        assert_eq!(
            fetch(None, Some(&candidate)).await.status(),
            StatusCode::NOT_MODIFIED,
            "If-None-Match: {candidate} should have matched"
        );
    }
}

/// Assets opt out of `no-store`; nothing else does. `no-cache` still forbids serving a stale copy,
/// so this buys the bytes without buying a stale dashboard after an upgrade.
#[tokio::test]
async fn assets_revalidate_while_the_api_is_still_never_stored() {
    let asset = fetch(None, None).await;
    assert_eq!(
        header_of(&asset, header::CACHE_CONTROL).as_deref(),
        Some("no-cache"),
        "assets must be storable but always revalidated"
    );

    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login should succeed");
    let api = common::get(&app, "/api/usage/today", Some(&cookie)).await;
    assert_eq!(api.status(), StatusCode::OK);
    assert_eq!(
        header_of(&api, header::CACHE_CONTROL).as_deref(),
        Some("no-store"),
        "the security default must survive: API bodies are never stored"
    );
    assert_eq!(
        header_of(&api, header::ETAG),
        None,
        "and they carry no validator, so there is nothing to revalidate against"
    );
}

/// The child's page and the dashboard shell go through the same helper, so they get the same
/// treatment — worth pinning, because they are the two largest single assets.
#[tokio::test]
async fn the_html_pages_are_negotiated_too() {
    for path in ["/", "/ask"] {
        let app = test_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            header_of(&res, header::CONTENT_ENCODING).as_deref(),
            Some("gzip"),
            "{path} should be compressed for a client that asked"
        );
        assert!(
            header_of(&res, header::ETAG).is_some(),
            "{path} should carry a validator"
        );
    }
}
