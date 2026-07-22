use std::time::{SystemTime, UNIX_EPOCH};

use rostra_core::Timestamp;

const INVALID_DATE: &str = "Invalid date";

/// Format a valid timestamp as UTC ISO 8601, or return `Invalid date`.
///
/// Valid output includes nanosecond precision, for example
/// `2024-07-01T00:00:00.000000000Z`.
pub fn format_timestamp_iso(timestamp: Timestamp) -> String {
    let Some(datetime) = timestamp_to_datetime(timestamp) else {
        return INVALID_DATE.to_string();
    };

    datetime
        .format(&time::format_description::well_known::Iso8601::DEFAULT)
        .expect("ISO 8601 formatting can't fail for valid OffsetDateTime")
}

pub fn format_timestamp(timestamp: Timestamp) -> String {
    if timestamp_to_datetime(timestamp).is_none() {
        return INVALID_DATE.to_string();
    }

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
    let Some(datetime) = timestamp_to_datetime(timestamp) else {
        return INVALID_DATE.to_string();
    };

    format!(
        "{}/{}/{}",
        datetime.month() as u8,
        datetime.day(),
        datetime.year()
    )
}

fn timestamp_to_datetime(timestamp: Timestamp) -> Option<time::OffsetDateTime> {
    i64::try_from(timestamp.as_u64())
        .ok()
        .and_then(|timestamp| time::OffsetDateTime::from_unix_timestamp(timestamp).ok())
}

#[cfg(test)]
mod tests {
    use rostra_core::Timestamp;

    use super::{INVALID_DATE, format_timestamp_date, format_timestamp_iso};

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
            (253_402_300_800, INVALID_DATE),
            (i64::MAX as u64, INVALID_DATE),
            (u64::MAX, INVALID_DATE),
        ];

        for (timestamp, expected) in cases {
            let formatted = format_timestamp_date(Timestamp::from(timestamp));
            assert_eq!(formatted, expected, "timestamp {timestamp}");
        }
    }

    #[test]
    fn formats_iso_timestamps_in_utc() {
        assert_eq!(
            format_timestamp_iso(Timestamp::from(1_719_792_000)),
            "2024-07-01T00:00:00.000000000Z"
        );
        assert_eq!(format_timestamp_iso(Timestamp::MAX), "Invalid date");
    }

    #[test]
    fn unrepresentable_timestamp_is_invalid() {
        assert_eq!(super::format_timestamp(Timestamp::MAX), "Invalid date");
    }
}
