use std::{
    net::TcpListener,
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    thread,
    time::Duration, io,
};

use anyhow::Context;

use crate::{
    bterror,
    error::BitTorrentError,
    peer::{
        message::{HandshakeMessage, PeerMessage, PieceMessage},
        tcp::TcpPeer,
    },
    util::{timestr, read_n_bytes_timeout_busy},
};

use super::{Corkboard, Piece, PieceState};

/// time in between non-blocking tcp listener requests (seconds)
const INTERVAL: u64 = 1;

/// Seeder thread: allows incoming peer connections and feeds torrent data back to them
pub fn seeder(corkboard: Arc<RwLock<Corkboard>>, alarm: Receiver<()>, verbose: bool) -> Result<(), BitTorrentError> {
    let log = |msg: String| if verbose{println!("[{}][S] {msg}", timestr())};

    log(format!("Seeder init"));

    // let listener = corkboard
    //     .read()
    //     .map(|board| TcpListener::bind(format!("0.0.0.0:{}", board.port)).unwrap())
    //     .unwrap();
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true)?;
    let port = corkboard.read().unwrap().port;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let address = stream.peer_addr().unwrap();
                let connection_board = corkboard.clone();

                log(format!("New connection from {address}"));
                thread::spawn(move || {
                    let log = |msg: String| println!("[{}][{}] {msg}", timestr(), address);
                    let (meta_info, peer_id) = connection_board
                        .read()
                        .map(|board| (board.meta_info.clone(), board.peer_id.clone()))
                        .unwrap();

                    // set up connection
                    let mut connection = TcpPeer {
                        stream,
                        address,
                        meta_info,
                        peer_id,
                        port,
                        verbose,
                        bitfield: Vec::new(),
                        timeout: Some(Duration::from_secs(INTERVAL)),
                    };

                    // recieve handshake
                    let handshake =
                        HandshakeMessage::decode(&read_n_bytes_timeout_busy(&mut connection.stream, 68, connection.timeout)?)?;
                    log(format!(
                        "{address} peer id: {}",
                        std::str::from_utf8(&handshake.peer_id).context("Peer id not bytes")?
                    ));

                    // send response handshake
                    connection.handshake()?;

                    // send bitfield
                    let bitfield = connection_board
                        .read()
                        .map(|board| {
                            board
                                .pieces
                                .iter()
                                .map(|piece| matches!(piece.state, PieceState::Fetched(_)))
                                .collect::<Vec<_>>()
                        })
                        .unwrap();
                    connection.send_peer_message(PeerMessage::Bitfield(bitfield))?;

                    // wait for interested
                    loop {
                        match connection.await_peer_message()? {
                            PeerMessage::Interested => break,
                            PeerMessage::NotInterested => {
                                return Err::<(), BitTorrentError>(bterror!(
                                    "{} was not interested",
                                    address
                                ))
                            }
                            _ => {}
                        }
                    }

                    // send unchoke
                    connection.send_peer_message(PeerMessage::Unchoke)?;

                    // respond to data requests
                    loop {
                        match connection.await_peer_message()? {
                            PeerMessage::Request(request) => {
                                let piece_id = request.index;
                                let chunk_data: Vec<u8> = connection_board
                                    .read()
                                    .map(|board| {
                                        let piece_data = match board.pieces.get(piece_id as usize) {
                                            Some(Piece {
                                                state: PieceState::Fetched(data),
                                                ..
                                            }) => data,
                                            _ => {
                                                return Err(bterror!(
                                                    "Piece {piece_id} is not fetched"
                                                ))
                                            }
                                        };
                                        Ok(piece_data
                                            .get(
                                                (request.begin as usize)
                                                    ..((request.begin + request.length) as usize),
                                            )
                                            .context("Invalid chunk data")?
                                            .into_iter()
                                            .copied()
                                            .collect())
                                    })
                                    .unwrap()?;
                                connection.send_peer_message(PeerMessage::Piece(
                                    PieceMessage {
                                        index: request.index,
                                        begin: request.begin,
                                        block: chunk_data,
                                    },
                                ))?;
                            }
                            _ => {}
                        }
                    }
                });
            }
            Err(err) => match err.kind() {
                io::ErrorKind::WouldBlock => {
                    if matches!(
                        alarm.recv_timeout(Duration::from_secs(INTERVAL)),
                        Err(RecvTimeoutError::Disconnected) | Ok(_)
                    ) {
                        break;
                    }
                }
                _ => log(format!("Tcp Listener error: {}", err)),
            },
        }
    }
    Ok(())
}
