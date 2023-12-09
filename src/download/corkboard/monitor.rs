use std::{sync::{Arc, RwLock, mpsc::{Receiver, RecvTimeoutError}}, time::Duration};

use crate::{error::BitTorrentError, util::timestr};

use super::{Corkboard, PeerState, PieceState};

/// time between reports (seconds)
const INTERVAL: u64 = 5;

/// Monitor thread: reads out statistics about the current download at regular intervals
pub fn monitor(corkboard: Arc<RwLock<Corkboard>>, alarm: Receiver<()>) -> Result<(), BitTorrentError> {
    let log = |msg: String| println!("[{}][M] {msg}", timestr());

    log(format!("Monitor init"));
    loop {
        corkboard.read().map(|board| {

            // collect peer statistics
            let total_peers = board.peers.len();
            let fresh_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Fresh)).count();
            let connecting_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Connecting)).count();
            let active_acquired_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Active(true))).count();
            let active_unaquired_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Active(false))).count();
            let total_active = active_acquired_count + active_unaquired_count;
            let superceded_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Superceded)).count();
            let error_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Error)).count();
            
            // collect piece statistics
            let total_pieces = board.pieces.len();
            let unfetched_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::Unfetched)).count();
            let in_progress_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::InProgress)).count();
            let fetched_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::Fetched(_))).count();

            // print results
            log(format!(""));
            log(format!("----------------------------"));
            log(format!(""));
            log(format!("Peer Stats:"));
            log(format!("Total Peers:   {total_peers}"));
            log(format!("Fresh:         {fresh_count}"));
            log(format!("Connecting:    {connecting_count}"));
            log(format!("Active:        {total_active} ({active_acquired_count} + {active_unaquired_count})"));
            log(format!("Superceded:    {superceded_count}"));
            log(format!("Error:         {error_count}"));
            log(format!(""));
            log(format!("Piece Stats:"));
            log(format!("Total Pieces:  {total_pieces}"));
            log(format!("Unfetched:     {unfetched_count}"));
            log(format!("In Progress:   {in_progress_count}"));
            log(format!("Fetched:       {fetched_count}"));
            log(format!(""));
            log(format!("----------------------------"));
            log(format!(""));

        }).unwrap();

        // wait on alarm
        if matches!(
            alarm.recv_timeout(Duration::from_secs(INTERVAL)), 
            Err(RecvTimeoutError::Disconnected) | Ok(_)
        ) {
            break;
        }
    }

    Ok(())
} 
