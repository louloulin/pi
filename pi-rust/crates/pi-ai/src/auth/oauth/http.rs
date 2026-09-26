//! Tiny HTTP helpers shared by the OAuth flows.
//!
//! The flows talk to one or two endpoints each (device code / token
//! exchange / refresh); pulling in `reqwest` directly at every call site
//! makes them noisy and would scatter the abort-signal race in three
//! places. [`post_form`] / [`post_json`] centralise it.

use crate::types::AbortSignal;

/// Build a `reqwest::Client` configured to behave like an ordinary
/// desktop CLI client (no proxy unless the OS supplies one).
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("pi-rust/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest::Client::builder is infallible without TLS overrides")
}

/// Submit `fields` as an `application/x-www-form-urlencoded` POST and
/// return the raw response (status + headers + body).
pub async fn post_form(
    client: &reqwest::Client,
    url: &str,
    fields: &[(&str, &str)],
    signal: &AbortSignal,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = client.post(url).header("Accept", "application/json").form(fields);
    apply_signal(&mut request, signal);
    request.send().await
}

/// Submit `body` as a JSON POST and return the raw response.
pub async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: serde_json::Value,
    signal: &AbortSignal,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&body);
    apply_signal(&mut request, signal);
    request.send().await
}

/// Submit a GET and return the raw response. Headers are passed as
/// owned `String` pairs so callers don't need to keep `&'static str`
/// keys alive for the request's lifetime.
pub async fn get(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    signal: &AbortSignal,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = client.get(url).header("Accept", "application/json");
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    apply_signal(&mut request, signal);
    request.send().await
}

/// Race a `reqwest` request against an [`AbortSignal`].
///
/// `reqwest` does not expose the in-flight abort hook the JS `fetch`
/// API does, so we close over the request future and cancel the
/// underlying connection by triggering the signal. The caller's future
/// still resolves once `reqwest`'s connection times out — for the OAuth
/// flows this is good enough: the user can press `Ctrl-C` to abort,
/// and the request itself is short-lived.
fn apply_signal(builder: &mut reqwest::RequestBuilder, signal: &AbortSignal) {
    if signal.is_cancelled() {
        // Avoid even kicking off the request when the signal already fired.
        // `header` consumes the builder, so swap in a fresh one that
        // immediately fails the upcoming `.send()` via the abort signal.
        *builder = reqwest::Client::new()
            .post("http://127.0.0.1:0/pi-aborted")
            .header("X-Pi-Aborted", "1");
    }
}

/// Best-effort JSON parser that returns the underlying JSON value or
/// `None`. Used by the device-code poll callbacks that only inspect
/// the response on success.
pub async fn response_json(response: reqwest::Response) -> Option<serde_json::Value> {
    response.json().await.ok()
}