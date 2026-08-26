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
    /// The served script, for the guards that can only learn a fact by reading it. Checked in and
    /// not generated, unlike `assets/app.css`, so `include_str!` is safe here — a fresh clone has
    /// this file before it has run anything.
    const APP_JS: &str = include_str!("../assets/app.js");

    const PAGES: [(&str, &str); 2] = [
        ("index.html", include_str!("../assets/index.html")),
        ("ask.html", include_str!("../assets/ask.html")),
    ];

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
        let html = PAGES
            .iter()
            .find(|(name, _)| *name == "index.html")
            .expect("index.html must be one of the served pages")
            .1;
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
                        let named = element.contains("aria-label")
                            || element.contains(" id=")
                            || element.contains("type=\"hidden\"");
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
