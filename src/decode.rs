use crate::{bterror, error::BitTorrentError};
use regex::Regex;
use serde_json::Value;

pub fn decode_bencoded_value(raw_value: &str) -> Result<Value, BitTorrentError> {
    let decode_string_re = Regex::new(r"^(\d+):(.*)$")
        .expect("String decode regex did not compile (for some reason??)");
    let decode_integer_re = Regex::new(r"^i(-?\d+)e$")
        .expect("Integer decode regex did not compile (for some reason??)");

    match (
        decode_string_re.captures(raw_value),
        decode_integer_re.captures(raw_value),
    ) {
        // decode string
        (Some(captures), _) => {
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
        
        // decode integer
        (_, Some(captures)) => {
            let value: i64 = captures
                .get(1)
                .ok_or(bterror!("value not present"))?
                .as_str()
                .parse()
                .map_err(|err| bterror!("Error parsing encoded integer: {}", err))?;
            Ok(Value::Number(value.into()))
        }

        // unrecognized value
        _ => Err(bterror!("Unhandled encoded value: {}", raw_value)),
    }
}
