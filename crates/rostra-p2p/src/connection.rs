use bincode::{Decode, Encode};
use convi::CastInto as _;
use rostra_core::bincode::STD_BINCODE_CONFIG;
use snafu::ResultExt as _;

use crate::{
    ConnectionSnafu, ReadSnafu, ResponseDecodingSnafu, ResponseTooLargeSnafu, RpcResult, WriteSnafu,
};

pub struct Connection(iroh_net::endpoint::Connection);

/// Max request size
///
/// Requests are smaller, because they are initiated by an unknown side
const MAX_REQUEST_SIZE: u32 = 16 * 1024;

const MAX_RESPONSE_SIZE: u32 = 32 * 1024 * 1024;

impl From<iroh_net::endpoint::Connection> for Connection {
    fn from(iroh_conn: iroh_net::endpoint::Connection) -> Self {
        Self(iroh_conn)
    }
}

#[repr(u16)]
#[derive(Encode, Decode)]
pub enum RpcId {
    Ping,
}

pub trait RpcRequest: bincode::Encode {
    const RPC_ID: RpcId;
    type Response: bincode::Decode;
}

#[derive(Encode, Decode)]
pub struct PingRequest(u64);

#[derive(Encode, Decode)]
pub struct PingResponse(u64);

impl RpcRequest for PingRequest {
    const RPC_ID: RpcId = RpcId::Ping;

    type Response = PingResponse;
}

fn write_framed_request<R>(v: &R) -> Vec<u8>
where
    R: RpcRequest,
{
    // No async writer support, sad
    let mut req_bytes = Vec::with_capacity(128);
    // id
    bincode::encode_into_std_write(R::RPC_ID, &mut req_bytes, STD_BINCODE_CONFIG)
        .expect("Can't fail");
    // length placeholder
    bincode::encode_into_std_write(0u32, &mut req_bytes, STD_BINCODE_CONFIG).expect("Can't fail");
    // request
    bincode::encode_into_std_write(v, &mut req_bytes, STD_BINCODE_CONFIG).expect("Can't fail");

    let req_body_len = (req_bytes.len() - 4) as u32;
    // Update placeholder
    req_bytes[4..8].copy_from_slice(&req_body_len.to_be_bytes());

    req_bytes
}

impl Connection {
    pub async fn rpc<R: RpcRequest>(&self, rpc: &R) -> RpcResult<<R as RpcRequest>::Response> {
        let (mut send, mut recv) = self.0.open_bi().await.context(ConnectionSnafu)?;

        send.write_all(&write_framed_request(rpc))
            .await
            .context(WriteSnafu)?;

        let mut len_bytes = [0u8; 4];
        recv.read_exact(len_bytes.as_mut_slice())
            .await
            .boxed()
            .context(ReadSnafu)?;

        let len = u32::from_be_bytes(len_bytes);

        if MAX_RESPONSE_SIZE < len {
            return ResponseTooLargeSnafu { len }.fail();
        }

        let len = len.cast_into();

        let mut resp_bytes = Vec::with_capacity(len);
        resp_bytes.resize(len, 0);

        recv.read_exact(resp_bytes.as_mut_slice())
            .await
            .boxed()
            .context(ReadSnafu)?;

        Ok(bincode::decode_from_slice(&resp_bytes, STD_BINCODE_CONFIG)
            .context(ResponseDecodingSnafu)?
            .0)
    }
}
