use std::{
    collections::HashMap,
    fs::create_dir_all,
    iter::once,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, mpsc::channel, Arc, RwLock},
    thread::{self},
};

use crate::{
    bterror, error::BitTorrentError, info::MetaInfo, peer::PeerConnection,
    torrent_source::TorrentSource, tracker::multimodal::Tracker, util::timestr,
};

mod monitor;
mod seeder;
mod watchdog;
mod worker;

pub struct Corkboard {
    pub meta_info: MetaInfo,
    pub pieces: Vec<Piece>,
    pub peers: HashMap<SocketAddr, Peer>,
    pub finishing: Arc<AtomicBool>,
}

impl Corkboard {
    pub fn new(meta_info: MetaInfo) -> Result<Self, BitTorrentError> {
        Ok(Self {
            pieces: meta_info.pieces()?.into_iter().map(Piece::new).collect(),
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

    let mut tracker = Tracker::new(torrent_source.clone(), config.peer_id.clone(), config.port)?;

    // get meta info
    let meta_info = match torrent_source {
        TorrentSource::File(meta_info) => meta_info,
        TorrentSource::Magnet(_) => {
            if !config.verbose {
                println!("Retrieving metadata");
            } else {
                log(format!("Retrieving metadata"));
            }
            (|| loop {
                for peer in tracker.query().0 {
                    match T::new(
                        peer,
                        torrent_source.clone(),
                        config.peer_id.clone(),
                        config.port,
                        config.verbose,
                        Arc::new(AtomicBool::new(false)),
                    ) {
                        Ok(peer) => {
                            return Ok::<_, BitTorrentError>(
                                peer.meta_info()
                                    .ok_or(bterror!("Meta info was not included"))?
                                    .clone(),
                            )
                        }
                        Err(err) => {
                            log(format!("[{}] Disconnected from peer: {}", peer, err));
                        },
                    }
                }
            })()?
        }
    };

    // create corkboard
    log(format!("Initializing Corkboard"));
    let corkboard: Arc<RwLock<Corkboard>> =
        Arc::new(RwLock::new(Corkboard::new(meta_info.clone())?));

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
    let tasks = [monitor::monitor, seeder::seeder]
        .map(|task| {
            let corkboard = corkboard.clone();
            let (notify, alarm) = channel();
            let meta_info = meta_info.clone();
            let config = config.clone();
            let handle = thread::spawn(move || task(corkboard, alarm, meta_info, config));
            (handle, notify)
        })
        .into_iter()
        .chain(once({
            let corkboard = corkboard.clone();
            let (notify, alarm) = channel();
            let config = config.clone();
            let handle =
                thread::spawn(move || watchdog::watchdog(corkboard, alarm, tracker, config));
            (handle, notify)
        }));

    // start workers
    log(format!("Starting workers"));
    let workers = (0..config.workers)
        .map(|worker_id| {
            let corkboard = corkboard.clone();
            let meta_info = meta_info.clone();
            let config = config.clone();
            thread::spawn(move || worker::worker::<T>(corkboard, worker_id, meta_info, config))
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
    let (handles, alarms): (Vec<_>, Vec<_>) = tasks.into_iter().unzip();
    for alarm in alarms {
        alarm.send(()).unwrap();
    }
    for handle in handles {
        handle.join().unwrap()?;
    }

    // coallate data
    log(format!("Coallating data"));
    let data = corkboard
        .read()
        .map(|board| {
            Ok::<_, BitTorrentError>(
                board
                    .pieces
                    .iter()
                    .map(|piece| match &piece.state {
                        PieceState::Fetched(data_location) => data_location.clone().load(),
                        _ => Err(bterror!("Unfetched piece data remains!")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<u8>>(),
            )
        })
        .unwrap()?;

    log(format!("Done"));

    Ok((data, meta_info))
}
