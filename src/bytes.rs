use std::{
    collections::HashMap,
    fmt::{Debug, Display, Formatter},
    net::{SocketAddr, SocketAddrV4, SocketAddrV6, IpAddr, Ipv4Addr, Ipv6Addr},
    ops::{Deref, DerefMut},
    vec,
};

use crate::{bterror, error::BitTorrentError};

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

#[macro_export]
macro_rules! bytes {
    ($b:expr) => {
        Bytes($b.to_vec())
    };
}

fn escaped_byte_to_string(byte: u8) -> String {
    if byte.is_ascii_alphanumeric() || byte.is_ascii_punctuation() || byte.is_ascii_whitespace() {
        String::from(byte as char)
    } else {
        format!(r"\x{:02x}", byte)
    }
}

impl Bytes {
    pub fn to_string(&self) -> String {
        self.0.iter().cloned().map(escaped_byte_to_string).collect()
    }

    pub fn into_string(self) -> String {
        self.0.into_iter().map(escaped_byte_to_string).collect()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

impl IntoIterator for Bytes {
    type Item = u8;

    type IntoIter = vec::IntoIter<u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl From<String> for Bytes {
    fn from(value: String) -> Self {
        Bytes(value.into_bytes())
    }
}

impl From<&[u8]> for Bytes {
    fn from(value: &[u8]) -> Self {
        Bytes(value.to_vec())
    }
}

impl From<SocketAddrV4> for Bytes {
    fn from(value: SocketAddrV4) -> Self {
        Bytes(
            value
                .ip()
                .to_bits()
                .to_be_bytes()
                .into_iter()
                .chain(value.port().to_be_bytes())
                .collect(),
        )
    }
}

impl From<Bytes> for SocketAddrV4 {
    fn from(val: Bytes) -> Self {
        let ip = u32::from_be_bytes(val[0..4].try_into().unwrap());
        let port = u16::from_be_bytes(val[4..6].try_into().unwrap());
        SocketAddrV4::new(ip.into(), port)
    }
}

impl From<SocketAddrV6> for Bytes {
    fn from(value: SocketAddrV6) -> Self {
        Bytes(
            value
                .ip()
                .to_bits()
                .to_be_bytes()
                .into_iter()
                .chain(value.port().to_be_bytes())
                .collect(),
        )
    }
}

impl From<Bytes> for SocketAddrV6 {
    fn from(val: Bytes) -> Self {
        let ip = u128::from_be_bytes(val[0..16].try_into().unwrap());
        let port = u16::from_be_bytes(val[16..18].try_into().unwrap());
        SocketAddrV6::new(ip.into(), port, 0, 0)
    }
}

impl From<SocketAddr> for Bytes {
    fn from(value: SocketAddr) -> Self {
        match value {
            SocketAddr::V4(v4) => Bytes::from(v4),
            SocketAddr::V6(v6) => Bytes::from(v6),
        }
    }
}

impl From<Bytes> for Result<SocketAddr, BitTorrentError> {
    fn from(val: Bytes) -> Self {
        match val.len() {
            6 => Ok(SocketAddr::V4(SocketAddrV4::from(val))),
            18 => Ok(SocketAddr::V6(SocketAddrV6::from(val))),
            l => Err(bterror!("Invalid socket address: {l}")),
        }
    }
}

impl From<Bytes> for Ipv4Addr {
    fn from(value: Bytes) -> Self {
        u32::from_be_bytes(value[0..4].try_into().unwrap()).into()
    }
}

impl From<Ipv4Addr> for Bytes {
    fn from(value: Ipv4Addr) -> Self {
        Bytes(value.octets().to_vec())
    }
}

impl From<Bytes> for Ipv6Addr {
    fn from(value: Bytes) -> Self {
        u128::from_be_bytes(value[0..16].try_into().unwrap()).into()
    }
}

impl From<Ipv6Addr> for Bytes {
    fn from(value: Ipv6Addr) -> Self {
        Bytes(value.octets().to_vec())
    }
}

impl From<Bytes> for Result<IpAddr, BitTorrentError> {
    fn from(val: Bytes) -> Self {
        match val.len() {
            4 => Ok(IpAddr::V4(Ipv4Addr::from(val))),
            16 => Ok(IpAddr::V6(Ipv6Addr::from(val))),
            l => Err(bterror!("Invalid ip address: {l}")),
        }
    }
}

impl From<IpAddr> for Bytes {
    fn from(val: IpAddr) -> Self {
        match val {
            IpAddr::V4(v4) => Bytes::from(v4),
            IpAddr::V6(v6) => Bytes::from(v6),
        }
    }
}

impl FromIterator<u8> for Bytes {
    fn from_iter<T: IntoIterator<Item = u8>>(iter: T) -> Self {
        Bytes(iter.into_iter().collect())
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Bytes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Debug for Bytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "b\"{}\"", self.to_string())
    }
}

impl Display for Bytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

pub trait PullBytes<T> {
    fn pull(&mut self, key: &[u8]) -> Option<T>;
}

impl<T> PullBytes<T> for HashMap<Bytes, T> {
    fn pull(&mut self, key: &[u8]) -> Option<T> {
        self.remove(&bytes!(key))
    }
}
