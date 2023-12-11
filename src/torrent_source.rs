use std::iter::once;

use crate::{
    error::BitTorrentError, info::MetaInfo, info_field, magnet::Magnet, util::bytes_to_hex,
};

#[derive(Debug, Clone)]
pub enum TorrentSource {
    File(MetaInfo),
    Magnet(Magnet),
}

impl TorrentSource {
    pub fn from_string(s: &str) -> Result<Self, BitTorrentError> {
        match s.get(..9) {
            Some("magnet?:") => Ok(TorrentSource::Magnet(Magnet::from_uri(s.to_string())?)),
            _ => Ok(TorrentSource::File(MetaInfo::from_file(&s)?)),
        }
    }

    pub fn hash(&self) -> Result<[u8; 20], BitTorrentError> {
        match self {
            TorrentSource::File(meta_info) => meta_info.info_hash(),
            TorrentSource::Magnet(magnet) => Ok(magnet.xt),
        }
    }

    pub fn trackers(&self) -> Vec<String> {
        match self {
            TorrentSource::File(meta_info) => once(meta_info.announce.clone())
                .chain(
                    meta_info
                        .announce_list
                        .clone()
                        .unwrap_or(vec![])
                        .into_iter()
                        .map(|l| l.first().map(Clone::clone)),
                )
                .filter_map(|x| x)
                .collect(),
            TorrentSource::Magnet(magnet) => magnet.tr.clone(),
        }
    }

    pub fn name(&self) -> String {
        match self {
            TorrentSource::File(meta_info) => info_field!(&meta_info.info, name).clone(),
            TorrentSource::Magnet(magnet) => magnet
                .dn
                .clone()
                .unwrap_or_else(|| bytes_to_hex(&magnet.xt)),
        }
    }
}
