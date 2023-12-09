use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    sync::{Arc, Condvar, Mutex, mpsc::{Receiver, Sender, channel}},
    thread,
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
    multithread::Semaphore,
    util::{bytes_to_hex, cap_length, read_n_bytes, sha1_hash, timestr},
};

use super::{
    message::{
        ExtensionHandshake, ExtensionMessage, HandshakeMessage, PeerMessage, PieceMessage,
        RequestMessage,
    },
    PeerConnection,
};

/// size of individual piece chunks to request from peer (bytes)
const CHUNK_SIZE: u32 = 16384;
/// number of requests that can be in flight at once
const IN_FLIGHT: usize = 4;
/// connection timeout when attempting to connect to a peer's tcp socket (seconds)
const TCP_TIMEOUT: u64 = 1;

#[derive(Debug)]
pub struct TcpPeer {
    #[allow(unused)]
    pub address: SocketAddr,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
    pub port: u16,
}

impl TcpPeer {
    /// Wait for a peer message to arrive from the peer and return it.
    pub fn await_peer_message(&mut self) -> Result<PeerMessage, BitTorrentError> {
        println!("[{}] <...< {}", timestr(), self.address);
        let buf = read_n_bytes(&mut self.stream, 4)?;
        let length = u32::from_be_bytes(buf.try_into().expect("Length buffer was not 4 bytes"));
        // println!("[{}] <.<.< {} {}b", timestr(), self.address, length);
        let buf: Vec<u8> = read_n_bytes(&mut self.stream, length as usize)?;
        let response = PeerMessage::decode(&buf)?;
        println!(
            "[{}] <<<<< {} {}",
            timestr(),
            self.address,
            cap_length(format!("{response:?}"), 100)
        );
        Ok(response)
    }

    /// Send a peer message `message` to the peer.
    pub fn send_peer_message(&mut self, message: PeerMessage) -> Result<(), BitTorrentError> {
        println!("[{}] >>>>> {} {:?}", timestr(), self.address, message);
        self.stream
            .write(&message.encode()?)
            .with_context(|| "Error sending peer message")?;
        Ok(())
    }

    /// Send a handshake message to the peer.
    pub fn handshake(&mut self) -> Result<HandshakeMessage, BitTorrentError> {
        self.stream
            .write(&HandshakeMessage::new(&self.meta_info, &self.peer_id)?.encode())
            .with_context(|| "Unable to write to peer")?;
        let buf = read_n_bytes(&mut self.stream, 68)?;
        HandshakeMessage::decode(&buf)
    }

    /// Attempt to clone the connection
    pub fn try_clone(&self) -> Result<Self, BitTorrentError> {
        Ok(TcpPeer {
            address: self.address,
            meta_info: self.meta_info.clone(),
            peer_id: self.peer_id.clone(),
            stream: self.stream.try_clone()?,
            bitfield: self.bitfield.clone(),
            port: self.port,
        })
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
    ) -> Result<TcpPeer, BitTorrentError> {
        let mut connection = TcpPeer {
            address: peer,
            stream: TcpStream::connect_timeout(&peer, std::time::Duration::from_secs(TCP_TIMEOUT))
                .with_context(|| "Error connecting to peer")?,
            meta_info,
            peer_id,
            bitfield: vec![],
            port,
        };

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

        // wait for extension handshake response
        // while !matches!(
        //     connection.await_peer_message()?,
        //     PeerMessage::Extension(ExtensionMessage::Handshake(_))
        // ) {}

        loop {
            match connection.await_peer_message()? {
                PeerMessage::Extension(ExtensionMessage::Handshake(handshake)) => {
                    println!("{:#?}", handshake);
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

        let chunks = (0..chunk_size).step_by(CHUNK_SIZE as usize);
        let chunks_len = chunks.len();

        let choked = Arc::new((Mutex::new(true), Condvar::new()));
        let in_flight = Arc::new(Semaphore::new(IN_FLIGHT));
        let (req_send, req_recv): (Sender<u32>, Receiver<u32>) = channel();
        chunks.for_each(|chunk| req_send.send(chunk).unwrap());

        let req_send = Arc::new(Mutex::new(req_send));
        let req_recv = Arc::new(Mutex::new(req_recv));

        // start requestor thread
        {
            let choked = choked.clone();
            let in_flight = in_flight.clone();
            let mut connection = self.try_clone()?;
            let req_recv = req_recv.clone();

            thread::spawn(move || {
                let (unchoked, choked_cvar) = &*choked;

                loop {
                    let message = req_recv.lock().unwrap().recv();
                    match message {
                        Ok(chunk_begin) => {
                            let mut unchoked = unchoked.lock().unwrap();
                            while !*unchoked {
                                unchoked = choked_cvar.wait(unchoked).unwrap();
                            }
                            in_flight.take();
        
                            let message_length = (chunk_size - chunk_begin).min(CHUNK_SIZE);
                            connection.send_peer_message(PeerMessage::Request(RequestMessage {
                                index: piece_id,
                                begin: chunk_begin,
                                length: message_length,
                            }))?;
                        }
                        Err(_) => break
                    }
                }

                Ok::<(), BitTorrentError>(())
            });
        }

        // start reciever thread
        let reciever = {
            let choked = choked.clone();
            let in_flight = in_flight.clone();
            let mut connection = self.try_clone()?;
            let req_send = req_send.clone();

            thread::spawn(move || {
                let (unchoked, choked_cvar) = &*choked;

                let mut pieces = Vec::new();

                loop {
                    match connection.await_peer_message()? {
                        PeerMessage::Piece(piece) => {
                            pieces.push(piece);
                            in_flight.put();
                        }
                        PeerMessage::RejectRequest(request) => {
                            req_send.lock().unwrap().send(request.begin).unwrap();
                            in_flight.put();
                        }
                        PeerMessage::Choke => {
                            let mut unchoked = unchoked.lock().unwrap();
                            *unchoked = false;
                            choked_cvar.notify_one();
                        }
                        PeerMessage::Unchoke => {
                            let mut unchoked = unchoked.lock().unwrap();
                            *unchoked = true;
                            choked_cvar.notify_one();
                        }
                        _ => {}
                    }
                    if pieces.len() == chunks_len {
                        break;
                    }
                }

                Ok::<Vec<PieceMessage>, BitTorrentError>(pieces)
            })
        };

        let mut responses = reciever.join().unwrap()?;

        // coallate chunks
        responses.sort_by(|PieceMessage { begin: a, .. }, PieceMessage { begin: b, .. }| a.cmp(b));
        let full_piece: Vec<u8> = responses
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
