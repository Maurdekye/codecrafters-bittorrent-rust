use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{
        atomic::{self, AtomicBool},
        Arc,
    },
    time::{Duration, SystemTime},
};

use anyhow::Context;
use lazy_static::lazy_static;
use std::default::Default;

use crate::{
    bencode::{BencodedValue, Number},
    bterror, bytes,
    bytes::Bytes,
    error::BitTorrentError,
    info::MetaInfo,
    torrent_source::TorrentSource,
    util::{bytes_to_hex, cap_length, sha1_hash, timestr},
    peer,
};

use super::{
    message::{
        ExtensionHandshake, ExtensionMessage, ExtensionMetadata, HandshakeMessage, PeerMessage,
        PeerMessageCodec, PieceMessage, RequestMessage,
    },
    PeerConnection,
};

/// size of individual piece chunks to request from peer (bytes)
const CHUNK_SIZE: u32 = 16384;
/// number of requests that can be in flight at once
const IN_FLIGHT: usize = 4;
/// connection timeout when attempting to connect to a peer's tcp socket
const TCP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);
/// timeout while waiting for a peer message to arrive, or while attempting to write to a peer's buffer
const TCP_READWRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// maximum number of allowed rejections before the peer is disconnected
const MAX_REJECTIONS: usize = 64;
/// supported extension message codes
const EXTENSION_CONFIG: &[(&[u8], u8)] = &[
    // ("ut_pex", 1),
    (b"ut_metadata", 2),
    // ("upload_only", 3),
    // ("ut_holepunch", 4),
    // ("lt_donthave", 7),
    // ("share_mode", 8),
];

lazy_static! {
    pub static ref CODEC_EXTENSION_CONFIG: Vec<(Bytes, Number)> = EXTENSION_CONFIG
        .iter()
        .map(|(k, v)| (Bytes::from(*k), *v as Number))
        .collect::<Vec<_>>();
    pub static ref HANDSHAKE_EXTENSION_CONFIG: HashMap<Bytes, Number> = CODEC_EXTENSION_CONFIG
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
}

#[derive(Debug)]
pub struct TcpPeer<F> where F: Fn(peer::Event) + Send + Clone {
    #[allow(unused)]
    pub address: SocketAddr,
    pub torrent_source: TorrentSource,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
    pub port: u16,
    pub verbose: bool,
    pub timeout: Option<Duration>,
    pub killswitch: Arc<AtomicBool>,
    pub encoder: PeerMessageCodec,
    pub decoder: PeerMessageCodec,
    pub choked: bool,
    event_callback: F
}

impl<F: Fn(peer::Event) + Send + Clone> TcpPeer<F> {
    /// Wait for a peer message to arrive from the peer and return it.
    pub fn await_peer_message(&mut self) -> Result<PeerMessage, BitTorrentError> {
        self.log("<...<");
        let buf = self.read_n_bytes(4)?;
        match buf.get(..4) {
            Some(b"\x13Bit") => {
                let rest_of_handshake = self.read_n_bytes(64)?;
                let handshake = PeerMessage::Handshake(HandshakeMessage::decode(
                    &buf.into_iter().chain(rest_of_handshake).collect::<Vec<_>>(),
                )?);
                self.log(cap_length(format!("<-<-< {handshake:?}"), 106));
                Ok(handshake)
            }
            Some(buf) => {
                let length = u32::from_be_bytes(buf.try_into().unwrap());
                self.log(format!("<.<.< {length}"));
                let buf: Vec<u8> = self.read_n_bytes(length as usize)?;
                // println!("{}", pretty_print_hex(&buf));
                let response = self.decoder.decode(&buf)?;
                self.log(cap_length(format!("<<<<< {response:?}"), 106));
                Ok(response)
            }
            None => unreachable!(),
        }
    }

    /// Send a peer message `message` to the peer.
    pub fn send_peer_message(&mut self, message: PeerMessage) -> Result<(), BitTorrentError> {
        self.log(format!(">...> {:?}", message));
        self.stream
            .write(&self.encoder.encode(message)?)
            .with_context(|| "Error sending peer message")?;
        self.log(">>>>>");
        Ok(())
    }

    /// Send a handshake message to the peer.
    pub fn handshake(&mut self) -> Result<HandshakeMessage, BitTorrentError> {
        self.log("Sending handshake");
        self.stream
            .write(&HandshakeMessage::new(&self.torrent_source, &self.peer_id)?.encode())
            .with_context(|| "Unable to write to peer")?;
        self.log("Waiting for handshake response");
        let buf = self.read_n_bytes(68)?;
        let handshake = HandshakeMessage::decode(&buf)?;
        self.log(format!("Handshake response: {:?}", handshake));
        Ok(handshake)
    }

    /// Attempt to clone the connection
    #[allow(unused)]
    pub fn try_clone(&self) -> Result<Self, BitTorrentError> {
        Ok(TcpPeer {
            address: self.address,
            torrent_source: self.torrent_source.clone(),
            peer_id: self.peer_id.clone(),
            stream: self.stream.try_clone()?,
            bitfield: self.bitfield.clone(),
            port: self.port,
            verbose: self.verbose,
            timeout: self.timeout,
            killswitch: self.killswitch.clone(),
            encoder: self.encoder.clone(),
            decoder: self.decoder.clone(),
            choked: self.choked,
            event_callback: self.event_callback.clone(),
        })
    }

    fn log(&self, message: impl Display) {
        if self.verbose {
            println!("[{}][{}] {}", timestr(), self.address, message);
        }
    }

    pub fn read_n_bytes(&mut self, mut n: usize) -> Result<Vec<u8>, BitTorrentError> {
        let deadline = self.timeout.map(|timeout| SystemTime::now() + timeout);
        let mut bytes = Vec::new();
        while n > 0 {
            if self.killswitch.load(atomic::Ordering::Relaxed) {
                return Err(bterror!("Peer killed"));
            }
            let timeout = deadline.map(|deadline| {
                deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    .max(Duration::from_millis(1))
            });
            self.stream.set_read_timeout(timeout)?;
            let mut buf = vec![0u8; n];
            let num_read = self
                .stream
                .read(&mut buf)
                .with_context(|| "Error reading tcp stream")?;
            if num_read == 0 {
                if deadline.map_or(false, |deadline| SystemTime::now() > deadline) {
                    return Err(bterror!("Tcp read timeout"));
                }
            } else {
                bytes.extend(&buf[..num_read]);
                n -= num_read;
            }
        }
        Ok(bytes)
    }
}

impl<F: Fn(peer::Event) + Send + Clone> PeerConnection<F> for TcpPeer<F> {
    type Error = BitTorrentError;

    /// Create a new peer connection.
    fn new(
        peer: SocketAddr,
        torrent_source: TorrentSource,
        peer_id: String,
        port: u16,
        verbose: bool,
        killswitch: Arc<AtomicBool>,
        event_callback: F
    ) -> Result<TcpPeer<F>, BitTorrentError> {
        let handshake = PeerMessage::Handshake(HandshakeMessage::new(&torrent_source, &peer_id)?);

        let mut connection = TcpPeer {
            address: peer,
            stream: TcpStream::connect_timeout(&peer, TCP_CONNECTION_TIMEOUT)
                .with_context(|| "Error connecting to peer")?,
            torrent_source,
            peer_id,
            bitfield: vec![],
            port,
            verbose,
            timeout: Some(TCP_READWRITE_TIMEOUT),
            killswitch,
            encoder: PeerMessageCodec::new(CODEC_EXTENSION_CONFIG.clone()),
            decoder: PeerMessageCodec::default(),
            choked: true,
            event_callback,
        };

        connection.stream.set_read_timeout(connection.timeout)?;
        connection.stream.set_write_timeout(connection.timeout)?;

        connection.log("TCP Connection established");

        let mut bitfield_source: Option<Box<dyn Iterator<Item = bool>>> = None;
        let meta_info = match &connection.torrent_source {
            TorrentSource::File(meta_info) => Some(meta_info.clone()),
            TorrentSource::Magnet(_) => None,
        };
        let mut meta_info_pieces = Vec::new();
        let mut meta_info_acquired = meta_info.is_some();
        let mut recieved_extension_handshake = false;
        let mut sent_initial_metadata_request = false;

        // send opening handshake
        connection.send_peer_message(handshake)?;

        // listen for peer messages until initialization is complete
        loop {
            match connection.await_peer_message()? {
                PeerMessage::Handshake(handshake) => {
                    // send extension handshake
                    connection.log(format!("{:?}", handshake));
                    connection.send_peer_message(PeerMessage::Extension(
                        ExtensionMessage::Handshake(ExtensionHandshake {
                            messages: Some(HANDSHAKE_EXTENSION_CONFIG.clone()),
                            version: Some(bytes!(b"MaurdekyeBitTorrent/1.0.0")),
                            yourip: Some(peer.ip().into()),
                            reqq: Some(500),
                            ..Default::default()
                        }),
                    ))?;
                }
                PeerMessage::Extension(ExtensionMessage::Handshake(handshake)) => {
                    connection.decoder = PeerMessageCodec::from_handshake(&handshake)?;
                    connection.log(format!("{:#?}", handshake));
                    recieved_extension_handshake = true;
                }
                PeerMessage::Bitfield(bitfield) => {
                    bitfield_source = Some(Box::new(bitfield.into_iter()))
                }
                PeerMessage::HaveAll => bitfield_source = Some(Box::new((0..).map(|_| true))),
                PeerMessage::HaveNone => {
                    // peer has no pieces, send notinterested and close the connection
                    connection.send_peer_message(PeerMessage::NotInterested)?;
                    return Err(bterror!("Peer has no data"));
                }
                PeerMessage::Extension(ExtensionMessage::Metadata(
                    ExtensionMetadata {
                        msg_type: 1,
                        total_size: Some(total_size),
                        ..
                    },
                    Some(data),
                )) => {
                    meta_info_pieces.push(data);
                    if meta_info_pieces.iter().flatten().count() < total_size as usize {
                        // not all pieces acquired, ask for more
                        connection.send_peer_message(PeerMessage::Extension(
                            ExtensionMessage::Metadata(
                                ExtensionMetadata {
                                    msg_type: 0,
                                    piece: meta_info_pieces.len() as Number,
                                    total_size: None,
                                },
                                None,
                            ),
                        ))?;
                    } else {
                        // mark meta_info as acquired
                        meta_info_acquired = true;
                    }
                }
                PeerMessage::Choke => connection.choked = true,
                PeerMessage::Unchoke => connection.choked = false,
                _ => (),
            }

            // request meta_info
            if recieved_extension_handshake
                && meta_info.is_none()
                && !sent_initial_metadata_request
            {
                connection.send_peer_message(PeerMessage::Extension(
                    ExtensionMessage::Metadata(
                        ExtensionMetadata {
                            msg_type: 0,
                            piece: 0,
                            total_size: None,
                        },
                        None,
                    ),
                ))?;
                sent_initial_metadata_request = true;
            }

            // send interested & exit listen loop
            if bitfield_source.is_some() && meta_info_acquired {
                connection.send_peer_message(PeerMessage::Interested)?;
                break;
            }
        }

        // construct meta_info
        let meta_info = match meta_info {
            Some(meta_info) => meta_info,
            None => MetaInfo {
                announce_list: Vec::new(),
                info: <Result<_, _>>::from(BencodedValue::ingest(
                    &mut &meta_info_pieces.into_iter().flatten().collect::<Vec<_>>()[..],
                )?)?,
            },
        };

        if let Some(bitfield_source) = bitfield_source {
            connection.bitfield = bitfield_source.take(meta_info.info.pieces.len()).collect();
            connection.torrent_source = TorrentSource::File(meta_info);
        } else {
            unreachable!()
        }

        Ok(connection)
    }

    /// Download a piece of the file, with `piece_id` corresponding to the piece to download.
    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, BitTorrentError> {
        let meta_info = self
            .meta_info()
            .ok_or(bterror!("Can't download a file without meta info!"))?;
        let piece_offset = (piece_id as usize) * meta_info.info.piece_length;
        let chunk_size =
            (meta_info.length() - piece_offset).min(meta_info.info.piece_length) as u32;

        let mut chunks = (0..chunk_size)
            .step_by(CHUNK_SIZE as usize)
            .collect::<VecDeque<_>>();
        let total_chunks = chunks.len();
        let mut pieces = Vec::new();
        let mut in_flight = 0;
        let mut rejections = 0;

        while pieces.len() < total_chunks {
            // send packets that may be sent
            while in_flight < IN_FLIGHT && !self.choked {
                match chunks.pop_front() {
                    Some(begin) => {
                        let length = (chunk_size - begin).min(CHUNK_SIZE);
                        self.send_peer_message(PeerMessage::Request(RequestMessage {
                            index: piece_id,
                            begin,
                            length,
                        }))?;
                        in_flight += 1;
                    }
                    None => break,
                }
            }

            // respond to incoming data
            match self.await_peer_message()? {
                PeerMessage::Piece(piece) => {
                    pieces.push(piece);
                    in_flight -= 1;
                }
                PeerMessage::RejectRequest(request) => {
                    chunks.push_back(request.begin);
                    in_flight -= 1;
                    rejections += 1;
                    if rejections >= MAX_REJECTIONS {
                        return Err(bterror!("Too many rejections"));
                    }
                }
                PeerMessage::Choke => {
                    self.choked = true;
                }
                PeerMessage::Unchoke => {
                    self.choked = false;
                }
                _ => {}
            }
        }

        // coallate chunks
        pieces.sort_by(|PieceMessage { begin: a, .. }, PieceMessage { begin: b, .. }| a.cmp(b));
        let full_piece: Vec<u8> = pieces
            .into_iter()
            .flat_map(|PieceMessage { block, .. }| block)
            .collect();

        // check hash
        let hash = sha1_hash(&full_piece);
        let meta_info = self
            .meta_info()
            .ok_or(bterror!("Can't download a file without meta info!"))?;
        let check_hash = meta_info.info.pieces[piece_id as usize];
        if hash != check_hash {
            Err(bterror!(
                "Piece hash mismatch: meta info hash: {}, actual hash: {}",
                bytes_to_hex(&check_hash),
                bytes_to_hex(&hash)
            ))
        } else {
            self.send_peer_message(PeerMessage::Have(piece_id))?;
            Ok(full_piece)
        }
    }

    fn sever(&self) -> Result<(), Self::Error> {
        self.stream.shutdown(Shutdown::Both)?;
        Ok(())
    }

    fn address(&self) -> &SocketAddr {
        &self.address
    }

    fn meta_info<'a>(&'a self) -> Option<&'a MetaInfo> {
        match &self.torrent_source {
            TorrentSource::File(meta_info) => Some(meta_info),
            _ => None,
        }
    }

    fn bitfield(&self) -> &Vec<bool> {
        &self.bitfield
    }
}
