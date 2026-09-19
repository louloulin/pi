//! Length-prefixed frame layer.
//!
//! Rust port of `packages/protocol/src/framing.ts`. Every protocol message is
//! encoded as a big-endian `u32` byte length followed by that many payload
//! bytes. The decoder tolerates arbitrary chunk boundaries, enforces a
//! configured maximum frame length, and refuses indefinitely-large frames
//! before allocating for them.

use std::fmt;

/// Default upper bound for one framed payload (16 MiB).
pub const DEFAULT_MAX_FRAME_LENGTH: usize = 16 * 1024 * 1024;

/// The frame header length in bytes.
pub const FRAME_HEADER_LENGTH: usize = 4;

const MAX_UINT32: usize = 0xffff_ffff;
const PAYLOAD_BLOCK_SIZE: usize = 64 * 1024;

/// A framing failure (bad length, truncation, or a failed decoder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError {
    message: String,
}

impl FrameError {
    /// Creates a frame error with `message`.
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

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FrameError {}

/// Prefixes `payload` with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_UINT32 {
        return Err(FrameError::new(
            "Frame payload exceeds the unsigned 32-bit length limit",
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Open,
    Ended,
    Failed,
}

/// Incrementally splits arbitrary byte chunks into length-prefixed payloads.
#[derive(Debug)]
pub struct FrameDecoder {
    header: [u8; FRAME_HEADER_LENGTH],
    header_length: usize,
    max_frame_length: usize,
    payload: Vec<u8>,
    expected_payload_length: Option<usize>,
    state: DecoderState,
}

impl FrameDecoder {
    /// Creates a decoder with the default maximum frame length.
    pub fn new() -> Self {
        Self::with_max_frame_length(DEFAULT_MAX_FRAME_LENGTH)
    }

    /// Creates a decoder with an explicit maximum frame length.
    ///
    /// A frame length of `0` is allowed (it yields an empty payload); values
    /// above `u32::MAX` are rejected.
    pub fn with_max_frame_length(max_frame_length: usize) -> Self {
        Self {
            header: [0; FRAME_HEADER_LENGTH],
            header_length: 0,
            max_frame_length,
            payload: Vec::new(),
            expected_payload_length: None,
            state: DecoderState::Open,
        }
    }

    /// The configured maximum frame length.
    pub fn max_frame_length(&self) -> usize {
        self.max_frame_length
    }

    /// Pushes a chunk and returns every complete frame it closed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::new("Frame decoder has ended")),
            DecoderState::Failed => return Err(FrameError::new("Frame decoder has failed")),
            DecoderState::Open => {}
        }

        let mut frames = Vec::new();
        let mut offset = 0usize;
        while offset < chunk.len() {
            if self.expected_payload_length.is_none() {
                let header_bytes = (FRAME_HEADER_LENGTH - self.header_length).min(chunk.len() - offset);
                self.header[self.header_length..self.header_length + header_bytes]
                    .copy_from_slice(&chunk[offset..offset + header_bytes]);
                self.header_length += header_bytes;
                offset += header_bytes;
                if self.header_length < FRAME_HEADER_LENGTH {
                    continue;
                }

                let frame_length = u32::from_be_bytes(self.header) as usize;
                self.header_length = 0;
                if frame_length > self.max_frame_length {
                    return Err(self.fail(format!(
                        "Frame length {frame_length} exceeds configured limit of {}",
                        self.max_frame_length
                    )));
                }
                if frame_length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                self.payload = Vec::with_capacity(frame_length.min(PAYLOAD_BLOCK_SIZE));
                self.expected_payload_length = Some(frame_length);
            }

            let expected = self.expected_payload_length.unwrap_or(0);
            let remaining = expected - self.payload.len();
            let take = remaining.min(chunk.len() - offset);
            self.payload.extend_from_slice(&chunk[offset..offset + take]);
            offset += take;
            if self.payload.len() == expected {
                let frame = std::mem::take(&mut self.payload);
                frames.push(frame);
                self.expected_payload_length = None;
            }
        }
        Ok(frames)
    }

    /// Signals end-of-stream, failing if a frame was left half-read.
    pub fn end(&mut self) -> Result<(), FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::new("Frame decoder has ended")),
            DecoderState::Failed => return Err(FrameError::new("Frame decoder has failed")),
            DecoderState::Open => {}
        }
        if self.header_length != 0 || self.expected_payload_length.is_some() {
            return Err(self.fail("Truncated frame at end of stream"));
        }
        self.state = DecoderState::Ended;
        Ok(())
    }

    fn fail(&mut self, message: impl Into<String>) -> FrameError {
        self.state = DecoderState::Failed;
        self.header_length = 0;
        self.payload.clear();
        self.expected_payload_length = None;
        FrameError::new(message)
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_split_arbitrarily() {
        let first = encode_frame(b"hello").unwrap();
        let second = encode_frame(&[0, 1, 2, 3]).unwrap();
        let mut stream = first.clone();
        stream.extend_from_slice(&second);

        let mut decoder = FrameDecoder::new();
        let mut frames = Vec::new();
        for byte in &stream {
            frames.extend(decoder.push(&[*byte]).unwrap());
        }
        decoder.end().unwrap();
        assert_eq!(frames, vec![b"hello".to_vec(), vec![0, 1, 2, 3]]);
    }

    #[test]
    fn empty_payload_is_a_frame() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0, 0, 0, 0, 0, 0, 0, 1, 7]).unwrap(),
            vec![Vec::<u8>::new(), vec![7]]
        );
    }

    #[test]
    fn oversized_frame_fails_before_allocating() {
        let mut decoder = FrameDecoder::with_max_frame_length(4);
        let error = decoder.push(&[0, 0, 0, 5]).unwrap_err();
        assert!(error.message().contains("exceeds configured limit"));
        assert!(decoder.push(&[1]).is_err());
    }

    #[test]
    fn truncated_stream_fails_on_end() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0, 0, 0, 2, 1]).unwrap();
        let error = decoder.end().unwrap_err();
        assert!(error.message().contains("Truncated frame"));
    }
}
