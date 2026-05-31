# Run the backtest across all recorded date folders for ONE strategy and print
# a per-day summary. PnL reported here is NET of the cost model in
# config/costs.toml (STT + exchange charges), so it is the go/no-go artifact.
#
# Usage:
#   .\scripts\run_backtest_all.ps1 -Strategy vwap
#   .\scripts\run_backtest_all.ps1 -Strategy basis
#
# Run each strategy separately so their PnL does not commingle.
param(
    [ValidateSet("basis", "vwap", "both")]
    [string]$Strategy = "vwap",
    [string]$DataDir = "./data/raw",
    [string]$Year = "2026",
    [string]$Month = "05"
)

$monthDir = Join-Path $DataDir "$Year/$Month"
if (-not (Test-Path $monthDir)) {
    Write-Host "No data directory: $monthDir"
    exit 1
}

# Dev profile: PnL is identical to release, and it avoids a long from-scratch
# release compile of the NautilusTrader dependency tree. Add -Release handling
# later if you need the latency numbers (not relevant for a PnL sweep).
Write-Host "Building backtest (dev)..."
cargo build -p backtest
if ($LASTEXITCODE -ne 0) { Write-Host "Build failed"; exit 1 }

$logDir = "./data/backtest_runs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

$days = Get-ChildItem -Path $monthDir -Directory | Sort-Object Name
Write-Host ""
Write-Host ("{0,-12} {1,-7} {2,-16} {3,-10} {4}" -f "DATE", "STRAT", "PnL(net INR)", "WinRate", "Orders")
Write-Host ("-" * 60)

foreach ($day in $days) {
    $date = "$Year-$Month-$($day.Name)"
    $logFile = Join-Path $logDir "$($Strategy)_$date.log"
    $raw = & cargo run -p backtest -- --date $date --strategy $Strategy 2>&1
    $raw | Out-File -FilePath $logFile -Encoding utf8

    $pnl = ($raw | Select-String -Pattern 'PnL \(total\):' | Select-Object -Last 1)
    $win = ($raw | Select-String -Pattern 'Win Rate:' | Select-Object -Last 1)
    $ord = ($raw | Select-String -Pattern 'Total orders:' | Select-Object -Last 1)
    $pnlV = if ($pnl) { ($pnl.Line -split ':')[-1].Trim() } else { "n/a" }
    $winV = if ($win) { ($win.Line -split ':')[-1].Trim() } else { "-" }
    $ordV = if ($ord) { ($ord.Line -split ':')[-1].Trim() } else { "-" }
    Write-Host ("{0,-12} {1,-7} {2,-16} {3,-10} {4}" -f $date, $Strategy, $pnlV, $winV, $ordV)
}

Write-Host ""
Write-Host "Per-day logs written to $logDir"
Write-Host "PnL is NET of config/costs.toml. Run -Strategy basis and -Strategy vwap separately to compare."
