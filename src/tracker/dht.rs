#![allow(unused)]
use std::{
    collections::VecDeque,
    fmt::Display,
    fs::File,
    net::{AddrParseError, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket},
    ops::Deref,
    option,
    str::FromStr,
    time::Duration,
};

use hex::encode;
use lazy_static::lazy_static;
use regex::Match;

use crate::{
    bencode::{BencodedValue, Number},
    bterror, bytes,
    bytes::{Bytes, PullBytes},
    dict,
    error::BitTorrentError,
    list,
    peer::message::Codec,
    torrent_source::TorrentSource,
    util::{read_datagram, timestr},
};

lazy_static! {
    pub static ref BOOTSTRAP_DHT_NODES: Vec<String> =
        serde_json::from_reader(File::open("dht_nodes.json").unwrap()).unwrap();
}

#[derive(Debug, Clone)]
pub enum NodeAddress {
    Ip(SocketAddr),
    Domain(String),
}

impl From<NodeAddress> for Bytes {
    fn from(val: NodeAddress) -> Self {
        match val {
            NodeAddress::Ip(ip) => Bytes::from(ip),
            NodeAddress::Domain(domain) => Bytes::from(domain),
        }
    }
}

impl ToSocketAddrs for NodeAddress {
    type Iter = option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> std::io::Result<Self::Iter> {
        match self {
            NodeAddress::Ip(ip) => Ok(Some(ip.clone()).into_iter()),
            NodeAddress::Domain(domain) => {
                domain.to_socket_addrs().map(|mut x| x.next().into_iter())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    id: Option<[u8; 20]>,
    address: NodeAddress,
}

impl From<Bytes> for Vec<Node> {
    fn from(val: Bytes) -> Self {
        val.chunks(26)
            .map(|chunk| Node {
                id: Some(chunk[0..20].try_into().unwrap()),
                address: NodeAddress::Ip(SocketAddr::from(Bytes(chunk[20..26].to_vec()))),
            })
            .collect()
    }
}

impl From<Vec<Node>> for Bytes {
    fn from(val: Vec<Node>) -> Self {
        Bytes(
            val.into_iter()
                .map(|node| {
                    node.id
                        .unwrap()
                        .into_iter()
                        .chain(Bytes::from(node.address))
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
    verbose: bool,
}

impl Dht {
    pub fn new(torrent_source: TorrentSource, verbose: bool) -> Self {
        let dht = Self {
            torrent_source,
            nodes: BOOTSTRAP_DHT_NODES
                .clone()
                .into_iter()
                .map(|ip| Node {
                    address: NodeAddress::Domain(ip.parse().unwrap()),
                    id: None,
                })
                .collect(),
            socket: UdpSocket::bind("0.0.0.0:0").unwrap(),
            verbose,
        };
        dht.socket.set_read_timeout(Some(Duration::from_secs(60)));
        dht
    }

    pub fn exchange_message(
        &mut self,
        node: &Node,
        message: DhtMessage,
    ) -> Result<DhtMessage, BitTorrentError> {
        self.socket.connect(node.address.clone());

        // send message
        let transaction_id = Bytes(rand::random::<[u8; 2]>().to_vec());
        let krpc_message = KrpcMessage {
            transaction_id: transaction_id.clone(),
            dht_message: message,
        };
        self.log(format!(" ^.^ {:#?}", &krpc_message));
        let bencoded: BencodedValue = krpc_message.into();
        self.log(&bencoded);
        let byte_encoded = bencoded.encode()?;
        self.socket.send(&byte_encoded[..]);
        self.log(" ^^^");

        // recieve message
        let response_bytes = read_datagram(&mut self.socket)?;
        let response =
            <Result<KrpcMessage, _>>::from(BencodedValue::ingest(&mut &response_bytes[..])?)?;
        self.log(format!(" vvv {:#?}", response));
        if response.transaction_id != transaction_id {
            Err(bterror!(
                "Response transaction id does not match: recieved {}, expected {}",
                response.transaction_id,
                transaction_id
            ))
        } else if let DhtMessage::Error(code, error) = response.dht_message {
            Err(bterror!(
                "Error response from DHT node: code {}: {}",
                code,
                error
            ))
        } else {
            Ok(response.dht_message)
        }
    }

    fn log(&self, message: impl Display) {
        if self.verbose {
            println!("[{}] {}", timestr(), message);
        }
    }
}

#[derive(Debug, Clone)]
pub struct KrpcMessage {
    transaction_id: Bytes,
    dht_message: DhtMessage,
}

impl From<KrpcMessage> for BencodedValue {
    fn from(value: KrpcMessage) -> Self {
        match value.dht_message {
            DhtMessage::Query(query) => dict! {
                b"t" => value.transaction_id,
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
            DhtMessage::Response(response) => dict! {
                b"t" => value.transaction_id,
                b"y" => bytes!(b"r"),
                b"r" => match response {
                    Response::Ping { id } => dict! {
                        b"id" => id
                    },
                    Response::FindNode { id, nodes } => dict! {
                        b"id" => id,
                        b"nodes" => Bytes::from(nodes),
                    },
                    Response::GetPeers { id, token, response } => match response {
                        GetPeersReturnData::Nodes(nodes) => dict! {
                            b"id" => id,
                            b"token" => token,
                            b"nodes" => Bytes::from(nodes),
                        },
                        GetPeersReturnData::Peers(peers) => dict! {
                            b"id" => id,
                            b"token" => token,
                            b"values" => Bytes(<Vec<SocketAddr>>::encode(peers).unwrap()),
                        },
                    },
                }
            },
            DhtMessage::Error(code, error) => dict! {
                b"t" => value.transaction_id,
                b"y" => bytes!(b"e"),
                b"e" => list! { code as Number, Bytes::from(error) }
            },
        }
    }
}

impl From<BencodedValue> for Result<KrpcMessage, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut message) = value {
            Ok(KrpcMessage {
                transaction_id: message
                    .pull(b"t")
                    .and_then(BencodedValue::into_bytes)
                    .ok_or(bterror!("Invalid KRPC message: missing transaction id"))?,
                dht_message: match &message
                    .pull(b"y")
                    .and_then(BencodedValue::into_bytes)
                    .ok_or(bterror!("Invalid KRPC message: missing message type"))?[..]
                {
                    b"q" => DhtMessage::Query({
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
                    b"r" => DhtMessage::Response({
                        if let Some(mut response) =
                            message.pull(b"r").and_then(BencodedValue::into_dict)
                        {
                            match (
                                response.pull(b"id").and_then(BencodedValue::into_bytes),
                                response
                                    .pull(b"nodes")
                                    .and_then(BencodedValue::into_bytes)
                                    .map(<Vec<Node>>::from),
                                response.pull(b"token").and_then(BencodedValue::into_bytes),
                                response
                                    .pull(b"values")
                                    .and_then(BencodedValue::into_list)
                                    .map(|list| {
                                        list.into_iter()
                                            .filter_map(BencodedValue::into_bytes)
                                            .map(SocketAddr::from)
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
                            DhtMessage::Error(error_code, error_message)
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
pub enum DhtMessage {
    Query(Query),
    Response(Response),
    Error(usize, String),
}

#[derive(Debug, Clone)]
pub enum Query {
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
pub enum Response {
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
pub enum GetPeersReturnData {
    Nodes(Vec<Node>),
    Peers(Vec<SocketAddr>),
}
