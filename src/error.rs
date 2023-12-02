use std::fmt::Display;
use std::error::Error;

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
    pub fn new(details: String) -> BitTorrentError {
        BitTorrentError { details: details }
    }
}

impl Display for BitTorrentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "BitTorrent Error: {}", self.details)
    }
}

impl Error for BitTorrentError {}
