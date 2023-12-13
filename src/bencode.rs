use std::{collections::HashMap, fmt::Display, iter::once};

use crate::{bterror, error::BitTorrentError, types::Bytes};

pub type Number = i64;

#[derive(Clone, Debug)]
pub enum BencodedValue {
    Bytes(Bytes),
    Int(Number),
    List(Vec<BencodedValue>),
    Dict(HashMap<Bytes, BencodedValue>),
    None,
}

impl BencodedValue {
    pub fn encode(self) -> Result<Vec<u8>, BitTorrentError> {
        match self {
            Self::Bytes(bytes) => Ok(bytes.into_inner()),
            Self::Int(int) => Ok(once(b'i')
                .chain(int.to_string().as_bytes().to_vec())
                .chain(once(b'e'))
                .collect()),
            Self::List(list) => Ok(once(b'l')
                .chain(
                    list.into_iter()
                        .map(|value| value.encode())
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>(),
                )
                .chain(once(b'e'))
                .collect()),
            Self::Dict(mut dict) => {
                let mut pairs = dict
                    .drain()
                    .filter(|(_, val)| !matches!(val, BencodedValue::None))
                    .collect::<Vec<_>>();
                pairs.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));
                Ok([b'd']
                    .into_iter()
                    .chain(
                        pairs
                            .into_iter()
                            .flat_map(|(k, v)| [Self::Bytes(k).encode(), v.encode()])
                            .collect::<Result<Vec<_>, _>>()?
                            .into_iter()
                            .flatten(),
                    )
                    .chain([b'e'])
                    .collect())
            }
            Self::None => Ok(vec![]),
        }
    }

    pub fn ingest(bytes: &mut &[u8]) -> Result<Self, BitTorrentError>
    where
        Self: Sized,
    {
        match bytes.split_first() {
            None => Err(bterror!("No data")),
            Some((b'i', rest)) => {
                let end_token = rest
                    .iter()
                    .position(|&b| b == b'e')
                    .ok_or(bterror!("Missing integer end token"))?;
                *bytes = &rest[end_token + 1..];
                Ok(Self::Int(
                    String::from_utf8(rest[..end_token].to_vec())?.parse()?,
                ))
            }
            Some((b'l', rest)) => {
                let mut items = Vec::new();
                loop {
                    match rest.split_first() {
                        None => return Err(bterror!("Missing list end token")),
                        Some((b'e', rest)) => {
                            *bytes = rest;
                            break;
                        }
                        Some(_) => {
                            *bytes = rest;
                            let value = Self::ingest(bytes)?;
                            items.push(value);
                        }
                    }
                }
                Ok(Self::List(items))
            }
            Some((b'd', rest)) => {
                let mut map = HashMap::new();
                loop {
                    match rest.split_first() {
                        None => return Err(bterror!("Missing dict end token")),
                        Some((b'e', rest)) => {
                            *bytes = rest;
                            break;
                        }
                        Some(_) => {
                            *bytes = rest;
                            if let Self::Bytes(key) = Self::ingest(bytes)? {
                                let value = Self::ingest(bytes)?;
                                map.insert(key, value);
                            } else {
                                return Err(bterror!("Invalid dict key"));
                            }
                        }
                    }
                }
                Ok(Self::Dict(map))
            }
            Some((b'0'..=b'9', _)) => {
                let length_end_token = bytes
                    .iter()
                    .position(|&b| b == b':')
                    .ok_or(bterror!("Missing string length end token"))?;
                let length: usize =
                    String::from_utf8(bytes[..length_end_token].to_vec())?.parse()?;
                if let Some(string) = bytes.get(length_end_token + 1..length_end_token + 1 + length)
                {
                    *bytes = &bytes[length_end_token + 1 + length..];
                    Ok(Self::Bytes(Bytes(string.to_vec())))
                } else {
                    Err(bterror!("Insufficient characters remaining"))
                }
            }
            _ => Err(bterror!("Invalid bencoded value")),
        }
    }

    pub fn as_bytes(&self) -> Option<&Bytes> {
        if let Self::Bytes(bytes) = self {
            Some(bytes)
        } else {
            None
        }
    }

    pub fn into_bytes(self) -> Option<Bytes> {
        if let Self::Bytes(bytes) = self {
            Some(bytes)
        } else {
            None
        }
    }

    pub fn as_int(&self) -> Option<&Number> {
        if let Self::Int(int) = self {
            Some(int)
        } else {
            None
        }
    }

    pub fn into_int(self) -> Option<Number> {
        if let Self::Int(int) = self {
            Some(int)
        } else {
            None
        }
    }

    pub fn as_list(&self) -> Option<&Vec<BencodedValue>> {
        if let Self::List(list) = self {
            Some(list)
        } else {
            None
        }
    }

    pub fn into_list(self) -> Option<Vec<BencodedValue>> {
        if let Self::List(list) = self {
            Some(list)
        } else {
            None
        }
    }

    pub fn as_dict(&self) -> Option<&HashMap<Bytes, BencodedValue>> {
        if let Self::Dict(dict) = self {
            Some(dict)
        } else {
            None
        }
    }

    pub fn into_dict(self) -> Option<HashMap<Bytes, BencodedValue>> {
        if let Self::Dict(dict) = self {
            Some(dict)
        } else {
            None
        }
    }
}

impl Display for BencodedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BencodedValue::Bytes(bytes) => write!(f, "b\"{}\"", bytes.to_string())?,
            BencodedValue::Int(int) => write!(f, "{}", int)?,
            BencodedValue::List(list) => match list.split_first() {
                None => write!(f, "[]")?,
                Some((first, rest)) => {
                    write!(f, "[ {}", first)?;
                    for value in rest {
                        write!(f, ", {}", value)?;
                    }
                    write!(f, " ]")?;
                }
            },
            BencodedValue::Dict(dict) => match dict.iter().collect::<Vec<_>>().split_first() {
                None => write!(f, "{{}}")?,
                Some(((first_key, first_val), rest)) => {
                    write!(f, "{{ b\"{}\": {}", first_key.to_string(), first_val)?;
                    for (key, value) in rest {
                        write!(f, ", b\"{}\": {}", key, value)?;
                    }
                    write!(f, " }}")?;
                }
            },
            BencodedValue::None => write!(f, "null")?,
        }
        Ok(())
    }
}

// when's specialization :,(

// impl<I: Into<Number>> From<I> for BencodedValue {
//     fn from(i: I) -> Self {
//         Self::Int(i.into())
//     }
// }

// impl<B: Into<Bytes>> From<B> for BencodedValue {
//     fn from(b: B) -> Self {
//         Self::Bytes(b.into())
//     }
// }

// impl<H: Into<HashMap<Bytes, BencodedValue>>> From<H> for BencodedValue {
//     fn from(h: H) -> Self {
//         Self::Dict(h.into())
//     }
// }

// impl<L: Into<Vec<BencodedValue>>> From<L> for BencodedValue {
//     fn from(l: L) -> Self {
//         Self::List(l.into())
//     }
// }

impl From<Number> for BencodedValue {
    fn from(value: Number) -> Self {
        Self::Int(value)
    }
}

impl From<Bytes> for BencodedValue {
    fn from(value: Bytes) -> Self {
        Self::Bytes(value)
    }
}

impl<T: Into<BencodedValue>> From<HashMap<Bytes, T>> for BencodedValue {
    fn from(value: HashMap<Bytes, T>) -> Self {
        Self::Dict(value.into_iter().map(|(k, v)| (k, v.into())).collect())
    }
}

impl<T: Into<BencodedValue>> From<Vec<T>> for BencodedValue {
    fn from(l: Vec<T>) -> Self {
        Self::List(l.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<BencodedValue>> From<Option<T>> for BencodedValue {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => value.into(),
            None => Self::None,
        }
    }
}

#[macro_export]
macro_rules! dict {
    ($($k:expr => $v:expr),* $(,)?) => {{
        BencodedValue::Dict(std::collections::HashMap::from([$(($k[..].into(), $v.into()),)*]))
    }};
}

#[macro_export]
macro_rules! list {
    ($($v:expr),* $(,)?) => {{
        BencodedValue::List(Vec::from([$($v.into(),)*]))
    }};
}
