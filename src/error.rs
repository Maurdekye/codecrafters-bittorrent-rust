use std::error::Error;
use std::fmt::Display;

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
        writeln!(f, "BitTorrent Error: {}", self.details)
    }
}

impl From<anyhow::Error> for BitTorrentError {
    fn from(value: anyhow::Error) -> Self {
        BitTorrentError::new(value.to_string())
    }
}

impl Error for BitTorrentError {}
