//! Pure-Rust SHA-1 / SHA-256 (FIPS 180-4) for the `node:crypto` bridges.
//!
//! Extensions reach hashing through two upstream surfaces: the Node API
//! (`createHash("sha256")`) and the Web Crypto API
//! (`crypto.subtle.digest("SHA-256", bytes)`). The latter is exercised by
//! `packages/coding-agent/examples/extensions/custom-provider-anthropic/index.ts`,
//! which derives a PKCE `code_challenge` from a SHA-256 digest before it can
//! complete the Anthropic OAuth flow.
//!
//! The workspace bundles neither `sha2` nor `sha1` (the offline registry has
//! no `digest` backend), so both primitives are implemented here, the same
//! way [`crate::deflate`] implements DEFLATE instead of pulling in `flate2`.
//! They are checked against the FIPS 180-4 vectors and against Python's
//! `hashlib` for the padding boundaries.
//!
//! Layout:
//!
//! * [`sha1`] / [`sha256`] — one-shot digests.
//! * [`digest`] — the shim's entry point: name lookup plus digest bytes.
//! * [`Algorithm`] — the supported algorithm set, parsed from a
//!   Node/WebCrypto-style name.

#![forbid(unsafe_code)]

/// A digest algorithm the host can compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// SHA-1 (160-bit). WebCrypto name `SHA-1`, Node name `sha1`.
    Sha1,
    /// SHA-256 (256-bit). WebCrypto name `SHA-256`, Node name `sha256`.
    Sha256,
}

impl Algorithm {
    /// Parse a Node (`sha256`) or WebCrypto (`SHA-256`) algorithm name.
    ///
    /// Matching is case-insensitive and the hyphen is optional, which is how
    /// Node's `createHash` accepts both spellings.
    pub fn parse(name: &str) -> Option<Self> {
        let normalized: String = name
            .trim()
            .chars()
            .filter(|character| *character != '-')
            .map(|character| character.to_ascii_lowercase())
            .collect();
        match normalized.as_str() {
            "sha1" => Some(Algorithm::Sha1),
            "sha256" => Some(Algorithm::Sha256),
            _ => None,
        }
    }
}

/// Digest `data` with `algorithm`.
pub fn digest(algorithm: Algorithm, data: &[u8]) -> Vec<u8> {
    match algorithm {
        Algorithm::Sha1 => sha1(data).to_vec(),
        Algorithm::Sha256 => sha256(data).to_vec(),
    }
}

/// SHA-256 of `data` (FIPS 180-4 §6.2).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    for block in padded_blocks(data) {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h].into_iter()) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// SHA-1 of `data` (FIPS 180-4 §6.1).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

    for block in padded_blocks(data) {
        let mut w = [0u32; 80];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        for (slot, value) in state.iter_mut().zip([a, b, c, d, e].into_iter()) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 20];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Split `data` into the 64-byte blocks both algorithms consume, applying the
/// shared Merkle–Damgård padding: `0x80`, zero fill, then the bit length as a
/// big-endian `u64` in the final eight bytes.
fn padded_blocks(data: &[u8]) -> Vec<[u8; 64]> {
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    // The bit length wraps for inputs above 2^61 bytes; no extension can
    // reach that, and Node's own digest has the same 64-bit limit.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    padded.extend_from_slice(&bit_len.to_be_bytes());

    padded
        .chunks_exact(64)
        .map(|chunk| {
            let mut block = [0u8; 64];
            block.copy_from_slice(chunk);
            block
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    #[test]
    fn algorithm_names_match_node_and_webcrypto_spellings() {
        assert_eq!(Algorithm::parse("sha256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::parse("SHA-256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::parse("Sha-256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::parse("sha1"), Some(Algorithm::Sha1));
        assert_eq!(Algorithm::parse("SHA-1"), Some(Algorithm::Sha1));
        assert_eq!(Algorithm::parse("md5"), None);
        assert_eq!(Algorithm::parse("sha512"), None);
        assert_eq!(Algorithm::parse(""), None);
    }

    #[test]
    fn sha256_matches_fips_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_handles_padding_boundaries() {
        // 55 bytes = the last length that fits before the padding forces an
        // extra block; 56 and 64 flip it. Values from Python
        // `hashlib.sha256(b"a" * n).hexdigest()`.
        assert_eq!(
            hex(&sha256(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 1000])),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn sha1_matches_fips_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha1_handles_padding_boundaries() {
        assert_eq!(
            hex(&sha1(&[b'a'; 56])),
            "c2db330f6083854c99d4b5bfb6e8f29f201be699"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 64])),
            "0098ba824b5c16427bd7a1122a5a442a25ec644d"
        );
    }

    #[test]
    fn digest_dispatches_on_the_parsed_algorithm() {
        assert_eq!(
            hex(&digest(Algorithm::Sha256, b"abc")).len(),
            64,
            "sha256 hex length"
        );
        assert_eq!(hex(&digest(Algorithm::Sha1, b"abc")).len(), 40);
        assert_eq!(digest(Algorithm::Sha256, b"abc"), sha256(b"abc").to_vec());
    }
}
