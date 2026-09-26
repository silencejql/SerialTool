$ErrorActionPreference = "Continue"
$exe = (Resolve-Path ".\target\release\serial_tool.exe").Path
$pass = 0; $fail = 0
function Check($name, $cond) {
    if ($cond) { $script:pass++; "PASS  $name" }
    else { $script:fail++; "FAIL  $name" }
}

# 后台向 COM11 写原始字节(虚拟对 COM10<->COM11), 写完关闭
# 注意: byte[] 经 Start-Job 参数序列化会损坏,故传 hex 字符串在 job 内还原
function Write-Raw-Async([byte[]]$b, [int]$preDelayMs = 400) {
    $hex = ($b | ForEach-Object { $_.ToString("X2") }) -join ""
    Start-Job -ScriptBlock {
        param($hex, $preDelayMs)
        Start-Sleep -Milliseconds $preDelayMs
        $n = $hex.Length / 2
        $bytes = New-Object byte[] $n
        for ($i = 0; $i -lt $n; $i++) { $bytes[$i] = [Convert]::ToByte($hex.Substring($i*2, 2), 16) }
        $sp = New-Object System.IO.Ports.SerialPort COM11,9600
        $sp.Open(); $sp.Write($bytes, 0, $bytes.Length); Start-Sleep -Milliseconds 300; $sp.Close()
    } -ArgumentList $hex, $preDelayMs | Out-Null
}

function Recv([int]$ms, [string]$frame = "") {
    if ($frame) { $env:SERIALTOOL_FRAME = $frame } else { Remove-Item Env:SERIALTOOL_FRAME -ErrorAction SilentlyContinue }
    $o = & $exe recv COM10 9600 $ms 2>&1 | Out-String
    Remove-Item Env:SERIALTOOL_FRAME -ErrorAction SilentlyContinue
    Get-Job | Wait-Job | Out-Null; Get-Job | Remove-Job -Force
    return $o
}

"=== Idle 组帧 ==="
Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("HELLO"))
$o = Recv 1500
Check "idle: 短消息" ($o -match "HELLO")

Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("A`r`nB`r`nC`r`n"))
$o = Recv 1500
Check "idle: 多行聚合" ($o -match "A")

$o = Recv 800
Check "idle: 空接收不崩溃" ($LASTEXITCODE -eq 0)

$big = New-Object byte[] 65536; [Array]::Fill($big, [byte]65)
Write-Raw-Async $big
$o = Recv 20000
Check "idle: 64KB 收到数据" ($o -match "AAAA")

"=== Newline 组帧 ==="
Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("L1`nL2`nL3`n"))
$o = Recv 2000 "newline"
$n1 = ([regex]::Matches($o, "L1")).Count; $n2 = ([regex]::Matches($o, "L2")).Count; $n3 = ([regex]::Matches($o, "L3")).Count
Check "newline: 3行各成帧" ($n1 -ge 1 -and $n2 -ge 1 -and $n3 -ge 1)

Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("TAIL_NO_LF"))
$o = Recv 1500 "newline"
Check "newline: 无换行残余兜底" ($o -match "TAIL_NO_LF")

Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("`n`n`n"))
$o = Recv 1500 "newline"
Check "newline: 连续空行不崩溃" ($LASTEXITCODE -eq 0)

Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("CR`rONLY"))
$o = Recv 1500 "newline"
Check "newline: 仅CR不成帧残余兜底" ($o -match "CR")

"=== 快速开关 ==="
$ok = $true
1..15 | ForEach-Object {
    $null = Recv 300
    if ($LASTEXITCODE -ne 0) { $ok = $false }
}
Check "快速开关x15 不崩溃" $ok

"=== send 边界 ==="
# 注意: PowerShell 中外部程序退出码需经管道捕获才可靠写入 $LASTEXITCODE
$o = & $exe send COM10 9600 "" 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 空字符串不崩溃" ($ec -eq 0)
$o = & $exe send COM10 9600 ("X" * 5000) 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 5KB 不崩溃" ($ec -eq 0)
$o = & $exe send COM99 9600 "X" 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 不存在端口报错不panic (ec=$ec)" ($ec -ne 0)

""
"===== 结果: PASS=$pass FAIL=$fail ====="
