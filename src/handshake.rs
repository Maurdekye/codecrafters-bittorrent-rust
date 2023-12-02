use std::io::prelude::*;
use std::net::{SocketAddrV4, TcpStream};

use crate::{bterror, error::BitTorrentError, info::MetaInfo};

pub struct HandshakeMessage {
    pub info_hash: Vec<u8>,
    pub peer_id: Vec<u8>,
}

impl HandshakeMessage {
    pub fn new(meta_info: &MetaInfo, peer_id: &str) -> Result<HandshakeMessage, BitTorrentError> {
        Ok(HandshakeMessage {
            info_hash: meta_info.info.hash()?,
            peer_id: peer_id.as_bytes().to_vec(),
        })
    }

    fn decode(bytes: &[u8]) -> Result<HandshakeMessage, BitTorrentError> {
        Ok(HandshakeMessage {
            info_hash: bytes[28..48].into(),
            peer_id: bytes[48..68].into()
        })
    }


    fn encode(&self) -> Vec<u8> {
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

pub fn send_handshake(
    peer: &SocketAddrV4,
    message: &HandshakeMessage,
) -> Result<HandshakeMessage, BitTorrentError> {
    let mut stream =
        TcpStream::connect(peer).map_err(|err| bterror!("Error connecting to peer: {}", err))?;
    stream
        .write(&message.encode())
        .map_err(|err| bterror!("Unable to write to peer: {}", err))?;
    let mut buf = [0u8; 68];
    let n_bytes = stream
        .read(&mut buf)
        .map_err(|err| bterror!("Error reading peer response: {}", err))?;
    if n_bytes < 68 {
        Err(bterror!(
            "Did not read enough bytes to construct a handshake message: need 68, only read {}",
            n_bytes
        ))
    } else {
        HandshakeMessage::decode(&buf)
    }
}
