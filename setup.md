# System Setup & Installation Guide

> This document details the prerequisite software, system configurations, and environment variables required to run the Phase 0 Angel One trading bot on Windows.

---

## 1. Core Prerequisites

### A. Rust Toolchain
The entire system is built in Rust. You need the standard Rust toolchain.
1. Download and install `rustup-init.exe` from [rustup.rs](https://rustup.rs/).
2. During installation, it will prompt you to install the **C++ build tools for Visual Studio** (MSVC). This is **required** to compile Rust binaries on Windows. Follow the prompts to install it.
3. Verify installation:
   ```powershell
   rustc --version
   cargo --version
   ```

### B. Docker Desktop (For Storage Node)
The hot-sink storage uses QuestDB, which runs in a Docker container.
1. Download and install [Docker Desktop for Windows](https://www.docker.com/products/docker-desktop/).
2. Ensure Docker Desktop is running (check the system tray icon).
3. Verify installation:
   ```powershell
   docker --version
   docker compose version
   ```

*(Note: If you only want to use Parquet cold storage and don't care about real-time QuestDB querying, Docker is optional. The system will gracefully degrade to Parquet-only mode).*

---

## 2. Environment Configuration

The system relies on a `.env` file at the root of the workspace. Do **not** commit this file to version control. 

1. Copy the example file to create your active `.env`:
   ```powershell
   Copy-Item .env.example .env
   ```

2. Open `.env` and fill in your Angel One SmartAPI credentials:
   ```env
   # Angel One Credentials
   ANGEL_CLIENT_ID="A321480"             # Your client code
   ANGEL_PIN="1234"                      # Your 4-digit PIN
   ANGEL_API_KEY="YOUR_API_KEY"          # From SmartAPI Dashboard
   ANGEL_TOTP_SECRET="YOUR_TOTP_SECRET"  # From Enable TOTP setup
   
   # Circuit Breaker Configuration
   CIRCUIT_BREAKER_MAX_LOSS=10000.0
   CIRCUIT_BREAKER_GRACE_SECS=10
   CIRCUIT_BREAKER_DRY_RUN=true          # MUST be true for Phase 0
   
   # Subscription Configuration
   SUBSCRIBE_EXCHANGE=1                  # 1=NSE_CM (Index), 2=NSE_FO
   ```

---

## 3. Project Compilation

Compile the entire workspace to ensure all dependencies are downloaded and built successfully.

```powershell
cargo build
```

**Note on ZeroMQ:** We use the pure-Rust `zeromq` crate. Unlike the C-bindings `zmq` crate, this does **not** require installing any external C libraries (like `libzmq`) on Windows. It will compile natively.

---

## 4. Running the System

To run the full Phase 0 pipeline, you need to start the components in this order:

### Step 1: Start QuestDB (Hot Storage)
```powershell
docker compose up -d
```
*You can view the QuestDB console at `http://localhost:9000`.*

### Step 2: Start the Ingestion Node
Open a new PowerShell terminal and run:
```powershell
cargo run -p ingestion
```
*This will authenticate, connect to the WebSocket, bind the ZMQ publisher to port `5555`, and start processing ticks.*

### Step 3: Start the Circuit Breaker
Open a **second** PowerShell terminal and run:
```powershell
cargo run -p circuit_breaker
```
*This connects to the ingestion node's ZMQ socket, receives heartbeats, and monitors PnL.*

---

## 5. Troubleshooting

**"Request Rejected" during authentication:**
Angel One uses a WAF that blocks loopback IPs. Ensure your ingestion node is successfully fetching your real public IP (via `api.ipify.org`) as implemented in the auth flow.

**"No data received" from WebSocket:**
Double-check `SUBSCRIBE_EXCHANGE` in `.env`. NIFTY 50 and BANKNIFTY index tokens require `1` (NSE_CM). If set to `2` (NSE_FO), the server accepts the subscription but sends no data.

**Circuit Breaker triggers immediately:**
Ensure the ingestion node is running and publishing heartbeats. If the circuit breaker starts without an ingestion node, it will trigger a timeout after the 10-second grace period.
