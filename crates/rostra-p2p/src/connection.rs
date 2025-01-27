use std::fmt;
use std::future::Future;
use std::pin::Pin;

use bao_tree::io::fsm::encode_ranges_validated;
use bao_tree::io::outboard::EmptyOutboard;
use bao_tree::io::round_up_to_chunks;
use bao_tree::{blake3, BaoTree, BlockSize, ByteRanges};
use bincode::{Decode, Encode};
use convi::{CastInto, ExpectFrom};
use iroh::endpoint::{RecvStream, SendStream};
use iroh_io::{TokioStreamReader, TokioStreamWriter};
use rostra_core::bincode::STD_BINCODE_CONFIG;
use rostra_core::event::{EventContent, SignedEvent};
use rostra_core::{ContentHash, MsgLen, ShortEventId};
use rostra_util_error::BoxedErrorResult;
use snafu::{OptionExt as _, ResultExt as _};

use crate::{
    ConnectionSnafu, DecodingBaoSnafu, DecodingSnafu, EncodingBaoSnafu, FailedSnafu,
    MessageTooLargeSnafu, ReadSnafu, RpcResult, TrailerSnafu, WriteSnafu,
};

pub struct Connection(iroh::endpoint::Connection);

/// Max request message size
///
/// Requests are smaller, because they are initiated by an unknown side
pub const MAX_REQUEST_SIZE: u32 = 4 * 1024;

/// Max response message size
pub const MAX_RESPONSE_SIZE: u32 = 32 * 1024 * 1024;

impl From<iroh::endpoint::Connection> for Connection {
    fn from(iroh_conn: iroh::endpoint::Connection) -> Self {
        Self(iroh_conn)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl fmt::Display for RpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl RpcId {
    pub const PING: Self = Self(0);
    pub const FEED_EVENT: Self = Self(1);
    pub const GET_EVENT: Self = Self(2);
    pub const GET_EVENT_CONTENT: Self = Self(3);
    pub const WAIT_HEAD_UPDATE: Self = Self(4);
    pub const fn const_from(value: u16) -> Self {
        Self(value)
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

pub trait Rpc: RpcRequest {
    const RPC_ID: RpcId;
    type Response: RpcResponse;
}

pub trait RpcMessage: bincode::Encode + bincode::Decode {
    fn decode_whole<const LIMIT: u32>(bytes: &[u8]) -> Result<Self, bincode::error::DecodeError> {
        if CastInto::<usize>::cast_into(LIMIT) < bytes.len() {
            return Err(bincode::error::DecodeError::LimitExceeded);
        }

        let (v, consumed_len) = bincode::decode_from_slice(bytes, STD_BINCODE_CONFIG)?;

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

macro_rules! define_rpc {
    ($id:expr, $req:ident, $req_body:item, $resp:ident, $resp_body:item) => {

        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[derive(Encode, Decode, Clone, Debug)]
        $req_body

        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        #[derive(Encode, Decode, Clone, Debug)]
        $resp_body

        impl RpcRequest for $req {}
        impl RpcMessage for $req {}

        impl RpcResponse for $resp {}
        impl RpcMessage for $resp {}

        impl Rpc for $req {
            const RPC_ID: RpcId = $id;

            type Response = $resp;
        }
    }
}

define_rpc!(
    RpcId::PING,
    PingRequest,
    pub struct PingRequest(pub u64);,
    PingResponse,
    pub struct PingResponse(pub u64);
);

define_rpc!(
    RpcId::FEED_EVENT,
    FeedEventRequest,
    pub struct FeedEventRequest(pub SignedEvent);,
    FeedEventResponse,
    pub struct FeedEventResponse;
);

define_rpc!(
    RpcId::GET_EVENT,
    GetEventRequest,
    pub struct GetEventRequest(pub ShortEventId);,
    GetEventResponse,
    pub struct GetEventResponse(pub Option<SignedEvent>);
);

define_rpc!(
    RpcId::WAIT_HEAD_UPDATE,
    WaitHeadUpdateRequest,
    pub struct WaitHeadUpdateRequest(pub ShortEventId);,
    WaitHeadUpdateResponse,
    pub struct WaitHeadUpdateResponse(pub ShortEventId);
);

impl GetEventRequest {
    pub const NOT_FOUND: u8 = 1;
}

define_rpc!(
    RpcId::GET_EVENT_CONTENT,
    GetEventContentRequest,
    pub struct GetEventContentRequest(pub ShortEventId);,
    GetEventContentResponse,
    pub struct GetEventContentResponse(pub bool);
);

impl FeedEventResponse {
    pub const RETURN_CODE_ALREADY_HAVE: u8 = 1;
    pub const RETURN_CODE_ID_MISMATCH: u8 = 2;
    pub const RETURN_CODE_TOO_LARGE: u8 = 3;
}

fn rpc_request_to_bytes<R>(v: &R) -> Vec<u8>
where
    R: Rpc,
{
    // No async writer support, sad
    let mut req_bytes = Vec::with_capacity(128);
    // id
    bincode::encode_into_std_write(R::RPC_ID, &mut req_bytes, STD_BINCODE_CONFIG)
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

        Self::read_success_error_code(&mut recv).await?;

        Self::read_message::<MAX_RESPONSE_SIZE, _>(&mut recv).await
    }

    /// Send an RPC that has "trailer data"
    ///
    /// The sequence:
    ///
    /// * request body
    /// * read success
    /// * read response
    /// * send extra data
    /// * read response
    pub async fn make_rpc_with_extra_data_send<R: Rpc, F>(
        &self,
        request: &R,
        extra_data_f: F,
    ) -> RpcResult<<R as Rpc>::Response>
    where
        F: for<'s> Fn(
            &'s mut SendStream,
        )
            -> Pin<Box<dyn Future<Output = BoxedErrorResult<()>> + 's + Send + Sync>>,
    {
        let (mut send, mut recv) = self.0.open_bi().await.context(ConnectionSnafu)?;

        Self::write_rpc_request(&mut send, request).await?;

        Self::read_success_error_code(&mut recv).await?;

        let resp = Self::read_message::<MAX_RESPONSE_SIZE, _>(&mut recv).await;

        (extra_data_f)(&mut send).await.context(TrailerSnafu)?;

        resp
    }

    pub async fn make_rpc_with_extra_data_recv<R: Rpc, F, T>(
        &self,
        request: &R,
        extra_data_f: F,
    ) -> RpcResult<(<R as Rpc>::Response, T)>
    where
        F: for<'s> Fn(
            &'s mut RecvStream,
            &<R as Rpc>::Response,
        )
            -> Pin<Box<dyn Future<Output = BoxedErrorResult<T>> + 's + Send + Sync>>,
    {
        let (mut send, mut recv) = self.0.open_bi().await.context(ConnectionSnafu)?;

        Self::write_rpc_request(&mut send, request).await?;

        Self::read_success_error_code(&mut recv).await?;

        let resp = Self::read_message::<MAX_RESPONSE_SIZE, _>(&mut recv).await?;

        let extra = (extra_data_f)(&mut recv, &resp)
            .await
            .context(TrailerSnafu)?;

        Ok((resp, extra))
    }

    pub async fn write_rpc_request<R: Rpc>(send: &mut SendStream, rpc: &R) -> RpcResult<()> {
        send.write_all(&rpc_request_to_bytes(rpc))
            .await
            .context(WriteSnafu)?;

        Ok(())
    }

    pub async fn read_success_error_code(recv: &mut RecvStream) -> RpcResult<u8> {
        let mut res = [0u8; 1];
        recv.read_exact(&mut res).await.boxed().context(ReadSnafu)?;

        if res[0] != 0 {
            return FailedSnafu {
                return_code: res[0],
            }
            .fail();
        }

        Ok(res[0])
    }

    pub async fn write_success_return_code(send: &mut SendStream) -> RpcResult<()> {
        send.write_all(&[0u8]).await.context(WriteSnafu)
    }

    pub async fn write_return_code(send: &mut SendStream, code: impl Into<u8>) -> RpcResult<()> {
        send.write_all(&[code.into()]).await.context(WriteSnafu)
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
        bincode::encode_into_std_write(v, &mut bytes, STD_BINCODE_CONFIG)
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

        let mut resp_bytes = vec![0u8; len];

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

    pub async fn write_bao_content(
        send: &mut SendStream,
        bytes: &[u8],
        hash: ContentHash,
    ) -> RpcResult<()> {
        let bytes_len = u32::try_from(bytes.len())
            .ok()
            .context(MessageTooLargeSnafu {
                len: u32::MAX,
                limit: u32::MAX,
            })?;
        /// Use a block size of 16 KiB, a good default
        /// for most cases
        pub(crate) const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);

        let mut ob = EmptyOutboard {
            tree: BaoTree::new(bytes_len.into(), BLOCK_SIZE),
            root: blake3::Hash::from_bytes(hash.into()),
        };

        // Encode the first 100000 bytes of the file
        let ranges = ByteRanges::from(0u64..bytes_len.into());
        let ranges = round_up_to_chunks(&ranges);
        encode_ranges_validated(bytes, &mut ob, &ranges, TokioStreamWriter(send))
            .await
            .context(EncodingBaoSnafu)?;

        Ok(())
    }

    pub async fn read_bao_content(
        read: &mut RecvStream,
        len: u32,
        hash: ContentHash,
    ) -> RpcResult<Vec<u8>> {
        const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);
        let ranges = ByteRanges::from(0u64..len.into());
        let chunk_ranges = round_up_to_chunks(&ranges);
        let mut decoded = Vec::with_capacity(len.cast_into());
        let mut ob = EmptyOutboard {
            tree: bao_tree::BaoTree::new(len.into(), BLOCK_SIZE),
            root: blake3::Hash::from_bytes(hash.into()),
        };
        bao_tree::io::fsm::decode_ranges(
            TokioStreamReader(read),
            chunk_ranges,
            &mut decoded,
            &mut ob,
        )
        .await
        .context(DecodingBaoSnafu)?;

        Ok(decoded)
    }
}

impl Connection {
    pub async fn get_event(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> RpcResult<Option<SignedEvent>> {
        let event = self.make_rpc(&GetEventRequest(event_id.into())).await?;

        Ok(event.0)
    }

    pub async fn get_event_content(
        &self,
        event_id: impl Into<ShortEventId>,
        len: u32,
        hash: ContentHash,
    ) -> RpcResult<Option<EventContent>> {
        let event_id = event_id.into();

        let (_resp, content) = self
            .make_rpc_with_extra_data_recv(&GetEventContentRequest(event_id), |recv, resp| {
                let resp = resp.to_owned();
                Box::pin(async move {
                    if resp.0 {
                        Ok(Some(EventContent::new(
                            Connection::read_bao_content(recv, len, hash).await?,
                        )))
                    } else {
                        Ok(None)
                    }
                })
            })
            .await?;
        Ok(content)
    }
}
