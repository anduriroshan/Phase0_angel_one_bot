# Phase 0: Data Substrate & Circuit Breaker Specification

## Objective
Establish a read-only, high-performance data ingestion pipeline from an NSE broker API and a hardwired risk circuit breaker. No live trading logic is implemented in this phase.

## 1. Unified Tick Schema
All incoming broker data must be normalized to this strict binary contract before moving downstream.

| Field | Type | Description |
| :--- | :--- | :--- |
| `ts_ns` | `Int64` | Exchange timestamp in nanoseconds |
| `inst_id` | `Int32` | Internal dictionary mapping for the NSE symbol |
| `side` | `Int8` | 1: Buy, 2: Sell, 0: Unknown/Trade |
| `price` | `Float64` | Execution or quote price |
| `qty` | `Int64` | Order or trade quantity |
| `seq_no` | `Int64` | Exchange sequence number (for gap detection) |

## 2. Component Architecture

### A. The Ingestion Node (Rust + Tokio)
* Connects to the broker's websocket feed.
* Deserializes incoming JSON packets.
* Maps fields to the Unified Tick Schema.
* Pushes the normalized struct into an in-memory `tokio::sync::mpsc` channel.

### B. The Storage Node (QuestDB + Parquet)
* **Hot Sink:** Reads from the `mpsc` channel and streams data to the local QuestDB instance via InfluxDB Line Protocol (ILP) over TCP (Port 9009) for sub-millisecond write latency.
* **Cold Sink:** Batches data into `arrow` record batches in memory. Flushes to disk as `.parquet` files compressed with Zstd every 60 minutes or when memory bounds are reached.
* Directory structure: `/data/raw/YYYY/MM/DD/inst_id.parquet`

### C. The Circuit Breaker (ZeroMQ + REST)
* Operates as an isolated Rust binary.
* Listens to a local ZeroMQ PUB/SUB socket (`ipc:///tmp/pnl_stream.ipc`) for heartbeat and PnL updates.
* If the heartbeat drops (timeout > 50ms) or PnL breaches the defined threshold, it immediately executes an HTTP POST request to the broker's `/cancelAllOrders` and `/closeAllPositions` endpoints.
* Requires a hard exit (`std::process::exit`) upon execution.

## 3. Execution Steps

1.  **Initialize Cargo Workspace:** Set up a workspace with three distinct crates: `ingestion`, `storage`, and `circuit_breaker`.
2.  **Websocket Connection:** Implement the `tokio-tungstenite` client to authenticate and subscribe to 5 liquid Nifty options via the broker API.
3.  **Schema Implementation:** Define the Rust `struct` for the Unified Tick Schema and implement conversion traits from the broker's JSON model.
4.  **Database Wiring:** Configure the ILP sender to push the mapped structs to the QuestDB Docker container.
5.  **Circuit Breaker Logic:** Implement the ZeroMQ listener and the REST API panic sequence.
6.  **Dry Run:** Start the system during active NSE market hours. Verify QuestDB ingestion rates and manually trigger the circuit breaker to confirm API cancellation latency.

1. The Core Engine & Architecture Reference
Repository: [nautechsystems/nautilus_trader](https://github.com/nautechsystems/nautilus_trader)

Why you need it: This is the gold standard for high-performance algorithmic trading in Rust. It is a production-grade, Rust-native engine with a deterministic event-driven architecture.

What to steal from it: Look at how they implement their data catalog and serialization. They have already written the highly optimized Rust code to batch tick data and flush it to Parquet files efficiently. Do not write your own Parquet writer; study theirs.

2. The Ingestion Node (Rust + Tokio Websockets)
Repository: [https://github.com/barter-rs/barter-rs](https://github.com/barter-rs/barter-rs)

Why you need it: Barter is an open-source Rust framework for event-driven live-trading. More specifically, its sub-crate barter-data is arguably the best open-source implementation of a high-performance websocket ingestion pipeline in Rust.

What to steal from it: Look at their tokio mpsc channel architecture. They show exactly how to manage dropping websocket connections, handle reconnections automatically, and deserialize incoming JSON streams into unified Rust structs without memory leaks. You can fork their barter-data crate and add a custom connector for the Angel One SmartAPI.

3. The QuestDB Storage Sink
Repository: [https://github.com/questdb/time-series-streaming-analytics-template](https://github.com/questdb/time-series-streaming-analytics-template)

Why you need it: This is QuestDB's official template repository for streaming financial tick data.

What to steal from it: It contains the exact Docker-compose setups and the Rust InfluxDB Line Protocol (ILP) sender configurations you need. It shows you how to format your Rust structs so they can be written to QuestDB in under a millisecond.

4. The NSE Specific Reference
Repository: [TechfaneTechnologies/nse](https://github.com/TechfaneTechnologies/nse)

Why you need it: This is a Rust CLI binary and library built specifically for extracting real-time data from the National Stock Exchange (India).

What to steal from it: While it may not use Angel One directly, you should study this repo to see how they model the NSE-specific data structures (like BankNifty option symbols and strike prices) inside a Rust environment.

Your Execution Strategy
If you want to use the "antigravity" approach (building fast by standing on the shoulders of giants), do this:

Clone barter-rs and use it as the blueprint for your Ingestion Node.

Clone the questdb streaming template to spin up your local Storage Node.

Write the "Glue": Your only real coding task for Phase 0 is writing the Angel One websocket connector to feed the barter-rs pipeline, and writing the ZeroMQ Circuit Breaker sidecar.


Yes, it is done perfectly. Your app `Bot_p0` is active, the dummy redirect URL bypassed the validation, and you have your API Key. 

Be sure to click the eye icon with the slash to reveal and copy your **Secret Key**, and ensure you have the **TOTP Secret** from the "Enable TOTP" page.

We will now verify these credentials by writing the first block of Rust code. This script will exclusively handle the REST authentication flow to prove we can programmatically generate a TOTP token and receive a valid session token from the exchange.

### 1. Configure the Environment
Inside your `sovereign_quant` directory, create a `.env` file to store your credentials securely. Do not commit this file to version control.

```env
# .env
ANGEL_CLIENT_ID="YOUR_CLIENT_ID"
ANGEL_PIN="YOUR_4_DIGIT_PIN"
ANGEL_API_KEY="YOUR_API_KEY"
ANGEL_TOTP_SECRET="YOUR_TOTP_SECRET"
```

### 2. Add Dependencies
Open your `Cargo.toml` and add the crates required for asynchronous HTTP requests, JSON parsing, and TOTP generation.

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.11", features = ["json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
dotenv = "0.15"
totp-rs = "5.3"
```

### 3. The Authentication Module
Replace the contents of `src/main.rs` with this authentication test. This code reads your `.env`, generates the live 6-digit TOTP code, and hits the Angel One login endpoint.

```rust
use dotenv::dotenv;
use reqwest::Client;
use serde_json::json;
use std::env;
use totp_rs::{Algorithm, Secret, TOTP};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    let client_id = env::var("ANGEL_CLIENT_ID").expect("ANGEL_CLIENT_ID must be set");
    let pin = env::var("ANGEL_PIN").expect("ANGEL_PIN must be set");
    let api_key = env::var("ANGEL_API_KEY").expect("ANGEL_API_KEY must be set");
    let totp_secret_str = env::var("ANGEL_TOTP_SECRET").expect("ANGEL_TOTP_SECRET must be set");

    // Generate the 6-digit TOTP token
    let secret = Secret::Encoded(totp_secret_str).to_bytes().unwrap();
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret,
    ).unwrap();
    let totp_code = totp.generate_current().unwrap();

    println!("Generated TOTP: {}", totp_code);
    println!("Authenticating...");

    // Execute the Login Request
    let client = Client::new();
    let login_url = "https://apiconnect.angelbroking.com/rest/auth/angelbroking/user/v1/loginByPassword";

    let payload = json!({
        "clientcode": client_id,
        "password": pin,
        "totp": totp_code
    });

    let res = client.post(login_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("X-UserType", "USER")
        .header("X-SourceID", "WEB")
        .header("X-ClientLocalIP", "127.0.0.1")
        .header("X-ClientPublicIP", "127.0.0.1")
        .header("X-MACAddress", "00-00-00-00-00-00")
        .header("X-PrivateKey", api_key)
        .json(&payload)
        .send()
        .await?;

    let response_text = res.text().await?;
    
    // Check if authentication was successful
    if response_text.contains("\"status\":true") {
        println!("Authentication Successful!");
        // The jwtToken from this response is what we need for the websocket connection
        // println!("Response: {}", response_text); 
    } else {
        println!("Authentication Failed. Check credentials.");
        println!("Error Data: {}", response_text);
    }

    Ok(())
}
```

Run `cargo run` in your terminal. If your `.env` configuration is correct, you will receive an "Authentication Successful!" message alongside a large JSON payload containing your `jwtToken` and `feedToken`, which are the exact keys required to unlock the websocket data stream in the next step.