//! Wall-clock helpers.
//!
//! `web-time` re-exports `std::time` on native targets and backs onto
//! `js_sys::Date` under `wasm32`, so the same call works in the browser.

use web_time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time in milliseconds since the Unix epoch.
///
/// Saturates to `0` rather than panicking if the host clock is set before the
/// epoch — a nonsensical timestamp is preferable to taking down a write.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::now_ms;

    #[test]
    fn now_ms_is_after_2020() {
        // 2020-01-01T00:00:00Z. Guards against a unit slip (secs vs millis).
        assert!(now_ms() > 1_577_836_800_000);
    }

    #[test]
    fn now_ms_is_monotonic_across_calls() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a);
    }
}
