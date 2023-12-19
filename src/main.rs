#![feature(ip_bits)]

use std::{
    fs,
    net::{AddrParseError, SocketAddr, TcpStream, UdpSocket},
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use anyhow::Context;
use clap::{ArgAction, Parser};
use download::download_piece_from_peer;
use error::BitTorrentError;
use tracker::{
    dht::{Dht, DhtMessage, Query},
    multimodal::Tracker,
};

use crate::{
    bencode::BencodedValue,
    download::{
        corkboard::{corkboard_download, Config},
        download_file,
    },
    info::{Info, MetaInfo},
    peer::{
        message::{ExtensionHandshake, ExtensionMetadata, PeerMessageCodec},
        tcp::TcpPeer,
    },
    torrent_source::TorrentSource,
    tracker::{dht::KrpcMessage, TrackerResponse},
    util::bytes_to_hex,
};

mod bencode;
mod bytes;
mod download;
mod error;
mod info;
mod magnet;
mod multithread;
mod peer;
mod pool;
mod torrent_source;
mod tracker;
mod util;

/// Rust BitTorrent Downloader
#[derive(Parser)]
#[clap(about, version)]
struct Args {
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(Parser)]
enum Subcommand {
    Decode(DecodeArgs),
    DecodeHex(DecodeHexArgs),
    Info(InfoArgs),
    Peers(PeersArgs),
    DhtPing(DhtPingArgs),
    Handshake(HandshakeArgs),
    #[command(name = "download_piece")]
    DownloadPiece(DownloadPieceArgs),
    Download(DownloadArgs),
    DownloadV2(DownloadV2Args),
}

#[derive(Parser)]
struct DecodeArgs {
    /// String to decode
    #[arg(required = true)]
    raw_content: String,
}

#[derive(Parser)]
struct DecodeHexArgs {
    /// Hex string to decode
    #[arg(required = true)]
    raw_content: String,
}

#[derive(Parser)]
struct InfoArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_file: String,
}

#[derive(Parser)]
struct PeersArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_source: String,

    /// Peer ID for GET request
    #[arg(short, long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for GET request
    #[arg(short = 't', long, default_value_t = 6881)]
    port: u16,
}

#[derive(Parser)]
struct DhtPingArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_source: String,

    /// Peer ID for GET request
    #[arg(short, long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for GET request
    #[arg(short = 't', long, default_value_t = 6881)]
    port: u16,
}

#[derive(Parser)]
struct HandshakeArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_source: String,

    /// IP & Port of peer to connect to
    #[clap(required = true, value_parser = peer_validator)]
    peer: SocketAddr,

    /// Peer ID for handshake
    #[arg(short = 'i', long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for handshake
    #[arg(short, long, default_value_t = 6881)]
    port: u16,
}

#[derive(Parser)]
struct DownloadPieceArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_file: String,

    /// Piece to download
    #[arg(required = true)]
    piece_id: usize,

    /// Output file location
    #[arg(short, long)]
    output: String,

    /// Peer ID for handshake
    #[arg(short = 'i', long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for handshake
    #[arg(short, long, default_value_t = 6881)]
    port: u16,
}

#[derive(Parser)]
struct DownloadArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_file: String,

    /// Output file location
    #[arg(short, long)]
    output: String,

    /// Peer ID for handshake
    #[arg(short = 'i', long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for handshake
    #[arg(short, long, default_value_t = 6881)]
    port: u16,
}

#[derive(Parser)]
struct DownloadV2Args {
    /// File with torrent information
    #[arg(required = true)]
    torrent_source: String,

    /// Output directory
    #[arg(short, long, value_parser = pathbuf_parse, default_value = "downloads/")]
    output: PathBuf,

    /// Peer ID for handshake
    #[arg(short = 'i', long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for handshake
    #[arg(short, long, default_value_t = 6881)]
    port: u16,

    /// Number of workers
    #[arg(short, long, default_value_t = 32)]
    workers: usize,

    /// Print verbose logging information
    #[arg(short, long, action = ArgAction::SetTrue)]
    verbose: bool,
}

fn pathbuf_parse(val: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(val))
}

/// Validate peer ip:port format.
fn peer_validator(val: &str) -> Result<SocketAddr, String> {
    val.parse().map_err(|err: AddrParseError| err.to_string())
}

fn main() -> Result<(), BitTorrentError> {
    let args = Args::parse();

    match args.subcommand {
        Subcommand::Decode(decode_args) => {
            let mut content = decode_args.raw_content.as_bytes();
            let decoded: BencodedValue = BencodedValue::ingest(&mut content)?;
            println!("{}", decoded);
        }
        Subcommand::DecodeHex(decode_args) => {
            let content = hex::decode(decode_args.raw_content)?;
            let mut cslice = &content[..];
            while cslice.len() > 0 {
                let decoded = BencodedValue::ingest(&mut cslice)?;
                dbg!(&decoded);
                let _ = dbg!(<Result<MetaInfo, _>>::from(decoded.clone()));
                let _ = dbg!(<Result<Info, _>>::from(decoded.clone()));
                let _ = dbg!(<Result<KrpcMessage, _>>::from(decoded.clone()));
                let _ = dbg!(<Result<ExtensionHandshake, _>>::from(decoded.clone()));
                let _ = dbg!(<Result<ExtensionMetadata, _>>::from(decoded.clone()));
                let _ = dbg!(<Result<TrackerResponse, _>>::from(decoded.clone()));
            }
        }
        Subcommand::Info(info_args) => {
            let content = fs::read(&info_args.torrent_file)?;
            let decoded_value = BencodedValue::ingest(&mut &content[..])?;
            println!("{:#?}", decoded_value);
            let meta_info = MetaInfo::from_file(&info_args.torrent_file)?;
            let info_hash = meta_info.info_hash()?;
            println!("Tracker URL: {}", meta_info.announce_list.first().unwrap());
            println!("Length: {}", meta_info.length());
            println!("Info Hash: {}", bytes_to_hex(&info_hash));
            println!("Piece Length: {}", meta_info.info.pieces.len());
            println!("Piece Hashes:");
            for hash in meta_info.info.pieces {
                println!("{}", bytes_to_hex(&hash));
            }
        }
        Subcommand::Peers(peers_args) => {
            let torrent_source = TorrentSource::from_string(&peers_args.torrent_source)?;
            let mut tracker = Tracker::new(torrent_source, peers_args.peer_id, peers_args.port)?;
            let (peers, _) = tracker.query().unwrap();
            for sock in peers {
                println!("{}", sock);
            }
        }
        Subcommand::DhtPing(dht_ping_args) => {
            let torrent_source = TorrentSource::from_string(&dht_ping_args.torrent_source)?;
            // let info_hash = torrent_source.hash()?;
            let peer_id = dht_ping_args.peer_id.clone();
            let mut dht = Dht::new(torrent_source, peer_id.clone().into(), true);
            while let Some(node) = dht.nodes.pop_front() {
                dbg!(&node);
                let mut socket = UdpSocket::bind("0.0.0.0:0")?;
                let response = Dht::exchange_message(
                    &mut socket,
                    &node,
                    DhtMessage::Query(Query::Ping {
                        id: peer_id.clone().into()
                    }),
                );
                match response {
                    Ok(message) => {
                        dbg!(message);
                        break;
                    }
                    Err(err) => println!("Error: {}", err),
                }
            }
        }
        Subcommand::Handshake(handshake_args) => {
            let mut connection = TcpPeer {
                address: handshake_args.peer,
                torrent_source: TorrentSource::from_string(&handshake_args.torrent_source)?,
                peer_id: handshake_args.peer_id,
                stream: TcpStream::connect(&handshake_args.peer)
                    .with_context(|| "Error connecting to peer")?,
                bitfield: Vec::new(),
                port: handshake_args.port,
                verbose: false,
                timeout: Some(Duration::from_secs(60)),
                killswitch: Arc::new(AtomicBool::new(false)),
                encoder: PeerMessageCodec::default(),
                decoder: PeerMessageCodec::default(),
                choked: false,
            };
            let response = connection.handshake()?;
            println!("Peer ID: {}", bytes_to_hex(&response.peer_id));
        }
        Subcommand::DownloadPiece(download_piece_args) => {
            let meta_info = MetaInfo::from_file(&download_piece_args.torrent_file)?;
            let data = download_piece_from_peer::<TcpPeer>(
                meta_info,
                download_piece_args.piece_id as u32,
                &download_piece_args.peer_id,
                download_piece_args.port,
            )?;
            fs::write(&download_piece_args.output, data)
                .with_context(|| "Error writing to disk")?;
            println!(
                "Piece {} downloaded to {}.",
                download_piece_args.piece_id, &download_piece_args.output
            );
        }
        Subcommand::Download(download_args) => {
            let meta_info = MetaInfo::from_file(&download_args.torrent_file)?;
            let full_file =
                download_file::<TcpPeer>(meta_info, &download_args.peer_id, download_args.port)?;
            fs::write(&download_args.output, full_file).with_context(|| "Error writing to disk")?;
            println!(
                "Downloaded {} to {}.",
                download_args.torrent_file, &download_args.output
            );
        }
        Subcommand::DownloadV2(download_args) => {
            let torrent_source = TorrentSource::from_string(&download_args.torrent_source)?;
            // dbg!(&torrent_source);
            let temp_path: PathBuf = PathBuf::from("tmp/in-progress/").join(torrent_source.name());
            let (full_file, meta_info) = corkboard_download::<TcpPeer>(
                torrent_source,
                Config {
                    peer_id: download_args.peer_id,
                    port: download_args.port,
                    workers: download_args.workers,
                    verbose: download_args.verbose,
                    temp_path,
                    ..Default::default()
                },
            )?;
            println!("Saving to file");
            meta_info
                .save_to_path(&download_args.output, full_file)
                .with_context(|| "Error saving torrent file(s)")?;
            println!(
                "Downloaded {} to {}.",
                download_args.torrent_source,
                &download_args.output.to_str().unwrap()
            );
        }
    }
    Ok(())
}
