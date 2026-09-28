# Run the backtest across all recorded date folders for ONE strategy and print
# a per-day summary, NET OF BOTH COST TERMS.
#
# Why this script applies brokerage instead of the engine:
#   NautilusTrader's FeeModelAny allows only ONE fee model per venue -- either
#   Fixed (flat) or MakerTaker (percentage), never both -- and ADR-007 pins the
#   crate. The engine therefore charges only the percentage/statutory term from
#   config/costs.toml. Flat per-order brokerage is applied here, afterwards.
#
# IMPORTANT -- "Total orders" is orders SUBMITTED, not orders FILLED. Brokerage
# is charged only on EXECUTED orders. The engine does not currently report a
# fill count, so this script brackets the answer instead of inventing one:
#
#   Net(best)  = PnL - cost * (2 x positions)   <- assumes only entry+exit filled
#   Net(worst) = PnL - cost * (total orders)    <- assumes every order filled
#   BE-fills   = PnL / cost                     <- fills above this => losing day
#
# BE-fills is the number that actually decides things: if the true fill count
# exceeds it, the day is negative. Instrumenting the real fill count is the
# follow-up fix (add on_order_filled to the strategies, or query the cache
# post-run) -- until then, treat Net(worst) as the honest default.
#
# Usage:
#   .\scripts\run_backtest_all.ps1 -Strategy vwap
#   .\scripts\run_backtest_all.ps1 -Strategy basis -Month 05
#
# Run each strategy separately so their PnL does not commingle.
param(
    [ValidateSet("basis", "vwap", "both")]
    [string]$Strategy = "vwap",
    [string]$DataDir = "./data/raw",
    [string]$Year = "2026",
    [string]$Month = "05",
    [double]$BrokeragePerOrder = -1,   # -1 => read from config/costs.toml
    [double]$GstPct = -1               # -1 => read from config/costs.toml
)

$monthDir = Join-Path $DataDir "$Year/$Month"
if (-not (Test-Path $monthDir)) {
    Write-Host "No data directory: $monthDir"
    exit 1
}

# --- Load the flat-brokerage term from config/costs.toml ---
function Get-TomlNumber {
    param([string]$Path, [string]$Key, [double]$Default)
    if (-not (Test-Path $Path)) { return $Default }
    $line = Select-String -Path $Path -Pattern ("^\s*" + [regex]::Escape($Key) + "\s*=") | Select-Object -First 1
    if (-not $line) { return $Default }
    $m = [regex]::Match($line.Line, '=\s*([0-9]*\.?[0-9]+)')
    if ($m.Success) { return [double]$m.Groups[1].Value }
    return $Default
}

$costsFile = "config/costs.toml"
if ($BrokeragePerOrder -lt 0) {
    $BrokeragePerOrder = Get-TomlNumber -Path $costsFile -Key "brokerage_per_order_inr" -Default 20.0
}
if ($GstPct -lt 0) {
    $GstPct = Get-TomlNumber -Path $costsFile -Key "gst_pct" -Default 18.0
}
$costPerFill = $BrokeragePerOrder * (1.0 + ($GstPct / 100.0))

Write-Host ""
Write-Host "Brokerage model: Rs $BrokeragePerOrder per executed order + $GstPct% GST = Rs $([math]::Round($costPerFill,2)) per fill"
Write-Host "VERIFY this matches your actual Angel One plan before trusting any net figure."
Write-Host ""

# Dev profile: PnL is identical to release, and it avoids a long from-scratch
# release compile of the NautilusTrader dependency tree.
Write-Host "Building backtest (dev)..."
cargo build -p backtest
if ($LASTEXITCODE -ne 0) { Write-Host "Build failed"; exit 1 }

$logDir = "./data/backtest_runs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

# Extract the first number from a matched log line (handles "PnL (total): 302.83 INR").
function Get-LineNumber {
    param($Match)
    if (-not $Match) { return $null }
    $m = [regex]::Match($Match.Line, '(-?[0-9][0-9,]*\.?[0-9]*)\s*(INR)?\s*$')
    if (-not $m.Success) {
        $m = [regex]::Match($Match.Line, '(-?[0-9][0-9,]*\.?[0-9]*)')
    }
    if ($m.Success) { return [double]($m.Groups[1].Value -replace ',', '') }
    return $null
}

$days = Get-ChildItem -Path $monthDir -Directory | Sort-Object Name
Write-Host ""
Write-Host ("{0,-12} {1,-7} {2,>12} {3,>6} {4,>5} {5,>12} {6,>12} {7,>9}" -f `
    "DATE", "STRAT", "PnL(stat)", "Ord", "Pos", "Net(best)", "Net(worst)", "BE-fills")
Write-Host ("-" * 85)

$sumPnl = 0.0; $sumBest = 0.0; $sumWorst = 0.0; $sumOrd = 0; $sumPos = 0

foreach ($day in $days) {
    $date = "$Year-$Month-$($day.Name)"
    $logFile = Join-Path $logDir "$($Strategy)_$date.log"
    $raw = & cargo run -p backtest -- --date $date --strategy $Strategy 2>&1
    $raw | Out-File -FilePath $logFile -Encoding utf8

    $pnlM = ($raw | Select-String -Pattern 'PnL \(total\):'   | Select-Object -Last 1)
    $winM = ($raw | Select-String -Pattern 'Win Rate:'        | Select-Object -Last 1)
    $ordM = ($raw | Select-String -Pattern 'Total orders:'    | Select-Object -Last 1)
    $posM = ($raw | Select-String -Pattern 'Total positions:' | Select-Object -Last 1)

    $pnl = Get-LineNumber $pnlM
    $ord = Get-LineNumber $ordM
    $pos = Get-LineNumber $posM

    if ($null -eq $pnl) {
        Write-Host ("{0,-12} {1,-7} {2,>12}" -f $date, $Strategy, "no-result")
        continue
    }
    if ($null -eq $ord) { $ord = 0 }
    if ($null -eq $pos) { $pos = 0 }

    # Best case: only entry+exit of each position actually filled.
    # Worst case: every submitted order filled.
    $fillsBest  = 2 * $pos
    $fillsWorst = $ord
    $netBest    = $pnl - ($costPerFill * $fillsBest)
    $netWorst   = $pnl - ($costPerFill * $fillsWorst)
    $beFills    = [math]::Floor($pnl / $costPerFill)

    $sumPnl += $pnl; $sumBest += $netBest; $sumWorst += $netWorst
    $sumOrd += $ord; $sumPos += $pos

    Write-Host ("{0,-12} {1,-7} {2,>12:N2} {3,>6} {4,>5} {5,>12:N2} {6,>12:N2} {7,>9}" -f `
        $date, $Strategy, $pnl, [int]$ord, [int]$pos, $netBest, $netWorst, [int]$beFills)
}

Write-Host ("-" * 85)
Write-Host ("{0,-12} {1,-7} {2,>12:N2} {3,>6} {4,>5} {5,>12:N2} {6,>12:N2}" -f `
    "TOTAL", $Strategy, $sumPnl, [int]$sumOrd, [int]$sumPos, $sumBest, $sumWorst)

Write-Host ""
Write-Host "PnL(stat)  = engine PnL, net of the percentage/statutory model only (config/costs.toml)."
Write-Host "Net(best)  = minus brokerage assuming ONLY entry+exit filled (2 x positions)."
Write-Host "Net(worst) = minus brokerage assuming EVERY submitted order filled."
Write-Host "BE-fills   = break-even fill count. More fills than this => the day loses money."
Write-Host ""
Write-Host "Per-day logs written to $logDir"
Write-Host "Report per-day, not aggregate: one good day can carry a whole month."
