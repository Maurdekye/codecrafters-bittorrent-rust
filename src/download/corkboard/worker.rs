use std::{
    error::Error,
    net::SocketAddr,
    sync::{atomic, Arc, RwLock},
    time::{SystemTime, Duration}, thread,
};

use crate::{
    error::BitTorrentError,
    peer::PeerConnection,
    util::{sha1_hash, sleep, timestr}, info::MetaInfo, torrent_source::TorrentSource,
};

use super::{Benchmark, Config, Corkboard, PeerState, Piece, PieceLocation, PieceState};

/// maximum number of time a given peer can be reused before it should be dropped
const MAX_PEER_USES: usize = 500;
/// time to wait before next check if no peers are available to take
const EMPTY_PEER_WAIT: Duration = Duration::from_secs(1);
/// conversion factor of bytes per millisecond to mebibites per second
const MB_S: f64 = 1048.576;
/// maximum total torrent size before pieces are saved to disk as opposed to being cached in memory (bytes)
const MAX_MEMORY_SIZE: usize = 52428800; // 50 MB
/// maximum reconnection attempts to be made to a given peer
// const MAX_RECONNECT_ATTEMPTS: usize = 5;

enum PeerSearchResult<T: PeerConnection> {
    ConnectNew(SocketAddr),
    Reuse(T),
    WaitThenRefetch(Duration),
    PromptRefetch,
    Exit,
}

enum LoopAction {
    Continue,
    Pass,
}

/// mutual exclusion zone 1:
/// * check if all pieces are downloaded
/// * review existing peer to see if it's okay to reuse or not
/// * find a new peer that's ready to be connected to if there is no existing peer
fn search_for_peer<T, F>(
    corkboard: &Arc<RwLock<Corkboard>>,
    active_connection: Option<T>,
    uses: usize,
    log: F,
) -> PeerSearchResult<T>
where
    T: PeerConnection,
    F: Fn(String),
{
    corkboard
        .write()
        .map(|mut board| {
            // If all pieces have been acquired, exit
            if board.finishing.load(atomic::Ordering::Relaxed)
                || board
                    .pieces
                    .iter()
                    .all(|piece| matches!(piece.state, PieceState::Fetched(_)))
            {
                log(format!("All pieces have been acquired, exiting"));
                active_connection.as_ref().map(|conn| {
                    board
                        .peers
                        .entry(*conn.address())
                        .and_modify(|peer| peer.state = PeerState::Active(false));
                });
                board.finishing.store(true, atomic::Ordering::Relaxed);
                return PeerSearchResult::Exit;
            }

            match active_connection.map(|connection| (connection.address().clone(), connection)) {
                // review existing peer connection
                Some((address, connection)) => {
                    let peer = board.peers.get(&address).unwrap().clone();
                    if peer.state == PeerState::Inactive || peer.state == PeerState::Fresh {
                        // existing peer is deactivated or refreshed: drop the connection and pick up a new one
                        log(format!("Existing peer is fresh or inactive, refetching"));
                        PeerSearchResult::PromptRefetch
                    } else if uses >= MAX_PEER_USES {
                        // peer has been reused too many times, drop it and find another peer
                        log(format!("Existing peer is overused, refetching"));
                        board
                            .peers
                            .entry(address)
                            .and_modify(|peer| peer.state = PeerState::Active(false));
                        connection.sever().unwrap();
                        PeerSearchResult::PromptRefetch
                    } else {
                        // existing peer is fine to reuse
                        PeerSearchResult::Reuse(connection)
                    }
                }

                // find a new peer
                None => {
                    log(format!("Searching for new peer"));

                    // search for unclaimed active & fresh peers
                    let mut potential_peers = board
                        .peers
                        .iter()
                        .filter(|(_, peer)| {
                            matches!(peer.state, PeerState::Fresh | PeerState::Active(false))
                        })
                        .collect::<Vec<_>>();

                    // sort by connection speed and connection attempts
                    potential_peers.sort_by(|(_, peer_a), (_, peer_b)| {
                        match (peer_a.performance, peer_b.performance) {
                            // If both have performance data, compare based on performance
                            (Some(perf_a), Some(perf_b)) => perf_b
                                .partial_cmp(&perf_a)
                                .unwrap_or(std::cmp::Ordering::Equal),

                            // If neither have performance data, compare based on connection attempts
                            (None, None) => {
                                peer_a.connection_attempts.cmp(&peer_b.connection_attempts)
                            }

                            // If only one has performance data, that one is prioritized
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                        }
                    });

                    let potential_peer =
                        potential_peers.first().map(|(&address, _)| address.clone());

                    match potential_peer {
                        // if a peer was found, mark it as connecting
                        Some(address) => {
                            log(format!("Found peer {address}, attempting to connect"));
                            board.peers.entry(address).and_modify(|peer| {
                                peer.connection_attempts += 1;
                                peer.state = PeerState::Connecting;
                            });
                            PeerSearchResult::ConnectNew(address)
                        }

                        // if no peer was found, wait a bit and try again
                        None => {
                            log(format!("No peers found, waiting and retrying"));
                            PeerSearchResult::WaitThenRefetch(EMPTY_PEER_WAIT)
                        }
                    }
                }
            }
        })
        .unwrap()
}

/// mutual exclusion zone 2:
/// * attempt to find a new piece to download
fn find_next_piece<T, F>(
    corkboard: &Arc<RwLock<Corkboard>>,
    connection: &T,
    log: F,
) -> Option<usize>
where
    T: PeerConnection,
    F: Fn(String),
{
    corkboard
        .write()
        .map(|mut board| {
            // once there are no unfetched pieces left to acquire,
            // in progress pieces become fair game for other workers to pick up
            // to speed up the final few piece downloads
            let piece_valid_predicate = if board
                .pieces
                .iter()
                .all(|piece| !matches!(piece.state, PieceState::Unfetched))
            {
                |piece: &Piece| matches!(piece.state, PieceState::Unfetched)
            } else {
                |piece: &Piece| !matches!(piece.state, PieceState::Fetched(_))
            };

            // try to find a piece to download
            let next_piece = board
                .pieces
                .iter()
                .enumerate()
                .zip(connection.bitfield().iter())
                .find(|((_, piece), has_piece)| piece_valid_predicate(piece) && **has_piece)
                .map(|((piece_id, _), _)| piece_id);
            match next_piece {
                // if piece was found, mark it as in progress
                Some(piece) => {
                    log(format!("Chose piece {piece}"));
                    board.pieces.get_mut(piece).map(|piece| {
                        piece.state = PieceState::InProgress;
                    });
                }

                // if no piece was found, mark peer as superceded and try again
                None => {
                    log(format!(
                        "No pieces available, dropping peer {}",
                        connection.address()
                    ));
                    board
                        .peers
                        .entry(connection.address().clone())
                        .and_modify(|peer| peer.state = PeerState::Superceded);
                }
            };
            next_piece
        })
        .unwrap()
}

/// mutual exclusion zone 3:
/// * check the download result
/// * update the peer's performance statistics
/// * check the downloaded data's hash
/// * store the downloaded data
fn finalize_download<T, F>(
    corkboard: &Arc<RwLock<Corkboard>>,
    download_result: Result<Vec<u8>, T::Error>,
    download_duration: usize,
    piece_id: usize,
    connection: &T,
    log: F,
    config: &Config,
) -> Result<LoopAction, BitTorrentError>
where
    T: PeerConnection,
    T::Error: Error,
    F: Fn(String),
{
    corkboard
        .write()
        .map(|mut board| {
            match download_result {
                // download failed
                Err(err) => {
                    log(format!(
                        "Failed to download piece {piece_id} from {}: {err}",
                        connection.address()
                    ));

                    // mark peer as errored & sever connection
                    board
                        .peers
                        .entry(connection.address().clone())
                        .and_modify(|peer| peer.state = PeerState::Error);
                    connection.sever().unwrap();

                    // mark piece as unfetched
                    board.pieces.get_mut(piece_id).map(|piece| {
                        piece.state = PieceState::Unfetched;
                    });

                    // try again
                    Ok(LoopAction::Continue)
                }

                // download succeeded
                Ok(data) => {
                    log(format!(
                        "Finished downloading piece {piece_id} from {}",
                        connection.address()
                    ));

                    // update peer performance
                    board
                        .peers
                        .entry(connection.address().clone())
                        .and_modify(|peer| {
                            peer.benchmarks.push(Benchmark {
                                bytes: data.len(),
                                duration_millis: download_duration,
                            });
                            let performance = peer.update_performance();
                            log(format!(
                                "{} has a performance of {:.4} MB/s",
                                connection.address(),
                                performance / MB_S
                            ));
                        });

                    // check hash
                    let should_save_to_disk = board.meta_info.length() > MAX_MEMORY_SIZE;
                    board
                        .pieces
                        .get_mut(piece_id)
                        .map_or(Ok(LoopAction::Continue), |piece| {
                            if piece.hash == sha1_hash(&data) {
                                // if hash matches, store data & keep peer for next loop
                                if !config.verbose {
                                    println!(
                                        "Downloaded piece {piece_id} from {}",
                                        connection.address()
                                    );
                                }

                                piece.state = PieceState::Fetched(if should_save_to_disk {
                                    log(format!("Saving to disk"));
                                    PieceLocation::save_to_disk(
                                        config.temp_path.join(format!("piece-{}.dat", piece_id)),
                                        data,
                                    )?
                                } else {
                                    log(format!("Saving to memory"));
                                    PieceLocation::Memory(data)
                                });
                                Ok(LoopAction::Pass)
                            } else {
                                log(format!(
                                    "Hash of piece {piece_id} does not match, dropping data"
                                ));

                                // if hash does not match, mark piece as unfetched
                                piece.state = PieceState::Unfetched;
                                Ok(LoopAction::Continue)
                            }
                        })
                }
            }
        })
        .unwrap()
}

/// Worker thread: connects to peers and downloads pieces from them
/// * `corkboard`: shared corkboard for coordinating peer connections and downloaded pieces
/// * `worker_id`: worker id number
pub fn worker<T>(
    corkboard: Arc<RwLock<Corkboard>>,
    worker_id: usize,
    meta_info: MetaInfo,
    config: Config,
) -> Result<(), BitTorrentError>
where
    T: PeerConnection,
{
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][{worker_id}] {msg}", timestr())
        }
    };
    // stagger startup to prevent thrashing
    sleep((worker_id * 1000) as u64);

    log(format!("Worker init"));

    let mut active_connection: Option<T> = None;
    let mut uses = 0;

    loop {
        // ! mutual exclusion zone 1: search for a peer to use / connect to
        let peer_search_result = search_for_peer(&corkboard, active_connection, uses, log);

        active_connection = None;
        let mut connection = match peer_search_result {
            PeerSearchResult::ConnectNew(address) => {
                // try to connect to the new peer
                let connection_result = T::new(
                    address.clone(),
                    TorrentSource::File(meta_info.clone()),
                    config.peer_id.clone(),
                    config.port,
                    config.verbose,
                    corkboard
                        .read()
                        .map(|board| board.finishing.clone())
                        .unwrap(),
                );

                match connection_result {
                    // if successful, mark peer as active & claimed
                    Ok(connection) => {
                        log(format!("Connected to {address}"));
                        uses = 0;
                        corkboard
                            .write()
                            .map(|mut board| {
                                board
                                    .peers
                                    .entry(address.clone())
                                    .and_modify(|peer| peer.state = PeerState::Active(true));
                            })
                            .unwrap();
                        connection
                    }

                    // if unsuccessful, mark peer as errored and try another one
                    Err(err) => {
                        corkboard
                            .write()
                            .map(|mut board| {
                                board.peers.entry(address.clone()).and_modify(|peer| {
                                    // if peer.connection_attempts < MAX_RECONNECT_ATTEMPTS {
                                    //     log(format!("Connection attempt {} to {address} failed: {err}", peer.connection_attempts));
                                    //     peer.state = PeerState::Active(false);
                                    // } else {
                                    //     log(format!("Connection attempt {} to {address} failed, marking as errored: {err}", peer.connection_attempts));
                                    //     peer.state = PeerState::Error;
                                    // }
                                    peer.state = PeerState::Active(false);
                                    log(format!(
                                        "Connection attempt {} to {address} failed: {err}",
                                        peer.connection_attempts
                                    ));
                                });
                            })
                            .unwrap();
                        continue;
                    }
                }
            }
            PeerSearchResult::Reuse(connection) => connection,
            PeerSearchResult::WaitThenRefetch(time) => {
                thread::sleep(time);
                continue;
            }
            PeerSearchResult::PromptRefetch => continue,
            PeerSearchResult::Exit => {
                break;
            }
        };

        // ! mutual exclusion zone 2: search for a piece to download
        let piece_id = match find_next_piece(&corkboard, &connection, log) {
            Some(piece) => piece,
            None => continue,
        };

        log(format!(
            "Downloading piece {piece_id} from {}",
            connection.address()
        ));

        // download piece, recording download time
        let start_time = SystemTime::now();
        let result = connection.download_piece(piece_id as u32);
        let duration = SystemTime::now()
            .duration_since(start_time)
            .unwrap()
            .as_millis() as usize;

        // ! mutual exclusion zone 3: finalize & store the downloaded piece
        if matches!(
            finalize_download(
                &corkboard,
                result,
                duration,
                piece_id,
                &connection,
                log,
                &config
            )?,
            LoopAction::Continue
        ) {
            continue;
        }

        active_connection = Some(connection);
        uses += 1;
    }
    log(format!("Exiting"));

    Ok(())
}
