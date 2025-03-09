use std::fmt;

use rostra_core::array_type_define_minimum;

pub trait ToShort {
    type ShortId;
    fn to_short(self) -> Self::ShortId;
}

impl ToShort for iroh::PublicKey {
    type ShortId = IrohEndpointShortId;

    fn to_short(self) -> Self::ShortId {
        IrohEndpointShortId::from_bytes(self.as_bytes()[..8].try_into().expect("Can't fail"))
    }
}

array_type_define_minimum!(
    #[derive(PartialEq, Eq)]
    struct IrohEndpointShortId,
    8
);

impl fmt::Display for IrohEndpointShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        data_encoding::HEXLOWER.encode_write(&self.0, f)
    }
}
