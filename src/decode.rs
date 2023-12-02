use crate::{bterror, error::BitTorrentError};
use regex::bytes::Regex;
use serde_json::{Map, Value};
use std::str::from_utf8;

pub struct Decoder {
    string_re: Regex,
    integer_re: Regex,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            string_re: Regex::new(r"^(\d+):").expect("regex compilation failed"),
            integer_re: Regex::new(r"^i(-?\d+)e").expect("regex compilation failed"),
        }
    }

    fn decode_dictionary(&self, data_stream: &mut &[u8]) -> Result<Value, BitTorrentError> {
        *data_stream = &data_stream[1..];
        let mut dict: Map<String, Value> = Map::new();
        loop {
            match data_stream.get(0) {
                Some(b'e') => {
                    *data_stream = &data_stream[1..];
                    break;
                }
                None => {
                    return Err(bterror!(
                        "Error parsing encoded dictionary: ending delimiter missing"
                    ))
                }
                _ => {
                    let first_item = self.consume_bencoded_value(data_stream)?;
                    let key = first_item
                        .as_str()
                        .ok_or(bterror!("Key element is not a string"))?;
                    let value = self.consume_bencoded_value(data_stream)?;
                    dict.insert(key.to_string(), value);
                }
            }
        }
        Ok(Value::Object(dict))
    }

    fn decode_list(&self, data_stream: &mut &[u8]) -> Result<Value, BitTorrentError> {
        *data_stream = &data_stream[1..];
        let mut list: Vec<Value> = Vec::new();
        loop {
            match data_stream.get(0) {
                Some(b'e') => {
                    *data_stream = &data_stream[1..];
                    break;
                }
                None => {
                    return Err(bterror!(
                        "Error parsing encoded list: ending delimiter missing"
                    ))
                }
                _ => list.push(self.consume_bencoded_value(data_stream)?),
            }
        }
        Ok(Value::Array(list))
    }

    fn decode_integer(&self, data_stream: &mut &[u8]) -> Result<Value, BitTorrentError> {
        let captures = self
            .integer_re
            .captures(&data_stream)
            .ok_or(bterror!("Error parsing encoded integer"))?;
        let integer: i64 = from_utf8(
            captures
                .get(1)
                .ok_or(bterror!("Integer not present"))?
                .as_bytes(),
        )
        .map_err(|err| bterror!("Error decoding integer bytes: {}", err))?
        .parse()
        .map_err(|err| bterror!("Error parsing encoded integer: {}", err))?;
        let content_end = captures
            .get(0)
            .ok_or(bterror!("Error parsing encoded integer chars"))?
            .end();

        *data_stream = &data_stream[content_end..];

        Ok(Value::Number(integer.into()))
    }

    fn decode_string(&self, data_stream: &mut &[u8]) -> Result<Value, BitTorrentError> {
        let captures = self
            .string_re
            .captures(&data_stream)
            .ok_or(bterror!("Error parsing encoded string length"))?;
        let length: usize = from_utf8(
            captures
                .get(1)
                .ok_or(bterror!("String length not present"))?
                .as_bytes(),
        )
        .map_err(|err| bterror!("Error decoding integer bytes: {}", err))?
        .parse()
        .map_err(|err| bterror!("Error parsing encoded string length: {}", err))?;
        let content_start = captures
            .get(0)
            .ok_or(bterror!("Error parsing encoded string"))?
            .end();
        let content_end = content_start + length;
        if content_end > data_stream.len() {
            Err(bterror!(
                "Content length too long: requested {} chars, stream only has {} chars",
                content_end,
                data_stream.len()
            ))
        } else {
            let content =
                String::from_utf8_lossy(&data_stream[content_start..content_end]).to_string();

            *data_stream = &data_stream[content_end..];

            Ok(Value::String(content.to_string()))
        }
    }

    pub fn consume_bencoded_value(
        &self,
        data_stream: &mut &[u8],
    ) -> Result<Value, BitTorrentError> {
        match data_stream.get(0) {
            Some(b'd') => self.decode_dictionary(data_stream),
            Some(b'l') => self.decode_list(data_stream),
            Some(b'i') => self.decode_integer(data_stream),
            Some(c) if c.is_ascii_digit() => self.decode_string(data_stream),
            _ => Err(bterror!("Unhandled encoded value: {:?}", data_stream)),
        }
    }
}
