# 发布构建:双架构 agent + 主程序 release,版本号(构建号)自动 +1。
# 用法:在工程根目录执行  ./release.ps1
$ErrorActionPreference = "Stop"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

Get-Process serial_tool -ErrorAction SilentlyContinue | Stop-Process -Force

Write-Host "[1/4] 构建 agent DLL (x64)..." -ForegroundColor Cyan
cargo build -p serial_agent --release

Write-Host "[2/4] 构建 agent DLL (x86)..." -ForegroundColor Cyan
cargo build -p serial_agent --release --target i686-pc-windows-msvc

# 强制重跑主程序 build.rs,使 assets/build_num.txt 构建号 +1
Write-Host "[3/4] 递增版本号并构建主程序 release..." -ForegroundColor Cyan
cargo clean -p serial_tool
cargo build --release

Write-Host "[4/4] 完成" -ForegroundColor Green
$exe = Join-Path $root "target\release\serial_tool.exe"
$item = Get-Item $exe
"{0}  ({1:N2} MB)" -f $item.FullName, ($item.Length / 1MB)
"当前构建号: $(Get-Content (Join-Path $root 'assets\build_num.txt') -Raw)"
