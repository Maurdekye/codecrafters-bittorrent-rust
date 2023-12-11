use std::{sync::{Arc, RwLock, mpsc::{Receiver, RecvTimeoutError}}, time::Duration};

use crate::{error::BitTorrentError, util::timestr, info::MetaInfo};

use super::{Corkboard, PeerState, PieceState, Config};

/// time between reports
const INTERVAL: Duration = Duration::from_secs(5);

/// Monitor thread: reads out statistics about the current download at regular intervals
pub fn monitor(corkboard: Arc<RwLock<Corkboard>>, alarm: Receiver<()>, _meta_info: MetaInfo, config: Config) -> Result<(), BitTorrentError> {
    let log = |msg: String| if config.verbose {println!("[{}][M] {msg}", timestr())};

    log(format!("Monitor init"));
    loop {
        corkboard.read().map(|board| {

            let total_peers = board.peers.len();
            let active_acquired_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Active(true))).count();

            let total_pieces = board.pieces.len();
            let fetched_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::Fetched(_))).count();

            if config.verbose {
                // collect peer statistics
                let fresh_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Fresh)).count();
                let connecting_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Connecting)).count();
                let active_unaquired_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Active(false))).count();
                let superceded_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Superceded)).count();
                let error_count = board.peers.iter().filter(|(_, peer)| matches!(peer.state, PeerState::Error)).count();
                
                // collect piece statistics
                let unfetched_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::Unfetched)).count();
                let in_progress_count = board.pieces.iter().filter(|piece| matches!(piece.state, PieceState::InProgress)).count();
    
                // print results
                log(format!(""));
                log(format!("----------------------------"));
                log(format!(""));
                log(format!("Peer Stats:"));
                log(format!("Total Peers:   {total_peers}"));
                log(format!("Fresh:         {fresh_count}"));
                log(format!("Active:        {active_unaquired_count}"));
                log(format!("Connecting:    {connecting_count}"));
                log(format!("Connected:     {active_acquired_count}"));
                log(format!("Superceded:    {superceded_count}"));
                log(format!("Error:         {error_count}"));
                log(format!(""));
                log(format!("Piece Stats:"));
                log(format!("Total Pieces:  {total_pieces}"));
                log(format!("Unfetched:     {unfetched_count}"));
                log(format!("In Progress:   {in_progress_count}"));
                log(format!("Fetched:       {fetched_count}"));
                if unfetched_count + in_progress_count < 50 {
                    log(format!(""));
                    log(format!("Unfetched ids: {:?}", board.pieces.iter().enumerate().filter_map(|(id, piece)| if matches!(piece.state, PieceState::Unfetched) { Some(id) } else { None }).collect::<Vec<_>>()));
                    log(format!("In Progress ids: {:?}", board.pieces.iter().enumerate().filter_map(|(id, piece)| if matches!(piece.state, PieceState::InProgress) { Some(id) } else { None }).collect::<Vec<_>>()));
                }
                log(format!(""));
                log(format!("----------------------------"));
                log(format!(""));
            } else {
                println!("{}: Connected to {active_acquired_count}/{total_peers} peers, downloaded {fetched_count}/{total_pieces} pieces", timestr());
            }

        }).unwrap();

        // wait on alarm
        if matches!(
            alarm.recv_timeout(INTERVAL), 
            Err(RecvTimeoutError::Disconnected) | Ok(_)
        ) {
            break;
        }
    }

    Ok(())
} 
