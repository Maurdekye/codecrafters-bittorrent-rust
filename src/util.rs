use anyhow::Context;
use chrono::Utc;
use sha1::{Digest, Sha1};
use std::{io::Read, net::UdpSocket, str::from_utf8};

use crate::error::BitTorrentError;

/// Convert a hex string to a byte array.
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

/// Read n bytes from a TcpStream and return them as a Vec<u8>.
pub fn read_n_bytes<T: Read>(stream: &mut T, mut n: usize) -> Result<Vec<u8>, BitTorrentError> {
    let mut bytes = Vec::new();
    while n > 0 {
        let mut buf = vec![0u8; n];
        let num_read = stream
            .read(&mut buf)
            .with_context(|| "Error reading tcp stream")?;
        bytes.extend(&buf[..num_read]);
        n -= num_read;
    }
    Ok(bytes)
}

pub fn read_datagram(stream: &mut UdpSocket) -> Result<Vec<u8>, BitTorrentError> {
    let mut buf = [0u8; 65536];
    let num_read = stream
        .recv(&mut buf)
        .with_context(|| "Error reading udp datagram")?;
    Ok(buf[..num_read].to_vec())
}

/// Decode a bitfield from a byte in big-endian order.
pub fn decode_bitfield_be(bits: u8) -> [bool; 8] {
    (0..8)
        .rev()
        .map(|i| (bits >> i) & 1 == 1)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

/// Encode a bitfield into a byte in big-endian order.
pub fn encode_bitfield_be(bits: [bool; 8]) -> u8 {
    bits.into_iter()
        .fold(0, |acc, bit| acc << 1 | Into::<u8>::into(bit))
}

/// Calculate the SHA1 hash of a byte slice.
pub fn sha1_hash(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub fn sleep(millis: u64) {
    std::thread::sleep(std::time::Duration::from_millis(millis))
}

pub fn timestr() -> String {
    Utc::now().format("%T.%f").to_string()
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


pub fn cap_length(msg: String, max_len: usize) -> String {
    if msg.len() > max_len {
        format!("{}...", &msg[..max_len - 3])
    } else {
        msg.to_string()
    }
}