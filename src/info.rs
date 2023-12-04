use std::fs;

use crate::{
    bterror,
    decode::Decoder,
    encode::{bencode_value, encode_maybe_b64_string},
    error::BitTorrentError, util::sha1_hash,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value};

#[derive(Deserialize, Debug, Clone)]
pub struct MetaInfo {
    pub announce: String,
    pub info: Info,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Info {
    pub length: usize,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pieces: String,
}

impl Info {
    pub fn hash(&self) -> Result<[u8; 20], BitTorrentError> {
        let as_object =
            to_value(self).map_err(|err| bterror!("Unable to bencode info dict: {}", err))?;
        let bencoded = bencode_value(as_object)?;
        Ok(sha1_hash(&bencoded))
    }

    pub fn pieces(&self) -> Result<Vec<[u8; 20]>, BitTorrentError> {
        Ok(encode_maybe_b64_string(&self.pieces)?
            .to_vec()
            .chunks(20)
            .map(|chunk| chunk.try_into().unwrap())
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
