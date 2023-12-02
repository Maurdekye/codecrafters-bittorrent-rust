use clap::Parser;
use error::BitTorrentError;
use info::read_metainfo;
use tracker::query_tracker;

use crate::{decode::Decoder, util::bytes_to_hex};

mod decode;
mod encode;
mod error;
mod info;
mod tracker;
mod util;

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
    file: String,
}

#[derive(Parser)]
struct PeersArgs {
    /// File with torrent information
    #[arg(required = true)]
    file: String,

    /// Peer ID for GET request
    #[arg(short, long, default_value = "00112233445566778899")]
    peer_id: String,

    /// Port for GET request
    #[arg(short = 't', long, default_value_t = 6881)]
    port: u16,
}

fn main() -> Result<(), BitTorrentError> {
    let args = Args::parse();
    // let args = Args::parse_from(["_", "decode", "lli4eei5ee"]);

    match args.subcommand {
        Subcommand::Decode(decode_args) => {
            let mut content = decode_args.raw_content.as_bytes();
            let decoded = Decoder::new().consume_bencoded_value(&mut content)?;
            println!("{}", decoded);
        }
        Subcommand::Info(info_args) => {
            let meta_info = read_metainfo(&info_args.file)?;
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
            let meta_info = read_metainfo(&peers_args.file)?;
            let tracker_info = query_tracker(&meta_info, &peers_args.peer_id, peers_args.port)?;
            for (ip, port) in tracker_info.peers()? {
                println!("{}:{}", ip, port);
            }
        }
    }
    Ok(())
}
