use std::{
    collections::VecDeque,
    iter::empty,
    net::{SocketAddr, UdpSocket},
    ops::ControlFlow,
    time::{Duration, SystemTime},
};

use anyhow::Context;

use lazy_static::lazy_static;
use regex::Regex;

use crate::{
    bencode::BencodedValue,
    bterror,
    error::BitTorrentError,
    peer::message::Codec,
    torrent_source::TorrentSource,
    tracker::TrackerResponse,
    util::{querystring_encode, read_datagram},
};

const TRACKER_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

lazy_static! {
    static ref UDP_TRACKER_RE: Regex = Regex::new(r"udp://([^:]+:\d+)(/announce)?").unwrap();
}

#[derive(Clone, Debug)]
pub enum Event {
    Created,
    CycledTracker(TrackerConnectionTypeEvent),
    Query(TrackerConnectionTypeEvent, QueryEvent),
}

#[derive(Clone, Debug)]
pub enum TrackerConnectionTypeEvent {
    Http(String),
    Udp(SocketAddr),
    None,
}

#[derive(Clone, Debug)]
pub enum QueryEvent {
    Start,
    Http(HttpQueryEvent),
    Udp(UdpQueryEvent),
}

#[derive(Clone, Debug)]
pub enum HttpQueryEvent {
    Start,
    Queried,
    SuccessResponse(PeerAnnounceResponse),
    FailureResponse(String),
}

#[derive(Clone, Debug)]
pub enum UdpQueryEvent {
    Start,
    Connect,
    Announce,
    AnnounceResponse(PeerAnnounceResponse),
}

#[derive(Clone, Debug)]
pub struct PeerAnnounceResponse {
    pub interval: Duration,
    pub peers: Vec<SocketAddr>,
}

impl TrackerConnectionTypeEvent {
    fn from_tracker_connection(connection: &TrackerConnection) -> TrackerConnectionTypeEvent {
        match connection {
            TrackerConnection::Http(url) => TrackerConnectionTypeEvent::Http(url.clone()),
            TrackerConnection::Udp(url) => {
                TrackerConnectionTypeEvent::Udp(url.connection.peer_addr().unwrap())
            }
            TrackerConnection::None => TrackerConnectionTypeEvent::None,
        }
    }
}

pub struct Tracker<F>
where
    F: Fn(Event) + Send + Clone,
{
    pub torrent_source: TorrentSource,
    active_connection: TrackerConnection,
    peers: VecDeque<SocketAddr>,
    pub trackers: VecDeque<String>,
    pub peer_id: String,
    pub port: u16,
    event_callback: F,
}

impl<F: Fn(Event) + Send + Clone> Tracker<F> {
    pub fn new(
        torrent_source: TorrentSource,
        peer_id: String,
        port: u16,
        event_callback: F,
    ) -> Result<Tracker<F>, BitTorrentError> {
        let mut trackers: VecDeque<String> = torrent_source.trackers().into();
        event_callback(Event::Created);
        Ok(Tracker {
            active_connection: Tracker::<F>::_next_tracker(&mut trackers),
            peers: VecDeque::new(),
            trackers,
            torrent_source,
            peer_id,
            port,
            event_callback,
        })
    }

    fn _next_tracker(trackers: &mut VecDeque<String>) -> TrackerConnection {
        loop {
            match trackers.pop_front() {
                Some(url) => match TrackerConnection::new(url.clone()) {
                    Ok(tracker_connection) => return tracker_connection,
                    Err(err) => println!("Error connecting to tracker at {}: {}", url, err),
                },
                None => return TrackerConnection::None,
            }
        }
    }

    fn cycle_trackers(&mut self) {
        self.active_connection = Tracker::<F>::_next_tracker(&mut self.trackers);
        (self.event_callback)(Event::CycledTracker(
            TrackerConnectionTypeEvent::from_tracker_connection(&self.active_connection),
        ));
    }

    fn _query(&mut self) -> Result<Option<(Vec<SocketAddr>, Duration)>, BitTorrentError> {
        let active_connection =
            TrackerConnectionTypeEvent::from_tracker_connection(&self.active_connection);
        let event_callback = self.event_callback.clone();
        let event_callback = move |event| event_callback(Event::Query(active_connection, event));
        (event_callback.clone())(QueryEvent::Start);
        match &mut self.active_connection {
            TrackerConnection::Http(announce) => {
                let event_callback = move |event| event_callback(QueryEvent::Http(event));
                let client = reqwest::blocking::Client::new();

                (event_callback.clone())(HttpQueryEvent::Start);

                let raw_body = client
                    .get(format!(
                        "{}?{}",
                        announce,
                        [
                            (
                                "info_hash",
                                querystring_encode(&self.torrent_source.hash()?)
                            ),
                            ("peer_id", self.peer_id.to_string()),
                            ("port", format!("{}", self.port)),
                            ("uploaded", "0".to_string()),
                            ("downloaded", "0".to_string()),
                            (
                                "left",
                                format!(
                                    "{}",
                                    match &self.torrent_source {
                                        TorrentSource::File(meta_info) => meta_info.length(),
                                        _ => 0,
                                    }
                                )
                            ),
                            ("compact", "1".to_string()),
                        ]
                        .into_iter()
                        .map(|(key, value)| format!("{}={}", key, value))
                        .collect::<Vec<_>>()
                        .join("&")
                    ))
                    .timeout(TRACKER_QUERY_TIMEOUT)
                    .send()
                    .with_context(|| "Error making request to tracker url")?
                    .bytes()
                    .with_context(|| "Error decoding request response")?
                    .to_vec();

                (event_callback.clone())(HttpQueryEvent::Queried);

                match <Result<_, _>>::from(BencodedValue::ingest(&mut &raw_body[..])?)? {
                    TrackerResponse::Success { interval, peers } => {
                        (event_callback.clone())(HttpQueryEvent::SuccessResponse(
                            PeerAnnounceResponse {
                                interval,
                                peers: peers.clone(),
                            },
                        ));
                        Ok(Some((peers, interval)))
                    }
                    TrackerResponse::Failure { failure_reason } => {
                        event_callback(HttpQueryEvent::FailureResponse(failure_reason.to_string()));
                        Err(bterror!("Tracker query failure: {}", failure_reason))
                    }
                }
            }
            TrackerConnection::Udp(udp_connection) => {
                let event_callback = |event| event_callback(QueryEvent::Udp(event));
                (event_callback.clone())(UdpQueryEvent::Start);
                if udp_connection.last_connection.map_or(true, |time| {
                    SystemTime::now().duration_since(time).unwrap().as_secs() > 60
                }) {
                    (event_callback.clone())(UdpQueryEvent::Connect);
                    udp_connection.connect()?;
                }
                (event_callback.clone())(UdpQueryEvent::Announce);
                let (peers, interval) =
                    udp_connection.annouce(&self.torrent_source, &self.peer_id, self.port)?;
                (event_callback.clone())(UdpQueryEvent::AnnounceResponse(PeerAnnounceResponse {
                    interval,
                    peers: peers.clone(),
                }));
                Ok(Some((peers, interval)))
            }
            _ => Ok(None),
        }
    }

    pub fn query(&mut self) -> Option<(Vec<SocketAddr>, Duration)> {
        loop {
            match self._query() {
                Ok(tracker_info) => return tracker_info,
                Err(err) => {
                    println!("Error querying tracker: {}", err);
                    self.cycle_trackers();
                }
            }
        }
    }
}

impl<F: Fn(Event) + Send + Clone> Iterator for Tracker<F> {
    type Item = (SocketAddr, ControlFlow<Duration, ()>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(peer) = self.peers.pop_front() {
                return Some((peer, ControlFlow::Continue(())));
            } else {
                let Some((peers, interval)) = self.query() else {
                    return None;
                };
                if let Some((yield_peer, rest)) = peers.split_first() {
                    self.peers.extend(rest);
                    return Some((*yield_peer, ControlFlow::Break(interval)));
                }
            }
        }
    }
}

#[derive(Debug)]
enum TrackerConnection {
    Http(String),
    Udp(UdpTrackerConnection),
    None,
}

impl TrackerConnection {
    fn new(url: String) -> Result<Self, BitTorrentError> {
        Ok(if url.starts_with("http") {
            Self::Http(url)
        } else if url.starts_with("udp") {
            match UDP_TRACKER_RE
                .captures(&url)
                .and_then(|captures| captures.get(1))
            {
                Some(group) => Self::Udp(UdpTrackerConnection::new(group.into())?),
                None => return Err(bterror!("Invalid tracker url: {}", url)),
            }
        } else {
            return Err(bterror!("Unrecognized tracker protocol: {}", url));
        })
    }
}

#[derive(Debug)]
struct UdpTrackerConnection {
    connection: UdpSocket,
    last_connection: Option<SystemTime>,
    connection_id: Option<u64>,
}

impl UdpTrackerConnection {
    fn new(tracker_url: &str) -> Result<UdpTrackerConnection, BitTorrentError> {
        let connection = UdpSocket::bind("0.0.0.0:0")
            .and_then(|connection| {
                connection.connect(tracker_url)?;
                Ok(connection)
            })
            .with_context(|| "Error connecting to tracker")?;
        connection.set_read_timeout(Some(TRACKER_QUERY_TIMEOUT))?;
        connection.set_write_timeout(Some(TRACKER_QUERY_TIMEOUT))?;
        Ok(UdpTrackerConnection {
            connection: connection,
            last_connection: None,
            connection_id: None,
        })
    }

    fn connect(&mut self) -> Result<(), BitTorrentError> {
        let transaction_id: u32 = rand::random();
        self.last_connection = Some(SystemTime::now());

        // send connection request
        let request_bytes = []
            .into_iter()
            .chain(0x41727101980_u64.to_be_bytes()) // magic number
            .chain(0_u32.to_be_bytes()) // action (0: connect)
            .chain(transaction_id.to_be_bytes())
            .collect::<Vec<_>>();

        self.connection
            .send(&request_bytes)
            .with_context(|| "Error sending connection request")?;

        // recieve connection request
        let response_bytes = read_datagram(&mut self.connection)?;

        let response_transaction_id = u32::from_be_bytes(response_bytes[4..8].try_into().unwrap());
        if response_transaction_id != transaction_id {
            return Err(bterror!("Invalid transaction id in connection response"));
        }

        // store connection id
        self.connection_id = Some(u64::from_be_bytes(
            response_bytes[8..16].try_into().unwrap(),
        ));

        Ok(())
    }

    fn annouce(
        &mut self,
        torrent_source: &TorrentSource,
        peer_id: &str,
        port: u16,
    ) -> Result<(Vec<SocketAddr>, Duration), BitTorrentError> {
        let transaction_id: u32 = rand::random();
        let key: u32 = rand::random();

        let connection_id = self
            .connection_id
            .ok_or(bterror!("No connection id to use"))?;

        // send announce request
        let request_bytes = empty()
            .chain(connection_id.to_be_bytes())
            .chain(1_u32.to_be_bytes()) // action (1: announce)
            .chain(transaction_id.to_be_bytes())
            .chain(torrent_source.hash()?)
            .chain(peer_id.bytes())
            .chain(0_u64.to_be_bytes()) // downloaded
            .chain(0_u64.to_be_bytes()) // left
            .chain(0_u64.to_be_bytes()) // uploaded
            .chain(0_u32.to_be_bytes()) // event (0: none)
            .chain(0_u32.to_be_bytes()) // ip address (0: all)
            .chain(key.to_be_bytes()) // key
            .chain((-1_i32).to_be_bytes()) // num_want (-1: all)
            .chain(port.to_be_bytes()) // port
            .collect::<Vec<_>>();

        self.connection
            .send(&request_bytes)
            .with_context(|| "Error sending announce request")?;

        // recieve response
        let response_bytes = read_datagram(&mut self.connection)?;

        let response_transaction_id = u32::from_be_bytes(response_bytes[4..8].try_into().unwrap());
        if response_transaction_id != transaction_id {
            return Err(bterror!("Invalid transaction id in announce response"));
        }

        let interval = u32::from_be_bytes(response_bytes[8..12].try_into().unwrap()) as u64;
        // let leechers = u32::from_be_bytes(response_bytes[12..16].try_into().unwrap());
        // let seeders = u32::from_be_bytes(response_bytes[16..20].try_into().unwrap());

        let peers = <Vec<SocketAddr>>::decode(&mut &response_bytes[20..])?;

        Ok((peers, Duration::from_secs(interval)))
    }
}
