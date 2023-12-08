use std::{
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    time::Duration,
};

use crate::{error::BitTorrentError, tracker::multimodal::Tracker, util::timestr};

use super::{Corkboard, Peer, PeerState};

/// time before retrying a tracker query request if the first request failed (seconds)
const RETRY_INTERVAL: u64 = 5;

/// Watchdog thread: periodically fetches and updates the peer list by contacting the tracker
pub fn watchdog(corkboard: Arc<RwLock<Corkboard>>, alarm: Receiver<()>) -> Result<(), BitTorrentError> {
    let log = |msg: String| println!("[{}][W] {msg}", timestr());

    log(format!("Watchdog init"));

    // fetch board to get a copy of meta_info
    let meta_info = corkboard
        .read()
        .map(|board| board.meta_info.clone())
        .unwrap();

    log(format!("Initializing tracker connection"));
    let mut tracker = Tracker::new(&meta_info).expect("Tracker unable to connect!");
    loop {
        // update peer information
        log(format!("Acquiring corkboard"));
        let mut board = corkboard.write().unwrap();

        log(format!("Updating peer list"));
        let interval = match tracker
            .query(&board.peer_id, board.port)
            .and_then(|response| Ok((response.peers()?, response.interval)))
        {
            Ok((mut peers, interval)) => {
                log(format!("Found {} peers", peers.len()));
                board.peers.iter_mut().for_each(|(address, peer)| {
                    if peer.state != PeerState::Error {
                        peer.state = match peers
                            .iter()
                            .enumerate()
                            .find(|(_, peer)| *peer == address)
                            .map(|(i, _)| i)
                        {
                            Some(i) => {
                                peers.remove(i);
                                PeerState::Fresh
                            }
                            None => PeerState::Inactive,
                        }
                    }
                });
                board
                    .peers
                    .extend(peers.into_iter().map(|address| (address, Peer::new())));
                interval as u64
            }
            Err(err) => {
                log(format!("Error querying tracker: {}", err));
                RETRY_INTERVAL
            }
        };

        // release board
        drop(board);

        // wait on watchdog alarm
        log(format!("Waiting {interval}s"));
        if matches!(
            alarm.recv_timeout(Duration::from_secs(interval)),
            Err(RecvTimeoutError::Disconnected) | Ok(_)
        ) {
            break;
        }
    }
    log(format!("Exiting"));

    Ok(())
}
