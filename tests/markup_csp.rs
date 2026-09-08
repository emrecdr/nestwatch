//! Guards two invariants over `assets/*.html` that only a browser would otherwise reveal: no
//! Alpine expression uses syntax the CSP build cannot parse, and no element claims modal semantics
//! it cannot deliver.
//!
//! Both are here for one reason, worth stating once rather than twice: **nothing in this repository
//! ever renders these pages.** `web/test/harness.js` says so in as many words — the Node suite is
//! deliberately DOM-free, and the Rust side reads the markup as text. So a property that only
//! exists once a browser has laid the page out is checked by a source scan or by nothing at all.
//! Both scans below are the former, and both are honest that they are not the latter.
//!
//! # Invariant 1 — syntax the CSP build cannot parse
//!
//! # The class this exists for
//!
//! The dashboard runs Alpine's **CSP build**, which parses attribute expressions with its own small
//! parser instead of handing them to `new Function`. That is the whole reason `script-src` can drop
//! `'unsafe-eval'`. The cost is that four ordinary JavaScript constructs do not parse there, and
//! `assets/app.js` records them with the parser errors they produce — established by probing the
//! build, because the documentation is silent on two of them:
//!
//! ```text
//!     `?.`     CSP Parser Error: Unexpected token: PUNCTUATION "."
//!     `??`     CSP Parser Error: Unexpected token: PUNCTUATION "?"
//!     backtick CSP Parser Error: Unexpected token: OPERATOR
//!     [...x]   renders nothing at all, with no error
//! ```
//!
//! **The fourth is why this file exists.** The first three announce themselves in the browser
//! console. A spread renders nothing, silently — no error, no failing test — and a card that draws
//! nothing is indistinguishable from a card with nothing to report. That ambiguity is the failure
//! this codebase spends the most effort avoiding everywhere else (`focus_missing`, `measured` vs
//! absent, an unpaired session marker drawn with no width).
//!
//! Nothing else catches it. The Node suite in `web/` exercises `app.js`'s methods directly and
//! never evaluates a template expression, so 467 expressions across two files are checked by
//! nothing at all. `docs/DECLINED-OPTIONS.md` already states the hazard — in the course of
//! declining a *refactor* into this markup — but a guard was never proposed.
//!
//! # Why now, and why it is cheap
//!
//! Every expression in the tree already passes. This locks in a property that holds rather than
//! opening a cleanup, which is the same bargain `Cargo.toml`'s `[lints]` block makes for itself and
//! for the same reason: a rule adopted while it is already green costs one commit, and a rule
//! adopted after it goes red costs an argument about whether to keep it.
//!
//! # What it does not claim
//!
//! It reads attribute values, not JavaScript. `app.js` uses all four constructs freely and
//! correctly — the constraint is on the *markup*, and the block of getters at the bottom of `app.js`
//! exists precisely to hold expressions that had to move out of an attribute. So a violation moved
//! into a method is invisible here, which is the intended outcome rather than a gap.
//!
//! It also cannot see a directive assembled at runtime, or one whose value arrives from the server.
//! Neither exists in this tree.

use std::path::{Path, PathBuf};

use nestwatch::srcscan::line_of;

/// Every `assets/*.html`, as (relative name, contents).
///
/// Read from disk rather than `include_str!` so that adding a third page is covered without
/// touching this file — the failure mode of a hardcoded list is that it keeps passing while the new
/// page goes unread.
fn markup() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    let mut out = Vec::new();
    for entry in entries {
        let path: PathBuf = entry.expect("unreadable directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("asset filename is not UTF-8")
            .to_owned();
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        out.push((name, body));
    }
    out.sort();
    // A broken reader must not be able to pass by finding nothing.
    assert!(
        out.len() >= 2,
        "found {} html assets; the reader is broken, not the markup",
        out.len()
    );
    out
}

/// `text` with every `<!-- ... -->` replaced by the newlines it spanned.
///
/// **Newlines are kept rather than dropped, so that offsets into the result still name the right
/// line of the original file.** Removing them outright was the first version, and it made
/// `no_element_claims_modal_semantics_without_being_a_dialog` report `index.html line 1456` for an
/// element that is on line 1769 — a guard that names the wrong line sends its reader to innocent
/// code, and this tree is 40% comment by design, so the drift is hundreds of lines rather than a
/// rounding error. Nothing here reads the stripped text as markup to render; only as a haystack.
///
/// **Load-bearing, not tidiness.** `index.html` carries a comment reading "A data URI rather than
/// x-html: the CSP already allows ..." — prose *about* the constraint, sitting next to the code that
/// respects it. A scan that read comments would flag the explanation of the rule as a breach of it,
/// and the usual fix for a guard that cries wolf is to delete the guard.
///
/// An unterminated comment consumes the rest of the file rather than being ignored, so a malformed
/// page fails closed on the count assertion below instead of being silently half-read.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) => {
                for _ in after[..end].bytes().filter(|b| *b == b'\n') {
                    out.push('\n');
                }
                rest = &after[end + 3..];
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Whether an attribute name introduces an Alpine expression.
///
/// The three spellings Alpine accepts: the full directive (`x-show`), the `x-on` shorthand (`@click`)
/// and the `x-bind` shorthand (`:class`). Modifiers ride on the name (`x-model.number`,
/// `@keydown.escape.window`) and need no special handling because this only inspects the prefix.
///
/// `x-data` is included even though every occurrence in this tree is the bare component name
/// `"app"`: it is an expression position, and the day one carries an inline object literal is
/// exactly the day it needs checking.
fn is_alpine_attr(name: &str) -> bool {
    name.starts_with("x-") || name.starts_with('@') || name.starts_with(':')
}

/// Every Alpine expression in `text`, as (attribute name, value).
///
/// **Reads the whole text, never line by line, and that is the point.** `tests/scanner_guards.rs`
/// exists because three guards in this crate were found blind after the formatter broke their needle
/// across a newline. A markup attribute has the same hazard for a different reason: an editor
/// wrapping one long `:aria-label` is enough to hide its value from a line-oriented scan, and the
/// guard would then report success over precisely the expression it exists to read. No attribute in
/// this tree spans a line today. This is written so that the day one does, nothing changes.
fn expressions(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(eq) = rest.find("=\"") {
        let (before, after) = rest.split_at(eq);
        // The attribute name is the run of name characters immediately before `="`. Anything else
        // (whitespace, `<`, a quote) ends it, so `href="..."` inside a value cannot be mistaken for
        // an attribute of the enclosing tag.
        let name: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let value_start = &after[2..];
        let Some(close) = value_start.find('"') else {
            // An unterminated attribute value. Stop rather than guess; the count assertion is what
            // turns this into a failure instead of a quiet short read.
            break;
        };
        if is_alpine_attr(&name) {
            found.push((name, value_start[..close].to_owned()));
        }
        rest = &value_start[close + 1..];
    }
    found
}

/// The constructs Alpine's CSP build cannot parse, each with the symptom it produces.
///
/// Spelled as data rather than as four `if`s so the failure message can name which one was hit —
/// three of these are announced in the browser console and one is not, and a reader who has just
/// broken the silent one needs to be told that is what happened.
const FORBIDDEN: [(&str, &str); 4] = [
    (
        "?.",
        "optional chaining — CSP Parser Error: Unexpected token: PUNCTUATION \".\"",
    ),
    (
        "??",
        "nullish coalescing — CSP Parser Error: Unexpected token: PUNCTUATION \"?\"",
    ),
    (
        "`",
        "a template literal — CSP Parser Error: Unexpected token: OPERATOR",
    ),
    (
        "...",
        "a spread — renders NOTHING AT ALL, with no error and no console message",
    ),
];

#[test]
fn no_markup_expression_uses_syntax_the_csp_build_cannot_parse() {
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    for (name, body) in markup() {
        for (attr, value) in expressions(&strip_comments(&body)) {
            checked += 1;
            for (token, symptom) in FORBIDDEN {
                if value.contains(token) {
                    offenders.push(format!("{name}: {attr}=\"{value}\"\n    -> {symptom}"));
                }
            }
        }
    }

    // A broken extractor must not be able to pass this by finding nothing. The tree holds several
    // hundred expressions; a floor well under that catches a parser that has stopped working
    // without pinning a number that a routine edit would have to chase.
    assert!(
        checked >= 300,
        "only found {checked} Alpine expressions; the extractor is broken, not the markup"
    );

    assert!(
        offenders.is_empty(),
        "these attribute expressions use syntax Alpine's CSP build cannot parse:\n  {}\n\nMove the \
         expression into a method or getter in assets/app.js, where all four are ordinary \
         JavaScript, and call that from the attribute. The block at the bottom of app.js exists \
         for exactly this.",
        offenders.join("\n  ")
    );
}

/// The attributes that *assert* modal semantics, each of which a native `<dialog>` already carries.
///
/// Spelled with their quotes so that prose about the rule — an `aria-modal` named in a sentence —
/// is not itself a breach. Comments are stripped before the scan anyway; this is the second layer.
const MODAL_CLAIMS: [&str; 5] = [
    "aria-modal",
    "role=\"dialog\"",
    "role='dialog'",
    "role=\"alertdialog\"",
    "role='alertdialog'",
];

/// A modal that is not a `<dialog>` is a modal with no focus trap.
///
/// The full-size screenshot overlay used to be a `<div>` carrying `role="dialog"` and
/// `aria-modal="true"`, shown with `x-show`. Both are *assertions to assistive technology* —
/// `aria-modal` says the rest of the page is unavailable — and nothing in the document made either
/// one true. Searching `assets/app.js` and `assets/ask.js` for `.focus()`, `activeElement`, `inert`
/// and `showModal` returned **nothing**: there was no focus management anywhere in the product.
///
/// Three things followed, and none of them needed a bug report to be real:
///
/// * Focus never entered the overlay. It stayed on the button that opened it — an element
///   `aria-modal` had just declared unavailable — so a screen-reader user was told the background
///   was hidden while their focus sat inside it, and nothing was announced.
/// * `Tab` walked out of the overlay into the page behind an opaque backdrop, which carries
///   **Kill** and **Shut down**.
/// * `x-show` hides with `display: none`, so closing dropped focus to `<body>` and the next `Tab`
///   restarted at the top of the document.
///
/// A native `<dialog>` opened with `showModal()` has the focus trap, the inert background, `Esc`
/// and top-layer rendering built in — and carries `role=dialog` and `aria-modal` *implicitly*.
/// That implicitness is what makes this scan a sound test rather than a style rule: finding either
/// attribute **spelled out** means one of exactly two things, and both are worth failing on.
/// Either the element is a `<dialog>`, and the attribute is redundant; or it is not, and the
/// attribute is a promise the markup cannot keep.
///
/// # What it does not claim
///
/// It cannot tell that a `<dialog>` is opened with `showModal()` rather than `show()` — the
/// non-modal form, which traps nothing. That call lives in `app.js` and is guarded by the reading
/// of whoever changes it. Nor does it check focus order, which no source scan can.
#[test]
fn no_element_claims_modal_semantics_without_being_a_dialog() {
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for (name, body) in markup() {
        let body = strip_comments(&body);
        scanned += body.len();
        for claim in MODAL_CLAIMS {
            for (at, _) in body.match_indices(claim) {
                offenders.push(format!("{name} line {}: {claim}", line_of(&body, at)));
            }
        }
    }

    // A reader that has stopped finding the pages must not be able to pass by finding nothing.
    // The two shipped pages are ~65 KB together; a floor well under that catches a broken reader
    // without pinning a number that ordinary editing would have to chase.
    assert!(
        scanned > 20_000,
        "only {scanned} bytes of markup scanned; the reader is broken, not the pages"
    );

    assert!(
        offenders.is_empty(),
        "these elements assert modal semantics that only a native <dialog> can deliver:\n  {}\n\n\
         Use `<dialog>` and open it with `showModal()`. It carries both attributes implicitly, so \
         writing them out is either redundant or false — and it brings the focus trap, the inert \
         background and Esc, none of which `x-show` on a <div> provides.",
        offenders.join("\n  ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extractor must survive an attribute broken across lines, because that is the shape that
    /// silently defeats a line-oriented scan.
    #[test]
    fn an_attribute_spanning_lines_is_still_read() {
        let text = "<p :aria-label=\"one\n   two\" x-show=\"ok\"></p>";
        let found = expressions(text);
        assert_eq!(found.len(), 2, "both attributes should be found: {found:?}");
        assert_eq!(found[0].1, "one\n   two");
    }

    /// Prose about the rule must not be read as a breach of it.
    #[test]
    fn comments_are_not_scanned() {
        let text = "<!-- prefer x-text over x-html: `a?.b` will not parse --><p x-text=\"ok\"></p>";
        let found = expressions(&strip_comments(text));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, "ok");
    }

    /// A non-Alpine attribute is not an expression position and must not be flagged.
    #[test]
    fn ordinary_attributes_are_ignored() {
        let text = "<a href=\"https://example.test/?a=1\" class=\"btn\" x-show=\"ok\"></a>";
        let found = expressions(text);
        assert_eq!(found.len(), 1, "only the Alpine attribute: {found:?}");
        assert_eq!(found[0].0, "x-show");
    }

    /// Each forbidden construct is actually detected — the guard's own red state, proven rather
    /// than assumed.
    #[test]
    fn every_forbidden_construct_is_caught() {
        for (token, _) in FORBIDDEN {
            let markup = format!("<p x-text=\"a{token}b\"></p>");
            let found = expressions(&markup);
            assert_eq!(found.len(), 1);
            assert!(
                FORBIDDEN.iter().any(|(t, _)| found[0].1.contains(t)),
                "the guard would not catch {token}"
            );
        }
    }
}
