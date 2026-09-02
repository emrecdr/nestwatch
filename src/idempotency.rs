//! Replay cache for the `Idempotency-Key` request header.
//!
//! Semantics follow the IETF HTTPAPI working group's draft
//! (`draft-ietf-httpapi-idempotency-key-header`, revision 07): a client retrying a POST whose
//! *response* was lost sends the same key, and the server returns the original
//! outcome instead of acting twice. The client this exists for is a phone app
//! granting earned bonus time after a sync — its scheduler retries with backoff
//! and can be killed between the write landing and the response arriving, which
//! without this would grant the same practice twice.
//!
//! **This cache is a courtesy, not the authority.** The once-per-source-per-day
//! latch in [`crate::config::Config::earned`] is what actually prevents double
//! grants, and it is persisted; this cache is in-memory and empties on restart.
//! The division is deliberate: losing the cache costs a retried request one
//! `already_granted_today` reply instead of its original body, never a second
//! grant. Two concurrent requests with the same key can both miss here — the
//! draft permits that, and the day latch still admits only one.
//!
//! **On the draft's standing.** Revision 07 (2025-10-15) *expired* on 2026-04-18 with its
//! intended RFC status recorded as "(None)" — it is not Standards-Track and never was, and an
//! earlier version of this comment said it was. It is cited here because it is the closest thing
//! to a written specification of a header the whole industry already ships, not because it
//! carries any authority.
//!
//! **Two deliberate departures, both toward what deployed implementations actually do:**
//!
//! 1. *Keys are scoped, and the draft does not say how.* It says only that "uniqueness of the key
//!    MUST be defined by the resource owner". Storing keys bare would let two integrations that
//!    both key by day — `2026-09-02` is the obvious choice — collide, and the loser would be
//!    handed the winner's response, grant nothing, and report success. The only client identity
//!    on this wire is `source`, so that is the scope; the handler namespaces before it gets here.
//!
//! 2. *A reused key carrying different inputs answers 400, not the draft's 422.* Not a
//!    disagreement about the rule — it is the same rule — but about which code can be read. This
//!    service is built on axum, whose `Json` extractor already returns 422 for a body that does
//!    not fit the struct, so a second meaning on 422 would leave a client unable to tell "your
//!    JSON is malformed" from "your key is reused". 400 with an explicit message is also what
//!    Stripe has returned for this for a decade, which is the behaviour client authors expect.
//!
//! Bounded three ways, because the caller is authenticated but this must not be
//! a memory lever: keys longer than [`MAX_KEY_LEN`] are rejected by the handler
//! before reaching here, entries expire after [`RETENTION`], and the map never
//! holds more than [`MAX_ENTRIES`] (oldest evicted first).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Longest key the handler accepts. UUIDs are 36 characters; triple that is
/// generous, and anything longer is not a key, it is a payload.
pub const MAX_KEY_LEN: usize = 128;

/// How long a stored response can be replayed. Two days covers every retry
/// schedule a phone scheduler produces, including one that straddles midnight,
/// without remembering requests from a week ago.
pub const RETENTION: Duration = Duration::from_secs(48 * 60 * 60);

/// Hard cap on stored entries. The expected population is a handful of grants a
/// day; 256 is far above any honest use and small enough that eviction never
/// needs to be clever.
pub const MAX_ENTRIES: usize = 256;

/// What a stored entry says about a request arriving under the same key.
#[derive(Debug, PartialEq)]
pub enum Replay {
    /// Nothing stored, or it expired. Run the request.
    Fresh,
    /// Same key, same inputs: the retry of a request that already ran. Hand back what it got.
    Stored(Value),
    /// Same key, **different inputs**. Not a retry — a client that reused one key across two
    /// different operations. Replaying here would answer the second request with the first's
    /// outcome and silently never perform it, so this is refused instead.
    Mismatch,
}

/// The in-memory replay store. One per process, behind a mutex in
/// [`crate::state::AppState`].
///
/// Each entry carries the *fingerprint* of the inputs that produced it — whatever the caller
/// decided actually determines the outcome, not the raw body. That distinction matters: a field
/// the handler never reads must not be able to make an honest retry look like a key collision.
#[derive(Debug, Default)]
pub struct IdempotencyCache {
    entries: HashMap<String, Entry>,
}

#[derive(Debug)]
struct Entry {
    at: Instant,
    fingerprint: Value,
    response: Value,
}

impl IdempotencyCache {
    /// What to do with a request carrying `key` and the inputs `fingerprint` describes.
    ///
    /// Takes `now` rather than reading the clock so tests can drive time; the
    /// caller passes `Instant::now()`.
    pub fn replay(&mut self, key: &str, fingerprint: &Value, now: Instant) -> Replay {
        self.prune(now);
        match self.entries.get(key) {
            None => Replay::Fresh,
            Some(e) if e.fingerprint == *fingerprint => Replay::Stored(e.response.clone()),
            Some(_) => Replay::Mismatch,
        }
    }

    /// Record `response` as the outcome for `key`.
    ///
    /// If the cache is full after pruning, the oldest entry is evicted — a
    /// replay lost to eviction degrades to the day latch's answer, which is
    /// the safe direction.
    pub fn record(&mut self, key: String, fingerprint: Value, response: Value, now: Instant) {
        self.prune(now);
        if self.entries.len() >= MAX_ENTRIES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            Entry {
                at: now,
                fingerprint,
                response,
            },
        );
    }

    /// Drop everything older than [`RETENTION`].
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, e| now.duration_since(e.at) < RETENTION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The fingerprint most of these tests do not care about.
    const ANY: Value = Value::Null;

    #[test]
    fn replays_what_was_recorded() {
        let mut cache = IdempotencyCache::default();
        let now = Instant::now();
        cache.record("k1".into(), ANY, json!({ "ok": true, "minutes": 30 }), now);
        assert_eq!(
            cache.replay("k1", &ANY, now),
            Replay::Stored(json!({ "ok": true, "minutes": 30 }))
        );
        assert_eq!(cache.replay("k2", &ANY, now), Replay::Fresh);
    }

    #[test]
    fn the_same_key_with_different_inputs_is_a_mismatch_not_a_replay() {
        let mut cache = IdempotencyCache::default();
        let now = Instant::now();
        cache.record("k".into(), json!(10), json!({ "ok": true }), now);

        // The retry of that request: same key, same inputs.
        assert_eq!(
            cache.replay("k", &json!(10), now),
            Replay::Stored(json!({ "ok": true })),
            "an honest retry still replays"
        );
        // A *different* request wearing the same key. Answering it with the stored response
        // would report success for something that never ran.
        assert_eq!(cache.replay("k", &json!(20), now), Replay::Mismatch);
        // Absent is its own input, distinct from any number — a client that dropped the field
        // is not repeating a client that sent one.
        assert_eq!(cache.replay("k", &Value::Null, now), Replay::Mismatch);
    }

    #[test]
    fn entries_expire_after_retention() {
        let mut cache = IdempotencyCache::default();
        let start = Instant::now();
        cache.record("k".into(), ANY, json!({ "ok": true }), start);
        let later = start + RETENTION + Duration::from_secs(1);
        assert_eq!(cache.replay("k", &ANY, later), Replay::Fresh);
    }

    #[test]
    fn full_cache_evicts_the_oldest_entry() {
        let mut cache = IdempotencyCache::default();
        let start = Instant::now();
        for i in 0..MAX_ENTRIES {
            // Later entries get later instants, so "oldest" is well-defined.
            cache.record(
                format!("k{i}"),
                ANY,
                json!(i),
                start + Duration::from_secs(i as u64),
            );
        }
        let now = start + Duration::from_secs(MAX_ENTRIES as u64);
        cache.record("overflow".into(), ANY, json!("new"), now);
        assert_eq!(
            cache.replay("k0", &ANY, now),
            Replay::Fresh,
            "oldest evicted"
        );
        assert_eq!(
            cache.replay("k1", &ANY, now),
            Replay::Stored(json!(1)),
            "second-oldest kept"
        );
        assert_eq!(
            cache.replay("overflow", &ANY, now),
            Replay::Stored(json!("new"))
        );
    }
}
