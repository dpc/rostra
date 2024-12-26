/// Meh alias
pub type IrohError = anyhow::Error;
pub type IrohResult<T> = anyhow::Result<T>;

pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type BoxedErrorResult<T> = std::result::Result<T, BoxedError>;
