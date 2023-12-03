use std::io::prelude::*;
use std::net::TcpStream;

use crate::{bterror, error::BitTorrentError, info::MetaInfo, util::read_n_bytes};

#[derive(Debug)]
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
    stream: &mut TcpStream,
    message: &HandshakeMessage,
) -> Result<HandshakeMessage, BitTorrentError> {
    stream
        .write(&message.encode())
        .map_err(|err| bterror!("Unable to write to peer: {}", err))?;
    let buf = read_n_bytes(stream, 68)?;
    HandshakeMessage::decode(&buf)
}
