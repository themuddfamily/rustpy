//! Regional presentation only: wire timestamps and source spelling are untouched.
use chrono::{Datelike, TimeZone, Timelike};
use std::sync::{OnceLock, RwLock};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Regional {
    pub locale: String,
    pub iso: bool,
    pub clock_12_hour: bool,
}
fn state() -> &'static RwLock<Regional> {
    static STATE: OnceLock<RwLock<Regional>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(Regional::default()))
}
pub fn configure(locale: &str, iso: bool, clock_12_hour: bool) {
    let next = Regional {
        locale: locale.into(),
        iso,
        clock_12_hour,
    };
    if let Ok(mut current) = state().write() {
        if *current != next {
            *current = next;
        }
    }
}
pub fn current() -> Regional {
    let mut value = state().read().map(|v| v.clone()).unwrap_or_default();
    if value.locale.is_empty() {
        value.locale = crate::locale();
    }
    value
}
pub fn date_in(region: &Regional, year: i32, month: u32, day: u32) -> String {
    if region.iso {
        return format!("{year:04}-{month:02}-{day:02}");
    }
    let locale = region.locale.replace('_', "-").to_lowercase();
    if locale.starts_with("ja") {
        format!("{year:04}/{month:02}/{day:02}")
    } else if locale.starts_with("zh") {
        format!("{year}年{month}月{day}日")
    } else if locale.starts_with("ko") {
        format!("{year}. {month}. {day}.")
    } else if locale == "en-us" || locale == "en" {
        format!("{month:02}/{day:02}/{year:04}")
    } else if locale.starts_with("de") {
        format!("{day:02}.{month:02}.{year:04}")
    } else if locale.starts_with("en-ca") {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        format!("{day:02}/{month:02}/{year:04}")
    }
}
pub fn time_in(region: &Regional, hour: u32, minute: u32, second: u32) -> String {
    if region.clock_12_hour {
        let period = if hour < 12 { "AM" } else { "PM" };
        let hour = if hour.is_multiple_of(12) {
            12
        } else {
            hour % 12
        };
        format!("{hour:02}:{minute:02}:{second:02} {period}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}")
    }
}
pub fn timestamp_millis(ms: i64) -> String {
    let Some(time) = chrono::Local.timestamp_millis_opt(ms).single() else {
        return ms.to_string();
    };
    let region = current();
    format!(
        "{} {}",
        date_in(&region, time.year(), time.month(), time.day()),
        time_in(&region, time.hour(), time.minute(), time.second())
    )
}
pub fn timestamp_seconds(seconds: i64) -> String {
    seconds
        .checked_mul(1000)
        .map(timestamp_millis)
        .unwrap_or_else(|| seconds.to_string())
}
pub fn time_millis(ms: i64) -> String {
    let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() else {
        return ms.to_string();
    };
    time_in(&current(), t.hour(), t.minute(), t.second())
}
pub fn clock_now() -> String {
    let t = chrono::Local::now();
    time_in(&current(), t.hour(), t.minute(), t.second())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regional_variants_and_date_order_are_explicit() {
        for (locale, expected) in [
            ("en-US", "10/15/2026"),
            ("en-GB", "15/10/2026"),
            ("zh-CN", "2026年10月15日"),
            ("ja", "2026/10/15"),
            ("ko", "2026. 10. 15."),
        ] {
            assert_eq!(
                date_in(
                    &Regional {
                        locale: locale.into(),
                        ..Default::default()
                    },
                    2026,
                    10,
                    15
                ),
                expected
            );
        }
        assert_eq!(
            date_in(
                &Regional {
                    iso: true,
                    ..Default::default()
                },
                2026,
                10,
                15
            ),
            "2026-10-15"
        );
    }
    #[test]
    fn clock_mode_is_independent_of_translation() {
        let r = Regional {
            clock_12_hour: true,
            ..Default::default()
        };
        assert_eq!(time_in(&r, 0, 5, 3), "12:05:03 AM");
        assert_eq!(time_in(&r, 13, 5, 3), "01:05:03 PM");
    }
}
