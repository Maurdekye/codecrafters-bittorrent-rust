#![allow(unused)]
use std::{
    collections::{HashSet, VecDeque},
    fmt::Display,
    fs::File,
    iter::once,
    net::{AddrParseError, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket},
    ops::Deref,
    option,
    str::FromStr,
    sync::{
        Arc, Mutex, RwLock,
    },
    thread::Scope,
    time::Duration,
};

use hex::encode;
use lazy_static::lazy_static;
use regex::Match;
use crossbeam::channel::{unbounded, Receiver};

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

const DHT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, std::hash::Hash)]
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

#[derive(Debug, Clone, Eq, std::hash::Hash)]
pub struct Node {
    id: Option<[u8; 20]>,
    address: NodeAddress,
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        match (self.id, other.id) {
            (Some(id1), Some(id2)) => id1 == id2,
            _ => self.address == other.address,
        }
    }
}

impl From<Bytes> for Vec<Node> {
    fn from(val: Bytes) -> Self {
        val.chunks(26)
            .map(|chunk| Node {
                id: Some(chunk[0..20].try_into().unwrap()),
                address: NodeAddress::Ip(
                    <Result<SocketAddr, _>>::from(Bytes(chunk[20..26].to_vec())).unwrap(),
                ),
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
    peer_id: Bytes,
}

impl Dht {
    pub fn new(torrent_source: TorrentSource, peer_id: Bytes, verbose: bool) -> Self {
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
            peer_id,
        };
        dht.socket.set_read_timeout(Some(DHT_QUERY_TIMEOUT));
        dht
    }

    pub fn exchange_message(
        socket: &mut UdpSocket,
        node: &Node,
        message: DhtMessage,
    ) -> Result<DhtMessage, BitTorrentError> {
        socket.connect(node.address.clone())?;

        // send message
        let transaction_id = Bytes(rand::random::<[u8; 2]>().to_vec());
        let krpc_message = KrpcMessage {
            transaction_id: transaction_id.clone(),
            dht_message: message,
        };
        let bencoded: BencodedValue = krpc_message.into();
        let byte_encoded = bencoded.encode()?;
        socket.send(&byte_encoded[..]);

        // recieve message
        let response_bytes = read_datagram(socket)?;
        let response =
            <Result<KrpcMessage, _>>::from(BencodedValue::ingest(&mut &response_bytes[..])?)?;
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

    pub fn initialize<'b, 'c>(&mut self, workers: usize, scope: &'b Scope<'c, '_>) -> Receiver<SocketAddr> where 'b: 'c {
        let (node_send, node_recv) = unbounded();
        let (addr_send, addr_recv) = unbounded();

        for node in self.nodes.iter() {
            node_send.send(node.clone()).unwrap();
        }

        let seen_nodes = Arc::new(RwLock::new(HashSet::new()));

        for i in 0..workers {
            let node_send = node_send.clone();
            let node_recv = node_recv.clone();
            let addr_send = addr_send.clone();
            let peer_id = self.peer_id.clone();
            let info_hash = self.torrent_source.hash().unwrap().clone();
            let marked_nodes = seen_nodes.clone();

            scope.spawn(move || {
                let mut socket = UdpSocket::bind("0.0.0.0:0").unwrap();
                socket.set_read_timeout(Some(DHT_QUERY_TIMEOUT)).unwrap();
                socket.set_write_timeout(Some(DHT_QUERY_TIMEOUT)).unwrap();

                for node in node_recv {
                    let (peers, nodes, keep) = match Dht::exchange_message(
                        &mut socket,
                        &node,
                        DhtMessage::Query(Query::GetPeers {
                            id: peer_id.clone().into(),
                            info_hash,
                        }),
                    ) {
                        Ok(DhtMessage::Response { nodes, peers, .. }) => {
                            if peers.as_ref().map_or(true, |peers| peers.is_empty()) {
                                (peers, nodes, false)
                            } else {
                                (peers, nodes, true)
                            }
                        }
                        _ => (None, None, false),
                    };

                    {
                        let marked_nodes = marked_nodes.read().unwrap();
                        for node in nodes
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|node| !marked_nodes.contains(node))
                        {
                            node_send.send(node).unwrap();
                        }
                    }
                    if keep {
                        node_send.send(node).unwrap();
                    } else {
                        let mut marked_nodes = marked_nodes.write().unwrap();
                        marked_nodes.insert(node);
                    }

                    for addr in peers.unwrap_or_default() {
                        addr_send.send(addr).unwrap();
                    }
                }
            });
        }
        addr_recv
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
            DhtMessage::Response {
                id,
                token,
                nodes,
                peers,
            } => dict! {
                b"t" => value.transaction_id,
                b"y" => bytes!(b"r"),
                b"r" => dict! {
                    b"id" => id,
                    b"token" => token,
                    b"nodes" => nodes.map(Bytes::from),
                    b"values" => peers.map(|peers| peers.into_iter().map(Bytes::from).flatten().collect::<Bytes>()),
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
                    b"r" => {
                        if let Some(mut response) =
                            message.pull(b"r").and_then(BencodedValue::into_dict)
                        {
                            DhtMessage::Response {
                                id: response
                                    .pull(b"id")
                                    .and_then(BencodedValue::into_bytes)
                                    .ok_or(bterror!("Missing id"))?,
                                token: response.pull(b"token").and_then(BencodedValue::into_bytes),
                                nodes: response
                                    .pull(b"nodes")
                                    .and_then(BencodedValue::into_bytes)
                                    .map(<Vec<Node>>::from),
                                peers: response
                                    .pull(b"values")
                                    .and_then(BencodedValue::into_list)
                                    .map(|list| {
                                        list.into_iter()
                                            .filter_map(BencodedValue::into_bytes)
                                            .map(<Result<SocketAddr, _>>::from)
                                            .collect::<Result<Vec<_>, _>>()
                                    })
                                    .transpose()?,
                            }
                        } else {
                            return Err(bterror!("Invalid KRPC message: missing response data"));
                        }
                    }
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
    Response {
        id: Bytes,
        token: Option<Bytes>,
        nodes: Option<Vec<Node>>,
        peers: Option<Vec<SocketAddr>>,
    },
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
