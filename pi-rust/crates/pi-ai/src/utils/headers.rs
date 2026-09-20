//! Header records — port of `packages/ai/src/utils/headers.ts`.
//!
//! Upstream has two one-liners with two different jobs:
//! `headersToRecord` flattens a `fetch` `Headers` object into a plain record
//! (the `onResponse` callback and the Codex websocket handshake both consume
//! it), and `providerHeadersToRecord` flattens a `ProviderHeaders` map whose
//! `null` values mean "do not send this header".
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **`HeaderMap`, not `Headers`.** Rust has no web `Headers` type on the
//!   request path; `reqwest::header::HeaderMap` is the equivalent source and
//!   is handled by [`header_map_to_record`] (native only, because `reqwest`
//!   is a native-only dependency of this crate). Like the Fetch `Headers`
//!   spec — which lowercases every name — `HeaderMap` stores lowercase names,
//!   so both ports yield lowercase keys. Repeated names are joined with
//!   `", "`, which is what `Headers` does; non-UTF-8 values are skipped,
//!   because a `String` record cannot carry opaque bytes and a provider
//!   response header is ASCII in practice.
//! * **The generic [`headers_to_record`] accepts any string pair source**, so
//!   a `BTreeMap`/`HashMap` header table (the shape the WASM host hands over)
//!   converts through the same code. Upstream only ever sees `Headers`; key
//!   case is preserved exactly as the source provides it.
//! * **`provider_headers_to_record` returns `Some` only for a non-empty
//!   record**, matching upstream's `Object.keys(result).length > 0` guard, and
//!   takes `Option<&ProviderHeaders>` rather than `ProviderHeaders | undefined`
//!   so the common `None` case costs nothing.

use std::collections::BTreeMap;

use crate::auth::types::ProviderHeaders;

/// `providerHeadersToRecord(headers)` — drop the `None` ("unsendable")
/// entries and return the remaining headers, or `None` when nothing remains.
///
/// `ProviderHeaders` is upstream's `Record<string, string | null>`; the Rust
/// port spells `null` as `None`. Key case is preserved.
pub fn provider_headers_to_record(
    headers: Option<&ProviderHeaders>,
) -> Option<BTreeMap<String, String>> {
    let headers = headers?;
    let mut record = BTreeMap::new();
    for (key, value) in headers {
        if let Some(value) = value {
            record.insert(key.clone(), value.clone());
        }
    }
    if record.is_empty() {
        None
    } else {
        Some(record)
    }
}

/// `headersToRecord(headers)` — collect any string name/value source into a
/// `BTreeMap`, preserving the source's key casing.
///
/// A `reqwest::header::HeaderMap` cannot be passed directly (its values are
/// `HeaderValue`, not `&str`); use [`header_map_to_record`] on native or map
/// it to `(name, value)` pairs first. This form is what a WASM-side header
/// table or a plain `BTreeMap` uses.
pub fn headers_to_record<I, K, V>(headers: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    headers
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect()
}

/// `headersToRecord(response.headers)` for a native `reqwest` response.
///
/// Keys are the lowercased `HeaderName`s; repeated names are joined with
/// `", "` (the Fetch `Headers` behaviour); values that are not valid UTF-8
/// are skipped rather than lossy-decoded.
#[cfg(not(target_arch = "wasm32"))]
pub fn header_map_to_record(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut record = BTreeMap::new();
    for name in headers.keys() {
        let values: Vec<&str> = headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        if !values.is_empty() {
            record.insert(name.as_str().to_string(), values.join(", "));
        }
    }
    record
}
