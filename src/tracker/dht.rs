use std::{collections::VecDeque, fs::File, net::SocketAddr};

use lazy_static::lazy_static;
use serde::Deserialize;

use crate::{magnet::Magnet, torrent_source::TorrentSource};

lazy_static! {
    pub static ref BOOTSTRAP_DHT_NODES: Vec<Node> =
        serde_json::from_reader(File::open("dht.json").unwrap()).unwrap();
}

#[derive(Deserialize, Clone, Debug)]
pub struct Node {
    domain: String,
    port: u16,
}

#[derive(Clone, Debug)]
pub struct Dht {
    pub torrent_source: TorrentSource,
    pub nodes: VecDeque<Node>,
}

impl Dht {
    pub fn new(torrent_source: TorrentSource) -> Self {
        Self {
            torrent_source,
            nodes: VecDeque::from(BOOTSTRAP_DHT_NODES.clone()),
        }
    }
}

impl Iterator for Dht {
    type Item = Vec<SocketAddr>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.nodes.pop_front()?;
        todo!()
    }
}