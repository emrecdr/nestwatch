//! Replay cache for the `Idempotency-Key` request header.
//!
//! Semantics follow the IETF HTTPAPI working group's Standards-Track draft
//! (`draft-ietf-httpapi-idempotency-key-header`): a client retrying a POST whose
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

/// The in-memory replay store. One per process, behind a mutex in
/// [`crate::state::AppState`].
#[derive(Debug, Default)]
pub struct IdempotencyCache {
    entries: HashMap<String, (Instant, Value)>,
}

impl IdempotencyCache {
    /// The stored response for `key`, if one was recorded within [`RETENTION`].
    ///
    /// Takes `now` rather than reading the clock so tests can drive time; the
    /// caller passes `Instant::now()`.
    pub fn replay(&mut self, key: &str, now: Instant) -> Option<Value> {
        self.prune(now);
        self.entries.get(key).map(|(_, v)| v.clone())
    }

    /// Record `response` as the outcome for `key`.
    ///
    /// If the cache is full after pruning, the oldest entry is evicted — a
    /// replay lost to eviction degrades to the day latch's answer, which is
    /// the safe direction.
    pub fn record(&mut self, key: String, response: Value, now: Instant) {
        self.prune(now);
        if self.entries.len() >= MAX_ENTRIES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(key, (now, response));
    }

    /// Drop everything older than [`RETENTION`].
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, (at, _)| now.duration_since(*at) < RETENTION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_what_was_recorded() {
        let mut cache = IdempotencyCache::default();
        let now = Instant::now();
        cache.record("k1".into(), json!({ "ok": true, "minutes": 30 }), now);
        assert_eq!(
            cache.replay("k1", now),
            Some(json!({ "ok": true, "minutes": 30 }))
        );
        assert_eq!(cache.replay("k2", now), None);
    }

    #[test]
    fn entries_expire_after_retention() {
        let mut cache = IdempotencyCache::default();
        let start = Instant::now();
        cache.record("k".into(), json!({ "ok": true }), start);
        let later = start + RETENTION + Duration::from_secs(1);
        assert_eq!(cache.replay("k", later), None);
    }

    #[test]
    fn full_cache_evicts_the_oldest_entry() {
        let mut cache = IdempotencyCache::default();
        let start = Instant::now();
        for i in 0..MAX_ENTRIES {
            // Later entries get later instants, so "oldest" is well-defined.
            cache.record(
                format!("k{i}"),
                json!(i),
                start + Duration::from_secs(i as u64),
            );
        }
        let now = start + Duration::from_secs(MAX_ENTRIES as u64);
        cache.record("overflow".into(), json!("new"), now);
        assert_eq!(cache.replay("k0", now), None, "oldest evicted");
        assert_eq!(
            cache.replay("k1", now),
            Some(json!(1)),
            "second-oldest kept"
        );
        assert_eq!(cache.replay("overflow", now), Some(json!("new")));
    }
}
