use std::fs;

use crate::{
    bterror,
    decode::Decoder,
    encode::{bencode_value, encode_maybe_b64_string},
    error::BitTorrentError,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value};
use sha1::{Digest, Sha1};

#[derive(Deserialize, Debug)]
pub struct MetaInfo {
    pub announce: String,
    pub info: Info,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Info {
    pub length: usize,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pieces: String,
}

impl Info {
    pub fn hash(&self) -> Result<Vec<u8>, BitTorrentError> {
        let as_object =
            to_value(self).map_err(|err| bterror!("Unable to bencode info dict: {}", err))?;
        let bencoded = bencode_value(as_object)?;
        let mut hasher = Sha1::new();
        hasher.update(bencoded);
        Ok(hasher.finalize().to_vec())
    }

    pub fn pieces(&self) -> Result<Vec<Vec<u8>>, BitTorrentError> {
        Ok(encode_maybe_b64_string(&self.pieces)?
            .to_vec()
            .chunks(20)
            .map(|chunk| chunk.to_vec())
            .collect())
    }
}

pub fn read_metainfo(filename: &str) -> Result<MetaInfo, BitTorrentError> {
    let content = fs::read(filename).map_err(|err| bterror!("Error reading file: {}", err))?;
    let decoded_value = Decoder::new().consume_bencoded_value(&mut &content[..])?;
    let meta_info = from_value(decoded_value)
        .map_err(|err| bterror!("Unable to parse meta info dictionary: {}", err))?;
    Ok(meta_info)
}
