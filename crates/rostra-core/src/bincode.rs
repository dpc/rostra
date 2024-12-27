use bincode::config;

pub const STANDARD_LIMIT_16M: usize = 0x1_0000_0000;
pub const STD_BINCODE_CONFIG: config::Configuration<
    config::BigEndian,
    config::Fixint,
    config::Limit<4294967296>,
> = config::standard()
    .with_limit::<STANDARD_LIMIT_16M>()
    .with_big_endian()
    .with_fixed_int_encoding();
