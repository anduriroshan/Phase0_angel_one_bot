# PowerShell script to stop the Angel One trading system.
# Usage: .\scripts\stop_trading.ps1

Write-Host "Shutting down trading nodes..." -ForegroundColor Red

# Stop the specific processes
$nodes = @("trading", "circuit_breaker")

foreach ($node in $nodes) {
    $proc = Get-Process -Name $node -ErrorAction SilentlyContinue
    if ($proc) {
        Write-Host "Stopping $node (PID: $($proc.Id))..." -ForegroundColor Yellow
        Stop-Process -Name $node -Force
    } else {
        Write-Host "$node is not running." -ForegroundColor Gray
    }
}

Write-Host "`nTrading system shutdown complete." -ForegroundColor Green

# --- Git Sync ---
$bot_root = (Get-Item -Path "$PSScriptRoot\..").FullName
Set-Location $bot_root

$date = Get-Date -Format "yyyy-MM-dd"
Write-Host "Starting Git sync for $date..." -ForegroundColor Cyan

git add .
git commit -m "Auto-commit: Trading session $date"
git push

Write-Host "Git push complete." -ForegroundColor Green
