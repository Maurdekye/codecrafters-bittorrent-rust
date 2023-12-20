use std::{
    io,
    net::TcpListener,
    sync::{
        mpsc::{Receiver, RecvTimeoutError},
        Arc, RwLock,
    },
    thread,
    time::Duration,
};

use anyhow::Context;

use crate::{
    bterror,
    error::BitTorrentError,
    info::MetaInfo,
    peer::{
        message::{HandshakeMessage, PeerMessage, PeerMessageCodec, PieceMessage},
        tcp::{TcpPeer, CODEC_EXTENSION_CONFIG},
    },
    torrent_source::TorrentSource,
    util::timestr,
};

use super::{Config, Corkboard, Piece, PieceState};

/// time in between non-blocking tcp listener requests
const INTERVAL: Duration = Duration::from_secs(1);

/// Seeder thread: allows incoming peer connections and feeds torrent data back to them
pub fn seeder(
    corkboard: Arc<RwLock<Corkboard>>,
    alarm: Receiver<()>,
    meta_info: MetaInfo,
    config: Config,
) -> Result<(), BitTorrentError> {
    let log = |msg: String| {
        if config.verbose {
            println!("[{}][S] {msg}", timestr())
        }
    };

    log(format!("Seeder init"));

    // let listener = corkboard
    //     .read()
    //     .map(|board| TcpListener::bind(format!("0.0.0.0:{}", board.port)).unwrap())
    //     .unwrap();
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let address = stream.peer_addr().unwrap();
                let connection_board = corkboard.clone();
                let peer_id = config.peer_id.clone();
                let meta_info = meta_info.clone();

                log(format!("New connection from {address}"));
                thread::spawn(move || {
                    let log = |msg: String| println!("[{}][{}] {msg}", timestr(), address);
                    let killswitch = connection_board
                        .read()
                        .map(|board| board.finishing.clone())
                        .unwrap();

                    // set up connection
                    let mut connection = TcpPeer {
                        stream,
                        address,
                        torrent_source: TorrentSource::File(meta_info),
                        peer_id,
                        port: config.port,
                        verbose: config.verbose,
                        bitfield: Vec::new(),
                        timeout: Some(INTERVAL),
                        killswitch,
                        encoder: PeerMessageCodec::new(CODEC_EXTENSION_CONFIG.clone()),
                        decoder: PeerMessageCodec::default(),
                        choked: true,
                    };

                    // recieve handshake
                    let handshake = HandshakeMessage::decode(&connection.read_n_bytes(68)?)?;
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
                                                state: PieceState::Fetched(location),
                                                ..
                                            }) => location.load()?,
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
                                connection.send_peer_message(PeerMessage::Piece(PieceMessage {
                                    index: request.index,
                                    begin: request.begin,
                                    block: chunk_data,
                                }))?;
                            }
                            _ => {}
                        }
                    }
                });
            }
            Err(err) => match err.kind() {
                io::ErrorKind::WouldBlock => {
                    if matches!(
                        alarm.recv_timeout(INTERVAL),
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
