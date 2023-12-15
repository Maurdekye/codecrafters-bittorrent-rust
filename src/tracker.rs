use crate::{
    bencode::{BencodedValue, Number},
    bterror,
    bytes::{Bytes, PullBytes},
    error::BitTorrentError,
    peer::message::Codec,
};
use std::net::{SocketAddr, SocketAddrV4};

pub mod dht;
pub mod multimodal;

#[derive(Debug)]
pub enum TrackerResponse {
    Success {
        interval: Number,
        peers: Vec<SocketAddr>,
    },
    Failure {
        failure_reason: Bytes,
    },
}

impl From<BencodedValue> for Result<TrackerResponse, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut response) = value {
            if let Some(BencodedValue::Bytes(peers)) = response.pull(b"peers") {
                Ok(TrackerResponse::Success {
                    interval: response
                        .pull(b"interval")
                        .and_then(BencodedValue::into_int)
                        .ok_or(bterror!("Interval missing"))?,
                    peers: <Vec<SocketAddr>>::decode(&mut &peers[..])?,
                })
            } else if let Some(BencodedValue::Bytes(failure_reason)) =
                response.pull(b"failure reason")
            {
                Ok(TrackerResponse::Failure { failure_reason })
            } else {
                Err(bterror!("Invalid tracker response"))
            }
        } else {
            Err(bterror!("Invalid tracker response"))
        }
    }
}

impl Codec for Vec<SocketAddr> {
    type Error = BitTorrentError;

    fn encode(self) -> Result<Vec<u8>, Self::Error> {
        Ok(self
            .into_iter()
            .map(|socket| match socket {
                SocketAddr::V4(socket) => Bytes::from(socket),
                SocketAddr::V6(socket) => Bytes::from(socket),
            })
            .flatten()
            .collect())
    }

    /// only decodes ipv4 sockets
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(bytes
            .chunks(6)
            .map(Bytes::from)
            .map(SocketAddrV4::from)
            .map(SocketAddr::V4)
            .collect())
    }
}
