use clap::Parser;
use error::BitTorrentError;
use info::read_metainfo;

use crate::decode::Decoder;

mod decode;
mod error;
mod info;

#[derive(Parser)]
#[clap(about, version)]
struct Args {
    #[clap(subcommand)]
    command: Subcommands,
}

#[derive(Parser)]
enum Subcommands {
    Decode(DecodeArgs),
    Info(InfoArgs),
}

#[derive(Parser)]
struct DecodeArgs {
    /// String to decode
    #[arg(required = true)]
    raw_content: String,
}

#[derive(Parser)]
struct InfoArgs {
    /// String to decode
    #[arg(required = true)]
    file: String,
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() -> Result<(), BitTorrentError> {
    let args = Args::parse();
    // let args = Args::parse_from(["_", "decode", "lli4eei5ee"]);

    match args.command {
        Subcommands::Decode(decode_args) => {
            let mut content = decode_args.raw_content.as_bytes();
            let decoded = Decoder::new().consume_bencoded_value(&mut content)?;
            println!("{}", decoded);
        }
        Subcommands::Info(info_args) => {
            let meta_info = read_metainfo(&info_args.file)?;
            println!("Tracker URL: {}", meta_info.announce);
            println!("Length: {}", meta_info.info.length);
        }
    }
    Ok(())
}
