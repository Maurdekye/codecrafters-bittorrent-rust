use std::{
    net::SocketAddr,
    ops::ControlFlow,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, channel, RecvTimeoutError},
        Arc, Mutex,
    },
    thread::Scope,
    time::Duration,
};

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::{
    tracker::{dht::{Dht, self}, multimodal::Tracker, self},
    util::timestr,
};

use super::Config;

/// maximum duration in between tracker queries
const MAX_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// number of worker threads used in exploring the dht network
const DHT_WORKERS: usize = 64;

#[derive(Clone, Debug)]
pub enum Event {
    Start,
    Tracker(TrackerEvent),
    Dht(DhtEvent),
    Initialized,
    Killed,
}

#[derive(Clone, Debug)]
pub enum TrackerEvent {
    Start,
    PeerLocated(SocketAddr),
    Wait(Duration),
    TrackersExhausted,
    Killed,
    Finish,
}

#[derive(Clone, Debug)]
pub enum DhtEvent {
    Start,
    PeerLocated(SocketAddr),
    Killed,
    Finish,
}

#[derive(Debug)]
pub struct PeerLocator<F>
where
    F: Fn(Event) + Send + Clone,
{
    pub sender: Sender<SocketAddr>,
    pub receiver: Receiver<SocketAddr>,
    dht_killswitch: Arc<AtomicBool>,
    tracker_notify: mpsc::Sender<()>,
    event_callback: F,
}

impl<F: Fn(Event) + Send + Clone> Clone for PeerLocator<F> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
            dht_killswitch: self.dht_killswitch.clone(),
            tracker_notify: self.tracker_notify.clone(),
            event_callback: self.event_callback.clone(),
        }
    }
}

impl<F: Fn(Event) + Send + Clone> PeerLocator<F> {
    pub fn new<'a>(
        tracker: Tracker<impl Fn(tracker::multimodal::Event) + Send + Clone + 'a>,
        mut dht: Dht<impl Fn(dht::Event) + Send + Clone + 'a>,
        scope: &'a Scope<'a, '_>,
        config: &Config,
        event_callback: F,
    ) -> Self 
    where F: 'a {
        let verbose = config.verbose;
        let log = move |msg: String| {
            if verbose {
                println!("[{}] {msg}", timestr())
            }
        };

        event_callback(Event::Start);

        let (peer_send, peer_recv) = unbounded();

        let dht_killswitch = Arc::new(AtomicBool::new(false));
        let (tracker_notify, tracker_alarm) = channel::<()>();

        // spawn tracker
        {
            let peer_send = peer_send.clone();
            let alarm = Arc::new(Mutex::new(tracker_alarm));
            let event_callback = event_callback.clone();
            scope.spawn(move || {
                let event_callback = |event| event_callback(Event::Tracker(event));
                event_callback(TrackerEvent::Start);
                log(format!("Initializing tracker"));
                for (new_peer, should_wait) in tracker {
                    event_callback(TrackerEvent::PeerLocated(new_peer.clone()));
                    log(format!("New peer from tracker: {new_peer}"));
                    peer_send.send(new_peer).unwrap();

                    // wait if requested to do so
                    if let ControlFlow::Break(wait_time) = should_wait {
                        let wait_time = wait_time.min(MAX_INTERVAL);
                        event_callback(TrackerEvent::Wait(wait_time));
                        log(format!("Waiting {}s", wait_time.as_secs()));
                        if matches!(
                            alarm.lock().unwrap().recv_timeout(wait_time),
                            Err(RecvTimeoutError::Disconnected) | Ok(_)
                        ) {
                            event_callback(TrackerEvent::Killed);
                            event_callback(TrackerEvent::Finish);
                            return;
                        }
                    }
                }
                event_callback(TrackerEvent::TrackersExhausted);
                event_callback(TrackerEvent::Finish);
                log(format!("Trackers exhausted"));
            });
        }

        // spawn dht
        {
            let peer_send = peer_send.clone();
            let killswitch = dht_killswitch.clone();
            let event_callback = event_callback.clone();
            scope.spawn(move || {
                let event_callback = |event| event_callback(Event::Dht(event));
                event_callback(DhtEvent::Start);
                log(format!("Initializing DHT"));
                let dht_peers = dht.initialize(DHT_WORKERS, scope);
                for peer in dht_peers {
                    event_callback(DhtEvent::PeerLocated(peer.clone()));
                    // log(format!("New peer from dht: {peer}"));
                    peer_send.send(peer).unwrap();
                    if killswitch.load(Ordering::Relaxed) {
                        event_callback(DhtEvent::Killed);
                        event_callback(DhtEvent::Finish);
                        return;
                    }
                }
                event_callback(DhtEvent::Finish);
            });
        }

        event_callback(Event::Initialized);

        PeerLocator {
            sender: peer_send,
            receiver: peer_recv,
            dht_killswitch,
            tracker_notify,
            event_callback,
        }
    }

    pub fn kill(&self) {
        (self.event_callback)(Event::Killed);
        self.tracker_notify.send(()).unwrap_or_default();
        self.dht_killswitch.store(true, Ordering::Relaxed);
    }
}
