//! OpenAI Codex (ChatGPT Plus/Pro) OAuth flow — port of
//! `packages/ai/src/auth/oauth/openai-codex.ts`.
//!
//! Two sub-flows:
//!
//! * **Browser callback**: PKCE + state CSRF, start a loopback HTTP
//!   server on `127.0.0.1:1455`, open `auth.openai.com/oauth/authorize`,
//!   wait for the redirect, exchange the authorization code for tokens,
//!   and decode the access JWT to extract `chatgpt_account_id`.
//! * **Device code** (headless fallback): ask
//!   `auth.openai.com/api/accounts/deviceauth/usercode` for a user code,
//!   poll until the user authorizes, then exchange the returned
//!   `authorization_code` + `code_verifier` at the OAuth `/token` endpoint.
//!
//! The login prompt is a [`AuthPrompt::Select`] choice between the two.
//! `to_auth` returns `{ apiKey: access }` — the OAuth access token is the
//! ChatGPT API key the streaming adapter sends as a Bearer.

use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;

use super::callback::{CallbackOutcome, LocalCallbackServer};
use super::device_code::{poll_device_code, PollOptions, PollStatus};
use super::http;
use super::pkce::generate_pkce;
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::auth::AuthError;
use crate::providers::registry::OAuthKindSpec;
use crate::types::AbortSignal;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CALLBACK_PATH: &str = "/auth/callback";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEVICE_CODE_TIMEOUT_SECONDS: u64 = 15 * 60;
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

const BROWSER_LOGIN_METHOD: &str = "browser";
const DEVICE_CODE_LOGIN_METHOD: &str = "device_code";

struct Token {
    access: String,
    refresh: String,
    expires: i64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval: f64,
}

struct DeviceTokenSuccess {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenErrorBody {
    #[serde(default)]
    error: Option<DeviceTokenErrorInner>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeviceTokenErrorInner {
    Str(String),
    Object { code: Option<String> },
}

fn decode_jwt(token: &str) -> Option<serde_json::Value> {
    // `token` is a JWS compact serialization: `header.payload.signature`.
    // We only need the payload, so base64url-decode it without verifying
    // the signature — the same assumption upstream makes when extracting
    // `chatgpt_account_id` from the access token.
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if parts.next().is_none() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt(access_token)?;
    let auth = payload.get(JWT_CLAIM_PATH)?;
    let account_id = auth.get("chatgpt_account_id")?.as_str()?;
    if account_id.is_empty() {
        return None;
    }
    Some(account_id.to_string())
}

fn credentials_from_token(token: Token) -> Result<OAuthCredential, AuthError> {
    let account_id = get_account_id(&token.access).ok_or_else(|| {
        AuthError::Store("Failed to extract chatgpt_account_id from access token".to_string())
    })?;
    let mut extra = std::collections::BTreeMap::new();
    extra.insert(
        "accountId".to_string(),
        serde_json::Value::String(account_id),
    );
    Ok(OAuthCredential {
        refresh: token.refresh,
        access: token.access,
        expires: token.expires,
        extra,
    })
}

fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    // URL form: pull the `code` and `state` out of the query string.
    if value.starts_with("http://") || value.starts_with("https://") {
        if let Some(qs_idx) = value.find('?') {
            let query = &value[qs_idx + 1..];
            let mut code = None;
            let mut state = None;
            for pair in query.split('&') {
                let mut iter = pair.split('=');
                if let Some(name) = iter.next() {
                    let value = iter.next().unwrap_or("");
                    if name == "code" {
                        code = Some(percent_decode(value));
                    } else if name == "state" {
                        state = Some(percent_decode(value));
                    }
                }
            }
            return (code, state);
        }
        return (None, None);
    }
    // `code#state` form.
    if value.contains('#') {
        let mut iter = value.splitn(2, '#');
        let code = iter.next().map(str::to_string);
        let state = iter.next().map(str::to_string);
        return (code, state);
    }
    // `key=value&key=value` form.
    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in value.split('&') {
            let mut iter = pair.split('=');
            if let Some(name) = iter.next() {
                let value = iter.next().unwrap_or("");
                if name == "code" {
                    code = Some(percent_decode(value));
                } else if name == "state" {
                    state = Some(percent_decode(value));
                }
            }
        }
        return (code, state);
    }
    // Bare code.
    (Some(value.to_string()), None)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_digit(bytes[i + 1]);
                let lo = hex_digit(bytes[i + 2]);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<Token, AuthError> {
    let client = http::client();
    let response = http::post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ],
        signal,
    )
    .await
    .map_err(|err| AuthError::Store(format!("token exchange failed: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "OpenAI Codex token exchange failed with status {status}: {body}"
        )));
    }
    let parsed: TokenResponse = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid token exchange response: {err}")))?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + parsed.expires_in * 1000;
    Ok(Token {
        access: parsed.access_token,
        refresh: parsed.refresh_token,
        expires,
    })
}

async fn refresh_access_token(
    refresh_token: &str,
    signal: &AbortSignal,
) -> Result<Token, AuthError> {
    let client = http::client();
    let response = http::post_form(
        &client,
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ],
        signal,
    )
    .await
    .map_err(|err| AuthError::Store(format!("OpenAI Codex token refresh error: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "OpenAI Codex token refresh failed with status {status}: {body}"
        )));
    }
    let parsed: TokenResponse = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid refresh response: {err}")))?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + parsed.expires_in * 1000;
    Ok(Token {
        access: parsed.access_token,
        refresh: parsed.refresh_token,
        expires,
    })
}

fn random_state() -> String {
    // 16 random bytes → 32-character hex string; `getrandom` is already
    // in the workspace dep graph via `pi-extensions`.
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS RNG must be available on every supported target");
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    hex
}

fn build_authorize_url(state: &str, challenge: &str) -> String {
    // Manual URL building so we don't pull in the `url` crate; the
    // upstream `/oauth/authorize` is stable and only a handful of
    // params go in.
    let mut url = String::from(AUTHORIZE_URL);
    url.push_str("?response_type=code");
    url.push_str("&client_id=");
    url.push_str(CLIENT_ID);
    url.push_str("&redirect_uri=");
    url.push_str(&percent_encode(REDIRECT_URI));
    url.push_str("&scope=");
    url.push_str(&percent_encode(SCOPE));
    url.push_str("&code_challenge=");
    url.push_str(challenge);
    url.push_str("&code_challenge_method=S256");
    url.push_str("&state=");
    url.push_str(state);
    url.push_str("&id_token_add_organizations=true");
    url.push_str("&codex_cli_simplified_flow=true");
    url.push_str("&originator=pi");
    url
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

async fn login_browser(
    interaction: &dyn AuthInteraction,
) -> Result<OAuthCredential, AuthError> {
    let pkce = generate_pkce();
    let state = random_state();
    let url = build_authorize_url(&state, &pkce.challenge);

    let server = LocalCallbackServer::start("127.0.0.1", CALLBACK_PATH, &state)
        .await
        .map_err(AuthError::Store)?;
    let bound_port = server.bound.port();

    interaction.notify(AuthEvent::AuthUrl {
        url: url.clone(),
        instructions: Some(
            format!(
                "Open the URL in your browser. After signing in, the callback will be handled on port {bound_port}. \
                 Alternatively, paste the redirect URL here:"
            ),
        ),
    });

    let server_task = tokio::spawn(async move { server.wait_for_code().await });

    // Race the callback against a manual paste prompt: whichever resolves
    // first wins, the other is cancelled.
    let manual_signal = AbortSignal::new();
    let manual_prompt_for_outer = AuthPrompt::ManualCode {
        signal: Some(manual_signal.clone()),
        message: "Complete login in your browser, or paste the authorization code / redirect URL here:".to_string(),
        placeholder: Some(REDIRECT_URI.to_string()),
    };
    let manual_prompt_for_paste = manual_prompt_for_outer.clone();

    let code_from_callback: Option<String>;
    let code_from_paste: Option<String>;
    tokio::select! {
        outcome = server_task => {
            match outcome {
                Ok(CallbackOutcome::Code(code)) => {
                    code_from_callback = Some(code);
                    code_from_paste = None;
                }
                _ => {
                    code_from_callback = None;
                    let prompt_result = interaction.prompt(manual_prompt_for_outer).await;
                    code_from_paste = match prompt_result {
                        Ok(input) => {
                            let (code, parsed_state) = parse_authorization_input(&input);
                            if let Some(s) = parsed_state {
                                if s != state {
                                    return Err(AuthError::Store("State mismatch".to_string()));
                                }
                            }
                            code
                        }
                        Err(err) => return Err(err),
                    };
                }
            }
        }
        prompt_result = async {
            interaction.prompt(manual_prompt_for_paste).await
        } => {
            code_from_callback = None;
            code_from_paste = match prompt_result {
                Ok(input) => {
                    let (code, parsed_state) = parse_authorization_input(&input);
                    if let Some(s) = parsed_state {
                        if s != state {
                            return Err(AuthError::Store("State mismatch".to_string()));
                        }
                    }
                    code
                }
                Err(err) => return Err(err),
            };
        }
    }

    let code = code_from_callback
        .or(code_from_paste)
        .ok_or_else(|| AuthError::Store("Missing authorization code".to_string()))?;

    let signal = interaction
        .signal()
        .cloned()
        .unwrap_or_else(AbortSignal::new);
    let token = exchange_authorization_code(&code, &pkce.verifier, REDIRECT_URI, &signal).await?;
    credentials_from_token(token)
}

async fn start_device_auth(signal: &AbortSignal) -> Result<DeviceAuthInfo, AuthError> {
    let client = http::client();
    let response = http::post_json(
        &client,
        DEVICE_USER_CODE_URL,
        serde_json::json!({ "client_id": CLIENT_ID }),
        signal,
    )
    .await
    .map_err(|err| AuthError::Store(format!("device code request failed: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        if status.as_u16() == 404 {
            return Err(AuthError::Store(
                "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.".to_string(),
            ));
        }
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "OpenAI Codex device code request failed with status {status}: {body}"
        )));
    }
    let parsed: DeviceAuthInfo = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid device code response: {err}")))?;
    Ok(parsed)
}

async fn poll_device_auth(
    device: DeviceAuthInfo,
    signal: AbortSignal,
) -> Result<DeviceTokenSuccess, AuthError> {
    let client = http::client();
    let interval_seconds = device.interval.max(0.0) as u64;
    let signal_for_poll = signal.clone();
    let poll: Box<
        dyn FnMut() -> futures::future::BoxFuture<
            'static,
            super::device_code::DeviceCodePollResult<DeviceTokenSuccess>,
        > + Send,
    > = Box::new(move || {
        let client = client.clone();
        let device_auth_id = device.device_auth_id.clone();
        let user_code = device.user_code.clone();
        let signal = signal_for_poll.clone();
        Box::pin(async move {
            let response = match http::post_json(
                &client,
                DEVICE_TOKEN_URL,
                serde_json::json!({
                    "device_auth_id": device_auth_id,
                    "user_code": user_code,
                }),
                &signal,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => {
                    return PollStatus::Failed(format!("device auth poll failed: {err}"));
                }
            };
            let status = response.status();
            if status.is_success() {
                let body: serde_json::Value = match response.json().await {
                    Ok(value) => value,
                    Err(err) => {
                        return PollStatus::Failed(format!(
                            "invalid device auth token response: {err}"
                        ));
                    }
                };
                let authorization_code = body
                    .get("authorization_code")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let code_verifier = body
                    .get("code_verifier")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                match (authorization_code, code_verifier) {
                    (Some(authorization_code), Some(code_verifier)) => PollStatus::Complete(
                        DeviceTokenSuccess {
                            authorization_code,
                            code_verifier,
                        },
                    ),
                    _ => PollStatus::Failed(format!(
                        "Invalid OpenAI Codex device auth token response: {body}"
                    )),
                }
            } else if status.as_u16() == 403 || status.as_u16() == 404 {
                PollStatus::Pending
            } else {
                let body = response.text().await.unwrap_or_default();
                let mut error_code: Option<String> = None;
                if let Ok(json) =
                    serde_json::from_str::<DeviceTokenErrorBody>(&body)
                {
                    if let Some(inner) = json.error {
                        match inner {
                            DeviceTokenErrorInner::Str(s) => error_code = Some(s),
                            DeviceTokenErrorInner::Object { code: Some(s) } => {
                                error_code = Some(s)
                            }
                            _ => {}
                        }
                    }
                }
                match error_code.as_deref() {
                    Some("deviceauth_authorization_pending") => PollStatus::Pending,
                    Some("slow_down") => PollStatus::SlowDown {
                        interval_seconds: None,
                    },
                    _ => PollStatus::Failed(format!(
                        "OpenAI Codex device auth failed with status {status}: {body}"
                    )),
                }
            }
        })
    });
    poll_device_code(PollOptions::<DeviceTokenSuccess> {
        interval_seconds: Some(interval_seconds),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        wait_before_first_poll: false,
        signal: signal.clone(),
        poll,
    })
    .await
    .map_err(AuthError::Store)
}

async fn login_device_code(
    interaction: &dyn AuthInteraction,
) -> Result<OAuthCredential, AuthError> {
    let signal = interaction
        .signal()
        .cloned()
        .unwrap_or_else(AbortSignal::new);
    let device = start_device_auth(&signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: DEVICE_VERIFICATION_URI.to_string(),
        interval_seconds: Some(device.interval.max(0.0) as u64),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
    });
    let token = poll_device_auth(
        DeviceAuthInfo {
            device_auth_id: device.device_auth_id,
            user_code: device.user_code,
            interval: device.interval,
        },
        signal.clone(),
    )
    .await?;
    let token_response = exchange_authorization_code(
        &token.authorization_code,
        &token.code_verifier,
        DEVICE_REDIRECT_URI,
        &signal,
    )
    .await?;
    credentials_from_token(token_response)
}

struct OpenAICodexOAuth;

#[async_trait]
impl OAuthAuth for OpenAICodexOAuth {
    fn name(&self) -> &'static str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&'static str> {
        Some("Sign in with OpenAI")
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        if let Some(signal) = interaction.signal() {
            if signal.is_cancelled() {
                return Err(AuthError::Aborted);
            }
        }
        let method = interaction
            .prompt(AuthPrompt::Select {
                signal: interaction.signal().cloned(),
                message: "Select OpenAI Codex login method:".to_string(),
                options: vec![
                    crate::auth::types::AuthPromptOption {
                        id: BROWSER_LOGIN_METHOD.to_string(),
                        label: "Browser login (default)".to_string(),
                        description: None,
                    },
                    crate::auth::types::AuthPromptOption {
                        id: DEVICE_CODE_LOGIN_METHOD.to_string(),
                        label: "Device code login (headless)".to_string(),
                        description: None,
                    },
                ],
            })
            .await?;
        match method.as_str() {
            DEVICE_CODE_LOGIN_METHOD => login_device_code(interaction).await,
            BROWSER_LOGIN_METHOD => login_browser(interaction).await,
            other => Err(AuthError::Store(format!(
                "Unknown OpenAI Codex login method: {other}"
            ))),
        }
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let token = refresh_access_token(&credential.refresh, signal).await?;
        credentials_from_token(token)
    }

    async fn to_auth(
        &self,
        credential: &OAuthCredential,
    ) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// Build the OAuth implementation for `openai-codex`. The `spec` is
/// currently informational — the OAuth module itself drives the loopback
/// server on port 1455, matching `OAuthKindSpec::Callback { port_range:
/// (1455, 1455) }` in the registry.
pub fn openai_codex_oauth(spec: OAuthKindSpec) -> Arc<dyn OAuthAuth> {
    let _ = spec;
    Arc::new(OpenAICodexOAuth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_jwt_extracts_payload_segment() {
        // header: `{"alg":"none","typ":"JWT"}` → eyJhbGc...
        // payload: `{"sub":"abc","https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}` → ...
        // signature: empty
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(b"{\"alg\":\"none\",\"typ\":\"JWT\"}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}"#);
        let token = format!("{header}.{payload}.");
        assert_eq!(get_account_id(&token).as_deref(), Some("acct_123"));
    }

    #[test]
    fn decode_jwt_returns_none_for_non_jwt() {
        assert!(decode_jwt("not-a-jwt").is_none());
        assert!(decode_jwt("a.b").is_none()); // missing signature segment
    }

    #[test]
    fn parse_authorization_input_handles_url_and_bare_code() {
        let (code, state) = parse_authorization_input(
            "http://localhost:1455/auth/callback?code=abc&state=xyz",
        );
        assert_eq!(code.as_deref(), Some("abc"));
        assert_eq!(state.as_deref(), Some("xyz"));
        let (code, _) = parse_authorization_input("abc#xyz");
        assert_eq!(code.as_deref(), Some("abc"));
        let (code, _) = parse_authorization_input("code=abc&state=xyz");
        assert_eq!(code.as_deref(), Some("abc"));
        let (code, _) = parse_authorization_input("bare-code");
        assert_eq!(code.as_deref(), Some("bare-code"));
    }

    #[test]
    fn credentials_from_token_requires_account_id() {
        let token = Token {
            access: "no-account-id".into(),
            refresh: "r".into(),
            expires: 0,
        };
        assert!(credentials_from_token(token).is_err());
    }

    #[test]
    fn random_state_is_32_hex_chars() {
        let s = random_state();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}