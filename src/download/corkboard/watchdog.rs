use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
};

use crate::{error::BitTorrentError, util::timestr};

use crossbeam::channel::Receiver;

use super::{Config, Corkboard, Peer, PeerState};

/// Watchdog thread: periodically fetches and updates the peer list by contacting the tracker
pub fn watchdog(
    corkboard: Arc<RwLock<Corkboard>>,
    peer_source: Receiver<SocketAddr>,
    config: Config,
) -> Result<(), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][W] {msg}", timestr())
        }
    };

    log(format!("Watchdog init"));
    for peer in peer_source {
        // add the new peer
        if let Ok(mut board) = corkboard.write() {
            if let Some((_, board_peer)) = board.peers.iter_mut().find(|(addr, _)| *addr == &peer) {
                board_peer.connection_attempts = 0;
                if !matches!(
                    board_peer.state,
                    PeerState::Error | PeerState::Active(true) | PeerState::Connecting
                ) {
                    board_peer.state = PeerState::Fresh;
                }
            } else {
                board.peers.insert(peer, Peer::new());
            }
        } else {
            println!("Failed to acquire board");
        }
    }
    log(format!("Exiting"));

    Ok(())
}
