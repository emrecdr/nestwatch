//! Pins the privacy claims that are otherwise only prose.
//!
//! `docs/SECURITY.md` tells a parent what is recorded about their child, and foreground tracking
//! made that answer longer: process names, and — for browser windows — page titles. Titles are
//! the most personal thing this system holds, so the promise around them should not be able to
//! widen by accident while somebody adds a feature.
//!
//! This does **not** assert "no titles". Recording page titles is a deliberate, documented trade
//! (see `FOREGROUND-TRACKING.md`). It asserts the *shape* of the promise: two maps of names to
//! second counts, nothing richer, and no third kind of thing appearing without the document that
//! describes it changing too.
//!
//! Deliberately an integration test rather than a unit test inside `foreground.rs`: it asserts a
//! documented promise about the module's public surface, not an internal behaviour, and it should
//! survive the module being reorganised.

use nestwatch::foreground::Sample;

/// The emitted sample carries exactly the two maps `SECURITY.md` describes.
///
/// Serialising a populated `Sample` and inspecting the JSON is the honest check: the serialised
/// form is what crosses the pipe from the watcher and lands in the rollup, so a field added to the
/// struct shows up here even if nothing else in the crate reads it yet.
#[test]
fn a_foreground_sample_carries_only_what_the_security_doc_describes() {
    let mut sample = Sample::default();
    sample.apps.insert("roblox.exe".into(), 120);
    sample.pages.insert("Roblox".into(), 45);

    let json = serde_json::to_value(&sample).expect("Sample must serialise");
    let object = json.as_object().expect("Sample serialises to an object");

    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["apps", "pages"],
        "a foreground sample must carry only `apps` and `pages`. docs/SECURITY.md tells the \
         parent exactly what is recorded about their child, and this is the structure that \
         carries it. If a field was added deliberately, update that section in the same commit — \
         do not just widen this list.",
    );

    // Both are name -> seconds. Anything richer is a place for a URL, a path, or a timestamp
    // trail to hide, none of which the document admits to.
    for field in ["apps", "pages"] {
        let map = object[field]
            .as_object()
            .unwrap_or_else(|| panic!("`{field}` must be an object"));
        for (name, secs) in map {
            assert!(
                secs.is_u64(),
                "`{field}` entry `{name}` maps to {secs}, not a plain second count",
            );
        }
    }
}

/// A malformed or hostile line never yields a sample carrying anything unexpected.
///
/// The watcher writes to a pipe the child's session can cut mid-line, so parsing is
/// failure-tolerant by design. That tolerance must not become a way to smuggle a field in: serde
/// ignores unknown keys, so a line claiming a URL is dropped rather than carried.
#[test]
fn a_sample_line_claiming_a_url_does_not_keep_it() {
    let line = r#"{"apps":{"chrome.exe":30},"pages":{"Roblox":20},
                   "url":"https://private.test/page","path":"C:\\Users\\child\\diary.docx"}"#;
    let sample = nestwatch::foreground::parse_sample(line).expect("the known fields are valid");

    let text = serde_json::to_string(&sample).expect("Sample must serialise");
    assert!(
        !text.contains("private.test") && !text.contains("diary"),
        "a line carrying extra fields must not round-trip them: {text}",
    );
    assert_eq!(
        sample.apps.get("chrome.exe"),
        Some(&30),
        "the real app data survives"
    );
    assert_eq!(
        sample.pages.get("Roblox"),
        Some(&20),
        "the real page data survives"
    );
}

/// Page titles are capped, and the cap is small enough to be a summary rather than a log.
///
/// Two separate promises in `SECURITY.md` rest on this: that a child retitling a window in a loop
/// cannot grow the tally file without bound, and that what is kept is "a summary of where the time
/// went, not a log of everything opened". A cap raised to, say, 5,000 would keep the first promise
/// and quietly break the second.
#[test]
fn the_page_title_cap_stays_a_summary() {
    assert!(
        nestwatch::foreground::MAX_PAGES <= 100,
        "MAX_PAGES is {}, which is a browsing log rather than a summary. docs/SECURITY.md \
         promises the parent the record is the latter; change the promise or keep the cap.",
        nestwatch::foreground::MAX_PAGES,
    );
}
