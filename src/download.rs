use std::io::prelude::*;
use std::net::TcpStream;

use serde::{Deserialize, Serialize};

use crate::{
    bterror,
    error::BitTorrentError,
    handshake::{send_handshake, HandshakeMessage},
    info::MetaInfo,
    tracker::query_tracker,
    util::read_n_bytes,
};

const CHUNK_SIZE: u32 = 16384;

#[derive(Debug)]
enum PeerMessage {
    Bitfield,
    Interested,
    Unchoke,
    Request(RequestMessage),
    Piece(PieceMessage),
}

#[derive(Serialize, Deserialize, Debug)]
struct RequestMessage {
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
    fn decode(bytes: &[u8]) -> Result<Self, BitTorrentError> {
        match bytes.get(0) {
            Some(5u8) => Ok(Self::Bitfield),
            Some(2u8) => Ok(Self::Interested),
            Some(1u8) => Ok(Self::Unchoke),
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
            _ => Err(bterror!("Invalid peer message")),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, BitTorrentError> {
        let base_message: Vec<u8> = match self {
            Self::Bitfield => vec![5],
            Self::Interested => vec![2],
            Self::Unchoke => vec![1],
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
        };
        let length = base_message.len() as u32;
        Ok(length
            .to_ne_bytes()
            .into_iter()
            .chain(base_message)
            .collect())
    }
}

fn await_peer_message(stream: &mut TcpStream) -> Result<PeerMessage, BitTorrentError> {
    let buf = read_n_bytes(stream, 4)?;
    let length = u32::from_be_bytes(buf.try_into().expect("Length buffer was not 4 bytes"));
    let buf = read_n_bytes(stream, length as usize)?;
    PeerMessage::decode(&buf)
}

fn send_peer_message(stream: &mut TcpStream, message: &PeerMessage) -> Result<(), BitTorrentError> {
    stream
        .write(&message.encode()?)
        .map_err(|err| bterror!("Error sending peer message: {}", err))?;
    Ok(())
}

pub fn download_piece(
    meta_info: &MetaInfo,
    piece_id: u32,
    peer_id: &str,
    port: u16,
) -> Result<Vec<u8>, BitTorrentError> {
    // find peer
    let tracker_response = query_tracker(&meta_info, peer_id, port)?;
    let peers = tracker_response.peers()?;
    let peer = peers.get(0).ok_or(bterror!("Tracker has no peers"))?;
    let mut stream =
        TcpStream::connect(peer).map_err(|err| bterror!("Error connecting to peer: {}", err))?;

    // send handshake
    send_handshake(&mut stream, &HandshakeMessage::new(&meta_info, &peer_id)?)?;

    // wait for bitfield
    match await_peer_message(&mut stream)? {
        PeerMessage::Bitfield => (),
        message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
    };

    // send interested
    send_peer_message(&mut stream, &PeerMessage::Interested)?;

    // wait for unchoke
    match await_peer_message(&mut stream)? {
        PeerMessage::Unchoke => (),
        message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
    }

    // send requests
    let piece_offset = piece_id * meta_info.info.piece_length as u32;
    let piece_size = (meta_info.info.length as u32 - piece_offset).min(meta_info.info.piece_length as u32);
    let mut responses = (0..piece_size)
        .step_by(CHUNK_SIZE as usize)
        .map(|chunk_offset| {
            let message_length =
                (piece_size - chunk_offset).min(CHUNK_SIZE);
            send_peer_message(
                &mut stream,
                &PeerMessage::Request(RequestMessage {
                    index: piece_id,
                    begin: chunk_offset,
                    length: message_length,
                }),
            )?;
            let piece_response = match await_peer_message(&mut stream)? {
                PeerMessage::Piece(piece) => {
                    if piece.index != piece_id {
                        return Err(bterror!("Received piece with wrong index"));
                    }
                    Ok(piece)
                }
                message => return Err(bterror!("Unexpected message from peer: {:?}", message)),
            }?;
            Ok(piece_response)
        })
        .collect::<Result<Vec<PieceMessage>, BitTorrentError>>()?;

    // coallate data
    responses.sort_by(|PieceMessage { begin: a, .. }, PieceMessage { begin: b, .. }| a.cmp(b));
    Ok(responses
        .into_iter()
        .flat_map(|PieceMessage { block, .. }| block)
        .collect())
}
