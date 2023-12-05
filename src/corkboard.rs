use std::{
    collections::HashMap,
    net::SocketAddrV4,
    sync::{Arc, RwLock, RwLockWriteGuard, mpsc::{channel, RecvTimeoutError}},
    thread::{self}, time::{SystemTime, Duration},
};

use crate::{
    download::PeerConnection, error::BitTorrentError, info::MetaInfo, tracker::query_tracker,
    util::{sleep, sha1_hash}, bterror,
};

const MAX_PEER_USES: usize = 5;
const WATCHDOG_INTERVAL: u64 = 30;

struct Corkboard {
    meta_info: MetaInfo,
    peer_id: String,
    port: u16,
    pieces: Vec<Piece>,
    remaining_pieces: usize,
    peers: HashMap<SocketAddrV4, Peer>,
}

impl Corkboard {
    fn new(meta_info: MetaInfo, peer_id: String, port: u16) -> Result<Self, BitTorrentError> {
        Ok(Self {
            remaining_pieces: meta_info.info.num_pieces(),
            pieces: meta_info
                .info
                .pieces()?
                .into_iter()
                .map(Piece::new)
                .collect(),
            peers: query_tracker(&meta_info, &peer_id, port)?
                .peers()?
                .into_iter()
                .map(|address| (address, Peer::new()))
                .collect(),
            meta_info,
            peer_id,
            port,
        })
    }

    fn notify_piece_completion(&mut self) {
        self.remaining_pieces -= 1;
    }
}

struct Piece {
    hash: [u8; 20],
    state: PieceState,
}

impl Piece {
    fn new(hash: [u8; 20]) -> Self {
        Self {
            hash,
            state: PieceState::Unfetched,
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
enum PieceState {
    Unfetched,
    InProgress,
    Fetched(Vec<u8>),
}

#[derive(Clone)]
struct Peer {
    state: PeerState,
    benchmarks: Vec<Benchmark>,
    performance: Option<f64>,
}

impl Peer {
    fn new() -> Self {
        Peer {
            state: PeerState::Fresh,
            benchmarks: Vec::new(),
            performance: None,
        }
    }

    fn update_performance(&mut self) {
        self.performance = Some(
            self.benchmarks.iter().map(|Benchmark { 
                bytes, 
                duration_millis
            }| (*bytes as f64) / (*duration_millis as f64)).sum::<f64>() / (self.benchmarks.len() as f64)
        );   
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeerState {
    Fresh,
    Active(bool),
    Inactive,
    Superceded,
    Error,
}

#[derive(Clone)]
struct Benchmark {
    bytes: usize,
    duration_millis: usize,
}

pub fn corkboard_download(
    meta_info: &MetaInfo,
    peer_id: &str,
    port: u16,
    workers: usize,
) -> Result<Vec<u8>, BitTorrentError> {

    // create corkboard
    println!("Initializing Corkboard");
    let corkboard = Arc::new(RwLock::new(Corkboard::new(
        meta_info.clone(),
        peer_id.to_string(),
        port,
    )?));

    let (watchdog_notify, watchdog_alarm) = channel();

    // start peer update watchdog
    println!("Starting watchdog");
    let watchdog_board = corkboard.clone();
    let watchdog = thread::spawn(move || {
        println!("[W] Watchdog init");
        loop {

            // wait on watchdog alarm
            println!("[W] Waiting");
            match watchdog_alarm.recv_timeout(Duration::from_secs(WATCHDOG_INTERVAL)) {
                Err(RecvTimeoutError::Disconnected) | Ok(_) => break,
                _ => (),
            }

            // update peer information
            println!("[W] Acquiring corkboard");
            let mut board = watchdog_board.write().unwrap();

            println!("[W] Updating peer list");
            match query_tracker(&board.meta_info, &board.peer_id, board.port).and_then(|response| {
                response.peers()
            }) {
                Ok(peers) => {
                    board.peers.iter_mut().for_each(|(address, peer)| {
                        peer.state = if peer.state != PeerState::Error && peers.contains(address) { 
                            PeerState::Fresh 
                        } else { 
                            PeerState::Inactive 
                        };
                    })
                }
                Err(err) => {
                    println!("[W] Error querying tracker: {}", err);
                }
            }

            // release board
            drop(board);
        }
        println!("[W] Exiting");
    });

    // start workers
    println!("Starting workers");
    let workers = (0..workers)
        .map(|worker_id| {
            let corkboard = corkboard.clone();
            thread::spawn(move || {
                println!("[{}] Worker init", worker_id);
                let mut active_connection: Option<PeerConnection> = None;
                let uses = 0;

                loop {
                    // Acquire board
                    println!("[{}] Acquiring corkboard pre-fetch", worker_id);
                    let mut board = corkboard.write().unwrap();

                    // If all pieces have been acquired, exit
                    if board.pieces.iter().all(|piece| matches!(piece.state, PieceState::Fetched(_))){
                        println!("[{}] All pieces acquired", worker_id);
                        break;
                    }

                    // Piece finding routine definition
                    let find_piece =
                        |active_connection: &Option<PeerConnection>,
                         board: &RwLockWriteGuard<Corkboard>| {
                            active_connection.as_ref().and_then(|connection| {
                                board
                                    .pieces
                                    .iter()
                                    .enumerate()
                                    .zip(connection.bitfield.clone().into_iter())
                                    .find(|((_, piece), has_piece)| {
                                        piece.state == PieceState::Unfetched && *has_piece
                                    })
                                    .map(|((piece_id, _), _)| piece_id)
                            })
                        };

                    let mut next_piece = None;

                    // Check currently active peer
                    active_connection.as_ref().map(|connection| connection.address.clone()).map(|address| {
                        println!("[{}] Reviewing active connection to peer {}", worker_id, address);
                        let mut peer = board.peers.get(&address).unwrap().clone();

                        // If it's inactive or recently refreshed, release it
                        if peer.state == PeerState::Inactive || peer.state == PeerState::Fresh {
                            println!("[{}] {} connection is {:?}, releasing", worker_id, address, peer.state);
                            active_connection = None;
                        } else {

                            // Locate a suitable next piece to download (if there is an active connection)
                            next_piece = find_piece(&active_connection, &board);

                            // If there is no suitable piece, then mark the peer as superceded and release it
                            if next_piece.is_none() {
                                println!("[{}] {} has no more needed pieces, releasing", worker_id, address);
                                active_connection = None;
                                peer.state = PeerState::Superceded;

                            // If this connection has been used more than a maximum number of times in a row, then release it
                            } else if uses >= MAX_PEER_USES {
                                println!("[{}] {} has been overused, releasing", worker_id, address);
                                active_connection = None;
                                peer.state = PeerState::Active(false);
                            }
                        }

                        // Update peer on board
                        board
                            .peers
                            .entry(address.clone())
                            .and_modify(|entry| *entry = peer);
                    });

                    if active_connection.is_none() {
                        println!("[{}] Acquiring new peer", worker_id);

                        // If there is no active peer, find the peer with the best connection that's available to connect to
                        let mut potential_peers = board
                            .peers
                            .iter()
                            .filter(|(_, peer)| matches!(peer.state, PeerState::Fresh | PeerState::Active(false))).collect::<Vec<_>>();
                        
                        potential_peers.sort_by(|(_, peer_a), (_, peer_b)| {
                            peer_b.performance.unwrap_or(f64::MAX)
                                .partial_cmp(&peer_a.performance.unwrap_or(f64::MAX))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });

                        // Attempt to connect to it
                        let potential_peer = potential_peers.first()
                            .map(|(address, _)| {
                                (
                                    PeerConnection::new(
                                        (*address).clone(),
                                        board.meta_info.clone(),
                                        board.peer_id.clone(),
                                    )
                                    .and_then(
                                        |mut connection| {
                                            connection.initialize()?;
                                            Ok(connection)
                                        },
                                    ),
                                    (*address).clone(),
                                )
                            });

                        match potential_peer {

                            // If no peer was found, wait a bit and find a new peer
                            None => {
                                println!("[{}] No available peers found, waiting", worker_id);
                                drop(board);
                                sleep(100);
                                continue;
                            }

                            // If the peer failed to connect, mark it as Errored and find a new peer
                            Some((Err(_), address)) => {
                                println!("[{}] Failed to connect to peer at {}, marking and looking for new peer", worker_id, address);
                                board
                                    .peers
                                    .entry(address.clone())
                                    .and_modify(|peer| peer.state = PeerState::Error);
                                continue;
                            }

                            // On successful connection, mark the peer as active and set it as the active peer
                            Some((Ok(connection), address)) => {
                                println!("[{}] Connected to {}", worker_id, address);
                                board
                                    .peers
                                    .entry(address.clone())
                                    .and_modify(|peer| peer.state = PeerState::Active(true));
                                active_connection = Some(connection);

                                // Locate a new piece to download
                                next_piece = find_piece(&active_connection, &board);

                                // If no pieces are available, mark connection as superceded and look for another peer
                                if next_piece.is_none() {
                                    println!("[{}] {} has no more needed pieces, releasing", worker_id, address);
                                    active_connection = None;
                                    board
                                        .peers
                                        .entry(address.clone())
                                        .and_modify(|peer| peer.state = PeerState::Superceded);
                                    continue;
                                }
                            }
                        }
                    }

                    // Mark chosen piece as in progress
                    next_piece.as_ref().map(|piece_id| {
                        board.pieces.get_mut(*piece_id).map(|piece| {
                            piece.state = PieceState::InProgress;
                        });
                    });

                    // Release board pre-fetch
                    drop(board);

                    // Download chosen piece from chosen peer, recording download time
                    let download_result = active_connection.as_mut().and_then(|connection| {
                        next_piece
                            .as_ref()
                            .map(|piece_id| {
                                println!("[{}] Downloading piece {} from {}", worker_id, piece_id, connection.address);
                                let start_time = SystemTime::now();
                                let result = connection.download_piece(*piece_id as u32);
                                let duration = SystemTime::now().duration_since(start_time).unwrap().as_millis() as usize;
                                result.map(|data| (data, duration))
                            })
                    });

                    // Check result of download
                    match download_result {

                        // No download occurred: this should never happen
                        None => {
                            println!("[{}] Nothing was downloaded; this should never happen", worker_id);
                        }

                        // Error during download
                        Some(Err(_)) => {

                            // Reaquire board
                            println!("[{}] Acquiring corkboard post-fetch", worker_id);
                            let mut board = corkboard.write().unwrap();

                            // Mark peer as errored
                            active_connection.as_ref().map(|connection| { 
                                println!("[{}] connection to {} interrupted, marking as errored and dropping", worker_id, connection.address);
                                board
                                    .peers
                                    .entry(connection.address.clone())
                                    .and_modify(|peer| peer.state = PeerState::Error);
                            });

                            // Release connection
                            active_connection = None;

                            // Re-mark piece as unfetched
                            next_piece.as_ref().map(|piece_id| {
                                board.pieces.get_mut(*piece_id).map(|piece| {
                                    piece.state = PieceState::Unfetched;
                                });
                            });

                            // Release board post-fetch
                            drop(board);
                        }

                        // Piece downloaded successfully
                        Some(Ok((data, duration))) => {

                            active_connection.as_ref().map(|connection| {
                                next_piece.as_ref().map(|piece_id: &usize| {
                                    println!("[{}] finished downloading piece {} from {}", worker_id, piece_id, connection.address);
                                })
                            });

                            // Reaquire board
                            println!("[{}] Acquiring corkboard post-fetch", worker_id);
                            let mut board = corkboard.write().unwrap();

                            // Record benchmark of network performance on peer
                            active_connection.as_ref().map(|connection| {
                                board.peers.entry(connection.address.clone()).and_modify(|peer| {
                                    peer.benchmarks.push(Benchmark { bytes: data.len(), duration_millis: duration });
                                    peer.update_performance();
                                });
                            });
                            
                            // Check piece hash
                            next_piece.as_ref().map(|piece_id: &usize| {
                                board.pieces.get_mut(*piece_id).map(|piece| {
                                    piece.hash == sha1_hash(&data)
                                }).map(|hash_matches| {
                                    if hash_matches {
    
                                        // If hash matches, mark piece as downloaded
                                        println!("[{}] Storing piece {}", worker_id, piece_id);
                                        board.pieces.get_mut(*piece_id).map(|piece| {
                                            piece.state = PieceState::Fetched(data);
                                        });
                                        board.notify_piece_completion();
                                    } else {
    
                                        println!("[{}] Hash of downloaded piece {} does not match; discarding", worker_id, piece_id);
                                        // If hash does not match, discard data and re-mark piece as unfetched
                                        board.pieces.get_mut(*piece_id).map(|piece| {
                                            piece.state = PieceState::Unfetched;
                                        });

                                        // Mark peer as errored? (unsure if necessary)
                                        // active_connection.as_ref().map(|connection| {
                                        //     board.peers.entry(connection.address.clone()).and_modify(|peer| {
                                        //         peer.state = PeerState::Error;
                                        //     })
                                        // });
                                    }
                                });
                            });

                            // Release board post-fetch
                            drop(board);
                        }
                    }
                }
                println!("[{}] Exiting", worker_id);
            })
        })
        .collect::<Vec<_>>();

    // wait for workers to finish
    println!("Waiting for workers to finish");
    workers.into_iter().for_each(|worker| worker.join().unwrap());

    // send kill signal to watchdog (may not be directly necessary; the channel dropping should kill the watchdog automatically)
    println!("Killing watchdog");
    watchdog_notify.send(()).unwrap();
    watchdog.join().unwrap();

    // collect data
    println!("Collecting data");
    let board = corkboard.read().unwrap();
    let data = board.pieces.iter().map(|piece| match &piece.state {
        PieceState::Fetched(data) => Ok(data),
        _ => Err(bterror!("Unfetched piece data remains!"))
    }).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect::<Vec<&u8>>();

    // this is really memory inefficient... need to figure out a better way to save and collect data
    println!("Done");
    Ok(data.into_iter().copied().collect())
}
