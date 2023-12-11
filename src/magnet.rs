use multihash::Multihash;

use crate::{bterror, error::BitTorrentError};

#[derive(Debug, Clone)]
pub struct Magnet {
    pub xt: [u8; 20],
    pub dn: Option<String>,
    pub tr: Vec<String>,
    pub xpe: Vec<String>,
}

impl Magnet {
    pub fn from_uri(uri: String) -> Result<Self, BitTorrentError> {
        if !uri.starts_with("magnet:?") {
            return Err(bterror!("Invalid magnet uri"));
        }

        let mut xt = None;
        let mut dn = None;
        let mut tr = Vec::new();
        let mut xpe = Vec::new();

        for (key, value) in uri[8..]
            .split("&")
            .filter_map(|kvpair| kvpair.split_once("="))
        {
            match key {
                "xt" => {
                    xt = match &value[..9] {
                        "urn:btih:" => Some(
                            hex::decode(&value[9..])?
                                .try_into()
                                .map_err(|_| bterror!("Could not decode info hash"))?,
                        ),
                        "urn:btmh:" => {
                            let raw_multihash = &value[9..].as_bytes();
                            let decoded: Multihash<20> =
                                Multihash::<20>::from_bytes(raw_multihash)?;
                            Some(decoded.digest().try_into()?)
                        }
                        _ => return Err(bterror!("Invalid xt")),
                    }
                }
                "dn" => dn = Some(value.to_string()),
                "tr" => tr.push(value.to_string()),
                "x.pe" => xpe.push(value.to_string()),
                key => return Err(bterror!("Unknown key in magnet uri: {key}")),
            }
        }

        match xt {
            Some(xt) => Ok(Self { xt, dn, tr, xpe }),
            None => return Err(bterror!("Missing xt")),
        }
    }
}
