use std::{error, fmt, result};

pub struct FmtCompactError<'e, E>(pub &'e E);

impl<'e, E> fmt::Display for FmtCompactError<'e, E>
where
    E: error::Error,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut error = Some(self.0 as &dyn error::Error);

        while let Some(err) = error {
            f.write_fmt(format_args!("{err}"))?;
            error = err.source();
            if error.is_some() {
                f.write_str(": ")?;
            }
        }

        Ok(())
    }
}

pub struct FmtCompactResult<'r, O, E>(pub &'r result::Result<O, E>);

impl<'r, O, E> fmt::Display for FmtCompactResult<'r, O, E>
where
    E: error::Error,
    O: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Ok(o) => o.fmt(f),
            Err(e) => FmtCompactError(e).fmt(f),
        }
    }
}
pub trait FmtCompact {
    type Report: fmt::Display;
    fn fmt_compact(self) -> Self::Report;
}

impl<'e, E> FmtCompact for &'e E
where
    E: error::Error,
{
    type Report = FmtCompactError<'e, E>;

    fn fmt_compact(self) -> Self::Report {
        FmtCompactError(self)
    }
}
