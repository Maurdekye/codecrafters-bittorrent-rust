use crate::{bterror, error::BitTorrentError};
use regex::Regex;
use serde_json::Value;

fn consume_bencoded_value(data_stream: &mut &str) -> Result<Value, BitTorrentError> {
    let string_length_re = Regex::new(r"^(\d+):").unwrap();
    let integer_re = Regex::new(r"^i(-?\d+)e").unwrap();
    match data_stream.chars().next() {

        // decode list
        Some('l') => {
            *data_stream = &data_stream[1..];
            let mut list: Vec<Value> = Vec::new();
            loop {
                match data_stream.chars().next() {
                    Some('e') => {
                        *data_stream = &data_stream[1..];
                        break
                    },
                    None => return Err(bterror!("Error parsing encoded list: ending delimiter missing")),
                    _ => list.push(consume_bencoded_value(data_stream)?)
                }
            }
            Ok(Value::Array(list))
        },

        // decode integer
        Some('i') => {
            let captures = integer_re
                .captures(&data_stream)
                .ok_or(bterror!("Error parsing encoded integer"))?;
            let integer: i64 = captures
                .get(1)
                .ok_or(bterror!("Integer not present"))?
                .as_str()
                .parse()
                .map_err(|err| bterror!("Error parsing encoded integer: {}", err))?;
            let content_end = captures
                .get(0)
                .ok_or(bterror!("Error parsing encoded integer chars"))?
                .end();

            *data_stream = &data_stream[content_end..];

            Ok(Value::Number(integer.into()))
        }
        Some(c) => {

            // decode string
            if c.is_numeric() {
                let captures = string_length_re
                    .captures(&data_stream)
                    .ok_or(bterror!("Error parsing encoded string length"))?;
                let length: usize = captures
                    .get(1)
                    .ok_or(bterror!("String length not present"))?
                    .as_str()
                    .parse()
                    .map_err(|err| bterror!("Error parsing encoded string length: {}", err))?;
                let content_start = captures
                    .get(0)
                    .ok_or(bterror!("Error parsing encoded string"))?
                    .end();
                let content_end = content_start + length;
                if content_end > data_stream.len() {
                    Err(bterror!("Content length too long: requested {} chars, stream only has {} chars", content_end, data_stream.len()))
                } else {
                    let content = &data_stream[content_start..content_end];
    
                    *data_stream = &data_stream[content_end..];
    
                    Ok(Value::String(content.to_string()))
                }
            } else {
                Err(bterror!("Unhandled encoded value: {}", data_stream))
            }
        }
        None => Err(bterror!("Unhandled encoded value: {}", data_stream)),
    }
}

pub fn decode_bencoded_value(raw_value: &str) -> Result<Value, BitTorrentError> {
    let mut stream = raw_value;
    consume_bencoded_value(&mut stream)
}