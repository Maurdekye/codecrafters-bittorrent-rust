use std::{fs, path::PathBuf};

use crate::{
    bterror,
    decode::Decoder,
    encode::{bencode_value, encode_maybe_b64_string},
    error::BitTorrentError,
    util::sha1_hash,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_value, to_value};

#[derive(Deserialize, Debug, Clone)]
pub struct MetaInfo {
    pub announce: String,
    #[serde(rename = "announce-list")]
    pub announce_list: Option<Vec<Vec<String>>>,
    pub info: Info,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum Info {
    SingleFile(SingleFile),
    MultiFile(MultiFile),
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SingleFile {
    pub length: usize,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pieces: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MultiFile {
    pub files: Vec<File>,
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pieces: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct File {
    pub length: usize,
    pub path: Vec<String>,
}

#[macro_export]
macro_rules! info_field {
    ($path:expr, $field:ident) => {
        match $path {
            crate::info::Info::SingleFile(single_file) => &single_file.$field,
            crate::info::Info::MultiFile(multi_file) => &multi_file.$field,
        }
    };
}

impl MetaInfo {
    /// Read the metainfo file into a `MetaInfo` result.
    pub fn from_file(filename: &str) -> Result<Self, BitTorrentError> {
        let content = fs::read(filename).map_err(|err| bterror!("Error reading file: {}", err))?;
        let decoded_value = Decoder::new().consume_bencoded_value(&mut &content[..])?;
        // dbg!(&decoded_value);
        let meta_info = from_value(decoded_value)
            .map_err(|err| bterror!("Unable to parse meta info dictionary: {}", err))?;
        Ok(meta_info)
    }

    /// Compute the SHA1 hash of the info dictionary.
    pub fn info_hash(&self) -> Result<[u8; 20], BitTorrentError> {
        let as_object = match &self.info {
            Info::SingleFile(single_file) => to_value(single_file),
            Info::MultiFile(multi_file) => to_value(multi_file),
        }
        .map_err(|err| bterror!("Unable to bencode info dict: {}", err))?;
        let bencoded = bencode_value(as_object)?;
        Ok(sha1_hash(&bencoded))
    }

    /// Get the SHA1 hashes of the pieces.
    pub fn pieces(&self) -> Result<Vec<[u8; 20]>, BitTorrentError> {
        Ok(
            encode_maybe_b64_string(info_field!(&self.info, pieces).as_str())?
                .to_vec()
                .chunks(20)
                .map(|chunk| chunk.try_into().unwrap())
                .collect(),
        )
    }

    /// Get the number of pieces.
    pub fn num_pieces(&self) -> usize {
        // self.length.div_ceil(self.piece_length)
        let piece_length = info_field!(&self.info, piece_length);
        (self.length() + piece_length - 1) / piece_length
    }

    /// Compute total torrent length.
    pub fn length(&self) -> usize {
        match self.info {
            Info::SingleFile(ref single_file) => single_file.length,
            Info::MultiFile(ref multi_file) => multi_file.files.iter().map(|f| f.length).sum(),
        }
    }

    /// Get a preferred tracker url
    pub fn preferred_tracker(&self) -> String {
        self.announce_list
            .as_ref()
            .and_then(|list| list.iter().flatten().find(|url| url.starts_with("http")))
            .unwrap_or(&self.announce)
            .clone()
    }

    /// Save the torrent data to the given path.
    pub fn save_to_path(&self, path: &PathBuf, data: Vec<u8>) -> std::io::Result<()> {
        match &self.info {
            Info::SingleFile(single_file) => {
                fs::write(path.join(&single_file.name), data)?;
            }
            Info::MultiFile(multi_file) => {
                let mut data_stream = &data[..];
                let base_path = path.join(&multi_file.name);
                for file in &multi_file.files {
                    let mut file_path = base_path.clone();
                    for path_part in &file.path {
                        file_path.push(path_part);
                    }
                    let file_data = &data_stream[..file.length];
                    fs::write(file_path, file_data)?;
                    data_stream = &data_stream[file.length..];
                }
            }
        }
        Ok(())
    }
}
