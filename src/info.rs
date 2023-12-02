use std::fs;

use crate::{bterror, decode::Decoder, error::BitTorrentError};
use serde::Deserialize;
use serde_json::from_value;

#[derive(Deserialize, Debug)]
pub struct MetaInfo {
    pub announce: String,
    pub info: Info,
}

#[derive(Deserialize, Debug)]
pub struct Info {
    pub length: usize,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pub pieces: String,
}

pub fn read_metainfo(filename: &str) -> Result<MetaInfo, BitTorrentError> {
    let content = fs::read(filename).map_err(|err| bterror!("Error reading file: {}", err))?;
    let decoded_value = Decoder::new().consume_bencoded_value(&mut &content[..])?;
    from_value(decoded_value)
        .map_err(|err| bterror!("Unable to parse meta info dictionary: {}", err))
}
