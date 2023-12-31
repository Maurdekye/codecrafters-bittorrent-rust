use std::{
    fs::{self, create_dir_all},
    io::Read,
    path::PathBuf,
};

use crate::{
    bencode::{BencodedValue, Number},
    bterror,
    bytes::{Bytes, PullBytes},
    dict,
    error::BitTorrentError,
    util::sha1_hash,
};
use anyhow::Context;

#[derive(Debug, Clone)]
pub struct MetaInfo {
    pub announce_list: Vec<String>,
    pub info: Info,
}

impl From<MetaInfo> for BencodedValue {
    fn from(val: MetaInfo) -> Self {
        let (announce, announce_list): (Option<&String>, Option<&[String]>) = val
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
            b"info" => val.info,
        }
    }
}

impl From<BencodedValue> for Result<MetaInfo, BitTorrentError> {
    fn from(value: BencodedValue) -> Self {
        if let BencodedValue::Dict(mut meta_info) = value {
            Ok(MetaInfo {
                announce_list: meta_info
                    .pull(b"announce")
                    .and_then(BencodedValue::into_bytes)
                    .into_iter()
                    .chain(
                        meta_info
                            .pull(b"announce-list")
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
                    .pull(b"info")
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

impl From<Info> for BencodedValue {
    fn from(val: Info) -> Self {
        match val.file_info {
            FileInfo::Length(length) => dict! {
                b"name" => Bytes::from(val.name),
                b"pieces" => Bytes(val.pieces.into_iter().flatten().collect()),
                b"piece length" => val.piece_length as Number,
                b"length" => length as Number,
            },
            FileInfo::Files(files) => dict! {
                b"name" => Bytes::from(val.name),
                b"pieces" => Bytes(val.pieces.into_iter().flatten().collect()),
                b"piece length" => val.piece_length as Number,
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
                    .pull(b"name")
                    .and_then(BencodedValue::into_bytes)
                    .map(Bytes::into_string)
                    .ok_or(bterror!("Missing name"))?,
                piece_length: info
                    .pull(b"piece length")
                    .and_then(BencodedValue::into_int)
                    .ok_or(bterror!("Missing piece length"))?
                    .try_into()?,
                pieces: info
                    .pull(b"pieces")
                    .and_then(BencodedValue::into_bytes)
                    .map(|value| value.chunks(20).filter_map(|x| x.try_into().ok()).collect())
                    .ok_or(bterror!("Missing pieces"))?,
                file_info: match info.pull(b"length").and_then(BencodedValue::into_int) {
                    Some(length) => FileInfo::Length(length as usize),
                    None => FileInfo::Files(
                        info.pull(b"files")
                            .and_then(BencodedValue::into_list)
                            .map(|files| {
                                files
                                    .into_iter()
                                    .filter_map(BencodedValue::into_dict)
                                    .map(|mut file| {
                                        Ok::<_, BitTorrentError>(File {
                                            length: file
                                                .pull(b"length")
                                                .and_then(BencodedValue::into_int)
                                                .ok_or(bterror!("Missing length"))?
                                                as usize,
                                            path: file
                                                .pull(b"path")
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
    pub fn save_to_path(&self, path: &PathBuf, mut data: impl Read) -> std::io::Result<()> {
        match &self.info.file_info {
            FileInfo::Length(_) => {
                let mut file = fs::File::create(path.join(&self.info.name))?;
                std::io::copy(&mut data, &mut file)?;
            }
            FileInfo::Files(files) => {
                let base_path = path.join(&self.info.name);
                for file_metadata in files {
                    let mut file_path = base_path.clone();
                    for path_part in &file_metadata.path {
                        file_path.push(path_part);
                    }
                    create_dir_all(file_path.parent().unwrap())?;
                    let mut file = fs::File::create(file_path)?;
                    std::io::copy(
                        &mut data.by_ref().take(file_metadata.length as u64),
                        &mut file,
                    )?;
                }
            }
        }
        Ok(())
    }
}
