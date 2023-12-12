use std::{
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    time::Duration,
};

use crate::{error::BitTorrentError, tracker::multimodal::Tracker, util::timestr};

use super::{Config, Corkboard, Peer, PeerState};

/// maximum duration in between tracker queries
const MAX_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Watchdog thread: periodically fetches and updates the peer list by contacting the tracker
pub fn watchdog(
    corkboard: Arc<RwLock<Corkboard>>,
    alarm: Receiver<()>,
    mut tracker: Tracker,
    config: Config,
) -> Result<(), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][W] {msg}", timestr())
        }
    };

    log(format!("Watchdog init"));
    loop {
        // update peer information
        log(format!("Querying peer source"));
        let interval = {
            let (mut peers, interval) = tracker.query();
            println!("{}", format!("Found {} peers", peers.len()));
            corkboard
                .write()
                .map(|mut board| {
                    board.peers.iter_mut().for_each(|(address, peer)| {
                        match peers
                            .iter()
                            .enumerate()
                            .find(|(_, peer)| *peer == address)
                            .map(|(i, _)| i)
                        {
                            Some(i) => {
                                peers.remove(i);
                                peer.connection_attempts = 0;
                                if !matches!(
                                    peer.state,
                                    PeerState::Error
                                        | PeerState::Active(true)
                                        | PeerState::Connecting
                                ) {
                                    peer.state = PeerState::Fresh;
                                }
                            }
                            None => (),
                        }
                    });
                    board
                        .peers
                        .extend(peers.into_iter().map(|address| (address, Peer::new())));
                })
                .unwrap();
            Duration::from_secs(interval).min(MAX_INTERVAL)
        };

        // wait on watchdog alarm
        log(format!("Waiting {}s", interval.as_secs()));
        if matches!(
            alarm.recv_timeout(interval),
            Err(RecvTimeoutError::Disconnected) | Ok(_)
        ) {
            break;
        }
    }
    log(format!("Exiting"));

    Ok(())
}
