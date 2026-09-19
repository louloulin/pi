//! Minimal definite-length CBOR codec for strict JSON values.
//!
//! Rust port of `packages/protocol/src/cbor/**`. The protocol payloads are
//! always [`serde_json::Value`]s, so this module only implements the strict
//! RFC 8949 subset the upstream encoder emits: definite-length arrays and
//! maps, UTF-8 text strings, integers, doubles, booleans and null. Byte
//! strings, tags, indefinite lengths and break markers are rejected, matching
//! upstream.

use std::fmt;

use serde_json::{Map, Number, Value};

/// Default maximum encoded byte length (16 MiB).
pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: usize = 16 * 1024 * 1024;
/// Default maximum array/map element count.
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: usize = 1_000_000;
/// Default maximum nesting depth.
pub const DEFAULT_MAX_CBOR_DEPTH: usize = 64;

const MAX_UINT32: u64 = 0xffff_ffff;
const MAX_CONFIGURED_DEPTH: usize = 512;

/// A CBOR encoding or decoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CborError {
    message: String,
}

impl CborError {
    /// Creates a CBOR error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error detail.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CborError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CborError {}

/// Resolved CBOR resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CborOptions {
    /// Maximum encoded byte length and maximum string length.
    pub max_byte_length: usize,
    /// Maximum number of array elements or map entries.
    pub max_container_length: usize,
    /// Maximum recursive item depth.
    pub max_depth: usize,
}

impl Default for CborOptions {
    fn default() -> Self {
        Self {
            max_byte_length: DEFAULT_MAX_CBOR_BYTE_LENGTH,
            max_container_length: DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
            max_depth: DEFAULT_MAX_CBOR_DEPTH,
        }
    }
}

impl CborOptions {
    /// Creates options with explicit limits (each clamped to its hard maximum).
    pub fn new(max_byte_length: usize, max_container_length: usize, max_depth: usize) -> Self {
        Self {
            max_byte_length: max_byte_length.min(MAX_UINT32 as usize),
            max_container_length: max_container_length.min(MAX_UINT32 as usize),
            max_depth: max_depth.min(MAX_CONFIGURED_DEPTH),
        }
    }
}

/// Encodes `value` into the protocol's strict CBOR subset.
pub fn encode_cbor(value: &Value, options: CborOptions) -> Result<Vec<u8>, CborError> {
    let mut writer = Writer {
        buffer: Vec::new(),
        max_byte_length: options.max_byte_length,
    };
    write_value(&mut writer, value, options, 0)?;
    Ok(writer.buffer)
}

/// Decodes exactly one CBOR item into a strict JSON value.
pub fn decode_cbor(bytes: &[u8], options: CborOptions) -> Result<Value, CborError> {
    if bytes.len() > options.max_byte_length {
        return Err(CborError::new(format!(
            "CBOR byte length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    let mut reader = Reader { bytes, offset: 0 };
    let value = read_item(&mut reader, options, 0)?;
    if reader.offset != bytes.len() {
        return Err(CborError::new("CBOR payload contains trailing data"));
    }
    Ok(value)
}

struct Writer {
    buffer: Vec<u8>,
    max_byte_length: usize,
}

impl Writer {
    fn write_byte(&mut self, value: u8) -> Result<(), CborError> {
        self.reserve(1)?;
        self.buffer.push(value);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CborError> {
        self.reserve(bytes.len())?;
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn reserve(&self, additional: usize) -> Result<(), CborError> {
        if self.buffer.len() + additional > self.max_byte_length {
            return Err(CborError::new(format!(
                "CBOR byte length exceeds configured limit of {}",
                self.max_byte_length
            )));
        }
        Ok(())
    }
}

fn write_argument(writer: &mut Writer, major_type: u8, value: u64) -> Result<(), CborError> {
    let prefix = major_type << 5;
    if value < 24 {
        writer.write_byte(prefix | value as u8)
    } else if value <= 0xff {
        writer.write_byte(prefix | 24)?;
        writer.write_byte(value as u8)
    } else if value <= 0xffff {
        writer.write_byte(prefix | 25)?;
        writer.write_bytes(&(value as u16).to_be_bytes())
    } else if value <= MAX_UINT32 {
        writer.write_byte(prefix | 26)?;
        writer.write_bytes(&(value as u32).to_be_bytes())
    } else {
        writer.write_byte(prefix | 27)?;
        writer.write_bytes(&value.to_be_bytes())
    }
}

fn write_text(writer: &mut Writer, value: &str, options: CborOptions) -> Result<(), CborError> {
    let bytes = value.as_bytes();
    if bytes.len() > options.max_byte_length {
        return Err(CborError::new(format!(
            "CBOR text string length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    write_argument(writer, 3, bytes.len() as u64)?;
    writer.write_bytes(bytes)
}

fn write_value(
    writer: &mut Writer,
    value: &Value,
    options: CborOptions,
    depth: usize,
) -> Result<(), CborError> {
    if depth > options.max_depth {
        return Err(CborError::new(format!(
            "CBOR nesting depth exceeds configured limit of {}",
            options.max_depth
        )));
    }
    match value {
        Value::Null => writer.write_byte(0xf6),
        Value::Bool(true) => writer.write_byte(0xf5),
        Value::Bool(false) => writer.write_byte(0xf4),
        Value::Number(number) => write_number(writer, number),
        Value::String(text) => write_text(writer, text, options),
        Value::Array(items) => {
            if items.len() > options.max_container_length {
                return Err(CborError::new(format!(
                    "CBOR array length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 4, items.len() as u64)?;
            for item in items {
                write_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(entries) => {
            if entries.len() > options.max_container_length {
                return Err(CborError::new(format!(
                    "CBOR map length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 5, entries.len() as u64)?;
            for (key, item) in entries {
                write_text(writer, key, options)?;
                write_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn write_number(writer: &mut Writer, number: &Number) -> Result<(), CborError> {
    if let Some(value) = number.as_u64() {
        return write_argument(writer, 0, value);
    }
    if let Some(value) = number.as_i64() {
        // Non-negative i64 values were already handled by `as_u64`.
        let encoded = (-1_i64).checked_sub(value).ok_or_else(|| {
            CborError::new("CBOR integers must be safe JavaScript integers")
        })?;
        return write_argument(writer, 1, encoded as u64);
    }
    let value = number
        .as_f64()
        .ok_or_else(|| CborError::new("Unsupported CBOR number"))?;
    if !value.is_finite() {
        return Err(CborError::new("CBOR numbers must be finite"));
    }
    if value.fract() == 0.0 {
        // Match upstream: an integral JavaScript number is encoded as an
        // integer argument, not a float.
        if value >= 0.0 && value <= MAX_UINT32 as f64 {
            return write_argument(writer, 0, value as u64);
        }
        if value < 0.0 && value >= -(MAX_UINT32 as f64) - 1.0 {
            return write_argument(writer, 1, (-1.0 - value) as u64);
        }
    }
    writer.write_byte(0xfb)?;
    writer.write_bytes(&value.to_be_bytes())
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn read_byte(&mut self) -> Result<u8, CborError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| CborError::new("Truncated CBOR payload"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&[u8], CborError> {
        if length > self.bytes.len().saturating_sub(self.offset) {
            return Err(CborError::new("Truncated CBOR payload"));
        }
        let slice = &self.bytes[self.offset..self.offset + length];
        self.offset += length;
        Ok(slice)
    }
}

fn read_item(reader: &mut Reader<'_>, options: CborOptions, depth: usize) -> Result<Value, CborError> {
    if depth > options.max_depth {
        return Err(CborError::new(format!(
            "CBOR nesting depth exceeds configured limit of {}",
            options.max_depth
        )));
    }
    let initial = reader.read_byte()?;
    let major_type = initial >> 5;
    let additional = initial & 0x1f;
    match major_type {
        0 => Ok(Value::Number(Number::from(read_argument(reader, additional)?))),
        1 => {
            let magnitude = read_argument(reader, additional)?;
            if magnitude > i64::MAX as u64 {
                return Err(CborError::new(
                    "Decoded CBOR integer is outside the safe range",
                ));
            }
            Ok(Value::Number(Number::from(-1 - magnitude as i64)))
        }
        2 => {
            let length = read_length(reader, additional, "byte string", options.max_byte_length)?;
            reader.read_bytes(length)?;
            Err(CborError::new(
                "CBOR byte strings are not valid protocol values",
            ))
        }
        3 => {
            let length = read_length(reader, additional, "text string", options.max_byte_length)?;
            let bytes = reader.read_bytes(length)?;
            let text = std::str::from_utf8(bytes)
                .map_err(|_| CborError::new("CBOR text string contains invalid UTF-8"))?;
            Ok(Value::String(text.to_owned()))
        }
        4 => {
            let length = read_length(reader, additional, "array", options.max_container_length)?;
            let mut items = Vec::with_capacity(length.min(1024));
            for _ in 0..length {
                items.push(read_item(reader, options, depth + 1)?);
            }
            Ok(Value::Array(items))
        }
        5 => {
            let length = read_length(reader, additional, "map", options.max_container_length)?;
            let mut entries = Map::new();
            for _ in 0..length {
                let key = match read_item(reader, options, depth + 1)? {
                    Value::String(key) => key,
                    _ => return Err(CborError::new("CBOR map keys must be strings")),
                };
                if entries.contains_key(&key) {
                    return Err(CborError::new("CBOR map contains a duplicate key"));
                }
                let value = read_item(reader, options, depth + 1)?;
                entries.insert(key, value);
            }
            Ok(Value::Object(entries))
        }
        6 => Err(CborError::new("CBOR tags are not supported")),
        7 => read_simple(reader, additional),
        _ => Err(CborError::new("Malformed CBOR major type")),
    }
}

fn read_simple(reader: &mut Reader<'_>, additional: u8) -> Result<Value, CborError> {
    match additional {
        20 => Ok(Value::Bool(false)),
        21 => Ok(Value::Bool(true)),
        22 => Ok(Value::Null),
        27 => {
            let bytes = reader.read_bytes(8)?;
            let mut array = [0u8; 8];
            array.copy_from_slice(bytes);
            let value = f64::from_be_bytes(array);
            if !value.is_finite() {
                return Err(CborError::new("Decoded CBOR number must be finite"));
            }
            // Normalise integral doubles to integers so a peer that encodes
            // `8.0` still satisfies the typed `u32` version field.
            if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= u64::MAX as f64 {
                if value >= 0.0 {
                    return Ok(Value::Number(Number::from(value as u64)));
                }
                return Ok(Value::Number(Number::from(value as i64)));
            }
            Number::from_f64(value)
                .map(Value::Number)
                .ok_or_else(|| CborError::new("Decoded CBOR number must be finite"))
        }
        31 => Err(CborError::new("CBOR break marker is not supported")),
        _ => Err(CborError::new(
            "Unsupported CBOR simple value or floating-point width",
        )),
    }
}

fn read_length(
    reader: &mut Reader<'_>,
    additional: u8,
    kind: &str,
    limit: usize,
) -> Result<usize, CborError> {
    if additional == 31 {
        return Err(CborError::new(format!(
            "Indefinite-length CBOR {kind}s are not supported"
        )));
    }
    let length = read_argument(reader, additional)?;
    if length > limit as u64 {
        return Err(CborError::new(format!(
            "CBOR {kind} length exceeds configured limit of {limit}"
        )));
    }
    Ok(length as usize)
}

fn read_argument(reader: &mut Reader<'_>, additional: u8) -> Result<u64, CborError> {
    if additional < 24 {
        return Ok(additional as u64);
    }
    match additional {
        24 => Ok(reader.read_byte()? as u64),
        25 => {
            let bytes = reader.read_bytes(2)?;
            Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as u64)
        }
        26 => {
            let bytes = reader.read_bytes(4)?;
            Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64)
        }
        27 => {
            let high = read_argument(reader, 26)?;
            let low = read_argument(reader, 26)?;
            if high > 0x1f_ffff {
                return Err(CborError::new(
                    "Decoded CBOR integer or length is outside the safe range",
                ));
            }
            Ok(high * (MAX_UINT32 + 1) + low)
        }
        31 => Err(CborError::new(
            "Indefinite-length CBOR items are not supported",
        )),
        _ => Err(CborError::new("Malformed CBOR additional information")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(value: Value) -> Value {
        let encoded = encode_cbor(&value, CborOptions::default()).unwrap();
        decode_cbor(&encoded, CborOptions::default()).unwrap()
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(round_trip(json!(null)), json!(null));
        assert_eq!(round_trip(json!(true)), json!(true));
        assert_eq!(round_trip(json!(0)), json!(0));
        assert_eq!(round_trip(json!(23)), json!(23));
        assert_eq!(round_trip(json!(24)), json!(24));
        assert_eq!(round_trip(json!(1_000_000)), json!(1_000_000));
        assert_eq!(round_trip(json!(-1)), json!(-1));
        assert_eq!(round_trip(json!(-100_000)), json!(-100_000));
        assert_eq!(round_trip(json!(1.5)), json!(1.5));
        assert_eq!(round_trip(json!("hello")), json!("hello"));
    }

    #[test]
    fn containers_round_trip() {
        let value = json!({
            "id": "request-1",
            "target": {"serverId": "abc"},
            "call": [1, 2, {"nested": true, "empty": []}]
        });
        assert_eq!(round_trip(value.clone()), value);
    }

    #[test]
    fn integral_float_encodes_as_integer() {
        let encoded = encode_cbor(&json!(8.0), CborOptions::default()).unwrap();
        assert_eq!(encoded, vec![0x08]);
        assert_eq!(
            decode_cbor(&[0xfb, 0x40, 0x20, 0, 0, 0, 0, 0, 0], CborOptions::default()).unwrap(),
            json!(8)
        );
    }

    #[test]
    fn indefinite_and_byte_strings_are_rejected() {
        // Major type 2 (byte string) length 1.
        let error = decode_cbor(&[0x41, 0x00], CborOptions::default()).unwrap_err();
        assert!(error.message().contains("byte strings"));
        // Break marker.
        let error = decode_cbor(&[0xff], CborOptions::default()).unwrap_err();
        assert!(error.message().contains("break marker"));
    }

    #[test]
    fn limits_are_enforced() {
        let options = CborOptions::new(1024, 2, 2);
        let error = encode_cbor(&json!([1, 2, 3]), options).unwrap_err();
        assert!(error.message().contains("array length"));

        let error = encode_cbor(&json!([[[1]]]), options).unwrap_err();
        assert!(error.message().contains("nesting depth"));

        let error = encode_cbor(&json!({"a": 1}), CborOptions::new(2, 10, 10)).unwrap_err();
        assert!(error.message().contains("byte length"));
    }
}
