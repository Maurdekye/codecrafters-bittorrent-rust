use std::array::TryFromSliceError;
use std::error::Error;
use std::fmt::Display;
use std::string::FromUtf8Error;
use std::sync::PoisonError;

use hex::FromHexError;


#[macro_export]
macro_rules! bterror {
    ($($arg:tt)*) => {
        BitTorrentError::new(format!($($arg)*))
    };
}

#[derive(Debug)]
pub struct BitTorrentError {
    details: String,
}

impl BitTorrentError {
    /// Create a new BitTorrentError.
    pub fn new(details: String) -> BitTorrentError {
        BitTorrentError { details: details }
    }
}

impl Display for BitTorrentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BitTorrent Error: {}", self.details)
    }
}

impl From<anyhow::Error> for BitTorrentError {
    fn from(value: anyhow::Error) -> Self {
        BitTorrentError::new(
            value
                .chain()
                .into_iter()
                .map(|err| err.to_string())
                .collect::<Vec<_>>()
                .join(": "),
        )
    }
}

macro_rules! from_err {
    ($err_type:ty) => {
        impl From<$err_type> for BitTorrentError {
            fn from(value: $err_type) -> Self {
                BitTorrentError::new(value.to_string())
            }
        }
    };
}

from_err!(std::io::Error);
from_err!(serde_json::Error);
from_err!(TryFromSliceError);
from_err!(FromHexError);
from_err!(multihash::Error);
from_err!(FromUtf8Error);

impl<T> From<PoisonError<T>> for BitTorrentError {
    fn from(value: PoisonError<T>) -> Self {
        BitTorrentError::new(value.to_string())
    }
}

impl Error for BitTorrentError {}
