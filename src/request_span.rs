//! One `tracing` span per request, so a log line can be tied to the request that produced it.
//!
//! `O99`: the service runs unattended for months, the audit log is deliberately a *security*
//! record, and nothing else said what the process was doing when something went wrong — a parent
//! reporting a fault had nothing to send. `tracing` was already a dependency; what was missing was
//! a span, and a decision about what may be recorded in it.
//!
//! **What is recorded, decided rather than defaulted.** A per-process counter, the HTTP method,
//! and the **route template** — `/api/providers/{name}`, never `/api/providers/studygo`. A concrete
//! path names an integration; a query string or a body can carry a page title or a message a
//! parent typed, and none of that belongs in a log that is not the audit log. The template is what
//! axum matched, so it carries no value a person supplied. The completion event adds the status
//! and the milliseconds, at `debug`, so the default `info` log stays quiet and the span does its
//! work the other way round: a `warn` written anywhere inside a handler now arrives with the
//! request that caused it.
//!
//! A `middleware::from_fn` rather than `tower-http`'s `TraceLayer`, because the layer would be a new
//! direct dependency on a project that gates its supply chain weekly, and this is fifteen lines.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// Numbers requests within this process, from one. A restart starts over, which is fine: a line is
/// read beside the service's own start line, and the number only has to be unique within one run.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// The outermost layer in `server::build_router`, so every other middleware's warning — a peer
/// refused by `require_lan_peer`, an origin refused by `require_same_origin` — lands inside the
/// span as well.
pub async fn span_each_request(
    matched: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Response {
    let req = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    // The template axum matched, or one fixed word for a path that matched nothing — never the
    // request's own path, which is the one string here a person may have chosen.
    let route = matched.as_ref().map_or("(unmatched)", MatchedPath::as_str);
    // `%` for both, so the line reads `method=GET route=/api/providers/{name}` rather than the
    // quoted Debug form; the grep a parent or a test does for `route=/api/...` then works.
    let span = tracing::info_span!("request", req, method = %request.method(), route = %route);
    let started = std::time::Instant::now();
    async move {
        let response = next.run(request).await;
        tracing::debug!(
            status = response.status().as_u16(),
            ms = started.elapsed().as_millis() as u64,
            "handled"
        );
        response
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::{Router, middleware};
    use tower::ServiceExt;

    /// Everything the subscriber wrote, shared with the test that reads it.
    #[derive(Clone, Default)]
    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn captured(sink: &Sink) -> String {
        String::from_utf8_lossy(&sink.0.lock().unwrap()).into_owned()
    }

    /// The production shape in miniature: a nested `/api` router under the span layer, exactly
    /// where `server::build_router` puts it, and a handler that warns — the case the span exists
    /// for.
    fn app() -> Router {
        let api = Router::new().route(
            "/providers/{name}",
            get(|| async {
                tracing::warn!("something a handler says");
                StatusCode::OK
            }),
        );
        Router::new()
            .nest("/api", api)
            .layer(middleware::from_fn(span_each_request))
    }

    /// The decision about what may be recorded, measured on the written log rather than argued
    /// from the field list: the template, never the name or the query.
    #[tokio::test]
    async fn a_request_is_logged_by_its_route_template_and_never_by_what_it_carried() {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer({
                let sink = sink.clone();
                move || sink.clone()
            })
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/api/providers/studygo?note=finished%20my%20homework")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "the span layer must not change the answer"
        );

        let log = captured(&sink);
        assert!(
            log.contains("route=/api/providers/{name}"),
            "the route template, with the nest prefix, is what the span records: {log}"
        );
        assert!(log.contains("method=GET") && log.contains("req="), "{log}");
        assert!(
            log.contains("something a handler says"),
            "the handler's own warning is captured at all: {log}"
        );
        // The warning sits inside the request span, so its line carries the request's fields.
        let warn_line = log
            .lines()
            .find(|l| l.contains("something a handler says"))
            .expect("the warning was written");
        assert!(
            warn_line.contains("req=") && warn_line.contains("route="),
            "a handler's warning must arrive with the request that caused it: {warn_line}"
        );
        assert!(
            log.contains("status=200"),
            "the completion event names the status: {log}"
        );
        for forbidden in ["studygo", "homework", "note="] {
            assert!(
                !log.contains(forbidden),
                "`{forbidden}` is something a person supplied and must not reach the log: {log}"
            );
        }
    }

    /// Two requests, two ids — the counter is what makes a line attributable to one request.
    #[tokio::test]
    async fn every_request_gets_its_own_number() {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer({
                let sink = sink.clone();
                move || sink.clone()
            })
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        for _ in 0..2 {
            app()
                .oneshot(
                    Request::builder()
                        .uri("/api/providers/x")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let ids: std::collections::BTreeSet<&str> = captured(&sink)
            .lines()
            .filter(|l| l.contains("status="))
            .filter_map(|l| l.split("req=").nth(1))
            .map(|rest| {
                rest.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|s| Box::leak(s.to_string().into_boxed_str()) as &str)
            .collect();
        assert_eq!(ids.len(), 2, "two requests must carry two different ids");
    }
}
