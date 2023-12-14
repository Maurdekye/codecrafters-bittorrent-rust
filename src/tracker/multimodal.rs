use std::{
    collections::VecDeque,
    iter::empty,
    net::{SocketAddr, UdpSocket},
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
    tracker::{
        dht::{DhtMessage, GetPeersReturnData, Response},
        TrackerResponse,
    },
    util::{querystring_encode, read_datagram},
};

use super::dht::{Dht, Query};

const TRACKER_QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const DHT_QUERY_INTERVAL: u64 = 30;

lazy_static! {
    static ref UDP_TRACKER_RE: Regex = Regex::new(r"udp://([^:]+:\d+)(/announce)?").unwrap();
}

pub struct Tracker {
    pub torrent_source: TorrentSource,
    active_connection: TrackerConnection,
    pub trackers: VecDeque<String>,
    pub peer_id: String,
    pub port: u16,
}

impl Tracker {
    pub fn new(
        torrent_source: TorrentSource,
        peer_id: String,
        port: u16,
    ) -> Result<Tracker, BitTorrentError> {
        let mut trackers: VecDeque<String> = torrent_source.trackers().into();
        Ok(Tracker {
            active_connection: Tracker::_next_tracker(&mut trackers, &torrent_source),
            trackers,
            torrent_source,
            peer_id,
            port,
        })
    }

    fn _next_tracker(
        trackers: &mut VecDeque<String>,
        torrent_source: &TorrentSource,
    ) -> TrackerConnection {
        loop {
            match trackers.pop_front() {
                Some(url) => match TrackerConnection::new(url.clone()) {
                    Ok(tracker_connection) => return tracker_connection,
                    Err(err) => println!("Error connecting to tracker at {}: {}", url, err),
                },
                None => return TrackerConnection::Dht(Dht::new(torrent_source.clone(), true)),
            }
        }
    }

    fn cycle_trackers(&mut self) {
        self.active_connection = Tracker::_next_tracker(&mut self.trackers, &self.torrent_source);
    }

    fn _query(&mut self) -> Result<(Vec<SocketAddr>, u64), BitTorrentError> {
        match &mut self.active_connection {
            TrackerConnection::Http(announce) => {
                let client = reqwest::blocking::Client::new();

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

                match <Result<_, _>>::from(BencodedValue::ingest(&mut &raw_body[..])?)? {
                    TrackerResponse::Success { interval, peers } => Ok((peers, interval as u64)),
                    TrackerResponse::Failure { failure_reason } => {
                        Err(bterror!("Tracker query failure: {}", failure_reason))
                    }
                }
            }
            TrackerConnection::Udp(udp_connection) => {
                if udp_connection.last_connection.map_or(true, |time| {
                    SystemTime::now().duration_since(time).unwrap().as_secs() > 60
                }) {
                    udp_connection.connect()?;
                }
                udp_connection.annouce(&self.torrent_source, &self.peer_id, self.port)
            }
            TrackerConnection::Dht(dht) => {
                println!("Querying DHT");
                while let Some(node) = dht.nodes.pop_front() {
                    dbg!(&node);
                    match dht.exchange_message(
                        &node,
                        DhtMessage::Query(Query::GetPeers {
                            id: self.peer_id.clone().into(),
                            info_hash: self.torrent_source.hash()?,
                        }),
                    ) {
                        Ok(DhtMessage::Response(Response::GetPeers { response, .. })) => {
                            match response {
                                GetPeersReturnData::Nodes(nodes) => dht.nodes.extend(nodes),
                                GetPeersReturnData::Peers(peers) => {
                                    dht.nodes.push_back(node);
                                    return Ok((peers, DHT_QUERY_INTERVAL));
                                }
                            }
                        }
                        Ok(msg) => println!("Unexpected message: {:?}", msg),
                        Err(err) => println!("Error from DHT node: {}", err),
                    }
                }
                Err(bterror!("No peers found in DHT"))
            }
        }
    }

    pub fn query(&mut self) -> (Vec<SocketAddr>, u64) {
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

#[derive(Debug)]
enum TrackerConnection {
    Http(String),
    Udp(UdpTrackerConnection),
    Dht(Dht),
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
    ) -> Result<(Vec<SocketAddr>, u64), BitTorrentError> {
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

        Ok((peers, interval))
    }
}
