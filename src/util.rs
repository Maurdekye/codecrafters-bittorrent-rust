use std::{net::TcpStream, io::Read};
use sha1::{Digest, Sha1};

use crate::{error::BitTorrentError, bterror};

pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

pub fn read_n_bytes(stream: &mut TcpStream, mut n: usize) -> Result<Vec<u8>, BitTorrentError> {
    let mut bytes = Vec::new();
    while n > 0 {
        let mut buf = vec![0u8; n];
        let num_read = stream
            .read(&mut buf)
            .map_err(|err| bterror!("Error reading tcp stream: {}", err))?;
        bytes.extend(&buf[..num_read]);
        n -= num_read;
    }
    Ok(bytes)
}

pub fn sha1_hash(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().into()
}