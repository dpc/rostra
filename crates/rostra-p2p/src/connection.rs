use bincode::{Decode, Encode};
use convi::{CastInto, ExpectFrom};
use iroh_net::endpoint::{RecvStream, SendStream};
use rostra_core::bincode::STD_BINCODE_CONFIG;
use snafu::ResultExt as _;

use crate::{
    ConnectionSnafu, DecodingSnafu, MessageTooLargeSnafu, ReadSnafu, RpcResult, WriteSnafu,
};

pub struct Connection(iroh_net::endpoint::Connection);

/// Max request message size
///
/// Requests are smaller, because they are initiated by an unknown side
pub const MAX_REQUEST_SIZE: u32 = 16 * 1024;

/// Max response message size
pub const MAX_RESPONSE_SIZE: u32 = 32 * 1024 * 1024;

impl From<iroh_net::endpoint::Connection> for Connection {
    fn from(iroh_conn: iroh_net::endpoint::Connection) -> Self {
        Self(iroh_conn)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MsgLen(u32);

impl bincode::Encode for MsgLen {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> core::result::Result<(), bincode::error::EncodeError> {
        bincode::Encode::encode(&self.0.to_be_bytes(), encoder)?;
        Ok(())
    }
}

impl bincode::Decode for MsgLen {
    fn decode<D: bincode::de::Decoder>(
        decoder: &mut D,
    ) -> core::result::Result<Self, bincode::error::DecodeError> {
        Ok(Self(u32::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RpcId(u16);

impl bincode::Encode for RpcId {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> core::result::Result<(), bincode::error::EncodeError> {
        bincode::Encode::encode(&self.0.to_be_bytes(), encoder)?;
        Ok(())
    }
}

impl bincode::Decode for RpcId {
    fn decode<D: bincode::de::Decoder>(
        decoder: &mut D,
    ) -> core::result::Result<Self, bincode::error::DecodeError> {
        Ok(Self(u16::from_be_bytes(bincode::Decode::decode(decoder)?)))
    }
}

impl RpcId {
    const fn const_from(value: u16) -> Self {
        Self(value)
    }

    pub fn to_known(self) -> Option<RpcIdKnown> {
        Some(match self.0 {
            0 => RpcIdKnown::Ping,
            _ => return None,
        })
    }
}

impl From<u16> for RpcId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl From<RpcId> for u16 {
    fn from(value: RpcId) -> Self {
        value.0
    }
}

impl From<RpcIdKnown> for RpcId {
    fn from(value: RpcIdKnown) -> Self {
        (value as u16).into()
    }
}

#[repr(u16)]
#[non_exhaustive]
pub enum RpcIdKnown {
    Ping = 0,
}

impl RpcIdKnown {
    const fn const_into(self) -> RpcId {
        RpcId::const_from(self as u16)
    }
}

pub trait Rpc: RpcRequest {
    const RPC_ID: RpcId;
    type Response: RpcResponse;
}

pub trait RpcMessage: bincode::Encode + bincode::Decode {
    fn decode_whole<const LIMIT: u32>(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        if CastInto::<usize>::cast_into(LIMIT) < bytes.len() {
            return Err(bincode::error::DecodeError::LimitExceeded);
        }

        let (v, consumed_len) = bincode::decode_from_slice(&bytes, STD_BINCODE_CONFIG)?;

        if consumed_len != bytes.len() {
            return Err(bincode::error::DecodeError::Other(
                "Not all bytes consumed during decoding",
            ));
        }

        Ok(v)
    }
}
pub trait RpcRequest: RpcMessage {}
pub trait RpcResponse: RpcMessage {}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Encode, Decode)]
pub struct PingRequest(pub u64);

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Encode, Decode)]
pub struct PingResponse(pub u64);

impl RpcRequest for PingRequest {}
impl RpcMessage for PingRequest {}

impl RpcResponse for PingResponse {}
impl RpcMessage for PingResponse {}

impl Rpc for PingRequest {
    const RPC_ID: RpcId = RpcIdKnown::Ping.const_into();

    type Response = PingResponse;
}

fn rpc_request_to_bytes<R>(v: &R) -> Vec<u8>
where
    R: Rpc,
{
    // No async writer support, sad
    let mut req_bytes = Vec::with_capacity(128);
    // id
    bincode::encode_into_std_write(&R::RPC_ID, &mut req_bytes, STD_BINCODE_CONFIG)
        .expect("Can't fail");
    // length placeholder
    bincode::encode_into_std_write(MsgLen(0), &mut req_bytes, STD_BINCODE_CONFIG)
        .expect("Can't fail");
    // request
    bincode::encode_into_std_write(v, &mut req_bytes, STD_BINCODE_CONFIG).expect("Can't fail");

    let req_body_len = (req_bytes.len() - 6) as u32;
    // Update placeholder
    req_bytes[2..6].copy_from_slice(&req_body_len.to_be_bytes());

    req_bytes
}

#[test]
fn rpc_request_to_bytes_test() {
    assert_eq!(
        rpc_request_to_bytes(&PingRequest(3)),
        [
            0, 0, // id
            0, 0, 0, 1, // len
            3  // ping req
        ]
    );
}

impl Connection {
    pub async fn make_rpc<R: Rpc>(&self, request: &R) -> RpcResult<<R as Rpc>::Response> {
        let (mut send, mut recv) = self.0.open_bi().await.context(ConnectionSnafu)?;

        Self::write_rpc_request(&mut send, request).await?;

        Self::read_message::<MAX_RESPONSE_SIZE, _>(&mut recv).await
    }

    pub async fn write_rpc_request<R: Rpc>(send: &mut SendStream, rpc: &R) -> RpcResult<()> {
        send.write_all(&rpc_request_to_bytes(rpc))
            .await
            .context(WriteSnafu)?;

        Ok(())
    }

    pub async fn read_message<const LIMIT: u32, V: RpcMessage>(
        recv: &mut RecvStream,
    ) -> RpcResult<V> {
        let bytes = Self::read_message_raw::<LIMIT>(recv).await?;

        V::decode_whole::<LIMIT>(&bytes).context(DecodingSnafu)
    }

    pub async fn write_message<R: RpcMessage>(send: &mut SendStream, v: &R) -> RpcResult<()> {
        let mut bytes = Vec::with_capacity(128);

        // len placeholder
        bincode::encode_into_std_write(MsgLen(0), &mut bytes, STD_BINCODE_CONFIG)
            .expect("Can't fail");
        // msg itself
        bincode::encode_into_std_write(&v, &mut bytes, STD_BINCODE_CONFIG)
            .expect("Can't fail encoding to vec");

        let msg_len = bytes.len() - 4;
        bytes[0..4].copy_from_slice(&u32::expect_from(msg_len).to_be_bytes());

        send.write_all(&bytes).await.context(WriteSnafu)?;

        Ok(())
    }

    pub async fn read_message_raw<const LIMIT: u32>(recv: &mut RecvStream) -> RpcResult<Vec<u8>> {
        let mut len_bytes = [0u8; 4];
        recv.read_exact(len_bytes.as_mut_slice())
            .await
            .boxed()
            .context(ReadSnafu)?;

        let len = u32::from_be_bytes(len_bytes);

        if LIMIT < len {
            return MessageTooLargeSnafu { len, limit: LIMIT }.fail();
        }

        let len = len.cast_into();

        let mut resp_bytes = Vec::with_capacity(len);
        resp_bytes.resize(len, 0);

        recv.read_exact(resp_bytes.as_mut_slice())
            .await
            .boxed()
            .context(ReadSnafu)?;

        Ok(resp_bytes)
    }

    pub async fn read_request_raw(recv: &mut RecvStream) -> RpcResult<(RpcId, Vec<u8>)> {
        let mut id_bytes = [0u8; 2];

        recv.read_exact(id_bytes.as_mut_slice())
            .await
            .boxed()
            .context(ReadSnafu)?;

        let id = RpcId::from(u16::from_be_bytes(id_bytes));

        let req = Self::read_message_raw::<MAX_REQUEST_SIZE>(recv).await?;

        Ok((id, req))
    }
}