use crate::Error;
use ciborium::value::Value;

/// Decodes one CBOR item from the front of `input`, returning it and the bytes left over.
pub(crate) fn decode_prefix(input: &[u8]) -> Result<(Value, &[u8]), Error> {
    let mut rest = input;
    let value: Value = ciborium::de::from_reader(&mut rest).map_err(|_| Error::MalformedCbor)?;
    Ok((value, rest))
}

/// Decodes exactly one CBOR item.
pub(crate) fn decode_exact(input: &[u8]) -> Result<Value, Error> {
    let (v, rest) = decode_prefix(input)?;
    if rest.is_empty() {
        Ok(v)
    } else {
        Err(Error::TrailingData)
    }
}

/// A map's entries, with duplicate keys rejected.
pub(crate) fn map_entries(v: Value) -> Result<Vec<(Value, Value)>, Error> {
    let Value::Map(entries) = v else {
        return Err(Error::MalformedCbor);
    };
    for (i, (k, _)) in entries.iter().enumerate() {
        if entries[..i].iter().any(|(o, _)| o == k) {
            return Err(Error::DuplicateKey);
        }
    }
    Ok(entries)
}

pub(crate) fn take_text(entries: &mut Vec<(Value, Value)>, key: &str) -> Option<Value> {
    let i = entries
        .iter()
        .position(|(k, _)| matches!(k, Value::Text(t) if t == key))?;
    Some(entries.swap_remove(i).1)
}

pub(crate) fn take_int(entries: &mut Vec<(Value, Value)>, key: i64) -> Option<Value> {
    let i = entries
        .iter()
        .position(|(k, _)| matches!(k, Value::Integer(n) if i64::try_from(*n) == Ok(key)))?;
    Some(entries.swap_remove(i).1)
}

pub(crate) fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(n) => i64::try_from(*n).ok(),
        _ => None,
    }
}
