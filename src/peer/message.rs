use std::collections::HashMap;
use std::iter::{empty, once};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
use std::ops::Deref;

use crate::bencode::{BencodedValue, Number};
use crate::bytes::{Bytes, PullBytes};
use crate::dict;
use crate::torrent_source::TorrentSource;
use crate::{
    bterror, bytes,
    error::BitTorrentError,
    util::{decode_bitfield_be, encode_bitfield_be},
};

pub trait Codec {
    type Error;

    fn encode(self) -> Result<Vec<u8>, Self::Error>;
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>
    where
        Self: Sized;
}

#[derive(Debug)]
pub enum PeerMessage {
    // base protocol messages
    Handshake(HandshakeMessage),
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

#[derive(Debug)]
pub struct RequestMessage {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug)]
pub struct PieceMessage {
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

#[derive(Debug)]
pub enum ExtensionMessage {
    Handshake(ExtensionHandshake),
    Metadata(ExtensionMetadata, Option<Vec<u8>>),
}

impl ExtensionMessage {
    pub fn name(&self) -> Bytes {
        match self {
            ExtensionMessage::Handshake(_) => bytes!(b"handshake"),
            ExtensionMessage::Metadata(_, _) => bytes!(b"ut_metadata"),
        }
    }
}

#[derive(Debug, Default)]
pub struct ExtensionHandshake {
    pub messages: Option<HashMap<Bytes, Number>>,
    pub port: Option<u16>,
    pub version: Option<Bytes>,
    pub yourip: Option<SocketAddr>,
    pub ipv6: Option<SocketAddrV6>,
    pub ipv4: Option<SocketAddrV4>,
    pub reqq: Option<Number>,
    pub metadata_size: Option<Number>,
}

impl From<BencodedValue> for Result<ExtensionHandshake, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut handshake) = value {
            Ok(ExtensionHandshake {
                messages: handshake
                    .pull(b"m")
                    .and_then(BencodedValue::into_dict)
                    .and_then(|map| {
                        let map = map
                            .into_iter()
                            .filter_map(|(k, v)| v.into_int().map(|i| (k, i)))
                            .collect::<HashMap<_, _>>();
                        map.is_empty().then_some(map)
                    }),
                port: handshake
                    .pull(b"p")
                    .and_then(BencodedValue::into_int)
                    .map(|inner| inner as u16),
                version: handshake.pull(b"v").and_then(BencodedValue::into_bytes),
                yourip: handshake
                    .pull(b"yourip")
                    .and_then(BencodedValue::into_bytes)
                    .map(<Result<SocketAddr, _>>::from)
                    .transpose()?,
                ipv6: handshake
                    .pull(b"ipv6")
                    .and_then(BencodedValue::into_bytes)
                    .map(SocketAddrV6::from),
                ipv4: handshake
                    .pull(b"ipv4")
                    .and_then(BencodedValue::into_bytes)
                    .map(SocketAddrV4::from),
                reqq: handshake.pull(b"reqq").and_then(BencodedValue::into_int),
                metadata_size: handshake
                    .pull(b"metadata_size")
                    .and_then(BencodedValue::into_int),
            })
        } else {
            Err(bterror!("Invalid extension handshake"))
        }
    }
}

impl From<ExtensionHandshake> for BencodedValue {
    fn from(val: ExtensionHandshake) -> Self {
        dict! {
            b"m" => val.messages,
            b"p" => val.port.map(|x| x as Number),
            b"v" => val.version,
            b"yourip" => val.yourip.map(Bytes::from),
            b"ipv6" => val.ipv6.map(Bytes::from),
            b"ipv4" => val.ipv4.map(Bytes::from),
            b"reqq" => val.reqq,
            b"metadata_size" => val.metadata_size,
        }
    }
}

#[derive(Debug)]
pub struct ExtensionMetadata {
    pub msg_type: Number,
    pub piece: Number,
    pub total_size: Option<Number>,
}

impl From<BencodedValue> for Result<ExtensionMetadata, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut metadata) = value {
            Ok(ExtensionMetadata {
                msg_type: metadata
                    .pull(b"msg_type")
                    .and_then(BencodedValue::into_int)
                    .ok_or(bterror!("msg_type missing"))?
                    .into(),
                piece: metadata
                    .pull(b"piece")
                    .and_then(BencodedValue::into_int)
                    .ok_or(bterror!("piece missing"))?
                    .into(),
                total_size: metadata
                    .pull(b"total_size")
                    .and_then(BencodedValue::into_int),
            })
        } else {
            Err(bterror!("Invalid extension metadata"))
        }
    }
}

impl From<ExtensionMetadata> for BencodedValue {
    fn from(val: ExtensionMetadata) -> Self {
        dict! {
            b"msg_type" => val.msg_type,
            b"piece" => val.piece,
            b"total_size" => val.total_size,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerMessageCodec {
    extension_codec: ExtensionMessageCodec,
}

impl PeerMessageCodec {
    pub fn new(extensions_map: Vec<(Bytes, Number)>) -> Self {
        Self {
            extension_codec: ExtensionMessageCodec::new(extensions_map),
        }
    }

    pub fn from_handshake(handshake: &ExtensionHandshake) -> Result<Self, BitTorrentError> {
        Ok(PeerMessageCodec::new(
            handshake
                .messages
                .clone()
                .unwrap_or(Default::default())
                .into_iter()
                .map(|(k, v)| Ok((k, v)))
                .collect::<Result<Vec<_>, BitTorrentError>>()?,
        ))
    }

    /// Decode a peer message from a byte array.
    pub fn decode(&self, bytes: &[u8]) -> Result<PeerMessage, BitTorrentError> {
        match bytes.get(0) {
            None => Ok(PeerMessage::Keepalive),
            Some(0) => Ok(PeerMessage::Choke),
            Some(1) => Ok(PeerMessage::Unchoke),
            Some(2) => Ok(PeerMessage::Interested),
            Some(3) => Ok(PeerMessage::NotInterested),
            Some(4) => Ok(PeerMessage::Have(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(5) => Ok(PeerMessage::Bitfield(
                bytes[1..]
                    .iter()
                    .copied()
                    .flat_map(decode_bitfield_be)
                    .collect(),
            )),
            Some(6) => Ok(PeerMessage::Request(RequestMessage::decode(
                &mut &bytes[1..],
            )?)),
            Some(7) => Ok(PeerMessage::Piece(PieceMessage::decode(&mut &bytes[1..])?)),
            Some(8) => Ok(PeerMessage::Cancel(RequestMessage::decode(
                &mut &bytes[1..],
            )?)),
            Some(9) => Ok(PeerMessage::Port(u16::from_be_bytes(
                bytes[1..3].try_into().unwrap(),
            ))),
            Some(14) => Ok(PeerMessage::HaveAll),
            Some(15) => Ok(PeerMessage::HaveNone),
            Some(16) => Ok(PeerMessage::RejectRequest(RequestMessage::decode(
                &mut &bytes[1..],
            )?)),
            Some(17) => Ok(PeerMessage::AllowFast(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(20) => Ok(PeerMessage::Extension(
                self.extension_codec.decode(&bytes[1..])?,
            )),
            Some(byte) => Err(bterror!("Invalid peer message: {byte}")),
        }
    }

    /// Encode a peer message into a byte array.
    pub fn encode(&self, message: PeerMessage) -> Result<Vec<u8>, BitTorrentError> {
        // dbg!(&self);
        let base_message: Vec<u8> = match message {
            PeerMessage::Keepalive => vec![],
            PeerMessage::Choke => vec![0],
            PeerMessage::Unchoke => vec![1],
            PeerMessage::Interested => vec![2],
            PeerMessage::NotInterested => vec![3],
            PeerMessage::Have(index) => once(4).chain(index.to_be_bytes()).collect(),
            PeerMessage::Bitfield(bitfield) => once(5)
                .chain(
                    bitfield
                        .chunks(8)
                        .map(|bits| encode_bitfield_be(bits.try_into().unwrap())),
                )
                .collect(),
            PeerMessage::Request(req) => once(6).chain(req.encode()?).collect(),
            PeerMessage::Piece(piece) => once(7).chain(piece.encode()?).collect(),
            PeerMessage::Cancel(cancel) => once(8).chain(cancel.encode()?).collect(),
            PeerMessage::Port(port) => once(9).chain(port.to_be_bytes()).collect(),
            PeerMessage::HaveAll => vec![14],
            PeerMessage::HaveNone => vec![15],
            PeerMessage::RejectRequest(req) => once(16).chain(req.encode()?).collect(),
            PeerMessage::AllowFast(index) => once(17).chain(index.to_be_bytes()).collect(),
            PeerMessage::Extension(ext) => {
                once(20).chain(self.extension_codec.encode(ext)?).collect()
            }
            PeerMessage::Handshake(handshake) => return Ok(handshake.encode()),
        };
        let length = base_message.len() as u32;
        Ok(length
            .to_be_bytes()
            .into_iter()
            .chain(base_message)
            .collect())
    }
}

impl Default for PeerMessageCodec {
    fn default() -> Self {
        Self::new(vec![])
    }
}

#[derive(Debug, Clone)]
pub struct ExtensionMessageCodec {
    extension_name_map: Vec<(Bytes, Number)>,
}

impl ExtensionMessageCodec {
    pub fn new(map: Vec<(Bytes, Number)>) -> Self {
        Self {
            extension_name_map: once((Bytes(b"handshake".to_vec()), 0)).chain(map).collect(),
        }
    }

    pub fn encode(&self, message: ExtensionMessage) -> Result<Vec<u8>, BitTorrentError> {
        let name = message.name();
        let (_, code) = self
            .extension_name_map
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or(bterror!("Extension '{}' is unsupported by peer", name))?;
        Ok(once(*code as u8)
            .chain(match message {
                ExtensionMessage::Handshake(handshake) => {
                    BencodedValue::from(handshake).encode()?
                }
                ExtensionMessage::Metadata(metadata, data) => BencodedValue::from(metadata)
                    .encode()?
                    .into_iter()
                    .chain(data.unwrap_or_default())
                    .collect::<Vec<_>>(),
            })
            .collect())
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<ExtensionMessage, BitTorrentError> {
        let (code, mut bytes) = bytes.split_first().ok_or(bterror!("Insufficient bytes"))?;
        let (name, _) = self
            .extension_name_map
            .iter()
            .find(|(_, c)| *c == (*code as i64))
            .ok_or(bterror!("Unrecognized extension code: {}", code))?;
        match name.deref() {
            b"handshake" => Ok(ExtensionMessage::Handshake(<Result<_, _>>::from(
                BencodedValue::ingest(&mut bytes)?,
            )?)),
            b"ut_metadata" => Ok(ExtensionMessage::Metadata(
                <Result<_, _>>::from(BencodedValue::ingest(&mut bytes)?)?,
                bytes.is_empty().then_some(bytes.to_vec()),
            )),
            name => Err(bterror!(
                "Unrecognized extension name: {}",
                Bytes::from(name)
            )),
        }
    }
}

impl Default for ExtensionMessageCodec {
    fn default() -> Self {
        Self::new(vec![])
    }
}

impl Codec for RequestMessage {
    type Error = BitTorrentError;

    fn encode(self) -> Result<Vec<u8>, Self::Error> {
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

impl Codec for PieceMessage {
    type Error = BitTorrentError;

    fn encode(self) -> Result<Vec<u8>, Self::Error> {
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
    pub fn new(
        torrent_source: &TorrentSource,
        peer_id: &str,
    ) -> Result<HandshakeMessage, BitTorrentError> {
        Ok(HandshakeMessage {
            info_hash: torrent_source.hash()?,
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
