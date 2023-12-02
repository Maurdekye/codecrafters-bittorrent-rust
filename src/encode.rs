use base64::{engine::general_purpose, Engine};
use serde_json::Value;

use crate::{bterror, error::BitTorrentError};

pub fn bencode_value(value: Value) -> Result<Vec<u8>, BitTorrentError> {
    match value {
        Value::Number(number) => Ok(format!("i{}e", number).as_bytes().to_vec()),
        Value::String(string) => {
            let bytes = if string.starts_with("base64:") {
                general_purpose::STANDARD_NO_PAD
                    .decode(&string[7..])
                    .map_err(|err| bterror!("Error decoding base64 for string value: {}", err))?
            } else {
                string.bytes().collect()
            };
            let length_prefix: Vec<u8> = format!("{}:", bytes.len()).bytes().collect();
            Ok(length_prefix.into_iter().chain(bytes).collect())
        }
        Value::Array(array) => Ok([b'l']
            .into_iter()
            .chain(
                array
                    .into_iter()
                    .map(|sub_value| bencode_value(sub_value))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten(),
            )
            .chain([b'e'])
            .collect()),
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> = map.into_iter().collect();
            pairs.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));
            Ok([b'd']
                .into_iter()
                .chain(
                    pairs
                        .into_iter()
                        .flat_map(|(k, v)| [bencode_value(Value::String(k)), bencode_value(v)])
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .flatten(),
                )
                .chain([b'e'])
                .collect())
        }
        _ => Err(bterror!("Invalid value type for bencoding: {}", value)),
    }
}
