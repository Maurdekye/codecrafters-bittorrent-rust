use std::{
    collections::HashMap,
    net::{TcpListener, SocketAddr},
    sync::{
        mpsc::{channel, RecvTimeoutError},
        Arc, RwLock,
    },
    thread::{self},
    time::{Duration, SystemTime},
};

use anyhow::Context;

use crate::{
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    peer::{PeerConnection, message::{HandshakeMessage, BitfieldMessage, PeerMessage, PieceMessage}, tcp::TcpPeer},
    tracker::multimodal::Tracker,
    util::{sha1_hash, sleep, timestr, read_n_bytes},
};

const MAX_PEER_USES: usize = 5; // seconds
const WATCHDOG_RETRY_INTERVAL: u64 = 5; // seconds
const EMPTY_PEER_WAIT: u64 = 1000; // milliseconds
const MONITOR_INTERVAL: u64 = 5; // seconds

/// conversion factor of bytes / millisecond to mebibites / second
const MB_S: f64 = 1048.576;

struct Corkboard {
    meta_info: MetaInfo,
    peer_id: String,
    port: u16,
    pieces: Vec<Piece>,
    peers: HashMap<SocketAddr, Peer>,
}

impl Corkboard {
    fn new(meta_info: MetaInfo, peer_id: String, port: u16) -> Result<Self, BitTorrentError> {
        Ok(Self {
            pieces: meta_info.pieces()?.into_iter().map(Piece::new).collect(),
            peers: HashMap::new(),
            meta_info,
            peer_id,
            port,
        })
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

    /// Update the peer's benchmark performance rating, for use in ranking their fitness to download a torrent with
    fn update_performance(&mut self) -> f64 {
        let performance = self
            .benchmarks
            .iter()
            .map(
                |Benchmark {
                     bytes,
                     duration_millis,
                 }| (*bytes as f64) / (*duration_millis as f64),
            )
            .sum::<f64>()
            / (self.benchmarks.len() as f64);
        self.performance = Some(performance);
        performance
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeerState {
    Fresh,
    Active(bool),
    Inactive,
    Superceded,
    Connecting,
    Error,
}

#[derive(Clone)]
struct Benchmark {
    bytes: usize,
    duration_millis: usize,
}

enum PeerSearchResult<T: PeerConnection> {
    ConnectNew(SocketAddr),
    Reuse(T),
    WaitThenRefetch(u64),
    PromptRefetch,
    Exit,
}

enum LoopAction {
    Continue,
    Pass,
}

/// ## Corkboard Download
///
/// Download the torrent using a self-coined 'Corkboard' synchronization strategy.
/// Each worker refernces a mutually accessible `Corkboard`, which contains relevant
/// information about all active peers, and all torrent pieces. Workers reference the
/// corkboard to determine which peers are valid to pick up, and which pieces need to be
/// fetched. They check it once before performing their download to determine which peer to
/// connect to and which piece to acquire, and once afterwards to validate and submit their
/// successful download to the board.
pub fn corkboard_download<T: PeerConnection>(
    meta_info: MetaInfo,
    peer_id: &str,
    port: u16,
    workers: usize,
) -> Result<Vec<u8>, BitTorrentError> {
    let log = |msg: String| println!("[{}] {msg}", timestr());

    // create corkboard
    log(format!("Initializing Corkboard"));
    let corkboard = Arc::new(RwLock::new(Corkboard::new(
        meta_info,
        peer_id.to_string(),
        port,
    )?));

    log(format!(
        "Preparing to download {} pieces",
        corkboard.clone().read().unwrap().pieces.len(),
    ));

    let (monitor_notify, monitor_alarm) = channel();

    log(format!("Starting monitor"));
    let monitor_board = corkboard.clone();
    let monitor = thread::spawn(move || {
        let log = |msg: String| println!("[{}][M] {msg}", timestr());

        log(format!("Monitor init"));
        loop {
            monitor_board.read().map(|board| {

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
                log(format!(">>>>>>>>>>>>><<<<<<<<<<<<<"));
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
                log(format!(">>>>>>>>>>>>><<<<<<<<<<<<<"));
                log(format!(""));

            }).unwrap();

            // wait on alarm
            if matches!(
                monitor_alarm.recv_timeout(Duration::from_secs(MONITOR_INTERVAL)), 
                Err(RecvTimeoutError::Disconnected) | Ok(_)
            ) {
                break;
            }
        }
    });

    let (watchdog_notify, watchdog_alarm) = channel();

    // start peer update watchdog
    log(format!("Starting watchdog"));
    let watchdog_board = corkboard.clone();
    let watchdog = thread::spawn(move || {
        let log = |msg: String| println!("[{}][W] {msg}", timestr());

        log(format!("Watchdog init"));

        // fetch board to get a copy of meta_info
        let board = watchdog_board.read().unwrap();
        let meta_info = board.meta_info.clone();
        drop(board);

        log(format!("Initializing tracker connection"));
        let mut tracker = Tracker::new(&meta_info).expect("Tracker unable to connect!");
        loop {
            // update peer information
            log(format!("Acquiring corkboard"));
            let mut board = watchdog_board.write().unwrap();

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
                    WATCHDOG_RETRY_INTERVAL
                }
            };

            // release board
            drop(board);

            // wait on watchdog alarm
            log(format!("Waiting {interval}s"));
            if matches!(
                watchdog_alarm.recv_timeout(Duration::from_secs(interval)),
                Err(RecvTimeoutError::Disconnected) | Ok(_)
            ) {
                break;
            }
        }
        log(format!("Exiting"));
    });

    // start workers
    println!("Starting workers");
    let workers = (0..workers)
        .map(|worker_id| {
            let corkboard = corkboard.clone();
            thread::spawn(move || {
                let log = |msg: String| println!("[{}][{worker_id}] {msg}", timestr());
                // stagger startup to prevent thrashing
                sleep((worker_id * 1000) as u64);

                log(format!("Worker init"));
                let (meta_info, peer_id) = corkboard
                    .read()
                    .map(|board| (board.meta_info.clone(), board.peer_id.clone()))
                    .unwrap();

                let mut active_connection: Option<T> = None;
                let mut uses = 0;

                loop {
                    // ! first exclusion zone
                    let peer_search_result = {
                        corkboard
                            .write()
                            .map(|mut board| {
                                // If all pieces have been acquired, exit
                                if board
                                    .pieces
                                    .iter()
                                    .all(|piece| matches!(piece.state, PieceState::Fetched(_)))
                                {
                                    log(format!("All pieces have been acquired, exiting"));
                                    return PeerSearchResult::Exit;
                                }

                                match active_connection
                                    .map(|connection| (connection.address().clone(), connection))
                                {
                                    // review existing peer connection
                                    Some((address, connection)) => {
                                        let peer = board.peers.get(&address).unwrap().clone();
                                        if peer.state == PeerState::Inactive
                                            || peer.state == PeerState::Fresh
                                        {
                                            // existing peer is deactivated or refreshed: drop the connection and pick up a new one
                                            log(format!("Existing peer is fresh or inactive, refetching"));
                                            PeerSearchResult::PromptRefetch
                                        } else if uses >= MAX_PEER_USES {
                                            // peer has been reused too many times, drop it and find another peer
                                            log(format!("Existing peer is overused, refetching"));
                                            board.peers.entry(address).and_modify(|peer| {
                                                peer.state = PeerState::Active(false)
                                            });
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
                                                matches!(
                                                    peer.state,
                                                    PeerState::Fresh | PeerState::Active(false)
                                                )
                                            })
                                            .collect::<Vec<_>>();

                                        // sort by connection speed
                                        potential_peers.sort_by(|(_, peer_a), (_, peer_b)| {
                                            peer_b
                                                .performance
                                                .unwrap_or(f64::MAX)
                                                .partial_cmp(
                                                    &peer_a.performance.unwrap_or(f64::MAX),
                                                )
                                                .unwrap_or(std::cmp::Ordering::Equal)
                                        });

                                        let potential_peer = potential_peers
                                            .first()
                                            .map(|(&address, _)| address.clone());

                                        match potential_peer {
                                            // if a peer was found, mark it as connecting
                                            Some(address) => {
                                                log(format!("Found peer {address}, attempting to connect"));
                                                board.peers.entry(address).and_modify(|peer| {
                                                    peer.state = PeerState::Connecting
                                                });
                                                PeerSearchResult::ConnectNew(address)
                                            }

                                            // if no peer was found, wait a bit and try again
                                            None => {
                                                log(format!("No peers found, waiting and retrying"));
                                                PeerSearchResult::WaitThenRefetch(EMPTY_PEER_WAIT)
                                            },
                                        }
                                    }
                                }
                            })
                            .unwrap()
                    };
                    // ! end of first mutual exclusion zone

                    active_connection = None;
                    let mut connection = match peer_search_result {
                        PeerSearchResult::ConnectNew(address) => {
                            // try to connect to the new peer
                            let connection_result = T::new(
                                address.clone(),
                                meta_info.clone(),
                                peer_id.to_string(),
                            );

                            match connection_result {

                                // if successful, mark peer as active & claimed
                                Ok(connection) => {
                                    log(format!("Connected to {address}"));
                                    uses = 0;
                                    corkboard
                                        .write()
                                        .map(|mut board| {
                                            board.peers.entry(address.clone()).and_modify(|peer| {
                                                peer.state = PeerState::Active(true)
                                            });
                                        })
                                        .unwrap();
                                    connection
                                }

                                // if unsuccessful, mark peer as errored and try another one
                                Err(err) => {
                                    log(format!("Failed to connect to {address}: {err}"));
                                    corkboard
                                        .write()
                                        .map(|mut board| {
                                            board
                                                .peers
                                                .entry(address.clone())
                                                .and_modify(|peer| peer.state = PeerState::Error);
                                        })
                                        .unwrap();
                                    continue;
                                }
                            }
                        }
                        PeerSearchResult::Reuse(connection) => connection,
                        PeerSearchResult::WaitThenRefetch(millis) => {
                            sleep(millis);
                            continue;
                        }
                        PeerSearchResult::PromptRefetch => continue,
                        PeerSearchResult::Exit => break,
                    };

                    // ! second mutual exclusion zone
                    let next_piece = {
                        corkboard
                            .write()
                            .map(|mut board| {

                                // try to find a piece to download
                                let next_piece = board
                                    .pieces
                                    .iter()
                                    .enumerate()
                                    .zip(connection.bitfield().iter())
                                    .find(|((_, piece), has_piece)| {
                                        piece.state == PieceState::Unfetched && **has_piece
                                    })
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
                                        log(format!("No pieces available, dropping peer {}", connection.address()));
                                        board
                                            .peers
                                            .entry(connection.address().clone())
                                            .and_modify(|peer| peer.state = PeerState::Superceded);
                                    }
                                };
                                next_piece
                            })
                            .unwrap()
                    };
                    // ! end of second mutual exclusion zone

                    let piece_id = match next_piece {
                        Some(piece) => piece,
                        None => continue,
                    };

                    log(format!("Downloading piece {piece_id} from {}", connection.address()));
                    // download piece, recording download time
                    let start_time = SystemTime::now();
                    let result = connection.download_piece(piece_id as u32);
                    let duration = SystemTime::now()
                        .duration_since(start_time)
                        .unwrap()
                        .as_millis() as usize;

                    // ! third mutual exclusion zone
                    let download_result = corkboard
                        .write()
                        .map(|mut board| {
                            match result {

                                // download failed
                                Err(err) => {
                                    log(format!("Failed to download piece {piece_id} from {}: {err}", connection.address()));

                                    // mark peer as errored
                                    board
                                        .peers
                                        .entry(connection.address().clone())
                                        .and_modify(|peer| peer.state = PeerState::Error);

                                    // mark piece as unfetched
                                    board.pieces.get_mut(piece_id).map(|piece| {
                                        piece.state = PieceState::Unfetched;
                                    });

                                    // try again
                                    LoopAction::Continue
                                }

                                // download succeeded
                                Ok(data) => {
                                    log(format!("Finished downloading piece {piece_id} from {}", connection.address()));

                                    // update peer performance
                                    board.peers.entry(connection.address().clone()).and_modify(
                                        |peer| {
                                            peer.benchmarks.push(Benchmark {
                                                bytes: data.len(),
                                                duration_millis: duration,
                                            });
                                            let performance = peer.update_performance();
                                            log(format!("{} has a performance of {:.4} MB/s", connection.address(), performance / MB_S ));
                                        },
                                    );

                                    // check hash
                                    board.pieces.get_mut(piece_id).map_or(
                                        LoopAction::Continue,
                                        |piece| {
                                            if piece.hash == sha1_hash(&data) {

                                                // if hash matches, store data & keep peer for next loop
                                                piece.state = PieceState::Fetched(data);
                                                LoopAction::Pass
                                            } else {
                                                log(format!("Hash of piece {piece_id} does not match, dropping data"));

                                                // if hash does not match, mark piece as unfetched
                                                piece.state = PieceState::Unfetched;
                                                LoopAction::Continue
                                            }
                                        },
                                    )
                                }
                            }
                        })
                        .unwrap();
                    // ! end of third mutual exclusion zone

                    if matches!(download_result, LoopAction::Continue) {
                        continue;
                    }

                    active_connection = Some(connection);
                    uses += 1;
                }
                log(format!("Exiting"));
            })
        })
        .collect::<Vec<_>>();

    // start seeders
    log(format!("Starting seeders"));
    let seeder_board = corkboard.clone();
    let _ = thread::spawn(move || {
        let log = |msg: String| println!("[{}][S] {msg}", timestr());

        log(format!("Seeder init"));

        let listener = TcpListener::bind(format!("0.0.0.0:{port}")).unwrap();

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let address = stream.peer_addr().unwrap();
                    let connection_board = seeder_board.clone();

                    log(format!("New connection from {address}"));
                    thread::spawn(move || {
                        let log = |msg: String| println!("[{}][{}] {msg}", timestr(), address);
                        let (meta_info, peer_id) = connection_board.read().map(|board| (board.meta_info.clone(), board.peer_id.clone())).unwrap();
                        
                        // set up connection
                        let mut connection = TcpPeer {
                            stream,
                            address,
                            meta_info,
                            peer_id,
                            bitfield: Vec::new(),
                        };

                        // recieve handshake
                        let handshake = HandshakeMessage::decode(&read_n_bytes(&mut connection.stream, 68)?)?;
                        log(format!("{address} peer id: {}", std::str::from_utf8(&handshake.peer_id).context("Peer id not bytes")?));

                        // send response handshake
                        connection.handshake()?;

                        // send bitfield
                        let bitfield = connection_board
                            .read()
                            .map(|board| board.pieces
                                .iter()
                                .map(|piece| matches!(piece.state, PieceState::Fetched(_)))
                            .collect::<Vec<_>>()
                        ).unwrap();
                        let bitfield_message = PeerMessage::Bitfield(BitfieldMessage { bitfield });
                        connection.send_peer_message(&bitfield_message)?;

                        // wait for interested
                        loop {
                            match connection.await_peer_message()? {
                                PeerMessage::Interested => break,
                                PeerMessage::NotInterested => return Err::<(), BitTorrentError>(bterror!("{} was not interested", address)),
                                _ => {}
                            }
                        }

                        // send unchoke
                        connection.send_peer_message(&PeerMessage::Unchoke)?;

                        // respond to data requests
                        loop {
                            match connection.await_peer_message()? {
                                PeerMessage::Request(request) => {
                                    let piece_id = request.index;
                                    let chunk_data: Vec<u8> = connection_board.read().map(|board| {
                                        let piece_data = match board.pieces.get(piece_id as usize) {
                                            Some(Piece { state: PieceState::Fetched(data), .. }) => data,
                                            _ => return Err(bterror!("Piece {piece_id} is not fetched")),
                                        };
                                        Ok(piece_data
                                            .get((request.begin as usize)..((request.begin + request.length) as usize))
                                            .context("Invalid chunk data")?.into_iter().copied().collect())
                                    }).unwrap()?;
                                    connection.send_peer_message(&PeerMessage::Piece(PieceMessage {
                                        index: request.index,
                                        begin: request.begin,
                                        block: chunk_data,
                                    }))?;
                                }
                                _ => {}
                            }
                        }
                    });
                }
                Err(err) => {
                    log(format!("Tcp Client connect error: {}", err))
                }
            }
        }
    });

    // wait for workers to finish
    log(format!("Waiting for workers to finish"));
    workers
        .into_iter()
        .for_each(|worker| worker.join().unwrap());

    // send kill signal to watchdog (may not be directly necessary; the channel dropping should kill the watchdog automatically)
    log(format!("Killing watchdog"));
    watchdog_notify.send(()).unwrap();
    watchdog.join().unwrap();

    // send kill signal to monitor
    log(format!("Killing monitor"));
    monitor_notify.send(()).unwrap();
    monitor.join().unwrap();

    // collect data
    log(format!("Collecting data"));
    let board = corkboard.read().unwrap();
    let data = board
        .pieces
        .iter()
        .map(|piece| match &piece.state {
            PieceState::Fetched(data) => Ok(data),
            _ => Err(bterror!("Unfetched piece data remains!")),
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<&u8>>();

    // this is really memory inefficient... need to figure out a better way to save and collect data
    log(format!("Done"));
    Ok(data.into_iter().copied().collect())
}
