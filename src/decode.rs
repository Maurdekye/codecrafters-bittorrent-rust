use crate::{bterror, error::BitTorrentError};
use regex::Regex;
use serde_json::Value;

pub fn decode_bencoded_value(raw_value: &str) -> Result<Value, BitTorrentError> {
    let decode_re =
        Regex::new(r"^(\d+):(.*)$").expect("Decode regex did not compile (for some reason??)");

    match decode_re.captures(raw_value) {
        Some(captures) => {
            let passed_length: usize = captures
                .get(1)
                .ok_or(bterror!("length specifier not present"))?
                .as_str()
                .parse()
                .map_err(|err| bterror!("Error parsing encoded string length: {}", err))?;
            let content = captures
                .get(2)
                .ok_or(bterror!("Content not present"))?
                .as_str();
            if passed_length != content.len() {
                Err(bterror!(
                    "Mismatched content length: parsed length {}, actual length {}",
                    passed_length,
                    content.len()
                ))
            } else {
                Ok(Value::String(content.to_string()))
            }
        }
        None => Err(bterror!("Unhandled encoded value: {}", raw_value)),
    }
}
