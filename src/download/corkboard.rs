use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        mpsc::{channel, Receiver},
        Arc, RwLock,
    },
    thread::{self, JoinHandle},
};

use crate::{bterror, error::BitTorrentError, info::MetaInfo, peer::PeerConnection, util::timestr};

use self::watchdog::watchdog;

mod monitor;
mod seeder;
mod watchdog;
mod worker;

pub struct Corkboard {
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub port: u16,
    pub pieces: Vec<Piece>,
    pub peers: HashMap<SocketAddr, Peer>,
}

impl Corkboard {
    pub fn new(meta_info: MetaInfo, peer_id: String, port: u16) -> Result<Self, BitTorrentError> {
        Ok(Self {
            pieces: meta_info.pieces()?.into_iter().map(Piece::new).collect(),
            peers: HashMap::new(),
            meta_info,
            peer_id,
            port,
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
    Fetched(Vec<u8>),
}

#[derive(Clone)]
pub struct Peer {
    pub state: PeerState,
    pub benchmarks: Vec<Benchmark>,
    pub performance: Option<f64>,
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
    Active(bool),
    Inactive,
    Superceded,
    Connecting,
    Error,
}

#[derive(Clone)]
pub struct Benchmark {
    bytes: usize,
    duration_millis: usize,
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

    // Preparation

    // create corkboard
    log(format!("Initializing Corkboard"));
    let corkboard: Arc<RwLock<Corkboard>> = Arc::new(RwLock::new(Corkboard::new(
        meta_info,
        peer_id.to_string(),
        port,
    )?));

    log(format!(
        "Preparing to download {} pieces",
        corkboard.clone().read().unwrap().pieces.len(),
    ));

    // Spawn subtasks

    let start_task =
        |task: fn(Arc<RwLock<Corkboard>>, Receiver<()>) -> Result<(), BitTorrentError>| {
            let corkboard = corkboard.clone();
            let (notify, alarm) = channel();
            let handle = thread::spawn(move || task(corkboard, alarm));
            (handle, notify)
        };

    // start subtasks
    log(format!("Starting subtasks"));
    let tasks = [
        start_task(|board, alarm| monitor::monitor(board, alarm)),
        start_task(|board, alarm| watchdog::watchdog(board, alarm)),
        start_task(|board, alarm| seeder::seeder(board, alarm)),
    ];

    // start workers
    log(format!("Starting workers"));
    let workers = (0..workers)
        .map(|worker_id| {
            let corkboard = corkboard.clone();
            thread::spawn(move || worker::worker::<T>(corkboard, worker_id))
        })
        .collect::<Vec<_>>();

    // Block on subtasks

    // wait for workers to finish
    log(format!("Waiting for workers to finish"));
    for worker in workers {
        worker.join().unwrap()?;
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
    // this is really memory inefficient... need to figure out a better way to save and collect data
    log(format!("Coallating data"));
    let data = corkboard
        .read()
        .map(|board| {
            Ok::<_, BitTorrentError>(
                board
                    .pieces
                    .iter()
                    .map(|piece| match &piece.state {
                        PieceState::Fetched(data) => Ok(data),
                        _ => Err(bterror!("Unfetched piece data remains!")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<u8>>(),
            )
        })
        .unwrap()?;

    log(format!("Done"));

    Ok(data)
}
