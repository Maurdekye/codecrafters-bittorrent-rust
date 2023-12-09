use crate::{bterror, error::BitTorrentError};
use anyhow::Context;
use base64::{engine::general_purpose, Engine};
use lazy_static::lazy_static;
use regex::bytes::Regex;
use serde_json::{Map, Value};
use std::str::from_utf8;

lazy_static! {
    pub static ref STRING_RE: Regex = Regex::new(r"^(\d+):").unwrap();
    pub static ref INTEGER_RE: Regex = Regex::new(r"^i(-?\d+)e").unwrap();
}

pub fn decode_maybe_b64_string(bytes: &[u8]) -> String {
    from_utf8(bytes)
            .map(String::from)
            .unwrap_or_else(|_| format!("base64:{}", general_purpose::STANDARD_NO_PAD.encode(bytes)))
}

/// Decode a bencoded dictionary into a `serde_json::Value`.
fn decode_dictionary(data: &mut &[u8]) -> Result<Value, BitTorrentError> {
    *data = &data[1..];
    let mut dict: Map<String, Value> = Map::new();
    loop {
        match data.get(0) {
            Some(b'e') => {
                *data = &data[1..];
                break;
            }
            None => {
                return Err(bterror!(
                    "Error parsing encoded dictionary: ending delimiter missing"
                ))
            }
            _ => {
                let first_item = consume_bencoded_value(data)?;
                let key = first_item
                    .as_str()
                    .ok_or(bterror!("Key element is not a string"))?;
                let value = consume_bencoded_value(data)?;
                dict.insert(key.to_string(), value);
            }
        }
    }
    Ok(Value::Object(dict))
}

/// Decode a bencoded list into a `serde_json::Value`.
fn decode_list(data: &mut &[u8]) -> Result<Value, BitTorrentError> {
    *data = &data[1..];
    let mut list: Vec<Value> = Vec::new();
    loop {
        match data.get(0) {
            Some(b'e') => {
                *data = &data[1..];
                break;
            }
            None => {
                return Err(bterror!(
                    "Error parsing encoded list: ending delimiter missing"
                ))
            }
            _ => list.push(consume_bencoded_value(data)?),
        }
    }
    Ok(Value::Array(list))
}

/// Decode a bencoded integer into a `serde_json::Value`.
fn decode_integer(data: &mut &[u8]) -> Result<Value, BitTorrentError> {
    let captures = INTEGER_RE
        .captures(&data)
        .ok_or(bterror!("Error parsing encoded integer"))?;
    let integer: i64 = from_utf8(
        captures
            .get(1)
            .ok_or(bterror!("Integer not present"))?
            .as_bytes(),
    )
    .with_context(|| "Error decoding integer bytes")?
    .parse()
    .with_context(|| "Error parsing encoded integer")?;
    let content_end = captures
        .get(0)
        .ok_or(bterror!("Error parsing encoded integer chars"))?
        .end();

    *data = &data[content_end..];

    Ok(Value::Number(integer.into()))
}

/// Decode a bencoded string into a `serde_json::Value`.
fn decode_string(data: &mut &[u8]) -> Result<Value, BitTorrentError> {
    let captures = STRING_RE
        .captures(&data)
        .ok_or(bterror!("Error parsing encoded string length"))?;
    let length: usize = from_utf8(
        captures
            .get(1)
            .ok_or(bterror!("String length not present"))?
            .as_bytes(),
    )
    .with_context(|| "Error decoding integer bytes")?
    .parse()
    .with_context(|| "Error parsing encoded string length")?;
    let content_start = captures
        .get(0)
        .ok_or(bterror!("Error parsing encoded string"))?
        .end();
    let content_end = content_start + length;
    if content_end > data.len() {
        Err(bterror!(
            "Content length too long: requested {} chars, stream only has {} chars",
            content_end,
            data.len()
        ))
    } else {
        let content = decode_maybe_b64_string(&data[content_start..content_end]);

        *data = &data[content_end..];

        Ok(Value::String(content.to_string()))
    }
}

/// Consume a single bencoded value from a byte slice.
/// Returns the value as a result, and drops the consumed value from the slice, modifying it in place.
pub fn consume_bencoded_value(
    data: &mut &[u8],
) -> Result<Value, BitTorrentError> {
    match data.get(0) {
        Some(b'd') => decode_dictionary(data),
        Some(b'l') => decode_list(data),
        Some(b'i') => decode_integer(data),
        Some(c) if c.is_ascii_digit() => decode_string(data),
        _ => Err(bterror!("Unhandled encoded value: {:?}", data)),
    }
}
