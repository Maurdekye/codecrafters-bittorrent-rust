use serde::{Serialize, Deserialize};

use crate::{error::BitTorrentError, util::{decode_bitfield_be, encode_bitfield_be}, bterror, info::MetaInfo};


#[derive(Debug)]
pub enum PeerMessage {
    Keepalive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(BitFieldMessage),
    Request(RequestMessage),
    Piece(PieceMessage),
    Cancel(CancelMessage),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BitFieldMessage {
    pub bitfield: Vec<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RequestMessage {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CancelMessage {
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

impl PeerMessage {
    /// Decode a peer message from a byte array.
    pub fn decode(bytes: &[u8]) -> Result<Self, BitTorrentError> {
        match bytes.get(0) {
            None => Ok(Self::Keepalive),
            Some(0u8) => Ok(Self::Choke),
            Some(1u8) => Ok(Self::Unchoke),
            Some(2u8) => Ok(Self::Interested),
            Some(3u8) => Ok(Self::NotInterested),
            Some(4u8) => Ok(Self::Have(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(5u8) => Ok(Self::Bitfield(BitFieldMessage {
                bitfield: bytes[1..]
                    .iter()
                    .copied()
                    .flat_map(decode_bitfield_be)
                    .collect(),
            })),
            Some(6u8) => Ok(Self::Request(RequestMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                length: u32::from_be_bytes(bytes[9..13].try_into().unwrap()),
            })),
            Some(7u8) => Ok(Self::Piece(PieceMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                block: bytes[9..].to_vec(),
            })),
            Some(8u8) => Ok(Self::Cancel(CancelMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                length: u32::from_be_bytes(bytes[9..13].try_into().unwrap()),
            })),
            Some(byte) => Err(bterror!("Invalid peer message: {byte}")),
        }
    }

    /// Encode a peer message into a byte array.
    pub fn encode(&self) -> Result<Vec<u8>, BitTorrentError> {
        let base_message: Vec<u8> = match self {
            Self::Keepalive => vec![],
            Self::Choke => vec![0],
            Self::Unchoke => vec![1],
            Self::Interested => vec![2],
            Self::NotInterested => vec![3],
            Self::Have(index) => vec![4].into_iter().chain(index.to_be_bytes()).collect(),
            Self::Bitfield(bitfield) => vec![5]
                .into_iter()
                .chain(
                    bitfield
                        .bitfield
                        .chunks(8)
                        .map(|bits| encode_bitfield_be(bits.try_into().unwrap())),
                )
                .collect(),
            Self::Request(req) => vec![6]
                .into_iter()
                .chain(req.index.to_be_bytes())
                .chain(req.begin.to_be_bytes())
                .chain(req.length.to_be_bytes())
                .collect(),
            Self::Piece(piece) => vec![7]
                .into_iter()
                .chain(piece.index.to_be_bytes())
                .chain(piece.begin.to_be_bytes())
                .chain(piece.block.clone())
                .collect(),
            Self::Cancel(cancel) => vec![8]
                .into_iter()
                .chain(cancel.index.to_be_bytes())
                .chain(cancel.begin.to_be_bytes())
                .chain(cancel.length.to_be_bytes())
                .collect(),
        };
        let length = base_message.len() as u32;
        Ok(length
            .to_ne_bytes()
            .into_iter()
            .chain(base_message)
            .collect())
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
        [19u8]
            .iter()
            .chain(b"BitTorrent protocol".into_iter())
            .chain((0..8).map(|_| &0u8))
            .chain(&self.info_hash)
            .chain(&self.peer_id)
            .copied()
            .collect()
    }
}