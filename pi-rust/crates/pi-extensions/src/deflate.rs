//! Pure-Rust DEFLATE (RFC 1951) / zlib (RFC 1950) / gzip (RFC 1952) codec.
//!
//! `node:zlib`'s gzip/deflate family is part of the `pi` extension surface:
//! `packages/coding-agent/test/tool-result-images.test.ts` builds PNG `IDAT`
//! chunks with `deflateSync` + `crc32`, and
//! `packages/coding-agent/examples/extensions/doom-overlay/wad-finder.ts`
//! inflates a downloaded WAD with `gunzipSync`. The workspace bundles `zstd`
//! (used by `pi-session`) but neither `flate2` nor `miniz_oxide`, so this
//! module implements the format directly instead of pulling in a new
//! dependency (the offline registry has no DEFLATE backend).
//!
//! Layout:
//!
//! * [`inflate_raw`] / [`deflate_raw`] — a raw DEFLATE stream (no wrapper).
//! * [`zlib_decompress`] / [`zlib_compress`] — the `Zlib` wrapper used by
//!   `deflateSync` / `inflateSync` (2-byte header + Adler-32 trailer).
//! * [`gzip_decompress`] / [`gzip_compress`] — the `gzip` wrapper used by
//!   `gzipSync` / `gunzipSync` (10-byte header with optional fields +
//!   CRC-32/ISIZE trailer).
//!
//! The decoder handles all three RFC 1951 block types (stored, fixed
//! Huffman, dynamic Huffman) and is checked against fixtures produced by
//! Python's zlib/Node's zlib in `tests/zlib_deflate.rs`. The encoder emits
//! stored blocks at level 0 and fixed-Huffman LZ77 blocks otherwise; it is
//! deliberately simple (greedy hash-chain matching, no dynamic Huffman), so
//! its ratio trails zlib's while its output stays valid for any decoder.

use std::sync::OnceLock;

/// Error codes mirror Node's `zlib` codes so the shim surfaces the same
/// `err.code` strings extensions branch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZlibError {
    pub code: &'static str,
    pub message: String,
}

impl ZlibError {
    /// `Z_DATA_ERROR` — malformed stream, bad checksum, invalid codes.
    fn data(message: impl Into<String>) -> Self {
        Self {
            code: "Z_DATA_ERROR",
            message: message.into(),
        }
    }

    /// `Z_BUF_ERROR` — the input ended before the stream finished.
    fn buf(message: impl Into<String>) -> Self {
        Self {
            code: "Z_BUF_ERROR",
            message: message.into(),
        }
    }

    /// `Z_STREAM_ERROR` — bad arguments / unsupported shape.
    fn stream(message: impl Into<String>) -> Self {
        Self {
            code: "Z_STREAM_ERROR",
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------

/// CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), as `zlib.crc32`
/// and the gzip trailer use it. `value` is the previously returned CRC so a
/// chained call continues the same stream (`crc32(a ++ b) == crc32(b,
/// crc32(a))`).
pub(crate) fn crc32(bytes: &[u8], value: u32) -> u32 {
    let mut crc = !value;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Adler-32 (RFC 1950 §9) — the zlib wrapper's trailer.
fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    // 5552 is the largest chunk whose accumulators cannot overflow 32 bits.
    for chunk in bytes.chunks(5552) {
        for byte in chunk {
            a += u32::from(*byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// Bit reader (LSB-first, RFC 1951 §3.1.1)
// ---------------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit offset from the start of `data`.
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Read `count` (0..=32) bits, LSB-first, as an integer.
    fn bits(&mut self, count: u32) -> Result<u32, ZlibError> {
        let mut value = 0u32;
        for index in 0..count {
            let byte = *self
                .data
                .get(self.bit_pos >> 3)
                .ok_or_else(|| ZlibError::buf("unexpected end of input"))?;
            let bit = (byte >> (self.bit_pos & 7)) & 1;
            value |= u32::from(bit) << index;
            self.bit_pos += 1;
        }
        Ok(value)
    }

    /// Discard bits up to the next byte boundary.
    fn align_to_byte(&mut self) {
        self.bit_pos = (self.bit_pos + 7) & !7;
    }

    fn byte(&mut self) -> Result<u8, ZlibError> {
        self.align_to_byte();
        let byte = *self
            .data
            .get(self.bit_pos >> 3)
            .ok_or_else(|| ZlibError::buf("unexpected end of input"))?;
        self.bit_pos += 8;
        Ok(byte)
    }
}

// ---------------------------------------------------------------------------
// Canonical Huffman decoding (RFC 1951 §3.2.2)
// ---------------------------------------------------------------------------

/// A canonical Huffman table in the counts/symbols form decoder-side
/// implementations use (see `puff.c`'s `huffman`): `counts[len]` is how many
/// symbols have a code of that length, and `symbols` lists the symbols in
/// code order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths. Over-subscribed sets are rejected;
    /// incomplete sets are accepted (RFC 1951 allows them, and the dynamic
    /// distance table is frequently empty).
    fn build(lengths: &[u8]) -> Result<Self, ZlibError> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            if length > 15 {
                return Err(ZlibError::data("invalid code length in Huffman table"));
            }
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;

        let mut left: i32 = 1;
        for count in counts.iter().skip(1) {
            left <<= 1;
            left -= i32::from(*count);
            if left < 0 {
                return Err(ZlibError::data("over-subscribed Huffman code lengths"));
            }
        }

        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[usize::from(offsets[usize::from(length)])] = symbol as u16;
                offsets[usize::from(length)] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    /// Decode one symbol, reading bits one at a time and walking the
    /// canonical code space.
    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, ZlibError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=15 {
            code |= reader.bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - first < count {
                let entry = index + (code - first);
                return self
                    .symbols
                    .get(entry as usize)
                    .copied()
                    .ok_or_else(|| ZlibError::data("invalid Huffman code"));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(ZlibError::data("invalid Huffman code"))
    }
}

/// Length codes 257..=285: base length and extra-bit count.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Distance codes 0..=29: base distance and extra-bit count.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The fixed literal/length + distance tables (RFC 1951 §3.2.6), built once.
fn fixed_tables() -> &'static (Huffman, Huffman) {
    static FIXED: OnceLock<(Huffman, Huffman)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let mut lit_lengths = [0u8; 288];
        lit_lengths[0..144].fill(8);
        lit_lengths[144..256].fill(9);
        lit_lengths[256..280].fill(7);
        lit_lengths[280..288].fill(8);
        let dist_lengths = [5u8; 32];
        (
            Huffman::build(&lit_lengths).expect("fixed literal table is valid"),
            Huffman::build(&dist_lengths).expect("fixed distance table is valid"),
        )
    })
}

// ---------------------------------------------------------------------------
// Inflate
// ---------------------------------------------------------------------------

/// Decode a raw DEFLATE stream (RFC 1951) — no zlib/gzip wrapper.
pub(crate) fn inflate_raw(data: &[u8]) -> Result<Vec<u8>, ZlibError> {
    let mut reader = BitReader::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let final_block = reader.bits(1)? == 1;
        match reader.bits(2)? {
            0 => inflate_stored(&mut reader, &mut out)?,
            1 => {
                let (lit, dist) = fixed_tables();
                inflate_huffman(&mut reader, &mut out, lit, dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut reader)?;
                inflate_huffman(&mut reader, &mut out, &lit, &dist)?;
            }
            _ => return Err(ZlibError::data("invalid DEFLATE block type")),
        }
        if final_block {
            break;
        }
    }
    Ok(out)
}

fn inflate_stored(reader: &mut BitReader<'_>, out: &mut Vec<u8>) -> Result<(), ZlibError> {
    reader.align_to_byte();
    let len = u32::from(reader.byte()?) | (u32::from(reader.byte()?) << 8);
    let nlen = u32::from(reader.byte()?) | (u32::from(reader.byte()?) << 8);
    if len != (!nlen & 0xFFFF) {
        return Err(ZlibError::data("stored block length check failed"));
    }
    for _ in 0..len {
        out.push(reader.byte()?);
    }
    Ok(())
}

fn inflate_huffman(
    reader: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), ZlibError> {
    loop {
        let symbol = lit.decode(reader)?;
        match symbol {
            0..=255 => out.push(symbol as u8),
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length =
                    usize::from(LENGTH_BASE[index]) + reader.bits(LENGTH_EXTRA[index])? as usize;
                let distance_symbol = usize::from(dist.decode(reader)?);
                if distance_symbol >= DIST_BASE.len() {
                    return Err(ZlibError::data("invalid distance code"));
                }
                let distance = usize::from(DIST_BASE[distance_symbol])
                    + reader.bits(DIST_EXTRA[distance_symbol])? as usize;
                if distance == 0 || distance > out.len() {
                    return Err(ZlibError::data("distance too far back"));
                }
                let start = out.len() - distance;
                // Byte-at-a-time so overlapping copies (distance < length)
                // repeat the pattern, exactly like RFC 1951 §3.2.3.
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            _ => return Err(ZlibError::data("invalid literal/length code")),
        }
    }
}

fn dynamic_tables(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman), ZlibError> {
    let literal_count = reader.bits(5)? as usize + 257;
    let distance_count = reader.bits(5)? as usize + 1;
    let code_length_count = reader.bits(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(ZlibError::data("too many dynamic Huffman codes"));
    }

    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut code_lengths = [0u8; 19];
    for &slot in ORDER.iter().take(code_length_count) {
        code_lengths[slot] = reader.bits(3)? as u8;
    }
    let code_length_table = Huffman::build(&code_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths = vec![0u8; total];
    let mut index = 0usize;
    while index < total {
        let symbol = code_length_table.decode(reader)?;
        match symbol {
            0..=15 => {
                lengths[index] = symbol as u8;
                index += 1;
            }
            16 => {
                let previous = if index == 0 { 0 } else { lengths[index - 1] };
                if previous == 0 {
                    return Err(ZlibError::data("repeat code with no previous length"));
                }
                let repeat = 3 + reader.bits(2)? as usize;
                if index + repeat > total {
                    return Err(ZlibError::data("code length repeat overflows table"));
                }
                for _ in 0..repeat {
                    lengths[index] = previous;
                    index += 1;
                }
            }
            17 => {
                let repeat = 3 + reader.bits(3)? as usize;
                if index + repeat > total {
                    return Err(ZlibError::data("code length repeat overflows table"));
                }
                index += repeat;
            }
            18 => {
                let repeat = 11 + reader.bits(7)? as usize;
                if index + repeat > total {
                    return Err(ZlibError::data("code length repeat overflows table"));
                }
                index += repeat;
            }
            _ => return Err(ZlibError::data("invalid code length symbol")),
        }
    }

    let lit = Huffman::build(&lengths[..literal_count])?;
    let dist = Huffman::build(&lengths[literal_count..])?;
    Ok((lit, dist))
}

// ---------------------------------------------------------------------------
// Deflate
// ---------------------------------------------------------------------------

struct BitWriter {
    out: Vec<u8>,
    bit_pos: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            bit_pos: 0,
        }
    }

    /// Write `count` (0..=32) bits, LSB-first.
    fn bits(&mut self, value: u32, count: u32) {
        for index in 0..count {
            if self.bit_pos & 7 == 0 {
                self.out.push(0);
            }
            if (value >> index) & 1 == 1 {
                let last = self.out.len() - 1;
                self.out[last] |= 1 << (self.bit_pos & 7);
            }
            self.bit_pos += 1;
        }
    }

    /// Write a Huffman code. RFC 1951 packs Huffman codes most-significant
    /// bit first, while `bits` writes LSB-first, so the code is reversed.
    fn huffman(&mut self, code: u32, count: u32) {
        let mut reversed = 0u32;
        for index in 0..count {
            reversed |= ((code >> index) & 1) << (count - 1 - index);
        }
        self.bits(reversed, count);
    }

    fn finish(self) -> Vec<u8> {
        self.out
    }
}

/// Fixed Huffman code for a literal/length symbol (RFC 1951 §3.2.6).
fn fixed_symbol_code(symbol: u32) -> (u32, u32) {
    match symbol {
        0..=143 => (0x30 + symbol, 8),
        144..=255 => (0x190 + (symbol - 144), 9),
        256..=279 => (symbol - 256, 7),
        _ => (0xC0 + (symbol - 280), 8),
    }
}

/// Map a match length onto (length symbol, extra value, extra bits).
fn length_code(length: usize) -> (u32, u32, u32) {
    let mut index = LENGTH_BASE.len() - 1;
    while index > 0 && usize::from(LENGTH_BASE[index]) > length {
        index -= 1;
    }
    let extra_bits = LENGTH_EXTRA[index];
    let extra = (length - usize::from(LENGTH_BASE[index])) as u32;
    (257 + index as u32, extra, extra_bits)
}

/// Map a match distance onto (distance symbol, extra value, extra bits).
fn distance_code(distance: usize) -> (u32, u32, u32) {
    let mut index = DIST_BASE.len() - 1;
    while index > 0 && usize::from(DIST_BASE[index]) > distance {
        index -= 1;
    }
    let extra_bits = DIST_EXTRA[index];
    let extra = (distance - usize::from(DIST_BASE[index])) as u32;
    (index as u32, extra, extra_bits)
}

/// Encode a raw DEFLATE stream. `level` follows Node/zlib's convention:
/// `0` selects stored blocks, `1..=9` (and `None`, the default) select
/// fixed-Huffman LZ77 with a match-search depth that scales with the level.
pub(crate) fn deflate_raw(data: &[u8], level: Option<i32>) -> Vec<u8> {
    let level = level.unwrap_or(-1);
    if level == 0 {
        return deflate_stored(data);
    }
    let depth = match level {
        1 => 4,
        2 => 8,
        3 => 12,
        4 => 16,
        5 => 24,
        6 => 64,
        7 => 96,
        8 => 192,
        9 => 384,
        _ => 64, // Node's Z_DEFAULT_COMPRESSION (-1) is zlib level 6.
    };
    deflate_fixed(data, depth)
}

fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::new();
    if data.is_empty() {
        writer.bits(1, 1); // BFINAL
        writer.bits(0, 2); // BTYPE = stored
        writer.bits(0, 16);
        writer.bits(0xFFFF, 16);
        return writer.finish();
    }
    let mut offset = 0usize;
    while offset < data.len() {
        let chunk = (data.len() - offset).min(u16::MAX as usize);
        let final_block = offset + chunk == data.len();
        writer.bits(u32::from(final_block), 1);
        writer.bits(0, 2);
        // `align_to_byte` is implicit: LEN/NLEN are bit-aligned after the
        // 3 header bits, and the writer always starts them on a byte only
        // when the header happened to end there. Emit padding explicitly.
        while writer.bit_pos & 7 != 0 {
            writer.bits(0, 1);
        }
        writer.bits(chunk as u32, 16);
        writer.bits((!chunk as u32) & 0xFFFF, 16);
        for &byte in &data[offset..offset + chunk] {
            writer.bits(u32::from(byte), 8);
        }
        offset += chunk;
    }
    writer.finish()
}

/// Greedy LZ77 with a hash chain over 3-byte prefixes, emitted as one
/// fixed-Huffman block.
fn deflate_fixed(data: &[u8], max_chain: usize) -> Vec<u8> {
    const HASH_SIZE: usize = 1 << 15;
    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 258;
    const MAX_DISTANCE: usize = 32_768;

    let mut writer = BitWriter::new();
    writer.bits(1, 1); // BFINAL
    writer.bits(1, 2); // BTYPE = fixed Huffman

    let hash_at = |index: usize| -> usize {
        let a = u32::from(data[index]);
        let b = u32::from(data[index + 1]);
        let c = u32::from(data[index + 2]);
        ((a << 10) ^ (b << 5) ^ c) as usize & (HASH_SIZE - 1)
    };
    let mut head = vec![usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; data.len().max(1)];

    let mut position = 0usize;
    while position < data.len() {
        let mut best_length = 0usize;
        let mut best_distance = 0usize;
        if position + MIN_MATCH <= data.len() {
            let hash = hash_at(position);
            let mut candidate = head[hash];
            prev[position] = candidate;
            head[hash] = position;
            let mut remaining = max_chain;
            let limit = position.saturating_sub(MAX_DISTANCE);
            while candidate != usize::MAX && candidate >= limit && remaining > 0 {
                remaining -= 1;
                let mut length = 0usize;
                while length < MAX_MATCH
                    && position + length < data.len()
                    && data[candidate + length] == data[position + length]
                {
                    length += 1;
                }
                if length > best_length {
                    best_length = length;
                    best_distance = position - candidate;
                    if best_length == MAX_MATCH {
                        break;
                    }
                }
                candidate = prev[candidate];
            }
        }

        if best_length >= MIN_MATCH {
            let (symbol, extra, extra_bits) = length_code(best_length);
            let (code, code_bits) = fixed_symbol_code(symbol);
            writer.huffman(code, code_bits);
            writer.bits(extra, extra_bits);
            let (distance_symbol, distance_extra, distance_extra_bits) =
                distance_code(best_distance);
            writer.huffman(distance_symbol, 5);
            writer.bits(distance_extra, distance_extra_bits);

            // Feed the skipped positions into the hash chain so later matches
            // can still find them.
            let end = (position + best_length).min(data.len());
            for (index, slot) in prev.iter_mut().enumerate().take(end).skip(position + 1) {
                if index + MIN_MATCH <= data.len() {
                    let hash = hash_at(index);
                    *slot = head[hash];
                    head[hash] = index;
                }
            }
            position = end;
        } else {
            let (code, code_bits) = fixed_symbol_code(u32::from(data[position]));
            writer.huffman(code, code_bits);
            position += 1;
        }
    }

    let (end_code, end_bits) = fixed_symbol_code(256);
    writer.huffman(end_code, end_bits);
    writer.finish()
}

// ---------------------------------------------------------------------------
// zlib wrapper (RFC 1950)
// ---------------------------------------------------------------------------

/// Wrap a raw DEFLATE stream in the zlib container (2-byte header + Adler-32).
pub(crate) fn zlib_compress(data: &[u8], level: Option<i32>) -> Vec<u8> {
    // FLEVEL reflects the requested level so `inflate` implementations that
    // sniff it see the same value zlib would write.
    let flag = match level.unwrap_or(-1) {
        0 | 1 => 0x01, // fastest
        2..=5 => 0x5E,
        9 => 0xDA, // best
        _ => 0x9C, // default
    };
    let mut out = vec![0x78, flag];
    out.extend_from_slice(&deflate_raw(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Parse the zlib container, verifying the header and Adler-32 trailer.
pub(crate) fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, ZlibError> {
    if data.len() < 2 {
        return Err(ZlibError::data("zlib header is truncated"));
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0F != 8 {
        return Err(ZlibError::data("unsupported zlib compression method"));
    }
    if usize::from(cmf >> 4) > 7 {
        return Err(ZlibError::data("zlib window size is too large"));
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(ZlibError::data("invalid zlib header check bits"));
    }
    if flg & 0x20 != 0 {
        return Err(ZlibError::stream(
            "zlib preset dictionaries are not supported",
        ));
    }
    if data.len() < 6 {
        return Err(ZlibError::buf("zlib stream is truncated"));
    }
    let body = &data[2..data.len() - 4];
    let decoded = inflate_raw(body)?;
    let expected = u32::from_be_bytes([
        data[data.len() - 4],
        data[data.len() - 3],
        data[data.len() - 2],
        data[data.len() - 1],
    ]);
    if adler32(&decoded) != expected {
        return Err(ZlibError::data("incorrect zlib data check"));
    }
    Ok(decoded)
}

// ---------------------------------------------------------------------------
// gzip wrapper (RFC 1952)
// ---------------------------------------------------------------------------

/// Wrap a raw DEFLATE stream in the gzip container: a 10-byte header with
/// `FLG = 0` and an unknown OS, then CRC-32 + ISIZE.
pub(crate) fn gzip_compress(data: &[u8], level: Option<i32>) -> Vec<u8> {
    let mut out = vec![
        0x1F, 0x8B, // magic
        8,    // CM = deflate
        0,    // FLG = no optional fields
        0, 0, 0, 0,    // MTIME = 0 (reproducible output)
        0,    // XFL
        0xFF, // OS = unknown
    ];
    out.extend_from_slice(&deflate_raw(data, level));
    out.extend_from_slice(&crc32(data, 0).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// Parse the gzip container (header + deflate + CRC-32/ISIZE trailer).
pub(crate) fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, ZlibError> {
    if data.len() < 18 {
        return Err(ZlibError::data("gzip stream is truncated"));
    }
    if data[0] != 0x1F || data[1] != 0x8B {
        return Err(ZlibError::data("incorrect gzip header"));
    }
    if data[2] != 8 {
        return Err(ZlibError::data("unsupported gzip compression method"));
    }
    let flags = data[3];
    if flags & 0xE0 != 0 {
        return Err(ZlibError::data("reserved gzip header flags are set"));
    }
    let mut cursor = 10usize;
    if flags & 0x04 != 0 {
        // FEXTRA: 2-byte little-endian length followed by that many bytes.
        if data.len() < cursor + 2 {
            return Err(ZlibError::data("gzip extra field is truncated"));
        }
        let length = usize::from(data[cursor]) | (usize::from(data[cursor + 1]) << 8);
        cursor += 2 + length;
        if data.len() < cursor {
            return Err(ZlibError::data("gzip extra field is truncated"));
        }
    }
    for mask in [0x08u8, 0x10] {
        // FNAME / FCOMMENT: NUL-terminated.
        if flags & mask != 0 {
            let rest = &data[cursor..];
            let end = rest
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| ZlibError::data("gzip header string is not terminated"))?;
            cursor += end + 1;
        }
    }
    if flags & 0x02 != 0 {
        // FHCRC: 2-byte header CRC, checked only for presence.
        cursor += 2;
        if data.len() < cursor {
            return Err(ZlibError::data("gzip header CRC is truncated"));
        }
    }
    if data.len() < cursor + 8 {
        return Err(ZlibError::data("gzip trailer is truncated"));
    }
    let decoded = inflate_raw(&data[cursor..data.len() - 8])?;
    let trailer = &data[data.len() - 8..];
    let expected_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let expected_size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if crc32(&decoded, 0) != expected_crc {
        return Err(ZlibError::data("incorrect gzip data check (CRC-32)"));
    }
    if decoded.len() as u32 != expected_size {
        return Err(ZlibError::data("incorrect gzip length check (ISIZE)"));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(payload: &[u8], level: Option<i32>) {
        let compressed = deflate_raw(payload, level);
        let decoded = inflate_raw(&compressed).expect("inflate our own output");
        assert_eq!(decoded, payload, "level {level:?}");
    }

    #[test]
    fn raw_deflate_round_trips_every_level() {
        let payload = b"the quick brown fox jumps over the lazy dog, the quick brown fox";
        for level in [None, Some(0), Some(1), Some(6), Some(9)] {
            round_trip(payload, level);
        }
    }

    #[test]
    fn raw_deflate_round_trips_edge_payloads() {
        round_trip(b"", Some(6));
        round_trip(&[0u8], Some(6));
        round_trip(b"a", Some(0));
        round_trip(&[0u8; 70_000], Some(6));
        let all_bytes: Vec<u8> = (0..=255u16).map(|byte| byte as u8).collect();
        round_trip(&all_bytes, Some(6));
        // Long-distance match: two copies of a long block separated by noise.
        let mut spaced = b"abcdefgh".repeat(64);
        spaced.extend(std::iter::repeat(0xAB).take(40));
        spaced.extend(b"abcdefgh".repeat(64));
        round_trip(&spaced, Some(6));
    }

    #[test]
    fn zlib_and_gzip_wrappers_round_trip() {
        let payload = "pi-rust zlib interop — 世界 1234567890".as_bytes();
        let zlibbed = zlib_compress(payload, None);
        assert_eq!(zlibbed[0], 0x78);
        assert_eq!(zlib_decompress(&zlibbed).expect("zlib decode"), payload);

        let gzipped = gzip_compress(payload, None);
        assert_eq!(&gzipped[..3], &[0x1F, 0x8B, 8]);
        assert_eq!(gzip_decompress(&gzipped).expect("gzip decode"), payload);
    }

    #[test]
    fn corrupt_streams_are_rejected_with_node_codes() {
        let payload = b"checksum me";
        let mut zlibbed = zlib_compress(payload, None);
        // Flip a bit in the middle of the deflate body (not the trailer).
        zlibbed[3] ^= 0x01;
        assert!(zlib_decompress(&zlibbed).is_err());

        let mut gzipped = gzip_compress(payload, None);
        let crc_at = gzipped.len() - 8;
        gzipped[crc_at] ^= 0xFF;
        let error = gzip_decompress(&gzipped).expect_err("crc mismatch");
        assert_eq!(error.code, "Z_DATA_ERROR");

        assert_eq!(
            gzip_decompress(b"not gzip at all, but long enough")
                .expect_err("bad magic")
                .code,
            "Z_DATA_ERROR"
        );
        assert!(zlib_decompress(&zlibbed[..2]).is_err());
    }

    #[test]
    fn crc32_and_adler32_match_the_spec_vectors() {
        assert_eq!(crc32(b"", 0), 0);
        assert_eq!(crc32(b"123456789", 0), 3_421_780_262);
        assert_eq!(crc32(b"456789", crc32(b"123", 0)), 3_421_780_262);
        assert_eq!(adler32(b""), 1);
        // RFC 1950's worked example: "Wikipedia" → 0x11E60398.
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
