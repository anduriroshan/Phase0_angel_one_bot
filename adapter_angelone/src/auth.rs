//! Angel One SmartAPI authentication.
//!
//! This is the canonical auth implementation for the Angel One adapter.
//! The `ingestion` crate uses its own copy today; in a future refactor
//! it should depend on this module instead.
//!
//! See: `domain/exchange_protocols.md` (REST auth section)

use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::info;

/// Tokens returned by a successful Angel One login.
#[derive(Debug, Clone)]
pub struct AuthTokens {
    pub jwt_token: String,
    pub feed_token: String,
    pub refresh_token: String,
    pub api_key: String,
    pub client_id: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponseData {
    #[serde(rename = "jwtToken")]
    jwt_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    #[serde(rename = "feedToken")]
    feed_token: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    status: bool,
    message: String,
    data: Option<LoginResponseData>,
}

const LOGIN_URL: &str =
    "https://apiconnect.angelone.in/rest/auth/angelbroking/user/v1/loginByPassword";

/// Authenticate with the Angel One SmartAPI and return session tokens.
///
/// Reads the following environment variables:
/// - `ANGEL_CLIENT_ID`
/// - `ANGEL_PIN`
/// - `ANGEL_API_KEY`
/// - `ANGEL_TOTP_SECRET`
pub async fn authenticate() -> anyhow::Result<AuthTokens> {
    let client_id = std::env::var("ANGEL_CLIENT_ID")
        .map_err(|_| anyhow::anyhow!("ANGEL_CLIENT_ID not set"))?;
    let pin = std::env::var("ANGEL_PIN")
        .map_err(|_| anyhow::anyhow!("ANGEL_PIN not set"))?;
    let api_key = std::env::var("ANGEL_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANGEL_API_KEY not set"))?;
    let totp_secret_str = std::env::var("ANGEL_TOTP_SECRET")
        .map_err(|_| anyhow::anyhow!("ANGEL_TOTP_SECRET not set"))?;

    let secret_bytes = Secret::Encoded(totp_secret_str)
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("Failed to decode TOTP secret: {e}"))?;

    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to create TOTP: {e}"))?;

    let totp_code = totp
        .generate_current()
        .map_err(|e| anyhow::anyhow!("Failed to generate TOTP code: {e}"))?;

    info!("Authenticating with Angel One SmartAPI...");

    let client = Client::new();
    let payload = json!({
        "clientcode": client_id,
        "password": pin,
        "totp": totp_code,
    });

    let res = client
        .post(LOGIN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("X-UserType", "USER")
        .header("X-SourceID", "WEB")
        .header("X-ClientLocalIP", "127.0.0.1")
        .header("X-ClientPublicIP", "127.0.0.1")
        .header("X-MACAddress", "00-00-00-00-00-00")
        .header("X-PrivateKey", &api_key)
        .json(&payload)
        .send()
        .await?;

    let status = res.status();
    let body = res.text().await?;

    if !status.is_success() {
        anyhow::bail!("Angel One login HTTP {status}: {body}");
    }

    let parsed: LoginResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("Failed to parse login response: {e}\nBody: {body}"))?;

    if !parsed.status {
        anyhow::bail!("Angel One login rejected: {}", parsed.message);
    }

    let data = parsed
        .data
        .ok_or_else(|| anyhow::anyhow!("Login succeeded but data field is null"))?;

    info!("Angel One authentication successful");

    Ok(AuthTokens {
        jwt_token: data.jwt_token,
        feed_token: data.feed_token,
        refresh_token: data.refresh_token,
        api_key,
        client_id,
    })
}
