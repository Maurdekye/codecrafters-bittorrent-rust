use std::{
    net::{Ipv4Addr, SocketAddrV4},
    str::from_utf8,
};

use serde::Deserialize;

use crate::{
    bterror, decode::Decoder, encode::encode_maybe_b64_string, error::BitTorrentError,
    info::MetaInfo,
};

#[derive(Deserialize)]
#[serde(untagged)]
pub enum TrackerResponse {
    Success(SuccessfulTrackerResponse),
    Failure(FailureTrackerResponse),
}

#[derive(Deserialize)]
pub struct SuccessfulTrackerResponse {
    pub interval: usize,
    pub peers: String,
}

#[derive(Deserialize)]
pub struct FailureTrackerResponse {
    #[serde(rename = "failure reason")]
    pub failure_reason: String,
}

impl SuccessfulTrackerResponse {
    /// Return the peers as a vector of socket addresses.
    pub fn peers(&self) -> Result<Vec<SocketAddrV4>, BitTorrentError> {
        Ok(encode_maybe_b64_string(&self.peers)?
            .to_vec()
            .chunks(6)
            .map(|chunk| {
                SocketAddrV4::new(
                    Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]),
                    u16::from_be_bytes([chunk[4], chunk[5]]),
                )
            })
            .collect())
    }
}

/// Encode a byte slice as a URL-safe percent-escaped string.
pub fn querystring_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || [b'-', b'_', b'.', b'~'].contains(byte) {
                from_utf8(&[*byte]).unwrap().to_string()
            } else {
                format!("%{:02x}", byte)
            }
        })
        .collect()
}

/// Query the tracker for a list of peers for the torrent associated with the `meta_info` object passed.
/// deprecated; use `multimodal_tracker::Tracker` instead 
#[deprecated = "use `multimodal_tracker::Tracker` instead"]
#[allow(unused)]
pub fn query_tracker(
    meta_info: &MetaInfo,
    peer_id: &str,
    port: u16,
) -> Result<SuccessfulTrackerResponse, BitTorrentError> {
    let client = reqwest::blocking::Client::new();

    let raw_body = client
        .get(format!(
            "{}?{}",
            meta_info.preferred_tracker(),
            [
                ("info_hash", querystring_encode(&meta_info.info_hash()?)),
                ("peer_id", peer_id.to_string()),
                ("port", format!("{}", port)),
                ("uploaded", "0".to_string()),
                ("downloaded", "0".to_string()),
                ("left", format!("{}", meta_info.length())),
                ("compact", "1".to_string()),
            ]
            .into_iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect::<Vec<_>>()
            .join("&")
        ))
        .send()
        .map_err(|err| bterror!("Error making request to tracker url: {}", err))?
        .bytes()
        .map_err(|err| bterror!("Error decoding request response: {}", err))?
        .to_vec();

    let tracker_response =
        serde_json::from_value(Decoder::new().consume_bencoded_value(&mut &raw_body[..])?)
            .map_err(|err| bterror!("Error deserializing tracker response: {}", err))?;

    match tracker_response {
        TrackerResponse::Success(tracker_info) => Ok(tracker_info),
        TrackerResponse::Failure(tracker_info) => Err(bterror!(
            "Tracker query failure: {}",
            tracker_info.failure_reason
        )),
    }
}
