use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
};

use clap::Parser;
use download::download_piece_from_peer;
use error::BitTorrentError;
use handshake::{send_handshake, HandshakeMessage};
use info::read_metainfo;
use regex::Regex;
use tracker::query_tracker;

use crate::{
    corkboard::corkboard_download, decode::Decoder, download::download_file, util::bytes_to_hex,
};

mod corkboard;
mod decode;
mod download;
mod encode;
mod error;
mod handshake;
mod info;
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
    Info(InfoArgs),
    Peers(PeersArgs),
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
struct InfoArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_file: String,
}

#[derive(Parser)]
struct PeersArgs {
    /// File with torrent information
    #[arg(required = true)]
    torrent_file: String,

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
    torrent_file: String,

    /// IP & Port of peer to connect to
    #[clap(required = true, value_parser = peer_validator)]
    peer: SocketAddrV4,

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

    /// Number of workers
    #[arg(short, long, default_value_t = 5)]
    workers: usize,
}

/// Validate peer ip:port format.
fn peer_validator(val: &str) -> Result<SocketAddrV4, String> {
    let port_ip_re = Regex::new(r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3}):(\d{1,5})").unwrap();
    match port_ip_re.captures(val) {
        None => Err("Invalid ip:port format specified".to_string()),
        Some(captures) => {
            let ip_parts = (1..=4)
                .map(|i| {
                    captures
                        .get(i)
                        .unwrap()
                        .as_str()
                        .parse()
                        .map_err(|_| format!("IP part {} not in the range 0-255", i))
                })
                .collect::<Result<Vec<u8>, String>>()?;
            Ok(SocketAddrV4::new(
                Ipv4Addr::new(ip_parts[0], ip_parts[1], ip_parts[2], ip_parts[3]),
                captures
                    .get(5)
                    .unwrap()
                    .as_str()
                    .parse()
                    .map_err(|_| "Port not in the range 0-65535")?,
            ))
        }
    }
}

fn main() -> Result<(), BitTorrentError> {
    let args = Args::parse();
    // let args = Args::parse_from(["_", "download", "-o", "sample.txt", "sample.torrent"]);

    match args.subcommand {
        Subcommand::Decode(decode_args) => {
            let mut content = decode_args.raw_content.as_bytes();
            let decoded = Decoder::new().consume_bencoded_value(&mut content)?;
            println!("{}", decoded);
        }
        Subcommand::Info(info_args) => {
            let meta_info = read_metainfo(&info_args.torrent_file)?;
            let info_hash = meta_info.info.hash()?;
            println!("Tracker URL: {}", meta_info.announce);
            println!("Length: {}", meta_info.info.length);
            println!("Info Hash: {}", bytes_to_hex(&info_hash));
            println!("Piece Length: {}", meta_info.info.piece_length);
            println!("Piece Hashes:");
            for hash in meta_info.info.pieces()? {
                println!("{}", bytes_to_hex(&hash));
            }
        }
        Subcommand::Peers(peers_args) => {
            let meta_info = read_metainfo(&peers_args.torrent_file)?;
            let tracker_info = query_tracker(&meta_info, &peers_args.peer_id, peers_args.port)?;
            for sock in tracker_info.peers()? {
                println!("{}", sock);
            }
        }
        Subcommand::Handshake(handshake_args) => {
            let meta_info = read_metainfo(&handshake_args.torrent_file)?;
            let message = HandshakeMessage::new(&meta_info, &handshake_args.peer_id)?;
            let mut stream = TcpStream::connect(&handshake_args.peer)
                .map_err(|err| bterror!("Error connecting to peer: {}", err))?;
            let response = send_handshake(&mut stream, &message)?;
            println!("Peer ID: {}", bytes_to_hex(&response.peer_id));
        }
        Subcommand::DownloadPiece(download_piece_args) => {
            let meta_info = read_metainfo(&download_piece_args.torrent_file)?;
            let data = download_piece_from_peer(
                &meta_info,
                download_piece_args.piece_id as u32,
                &download_piece_args.peer_id,
                download_piece_args.port,
            )?;
            fs::write(&download_piece_args.output, data)
                .map_err(|err| bterror!("Error writing to disk: {}", err))?;
            println!(
                "Piece {} downloaded to {}.",
                download_piece_args.piece_id, &download_piece_args.output
            );
        }
        Subcommand::Download(download_args) => {
            let meta_info = read_metainfo(&download_args.torrent_file)?;
            let full_file = download_file(&meta_info, &download_args.peer_id, download_args.port)?;
            fs::write(&download_args.output, full_file)
                .map_err(|err| bterror!("Error writing to disk: {}", err))?;
            println!(
                "Downloaded {} to {}.",
                download_args.torrent_file, &download_args.output
            );
        }
        Subcommand::DownloadV2(download_args) => {
            let meta_info = read_metainfo(&download_args.torrent_file)?;
            let full_file = corkboard_download(
                &meta_info,
                &download_args.peer_id,
                download_args.port,
                download_args.workers,
            )?;
            fs::write(&download_args.output, full_file)
                .map_err(|err| bterror!("Error writing to disk: {}", err))?;
            println!(
                "Downloaded {} to {}.",
                download_args.torrent_file, &download_args.output
            );
        }
    }
    Ok(())
}
