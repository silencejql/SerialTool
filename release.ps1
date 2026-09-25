# 发布构建:双架构 agent + 主程序 release。
# 版本号取自 assets/version.txt(首版 1.0.1),发布成功后补丁位自动 +1
# (本次发布产物仍是旧号,文件写回的是下一版:1.0.1 -> 1.0.2)。
# 用法:在工程根目录执行  ./release.ps1
$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

Get-Process serial_tool -ErrorAction SilentlyContinue | Stop-Process -Force

$versionFile = Join-Path $root "assets\version.txt"
$version = (Get-Content $versionFile -Raw).Trim()

Write-Host "[1/3] 构建 agent DLL (x64 + x86)..." -ForegroundColor Cyan
cargo build -p serial_agent --release
cargo build -p serial_agent --release --target i686-pc-windows-msvc

# 强制重跑主程序 build.rs,使 APP_VERSION 重新取 version.txt
Write-Host "[2/3] 构建主程序 release (v$version)..." -ForegroundColor Cyan
cargo clean -p serial_tool
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "主程序构建失败" }

# 发布成功:补丁位 +1 写回,供下次发布
$parts = $version.Split(".")
$parts[2] = ([int]$parts[2] + 1).ToString()
$next = $parts -join "."
Set-Content -Path $versionFile -Value $next -NoNewline -Encoding ascii

Write-Host "[3/3] 完成" -ForegroundColor Green
$exe = Join-Path $root "target\release\serial_tool.exe"
$item = Get-Item $exe
"本次发布: v$version  ->  $($item.FullName)  ({0:N2} MB)" -f ($item.Length / 1MB)
"下次发布版本: v$next(已写回 assets/version.txt)"
