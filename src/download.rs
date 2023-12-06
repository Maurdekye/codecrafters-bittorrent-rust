use std::net::{SocketAddrV4, TcpStream};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::{io::prelude::*, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::info_field;
use crate::multimodal_tracker::Tracker;
use crate::util::{decode_bitfield_be, encode_bitfield_be};
use crate::{
    bterror,
    error::BitTorrentError,
    handshake::{send_handshake, HandshakeMessage},
    info::MetaInfo,
    util::{bytes_to_hex, read_n_bytes, sha1_hash},
};

const CHUNK_SIZE: u32 = 16384;

#[derive(Debug)]
enum PeerMessage {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(BitFieldMessage),
    Request(RequestMessage),
    Piece(PieceMessage),
    Cancel(CancelMessage),
}

#[derive(Serialize, Deserialize, Debug)]
struct BitFieldMessage {
    bitfield: Vec<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
struct RequestMessage {
    index: u32,
    begin: u32,
    length: u32,
}

#[derive(Serialize, Deserialize, Debug)]
struct CancelMessage {
    index: u32,
    begin: u32,
    length: u32,
}

#[derive(Serialize, Deserialize, Debug)]
struct PieceMessage {
    index: u32,
    begin: u32,
    block: Vec<u8>,
}

impl PeerMessage {
    /// Decode a peer message from a byte array.
    fn decode(bytes: &[u8]) -> Result<Self, BitTorrentError> {
        match bytes.get(0) {
            Some(0u8) => Ok(Self::Choke),
            Some(1u8) => Ok(Self::Unchoke),
            Some(2u8) => Ok(Self::Interested),
            Some(3u8) => Ok(Self::NotInterested),
            Some(4u8) => Ok(Self::Have(u32::from_be_bytes(
                bytes[1..5].try_into().unwrap(),
            ))),
            Some(5u8) => Ok(Self::Bitfield(BitFieldMessage {
                bitfield: bytes[1..]
                    .iter()
                    .copied()
                    .flat_map(decode_bitfield_be)
                    .collect(),
            })),
            Some(6u8) => Ok(Self::Request(RequestMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                length: u32::from_be_bytes(bytes[9..13].try_into().unwrap()),
            })),
            Some(7u8) => Ok(Self::Piece(PieceMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                block: bytes[9..].to_vec(),
            })),
            Some(8u8) => Ok(Self::Cancel(CancelMessage {
                index: u32::from_be_bytes(bytes[1..5].try_into().unwrap()),
                begin: u32::from_be_bytes(bytes[5..9].try_into().unwrap()),
                length: u32::from_be_bytes(bytes[9..13].try_into().unwrap()),
            })),
            Some(byte) => Err(bterror!("Invalid peer message: {byte}")),
            None => Err(bterror!("Invalid peer message: empty")),
        }
    }

    /// Encode a peer message into a byte array.
    fn encode(&self) -> Result<Vec<u8>, BitTorrentError> {
        let base_message: Vec<u8> = match self {
            Self::Choke => vec![0],
            Self::Unchoke => vec![1],
            Self::Interested => vec![2],
            Self::NotInterested => vec![3],
            Self::Have(index) => vec![4].into_iter().chain(index.to_be_bytes()).collect(),
            Self::Bitfield(bitfield) => vec![5]
                .into_iter()
                .chain(
                    bitfield
                        .bitfield
                        .chunks(8)
                        .map(|bits| encode_bitfield_be(bits.try_into().unwrap())),
                )
                .collect(),
            Self::Request(req) => vec![6]
                .into_iter()
                .chain(req.index.to_be_bytes())
                .chain(req.begin.to_be_bytes())
                .chain(req.length.to_be_bytes())
                .collect(),
            Self::Piece(piece) => vec![7]
                .into_iter()
                .chain(piece.index.to_be_bytes())
                .chain(piece.begin.to_be_bytes())
                .chain(piece.block.clone())
                .collect(),
            Self::Cancel(cancel) => vec![8]
                .into_iter()
                .chain(cancel.index.to_be_bytes())
                .chain(cancel.begin.to_be_bytes())
                .chain(cancel.length.to_be_bytes())
                .collect(),
        };
        let length = base_message.len() as u32;
        Ok(length
            .to_ne_bytes()
            .into_iter()
            .chain(base_message)
            .collect())
    }
}

#[derive(Debug)]
pub struct PeerConnection {
    #[allow(unused)]
    pub address: SocketAddrV4,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
}

impl PeerConnection {
    /// Create a new peer connection.
    pub fn new(
        peer: SocketAddrV4,
        meta_info: MetaInfo,
        peer_id: String,
    ) -> Result<PeerConnection, BitTorrentError> {
        Ok(PeerConnection {
            address: peer,
            stream: TcpStream::connect(peer)
                .map_err(|err| bterror!("Error connecting to peer: {}", err))?,
            meta_info: meta_info,
            peer_id: peer_id,
            bitfield: vec![],
        })
    }

    /// Initialize the peer connection with a handshake and initialization messages.
    /// 1. sends a `handshake`
    /// 2. waits for a `bitfield`
    /// 3. sends an `interested`
    /// 4. waits for an `unchoke`
    pub fn initialize(&mut self) -> Result<(), BitTorrentError> {
        // send handshake
        send_handshake(
            &mut self.stream,
            &HandshakeMessage::new(&self.meta_info, &self.peer_id)?,
        )?;

        // accept bitfield
        self.bitfield = match self.await_peer_message()? {
            PeerMessage::Bitfield(bitfield) => {
                bitfield.bitfield[..self.meta_info.num_pieces()].to_vec()
            }
            message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
        };

        // send interested
        self.send_peer_message(&PeerMessage::Interested)?;

        // wait for unchoke
        while !matches!(self.await_peer_message()?, PeerMessage::Unchoke) {}

        Ok(())
    }

    /// Download a piece of the file, with `piece_id` corresponding to the piece to download.
    pub fn download_piece(&mut self, piece_id: u32) -> Result<Vec<u8>, BitTorrentError> {
        let chunk_offset = piece_id * *info_field!(&self.meta_info.info, piece_length) as u32;
        let chunk_size = (self.meta_info.length() as u32 - chunk_offset)
            .min(*info_field!(&self.meta_info.info, piece_length) as u32);

        // send requests
        let mut responses = (0..chunk_size)
            .step_by(CHUNK_SIZE as usize)
            .collect::<Vec<_>>().chunks(8)
            .map(|chunk_offsets| {
                for chunk_offset in chunk_offsets.iter() {
                    let message_length = (chunk_size - chunk_offset).min(CHUNK_SIZE);
                    self.send_peer_message(&PeerMessage::Request(RequestMessage {
                        index: piece_id,
                        begin: *chunk_offset,
                        length: message_length,
                    }))?;
                }

                let mut choked = false;
                let mut pieces = Vec::new();

                loop {
                    match self.await_peer_message()? {
                        PeerMessage::Piece(piece) => {
                            pieces.push(piece);
                        }
                        PeerMessage::Choke => {
                            println!("Choked!!");
                            choked = true;
                        }
                        PeerMessage::Unchoke => {
                            println!("Unchoked~");
                            choked = false;
                        }
                        message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
                    }
                    if !choked && pieces.len() == chunk_offsets.len() {
                        break;
                    }
                }
                Ok(pieces)
            })
            .collect::<Result<Vec<Vec<PieceMessage>>, BitTorrentError>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();

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
            Ok(full_piece)
        }
    }

    /// Wait for a peer message to arrive from the peer and return it.
    fn await_peer_message(&mut self) -> Result<PeerMessage, BitTorrentError> {
        // dbg!("reading");
        let buf = read_n_bytes(&mut self.stream, 4)?;
        let length = u32::from_be_bytes(buf.try_into().expect("Length buffer was not 4 bytes"));
        let buf = read_n_bytes(&mut self.stream, length as usize)?;
        PeerMessage::decode(&buf)
    }

    /// Send a peer message `message` to the peer.
    fn send_peer_message(&mut self, message: &PeerMessage) -> Result<(), BitTorrentError> {
        // dbg!(message);
        self.stream
            .write(&message.encode()?)
            .map_err(|err| bterror!("Error sending peer message: {}", err))?;
        // dbg!("sent");
        Ok(())
    }

    /// Check if the peer has the piece with `piece_id` available to download.
    fn has(&self, piece_id: usize) -> bool {
        *self.bitfield.get(piece_id).unwrap_or(&false)
    }
}

/// Download piece `piece_id` of the file from the torrent associated with the `meta_info` object passed.
/// Returns a byte vector containing the piece data.
pub fn download_piece_from_peer(
    meta_info: &MetaInfo,
    piece_id: u32,
    peer_id: &str,
    port: u16,
) -> Result<Vec<u8>, BitTorrentError> {
    let mut tracker = Tracker::new(&meta_info)?;
    let tracker_response = tracker.query(&peer_id, port)?;
    let peers = tracker_response.peers()?;
    let peer = peers.get(0).ok_or(bterror!("Tracker has no peers"))?;
    let mut connection = PeerConnection::new(*peer, meta_info.clone(), peer_id.to_string())?;
    connection.initialize()?;
    connection.download_piece(piece_id)
}

/// Download the full file from the torrent associated with the `meta_info` object passed.
/// Returns a byte vector containing the file data.
pub fn download_file(
    meta_info: &MetaInfo,
    peer_id: &str,
    port: u16,
) -> Result<Vec<u8>, BitTorrentError> {
    let mut tracker = Tracker::new(&meta_info)?;
    let tracker_response = tracker.query(&peer_id, port)?;
    let peers = tracker_response.peers()?;

    let (worker_send, worker_recieve) = mpsc::channel();
    let worker_send = Arc::new(Mutex::new(worker_send));
    let worker_recieve = Arc::new(Mutex::new(worker_recieve));
    let (master_send, master_recieve) = mpsc::channel();
    let master_send = Arc::new(Mutex::new(master_send));

    let num_pieces = meta_info.num_pieces();

    for piece_id in 0..num_pieces {
        worker_send
            .lock()
            .unwrap()
            .send(piece_id as u32)
            .map_err(|err| bterror!("Threading error submitting work: {}", err))?;
    }

    for peer in peers {
        let worker_recieve = worker_recieve.clone();
        let worker_send = worker_send.clone();
        let master_send = master_send.clone();
        let meta_info = meta_info.clone();
        let peer_id = peer_id.to_string();
        thread::spawn(move || {
            // initialize connection
            let mut connection = PeerConnection::new(peer, meta_info, peer_id)?;
            connection.initialize()?;

            // wait for messages
            loop {
                let result = worker_recieve.lock().unwrap().recv();
                match result {
                    Ok(piece_id) => {
                        if connection.has(piece_id as usize) {
                            // download piece
                            let piece = connection.download_piece(piece_id);
                            match piece {
                                Ok(piece) => master_send
                                    .lock()
                                    .unwrap()
                                    .send((piece_id, piece))
                                    .map_err(|err| bterror!("Error submitting work: {}", err))?,
                                Err(_) => {
                                    worker_send.lock().unwrap().send(piece_id).map_err(|err| {
                                        bterror!("Error remitting to queue: {}", err)
                                    })?
                                }
                            }
                        } else {
                            // send back to queue if not available
                            worker_send
                                .lock()
                                .unwrap()
                                .send(piece_id)
                                .map_err(|err| bterror!("Error remitting to queue: {}", err))?;
                        }
                    }
                    Err(_) => break,
                }
            }
            Ok::<(), BitTorrentError>(())
        });
    }

    let mut pieces = master_recieve
        .into_iter()
        .take(num_pieces)
        .collect::<Vec<_>>();
    pieces.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(pieces.into_iter().flat_map(|(_, piece)| piece).collect())
}
