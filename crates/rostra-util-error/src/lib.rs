mod fmt;

pub use self::fmt::*;

pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type BoxedErrorResult<T> = std::result::Result<T, BoxedError>;
