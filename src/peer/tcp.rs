use std::{
    io::Write,
    net::{SocketAddrV4, TcpStream},
};

use anyhow::Context;

use crate::{
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    info_field,
    util::{bytes_to_hex, read_n_bytes, sha1_hash},
};

use super::{
    message::{HandshakeMessage, PeerMessage, PieceMessage, RequestMessage},
    PeerConnection,
};

const CHUNK_SIZE: u32 = 16384;

#[derive(Debug)]
pub struct TcpPeerConnection {
    #[allow(unused)]
    pub address: SocketAddrV4,
    pub meta_info: MetaInfo,
    pub peer_id: String,
    pub stream: TcpStream,
    pub bitfield: Vec<bool>,
}

impl TcpPeerConnection {
    /// Wait for a peer message to arrive from the peer and return it.
    fn await_peer_message(&mut self) -> Result<PeerMessage, BitTorrentError> {
        // println!("reading {}", self.address);
        let buf = read_n_bytes(&mut self.stream, 4)?;
        let length = u32::from_be_bytes(buf.try_into().expect("Length buffer was not 4 bytes"));
        let buf: Vec<u8> = read_n_bytes(&mut self.stream, length as usize)?;
        let response = PeerMessage::decode(&buf)?;
        // println!("recv {} {:?}", self.address, &response);
        Ok(response)
    }

    /// Send a peer message `message` to the peer.
    fn send_peer_message(&mut self, message: &PeerMessage) -> Result<(), BitTorrentError> {
        // println!("send {} {:?}", self.address, message);
        self.stream
            .write(&message.encode()?)
            .with_context(|| "Error sending peer message")?;
        // println!("sent to {}", self.address);
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
}

impl PeerConnection for TcpPeerConnection {
    type Error = BitTorrentError;

    /// Create a new peer connection.
    fn new(
        peer: SocketAddrV4,
        meta_info: MetaInfo,
        peer_id: String,
    ) -> Result<TcpPeerConnection, BitTorrentError> {
        let mut connection = TcpPeerConnection {
            address: peer,
            stream: TcpStream::connect(peer)
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
        let chunk_offset = piece_id * *info_field!(&self.meta_info.info, piece_length) as u32;
        let chunk_size = (self.meta_info.length() as u32 - chunk_offset)
            .min(*info_field!(&self.meta_info.info, piece_length) as u32);

        // send requests
        let mut responses = (0..chunk_size)
            .step_by(CHUNK_SIZE as usize)
            .collect::<Vec<_>>()
            .chunks(1)
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
                            choked = true;
                        }
                        PeerMessage::Unchoke => {
                            choked = false;
                        }
                        message => {
                            return Err(bterror!("Unexpected message from peer: {:?}", message))
                        }
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

    fn address(&self) -> &SocketAddrV4 {
        &self.address
    }

    fn meta_info(&self) -> &MetaInfo {
        &self.meta_info
    }

    fn bitfield(&self) -> &Vec<bool> {
        &self.bitfield
    }
}
