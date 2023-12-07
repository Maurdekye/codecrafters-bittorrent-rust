use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    sync::{Arc, Condvar, Mutex},
    thread,
};

use anyhow::Context;

use crate::{
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    info_field,
    util::{bytes_to_hex, cap_length, read_n_bytes, sha1_hash, timestr, sleep},
};

use super::{
    message::{HandshakeMessage, PeerMessage, PieceMessage, RequestMessage},
    PeerConnection,
};

const CHUNK_SIZE: u32 = 16384;

#[derive(Debug)]
pub struct TcpPeer {
    #[allow(unused)]
    pub address: SocketAddr,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
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
    pub fn send_peer_message(&mut self, message: &PeerMessage) -> Result<(), BitTorrentError> {
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

    pub fn try_clone(&self) -> Result<Self, BitTorrentError> {
        Ok(TcpPeer {
            address: self.address,
            meta_info: self.meta_info.clone(),
            peer_id: self.peer_id.clone(),
            stream: self.stream.try_clone()?,
            bitfield: self.bitfield.clone(),
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
    ) -> Result<TcpPeer, BitTorrentError> {
        let mut connection = TcpPeer {
            address: peer,
            stream: TcpStream::connect_timeout(&peer, std::time::Duration::from_secs(5))
                .with_context(|| "Error connecting to peer")?,
            meta_info: meta_info,
            peer_id: peer_id,
            bitfield: vec![],
        };

        // send handshake
        connection.handshake()?;

        // accept bitfield
        connection.bitfield = match connection.await_peer_message()? {
            PeerMessage::Bitfield(bitfield) => {
                bitfield.bitfield[..connection.meta_info.num_pieces()].to_vec()
            }
            message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
        };

        // send interested
        connection.send_peer_message(&PeerMessage::Interested)?;

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

        // start requestor thread
        let requester_choked = choked.clone();
        let mut requester_connection = self.try_clone()?;
        thread::spawn(move || {
            let (unchoked, choked_cvar) = &*requester_choked;

            for chunk in chunks {
                let mut unchoked = unchoked.lock().unwrap();
                while !*unchoked {
                    unchoked = choked_cvar.wait(unchoked).unwrap();
                }

                let message_length = (chunk_size - chunk).min(CHUNK_SIZE);
                requester_connection.send_peer_message(&PeerMessage::Request(RequestMessage {
                    index: piece_id,
                    begin: piece_offset + chunk,
                    length: message_length,
                }))?;

                sleep(50);
            }

            Ok::<(), BitTorrentError>(())
        });

        // start reciever thread
        let reciever_choked = choked.clone();
        let mut reciever_connection = self.try_clone()?;
        let reciever = thread::spawn(move || {
            let (unchoked, choked_cvar) = &*reciever_choked;

            let mut pieces = Vec::new();

            loop {
                match reciever_connection.await_peer_message()? {
                    PeerMessage::Piece(piece) => {
                        pieces.push(piece);
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
        });

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
