use std::{
    fmt::{Debug, Display, Formatter},
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    ops::{Deref, DerefMut},
    vec,
};

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

#[macro_export]
macro_rules! bytes {
    ($b:expr) => {
        Bytes($b.to_vec())
    };
}

fn escaped_byte_to_string(byte: u8) -> String {
    if byte.is_ascii() {
        String::from(byte as char)
    } else {
        format!(r"\x{:02X}", byte)
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

impl Into<SocketAddrV4> for Bytes {
    fn into(self) -> SocketAddrV4 {
        let ip = u32::from_be_bytes(self[0..4].try_into().unwrap());
        let port = u16::from_be_bytes(self[4..6].try_into().unwrap());
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

impl Into<SocketAddrV6> for Bytes {
    fn into(self) -> SocketAddrV6 {
        let ip = u128::from_be_bytes(self[0..16].try_into().unwrap());
        let port = u16::from_be_bytes(self[16..18].try_into().unwrap());
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

impl Into<SocketAddr> for Bytes {
    fn into(self) -> SocketAddr {
        match self.len() {
            4 => SocketAddr::V4(Into::<SocketAddrV4>::into(self)),
            16 => SocketAddr::V6(Into::<SocketAddrV6>::into(self)),
            _ => panic!("Invalid socket address"),
        }
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
