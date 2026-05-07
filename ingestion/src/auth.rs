//! # Angel One SmartAPI REST Authentication
//!
//! Handles the full login flow:
//! 1. Load credentials from `.env`
//! 2. Generate a live TOTP code
//! 3. POST to the login endpoint
//! 4. Return JWT + feed tokens for WebSocket use

use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::{error, info};

/// Tokens returned by a successful Angel One login.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuthTokens {
    pub jwt_token: String,
    pub feed_token: String,
    pub refresh_token: String,
    pub api_key: String,
    pub client_id: String,
}

/// Raw JSON shape of the login response data payload.
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
/// Reads the following environment variables (typically loaded from `.env`):
/// - `ANGEL_CLIENT_ID`
/// - `ANGEL_PIN`
/// - `ANGEL_API_KEY`
/// - `ANGEL_TOTP_SECRET`
pub async fn authenticate() -> Result<AuthTokens, Box<dyn std::error::Error + Send + Sync>> {
    let client_id = std::env::var("ANGEL_CLIENT_ID")
        .map_err(|_| "ANGEL_CLIENT_ID not set in environment")?;
    let pin =
        std::env::var("ANGEL_PIN").map_err(|_| "ANGEL_PIN not set in environment")?;
    let api_key = std::env::var("ANGEL_API_KEY")
        .map_err(|_| "ANGEL_API_KEY not set in environment")?;
    let totp_secret_str = std::env::var("ANGEL_TOTP_SECRET")
        .map_err(|_| "ANGEL_TOTP_SECRET not set in environment")?;

    // Generate the 6-digit TOTP token
    let secret_bytes = Secret::Encoded(totp_secret_str)
        .to_bytes()
        .map_err(|e| format!("Failed to decode TOTP secret: {e}"))?;

    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret_bytes)
        .map_err(|e| format!("Failed to create TOTP instance: {e}"))?;

    let totp_code = totp
        .generate_current()
        .map_err(|e| format!("Failed to generate TOTP code: {e}"))?;

    info!("Generated TOTP code, authenticating with Angel One...");

    let client = Client::new();
    let payload = json!({
        "clientcode": client_id,
        "password": pin,
        "totp": totp_code
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
        error!("Login HTTP error {status}: {body}");
        return Err(format!("Login failed with HTTP {status}").into());
    }

    let login_resp: LoginResponse = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse login response: {e}\nBody: {body}"))?;

    if !login_resp.status {
        error!("Login rejected: {}", login_resp.message);
        return Err(format!("Login rejected: {}", login_resp.message).into());
    }

    let data = login_resp
        .data
        .ok_or("Login response had status=true but no data payload")?;

    info!("Authentication successful for client {client_id}");

    Ok(AuthTokens {
        jwt_token: data.jwt_token,
        feed_token: data.feed_token,
        refresh_token: data.refresh_token,
        api_key,
        client_id,
    })
}
