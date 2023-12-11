use std::iter::{empty, once};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::bencode::encode::bencode_value;
use crate::{
    bencode::decode::consume_bencoded_value,
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    util::{decode_bitfield_be, encode_bitfield_be},
};

pub trait EncodeDecode {
    type Error;

    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>
    where
        Self: Sized;
}

#[derive(Debug)]
pub enum PeerMessage {
    // base protocol messages
    Keepalive,               // empty id
    Choke,                   // 0
    Unchoke,                 // 1
    Interested,              // 2
    NotInterested,           // 3
    Have(u32),               // 4
    Bitfield(Vec<bool>),     // 5
    Request(RequestMessage), // 6
    Piece(PieceMessage),     // 7
    Cancel(RequestMessage),  // 8

    // extension messages
    Port(u16),                     // 9
    HaveAll,                       // 14
    HaveNone,                      // 15
    RejectRequest(RequestMessage), // 16
    AllowFast(u32),                // 17
    Extension(ExtensionMessage),   // 20
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RequestMessage {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PieceMessage {
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ExtensionMessage {
    Handshake(ExtensionHandshake),
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct ExtensionHandshake {
    #[serde(rename = "m")]
    pub messages: Option<Map<String, Value>>,
    #[serde(rename = "p")]
    pub port: Option<u16>,
    #[serde(rename = "v")]
    pub version: Option<String>,
    pub yourip: Option<String>,
    pub ipv6: Option<String>,
    pub ipv4: Option<String>,
    pub reqq: Option<Number>,
}

impl ExtensionMessage {
    pub fn decode(bytes: &[u8]) -> Result<Self, BitTorrentError> {
        match bytes[0] {
            0 => Ok(Self::Handshake(serde_json::from_value(
                consume_bencoded_value(&mut &bytes[1..])?,
            )?)),
            byte => Err(bterror!("Invalid extension message id: {byte}")),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, BitTorrentError> {
        match self {
            Self::Handshake(handshake) => bencode_value(serde_json::to_value(handshake)?),
        }
    }
}

impl EncodeDecode for PeerMessage {
    type Error = BitTorrentError;

    /// Decode a peer message from a byte array.
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error> {
        match bytes.get(0) {
            None => Ok(Self::Keepalive),
            Some(0) => Ok(Self::Choke),
            Some(1) => Ok(Self::Unchoke),
            Some(2) => Ok(Self::Interested),
            Some(3) => Ok(Self::NotInterested),
            Some(4) => Ok(Self::Have(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(5) => Ok(Self::Bitfield(
                bytes[1..]
                    .iter()
                    .copied()
                    .flat_map(decode_bitfield_be)
                    .collect(),
            )),
            Some(6) => Ok(Self::Request(RequestMessage::decode(&bytes[1..])?)),
            Some(7) => Ok(Self::Piece(PieceMessage::decode(&bytes[1..])?)),
            Some(8) => Ok(Self::Cancel(RequestMessage::decode(&bytes[1..])?)),
            Some(9) => Ok(Self::Port(u16::from_be_bytes(
                bytes[1..3].try_into().unwrap(),
            ))),
            Some(14) => Ok(Self::HaveAll),
            Some(15) => Ok(Self::HaveNone),
            Some(16) => Ok(Self::RejectRequest(RequestMessage::decode(&bytes[1..])?)),
            Some(17) => Ok(Self::AllowFast(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(20) => Ok(Self::Extension(ExtensionMessage::decode(&bytes[1..])?)),
            Some(byte) => Err(bterror!("Invalid peer message: {byte}")),
        }
    }

    /// Encode a peer message into a byte array.
    fn encode(&self) -> Result<Vec<u8>, Self::Error> {
        // dbg!(&self);
        let base_message: Vec<u8> = match self {
            Self::Keepalive => vec![],
            Self::Choke => vec![0],
            Self::Unchoke => vec![1],
            Self::Interested => vec![2],
            Self::NotInterested => vec![3],
            Self::Have(index) => once(4).chain(index.to_be_bytes()).collect(),
            Self::Bitfield(bitfield) => once(5)
                .chain(
                    bitfield
                        .chunks(8)
                        .map(|bits| encode_bitfield_be(bits.try_into().unwrap())),
                )
                .collect(),
            Self::Request(req) => once(6).chain(req.encode()?).collect(),
            Self::Piece(piece) => once(7).chain(piece.encode()?).collect(),
            Self::Cancel(cancel) => once(8).chain(cancel.encode()?).collect(),
            Self::Port(port) => once(9).chain(port.to_be_bytes()).collect(),
            Self::HaveAll => vec![14],
            Self::HaveNone => vec![15],
            Self::RejectRequest(req) => once(16).chain(req.encode()?).collect(),
            Self::AllowFast(index) => once(17).chain(index.to_be_bytes()).collect(),
            Self::Extension(ext) => once(20).chain(ext.encode()?).collect(),
        };
        let length = base_message.len() as u32;
        Ok(length
            .to_be_bytes()
            .into_iter()
            .chain(base_message)
            .collect())
    }
}

impl EncodeDecode for RequestMessage {
    type Error = BitTorrentError;

    fn encode(&self) -> Result<Vec<u8>, Self::Error> {
        Ok(empty()
            .chain(self.index.to_be_bytes())
            .chain(self.begin.to_be_bytes())
            .chain(self.length.to_be_bytes())
            .collect())
    }

    fn decode(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self {
            index: u32::from_be_bytes(
                bytes
                    .get(0..4)
                    .ok_or(bterror!("Insufficient bytes"))?
                    .try_into()?,
            ),
            begin: u32::from_be_bytes(
                bytes
                    .get(4..8)
                    .ok_or(bterror!("Insufficient bytes"))?
                    .try_into()?,
            ),
            length: u32::from_be_bytes(
                bytes
                    .get(8..12)
                    .ok_or(bterror!("Insufficient bytes"))?
                    .try_into()?,
            ),
        })
    }
}

impl EncodeDecode for PieceMessage {
    type Error = BitTorrentError;

    fn encode(&self) -> Result<Vec<u8>, Self::Error> {
        Ok(empty()
            .chain(self.index.to_be_bytes())
            .chain(self.begin.to_be_bytes())
            .chain(self.block.clone())
            .collect())
    }

    fn decode(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self {
            index: u32::from_be_bytes(
                bytes
                    .get(0..4)
                    .ok_or(bterror!("Insufficient bytes"))?
                    .try_into()?,
            ),
            begin: u32::from_be_bytes(
                bytes
                    .get(4..8)
                    .ok_or(bterror!("Insufficient bytes"))?
                    .try_into()?,
            ),
            block: bytes
                .get(8..)
                .ok_or(bterror!("Insufficient bytes"))?
                .to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct HandshakeMessage {
    pub info_hash: [u8; 20],
    pub peer_id: Vec<u8>,
}

impl HandshakeMessage {
    /// Create a new HandshakeMessage.
    pub fn new(meta_info: &MetaInfo, peer_id: &str) -> Result<HandshakeMessage, BitTorrentError> {
        Ok(HandshakeMessage {
            info_hash: meta_info.info_hash()?,
            peer_id: peer_id.as_bytes().to_vec(),
        })
    }

    /// Decode a handshake message from a byte array.
    pub fn decode(bytes: &[u8]) -> Result<HandshakeMessage, BitTorrentError> {
        Ok(HandshakeMessage {
            info_hash: bytes[28..48].try_into().unwrap(),
            peer_id: bytes[48..68].into(),
        })
    }

    /// Encode a handshake message into a byte array.
    pub fn encode(&self) -> Vec<u8> {
        [19].iter()
            .chain(b"BitTorrent protocol".into_iter())
            .chain(&[
                0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00000000, 0b00011000, 0b00000000,
                0b00000101,
            ])
            .chain(&self.info_hash)
            .chain(&self.peer_id)
            .copied()
            .collect()
    }
}
