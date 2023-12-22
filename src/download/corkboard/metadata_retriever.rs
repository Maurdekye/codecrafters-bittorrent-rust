use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    thread::Scope,
};

use crate::{
    error::BitTorrentError,
    info::MetaInfo,
    multithread::SyncDoor,
    peer::{self, PeerConnection},
    torrent_source::TorrentSource,
    util::timestr,
};

use super::{peer_locator::{PeerLocator, self}, Config};

#[derive(Clone, Debug)]
pub enum Event {
    Start,
    Worker(usize, WorkerEvent),
    WorkersFinished,
    MetaInfoLocated(MetaInfo),
    Finish,
}

#[derive(Clone, Debug)]
pub enum WorkerEvent {
    Start,
    Peer(SocketAddr, PeerEvent),
    Finish,
}

#[derive(Clone, Debug)]
pub enum PeerEvent {
    Retrieved,
    ReadmittedEarly,
    Connected,
    ConnectFailed,
    PeerConnection(peer::Event),
    MetaInfoLocated(MetaInfo),
    Readmitted,
    MetaInfoStored,
}

pub fn retrieve_metadata<'a, F: Fn(peer::Event) + Send + Clone, T: PeerConnection<F>>(
    torrent_source: TorrentSource,
    peer_locator: PeerLocator<impl Fn(peer_locator::Event) + Send + Clone>,
    scope: &'a Scope<'a, '_>,
    config: &Config,
    event_callback: impl Fn(Event) + Send + Clone + 'a,
) -> Result<MetaInfo, BitTorrentError> {
    let verbose = config.verbose;
    let log = move |msg: String| {
        if verbose {
            println!("[{}] {msg}", timestr())
        }
    };

    event_callback(Event::Start);

    let meta_info = Arc::new(RwLock::new(None::<MetaInfo>));
    let peer_search_killswitch = Arc::new(AtomicBool::new(false));
    let door = Arc::new(SyncDoor::new());
    door.close().unwrap();
    for i in 0..config.workers {
        let meta_info = meta_info.clone();
        let peer_locator = peer_locator.clone();
        let torrent_source = torrent_source.clone();
        let config = config.clone();
        let peer_search_killswitch = peer_search_killswitch.clone();
        let door = door.clone();
        let event_callback = event_callback.clone();
        scope.spawn(move || {
            let event_callback = move |event| event_callback(Event::Worker(i, event));
            event_callback(WorkerEvent::Start);
            for peer in peer_locator.receiver.iter() {
                let event_callback = event_callback.clone();
                let event_callback = move |event| event_callback(WorkerEvent::Peer(peer.clone(), event));
                event_callback(PeerEvent::Retrieved);
                {
                    if meta_info.read().unwrap().is_some() {
                        peer_locator.sender.send(peer).unwrap();
                        event_callback(PeerEvent::ReadmittedEarly);
                        break;
                    }
                }
                log(format!("[{}] Connecting to peer", peer));
                let peer_search_killswitch = peer_search_killswitch.clone();
                let peer_connection = {
                    let event_callback = event_callback.clone();
                    T::new(
                        peer,
                        torrent_source.clone(),
                        config.peer_id.clone(),
                        config.port,
                        config.verbose,
                        peer_search_killswitch.clone(),
                        move |event| event_callback(PeerEvent::PeerConnection(event)),
                    )
                };
                let found_meta_info: MetaInfo = match peer_connection {
                    Ok(peer_connection) => {
                        event_callback(PeerEvent::Connected);
                        peer_connection
                            .meta_info()
                            .expect("Meta info was not included")
                            .clone()
                    }
                    Err(err) => {
                        event_callback(PeerEvent::ConnectFailed);
                        log(format!("[{}] Failed to connect to peer: {}", peer, err));
                        continue;
                    }
                };
                event_callback(PeerEvent::MetaInfoLocated(found_meta_info.clone()));
                log(format!("[{}] Metadata located from peer", peer));
                peer_locator.sender.send(peer).unwrap();
                event_callback(PeerEvent::Readmitted);
                {
                    if !meta_info.read().unwrap().is_some() {
                        meta_info.write().unwrap().replace(found_meta_info);
                        door.open().unwrap();
                        event_callback(PeerEvent::MetaInfoStored);
                    }
                }
                break;
            }
            event_callback(WorkerEvent::Finish);
            peer_search_killswitch.store(true, Ordering::Relaxed);
            log(format!("Exiting"));
        });
    }
    door.wait().unwrap();
    event_callback(Event::WorkersFinished);
    let meta_info = meta_info
        .read()
        .unwrap()
        .clone()
        .expect("Meta info was not located");
    event_callback(Event::MetaInfoLocated(meta_info.clone()));
    event_callback(Event::Finish);
    Ok(meta_info)
}
