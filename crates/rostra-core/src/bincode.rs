use bincode::config;

use crate::{MsgLen, NullableShortEventId, ShortEventId, Timestamp};

pub const STANDARD_LIMIT_16M: usize = 0x1_0000_0000;
pub const STD_BINCODE_CONFIG: config::Configuration<
    config::BigEndian,
    config::Varint,
    config::Limit<4294967296>,
> = config::standard()
    .with_limit::<STANDARD_LIMIT_16M>()
    .with_big_endian()
    .with_variable_int_encoding();

impl bincode::Encode for MsgLen {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> core::result::Result<(), bincode::error::EncodeError> {
        bincode::Encode::encode(&self.0.to_be_bytes(), encoder)?;
        Ok(())
    }
}

impl<'de> bincode::BorrowDecode<'de> for MsgLen {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        Ok(Self(u32::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

impl bincode::Decode for MsgLen {
    fn decode<D: bincode::de::Decoder>(
        decoder: &mut D,
    ) -> core::result::Result<Self, bincode::error::DecodeError> {
        Ok(Self(u32::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

impl bincode::Encode for Timestamp {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> core::result::Result<(), bincode::error::EncodeError> {
        bincode::Encode::encode(&self.0.to_be_bytes(), encoder)?;
        Ok(())
    }
}

impl<'de> bincode::BorrowDecode<'de> for Timestamp {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        Ok(Self(u64::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

impl bincode::Decode for Timestamp {
    fn decode<D: bincode::de::Decoder>(
        decoder: &mut D,
    ) -> core::result::Result<Self, bincode::error::DecodeError> {
        Ok(Self(u64::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

impl bincode::Encode for NullableShortEventId {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> core::result::Result<(), bincode::error::EncodeError> {
        if let Some(event_id) = self.0 {
            event_id.0.encode(encoder)
        } else {
            ShortEventId::ZERO.encode(encoder)
        }
    }
}

impl<'de> bincode::BorrowDecode<'de> for NullableShortEventId {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let event_id = ShortEventId::borrow_decode(decoder)?;
        Ok(if event_id == ShortEventId::ZERO {
            Self(None)
        } else {
            Self(Some(event_id))
        })
    }
}

impl bincode::Decode for NullableShortEventId {
    fn decode<D: bincode::de::Decoder>(
        decoder: &mut D,
    ) -> core::result::Result<Self, bincode::error::DecodeError> {
        let event_id = ShortEventId::decode(decoder)?;
        Ok(if event_id == ShortEventId::ZERO {
            Self(None)
        } else {
            Self(Some(event_id))
        })
    }
}
