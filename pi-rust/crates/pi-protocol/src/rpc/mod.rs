//! Server/client RPC wire protocol.
//!
//! Rust port of `packages/protocol`: the strict JSON message vocabulary
//! ([`protocol`]), the length-prefixed frame layer ([`framing`]), the
//! definite-length CBOR codec ([`cbor`]) and the validating message codec
//! ([`codec`]).
//!
//! `pi-server` and (Stage 19, parallel) `pi-client` both build on this
//! module, so the two crates share one wire definition instead of forking it.

pub mod cbor;
pub mod codec;
pub mod framing;
pub mod protocol;

pub use cbor::{
    decode_cbor, encode_cbor, CborError, CborOptions, DEFAULT_MAX_CBOR_BYTE_LENGTH,
    DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH,
};
pub use codec::{
    encode_client_message, encode_server_message, parse_client_message, parse_server_message,
    validate_client_message, validate_server_message, ClientMessageDecoder,
    ProtocolValidationError, ServerMessageDecoder,
};
pub use framing::{
    encode_frame, FrameDecoder, FrameError, DEFAULT_MAX_FRAME_LENGTH, FRAME_HEADER_LENGTH,
};
pub use protocol::{
    is_server_id, is_supported_protocol_version, AttachmentEnvelope, CancelEnvelope, ClientHello,
    ClientMessage, ProtocolError, RequestEnvelope, ResponseEnvelope, RpcTarget, ServerHello,
    ServerHelloError, ServerMessage, ServerTarget, ServiceEventEnvelope, SessionTarget,
    PROTOCOL_VERSION,
};
