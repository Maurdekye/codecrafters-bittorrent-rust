#![allow(unused)]
use std::{
    collections::VecDeque,
    fmt::Display,
    fs::File,
    net::{AddrParseError, SocketAddr, SocketAddrV4, UdpSocket},
    ops::Deref,
    str::FromStr,
    time::Duration,
};

use hex::encode;
use lazy_static::lazy_static;
use regex::Match;

use crate::{
    bencode::{BencodedValue, Number},
    bterror, bytes, dict,
    error::BitTorrentError,
    list,
    peer::message::Codec,
    torrent_source::TorrentSource,
    types::{Bytes, PullBytes},
};

lazy_static! {
    pub static ref BOOTSTRAP_DHT_NODES: Vec<String> =
        serde_json::from_reader(File::open("dht_nodes.json").unwrap()).unwrap();
}

#[derive(Debug, Clone)]
pub struct Node {
    id: Option<[u8; 20]>,
    address: SocketAddr,
}

impl Into<Vec<Node>> for Bytes {
    fn into(self) -> Vec<Node> {
        self.chunks(26)
            .map(|chunk| Node {
                id: Some(chunk[0..20].try_into().unwrap()),
                address: Bytes(chunk[20..26].to_vec()).into(),
            })
            .collect()
    }
}

impl Into<Bytes> for Vec<Node> {
    fn into(self) -> Bytes {
        Bytes(
            self.into_iter()
                .map(|node| {
                    node.id
                        .unwrap()
                        .into_iter()
                        .chain(Into::<Bytes>::into(node.address))
                        .collect::<Vec<u8>>()
                })
                .flatten()
                .collect::<Vec<_>>(),
        )
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

impl Into<BencodedValue> for Message {
    fn into(self) -> BencodedValue {
        match self.message {
            MessageType::Query(query) => dict! {
                b"t" => self.transaction_id,
                b"y" => bytes!(b"q"),
                b"q" => match query {
                    Query::Ping { .. } => bytes!(b"ping"),
                    Query::FindNode { .. } => bytes!(b"find_node"),
                    Query::GetPeers { .. } => bytes!(b"get_peers"),
                    Query::AnnouncePeer { .. } => bytes!(b"announce_peer"),
                },
                b"a" => match query {
                    Query::Ping { id } => dict! {
                        b"id" => id
                    },
                    Query::FindNode { id, target } => dict! {
                        b"id" => id,
                        b"target" => target
                    },
                    Query::GetPeers { id, info_hash } => dict! {
                        b"id" => id,
                        b"info_hash" => Bytes(info_hash.to_vec())
                    },
                    Query::AnnouncePeer { id, port, info_hash, token } => match port {
                        Some(port) => dict! {
                            b"id" => id,
                            b"port" => port as Number,
                            b"info_hash" => Bytes(info_hash.to_vec()),
                            b"token" => token
                        },
                        None => dict! {
                            b"id" => id,
                            b"implied_port" => 1,
                            b"info_hash" => Bytes(info_hash.to_vec()),
                            b"token" => token
                        }
                    }
                }
            },
            MessageType::Response(response) => dict! {
                b"t" => self.transaction_id,
                b"y" => bytes!(b"r"),
                b"r" => match response {
                    Response::Ping { id } => dict! {
                        b"id" => id
                    },
                    Response::FindNode { id, nodes } => dict! {
                        b"id" => id,
                        b"nodes" => Into::<Bytes>::into(nodes),
                    },
                    Response::GetPeers { id, token, response } => match response {
                        GetPeersReturnData::Nodes(nodes) => dict! {
                            b"id" => id,
                            b"token" => token,
                            b"nodes" => Into::<Bytes>::into(nodes),
                        },
                        GetPeersReturnData::Peers(peers) => dict! {
                            b"id" => id,
                            b"token" => token,
                            b"values" => Bytes(<Vec<SocketAddr>>::encode(peers).unwrap()),
                        },
                    },
                }
            },
            MessageType::Error(code, error) => dict! {
                b"t" => self.transaction_id,
                b"y" => bytes!(b"e"),
                b"e" => list! { code as Number, Bytes::from(error) }
            },
        }
    }
}

impl From<BencodedValue> for Result<Message, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut message) = value {
            Ok(Message {
                transaction_id: message
                    .pull(b"t")
                    .and_then(BencodedValue::into_bytes)
                    .ok_or(bterror!("Invalid KRPC message: missing transaction id"))?,
                message: match &message
                    .pull(b"y")
                    .and_then(BencodedValue::into_bytes)
                    .ok_or(bterror!("Invalid KRPC message: missing message type"))?[..]
                {
                    b"q" => MessageType::Query({
                        if let Some(mut arguments) =
                            message.pull(b"a").and_then(BencodedValue::into_dict)
                        {
                            let id = arguments
                                .pull(b"id")
                                .and_then(BencodedValue::into_bytes)
                                .ok_or(bterror!("Invalid KRPC message: missing id"));
                            let target = arguments
                                .pull(b"target")
                                .and_then(BencodedValue::into_bytes)
                                .ok_or(bterror!("Invalid KRPC message: missing target"));
                            let info_hash = arguments
                                .pull(b"info_hash")
                                .and_then(BencodedValue::into_bytes)
                                .and_then(|x| x[..].try_into().ok())
                                .ok_or(bterror!("Invalid KRPC message: missing info_hash"));
                            let implied_port = arguments
                                .pull(b"implied_port")
                                .and_then(BencodedValue::into_int)
                                .map_or(false, |x| x == 1);
                            let port = arguments
                                .pull(b"port")
                                .and_then(BencodedValue::into_int)
                                .map(|x| x as u16)
                                .ok_or(bterror!("Invalid KRPC message: missing port"));
                            let token = arguments
                                .pull(b"token")
                                .and_then(BencodedValue::into_bytes)
                                .ok_or(bterror!("Invalid KRPC message: missing token"));
                            match &message
                                .pull(b"q")
                                .and_then(BencodedValue::into_bytes)
                                .ok_or(bterror!("Invalid KRPC message: missing query type"))?[..]
                            {
                                b"ping" => Query::Ping { id: id? },
                                b"find_node" => Query::FindNode {
                                    id: id?,
                                    target: target?,
                                },
                                b"get_peers" => Query::GetPeers {
                                    id: id?,
                                    info_hash: info_hash?,
                                },
                                b"announce_peer" => Query::AnnouncePeer {
                                    id: id?,
                                    port: if implied_port { None } else { Some(port?) },
                                    info_hash: info_hash?,
                                    token: token?,
                                },
                                _ => {
                                    return Err(bterror!(
                                        "Invalid KRPC message: invalid query type"
                                    ))
                                }
                            }
                        } else {
                            return Err(bterror!("Invalid KRPC message: missing query arguments"));
                        }
                    }),
                    b"r" => MessageType::Response({
                        if let Some(mut response) =
                            message.pull(b"r").and_then(BencodedValue::into_dict)
                        {
                            match (
                                response.pull(b"id").and_then(BencodedValue::into_bytes),
                                response
                                    .pull(b"nodes")
                                    .and_then(BencodedValue::into_bytes)
                                    .map(Into::<Vec<Node>>::into),
                                response.pull(b"token").and_then(BencodedValue::into_bytes),
                                response
                                    .pull(b"values")
                                    .and_then(BencodedValue::into_list)
                                    .map(|list| {
                                        list.into_iter()
                                            .filter_map(BencodedValue::into_bytes)
                                            .map(Into::<SocketAddr>::into)
                                            .collect()
                                    }),
                            ) {
                                (Some(id), None, None, None) => Response::Ping { id },
                                (Some(id), Some(nodes), None, None) => {
                                    Response::FindNode { id, nodes }
                                }
                                (Some(id), nodes, Some(token), values) => Response::GetPeers {
                                    id,
                                    token,
                                    response: match (nodes, values) {
                                        (Some(nodes), None) => GetPeersReturnData::Nodes(nodes),
                                        (None, Some(values)) => GetPeersReturnData::Peers(values),
                                        _ => {
                                            return Err(bterror!(
                                            "Invalid KRPC message: nodes and values at same time"
                                        ))
                                        }
                                    },
                                },
                                _ => {
                                    return Err(bterror!(
                                        "Invalid KRPC message: invalid response data"
                                    ))
                                }
                            }
                        } else {
                            return Err(bterror!("Invalid KRPC message: missing response data"));
                        }
                    }),
                    b"e" => {
                        if let Some(mut error) =
                            message.pull(b"e").and_then(BencodedValue::into_list)
                        {
                            let error_message = error
                                .pop()
                                .and_then(BencodedValue::into_bytes)
                                .map(Bytes::into_string)
                                .ok_or(bterror!("Invalid KRPC message: missing error message"))?;
                            let error_code = error
                                .pop()
                                .and_then(BencodedValue::into_int)
                                .ok_or(bterror!("Invalid KRPC message: missing error code"))?
                                as usize;
                            MessageType::Error(error_code, error_message)
                        } else {
                            return Err(bterror!("Invalid KRPC message: missing error data"));
                        }
                    }
                    _ => return Err(bterror!("Invalid KRPC message: invalid message type")),
                },
            })
        } else {
            Err(bterror!("Invalid KRPC message"))
        }
    }
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
        response: GetPeersReturnData,
    },
}

#[derive(Debug, Clone)]
enum GetPeersReturnData {
    Nodes(Vec<Node>),
    Peers(Vec<SocketAddr>),
}
