#[cfg(not(target_family = "wasm"))]
pub fn unix_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_millis().min(i64::MAX as u128) as i64
}

/// `SystemTime::now` aborts on `wasm32-unknown-unknown`; browser and mobile
/// WebView builds must read the clock through the JS host instead.
#[cfg(target_family = "wasm")]
pub fn unix_timestamp_ms() -> i64 {
    js_sys::Date::now().max(0.0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_positive() {
        assert!(unix_timestamp_ms() > 0);
    }
}
