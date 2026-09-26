//! Kimi Code (subscription) OAuth flow — port of
//! `packages/ai/src/auth/oauth/kimi-coding.ts`.
//!
//! RFC 8628 device authorization grant against `https://auth.kimi.com`
//! with JSON responses. The access token authenticates requests to
//! `https://api.kimi.com/coding` as an `Authorization: Bearer` header,
//! so [`to_auth`](crate::auth::OAuthAuth::to_auth) returns headers
//! instead of an api key — the streaming adapter picks the header form.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::device_code::{poll_device_code, PollOptions, PollStatus};
use super::http;
use crate::auth::types::{
    AuthEvent, AuthInteraction, ModelAuth, OAuthAuth, OAuthCredential, ProviderHeaders,
};
use crate::auth::AuthError;
use crate::providers::registry::OAuthKindSpec;
use crate::types::AbortSignal;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const DEVICE_CODE_TIMEOUT_SECONDS: u64 = 15 * 60;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 5;
const REFRESH_MAX_RETRIES: u32 = 3;

struct Token {
    access: String,
    refresh: String,
    expires: i64,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    #[serde(default)]
    interval: Option<f64>,
    #[serde(default)]
    expires_in: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: f64,
}

fn trusted_http_url(value: &str) -> Option<String> {
    if !value.starts_with("http://") && !value.starts_with("https://") {
        return None;
    }
    Some(value.to_string())
}

fn oauth_host() -> String {
    // Upstream reads `KIMI_CODE_OAUTH_HOST` / `KIMI_OAUTH_HOST` env vars
    // via `getProviderEnvValue`. The Rust AuthContext surface would let
    // the caller inject these, but the OAuth flow doesn't carry an
    // AuthContext today — default to the production host. A future
    // extension (or the `/login` UI) can surface this.
    DEFAULT_OAUTH_HOST.to_string()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn start_device_authorization(
    host: &str,
    signal: &AbortSignal,
) -> Result<DeviceAuthorization, AuthError> {
    let client = http::client();
    let url = format!("{host}/api/oauth/device_authorization");
    let response = http::post_form(
        &client,
        &url,
        &[("client_id", CLIENT_ID)],
        signal,
    )
    .await
    .map_err(|err| AuthError::Store(format!("device authorization failed: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "Kimi Code device authorization failed with status {status}: {body}"
        )));
    }
    let parsed: DeviceAuthorization = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid device authorization response: {err}")))?;
    if trusted_http_url(&parsed.verification_uri).is_none()
        || trusted_http_url(&parsed.verification_uri_complete).is_none()
    {
        return Err(AuthError::Store(format!(
            "Invalid Kimi Code device authorization response: verification_uri(s) untrusted"
        )));
    }
    Ok(parsed)
}

fn parse_token_response(raw: serde_json::Value) -> Result<Token, AuthError> {
    let access_token = raw
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let refresh_token = raw
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let expires_in = raw.get("expires_in").and_then(|v| v.as_f64());
    match (access_token, refresh_token, expires_in) {
        (Some(access), Some(refresh), Some(expires)) if !access.is_empty() && !refresh.is_empty() && expires > 0.0 => {
            Ok(Token {
                access,
                refresh,
                expires: now_millis() + (expires * 1000.0) as i64,
            })
        }
        _ => Err(AuthError::Store(format!(
            "Kimi Code token response missing fields: {raw}"
        ))),
    }
}

async fn poll_for_token(
    host: String,
    device: DeviceAuthorization,
    signal: AbortSignal,
) -> Result<Token, AuthError> {
    let client = http::client();
    let interval_seconds = device
        .interval
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v as u64)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
    let expires_in_seconds = device
        .expires_in
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v as u64)
        .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS);
    let signal_for_poll = signal.clone();
    let poll: Box<
        dyn FnMut() -> futures::future::BoxFuture<'static, super::device_code::DeviceCodePollResult<Token>>
            + Send,
    > = Box::new(move || {
        let client = client.clone();
        let host = host.clone();
        let device_code = device.device_code.clone();
        let signal = signal_for_poll.clone();
        Box::pin(async move {
            let url = format!("{host}/api/oauth/token");
            let response = match http::post_form(
                &client,
                &url,
                &[
                    ("client_id", CLIENT_ID),
                    ("device_code", &device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ],
                &signal,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => {
                    return PollStatus::Failed(format!("device token request failed: {err}"));
                }
            };
            let status = response.status();
            if status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                return PollStatus::Failed(format!(
                    "Kimi Code device token request failed with status {status}: {body}"
                ));
            }
            let raw: serde_json::Value = match response.json().await {
                Ok(value) => value,
                Err(err) => {
                    return PollStatus::Failed(format!("invalid poll response: {err}"));
                }
            };
            if status.is_success() {
                if let Some(access) = raw.get("access_token").and_then(|v| v.as_str()) {
                    if !access.is_empty() {
                        return match parse_token_response(raw) {
                            Ok(token) => PollStatus::Complete(token),
                            Err(err) => PollStatus::Failed(err.to_string()),
                        };
                    }
                }
                return PollStatus::Failed("Kimi Code poll response missing access_token".into());
            }
            let error = raw.get("error").and_then(|v| v.as_str()).unwrap_or("");
            match error {
                "authorization_pending" => PollStatus::Pending,
                "slow_down" => {
                    let interval = raw
                        .get("interval")
                        .and_then(|v| v.as_f64())
                        .filter(|v| *v > 0.0)
                        .map(|v| v as u64);
                    PollStatus::SlowDown {
                        interval_seconds: interval,
                    }
                }
                "expired_token" => PollStatus::Failed(
                    "Kimi Code device authorization expired. Please restart login.".into(),
                ),
                "access_denied" => {
                    PollStatus::Failed("Kimi Code login was denied.".into())
                }
                _ => PollStatus::Failed(format!(
                    "Kimi Code device token request failed (status {status}): {raw}"
                )),
            }
        })
    });
    poll_device_code(PollOptions::<Token> {
        interval_seconds: Some(interval_seconds),
        expires_in_seconds: Some(expires_in_seconds),
        wait_before_first_poll: true,
        signal: signal.clone(),
        poll,
    })
    .await
    .map_err(AuthError::Store)
}

async fn refresh_token(host: &str, refresh: &str, signal: &AbortSignal) -> Result<Token, AuthError> {
    let client = http::client();
    let url = format!("{host}/api/oauth/token");
    let mut last_error: Option<AuthError> = None;
    for attempt in 0..=REFRESH_MAX_RETRIES {
        if attempt > 0 {
            let backoff = std::time::Duration::from_millis(1000u64 << (attempt - 1));
            tokio::time::sleep(backoff).await;
            if signal.is_cancelled() {
                return Err(AuthError::Aborted);
            }
        }
        let response = match http::post_form(
            &client,
            &url,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ],
            signal,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => {
                last_error = Some(AuthError::Store(format!("Kimi Code token refresh error: {err}")));
                continue;
            }
        };
        let status = response.status();
        let raw: serde_json::Value = match response.json().await {
            Ok(value) => value,
            Err(err) => {
                last_error = Some(AuthError::Store(format!(
                    "invalid Kimi Code refresh response: {err}"
                )));
                continue;
            }
        };
        if status.is_success() {
            return parse_token_response(raw);
        }
        // 401 / 403 / invalid_grant — the stored credential is dead.
        if status.as_u16() == 401
            || status.as_u16() == 403
            || raw
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s == "invalid_grant")
                .unwrap_or(false)
        {
            return Err(AuthError::Store(format!(
                "Kimi Code token refresh unauthorized (status {status}): {raw}"
            )));
        }
        if (status.as_u16() == 429 || status.is_server_error()) && attempt < REFRESH_MAX_RETRIES {
            last_error = Some(AuthError::Store(format!(
                "Kimi Code token refresh failed with status {status}"
            )));
            continue;
        }
        return Err(AuthError::Store(format!(
            "Kimi Code token refresh failed with status {status}: {raw}"
        )));
    }
    Err(last_error.unwrap_or_else(|| AuthError::Store(
        "Kimi Code token refresh failed".to_string(),
    )))
}

struct KimiCodingOAuth;

#[async_trait]
impl OAuthAuth for KimiCodingOAuth {
    fn name(&self) -> &'static str {
        "Kimi Code (subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&'static str> {
        Some("Sign in with Kimi Code")
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
        let host = oauth_host();
        let signal = interaction
            .signal()
            .cloned()
            .unwrap_or_else(AbortSignal::new);
        let device = start_device_authorization(&host, &signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri_complete.clone(),
            interval_seconds: Some(
                device
                    .interval
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(|v| v as u64)
                    .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
            ),
            expires_in_seconds: Some(
                device
                    .expires_in
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(|v| v as u64)
                    .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS),
            ),
        });
        let token = poll_for_token(host.clone(), device, signal.clone()).await?;
        Ok(OAuthCredential {
            refresh: token.refresh,
            access: token.access,
            expires: token.expires,
            extra: std::collections::BTreeMap::new(),
        })
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let host = oauth_host();
        let token = refresh_token(&host, &credential.refresh, signal).await?;
        Ok(OAuthCredential {
            refresh: token.refresh,
            access: token.access,
            expires: token.expires,
            extra: credential.extra.clone(),
        })
    }

    async fn to_auth(
        &self,
        credential: &OAuthCredential,
    ) -> Result<ModelAuth, AuthError> {
        let mut headers = ProviderHeaders::new();
        headers.insert(
            "Authorization".to_string(),
            Some(format!("Bearer {}", credential.access)),
        );
        Ok(ModelAuth {
            api_key: None,
            headers: Some(headers),
            base_url: None,
        })
    }
}

/// Build the OAuth implementation for `kimi-coding`. The `spec` is
/// currently informational; the flow is always a device-code grant.
pub fn kimi_coding_oauth(spec: OAuthKindSpec) -> Arc<dyn OAuthAuth> {
    let _ = spec;
    Arc::new(KimiCodingOAuth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_token_response_valid_payload() {
        let value = serde_json::json!({
            "access_token": "a",
            "refresh_token": "r",
            "expires_in": 3600,
        });
        let token = parse_token_response(value).unwrap();
        assert_eq!(token.access, "a");
        assert_eq!(token.refresh, "r");
        assert!(token.expires > now_millis());
    }

    #[test]
    fn parse_token_response_missing_fields_fails() {
        let value = serde_json::json!({
            "access_token": "a",
        });
        assert!(parse_token_response(value).is_err());
    }

    #[test]
    fn parse_token_response_zero_expires_in_fails() {
        let value = serde_json::json!({
            "access_token": "a",
            "refresh_token": "r",
            "expires_in": 0,
        });
        assert!(parse_token_response(value).is_err());
    }

    #[test]
    fn trusted_http_url_rejects_non_http_schemes() {
        assert!(trusted_http_url("file:///etc/passwd").is_none());
        assert!(trusted_http_url("javascript:alert(1)").is_none());
        assert!(trusted_http_url("https://auth.kimi.com").is_some());
        assert!(trusted_http_url("http://localhost:8080/path").is_some());
    }
}