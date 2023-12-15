use std::{
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    time::Duration, ops::ControlFlow,
};

use crate::{error::BitTorrentError, tracker::multimodal::Tracker, util::timestr};

use super::{Config, Corkboard, Peer, PeerState};

/// maximum duration in between tracker queries
const MAX_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Watchdog thread: periodically fetches and updates the peer list by contacting the tracker
pub fn watchdog(
    corkboard: Arc<RwLock<Corkboard>>,
    alarm: Receiver<()>,
    tracker: Tracker,
    config: Config,
) -> Result<(), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][W] {msg}", timestr())
        }
    };

    log(format!("Watchdog init"));
    for (new_peer, should_wait) in tracker {

        // add the new peer
        if let Ok(mut board) = corkboard.write() {
            if let Some((_, board_peer)) =
                board.peers.iter_mut().find(|(addr, _)| *addr == &new_peer)
            {
                board_peer.connection_attempts = 0;
                if !matches!(
                    board_peer.state,
                    PeerState::Error | PeerState::Active(true) | PeerState::Connecting
                ) {
                    board_peer.state = PeerState::Fresh;
                }
            } else {
                board.peers.insert(new_peer, Peer::new());
            }
        } else {
            println!("Failed to acquire board");
        }

        // wait if requested to do so
        if let ControlFlow::Break(wait_time) = should_wait {
            let wait_time = wait_time.min(MAX_INTERVAL);
            log(format!("Waiting {}s", wait_time.as_secs()));
            if matches!(
                alarm.recv_timeout(wait_time),
                Err(RecvTimeoutError::Disconnected) | Ok(_)
            ) {
                break;
            }
        }
    }
    log(format!("Exiting"));

    Ok(())
}
