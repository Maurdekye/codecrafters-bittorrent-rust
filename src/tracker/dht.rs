#![allow(unused)]
use std::{
    collections::VecDeque,
    fs::File,
    net::{AddrParseError, SocketAddr, UdpSocket},
    str::FromStr,
    time::Duration,
};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{bencode::BencodedValue, torrent_source::TorrentSource, types::Bytes};

lazy_static! {
    pub static ref BOOTSTRAP_DHT_NODES: Vec<String> =
        serde_json::from_reader(File::open("dht_nodes.json").unwrap()).unwrap();
}

#[derive(Debug, Deserialize, Clone)]
pub struct Node {
    id: Option<String>,
    address: SocketAddr,
}

pub struct NodeIter(VecDeque<Node>);

impl From<Bytes> for NodeIter {
    fn from(value: Bytes) -> Self {
        todo!()
    }
}

impl Iterator for NodeIter {
    type Item = Node;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.pop_front()
    }
}

#[derive(Debug)]
pub struct Dht {
    pub torrent_source: TorrentSource,
    pub nodes: VecDeque<Node>,
    socket: UdpSocket,
    peer_id: String,
}

impl Dht {
    pub fn new(torrent_source: TorrentSource, peer_id: String) -> Self {
        let dht = Self {
            torrent_source,
            nodes: BOOTSTRAP_DHT_NODES
                .clone()
                .into_iter()
                .map(|ip| Node {
                    address: ip.parse().unwrap(),
                    id: None,
                })
                .collect(),
            socket: UdpSocket::bind("0.0.0.0:0").unwrap(),
            peer_id,
        };
        dht.socket.set_read_timeout(Some(Duration::from_secs(20)));
        dht
    }
}

impl Iterator for Dht {
    type Item = Vec<SocketAddr>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let node = self.nodes.pop_front()?;

            dbg!(&node);
            self.socket.send_to(b"", node.address).unwrap();

            let mut buf = [0; 65536];
            match self.socket.recv_from(&mut buf) {
                Ok(data) => {
                    let decoded = BencodedValue::ingest(&mut &buf[..]).unwrap();
                    dbg!(decoded);
                    break;
                }
                _ => (),
            };
        }
        None
    }
}

#[derive(Debug, Clone)]
struct Message {
    transaction_id: Bytes,
    message: MessageType,
}

#[derive(Debug, Clone)]
enum MessageType {
    Query(Query),
    Response(Response),
    Error(usize, String),
}

#[derive(Debug, Clone)]
enum Query {
    Ping {
        id: Bytes,
    },
    FindNode {
        id: Bytes,
        target: Bytes,
    },
    GetPeers {
        id: Bytes,
        info_hash: [u8; 20],
    },
    AnnouncePeer {
        id: Bytes,
        port: Option<u16>,
        info_hash: [u8; 20],
        token: Bytes,
    },
}

#[derive(Debug, Clone)]
enum Response {
    Ping {
        id: Bytes,
    },
    FindNode {
        id: Bytes,
        nodes: Vec<Node>,
    },
    GetPeers {
        id: Bytes,
        token: Bytes,
        response: GetPeersResponse,
    },
}

#[derive(Debug, Clone)]
enum GetPeersResponse {
    Nodes(Vec<Node>),
    Peers(Vec<SocketAddr>),
}
