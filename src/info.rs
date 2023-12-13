use std::{
    fs::{self, create_dir_all},
    path::PathBuf,
};

use crate::{
    bencode::{BencodedValue, Number},
    bterror, bytes, dict,
    error::BitTorrentError,
    types::Bytes,
    util::sha1_hash,
};
use anyhow::Context;

#[derive(Debug, Clone)]
pub struct MetaInfo {
    pub announce_list: Vec<String>,
    pub info: Info,
}

impl Into<BencodedValue> for MetaInfo {
    fn into(self) -> BencodedValue {
        let (announce, announce_list): (Option<&String>, Option<&[String]>) = self
            .announce_list
            .split_first()
            .map(|(x, y)| (Some(x), Some(y)))
            .unwrap_or_default();
        dict! {
            b"announce" => announce
                .cloned()
                .map(Bytes::from),
            b"announce-list" => announce_list
                .map(|v| v
                    .into_iter()
                    .map(|s| BencodedValue::List(vec![BencodedValue::Bytes(Bytes::from(s.clone()))])).collect::<Vec<_>>()),
            b"info" => self.info,
        }
    }
}

impl From<BencodedValue> for Result<MetaInfo, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut meta_info) = value {
            Ok(MetaInfo {
                announce_list: meta_info
                    .remove(&bytes!(b"announce"))
                    .and_then(BencodedValue::into_bytes)
                    .into_iter()
                    .chain(
                        meta_info
                            .remove(&bytes!(b"announce-list"))
                            .and_then(BencodedValue::into_list)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(BencodedValue::into_list)
                            .flatten()
                            .filter_map(BencodedValue::into_bytes),
                    )
                    .map(Bytes::into_string)
                    .collect(),
                info: meta_info
                    .remove(&bytes!(b"info"))
                    .ok_or(bterror!("Missing info"))
                    .and_then(<Result<_, _>>::from)?,
            })
        } else {
            Err(bterror!("Invalid meta info"))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Info {
    pub name: String,
    pub piece_length: usize,
    pub pieces: Vec<[u8; 20]>,
    pub file_info: FileInfo,
}

impl Info {}

impl Into<BencodedValue> for Info {
    fn into(self) -> BencodedValue {
        match self.file_info {
            FileInfo::Length(length) => dict! {
                b"name" => Bytes::from(self.name),
                b"pieces" => Bytes(self.pieces.into_iter().flatten().collect()),
                b"piece length" => self.piece_length as Number,
                b"length" => length as Number,
            },
            FileInfo::Files(files) => dict! {
                b"name" => Bytes::from(self.name),
                b"piece length" => self.piece_length as Number,
                b"files" => files.into_iter().map(|file| dict! {
                    b"length" => file.length as Number,
                    b"path" => file.path.into_iter().map(Bytes::from).collect::<Vec<_>>(),
                }).collect::<Vec<_>>(),
            },
        }
    }
}

impl From<BencodedValue> for Result<Info, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut info) = value {
            Ok(Info {
                name: info
                    .remove(&bytes!(b"name"))
                    .and_then(BencodedValue::into_bytes)
                    .map(Bytes::into_string)
                    .ok_or(bterror!("Missing name"))?,
                piece_length: info
                    .remove(&bytes!(b"piece length"))
                    .and_then(BencodedValue::into_int)
                    .ok_or(bterror!("Missing piece length"))?
                    .try_into()?,
                pieces: info
                    .remove(&bytes!(b"pieces"))
                    .and_then(BencodedValue::into_bytes)
                    .map(|value| value.chunks(20).filter_map(|x| x.try_into().ok()).collect())
                    .ok_or(bterror!("Missing pieces"))?,
                file_info: match info
                    .remove(&bytes!(b"length"))
                    .and_then(BencodedValue::into_int)
                {
                    Some(length) => FileInfo::Length(length as usize),
                    None => FileInfo::Files(
                        info.remove(&bytes!(b"files"))
                            .and_then(BencodedValue::into_list)
                            .map(|files| {
                                files
                                    .into_iter()
                                    .filter_map(BencodedValue::into_dict)
                                    .map(|mut file| {
                                        Ok::<_, BitTorrentError>(File {
                                            length: file
                                                .remove(&bytes!(b"length"))
                                                .and_then(BencodedValue::into_int)
                                                .ok_or(bterror!("Missing length"))?
                                                as usize,
                                            path: file
                                                .remove(&bytes!(b"path"))
                                                .and_then(BencodedValue::into_list)
                                                .map(|path| {
                                                    path.into_iter()
                                                        .filter_map(BencodedValue::into_bytes)
                                                        .map(Bytes::into_string)
                                                        .collect()
                                                })
                                                .ok_or(bterror!("Missing path"))?,
                                        })
                                    })
                                    .collect::<Result<_, _>>()
                            })
                            .ok_or(bterror!("Missing length or path field"))??,
                    ),
                },
            })
        } else {
            Err(bterror!("Invalid info"))
        }
    }
}

#[derive(Debug, Clone)]
pub enum FileInfo {
    Length(usize),
    Files(Vec<File>),
}

#[derive(Debug, Clone)]
pub struct File {
    pub length: usize,
    pub path: Vec<String>,
}

impl MetaInfo {
    /// Read the metainfo file into a `MetaInfo` result.
    pub fn from_file(filename: &str) -> Result<Self, BitTorrentError> {
        let content = fs::read(filename).with_context(|| "Error reading file")?;
        let decoded_value = BencodedValue::ingest(&mut &content[..])?;
        let meta_info = <Result<_, _>>::from(decoded_value)?;
        Ok(meta_info)
    }

    /// Compute the SHA1 hash of the info dictionary.
    pub fn info_hash(&self) -> Result<[u8; 20], BitTorrentError> {
        Ok(sha1_hash(&BencodedValue::encode(self.info.clone().into())?))
    }

    /// Compute total torrent length.
    pub fn length(&self) -> usize {
        match &self.info.file_info {
            FileInfo::Length(length) => *length,
            FileInfo::Files(files) => files.iter().map(|f| f.length).sum(),
        }
    }

    /// Save the torrent data to the given path.
    pub fn save_to_path(&self, path: &PathBuf, data: Vec<u8>) -> std::io::Result<()> {
        match &self.info.file_info {
            FileInfo::Length(_) => {
                fs::write(path.join(&self.info.name), data)?;
            }
            FileInfo::Files(files) => {
                let mut data_stream = &data[..];
                let base_path = path.join(&self.info.name);
                for file in files {
                    let mut file_path = base_path.clone();
                    for path_part in &file.path {
                        file_path.push(path_part);
                    }
                    create_dir_all(file_path.parent().unwrap())?;
                    let file_data = &data_stream[..file.length];
                    fs::write(file_path, file_data)?;
                    data_stream = &data_stream[file.length..];
                }
            }
        }
        Ok(())
    }
}
