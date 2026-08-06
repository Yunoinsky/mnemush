# mnemush — Windows 手动验证脚本
# 用途: 不依赖 GitHub Actions, 在本地 Windows 机器上一键验证整个项目。
# 用法:  在仓库根目录运行  powershell -ExecutionPolicy Bypass -File scripts/verify-windows.ps1
# 前置:  Rust (rustup stable), Node.js >= 20, Python 3, Git (msysgit) 已安装并在 PATH。
#
# 等价于 CI 的 9 个 matrix job 里 Windows 相关的部分, 外加 sync 端到端。

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $repoRoot
$failCount = 0

function Step([string]$name, [scriptblock]$body) {
    Write-Host "`n=== [$name] ===" -ForegroundColor Cyan
    try {
        & $body
        if ($LASTEXITCODE -ne 0) { throw "exit code $LASTEXITCODE" }
        Write-Host "  [OK] $name" -ForegroundColor Green
    } catch {
        Write-Host "  [FAIL] $name : $($_.Exception.Message)" -ForegroundColor Red
        $script:failCount++
    }
}

# 0. 前置检查
Step "前置: rustc/cargo/node/npm/python/git 存在" {
    $tools = @("rustc", "cargo", "node", "npm", "python", "git")
    foreach ($t in $tools) {
        if (-not (Get-Command $t -ErrorAction SilentlyContinue)) {
            throw "缺少 $t (请安装并加入 PATH)"
        }
    }
    Write-Host "  rustc: $(rustc --version)"
    Write-Host "  node:  $(node --version)"
    Write-Host "  python: $(python --version)"
}

# 1. Rust release build (含 fastembed / ONNX Runtime, 最耗时的步骤)
Step "cargo build --release" {
    cargo build --manifest-path crates/mnemush/Cargo.toml --release
}

# 2. Rust 全量测试 (127 tests)
Step "cargo test" {
    cargo test --manifest-path crates/mnemush/Cargo.toml
}

# 3. clippy (CI 用 --lib --bins -D clippy::all)
Step "cargo clippy --lib --bins -D clippy::all" {
    cargo clippy --manifest-path crates/mnemush/Cargo.toml --lib --bins -- -D clippy::all
}

# 4. fmt check
Step "cargo fmt --check" {
    cargo fmt --manifest-path crates/mnemush/Cargo.toml --check
}

# 5. TS 依赖 + build
Step "npm install + npm run build:ts" {
    npm install
    npm run build:ts
}

# 6. TS 单元测试 (client regex + pi)
Step "mnemush-client 测试" {
    Set-Location packages/mnemush-client
    npm test
    Set-Location $repoRoot
}

# 7. MCP smoke (spawn mnemush-mcp.exe, 覆盖 12+ 工具)
Step "python scripts/test-mcp.py" {
    python scripts/test-mcp.py
}

# 8. OpenCode 插件集成 (需要刚 build 的 mnemush-mcp.exe)
Step "mnemush-opencode 集成测试" {
    Set-Location packages/mnemush-opencode
    npm test
    Set-Location $repoRoot
}

# 9. mnemush 二进制 CLI 冒烟 + sync 端到端 (Git 作为传输层)
Step "mnemush CLI + sync 端到端" {
    $bin = Join-Path $repoRoot "crates\mnemush\target\release\mnemush.exe"
    if (-not (Test-Path $bin)) { throw "未找到 $bin" }
    & $bin --version
    if ($LASTEXITCODE -ne 0) { throw "mnemush --version 失败" }

    # 用一个临时数据目录, 避免碰真实 ~/.mnemush
    $tmp = Join-Path $env:TEMP ("mnemush-verify-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    $env:MNEMUSH_DATA_DIR = $tmp
    try {
        & $bin add "hello" "windows verify" -c note
        if ($LASTEXITCODE -ne 0) { throw "mnemush add 失败" }
        & $bin search "hello"
        if ($LASTEXITCODE -ne 0) { throw "mnemush search 失败" }

        # sync init + import round-trip (验证 git 在 Windows 上工作)
        $syncDir = Join-Path $tmp "sync"
        & $bin sync init -d $syncDir
        if ($LASTEXITCODE -ne 0) { throw "mnemush sync init 失败" }
        & $bin sync export -d $syncDir
        if ($LASTEXITCODE -ne 0) { throw "mnemush sync export 失败" }

        # 验证 sync 目录里 git 仓库存在且 memory.json 有内容
        if (-not (Test-Path (Join-Path $syncDir ".git"))) { throw "sync 目录不是 git 仓库" }
        $memJson = Join-Path $syncDir "memory.json"
        if (-not (Test-Path $memJson)) { throw "sync 缺 memory.json" }
        $n = (Get-Content $memJson -Raw | ConvertFrom-Json).Count
        if ($n -lt 1) { throw "memory.json 为空" }
        Write-Host "  sync round-trip OK: memory.json 含 $n 条"
    } finally {
        Remove-Item Env:MNEMUSH_DATA_DIR -ErrorAction SilentlyContinue
        # 清理临时目录 (可选: 注释掉以保留检查产物)
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

Write-Host "`n=========================================="
if ($failCount -eq 0) {
    Write-Host "✅ 全部通过! mnemush 在 Windows 上验证成功。" -ForegroundColor Green
    exit 0
} else {
    Write-Host "❌ $failCount 项失败, 见上方红色 [FAIL] 输出。" -ForegroundColor Red
    exit 1
}
