//! Curfew: a "closed" time window during which the machine must not be on.
//!
//! The window is stored as two `HH:MM` local times and may wrap past midnight
//! (e.g. 22:00 → 07:00). The pure [`is_within`] check is separated from the clock so it
//! can be unit-tested exhaustively; [`Curfew::is_active_now`] applies it to local time.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{Local, NaiveTime};
use serde::{Deserialize, Serialize};

use crate::control::SystemControl;

fn default_warn_secs() -> u32 {
    60
}

/// How often the enforcer re-checks the clock.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Curfew {
    pub enabled: bool,
    /// Window start, `HH:MM` local time.
    pub start: String,
    /// Window end, `HH:MM` local time.
    pub end: String,
    /// Grace period (Windows shows a countdown + message) before power-off.
    #[serde(default = "default_warn_secs")]
    pub warn_secs: u32,
}

impl Default for Curfew {
    fn default() -> Self {
        Self { enabled: false, start: "22:00".into(), end: "07:00".into(), warn_secs: 60 }
    }
}

impl Curfew {
    /// Is the *current* local time inside the closed window? `false` if disabled or if the
    /// times are unparseable (fail-open, so a bad config never bricks the machine).
    pub fn is_active_now(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match (parse_hm(&self.start), parse_hm(&self.end)) {
            (Some(start), Some(end)) => is_within(Local::now().time(), start, end),
            _ => {
                tracing::warn!(start = %self.start, end = %self.end, "invalid curfew times; ignoring");
                false
            }
        }
    }

    /// Validate the `HH:MM` fields (used when accepting settings from the UI).
    pub fn validate(&self) -> Result<(), String> {
        if parse_hm(&self.start).is_none() {
            return Err(format!("invalid start time: {}", self.start));
        }
        if parse_hm(&self.end).is_none() {
            return Err(format!("invalid end time: {}", self.end));
        }
        Ok(())
    }
}

/// Background loop: every [`CHECK_INTERVAL`], if the current time is inside an enabled
/// window, initiate a warned shutdown exactly once per window entry (`armed`). Leaving the
/// window disarms, so the next entry fires again. Runs for the life of the server.
pub async fn run_enforcer(control: Arc<dyn SystemControl>, curfew: Arc<RwLock<Curfew>>) {
    let mut armed = false;
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;

        let (active, warn_secs) = {
            let c = curfew.read().unwrap();
            (c.is_active_now(), c.warn_secs)
        };

        if active && !armed {
            armed = true;
            tracing::warn!("curfew window active — initiating shutdown ({warn_secs}s warning)");
            let control = control.clone();
            let msg = "Curfew: this computer is shutting down.".to_string();
            let result =
                tokio::task::spawn_blocking(move || control.shutdown(warn_secs, Some(msg))).await;
            if let Ok(Err(e)) = result {
                tracing::error!(error = %e, "curfew shutdown failed");
                armed = false; // allow a retry on the next tick
            }
        } else if !active {
            armed = false;
        }
    }
}

/// Parse `"HH:MM"` (24-hour) into a `NaiveTime`.
fn parse_hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Whether `now` falls in `[start, end)`, treating `start > end` as a window that wraps
/// midnight. An empty window (`start == end`) is never active.
fn is_within(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    use std::cmp::Ordering;
    match start.cmp(&end) {
        Ordering::Less => now >= start && now < end,     // same day, e.g. 09:00–17:00
        Ordering::Greater => now >= start || now < end,  // wraps midnight, e.g. 22:00–07:00
        Ordering::Equal => false,                        // empty window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn same_day_window() {
        let (s, e) = (t(9, 0), t(17, 0));
        assert!(!is_within(t(8, 59), s, e));
        assert!(is_within(t(9, 0), s, e)); // inclusive start
        assert!(is_within(t(12, 0), s, e));
        assert!(!is_within(t(17, 0), s, e)); // exclusive end
        assert!(!is_within(t(23, 0), s, e));
    }

    #[test]
    fn window_wraps_midnight() {
        let (s, e) = (t(22, 0), t(7, 0));
        assert!(is_within(t(22, 0), s, e)); // inclusive start
        assert!(is_within(t(23, 59), s, e));
        assert!(is_within(t(0, 0), s, e));
        assert!(is_within(t(6, 59), s, e));
        assert!(!is_within(t(7, 0), s, e)); // exclusive end
        assert!(!is_within(t(12, 0), s, e));
    }

    #[test]
    fn empty_window_is_never_active() {
        let x = t(10, 0);
        assert!(!is_within(x, x, x));
    }

    #[test]
    fn parses_and_rejects_times() {
        assert!(parse_hm("07:30").is_some());
        assert!(parse_hm("23:59").is_some());
        assert!(parse_hm("24:00").is_none());
        assert!(parse_hm("7:5").is_some()); // %H:%M accepts single digits
        assert!(parse_hm("nope").is_none());
    }
}
