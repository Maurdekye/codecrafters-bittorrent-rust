use std::{net::UdpSocket, time::SystemTime};

use base64::{engine::general_purpose, Engine};

use lazy_static::lazy_static;
use regex::Regex;

use crate::{
    bterror,
    decode::Decoder,
    error::BitTorrentError,
    info::MetaInfo,
    tracker::{querystring_encode, SuccessfulTrackerResponse, TrackerResponse}, util::read_datagram,
};

lazy_static! {
    static ref UDP_TRACKER_RE: Regex = Regex::new(r"udp://([^:]+:\d+)(/announce)?").unwrap();
}

pub struct Tracker<'a> {
    pub meta_info: &'a MetaInfo,
    connection: TrackerConnection,
}

impl Tracker<'_> {
    pub fn new(meta_info: &MetaInfo) -> Result<Tracker, BitTorrentError> {
        let tracker_url = meta_info.announce.clone();
        Ok(Tracker {
            meta_info: meta_info,
            connection: if tracker_url.starts_with("http") {
                TrackerConnection::Http(tracker_url)
            } else if tracker_url.starts_with("udp") {
                match UDP_TRACKER_RE
                    .captures(&tracker_url)
                    .and_then(|captures| captures.get(1))
                {
                    Some(group) => TrackerConnection::Udp(UdpTrackerConnection::new(group.into())?),
                    None => return Err(bterror!("Invalid tracker url: {}", tracker_url)),
                }
            } else {
                return Err(bterror!("Unrecognized tracker protocol: {}", tracker_url));
            },
        })
    }

    pub fn query(
        &mut self,
        peer_id: &str,
        port: u16,
    ) -> Result<SuccessfulTrackerResponse, BitTorrentError> {
        match &mut self.connection {
            TrackerConnection::Http(announce) => {
                let client = reqwest::blocking::Client::new();

                let raw_body = client
                    .get(format!(
                        "{}?{}",
                        announce,
                        [
                            ("info_hash", querystring_encode(&self.meta_info.info_hash()?)),
                            ("peer_id", peer_id.to_string()),
                            ("port", format!("{}", port)),
                            ("uploaded", "0".to_string()),
                            ("downloaded", "0".to_string()),
                            ("left", format!("{}", self.meta_info.length())),
                            ("compact", "1".to_string()),
                        ]
                        .into_iter()
                        .map(|(key, value)| format!("{}={}", key, value))
                        .collect::<Vec<_>>()
                        .join("&")
                    ))
                    .send()
                    .map_err(|err| bterror!("Error making request to tracker url: {}", err))?
                    .bytes()
                    .map_err(|err| bterror!("Error decoding request response: {}", err))?
                    .to_vec();

                let tracker_response = serde_json::from_value(
                    Decoder::new().consume_bencoded_value(&mut &raw_body[..])?,
                )
                .map_err(|err| bterror!("Error deserializing tracker response: {}", err))?;

                match tracker_response {
                    TrackerResponse::Success(tracker_info) => Ok(tracker_info),
                    TrackerResponse::Failure(tracker_info) => Err(bterror!(
                        "Tracker query failure: {}",
                        tracker_info.failure_reason
                    )),
                }
            }
            TrackerConnection::Udp(udp_connection) => {
                if udp_connection.last_connection.map_or(true, |time| {
                    SystemTime::now().duration_since(time).unwrap().as_secs() > 60
                }) {
                    udp_connection.connect()?;
                }
                udp_connection.annouce(&self.meta_info, peer_id, port)
            }
        }
    }
}

enum TrackerConnection {
    Http(String),
    Udp(UdpTrackerConnection),
}

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
            .map_err(|err| bterror!("Error connecting to tracker: {}", err))?;
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
            .map_err(|err| bterror!("Error sending connection request: {}", err))?;

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
        meta_info: &MetaInfo,
        peer_id: &str,
        port: u16,
    ) -> Result<SuccessfulTrackerResponse, BitTorrentError> {
        let transaction_id: u32 = rand::random();
        let key: u32 = rand::random();

        let connection_id = self.connection_id.ok_or(bterror!("No connection id to use"))?;

        // send announce request
        let request_bytes = []
            .into_iter()
            .chain(connection_id.to_be_bytes())
            .chain(1_u32.to_be_bytes()) // action (1: announce)
            .chain(transaction_id.to_be_bytes())
            .chain(meta_info.info_hash()?)
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
            .map_err(|err| bterror!("Error sending announce request: {}", err))?;

        // recieve response
        let response_bytes = read_datagram(&mut self.connection)?;

        let response_transaction_id = u32::from_be_bytes(response_bytes[4..8].try_into().unwrap());
        if response_transaction_id != transaction_id {
            return Err(bterror!("Invalid transaction id in announce response"));
        }

        let interval = u32::from_be_bytes(response_bytes[8..12].try_into().unwrap()) as usize;
        // let leechers = u32::from_be_bytes(response_bytes[12..16].try_into().unwrap());
        // let seeders = u32::from_be_bytes(response_bytes[16..20].try_into().unwrap());

        let peers = format!(
            "base64:{}",
            general_purpose::STANDARD_NO_PAD
                .encode(&response_bytes[20..])
        );

        Ok(SuccessfulTrackerResponse { interval, peers })
    }
}
