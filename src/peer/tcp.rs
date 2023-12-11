use std::{
    collections::VecDeque,
    fmt::Display,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{atomic::{AtomicBool, self}, Arc},
    time::{Duration, SystemTime},
};

use anyhow::Context;
use serde_json::{Number, Value};
use std::default::Default;

use crate::{
    bencode::decode::decode_maybe_b64_string,
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    info_field,
    util::{bytes_to_hex, cap_length, sha1_hash, timestr},
};

use super::{
    message::{
        ExtensionHandshake, ExtensionMessage, HandshakeMessage, PeerMessage, PieceMessage,
        RequestMessage, EncodeDecode,
    },
    PeerConnection,
};

/// size of individual piece chunks to request from peer (bytes)
const CHUNK_SIZE: u32 = 16384;
/// number of requests that can be in flight at once
const IN_FLIGHT: usize = 4;
/// connection timeout when attempting to connect to a peer's tcp socket
const TCP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);
/// timeout while waiting for a peer message to arrive
const TCP_READWRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// maximum number of allowed rejections before the peer is disconnected
const MAX_REJECTIONS: usize = 64;

#[derive(Debug)]
pub struct TcpPeer {
    #[allow(unused)]
    pub address: SocketAddr,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
    pub port: u16,
    pub verbose: bool,
    pub timeout: Option<Duration>,
    pub killswitch: Arc<AtomicBool>,
}

impl TcpPeer {
    /// Wait for a peer message to arrive from the peer and return it.
    pub fn await_peer_message(&mut self) -> Result<PeerMessage, BitTorrentError> {
        self.log("<...<");
        let buf = self.read_n_bytes(4)?;
        let length = u32::from_be_bytes(buf.try_into().expect("Length buffer was not 4 bytes"));
        let buf: Vec<u8> = self.read_n_bytes(length as usize)?;
        let response = PeerMessage::decode(&buf)?;
        self.log(cap_length(format!("<<<<< {response:?}"), 106));
        Ok(response)
    }

    /// Send a peer message `message` to the peer.
    pub fn send_peer_message(&mut self, message: PeerMessage) -> Result<(), BitTorrentError> {
        self.log(format!(">...> {:?}", message));
        self.stream
            .write(&message.encode()?)
            .with_context(|| "Error sending peer message")?;
        self.log(">>>>>");
        Ok(())
    }

    /// Send a handshake message to the peer.
    pub fn handshake(&mut self) -> Result<HandshakeMessage, BitTorrentError> {
        self.log("Sending handshake");
        self.stream
            .write(&HandshakeMessage::new(&self.meta_info, &self.peer_id)?.encode())
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
            meta_info: self.meta_info.clone(),
            peer_id: self.peer_id.clone(),
            stream: self.stream.try_clone()?,
            bitfield: self.bitfield.clone(),
            port: self.port,
            verbose: self.verbose,
            timeout: self.timeout,
            killswitch: self.killswitch.clone(),
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

impl PeerConnection for TcpPeer {
    type Error = BitTorrentError;

    /// Create a new peer connection.
    fn new(
        peer: SocketAddr,
        meta_info: MetaInfo,
        peer_id: String,
        port: u16,
        verbose: bool,
        killswitch: Arc<AtomicBool>,
    ) -> Result<TcpPeer, BitTorrentError> {
        let mut connection = TcpPeer {
            address: peer,
            stream: TcpStream::connect_timeout(
                &peer,
                TCP_CONNECTION_TIMEOUT,
            )
            .with_context(|| "Error connecting to peer")?,
            meta_info,
            peer_id,
            bitfield: vec![],
            port,
            verbose,
            timeout: Some(TCP_READWRITE_TIMEOUT),
            killswitch,
        };

        connection.stream.set_read_timeout(connection.timeout)?;
        connection.stream.set_write_timeout(connection.timeout)?;

        connection.log("TCP Connection established");

        // send handshake
        connection.handshake()?;

        // send extension handshake
        connection.send_peer_message(PeerMessage::Extension(ExtensionMessage::Handshake(
            ExtensionHandshake {
                messages: Some(
                    [
                        // ("ut_pex", 1),
                        // ("ut_metadata", 2),
                        // ("upload_only", 3),
                        // ("ut_holepunch", 4),
                        // ("lt_donthave", 7),
                        // ("share_mode", 8),
                    ]
                    .into_iter()
                    .map(|(k, v): (&str, usize)| (k.to_string(), Value::Number(Number::from(v))))
                    .collect(),
                ),
                version: Some("MaurdekyeBitTorrent/1.0.0".to_string()),
                yourip: Some(match peer {
                    SocketAddr::V4(ip) => decode_maybe_b64_string(
                        &ip.ip()
                            .to_bits()
                            .to_be_bytes()
                            .into_iter()
                            .chain(ip.port().to_be_bytes())
                            .collect::<Vec<_>>(),
                    ),
                    SocketAddr::V6(ip) => decode_maybe_b64_string(
                        &ip.ip()
                            .to_bits()
                            .to_be_bytes()
                            .into_iter()
                            .chain(ip.port().to_be_bytes())
                            .collect::<Vec<_>>(),
                    ),
                }),
                reqq: Some(Number::from(500)),
                ..Default::default()
            },
        )))?;

        loop {
            match connection.await_peer_message()? {
                PeerMessage::Extension(ExtensionMessage::Handshake(handshake)) => {
                    connection.log(format!("{:#?}", handshake));
                    break;
                }
                _ => (),
            }
        }

        // accept bitfield or have- variant
        let pieces = connection.meta_info.num_pieces();
        connection.bitfield = match connection.await_peer_message()? {
            PeerMessage::Bitfield(bitfield) => bitfield[..pieces].to_vec(),
            PeerMessage::HaveAll => vec![true; pieces],
            PeerMessage::HaveNone => {
                // peer has no pieces, send notinterested and close the connection
                connection.send_peer_message(PeerMessage::NotInterested)?;
                return Err(bterror!("Peer has no data"));
            }
            message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
        };

        // // notify with HaveNone
        // connection.send_peer_message(PeerMessage::HaveNone)?;

        // // send port
        // connection.send_peer_message(PeerMessage::Port(port))?;

        // // send interested
        connection.send_peer_message(PeerMessage::Interested)?;

        // wait for unchoke
        while !matches!(connection.await_peer_message()?, PeerMessage::Unchoke) {}

        Ok(connection)
    }

    /// Download a piece of the file, with `piece_id` corresponding to the piece to download.
    fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, BitTorrentError> {
        let piece_offset = piece_id * *info_field!(&self.meta_info.info, piece_length) as u32;
        let chunk_size = (self.meta_info.length() as u32 - piece_offset)
            .min(*info_field!(&self.meta_info.info, piece_length) as u32);

        let mut chunks = (0..chunk_size)
            .step_by(CHUNK_SIZE as usize)
            .collect::<VecDeque<_>>();
        let total_chunks = chunks.len();
        let mut pieces = Vec::new();
        let mut in_flight = 0;
        let mut rejections = 0;
        let mut choked = false;

        while pieces.len() < total_chunks {
            // send packets that may be sent
            while in_flight < IN_FLIGHT && !choked {
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
                    choked = true;
                }
                PeerMessage::Unchoke => {
                    choked = false;
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
        let check_hash = self.meta_info.pieces()?[piece_id as usize];
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

    fn meta_info(&self) -> &MetaInfo {
        &self.meta_info
    }

    fn bitfield(&self) -> &Vec<bool> {
        &self.bitfield
    }
}
