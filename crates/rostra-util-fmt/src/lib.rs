use std::fmt;

pub struct FmtOption<'r, O>(pub Option<&'r O>);

impl<O> fmt::Display for FmtOption<'_, O>
where
    O: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(o) => o.fmt(f),
            None => f.write_str("-"),
        }
    }
}

pub trait AsFmtOption {
    type Fmt: fmt::Display;
    fn fmt_option(self) -> Self::Fmt;
}

impl<'e, O> AsFmtOption for &'e Option<O>
where
    O: fmt::Display,
{
    type Fmt = FmtOption<'e, O>;

    fn fmt_option(self) -> Self::Fmt {
        FmtOption(self.as_ref())
    }
}

/// Format a byte count as human-readable string (e.g., "1.5 KB", "3.2 MB").
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if GB <= bytes {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if MB <= bytes {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if KB <= bytes {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Format a duration (in seconds) as a relative time string (e.g., "5m", "2h",
/// "3d").
///
/// For durations over 30 days, returns None to indicate the caller should
/// format as an absolute date instead.
pub fn format_duration_relative(seconds: u64) -> Option<String> {
    if seconds < 60 {
        Some(format!("{seconds}s"))
    } else if seconds < 3600 {
        Some(format!("{}m", seconds / 60))
    } else if seconds < 86400 {
        Some(format!("{}h", seconds / 3600))
    } else if seconds < 2592000 {
        // 30 days
        Some(format!("{}d", seconds / 86400))
    } else {
        None
    }
}
