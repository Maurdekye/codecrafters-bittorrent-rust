use std::{
    borrow::Cow,
    collections::HashMap,
    fs::create_dir_all,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, mpsc::channel, Arc, Mutex, RwLock},
    thread::Scope,
};

use crate::{
    bterror,
    data_proxy::DataProxy,
    download::corkboard::{metadata_retriever::retrieve_metadata, peer_locator::PeerLocator},
    error::BitTorrentError,
    info::MetaInfo,
    multithread::SyncDoor,
    peer::{PeerConnection, self},
    torrent_source::TorrentSource,
    tracker::{dht::Dht, multimodal::Tracker},
    util::timestr,
};

mod metadata_retriever;
mod monitor;
mod peer_locator;
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
    pub fn load(&self) -> Result<Cow<Vec<u8>>, BitTorrentError> {
        match self {
            Self::Memory(data) => Ok(Cow::Borrowed(data)),
            Self::Disk(path) => Ok(Cow::Owned(std::fs::read(path)?)),
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
            workers: 32,
            verbose: false,
            temp_path: Path::new("tmp/in-progress/").to_path_buf(),
            peer_id: "00112233445566778899".to_string(),
            port: 6881,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Start,
    PeerLocator(peer_locator::Event),
    MetadataRetrieval(metadata_retriever::Event),
    MetaInfoLocated(MetaInfo),
    Tracker(crate::tracker::multimodal::Event),
    Dht(crate::tracker::dht::Event),
    DownloadInitialized,
    Seeder(seeder::Event),
    Watchdog(watchdog::Event),
    Worker(usize, worker::Event),
    SubtaskInitialized,
    WorkersInitialized,
    DownloadFinished,
    DataCoallated,
    Finish,
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
pub fn corkboard_download<'a, F: Fn(peer::Event) + Send + Clone, T: PeerConnection<F>>(
    torrent_source: TorrentSource,
    download_scope: &'a Scope<'a, '_>,
    config: Config,
    event_callback: impl Fn(Event) + Send + Clone + 'a,
) -> Result<(DataProxy, MetaInfo), BitTorrentError> {
    let verbose = config.verbose.clone();
    let log = move |msg: String| {
        if verbose {
            println!("[{}] {msg}", timestr())
        }
    };

    event_callback(Event::Start);

    let download_finished = Arc::new(SyncDoor::new());
    download_finished.close().unwrap();

    // initialize peer sources
    let tracker = {
        let event_callback = event_callback.clone();
        Tracker::new(
            torrent_source.clone(),
            config.peer_id.clone(),
            config.port,
            move |event| event_callback(Event::Tracker(event)),
        )?
    };
    let dht = {
        let event_callback = event_callback.clone();
        Dht::new(
            torrent_source.clone(),
            config.peer_id.clone().into(),
            config.verbose,
            move |event| event_callback(Event::Dht(event)),
        )
    };

    let peer_locator = {
        let event_callback = event_callback.clone();
        PeerLocator::new(tracker, dht, download_scope, &config, move |event| {
            event_callback(Event::PeerLocator(event))
        })
    };

    // get meta info
    let peer_list = Arc::new(Mutex::new(Vec::new()));
    let meta_info = match torrent_source {
        TorrentSource::File(meta_info) => meta_info,
        ref torrent_source @ TorrentSource::Magnet(_) => {
            println!("Retrieving metadata");
            log(format!("Retrieving metadata"));
            let event_callback = event_callback.clone();
            retrieve_metadata::<T>(
                torrent_source.clone(),
                peer_locator.clone(),
                download_scope,
                &config,
                move |event| event_callback(Event::MetadataRetrieval(event)),
            )?
        }
    };

    event_callback(Event::MetaInfoLocated(meta_info.clone()));

    // create corkboard
    log(format!("Initializing Corkboard"));
    let corkboard = Arc::new(RwLock::new(Corkboard::new(meta_info.clone())?));
    if let Ok(mut board) = corkboard.write() {
        board.peers.extend(
            peer_list
                .lock()
                .unwrap()
                .iter()
                .map(|peer: &SocketAddr| (peer.clone(), Peer::new())),
        );
    }

    event_callback(Event::DownloadInitialized);
    println!("Downloading {}", meta_info.info.name);
    log(format!(
        "Preparing to download {} pieces",
        corkboard.clone().read().unwrap().pieces.len(),
    ));

    // spawn subtasks
    log(format!("Starting subtasks"));
    let mut tasks = Vec::new();
    {
        let corkboard = corkboard.clone();
        let (notify, alarm) = channel();
        let meta_info = meta_info.clone();
        let config = config.clone();
        let event_callback = event_callback.clone();
        download_scope.spawn(move || {
            seeder::seeder(corkboard, alarm, meta_info, config, move |event| {
                event_callback(Event::Seeder(event))
            })
        });
        tasks.push(notify);
    }
    {
        let corkboard = corkboard.clone();
        let (notify, alarm) = channel();
        let meta_info = meta_info.clone();
        let config = config.clone();
        download_scope.spawn(move || monitor::monitor(corkboard, alarm, meta_info, config));
        tasks.push(notify);
    }
    {
        let corkboard = corkboard.clone();
        let config = config.clone();
        let peer_recv = peer_locator.receiver.clone();
        let event_callback = event_callback.clone();
        download_scope.spawn(move || {
            watchdog::watchdog(corkboard, peer_recv, config, move |event| {
                event_callback(Event::Watchdog(event))
            })
        });
    }

    event_callback(Event::SubtaskInitialized);

    // start workers
    log(format!("Starting workers"));
    for worker_id in 0..config.workers {
        let corkboard = corkboard.clone();
        let meta_info = meta_info.clone();
        let download_finished = download_finished.clone();
        let config = config.clone();
        let event_callback = event_callback.clone();
        download_scope.spawn(move || {
            worker::worker::<T>(
                corkboard,
                worker_id,
                meta_info,
                download_finished,
                config,
                move |event| event_callback(Event::Worker(worker_id, event)),
            )
        });
    }

    event_callback(Event::WorkersInitialized);

    // wait for workers to finish
    log(format!("Waiting for workers to finish"));
    download_finished.wait().unwrap();

    event_callback(Event::DownloadFinished);

    println!("Finished downloading");

    // send kill signals to subtasks
    log(format!("Killing subtasks"));
    for alarm in tasks {
        alarm.send(()).unwrap();
    }
    peer_locator.kill();

    // coallate data
    log(format!("Coallating data"));
    let data = corkboard
        .read()
        .unwrap()
        .pieces
        .iter()
        .map(|piece| match &piece.state {
            PieceState::Fetched(data_location) => Ok(data_location.clone()),
            _ => Err(bterror!("Unfetched piece data remains!")),
        })
        .collect::<Result<Vec<PieceLocation>, _>>()?;

    let proxy = DataProxy::new(data, meta_info.info.piece_length, meta_info.length());

    event_callback(Event::DataCoallated);
    event_callback(Event::Finish);

    log(format!("Done"));

    Ok((proxy, meta_info))
}
