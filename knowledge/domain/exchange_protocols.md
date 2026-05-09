# Exchange Protocols — Angel One SmartAPI

> This document defines the wire-level protocol details of the Angel One
> SmartAPI, as currently implemented in this system.

---

## Authentication Flow

### REST Login

```
POST https://apiconnect.angelone.in/rest/auth/angelbroking/user/v1/loginByPassword
```

**Request:**
```json
{
  "clientcode": "A321480",
  "password": "<4-digit PIN>",
  "totp": "<6-digit TOTP code>"
}
```

**Required headers:**
```
Content-Type: application/json
Accept: application/json
X-UserType: USER
X-SourceID: WEB
X-ClientLocalIP: 127.0.0.1
X-ClientPublicIP: <real public IP>
X-MACAddress: 00-00-00-00-00-00
X-PrivateKey: <API key>
```

**Response (on success):**
```json
{
  "status": true,
  "message": "SUCCESS",
  "data": {
    "jwtToken": "...",
    "refreshToken": "...",
    "feedToken": "..."
  }
}
```

**Tokens returned:**
| Token | Purpose | Lifetime |
|---|---|---|
| `jwtToken` | REST API authorization (`Bearer` header) | Until 00:00 IST (same day) |
| `feedToken` | WebSocket authentication header | Same session |
| `refreshToken` | Token renewal (not currently used) | — |

**TOTP generation:** SHA1, 6 digits, 30-second window, using the TOTP secret from the Angel One developer portal.

**Implementation:** [`ingestion/src/auth.rs`](../../ingestion/src/auth.rs)

---

## WebSocket v2 Stream

### Connection

```
wss://smartapisocket.angelone.in/smart-stream
```

**Auth headers (on upgrade):**
```
Authorization: Bearer <jwtToken>
x-api-key: <API key>
x-client-code: <client ID>
x-feed-token: <feedToken>
```

### Subscription Request

Sent as a JSON text frame immediately after connection:

```json
{
  "correlationID": "phase0_sub",
  "action": 1,
  "params": {
    "mode": 3,
    "tokenList": [{
      "exchangeType": 1,
      "tokens": ["26009", "26000"]
    }]
  }
}
```

**Action values:**
| Value | Meaning |
|---|---|
| 1 | Subscribe |
| 2 | Unsubscribe |

**Subscription modes:**
| Mode | Name | Data Level | Packet Size |
|---|---|---|---|
| 1 | LTP | Last traded price only | 51 bytes |
| 2 | Quote | LTP + OHLCV + total buy/sell qty | 123 bytes |
| 3 | SnapQuote | Quote + depth + OI + circuits | 379 bytes |
| 4 | Depth | Full 20-level order book | variable |

**Exchange types:**
| Value | Exchange | Token Examples |
|---|---|---|
| 1 | NSE_CM (Cash Market) | 26009 (NIFTY 50), 26000 (NIFTY BANK) |
| 2 | NSE_FO (Futures & Options) | F&O instrument tokens |
| 3 | BSE_CM | BSE cash equities |
| 4 | BSE_FO | BSE derivatives |
| 5 | MCX_FO | MCX commodity derivatives |

> **CRITICAL:** Tokens 26009 (NIFTY 50) and 26000 (NIFTY BANK) are index
> tokens on **NSE_CM (exchange_type=1)**, NOT NSE_FO.  Subscribing them on
> exchange_type=2 silently succeeds but returns zero data.

### Binary Packet Layout

All responses are **Little-Endian binary frames**.

#### Common Header (all modes, 51 bytes)

| Offset | Size | Type | Field |
|---|---|---|---|
| 0 | 1 | `u8` | `subscription_mode` |
| 1 | 1 | `u8` | `exchange_type` |
| 2 | 25 | `[u8;25]` | `token` (null-terminated ASCII) |
| 27 | 8 | `i64` | `sequence_number` |
| 35 | 8 | `i64` | `exchange_timestamp` (ms since epoch) |
| 43 | 8 | `i64` | `last_traded_price` (in paise) |

#### Quote Extension (mode 2+, bytes 51–122)

| Offset | Size | Type | Field |
|---|---|---|---|
| 51 | 8 | `i64` | `last_traded_qty` |
| 59 | 8 | `i64` | `avg_traded_price` |
| 67 | 8 | `i64` | `volume` |
| 75 | 8 | `f64` | `total_buy_qty` |
| 83 | 8 | `f64` | `total_sell_qty` |
| 91 | 8 | `i64` | `open` (paise) |
| 99 | 8 | `i64` | `high` (paise) |
| 107 | 8 | `i64` | `low` (paise) |
| 115 | 8 | `i64` | `close` (paise) |

#### SnapQuote Extension (mode 3, bytes 123–378)

| Offset | Size | Type | Field |
|---|---|---|---|
| 123 | 8 | `i64` | `last_traded_timestamp` |
| 131 | 8 | `i64` | `open_interest` |
| 139 | 8 | `i64` | `oi_change_pct` |
| 147 | 200 | `[DepthEntry;10]` | Best 5 buy + 5 sell levels |
| 347 | 8 | `i64` | `upper_circuit` |
| 355 | 8 | `i64` | `lower_circuit` |
| 363 | 8 | `i64` | `52_week_high` |
| 371 | 8 | `i64` | `52_week_low` |

**DepthEntry** (20 bytes each):

| Offset | Size | Type | Field |
|---|---|---|---|
| 0 | 2 | `u16` | `flag` (0 = buy, 1 = sell) |
| 2 | 8 | `i64` | `qty` |
| 10 | 8 | `i64` | `price` (paise) |
| 18 | 2 | `u16` | `num_orders` |

**Implementation:** [`common/src/parser.rs`](../../common/src/parser.rs)

### Heartbeat

The WebSocket connection requires a **ping every 10 seconds** or the server will disconnect. We send `"ping"` as a text frame and expect `"pong"` back.

**Implementation:** [`ingestion/src/websocket.rs`](../../ingestion/src/websocket.rs) — ping scheduler in the `run_stream` function.

---

## REST API Endpoints (Circuit Breaker)

Used only in emergency shutdown (Phase 1+, dry-run in Phase 0).

**Base URL:** `https://apiconnect.angelone.in/rest/secure/angelbroking`

### Cancel All Orders
```
POST /order/v1/cancelAllOrders
```

### Exit All Positions
```
POST /order/v1/exitAllPositions
```

**Required headers:**
```
Authorization: Bearer <jwtToken>
X-PrivateKey: <API key>
Content-Type: application/json
Accept: application/json
X-UserType: USER
X-SourceID: WEB
X-ClientLocalIP: 127.0.0.1
X-ClientPublicIP: <real public IP>
X-MACAddress: 00-00-00-00-00-00
User-Agent: AngelOneTradingBot/1.0
```

> **WAF NOTE:** Angel One's API sits behind a Web Application Firewall.
> Using `X-ClientPublicIP: 127.0.0.1` causes requests to be rejected with
> "Request Rejected." Always use the machine's real public IP (fetched via
> `api.ipify.org`). The `User-Agent` header is also required.

**Implementation:** [`circuit_breaker/src/main.rs`](../../circuit_breaker/src/main.rs)

---

## Known Quirks & Gotchas

1. **Index tokens have `qty=0`:** NIFTY 50 and NIFTY BANK are derived indices, not tradable instruments. `last_traded_qty` is always 0. This is expected.

2. **Silent subscription failure:** Subscribing a token on the wrong exchange type succeeds without error but produces zero data. Always verify `exchange_type` matches the token.

3. **Token reuse:** JWT tokens are valid until midnight IST. For systems running across midnight, token refresh is needed.

4. **Binary framing:** All numeric fields in the WebSocket stream are Little-Endian. Prices are in **paise** (integer, ₹1 = 100 paise).

5. **Connection limits:** Angel One allows a limited number of concurrent WebSocket connections per API key (typically 3). Reconnection logic must include backoff.
