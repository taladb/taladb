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

    /// Deliberately a *bound*, not `b >= a`. This is the wall clock: an NTP
    /// step between the two calls can legitimately move it backwards, and a
    /// test that fails when the host adjusts its clock is noise, not a signal.
    /// What actually matters here is that successive calls stay in the same
    /// era — i.e. the unit is milliseconds and the reading is live.
    #[test]
    fn now_ms_readings_are_close_together() {
        let a = now_ms();
        let b = now_ms();
        assert!(
            a.abs_diff(b) < 1_000,
            "{a} and {b} are implausibly far apart"
        );
    }
}
