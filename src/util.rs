#![allow(unused)]

use anyhow::Context;
use chrono::Utc;
use lazy_static::lazy_static;
use regex::Regex;
use sha1::{Digest, Sha1};
use std::{
    collections::HashSet,
    hash::Hash,
    io::Read,
    net::{TcpStream, UdpSocket},
    str::from_utf8,
    time::{Duration, SystemTime},
};

use crate::{bterror, error::BitTorrentError};

lazy_static! {
    pub static ref SOCKADDR_V4: Regex =
        Regex::new(r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3}):(\d{1,5})").unwrap();
}

/// Convert a hex string to a byte array.
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

/// Read n bytes from a TcpStream and return them as a Vec<u8>.
#[allow(unused)]
pub fn read_n_bytes_timeout(
    stream: &mut TcpStream,
    mut n: usize,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, BitTorrentError> {
    match timeout {
        None => read_n_bytes(stream, n),
        Some(timeout) => {
            stream.set_nonblocking(false)?;
            let addr = stream.peer_addr()?;
            let deadline = SystemTime::now() + timeout;
            let mut bytes = Vec::new();
            while n > 0 {
                let timeout = deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    .max(Duration::from_millis(1));
                stream.set_read_timeout(Some(timeout))?;
                let mut buf = vec![0u8; n];
                let num_read = stream
                    .read(&mut buf)
                    .with_context(|| "Error reading tcp stream")?;
                if num_read == 0 {
                    // println!(
                    //     "[{}][{}] {} - 0 = {} | {:.2}",
                    //     timestr(),
                    //     addr,
                    //     n,
                    //     n,
                    //     deadline
                    //         .duration_since(SystemTime::now())
                    //         .map_or(0.0, |timeout| timeout.as_secs_f32())
                    // );
                    if SystemTime::now() > deadline {
                        return Err(bterror!("Tcp read timeout"));
                    }
                } else {
                    // println!(
                    //     "[{}][{}] {} - {} = {}",
                    //     timestr(),
                    //     addr,
                    //     n,
                    //     num_read,
                    //     n - num_read
                    // );
                    bytes.extend(&buf[..num_read]);
                    n -= num_read;
                }
            }
            Ok(bytes)
        }
    }
}

/// Read n bytes from a TcpStream and return them as a Vec<u8>, busy waiting until the bytes arrive
#[allow(unused)]
pub fn read_n_bytes_timeout_busy(
    stream: &mut TcpStream,
    mut n: usize,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, BitTorrentError> {
    let start = SystemTime::now();
    stream.set_nonblocking(true)?;
    let addr = stream.peer_addr()?;
    let mut bytes = Vec::new();
    while n > 0 {
        let mut buf = vec![0u8; n];
        let num_read = match stream.read(&mut buf) {
            Ok(num_read) => num_read,
            Err(err) if matches!(err.kind(), std::io::ErrorKind::WouldBlock) => 0,
            Err(err) => Err(err).context("Error reading tcp stream")?,
        };
        match num_read {
            0 => {
                if timeout.map_or(true, |timeout| {
                    // println!(
                    //     "[{}][{}] {} - 0 = {} | {:.2}",
                    //     timestr(),
                    //     addr,
                    //     n,
                    //     n,
                    //     timeout.as_secs_f32()
                    //         - SystemTime::now()
                    //             .duration_since(start)
                    //             .unwrap()
                    //             .as_secs_f32()
                    // );
                    SystemTime::now().duration_since(start).unwrap() > timeout
                }) {
                    return Err(bterror!("Tcp read timeout"));
                } else {
                    sleep(50);
                }
            }
            _ => {
                // println!(
                //     "[{}][{}] {} - {} = {}",
                //     timestr(),
                //     addr,
                //     n,
                //     num_read,
                //     n - num_read
                // );
                bytes.extend(&buf[..num_read]);
                n -= num_read;
            }
        }
    }
    Ok(bytes)
}

#[allow(unused)]
pub fn read_n_bytes(stream: &mut TcpStream, mut n: usize) -> Result<Vec<u8>, BitTorrentError> {
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
        .fold(0, |acc, bit| acc << 1 | u8::from(bit))
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

/// Decode a URL-safe percent-escaped string into a byte slice.
pub fn querystring_decode(s: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex = chars.next().unwrap().to_string() + &chars.next().unwrap().to_string();
            bytes.push(u8::from_str_radix(&hex, 16).unwrap());
        } else if c == '+' {
            bytes.push(b' ');
        } else {
            bytes.push(c as u8);
        }
    }

    bytes
}

pub fn cap_length(msg: String, max_len: usize) -> String {
    if msg.len() > max_len {
        format!("{}...", &msg[..max_len - 3])
    } else {
        msg.to_string()
    }
}

#[allow(unused)]
#[derive(Debug, Clone)]
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

pub trait Unique<I: Iterator>: Iterator
where
    I::Item: Eq + Hash + Clone,
{
    fn unique(self) -> UniqueIter<I>;
}

impl<I> Unique<I> for I
where
    I: Iterator,
    I::Item: Eq + Hash + Clone,
{
    fn unique(self) -> UniqueIter<I> {
        UniqueIter {
            iter: self,
            seen: HashSet::new(),
        }
    }
}

pub struct UniqueIter<I: Iterator>
where
    I::Item: Eq + Hash + Clone,
{
    iter: I,
    seen: HashSet<I::Item>,
}

impl<I> Iterator for UniqueIter<I>
where
    I: Iterator,
    I::Item: Eq + Hash + Clone,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(item) = self.iter.next() {
            if self.seen.insert(item.clone()) {
                return Some(item);
            }
        }
        None
    }
}

use std::fmt::Write;

pub fn pretty_print_hex(data: &[u8]) -> String {
    let mut output = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        write!(&mut output, "{:08X}: ", i * 16).unwrap();
        for &byte in chunk {
            write!(&mut output, "{:02X} ", byte).unwrap();
        }
        if chunk.len() < 16 {
            for _ in 0..16 - chunk.len() {
                output.push_str("   ");
            }
        }
        output.push_str(" |");
        for &byte in chunk {
            if byte.is_ascii_graphic() || byte == b' ' {
                output.push(byte as char);
            } else {
                output.push('.');
            }
        }
        output.push_str("|\n");
    }
    output
}
