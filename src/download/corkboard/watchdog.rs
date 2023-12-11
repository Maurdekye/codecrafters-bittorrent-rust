use std::{
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    time::Duration,
};

use crate::{error::BitTorrentError, tracker::multimodal::Tracker, util::timestr};

use super::{Config, Corkboard, Peer, PeerState};

/// time before retrying after a failed tracker query
const RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// maximum duration in between tracker queries
const MAX_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Watchdog thread: periodically fetches and updates the peer list by contacting the tracker
pub fn watchdog(
    corkboard: Arc<RwLock<Corkboard>>,
    alarm: Receiver<()>,
    config: Config,
) -> Result<(), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][W] {msg}", timestr())
        }
    };

    log(format!("Watchdog init"));

    // fetch board to get a copy of meta_info
    let (meta_info, peer_id, port) = corkboard
        .read()
        .map(|board| (board.meta_info.clone(), board.peer_id.clone(), board.port))
        .unwrap();

    log(format!("Initializing tracker connection"));
    let mut tracker = Tracker::new(&meta_info).expect("Tracker unable to connect!");
    loop {
        // update peer information

        log(format!("Querying tracker: {:?}", tracker.servers.front()));
        let interval = match tracker
            .query(&peer_id, port, true)
            .and_then(|response| Ok((response.peers()?, response.interval)))
        {
            Ok((mut peers, interval)) => {
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
                Duration::from_secs(interval as u64).min(MAX_INTERVAL)
            }
            Err(err) => {
                println!("{}", format!("Error querying tracker: {}", err));
                RETRY_INTERVAL
            }
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
