use std::{
    collections::HashMap,
    fs::create_dir_all,
    net::SocketAddr,
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, RecvTimeoutError},
        Arc, Mutex, RwLock,
    },
    thread::{self},
    time::Duration,
};

use crate::{
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    multithread::SyncDoor,
    peer::PeerConnection,
    torrent_source::TorrentSource,
    tracker::{dht::Dht, multimodal::Tracker},
    util::timestr,
};

use crossbeam::channel::unbounded;

mod locator;
mod monitor;
mod seeder;
mod watchdog;
mod worker;

/// maximum duration in between tracker queries
const MAX_INTERVAL: Duration = Duration::from_secs(2 * 60);

const DHT_WORKERS: usize = 64;

pub struct Corkboard {
    pub meta_info: MetaInfo,
    pub pieces: Vec<Piece>,
    pub peers: HashMap<SocketAddr, Peer>,
    pub finishing: Arc<AtomicBool>,
}

impl Corkboard {
    pub fn new(meta_info: MetaInfo) -> Result<Self, BitTorrentError> {
        Ok(Self {
            pieces: meta_info
                .info
                .pieces
                .iter()
                .cloned()
                .map(Piece::new)
                .collect(),
            peers: HashMap::new(),
            finishing: Arc::new(AtomicBool::new(false)),
            meta_info,
        })
    }
}

pub struct Piece {
    pub hash: [u8; 20],
    pub state: PieceState,
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
pub enum PieceState {
    Unfetched,
    InProgress,
    Fetched(PieceLocation),
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum PieceLocation {
    Memory(Vec<u8>),
    Disk(PathBuf),
}

impl PieceLocation {
    fn load(self) -> Result<Vec<u8>, BitTorrentError> {
        match self {
            Self::Memory(data) => Ok(data),
            Self::Disk(path) => Ok(std::fs::read(path)?),
        }
    }

    fn save_to_disk(path: PathBuf, data: Vec<u8>) -> Result<Self, BitTorrentError> {
        create_dir_all(path.parent().unwrap())?;
        std::fs::write(path.clone(), data)?;
        Ok(Self::Disk(path))
    }
}

#[derive(Clone)]
pub struct Peer {
    pub state: PeerState,
    pub benchmarks: Vec<Benchmark>,
    pub performance: Option<f64>,
    pub connection_attempts: usize,
}

impl Peer {
    fn new() -> Self {
        Peer {
            state: PeerState::Fresh,
            benchmarks: Vec::new(),
            performance: None,
            connection_attempts: 0,
        }
    }

    /// Update the peer's benchmark performance rating, for use in ranking their fitness to download a torrent with
    pub fn update_performance(&mut self) -> f64 {
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
pub enum PeerState {
    Fresh,
    Connecting,
    Active(bool),
    Inactive,
    Superceded,
    #[allow(unused)]
    Error,
}

#[derive(Clone)]
pub struct Benchmark {
    bytes: usize,
    duration_millis: usize,
}

#[derive(Clone)]
pub struct Config {
    pub workers: usize,
    pub verbose: bool,
    pub temp_path: PathBuf,
    pub peer_id: String,
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workers: 4,
            verbose: false,
            temp_path: Path::new("tmp/in-progress/").to_path_buf(),
            peer_id: "00112233445566778899".to_string(),
            port: 6881,
        }
    }
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
    torrent_source: TorrentSource,
    config: Config,
) -> Result<(Vec<u8>, MetaInfo), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}] {msg}", timestr())
        }
    };

    let tracker = Tracker::new(torrent_source.clone(), config.peer_id.clone(), config.port)?;

    let (data, meta_info) = thread::scope(|download_scope| {
        let (peer_send, peer_recv) = unbounded();

        let dht_killswitch = Arc::new(AtomicBool::new(false));
        let (tracker_notify, tracker_alarm) = channel::<()>();

        // spawn tracker
        {
            let peer_send = peer_send.clone();
            let alarm = Arc::new(Mutex::new(tracker_alarm));
            download_scope.spawn(move || {
                log(format!("Initializing tracker"));
                for (new_peer, should_wait) in tracker {
                    log(format!("New peer from tracker: {new_peer}"));
                    peer_send.send(new_peer).unwrap();

                    // wait if requested to do so
                    if let ControlFlow::Break(wait_time) = should_wait {
                        let wait_time = wait_time.min(MAX_INTERVAL);
                        log(format!("Waiting {}s", wait_time.as_secs()));
                        if matches!(
                            alarm.lock().unwrap().recv_timeout(wait_time),
                            Err(RecvTimeoutError::Disconnected) | Ok(_)
                        ) {
                            break;
                        }
                    }
                }
                log(format!("Trackers exhausted"));
            });
        }

        // spawn dht
        {
            let peer_send = peer_send.clone();
            let killswitch = dht_killswitch.clone();
            let mut dht = Dht::new(
                torrent_source.clone(),
                config.peer_id.clone().into(),
                config.verbose,
            );
            download_scope.spawn(move || {
                log(format!("Initializing DHT"));
                let dht_peers = dht.initialize(DHT_WORKERS, download_scope);
                for peer in dht_peers {
                    // log(format!("New peer from dht: {peer}"));
                    peer_send.send(peer).unwrap();
                    if killswitch.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
        }

        thread::scope(|metadata_discovery_scope| {
            // get meta info
            let peer_list = Arc::new(Mutex::new(Vec::new()));
            let meta_info = match torrent_source {
                TorrentSource::File(meta_info) => meta_info,
                TorrentSource::Magnet(_) => {
                    if !config.verbose {
                        println!("Retrieving metadata");
                    } else {
                        log(format!("Retrieving metadata"));
                    }
                    let meta_info = Arc::new(RwLock::new(None::<MetaInfo>));
                    let peer_search_killswitch = Arc::new(AtomicBool::new(false));
                    let door = Arc::new(SyncDoor::new());
                    door.close().unwrap();
                    for _ in 0..config.workers {
                        let meta_info = meta_info.clone();
                        let peer_recv = peer_recv.clone();
                        let torrent_source = torrent_source.clone();
                        let config = config.clone();
                        let peer_list = peer_list.clone();
                        let peer_search_killswitch = peer_search_killswitch.clone();
                        let door = door.clone();
                        metadata_discovery_scope.spawn(move || {
                            for peer in peer_recv {
                                {
                                    if meta_info.read().unwrap().is_some() {
                                        break;
                                    }
                                }
                                log(format!("[{}] Connecting to peer", peer));
                                let peer_search_killswitch = peer_search_killswitch.clone();
                                let found_meta_info: MetaInfo = match T::new(
                                    peer,
                                    torrent_source.clone(),
                                    config.peer_id.clone(),
                                    config.port,
                                    config.verbose,
                                    peer_search_killswitch.clone(),
                                ) {
                                    Ok(peer_connection) => peer_connection
                                        .meta_info()
                                        .expect("Meta info was not included")
                                        .clone(),
                                    Err(err) => {
                                        log(format!(
                                            "[{}] Failed to connect to peer: {}",
                                            peer, err
                                        ));
                                        continue;
                                    }
                                };
                                log(format!("[{}] Metadata located from peer", peer));
                                peer_list.lock().unwrap().push(peer);
                                {
                                    if meta_info.read().unwrap().is_some() {
                                        break;
                                    } else {
                                        meta_info.write().unwrap().replace(found_meta_info);
                                        door.open().unwrap();
                                    }
                                }
                            }
                            peer_search_killswitch.store(true, Ordering::Relaxed);
                            log(format!("Exiting"));
                        });
                    }
                    door.wait().unwrap();
                    let meta_info = meta_info
                        .read()
                        .unwrap()
                        .clone()
                        .expect("Meta info was not located");
                    meta_info
                }
            };

            // create corkboard
            log(format!("Initializing Corkboard"));
            let corkboard: Arc<RwLock<Corkboard>> =
                Arc::new(RwLock::new(Corkboard::new(meta_info.clone())?));
            if let Ok(mut board) = corkboard.write() {
                board.peers.extend(
                    peer_list
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|peer| (peer.clone(), Peer::new())),
                );
            }

            if !config.verbose {
                println!("Starting download");
            } else {
                log(format!(
                    "Preparing to download {} pieces",
                    corkboard.clone().read().unwrap().pieces.len(),
                ));
            }

            // spawn subtasks
            log(format!("Starting subtasks"));
            let tasks = [monitor::monitor, seeder::seeder].map(|task| {
                let corkboard = corkboard.clone();
                let (notify, alarm) = channel();
                let meta_info = meta_info.clone();
                let config = config.clone();
                download_scope.spawn(move || task(corkboard, alarm, meta_info, config));
                notify
            });
            {
                let corkboard = corkboard.clone();
                let config = config.clone();
                download_scope.spawn(move || watchdog::watchdog(corkboard, peer_recv, config));
            }

            // start workers
            log(format!("Starting workers"));
            let workers = (0..config.workers)
                .map(|worker_id| {
                    let corkboard = corkboard.clone();
                    let meta_info = meta_info.clone();
                    let config = config.clone();
                    download_scope
                        .spawn(move || worker::worker::<T>(corkboard, worker_id, meta_info, config))
                })
                .collect::<Vec<_>>();

            // wait for workers to finish
            log(format!("Waiting for workers to finish"));
            for worker in workers {
                worker.join().unwrap()?;
            }

            if !config.verbose {
                println!("Finished downloading");
            }

            // send kill signals to subtasks
            log(format!("Killing subtasks"));
            for alarm in tasks {
                alarm.send(()).unwrap();
            }
            tracker_notify.send(()).unwrap_or_default();
            dht_killswitch.store(true, Ordering::Relaxed);

            // coallate data
            log(format!("Coallating data"));
            let data = corkboard.read().map(|board| {
                Ok::<_, BitTorrentError>(board
                    .pieces
                    .iter()
                    .map(|piece| match &piece.state {
                        PieceState::Fetched(data_location) => data_location.clone().load(),
                        _ => Err(bterror!("Unfetched piece data remains!")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<u8>>())
            })?.unwrap();

            Ok::<_, BitTorrentError>((data, meta_info))
        })
    })?;

    log(format!("Done"));

    Ok((data, meta_info))
}
