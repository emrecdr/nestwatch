//! Serves the embedded single-page UI (HTML + built CSS + vendored Alpine.js).
//!
//! Assets in `assets/` are compiled into the binary in release builds via `rust-embed`
//! (in debug builds they're read from disk, so edits show up on refresh). This keeps the
//! shipped artifact a single self-contained `.exe` with no loose files or CDN dependency.

use std::borrow::Cow;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// `GET /` → the app shell.
pub async fn index(headers: HeaderMap) -> Response {
    serve_asset("index.html", &headers)
}

/// The path the child's "request more time" page is served at.
///
/// Named once because two places now have to agree on it: the route registration in `server.rs`
/// and [`ask_url`], which prints it. A literal in each would be the shape this codebase keeps
/// finding — nothing empty, nothing erroring, one of them simply no longer true.
pub const ASK_PATH: &str = "/ask";

/// `GET /ask` → the child's "request more time" page (unauthenticated, LAN-gated).
pub async fn ask(headers: HeaderMap) -> Response {
    serve_asset("ask.html", &headers)
}

/// The URL a child opens to ask for more minutes, for the callers that have to *print* it.
///
/// **`localhost`, and it is the only address that is always right for this reader.** The child is
/// sitting at the machine, so this needs no DHCP lease, no name resolution, and no knowledge of
/// what the PC is called — the three things that broke the address `install` used to print here.
/// A lease change on a rebooted router is not hypothetical; it is what sent a parent looking for
/// an IP that had already moved. The LAN IP and hostname `install` prints solve a different
/// problem, which is reaching the dashboard from the *parent's* phone.
///
/// It resolves for free, it is a SAN on the certificate `cert::generate` writes, and
/// `security::is_lan` admits loopback — so the page opens rather than warning or refusing.
///
/// **The v6 stumble is real and was measured rather than argued.** `server.rs` binds `0.0.0.0`,
/// which is IPv4 only, while resolvers hand back `::1` first for `localhost`. The v6 attempt is
/// therefore refused — but a refusal is an immediate RST, not a hang, so a Happy Eyeballs client
/// (RFC 8305: every current browser) has already connected on `127.0.0.1`. Measured against a
/// socket bound exactly as `server.rs` binds it: 0.3 ms. Worth knowing before anyone "fixes" this
/// to `127.0.0.1`, which would connect but fail the certificate — the cert carries `localhost` as
/// a DNS SAN and no loopback IP SAN.
///
/// The port is a parameter because `install --port N` moves it, and a wrong port here is worse
/// than printing no address at all.
pub fn ask_url(port: u16) -> String {
    format!("https://localhost:{port}{ASK_PATH}")
}

/// Fallback → serve any other embedded asset by path (e.g. `/app.css`, `/alpine.min.js`).
/// `/` is handled by [`index`], so this never sees an empty path.
pub async fn static_handler(uri: Uri, headers: HeaderMap) -> Response {
    serve_asset(uri.path().trim_start_matches('/'), &headers)
}

/// Whether the client asked for gzip, per RFC 9110's `Accept-Encoding` grammar.
///
/// Deliberately narrow: **gzip or nothing.** brotli compresses this page set about 15% smaller
/// again, which on a LAN is a few milliseconds nobody can feel, and it would cost a new dependency
/// and — the reason that actually decided it — break the Android client. `dart:io`'s
/// `HttpClient.autoUncompress` un-compresses `gzip` and nothing else, so a `br` body would reach it
/// as undecodable bytes and surface as a JSON parse error naming nothing to do with compression.
/// Negotiating properly means that client simply never asks for what it cannot read.
///
/// `q=0` means "not acceptable" and is honoured, because a client that goes to the trouble of
/// saying so is the one most likely to be a proxy that cannot handle it.
fn wants_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|part| {
        let mut bits = part.split(';').map(str::trim);
        let token = bits.next().unwrap_or_default();
        if !token.eq_ignore_ascii_case("gzip") {
            return false;
        }
        // Any explicit `q=0` (or `q=0.000`) disqualifies it; anything else accepts.
        !bits.any(|p| {
            p.strip_prefix("q=")
                .and_then(|q| q.parse::<f32>().ok())
                .is_some_and(|q| q == 0.0)
        })
    })
}

/// A strong validator for `bytes` — the first 16 hex digits of its SHA-256, quoted.
///
/// Content-derived rather than mtime-derived on purpose: in debug builds `rust-embed` re-reads
/// `assets/` from disk on every request, so an editor that rewrites a file without changing it
/// would otherwise invalidate the cache and make the dev loop look broken in a way that is hard to
/// attribute. Hashing the bytes means the tag changes exactly when the bytes do.
fn etag_for(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut tag = String::with_capacity(20);
    tag.push('"');
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(tag, "{byte:02x}");
    }
    tag.push('"');
    tag
}

/// Serve one embedded asset, with conditional requests and gzip negotiation.
///
/// # Why this stopped being three lines
///
/// `security.rs` stamps `Cache-Control: no-store` on every response, and its reasoning — that the
/// most sensitive bytes this service produces are captures of a child's desktop — is right for
/// `/api/*` and wrong here, for a reason that was asserted rather than measured: *"every page is
/// embedded in the binary and served over a LAN, so there is no round trip worth saving."* The
/// round trip is not what was being paid. `no-store` forbids **storing**, so a parent's phone
/// re-downloaded the entire UI on every visit.
///
/// Measured through this router — `/`, `/app.js`, `/app.css`, `/alpine.min.js`, which is what one
/// cold dashboard load actually fetches:
///
/// | | bytes |
/// |---|---|
/// | before | 328,080 |
/// | after, first visit (gzip) | 85,226 — **3.85×**, 242,854 saved |
/// | after, repeat visit (304) | **0 body bytes**, four small revalidations |
///
/// The asset sizes drift as the UI changes, so the ratio is the durable number, not the totals.
///
/// Two changes, in the order they matter. **Conditional requests** turn the repeat visit — which is
/// the normal case, since a parent opens this several times a day — into four small revalidations
/// answered `304` with no body at all. **gzip** cuts what remains by 3.9× on the genuinely-first
/// load, and after every upgrade, when all four validators change at once.
///
/// `no-cache` rather than a long `max-age`: it permits storing but requires revalidation before
/// use, so nothing is ever served stale, `assets/app.css` stays safe to edit in a debug build, and
/// an upgraded binary cannot be shadowed by a cached asset from the previous version. The bytes
/// saved are the same; only the freshness guarantee differs, and this end of it costs nothing.
fn serve_asset(path: &str, headers: &HeaderMap) -> Response {
    let Some(file) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let mime = file.metadata.mimetype().to_string();
    // In release builds `data` borrows a `&'static [u8]`, so serve it zero-copy;
    // in debug (assets read from disk) it's owned. Avoid the per-request copy of
    // `into_owned()` on the hot page-load path.
    let body = match file.data {
        Cow::Borrowed(bytes) => Bytes::from_static(bytes),
        Cow::Owned(bytes) => Bytes::from(bytes),
    };

    let etag = etag_for(&body);
    // `Vary` even on the 304: a shared cache that stored the gzip body must not hand it to a
    // client that did not ask for gzip.
    let common = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, "no-cache".to_string()),
        (header::VARY, header::ACCEPT_ENCODING.to_string()),
    ];

    // `If-None-Match` is a list, and a proxy may append `W/` weak forms. Comparing membership
    // rather than equality keeps this correct without parsing the grammar in full.
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag)
        });
    if fresh {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }

    if wants_gzip(headers) {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        if encoder.write_all(&body).is_ok()
            && let Ok(compressed) = encoder.finish()
        {
            return (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CONTENT_ENCODING, "gzip".into()),
                ],
                common,
                Bytes::from(compressed),
            )
                .into_response();
        }
        // Compression is an optimisation; a failure serves the asset uncompressed rather than
        // failing the page. Falling through is deliberate.
    }

    ([(header::CONTENT_TYPE, mime)], common, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::Assets;

    /// The served script, for the guards that can only learn a fact by reading it. Checked in and
    /// not generated, unlike `assets/app.css`, so `include_str!` is safe here — a fresh clone has
    /// this file before it has run anything.
    const APP_JS: &str = include_str!("../assets/app.js");

    /// The served pages. Every scan below iterates this, so it is the list that decides what is
    /// guarded at all — which is why [`every_served_page_is_reached_by_the_scans`] ties it to what
    /// is actually served rather than to what someone remembered.
    const PAGES: [(&str, &str); 2] = [
        ("index.html", include_str!("../assets/index.html")),
        ("ask.html", include_str!("../assets/ask.html")),
    ];

    /// Every page the binary will actually serve is in [`PAGES`].
    ///
    /// `Assets` embeds the whole `assets/` folder and [`static_handler`](super::static_handler)
    /// serves any of it by path, so dropping `settings.html` in there publishes it with no code
    /// change anywhere. Every scan in this module iterates `PAGES` — no inline `<script>`, CSP
    /// parseability, form labels, table headers, class coverage — so a served page missing from
    /// that list is checked by **none** of them, and nothing goes red. The list is what makes the
    /// other guards mean anything, and it was the one part kept by hand.
    ///
    /// Both directions on purpose. A page served but unlisted is the dangerous one; a listed page
    /// that no longer exists means a scan is passing over nothing and reporting success.
    #[test]
    fn every_served_page_is_reached_by_the_scans() {
        let mut served: Vec<String> = Assets::iter()
            .filter(|f| f.ends_with(".html"))
            .map(|f| f.to_string())
            .collect();
        served.sort();

        let mut scanned: Vec<String> = PAGES.iter().map(|(name, _)| name.to_string()).collect();
        scanned.sort();

        assert_eq!(
            served, scanned,
            "`assets/` serves the first list and `PAGES` carries the second. Every scan in this \
             module iterates `PAGES`, so a page that is served but unlisted is guarded by none of \
             them while the suite stays green. Add it to `PAGES` with its `include_str!`."
        );
    }

    /// The address printed to the child resolves without the network, and its route exists.
    ///
    /// Two silent failures, both of which have a precedent in this file's history. Renaming the
    /// route leaves `install` printing a 404 at a child who was just told to go there, and no
    /// compiler can see across a string literal in `server.rs` and one here. And "fixing" the host
    /// to the LAN IP or the machine name would reintroduce the exact lease-change failure this
    /// replaced — the IP was what this line printed until now, and it stopped resolving the moment
    /// a rebooted router handed out a new lease.
    #[test]
    fn the_child_link_is_loopback_and_its_route_exists() {
        const SERVER_RS: &str = include_str!("server.rs");

        // Non-vacuity: an empty or bare-slash path would make the `contains` below match any
        // route at all, and the scan would pass while proving nothing.
        assert!(
            super::ASK_PATH.len() > 1 && super::ASK_PATH.starts_with('/'),
            "ASK_PATH must be a real path; the route scan below is vacuous otherwise, got {:?}",
            super::ASK_PATH
        );
        let registration = format!(".route(\"{}\"", super::ASK_PATH);
        assert!(
            SERVER_RS.contains(&registration),
            "`server.rs` registers no route at {:?}, so `ask_url` prints a 404 to a child who was \
             told to open it. Looked for {:?}.",
            super::ASK_PATH,
            registration
        );

        assert_eq!(super::ask_url(8443), "https://localhost:8443/ask");
        // A non-default port, because `install --port N` moves it and a hardcoded 8443 here would
        // pass the line above while sending the child to a port nothing is listening on.
        assert_eq!(super::ask_url(9001), "https://localhost:9001/ask");
    }

    /// Every authenticated endpoint is reachable from the only interface that can authenticate.
    ///
    /// # The state this exists to prevent
    ///
    /// `GET /api/export` and `POST /api/language` were each implemented, tested, documented in
    /// `server.rs`'s route map — and referenced nowhere in `app.js` or `index.html`. A parent had
    /// no way to reach either. That is worse than not having built them: the cost is paid, the
    /// risk surface exists, and the value is not collected. Nothing failed, because nothing was
    /// looking.
    ///
    /// `POST /api/re-anchor` was the sharper version of the same thing. `doctor`'s failure text
    /// told the parent to "sign in to the dashboard and use `Re-anchor the clock`" — naming a
    /// control that did not exist. A diagnostic whose fix instruction is fiction is worse than one
    /// that says nothing, and no test could catch it while the two files were checked separately.
    ///
    /// Matches on the path stem, so a route with a `{param}` is satisfied by the client building
    /// the URL around it. That is deliberately loose: this asks "does anything reference this
    /// endpoint at all", which is the question. Whether it is wired to the *right* control is what
    /// review is for.
    #[test]
    fn every_authenticated_route_is_reachable_from_the_dashboard() {
        const SERVER_RS: &str = include_str!("server.rs");
        let router_src = SERVER_RS
            .split_once("#[cfg(test)]")
            .map_or(SERVER_RS, |(before, _)| before);
        let guarded = router_src
            .split_once("route_layer(middleware::from_fn(auth::require_auth))")
            .expect("the /api router must apply require_auth")
            .0
            .split_once("let api = Router::new()")
            .expect("the guarded router must still be built here")
            .1;

        let ui = format!(
            "{}{}",
            include_str!("../assets/app.js"),
            include_str!("../assets/index.html")
        );

        let mut unreachable = Vec::new();
        let mut seen = 0;
        for (i, m) in guarded.match_indices(".route(\"") {
            let rest = &guarded[i + m.len()..];
            let path = &rest[..rest.find('"').expect("unterminated route path")];
            seen += 1;
            // The stem before any `{param}` — the client builds the rest.
            let stem = path.split('{').next().unwrap_or(path).trim_end_matches('/');
            if !ui.contains(&format!("/api{stem}")) {
                unreachable.push(path);
            }
        }

        assert!(
            seen > 10,
            "only found {seen} guarded routes — the split above has probably gone stale"
        );
        assert!(
            unreachable.is_empty(),
            "these endpoints exist, are authenticated, and nothing in the dashboard references \
             them, so a parent cannot reach them: {unreachable:?}"
        );
    }

    /// Every progress bar derives its urgency colour from a named helper, never a literal.
    ///
    /// Three bars, two decisions: `budgetTone` (three states, keyed on minutes *remaining*, with an
    /// amber band matching the child's own 15-minute warning) and `limitTone` (binary, keyed on
    /// used-vs-limit, no amber because a per-app limit has no warning to match). Before this, the
    /// budget decision was written inline once and about to be twice, and the limit decision was
    /// written out byte-identically in two places.
    ///
    /// The drift this prevents is not cosmetic. The summary strip and the Today card show the same
    /// budget; a strip in amber above a bar in green forces the parent to work out which to
    /// believe, which is worse than either being wrong on its own.
    #[test]
    fn no_progress_bar_hard_codes_its_own_urgency_colour() {
        let markup = strip_html_comments(include_str!("../assets/index.html"));
        for tone in ["progress-error", "progress-warning"] {
            assert!(
                !markup.contains(tone),
                "`{tone}` is written into the markup. Urgency is a decision, and it lives in \
                 `budgetTone`/`limitTone` in app.js so two bars showing the same fact cannot \
                 disagree — build the class from one of those instead."
            );
        }
        // …and the helpers are actually reached, so the assertion above cannot be satisfied by a
        // page that simply has no progress bars left.
        assert!(
            markup.contains("budgetTone(") && markup.contains("limitTone("),
            "both tone helpers must still be used by the markup"
        );
    }

    /// The child's page keeps its accessible names and atomic live regions.
    ///
    /// # Why this one page and not both
    ///
    /// Scoped deliberately, and the scope is the finding. `index.html` would fail both halves today
    /// — three forms with no accessible name, and five `aria-live` regions of which three are
    /// atomic — so a page-agnostic guard could only exist with a standing exemption for the larger
    /// page, which is the shape this project already dislikes.
    ///
    /// The asymmetry is also real rather than convenient. A parent who cannot use the dashboard has
    /// a phone, a laptop and the `doctor` command; the child has this page and nothing else, and it
    /// is the surface that tells them how much time they have and carries the disclosure about what
    /// is being watched. If only one page is held to this, it is this one.
    ///
    /// `aria-atomic` matters here specifically because every one of these regions is rewritten
    /// wholesale — "23 minutes left today" becomes "22 minutes left today" — and without it a
    /// screen reader may announce only the changed fragment, which is the number stripped of what
    /// it counts.
    #[test]
    fn the_childs_page_keeps_its_accessible_names_and_atomic_live_regions() {
        let page = strip_html_comments(include_str!("../assets/ask.html"));

        for (at, _) in page.match_indices("<form") {
            let tag_end = page[at..].find('>').expect("unterminated <form>") + at;
            let tag = &page[at..tag_end];
            assert!(
                tag.contains("aria-labelledby=") || tag.contains("aria-label="),
                "a form on the child's page has no accessible name, so a screen reader announces \
                 only \"form\": {tag}"
            );
        }

        let live = page.matches("aria-live=").count();
        let atomic = page.matches("aria-atomic=").count();
        assert!(
            live > 0,
            "the time-remaining region must stay a live region — it is the whole point of the page"
        );
        assert_eq!(
            live, atomic,
            "every live region here is rewritten whole, so each needs aria-atomic; found {live} \
             live and {atomic} atomic"
        );
    }

    /// No inline `<script>` on any served page.
    ///
    /// `security::CSP` dropped `'unsafe-inline'` from `script-src` once both pages moved their
    /// JavaScript into `assets/app.js` and `assets/ask.js`. That directive and this shape are one
    /// decision in two files: add an inline `<script>` back and the browser silently refuses to
    /// run it. Silently is the problem — there is no error on the page, just a dashboard that
    /// does nothing, which is the same symptom as the chart bug this suite already carries a test
    /// for.
    ///
    /// Matches `<script` not followed by a `src`, so `<script defer src="/app.js">` passes and a
    /// bare `<script>` does not.
    #[test]
    fn no_inline_script_on_any_served_page() {
        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            for (at, _) in html.match_indices("<script") {
                let tag_end = html[at..]
                    .find('>')
                    .map(|e| at + e)
                    .unwrap_or_else(|| panic!("{name}: unterminated <script at byte {at}"));
                let tag = &html[at..tag_end];
                assert!(
                    tag.contains("src="),
                    "{name}: inline <script> at byte {at}. script-src no longer admits \
                     'unsafe-inline', so the browser will refuse to run this and the page will \
                     fail with no visible error. Put the code in assets/*.js and load it with \
                     src=.",
                );
            }
        }
    }

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

    /// Every minutes limit a person is shown matches the one its own endpoint enforces.
    ///
    /// There are **two** limits here and they are not the same fact. `timecode::MAX_CODE_MINUTES`
    /// bounds the code a parent issues; `timereq::MAX_REQUEST_MINUTES` bounds the extra time a
    /// child asks for and the bonus a parent grants. Both are 240 today, which is exactly why this
    /// needs a table rather than one number: the first version of this test asserted every surface
    /// against `MAX_CODE_MINUTES` and passed — including for the child's input, which that constant
    /// does not govern. Raising one alone would have demanded the other move with it.
    ///
    /// The surfaces that restate these are `max=` attributes, none of them near the enforcement.
    /// Nothing connected any of them and nothing pinned any of them, so raising a constant left the
    /// server accepting a value every box still refused — including on `/ask`, where the person
    /// told the wrong limit is the child, who cannot ask why the number is wrong.
    ///
    /// It guarded two toast messages as well until `api::require_minutes` and the active-code cap
    /// learned to send their bound with the refusal. `app.js` now prints what the server said
    /// rather than a number copied from it, so there is no second spelling left to pin. That is
    /// the better shape of the same fix: the limit travels *with* the error instead of being held
    /// against it from outside, and a pinning loop is only ever the second-best way to stop two
    /// copies drifting.
    ///
    /// A source scan because the property lives in markup and in strings, which no Rust type
    /// reaches — the sanctioned case in `OPEN-FINDINGS.md` O54. **Every `type="number"` input on
    /// either page is selected**, and an unrecognised one is a panic rather than a skip, so a new
    /// number box cannot pass unchecked.
    ///
    /// That selector was `min="1"`, which reached two inputs of the eight. It was chosen when the
    /// other six were unbounded, and it made their unboundedness invisible to the one test placed
    /// to notice: four of them accepted any number at all while `Rules::validate` rejected
    /// anything over `MAX_BUDGET_MINS`, so a parent typing 99,999 got a 400 the box had just told
    /// them was fine. Selecting on `min="1"` also meant *removing* a `max` would drop an input out
    /// of the scan silently, which is the same failure as deleting a golden file to make a test
    /// pass. Keying on `type="number"` closes both: an input cannot leave this test's attention by
    /// losing an attribute, only by ceasing to be a number input.
    #[test]
    fn every_minutes_limit_a_person_sees_matches_the_one_the_server_enforces() {
        use crate::curfew::MAX_WARN_SECS;
        use crate::rules::MAX_BUDGET_MINS;
        use crate::timecode::MAX_CODE_MINUTES;
        use crate::timereq::MAX_REQUEST_MINUTES;

        // Marker → the constant whose value that box is restating. Keyed on `x-model` rather than
        // position, so reordering the form cannot silently repoint a row at a different field.
        let inputs = [
            (
                "index.html",
                "x-model.number=\"newCodeMins\"",
                MAX_CODE_MINUTES,
                "the code a parent issues",
            ),
            (
                "index.html",
                "x-model.number=\"curfew.warn_secs\"",
                MAX_WARN_SECS,
                "the curfew's warning",
            ),
            (
                "index.html",
                "x-model.number=\"rules.warn_secs\"",
                MAX_WARN_SECS,
                "the budget's warning",
            ),
            (
                "index.html",
                "x-model.number=\"rules.daily_budget_mins\"",
                MAX_BUDGET_MINS,
                "the daily limit",
            ),
            (
                "index.html",
                "x-model.number=\"rules.budget_by_weekday[i]\"",
                MAX_BUDGET_MINS,
                "a per-weekday limit",
            ),
            (
                "index.html",
                "x-model.number=\"row.mins\"",
                MAX_BUDGET_MINS,
                "a per-app limit",
            ),
            (
                "index.html",
                "x-model.number=\"g.limit_mins\"",
                MAX_BUDGET_MINS,
                "an app-group limit",
            ),
            (
                "ask.html",
                "id=\"minutes\"",
                MAX_REQUEST_MINUTES,
                "the extra time a child asks for",
            ),
        ];

        let mut seen = 0;
        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            for tag in html.split("<input").skip(1) {
                let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
                if !tag.contains("type=\"number\"") {
                    continue;
                }
                seen += 1;
                let Some(&(_, _, limit, what)) = inputs
                    .iter()
                    .find(|(p, marker, _, _)| *p == name && tag.contains(marker))
                else {
                    panic!(
                        "{name} carries a `type=\"number\"` input this test does not recognise, so \
                         nothing is checking it against a server limit. Add it to the table with \
                         the constant its endpoint enforces — and if it has no server limit, that \
                         is the thing to fix, not this test:\n{tag}"
                    );
                };
                assert!(
                    tag.contains(&format!("max=\"{limit}\"")),
                    "{name}'s input for {what} does not cap at {limit}, which is what its endpoint \
                     enforces. A box that accepts more than the server does turns a valid-looking \
                     entry into a 400 the person cannot explain:\n{tag}"
                );
            }
        }
        assert_eq!(
            seen,
            inputs.len(),
            "expected {} minutes inputs across the served pages, found {seen}",
            inputs.len()
        );
    }

    /// The lockout a parent is told to wait out matches the one actually enforced.
    ///
    /// `app.js` says "wait a minute". That sentence cannot interpolate a constant — it is prose in
    /// a static file — so what is pinned here is the *pairing*, and the value of this test is
    /// entirely in its failure message: it names the sentence that has to move with the constant.
    /// A bare `assert_eq!` of a constant against its own literal would pin nothing; this one exists
    /// because the other side of the comparison is English.
    ///
    /// The consequence of the drift is small and nasty. Raise the lockout to five minutes and a
    /// locked-out parent is told to wait one, comes back early, fails again — and they are doing
    /// this while trying to look at their child's screen, which is when they are least willing to
    /// believe the tool rather than their own retry.
    #[test]
    fn the_lockout_a_parent_is_told_to_wait_matches_the_one_enforced() {
        // Built FROM the constant, never compared beside it.
        //
        // This was two assertions — `LOGIN_LOCKOUT == 60s`, and `app.js` contains "wait a minute"
        // — sharing one failure message that said to change them together. Nothing made that true.
        // Raising the lockout to five minutes failed the first, and the obvious repair is to edit
        // the `60` on the line that failed; the second never fires, because the sentence still
        // exists. Verified: with `LOGIN_LOCKOUT` at 300s and the literal updated to match, this
        // test PASSED while a locked-out parent was told to wait a minute for a five-minute
        // lockout. A guard whose own repair silences it is worse than no guard, because the next
        // person reads the green and stops looking.
        //
        // Deriving the sentence leaves only one thing to satisfy: the phrase has to be IN `app.js`,
        // and the only way to produce it is to write the wait the constant actually names.
        let secs = crate::auth::LOGIN_LOCKOUT.as_secs();
        let wait = match secs {
            60 => "a minute".to_string(),
            s if s % 60 == 0 => format!("{} minutes", s / 60),
            s => format!("{s} seconds"),
        };
        let sentence = format!("wait {wait} and try again");
        assert!(
            APP_JS.contains(&sentence),
            "`LOGIN_LOCKOUT` is {secs}s, so a locked-out parent must be told to \"{sentence}\", \
             and no sentence in `assets/app.js` reads that. The lockout and the prose describing \
             it have drifted — and the parent is reading this while trying to see their child's \
             screen, which is when they are least willing to believe the tool over their own retry."
        );
    }

    /// The minutes at which the dashboard turns the budget amber match the first warning the child
    /// gets on their own desktop.
    ///
    /// `assets/app.js` declares `BUDGET_LOW_MINS = 15` with a comment saying it "matches the first
    /// warning the child gets" — and nothing made that true. It is a hand-copied second statement
    /// of `countdown::WARN_AT_MINS[0]`, in another language, in another file, with no mechanical
    /// link; the Rust side already derives `LOOKAHEAD_MINS` from that same element rather than
    /// repeating it.
    ///
    /// Built FROM the constant, for the reason spelled out in
    /// [`the_lockout_a_parent_is_told_to_wait_matches_the_one_enforced`] above: an `assert_eq!` of
    /// 15 against 15 pins nothing, because the natural repair when it fails is to edit whichever
    /// literal the failure named. Deriving the source line leaves one way to satisfy this — write
    /// the number the constant actually holds.
    ///
    /// The drift is quiet and it points the wrong way. Move the child's first warning to 30 and the
    /// child is told time is running out while the parent's dashboard still reads calm green; move
    /// it to 5 and the dashboard cries amber for ten minutes during which nothing is happening on
    /// the child's screen. Either way the two people looking at the same budget are shown different
    /// urgency, and the one who can act on it is the one being misinformed.
    #[test]
    fn the_budget_the_dashboard_calls_low_is_the_childs_first_warning() {
        let declaration = format!(
            "const BUDGET_LOW_MINS = {};",
            crate::countdown::WARN_AT_MINS[0]
        );
        assert!(
            APP_JS.contains(&declaration),
            "the child's first countdown warning fires at {} minutes, so `assets/app.js` must \
             declare `{declaration}` and it does not. The dashboard's amber threshold and the \
             child's first warning have drifted, so a parent and their child are being shown \
             different urgency about the same remaining budget.",
            crate::countdown::WARN_AT_MINS[0]
        );

        // And that the declaration is what the colour actually reads.
        //
        // Pinning only the `const` line leaves the same hole this test was written to close: the
        // threshold could be spelled as a literal at the use site, or the amber branch deleted
        // outright, and the assertion above stays green over a constant nothing consults. A
        // declaration guarded in isolation is a decoy — it reads as pinned precisely because
        // someone took the trouble to pin it.
        assert!(
            APP_JS.contains("remaining <= BUDGET_LOW_MINS"),
            "`BUDGET_LOW_MINS` is declared in `assets/app.js` but the budget tone no longer \
             compares against it, so the constant this test pins is not the number the dashboard \
             uses. Either restore the comparison or delete the constant — do not leave a guarded \
             declaration that nothing reads."
        );
    }

    /// Every member that exists only to be displayed is actually displayed.
    ///
    /// Two families, because the component has two: a `get show…()` decides whether something
    /// appears, and a `…Note()` produces the sentence that appears. Both exist for the markup and
    /// for nothing else, so either one going unreferenced is a decision computed and thrown away.
    /// It fails silently in the worst direction — the state it was written to distinguish reaches
    /// the reader as the same blank space as the state it was distinguishing it *from*.
    ///
    /// That is not hypothetical here. `first_seen` carried three states from `screentime.rs`,
    /// typed, serialized, argued for in a doc comment saying "the UI must distinguish that", and
    /// pinned by tests in two languages — and the card rendered two of them, for as long as the
    /// feature existed. `firstSeenNote()` computed a sentence for the quiet day that nothing ever
    /// displayed.
    ///
    /// **The note half is why this covers both.** Scanning only the getters left the exact shape of
    /// the original bug uncovered: deleting `x-text="firstSeenQuietNote()"` from the markup leaves
    /// `showFirstSeenQuiet` still referenced by its `x-if`, so the getter scan stays green while
    /// the quiet day renders as an empty `<p>` — the bug reintroduced through the gap in its own
    /// guard. Verified by doing it: 17 of 17 passed.
    ///
    /// Getters are matched with the quotes around the name so `showFirstSeen` cannot be satisfied
    /// by `showFirstSeenQuiet` happening to contain it; notes are matched with their parentheses,
    /// which separates them for the same reason.
    #[test]
    fn every_display_only_member_reaches_the_markup() {
        let html = strip_html_comments(index_html());

        let shown: Vec<String> = APP_JS
            .split("get show")
            .skip(1)
            .filter_map(|rest| rest.split_once("()"))
            .map(|(name, _)| format!("show{name}"))
            .collect();

        let notes: Vec<String> = APP_JS
            .lines()
            .filter_map(|line| line.trim().strip_suffix("Note() {"))
            .map(|stem| format!("{stem}Note"))
            .collect();

        assert!(
            shown.len() >= 3 && notes.len() >= 3,
            "expected the first-seen getters and notes at least; found {} getters and {} notes, so \
             this scan has stopped matching how they are declared",
            shown.len(),
            notes.len()
        );

        for name in &shown {
            assert!(
                html.contains(&format!("\"{name}\"")),
                "`{name}` decides whether something is shown and nothing in index.html asks it, so \
                 whatever state it distinguishes renders as the same nothing as every other state"
            );
        }
        for name in &notes {
            assert!(
                html.contains(&format!("{name}()")),
                "`{name}` builds a sentence for the markup and nothing in index.html renders it, \
                 so the state it describes reaches the reader as blank space"
            );
        }
    }

    fn index_html() -> &'static str {
        PAGES
            .iter()
            .find(|(name, _)| *name == "index.html")
            .expect("index.html must be one of the served pages")
            .1
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

    /// Every Alpine directive on a page, as `(attribute, value)`.
    ///
    /// `x-`, `@` and `:` attributes only — those are the ones Alpine evaluates. A plain `class` or
    /// `href` is inert text as far as the expression parser is concerned.
    ///
    /// Both quote styles are read, for the reason `static_class_attrs` states below: the failure
    /// this feeds — a CSP-build violation like a `[...spread]` — renders nothing and raises no
    /// error, so a directive the scanner skips is covered by nothing at all. Matching only `="`
    /// would go on "working" in silence the day someone writes `x-text='…'`.
    fn alpine_directives(html: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < html.len() {
            let Some(eq) = html[i..].find('=') else { break };
            let at = i + eq;
            let open = at + 1;
            // Quoted values only. An unquoted `disabled=true` carries no expression, and an `=`
            // inside a value is already skipped past by the cursor jump at the end of the loop.
            let Some(q @ ('"' | '\'')) = html[open..].chars().next() else {
                i = open;
                continue;
            };
            let value = open + q.len_utf8();
            let Some(end) = html[value..].find(q) else {
                break;
            };
            let end = value + end;
            // Walk back over the attribute name.
            let start = html[..at]
                .rfind(|c: char| c.is_whitespace())
                .map_or(0, |p| p + 1);
            let name = &html[start..at];
            if name.starts_with("x-") || name.starts_with('@') || name.starts_with(':') {
                out.push((name.to_string(), html[value..end].to_string()));
            }
            i = end + q.len_utf8();
        }
        out
    }

    /// What the directive scan does and does not pick up, pinned.
    ///
    /// Same reasoning as `the_class_scan_reads_static_attributes_and_only_those` below: this
    /// helper decides what the CSP guard can see, so a gap in it makes that guard pass while
    /// checking less than it claims. Without this test, narrowing the scan back to double quotes
    /// only breaks nothing and fails nothing — the directives simply stop being examined.
    #[test]
    fn the_directive_scan_reads_both_quote_styles_and_only_alpine_attributes() {
        let html = concat!(
            r#"<div x-text="a" :class='b' @click="c" x-bind:value='d'"#,
            r#" class="plain" href='/x' disabled=true data-x="e">"#,
        );
        let found = alpine_directives(html);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();

        assert!(names.contains(&"x-text"), "double-quoted x- attribute");
        assert!(
            names.contains(&":class"),
            "single-quoted `:class` must be read — it holds an expression the CSP build has to              be able to parse, and quoting it differently must not hide it"
        );
        assert!(names.contains(&"@click"), "double-quoted @ attribute");
        assert!(names.contains(&"x-bind:value"), "single-quoted x-bind");

        assert!(
            !names.contains(&"class"),
            "a plain class attribute is inert text"
        );
        assert!(
            !names.contains(&"href"),
            "so is href, in either quote style"
        );
        assert!(!names.contains(&"data-x"), "and so is a data- attribute");

        let value = |n: &str| {
            found
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
                .unwrap_or_default()
        };
        assert_eq!(
            value("x-text"),
            "a",
            "the value stops at its own closing quote"
        );
        assert_eq!(value(":class"), "b");
        assert_eq!(value("x-bind:value"), "d");
    }

    /// The stylesheet that ships, so the scan below compares markup against what a browser will
    /// actually receive — not against what Tailwind *could* generate from a fresh build.
    ///
    /// Read at run time rather than with `include_str!`, because `assets/app.css` is **generated
    /// and gitignored**. Baking it in with `include_str!` makes it a compile-time requirement, so a
    /// fresh clone that has not run `npm run build` fails to compile with a bare "couldn't read"
    /// pointing at a file that is not in the repository — replacing the actionable warning
    /// `build.rs` prints for exactly this case, and contradicting its promise that the app still
    /// builds without it. Reading here fails only this test, and says what to run.
    fn app_css() -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.css");
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "cannot read {path}: {e}\nThis file is generated and gitignored — run \
                 `cd web && npm install && npm run build` first. (Failing rather than skipping: a \
                 guard that quietly passes when its input is missing is worse than no guard.)"
            )
        })
    }

    /// Every class a `*Class` method builds at run time must have a rule in the **shipped**
    /// stylesheet.
    ///
    /// These are the classes no other guard can see. A `*Class` method returns them as strings, so
    /// they appear in no `class=` attribute, and `the_class_scan_reads_static_attributes_and_only_those`
    /// pins that scanner to static attributes deliberately, because reading Alpine expressions
    /// yields `===` and `null` as class names. So the markup guard cannot cover them by construction.
    ///
    /// The JavaScript tests assert the *strings* these methods return, and would all still pass if
    /// the rules vanished from the CSS. What would ship is an element carrying a class with nothing
    /// behind it: an over-budget day rendered identically to an ordinary one, a live timeline span
    /// with no ring, with no error anywhere. That is the same silent failure the markup guard
    /// exists to prevent, arriving through the one door it does not watch.
    ///
    /// **This scan is derived rather than listed, and that is the point.** It replaced a hard-coded
    /// `[".st-nodata", ".st-over"]`, whose doc claimed those were "the only classes in the product
    /// that no other guard can see". They were not. Three more builders had been added in the same
    /// diff — `spanClass`, `glanceClass`, `shotAgeClass` — and five of their names (`bg-primary`,
    /// `bg-error`, `bg-success`, `ring-inset`, `ring-base-content`) occur in **no** static `class=`
    /// attribute on either page, so nothing covered them at all. A list that must be extended by
    /// hand records the day it was written; this one reads the code.
    ///
    /// Note the two kinds are protected differently, and only one of them by this test.
    /// `.st-nodata` / `.st-over` are plain CSS in `web/src/app.css`, emitted unconditionally. The
    /// rest are Tailwind utilities, emitted **only** because the literal string appears in
    /// `web/.scan/app.js` for `@source` to find. So a name built as `"st-" + kind` rather than
    /// written as a literal keeps its rule; a utility built the same way loses one silently. This
    /// test catches a rule going missing. Nothing catches it being reachable only by luck, which is
    /// what the plain-CSS choice for the two `.st-*` names is for.
    #[test]
    fn every_class_a_runtime_builder_returns_survives_into_the_shipped_stylesheet() {
        let css = unescape_css(&app_css());
        let built = runtime_built_classes(APP_JS);

        // A floor, not a count: the failure this guards against is the scan silently finding
        // nothing — a renamed convention, a reformat — at which point the loop below passes
        // vacuously and this guard reports coverage it no longer has.
        assert!(
            built.len() >= 12,
            "only {} runtime-built classes found in assets/app.js, expected at least a dozen.              The scan has stopped finding `*Class` methods — check the convention it relies on              (a method named `…Class` at four-space indent, returning string literals) before              lowering this floor.",
            built.len()
        );

        let missing: Vec<&str> = built
            .iter()
            .map(String::as_str)
            .filter(|c| !css_has_rule(&css, c))
            .collect();

        assert!(
            missing.is_empty(),
            "built at run time by a `*Class` method in assets/app.js, but with no rule in the \
             shipped CSS: {missing:?}\n\nNo markup scan and no JS test can catch this — the \
             element renders unstyled and nothing fails.\nLikely causes, in order: the rule was \
             deleted from `web/src/app.css`; or a Tailwind utility stopped appearing as a literal \
             in a scanned file, so it is no longer emitted; or `npm run build` has not run since \
             the class was added."
        );
    }

    /// Every class name a `*Class` method in `app.js` can return.
    ///
    /// Derived from the source text because that is the only place these names exist — by the time
    /// they reach a browser they are the result of a branch nothing static can follow.
    fn runtime_built_classes(js: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut inside = false;

        for line in js.lines() {
            let body = line.trim_start();
            let indent = line.len() - body.len();

            if !inside {
                // `    someNameClass(` — a method at object-literal indent whose name says what it
                // returns. `stBarKey` and friends are getters over these, not builders themselves.
                if indent == 4
                    && let Some(open) = body.find('(')
                    && body[..open].ends_with("Class")
                    && body[..open]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_')
                {
                    inside = true;
                }
                continue;
            }

            if indent == 4 && (body == "}," || body == "}") {
                inside = false;
                continue;
            }

            // Every double-quoted literal on a `return` line, split into individual class names —
            // one `return` can carry several (`"bg-error st-over"`) and a ternary carries two.
            if let Some(r) = body.find("return ") {
                let mut rest = &body[r + "return ".len()..];
                while let Some(a) = rest.find('"') {
                    let after = &rest[a + 1..];
                    let Some(b) = after.find('"') else { break };
                    for tok in after[..b].split_whitespace() {
                        if !out.iter().any(|c| c == tok) {
                            out.push(tok.to_string());
                        }
                    }
                    rest = &after[b + 1..];
                }
            }
        }

        out
    }

    /// Every list of page titles must carry the game-portal label, not just the one on the Today
    /// card.
    ///
    /// The same data is rendered twice — today's card and the per-day rows of the screen-time
    /// report — and the label was added to only the first. A portal would then appear to stop being
    /// one the moment a parent looked at a past day, which is worse than never labelling it: an
    /// inconsistent signal teaches the reader to distrust the signal.
    ///
    /// Counted rather than merely present, because "at least one badge exists" is satisfied by the
    /// state this test was written to catch. A JS test cannot see it either — `gamePortal` would
    /// keep passing its own tests while nothing in the markup called it for the second list.
    #[test]
    fn every_page_title_list_carries_the_game_portal_label() {
        // Stripped first, like every other counting scan here. `labelled` counts a literal
        // directive, and the house style puts an explanatory comment directly above BOTH badge
        // templates — so a comment that quoted the directive would inflate the count and let the
        // test pass with a badge deleted from the report list, which is the exact state it exists
        // to catch. Verified by putting that comment in and watching it fail.
        let html = strip_html_comments(index_html());
        let lists =
            html.matches("in today.pages").count() + html.matches("in stRows('pages')").count();
        let labelled = html.matches("x-if=\"gamePortal(").count();
        assert!(lists > 0, "the markup must render page titles somewhere");
        assert_eq!(
            labelled, lists,
            "{lists} list(s) of page titles but {labelled} carrying a game-portal label — every \
             list showing page titles must label portals the same way, or the same site reads as a \
             portal on one card and not on another."
        );
    }

    /// The chart's key must be rendered from `stBarKey`, not written out in the markup.
    ///
    /// It *was* written out in the markup, and that is exactly how it broke: the three swatch
    /// classes were spelled out as literals in `index.html`, so adding the `.st-over` texture
    /// restyled the bars and left the key behind. Two of the three swatches then disagreed with the
    /// chart they explained — and disagreed in the one channel the texture exists for, since a
    /// reader who cannot separate `bg-primary` from `bg-error` (measured at 1.22 contrast) was
    /// handed a key encoded in precisely the pair they cannot read.
    ///
    /// `web/test/app.test.js` pins `stBarKey`'s entries against `stBarClass`, but a JS test cannot
    /// notice the *markup* reverting to literals — the getter would keep agreeing with itself while
    /// nothing rendered it. That is what this scan is for.
    ///
    /// The labels are the tell: they live in `app.js` beside the classes they are paired with, so
    /// finding one in the markup means the key was re-inlined. "not measured" is deliberately not
    /// checked — it legitimately appears in the chart's own `aria-label`, which is a different
    /// sentence for a different reader.
    #[test]
    fn the_chart_key_is_rendered_from_the_bar_classes_not_written_into_the_markup() {
        let html = strip_html_comments(index_html());
        // The bindings that actually hold the property. Asserting these is the positive half:
        // without them nothing renders the key at all, and `stBarKey` merely appearing somewhere in
        // the file (in a comment, say) would satisfy a bare name check.
        for binding in ["x-for=\"k in stBarKey\"", ":class=\"stBarClass(k)\""] {
            assert!(
                html.contains(binding),
                "the screen-time chart's key must be rendered by {binding}, so each swatch is \
                 painted by the same method as the bars it explains."
            );
        }

        // And the negative half: a label in the markup means a swatch is being hand-painted again.
        // "not measured" is deliberately absent from this list — it legitimately appears in the
        // chart's own `aria-label`. If this ever fires on ordinary dashboard copy that happens to
        // contain one of these phrases, narrow the list rather than deleting the check.
        for label in ["within budget", "over budget"] {
            assert!(
                !html.contains(label),
                "index.html spells out the chart-key label {label:?}. The key is built by \
                 `stBarKey` in assets/app.js so each swatch is painted by `stBarClass` — the same \
                 method the bars use — and cannot drift from them. A label in the markup means a \
                 swatch is being hand-painted again, which is how the key came to show a flat \
                 `bg-error` chip beside striped `st-over` bars."
            );
        }
        assert!(
            html.contains("stBarKey"),
            "nothing in index.html renders `stBarKey`, so the screen-time chart has no key at \
             all. Three encoded states without one is a puzzle rather than a chart."
        );
    }

    /// The CSS build chain must strip, then compile, then stamp — in that order.
    ///
    /// `web/scripts/stamp-build.mjs` advances `assets/app.css`'s mtime after a successful build, so
    /// `build.rs`'s freshness warning stops firing on the byte-identical rebuilds Tailwind
    /// deliberately does not write. That repair depends entirely on *where* the stamp sits in the
    /// chain, and the chain is a single line of JSON that anyone might reorder:
    ///
    /// * `strip-comments.mjs` regenerates `web/.scan/`, the copies Tailwind actually scans. Stamp a
    ///   stylesheet compiled from a stale scan and the freshness it claims is not there.
    /// * The stamp must come **after** Tailwind. Moved earlier — or run when the compile failed —
    ///   the warning inverts from a false alarm into a false *silence*, which is strictly worse:
    ///   the check exists to speak up when the stylesheet is behind.
    ///
    /// `&&` between each step is part of the contract, not formatting: with `;` a failed compile
    /// would still stamp, asserting freshness for a stylesheet that was never rebuilt.
    ///
    /// Read as text rather than parsed, for the same reason the manifest guard in `control` is:
    /// this must fail on a machine that has never run `npm`, which is every machine in CI's Rust
    /// jobs and the one this is developed on.
    #[test]
    fn the_css_build_chain_stamps_only_after_a_successful_compile() {
        const PKG: &str = include_str!("../web/package.json");

        let build = PKG
            .lines()
            .find(|l| l.trim_start().starts_with("\"build\":"))
            .expect("web/package.json must define a `build` script");

        let step = |needle: &str| {
            build
                .find(needle)
                .unwrap_or_else(|| panic!("the build chain no longer runs `{needle}`:\n  {build}"))
        };
        let (strip, compile, stamp) = (
            step("strip-comments.mjs"),
            step("tailwindcss"),
            step("stamp-build.mjs"),
        );

        assert!(
            strip < compile,
            "`strip-comments.mjs` must run before Tailwind, or Tailwind compiles a stale scan:\n  \
             {build}"
        );
        assert!(
            compile < stamp,
            "`stamp-build.mjs` must run after Tailwind, or a build that never compiled is stamped \
             as fresh — turning `build.rs`'s warning from a false alarm into a false silence:\n  \
             {build}"
        );
        // Between the last two steps specifically: `;` there would stamp a failed compile.
        let between = &build[compile..stamp];
        assert!(
            between.contains("&&"),
            "the compile and the stamp must be joined by `&&`, so a failed compile stamps \
             nothing:\n  {build}"
        );
    }

    /// No served page uses an Alpine expression its CSP build cannot parse.
    ///
    /// `script-src` is `'self'` with no `'unsafe-eval'`, which is only possible because the CSP
    /// build parses attribute expressions with its own small parser instead of `new Function`.
    /// That parser is stricter than JavaScript, and the failure mode is the one this codebase keeps
    /// meeting: the page renders, nothing throws in Rust, and one directive silently stops
    /// evaluating.
    ///
    /// The four constructs below were established by probing the build, not by reading about it —
    /// the documentation is silent on two of them, and the review pass that tracked this work
    /// records an earlier confident claim about `x-model` that turned out to be false. What the
    /// parser reports:
    ///
    /// * `?.`         — `CSP Parser Error: Unexpected token: PUNCTUATION "."`
    /// * `??`         — `CSP Parser Error: Unexpected token: PUNCTUATION "?"`
    /// * a backtick   — `CSP Parser Error: Unexpected token: OPERATOR`
    /// * `[...spread]` — no error at all; the loop simply renders nothing
    ///
    /// The last is why this test exists rather than a reliance on the console: a construct that
    /// fails *silently* is exactly what shipped a chart with no bars once already.
    ///
    /// Everything else Alpine's own expressions need still works in an attribute — property paths,
    /// ternaries, comparisons, method calls with arguments, assignment, `x-model`, array literals.
    /// The fix for anything caught here is a getter or a method on the component, never widening
    /// the policy.
    #[test]
    fn no_alpine_expression_needs_more_than_the_csp_build_can_parse() {
        // (pattern, what the CSP parser does with it)
        const FORBIDDEN: &[(&str, &str)] = &[
            ("`", "template literal — Unexpected token: OPERATOR"),
            ("...", "spread — renders nothing, silently"),
            (
                "?.",
                "optional chaining — Unexpected token: PUNCTUATION \".\"",
            ),
            (
                "??",
                "nullish coalescing — Unexpected token: PUNCTUATION \"?\"",
            ),
        ];

        let mut bad = Vec::new();
        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            for (attr, value) in alpine_directives(&html) {
                for (pattern, why) in FORBIDDEN {
                    if value.contains(pattern) {
                        bad.push(format!("{name}: {attr}=\"{value}\" — {why}"));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "these expressions cannot be parsed by Alpine's CSP build, so the directive would \
             quietly stop working while `script-src` stays tight.\nMove the expression into a \
             getter or method on the component:\n{bad:#?}",
        );
    }

    /// A `<summary>` never contains a control of its own.
    ///
    /// The five rarely-used cards are `<details>`, so a parent scrolling a phone passes five
    /// headings instead of five full panels. That works because the browser toggles on a click
    /// anywhere in the `<summary>` — which is also the trap: put the card's **Refresh** button in
    /// there and pressing it collapses the panel you were trying to refresh. The button is
    /// deliberately in the body, and the layout that keeps it there is a `justify-end` row that
    /// looks like an oversight and is not.
    ///
    /// Nothing else can catch this. Both arrangements parse, both render, both look right in a
    /// screenshot, and the wrong one only shows itself to someone who clicks the button while the
    /// panel is open. Every Rust gate and every markup guard here passed over exactly that.
    #[test]
    fn no_summary_swallows_a_control() {
        let mut bad = Vec::new();
        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            let mut rest = html.as_str();
            while let Some(start) = rest.find("<summary") {
                let after = &rest[start..];
                let end = after.find("</summary>").expect("an unterminated <summary>");
                let block = &after[..end];
                for control in ["<button", "<input", "<select", "<textarea", "<a "] {
                    if block.contains(control) {
                        bad.push(format!("{name}: a <summary> contains `{control}`"));
                    }
                }
                rest = &after[end..];
            }
        }
        assert!(
            bad.is_empty(),
            "a control inside a <summary> is pressed *and* toggles the panel, so the control \
             appears not to work.\nPut it in the body instead:\n{bad:#?}",
        );
    }

    /// Every class written into the markup has a rule behind it in the compiled stylesheet.
    ///
    /// This is the guard for a failure with no symptom. A class that no longer exists is still
    /// emitted into the DOM and still looks like styling to anyone reading the source; the element
    /// simply renders unstyled, and nothing anywhere reports it. That is how the daisyUI 4 → 5
    /// upgrade left **69 dead references** across these two pages — `label-text`, `form-control`,
    /// `input-bordered`, `select-bordered`, all four removed in v5 — with two of them silently
    /// changing how every form on the product looked.
    ///
    /// Two things this deliberately gets right, both of which produced false findings on the way
    /// here:
    ///
    /// * **Static `class="…"` only.** Alpine's `:class` holds JavaScript, not class names, and
    ///   scanning it reports `===`, `null` and every property name as a missing class.
    /// * **CSS escaping is undone first.** Tailwind writes `2xl:max-w-[110rem]` as
    ///   `.\32 xl\:max-w-\[110rem\]` — a leading digit becomes `\3<hex><space>`, and `:`, `/`, `[`
    ///   and `]` each gain a backslash. Matching the raw text reports classes as missing while
    ///   their rules sit right there.
    ///
    /// Failing here also catches a stale `assets/app.css`: editing the markup without rebuilding
    /// leaves new utilities with no rule, which is the same defect arriving by a different route.
    /// `build.rs` warns about that, and a warning that scrolls past is indistinguishable from none.
    #[test]
    fn every_class_in_the_markup_has_a_rule_in_the_shipped_css() {
        let css = unescape_css(&app_css());
        // A set, not a sorted-and-deduped Vec: the same class appears in dozens of attributes, and
        // ordered-unique is what this collection *is* rather than something done to it afterwards.
        let mut missing = std::collections::BTreeSet::new();

        for (name, page) in PAGES {
            let html = strip_html_comments(page);
            for classes in static_class_attrs(&html) {
                for class in classes.split_whitespace() {
                    if !css_has_rule(&css, class) {
                        missing.insert(format!("{name}: {class}"));
                    }
                }
            }
        }

        assert!(
            missing.is_empty(),
            "these classes are in the markup but have no rule in assets/app.css.\nEither they were \
             removed by a library upgrade, or app.css needs `cd web && npm run build`:\n{missing:#?}"
        );
    }

    /// The values of every **static** `class` attribute, skipping Alpine's `:class` /
    /// `x-bind:class`.
    ///
    /// Accepts either quote style. Both pages happen to use double quotes throughout, so matching
    /// only `class="` works today — and would go on "working" in silence the day one attribute is
    /// written with single quotes, skipping that element with no failure. A scanner that quietly
    /// covers less than it claims is the exact defect this whole test exists to catch, so it must
    /// not have one of its own.
    fn static_class_attrs(html: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut from = 0usize;

        while let Some(at) = html[from..].find("class=") {
            let at = from + at;
            let before = html[..at].chars().next_back();
            let open = at + "class=".len();
            let quote = html[open..].chars().next();

            // `:class="…"` and `x-bind:class="…"` both end in a colon and both hold JavaScript,
            // not class names — scanning them reports `===` and `null` as missing classes.
            let bound = before == Some(':');
            // `superclass="…"` is a different attribute that merely ends in these six letters.
            let is_attribute = before.is_none_or(|c| c.is_whitespace() || c == ':');

            match quote {
                Some(q @ ('"' | '\'')) if is_attribute => {
                    let value = open + q.len_utf8();
                    let end = html[value..]
                        .find(q)
                        .map(|e| value + e)
                        .unwrap_or(html.len());
                    if !bound {
                        found.push(&html[value..end]);
                    }
                    from = end + q.len_utf8();
                }
                _ => from = open,
            }
        }

        found
    }

    /// What the class scan does and does not pick up, pinned.
    ///
    /// This helper decides what the guard above can see, so a gap in it makes that guard pass
    /// while checking less than it says. That is a worse failure than the one it was written to
    /// catch, because it looks like coverage.
    #[test]
    fn the_class_scan_reads_static_attributes_and_only_those() {
        let html = r#"
            <div class="a b"></div>
            <div class='c'></div>
            <div :class="cond ? 'x' : null"></div>
            <div x-bind:class="obj"></div>
            <div superclass="not-a-class"></div>
        "#;

        assert_eq!(
            static_class_attrs(html),
            vec!["a b", "c"],
            "both quote styles are read; bound expressions and lookalike attributes are not"
        );
    }

    /// Undo CSS identifier escaping so a selector can be compared against a plain class name.
    fn unescape_css(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut chars = css.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            // `\3<hex><space>` is how a leading digit is written: `.2xl…` ships as `.\32 xl…`.
            let mut probe = chars.clone();
            if probe.next() == Some('3')
                && let Some(digit) = probe.next()
                && digit.is_ascii_hexdigit()
            {
                let mut after = probe.clone();
                if after.next() == Some(' ') {
                    probe = after;
                }
                out.push(digit);
                chars = probe;
                continue;
            }
            // Anything else is one escaped punctuation character: `\:`, `\/`, `\[`, `\.`.
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            }
        }
        out
    }

    /// Whether `class` appears as a class selector in the (already unescaped) stylesheet.
    fn css_has_rule(css: &str, class: &str) -> bool {
        let needle = format!(".{class}");
        let mut from = 0usize;
        while let Some(at) = css[from..].find(&needle) {
            let at = from + at;
            let after = css[at + needle.len()..].chars().next();
            // `.flex` must not be satisfied by `.flex-col`: the name has to end where it ends.
            let ends = after.is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '_');
            if ends {
                return true;
            }
            from = at + needle.len();
        }
        false
    }

    /// Every form control can be named by a screen reader.
    ///
    /// A control with only a `placeholder` is announced as "edit, blank" — the placeholder is a
    /// hint, not a name, and it vanishes the moment anything is typed. Eight controls here had
    /// nothing else: six rows in the rules editor, the routine-name box, and the curfew enable
    /// toggle, which announced as "checkbox, not checked" beside a heading it had no relationship
    /// to. The ✕ buttons sitting immediately beside those same rows all carried `aria-label`, which
    /// is what marks this as an oversight rather than a decision.
    ///
    /// A wrapping `<label>` counts — that is how the per-weekday budget boxes are named, and they
    /// are correct as they stand. Scanned rather than counted, because the regression mode is a new
    /// row copied from an old one.
    #[test]
    fn every_form_control_can_be_named_by_a_screen_reader() {
        const CONTROLS: [&str; 3] = ["<input", "<select", "<textarea"];
        let mut unnamed: Vec<String> = Vec::new();

        for (page, html) in PAGES {
            let html = strip_html_comments(html);
            let mut depth = 0usize;
            let mut at = 0usize;

            while at < html.len() {
                let next = ["<label", "</label>"]
                    .iter()
                    .chain(CONTROLS.iter())
                    .filter_map(|tag| html[at..].find(tag).map(|i| (at + i, *tag)))
                    .min_by_key(|(i, _)| *i);
                let Some((found, tag)) = next else { break };

                match tag {
                    "<label" => depth += 1,
                    "</label>" => depth = depth.saturating_sub(1),
                    _ => {
                        let end = html[found..]
                            .find('>')
                            .map(|e| found + e)
                            .unwrap_or(html.len());
                        let element = &html[found..end];
                        // A control removed from the accessibility tree needs no name, because
                        // nothing will ever announce it. `type="hidden"` is the obvious form of
                        // that. The sign-in form's user-name field is the other: it exists only so
                        // a password manager can key the credential, it carries no information a
                        // person needs, and it has to be *rendered* (`sr-only`, not `display:
                        // none`) because some managers skip fields that are not.
                        //
                        // Both attributes are required together and that is the whole point of
                        // writing it this way. `aria-hidden` on a control a keyboard can still
                        // reach is a textbook defect — focus lands on something the screen reader
                        // has been told does not exist — and `tabindex="-1"` alone leaves the
                        // control in the tree, still announced and still nameless. Neither half
                        // opens this exemption on its own.
                        let removed_from_a11y_tree = element.contains("aria-hidden=\"true\"")
                            && element.contains("tabindex=\"-1\"");
                        let named = element.contains("aria-label")
                            || element.contains(" id=")
                            || element.contains("type=\"hidden\"")
                            || removed_from_a11y_tree;
                        // Inside a <label>, the label's own text is the name.
                        if depth == 0 && !named {
                            unnamed.push(format!("{page}: {}", element.trim()));
                        }
                    }
                }
                at = found + tag.len();
            }
        }

        assert!(
            unnamed.is_empty(),
            "these controls sit outside any <label> and carry no aria-label or id, so a screen \
             reader has nothing to announce but the field type:\n{unnamed:#?}"
        );
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
        for (name, page) in PAGES {
            // Comments stripped, like every other scan here. This one read the raw markup and
            // passed only because no comment happens to contain `<th` — while index.html already
            // has one containing `<template` and `<svg`, which is why the scan beside it strips
            // first. Prose about a table would have failed this test, and the repo has been bitten
            // by exactly that twice.
            let html = strip_html_comments(page);
            let mut rest = html.as_str();
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
