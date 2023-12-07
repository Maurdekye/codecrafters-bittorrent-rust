use std::net::{SocketAddrV4, UdpSocket};

use anyhow::Context;

use crate::{error::BitTorrentError, info::MetaInfo};

use super::PeerConnection;

pub enum UtpMessage {
    Data,
    Fin,
    State,
    Reset,
    Syn,
}

impl UtpMessage {
    fn encode(&self) -> Vec<u8> {
        todo!()
    }
}

#[derive(Debug)]
pub struct UtpPeer {
    pub address: SocketAddrV4,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub socket: UdpSocket,
    pub bitfield: Vec<bool>,
    pub connection_id: u16,
    pub seq_nr: u16,
    pub ack_nr: u16,
}

impl UtpPeer {
    
}

impl PeerConnection for UtpPeer {
    type Error = BitTorrentError;

    fn new(peer: SocketAddrV4, meta_info: MetaInfo, peer_id: String) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        let mut connection = UtpPeer {
            socket: UdpSocket::bind("0.0.0.0:0")
                .with_context(|| "Unable to bind to local socket")?,
            bitfield: Vec::new(),
            connection_id: 0,
            seq_nr: 1,
            ack_nr: 0,
            address: peer,
            meta_info,
            peer_id,
        };

        connection
            .socket
            .connect(peer)
            .with_context(|| "Unable to connect to peer")?;

        Ok(connection)
    }

    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, Self::Error> {
        todo!()
    }

    fn address(&self) -> &SocketAddrV4 {
        &self.address
    }

    fn meta_info(&self) -> &MetaInfo {
        &self.meta_info
    }

    fn bitfield(&self) -> &Vec<bool> {
        &self.bitfield
    }
}
