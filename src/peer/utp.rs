use std::{net::{UdpSocket, SocketAddr}, sync::{atomic::AtomicBool, Arc}};

use anyhow::Context;

use crate::{error::BitTorrentError, info::MetaInfo};

use super::PeerConnection;

#[allow(unused)]
pub enum UtpMessage {
    Data,
    Fin,
    State,
    Reset,
    Syn,
}

#[allow(unused)]
impl UtpMessage {
    fn encode(&self) -> Vec<u8> {
        unimplemented!()
    }
}

#[derive(Debug)]
pub struct UtpPeer {
    pub address: SocketAddr,
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

    fn new(peer: SocketAddr, meta_info: MetaInfo, peer_id: String, _port: u16, _verbose: bool, _killswitch: Arc<AtomicBool>) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        let connection = UtpPeer {
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

    #[allow(unused)]
    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, Self::Error> {
        unimplemented!()
    }

    fn address(&self) -> &SocketAddr {
        &self.address
    }

    fn meta_info(&self) -> &MetaInfo {
        &self.meta_info
    }

    fn bitfield(&self) -> &Vec<bool> {
        &self.bitfield
    }

    fn sever(&self) -> Result<(), Self::Error> {
        todo!()
    }
}
