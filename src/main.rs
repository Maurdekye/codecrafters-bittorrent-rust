use clap::Parser;
use error::BitTorrentError;

use crate::decode::decode_bencoded_value;

mod decode;
mod error;

#[derive(Parser)]
#[clap(about, version)]
struct Args {
    #[clap(subcommand)]
    command: Subcommands,
}

#[derive(Parser)]
enum Subcommands {
    Decode(DecodeArgs),
}

#[derive(Parser)]
struct DecodeArgs {
    /// String to decode
    #[arg(required = true)]
    raw_content: String,
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() -> Result<(), BitTorrentError> {
    let args = Args::parse();

    match args.command {
        Subcommands::Decode(decode_args) => {
            println!(
                "{}",
                decode_bencoded_value(decode_args.raw_content.as_str())?
            );
        }
    }
    Ok(())
}
