use std::sync::Arc;
use std::sync::{mpsc, Mutex};
use std::thread;

use anyhow::Context;

use crate::peer::PeerConnection;
use crate::tracker::multimodal::Tracker;
use crate::{bterror, error::BitTorrentError, info::MetaInfo};

pub mod corkboard;

/// Download piece `piece_id` of the file from the torrent associated with the `meta_info` object passed.
/// Returns a byte vector containing the piece data.
pub fn download_piece_from_peer<T: PeerConnection<Error = BitTorrentError>>(
    meta_info: &MetaInfo,
    piece_id: u32,
    peer_id: &str,
    port: u16,
) -> Result<Vec<u8>, BitTorrentError> {
    let mut tracker = Tracker::new(&meta_info)?;
    let tracker_response = tracker.query(&peer_id, port)?;
    let peers = tracker_response.peers()?;
    let peer = peers.get(0).ok_or(bterror!("Tracker has no peers"))?;
    let mut connection = T::new(*peer, meta_info.clone(), peer_id.to_string(), port, false)?;
    connection.download_piece(piece_id)
}

/// Download the full file from the torrent associated with the `meta_info` object passed.
/// Returns a byte vector containing the file data.
pub fn download_file<T: PeerConnection<Error = BitTorrentError>>(
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
            .with_context(|| "Threading error submitting work")?;
    }

    for peer in peers {
        let worker_recieve = worker_recieve.clone();
        let worker_send = worker_send.clone();
        let master_send = master_send.clone();
        let meta_info = meta_info.clone();
        let peer_id = peer_id.to_string();
        thread::spawn(move || {
            // initialize connection
            let mut connection = T::new(peer, meta_info, peer_id, port, false)?;

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
                                    .with_context(|| "Error submitting work")?,
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
                                .with_context(|| "Error remitting to queue")?;
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
