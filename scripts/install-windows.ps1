# mnemush — Windows 安装脚本
# 用法: powershell -ExecutionPolicy Bypass -File scripts/install-windows.ps1
# 前置: Rust (rustup stable), Node.js >= 20, Git (任意)在 PATH。

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $repoRoot

Write-Host "=== [1/4] Building Rust binary (release) ===" -ForegroundColor Cyan
cargo build --release --manifest-path crates/mnemush/Cargo.toml
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "=== [2/4] Installing binaries to ~/.cargo/bin ===" -ForegroundColor Cyan
$bin = Join-Path $repoRoot "crates\mnemush\target\release\mnemush.exe"
$mcp = Join-Path $repoRoot "crates\mnemush\target\release\mnemush-mcp.exe"
foreach ($f in @($bin, $mcp)) {
    if (-not (Test-Path $f)) { throw "missing build artifact: $f" }
}
$dst = Join-Path $env:USERPROFILE ".cargo\bin"
New-Item -ItemType Directory -Path $dst -Force | Out-Null
Copy-Item -Force $bin (Join-Path $dst "mnemush.exe")
Copy-Item -Force $mcp (Join-Path $dst "mnemush-mcp.exe")
Write-Host "  installed to $dst" -ForegroundColor Green

# PATH 检查(当前会话 / 持久)
$pathNow = $env:PATH -split ";" | Where-Object { $_ -eq $dst }
if (-not $pathNow) {
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$dst*") {
        Write-Host "  ! 注意: $dst 不在 PATH 中, 可执行: `"`$env:PATH = '$dst;' + `$env:PATH`"" -ForegroundColor Yellow
    }
}

Write-Host "=== [3/4] Initializing ~/.mnemush/ ===" -ForegroundColor Cyan
& (Join-Path $dst "mnemush.exe") init

Write-Host "=== [4/4] Building TS packages ===" -ForegroundColor Cyan
npm install
npm run build:ts

Write-Host ""
Write-Host "✓ Done. Try:" -ForegroundColor Green
Write-Host "  mnemush --version"
Write-Host "  mnemush stats"
Write-Host "  mnemush add 'use jose not jsonwebtoken' 'rationale here' -c decision --importance 0.9"
Write-Host ""
Write-Host "For Pi:    pi install npm:mnemush-pi  (or symlink packages\mnemush-pi into ~/.pi\agent\extensions\)"
Write-Host "For OpenCode:  ln -sf `"$repoRoot\packages\mnemush-opencode\dist\index.js`"  `$env:USERPROFILE\.config\opencode\plugin\mnemush.js"
Write-Host "For DSH:   dsh plugin --profile web add -w `"$repoRoot\packages\mnemush-dsh`""
