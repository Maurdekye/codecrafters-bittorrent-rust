use std::{
    error,
    net::SocketAddr,
    sync::{atomic::AtomicBool, Arc},
};

use crate::{info::MetaInfo, torrent_source::TorrentSource};

pub mod message;
pub mod tcp;
pub mod utp;

pub trait PeerConnection {
    type Error: error::Error;

    fn new(
        peer: SocketAddr,
        torrent_source: TorrentSource,
        peer_id: String,
        port: u16,
        verbose: bool,
        killswitch: Arc<AtomicBool>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized;
    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, Self::Error>;

    fn sever(&self) -> Result<(), Self::Error>;

    fn address(&self) -> &SocketAddr;
    fn meta_info(&self) -> Option<&MetaInfo>;
    fn bitfield(&self) -> &Vec<bool>;

    /// Check if the peer has the piece with `piece_id` available to download.
    fn has(&self, piece_id: usize) -> bool {
        *self.bitfield().get(piece_id).unwrap_or(&false)
    }
}
