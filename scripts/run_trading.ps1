# PowerShell script to start the Angel One trading system in sequence.
# Usage: .\scripts\run_trading.ps1

$bot_root = (Get-Item -Path ".\").FullName
Write-Host "Project Root: $bot_root" -ForegroundColor Yellow

# 1. Start Circuit Breaker in a new window
Write-Host "Step 1: Starting Circuit Breaker..." -ForegroundColor Cyan
Start-Process powershell.exe -ArgumentList "-NoExit", "-Command", "Set-Location '$bot_root'; cargo run -p circuit_breaker"

# 2. Wait for sockets to bind
Write-Host "Step 2: Waiting 5 seconds for ZMQ sockets to bind..." -ForegroundColor Gray
Start-Sleep -Seconds 5

# 3. Start Trading Node in a new window
Write-Host "Step 3: Starting Live Trading Node..." -ForegroundColor Cyan
Start-Process powershell.exe -ArgumentList "-NoExit", "-Command", "Set-Location '$bot_root'; cargo run -p trading"

Write-Host "`nAll nodes initiated in separate windows." -ForegroundColor Green
Write-Host "Monitor the new windows for logs/errors." -ForegroundColor Gray
