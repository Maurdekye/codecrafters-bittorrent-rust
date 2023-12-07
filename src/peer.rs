use std::{fmt::Display, net::SocketAddrV4};

use crate::info::MetaInfo;

pub mod tcp;
pub mod message; 

pub trait PeerConnection {
    type Error: Display;

    fn new(
        peer: SocketAddrV4,
        meta_info: MetaInfo,
        peer_id: String,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized;
    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, Self::Error>;

    fn address(&self) -> &SocketAddrV4;
    fn meta_info(&self) -> &MetaInfo;
    fn bitfield(&self) -> &Vec<bool>;

    /// Check if the peer has the piece with `piece_id` available to download.
    fn has(&self, piece_id: usize) -> bool {
        *self.bitfield().get(piece_id).unwrap_or(&false)
    }
}