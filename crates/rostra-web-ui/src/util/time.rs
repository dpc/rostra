use std::time::{SystemTime, UNIX_EPOCH};

use rostra_core::Timestamp;

/// Format a timestamp as ISO 8601 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_timestamp_iso(timestamp: Timestamp) -> String {
    let dt = time::OffsetDateTime::from_unix_timestamp(timestamp.as_u64() as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    dt.format(&time::format_description::well_known::Iso8601::DEFAULT)
        .expect("ISO 8601 formatting can't fail for valid OffsetDateTime")
}

pub fn format_timestamp(timestamp: Timestamp) -> String {
    let now = SystemTime::now();
    let duration_since = UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(timestamp.as_u64()))
        .and_then(|system_time| now.duration_since(system_time).ok())
        .unwrap_or_default();

    let seconds = duration_since.as_secs();

    if let Some(relative) = rostra_util_fmt::format_duration_relative(seconds) {
        relative
    } else {
        format_timestamp_date(timestamp)
    }
}

fn format_timestamp_date(timestamp: Timestamp) -> String {
    let dt = i64::try_from(timestamp.as_u64())
        .ok()
        .and_then(|timestamp| time::OffsetDateTime::from_unix_timestamp(timestamp).ok())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

    format!("{}/{}/{}", dt.month() as u8, dt.day(), dt.year())
}

#[cfg(test)]
mod tests {
    use rostra_core::Timestamp;

    use super::format_timestamp_date;

    #[test]
    fn formats_calendar_dates_in_utc() {
        let cases = [
            (0, "1/1/1970"),
            (951_782_400, "2/29/2000"),
            (1_612_051_200, "1/31/2021"),
            (1_614_556_800, "3/1/2021"),
            (1_640_908_800, "12/31/2021"),
            (1_640_995_200, "1/1/2022"),
            (1_719_792_000, "7/1/2024"),
            (253_402_300_799, "12/31/9999"),
            (253_402_300_800, "1/1/1970"),
            (i64::MAX as u64, "1/1/1970"),
            (u64::MAX, "1/1/1970"),
        ];

        for (timestamp, expected) in cases {
            let formatted = format_timestamp_date(Timestamp::from(timestamp));
            assert_eq!(formatted, expected, "timestamp {timestamp}");

            let month = formatted
                .split_once('/')
                .expect("date contains a month separator")
                .0
                .parse::<u8>()
                .expect("month is numeric");
            assert!((1..=12).contains(&month), "timestamp {timestamp}");
        }
    }

    #[test]
    fn unrepresentable_system_timestamp_is_treated_as_future() {
        assert_eq!(super::format_timestamp(Timestamp::MAX), "0s");
    }
}
