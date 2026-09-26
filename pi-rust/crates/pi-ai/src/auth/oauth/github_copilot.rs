//! GitHub Copilot OAuth flow — port of
//! `packages/ai/src/auth/oauth/github-copilot.ts`.
//!
//! Two-stage flow:
//!
//! 1. **Device-code grant** against `https://github.com/login/device/code`
//!    (or the GitHub Enterprise equivalent) for the OAuth access/refresh
//!    pair.
//! 2. **Copilot token exchange** at
//!    `https://api.<domain>/copilot_internal/v2/token` to mint a per-session
//!    Copilot token whose `proxy-ep=` claim selects the request-time base
//!    URL.
//!
//! The Copilot token lives in `credential.access` and the derived base URL
//! in [`get_base_url`]. Refresh always re-runs step 2 — the OAuth refresh
//! token has a much longer lifetime than the Copilot proxy token — so a
//! successful refresh always rotates `access`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::device_code::{poll_device_code, PollOptions, PollStatus};
use super::http;
use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::auth::AuthError;
use crate::providers::registry::OAuthKindSpec;
use crate::types::AbortSignal;

/// `SXYxLmI1MDdhMDhjODdlY2ZlOTg=` decoded — the GitHub Copilot Chat VSCode
/// public client id.
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// Copilot-issued header prefixes shared between the token refresh and
/// the model catalog request so both look like the same upstream caller.
const COPILOT_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];

const COPILOT_API_VERSION: &str = "2026-06-01";

/// Public GitHub domain used when no enterprise host is supplied.
const DEFAULT_DOMAIN: &str = "github.com";

/// Endpoints for `domain` (or the public `github.com`).
fn urls_for(domain: &str) -> Endpoints {
    Endpoints {
        device_code_url: format!("https://{domain}/login/device/code"),
        access_token_url: format!("https://{domain}/login/oauth/access_token"),
        copilot_token_url: format!("https://api.{domain}/copilot_internal/v2/token"),
    }
}

struct Endpoints {
    device_code_url: String,
    access_token_url: String,
    copilot_token_url: String,
}

fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip an optional scheme prefix so `company.ghe.com`,
    // `https://company.ghe.com/`, and `https://company.ghe.com:8443`
    // all collapse to the same hostname. We do not pull in the `url`
    // crate just for this; a manual split is enough.
    let host = if let Some(idx) = trimmed.find("://") {
        &trimmed[idx + 3..]
    } else {
        trimmed
    };
    let host = host.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(host.to_string())
}

fn enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    let raw = credential.extra.get("enterpriseUrl")?;
    let value = raw.as_str()?;
    normalize_domain(value)
}

fn get_base_url_from_token(token: &str) -> Option<String> {
    // Copilot tokens carry `proxy-ep=...` as a `;`-separated field; convert
    // the `proxy.` host to `api.` and trust the well-known Copilot proxy
    // path layout. Falls back to `None` when the field is absent.
    let value = token.split(';').find_map(|part| {
        let mut iter = part.splitn(2, '=');
        match (iter.next(), iter.next()) {
            (Some(key), Some(v)) if key == "proxy-ep" => Some(v.to_string()),
            _ => None,
        }
    })?;
    let api_host = if let Some(stripped) = value.strip_prefix("proxy.") {
        format!("api.{stripped}")
    } else {
        value
    };
    Some(format!("https://{api_host}"))
}

fn get_base_url(token: Option<&str>, enterprise: Option<&str>) -> String {
    if let Some(token) = token {
        if let Some(url) = get_base_url_from_token(token) {
            return url;
        }
    }
    if let Some(domain) = enterprise {
        return format!("https://copilot-api.{domain}");
    }
    "https://api.individual.githubcopilot.com".to_string()
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    interval: Option<u64>,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenSuccess {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: i64,
}

struct LoginState {
    domain: String,
    enterprise: Option<String>,
    signal: AbortSignal,
}

async fn start_device_flow(state: &LoginState) -> Result<DeviceCodeResponse, AuthError> {
    let urls = urls_for(&state.domain);
    let client = http::client();
    let response = http::post_form(
        &client,
        &urls.device_code_url,
        &[("client_id", CLIENT_ID), ("scope", "read:user")],
        &state.signal,
    )
    .await
    .map_err(|err| AuthError::Store(format!("device code request failed: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "GitHub Copilot device code request failed with status {status}: {body}"
        )));
    }
    let device: DeviceCodeResponse = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid device code response: {err}")))?;
    // The verification URI is opened in the user's browser; reject
    // anything that is not http(s) so a malicious server cannot have
    // us open a `file://` or similar URL.
    if !(device.verification_uri.starts_with("https://")
        || device.verification_uri.starts_with("http://"))
    {
        return Err(AuthError::Store(
            "untrusted verification_uri in device code response".into(),
        ));
    }
    Ok(device)
}

async fn poll_for_access_token(
    state: &LoginState,
    device: DeviceCodeResponse,
) -> Result<String, AuthError> {
    let urls = urls_for(&state.domain);
    let client = http::client();
    let device_code = device.device_code.clone();
    let signal = state.signal.clone();
    let access_token_url = urls.access_token_url.clone();
    let poll: Box<
        dyn FnMut() -> futures::future::BoxFuture<
            'static,
            super::device_code::DeviceCodePollResult<String>,
        > + Send,
    > = Box::new(move || {
        let client = client.clone();
        let url = access_token_url.clone();
        let device_code = device_code.clone();
        let signal = signal.clone();
        Box::pin(async move {
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
                    return PollStatus::Failed(format!("poll request failed: {err}"));
                }
            };
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return PollStatus::Failed(format!(
                    "GitHub Copilot poll failed with status {status}: {body}"
                ));
            }
            let raw: serde_json::Value = match response.json().await {
                Ok(value) => value,
                Err(err) => return PollStatus::Failed(format!("invalid poll response: {err}")),
            };
            if let Ok(success) = serde_json::from_value::<DeviceTokenSuccess>(raw.clone()) {
                return PollStatus::Complete(success.access_token);
            }
            match serde_json::from_value::<DeviceTokenError>(raw) {
                Ok(error) if error.error == "authorization_pending" => PollStatus::Pending,
                Ok(error) if error.error == "slow_down" => PollStatus::SlowDown {
                    interval_seconds: error.interval,
                },
                Ok(error) => PollStatus::Failed(format!(
                    "Device flow failed: {}{}",
                    error.error,
                    error
                        .error_description
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default()
                )),
                Err(err) => PollStatus::Failed(format!("invalid poll payload: {err}")),
            }
        })
    });
    poll_device_code(PollOptions::<String> {
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
        wait_before_first_poll: true,
        signal: state.signal.clone(),
        poll,
    })
    .await
    .map_err(AuthError::Store)
}

async fn refresh_copilot_token(
    state: &LoginState,
    refresh_token: &str,
) -> Result<OAuthCredential, AuthError> {
    let urls = urls_for(&state.domain);
    let client = http::client();
    let mut headers: Vec<(String, String)> = vec![("Accept".to_string(), "application/json".to_string())];
    headers.push(("Authorization".to_string(), format!("Bearer {refresh_token}")));
    for (name, value) in COPILOT_HEADERS {
        headers.push((name.to_string(), value.to_string()));
    }
    let response = http::get(&client, &urls.copilot_token_url, &headers, &state.signal)
        .await
        .map_err(|err| AuthError::Store(format!("Copilot token request failed: {err}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Store(format!(
            "Copilot token request failed with status {status}: {body}"
        )));
    }
    let parsed: CopilotTokenResponse = response
        .json()
        .await
        .map_err(|err| AuthError::Store(format!("invalid Copilot token response: {err}")))?;
    let mut extra = std::collections::BTreeMap::new();
    if let Some(domain) = &state.enterprise {
        extra.insert(
            "enterpriseUrl".to_string(),
            serde_json::Value::String(domain.clone()),
        );
    }
    Ok(OAuthCredential {
        refresh: refresh_token.to_string(),
        access: parsed.token,
        // Copilot returns seconds; subtract 5 minutes of slack.
        expires: parsed.expires_at * 1000 - 5 * 60 * 1000,
        extra,
    })
}

struct GitHubCopilotOAuth {
    interval_seconds: u64,
}

impl GitHubCopilotOAuth {
    fn new(interval_seconds: u64) -> Self {
        Self { interval_seconds }
    }
}

#[async_trait]
impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&'static str> {
        Some("Sign in with GitHub")
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
        let raw_input = interaction
            .prompt(AuthPrompt::Text {
                signal: interaction.signal().cloned(),
                message: "GitHub Enterprise URL/domain (blank for github.com)".to_string(),
                placeholder: Some("company.ghe.com".to_string()),
            })
            .await?;
        if let Some(signal) = interaction.signal() {
            if signal.is_cancelled() {
                return Err(AuthError::Aborted);
            }
        }
        let trimmed = raw_input.trim();
        let enterprise = if trimmed.is_empty() {
            None
        } else {
            Some(normalize_domain(trimmed).ok_or_else(|| {
                AuthError::Store("Invalid GitHub Enterprise URL/domain".to_string())
            })?)
        };
        let domain = enterprise.clone().unwrap_or_else(|| DEFAULT_DOMAIN.to_string());
        let signal = interaction
            .signal()
            .cloned()
            .unwrap_or_else(AbortSignal::new);
        let state = LoginState {
            domain,
            enterprise: enterprise.clone(),
            signal,
        };
        let device = start_device_flow(&state).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: Some(device.interval.unwrap_or(self.interval_seconds)),
            expires_in_seconds: Some(device.expires_in),
        });
        let access_token = poll_for_access_token(&state, device).await?;
        refresh_copilot_token(&state, &access_token).await
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let domain = enterprise_domain(credential).unwrap_or_else(|| DEFAULT_DOMAIN.to_string());
        let state = LoginState {
            domain,
            enterprise: enterprise_domain(credential),
            signal: signal.clone(),
        };
        refresh_copilot_token(&state, &credential.refresh).await
    }

    async fn to_auth(
        &self,
        credential: &OAuthCredential,
    ) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            base_url: Some(get_base_url(
                Some(&credential.access),
                enterprise_domain(credential).as_deref(),
            )),
            ..ModelAuth::default()
        })
    }
}

/// Build the OAuth implementation for `github-copilot`. The `interval_seconds`
/// is the initial device-code poll cadence surfaced to the UI.
pub fn github_copilot_oauth(spec: OAuthKindSpec) -> Arc<dyn OAuthAuth> {
    let interval: u64 = match spec {
        OAuthKindSpec::DeviceCode { interval_ms } => u64::from(interval_ms) / 1000,
        _ => 5,
    };
    let interval = interval.max(1);
    Arc::new(GitHubCopilotOAuth::new(interval))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    #[test]
    fn decode_client_id_matches_upstream() {
        let decoded = String::from_utf8(BASE64.decode(b"SXYxLmI1MDdhMDhjODdlY2ZlOTg=").unwrap())
            .unwrap();
        assert_eq!(decoded, CLIENT_ID);
    }

    #[test]
    fn normalize_domain_handles_plain_and_schemed_inputs() {
        assert_eq!(
            normalize_domain("company.ghe.com").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(
            normalize_domain("https://company.ghe.com").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(
            normalize_domain("https://company.ghe.com/path").as_deref(),
            Some("company.ghe.com")
        );
        assert!(normalize_domain("not a url").is_none());
        assert!(normalize_domain("").is_none());
    }

    #[test]
    fn base_url_from_proxy_ep() {
        let token = "tid=abc;exp=123;proxy-ep=proxy.individual.githubcopilot.com;sku=foo";
        assert_eq!(
            get_base_url_from_token(token).as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
        assert!(get_base_url_from_token("no field").is_none());
    }

    #[test]
    fn get_base_url_falls_back_through_layers() {
        assert_eq!(
            get_base_url(None, None),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            get_base_url(None, Some("example.com")),
            "https://copilot-api.example.com"
        );
        assert_eq!(
            get_base_url(Some("tid=abc;proxy-ep=proxy.enterprise.githubcopilot.com;exp=1"), None),
            "https://api.enterprise.githubcopilot.com"
        );
    }

    #[test]
    fn enterprise_domain_round_trips() {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert(
            "enterpriseUrl".to_string(),
            serde_json::Value::String("company.ghe.com".to_string()),
        );
        let credential = OAuthCredential {
            refresh: "r".into(),
            access: "a".into(),
            expires: 0,
            extra,
        };
        assert_eq!(
            enterprise_domain(&credential).as_deref(),
            Some("company.ghe.com")
        );
    }
}