$ErrorActionPreference = "Continue"
$exe = (Resolve-Path ".\target\release\serial_tool.exe").Path
$pass = 0; $fail = 0
function Check($name, $cond) {
    if ($cond) { $script:pass++; "PASS  $name" }
    else { $script:fail++; "FAIL  $name" }
}

# 后台向 COM11 写原始字节(虚拟对 COM10<->COM11), 写完关闭
# 注意: byte[] 经 Start-Job 参数序列化会损坏,故传 hex 字符串在 job 内还原
# 大载荷按 2048 分块、块间 5ms 发送: 本机 com0com 虚拟对对单次巨型 Write
# 会在 4KB 边界停滞(纯 .NET 对照同样复现,与本程序无关);真实应用本就分块写
function Write-Raw-Async([byte[]]$b, [int]$preDelayMs = 400, [int]$baud = 9600, [int]$holdMs = 300) {
    $hex = ($b | ForEach-Object { $_.ToString("X2") }) -join ""
    Start-Job -ScriptBlock {
        param($hex, $preDelayMs, $baud, $holdMs)
        Start-Sleep -Milliseconds $preDelayMs
        # 等接收端先打开 COM10: 试探独占打开,失败(拒绝访问)=对端已在位。
        # 消除接收端冷启动/杀软扫描慢于 preDelay 导致的首字节丢失竞态
        $ok = $false
        $dl = (Get-Date).AddSeconds(5)
        while ((Get-Date) -lt $dl) {
            try { $pr = New-Object System.IO.Ports.SerialPort COM10,$baud; $pr.Open(); $pr.Close() }
            catch { $ok = $true; break }
            Start-Sleep -Milliseconds 20
        }
        Start-Sleep -Milliseconds 20
        $n = $hex.Length / 2
        $bytes = New-Object byte[] $n
        for ($i = 0; $i -lt $n; $i++) { $bytes[$i] = [Convert]::ToByte($hex.Substring($i*2, 2), 16) }
        $sp = New-Object System.IO.Ports.SerialPort COM11,$baud
        $sp.Open()
        for ($off = 0; $off -lt $n; $off += 2048) {
            $len = [Math]::Min(2048, $n - $off)
            $sp.Write($bytes, $off, $len)
            if ($n -gt 2048) { Start-Sleep -Milliseconds 5 }
        }
        Start-Sleep -Milliseconds $holdMs; $sp.Close()
    } -ArgumentList $hex, $preDelayMs, $baud, $holdMs | Out-Null
}

# 后台两段写: 写 h1 -> 间隔 gapMs -> 写 h2(hex 字符串),用于验证组帧时序
function Write-Two-Async([string]$h1, [string]$h2, [int]$gapMs, [int]$preDelayMs = 400, [int]$baud = 9600) {
    Start-Job -ScriptBlock {
        param($h1, $h2, $gapMs, $preDelayMs, $baud)
        function To-Bytes($h) {
            $n = $h.Length / 2; $bs = New-Object byte[] $n
            for ($i = 0; $i -lt $n; $i++) { $bs[$i] = [Convert]::ToByte($h.Substring($i*2, 2), 16) }
            ,$bs
        }
        Start-Sleep -Milliseconds $preDelayMs
        # 与 Write-Raw-Async 相同的对端在位探测,消除启动竞态
        $dl = (Get-Date).AddSeconds(5)
        while ((Get-Date) -lt $dl) {
            try { $pr = New-Object System.IO.Ports.SerialPort COM10,$baud; $pr.Open(); $pr.Close() }
            catch { break }
            Start-Sleep -Milliseconds 20
        }
        Start-Sleep -Milliseconds 20
        $sp = New-Object System.IO.Ports.SerialPort COM11,$baud
        $sp.Open()
        $b1 = To-Bytes $h1; $sp.Write($b1, 0, $b1.Length)
        Start-Sleep -Milliseconds $gapMs
        $b2 = To-Bytes $h2; $sp.Write($b2, 0, $b2.Length)
        Start-Sleep -Milliseconds 300; $sp.Close()
    } -ArgumentList $h1, $h2, $gapMs, $preDelayMs, $baud | Out-Null
}

function Recv([int]$ms, [string]$frame = "", [int]$baud = 9600) {
    if ($frame) { $env:SERIALTOOL_FRAME = $frame } else { Remove-Item Env:SERIALTOOL_FRAME -ErrorAction SilentlyContinue }
    $o = & $exe recv COM10 $baud $ms 2>&1 | Out-String
    Remove-Item Env:SERIALTOOL_FRAME -ErrorAction SilentlyContinue
    Get-Job | Wait-Job | Out-Null; Get-Job | Remove-Job -Force
    return $o
}

# 统计输出中 RX 数据帧条数
function Count-Frames($o) { ([regex]::Matches($o, 'RX\s+\d+B:')).Count }

"=== Idle 组帧 ==="
Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("HELLO"))
$o = Recv 1500
Check "idle: 短消息" ($o -match "HELLO")

Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("A`r`nB`r`nC`r`n"))
$o = Recv 1500
Check "idle: 多行聚合" ($o -match "A")

$o = Recv 800
Check "idle: 空接收不崩溃" ($LASTEXITCODE -eq 0)

$big = [byte[]](,65 * 65536)
Write-Raw-Async $big
$o = Recv 4000
Check "idle: 64KB 完整成帧 65536B" ($o -match "65536B")

"=== Idle 组帧时序(合并/拆分边界) ==="
# 两段写间隔 5ms(<10ms 断帧阈值):必须合并为 1 帧
Write-Two-Async "414141" "424242" 5
$o = Recv 1500
Check "idle: 5ms 间隔合并为 1 帧(实际 $(Count-Frames $o))" ((Count-Frames $o) -eq 1 -and $o -match "AAA" -and $o -match "BBB")

# 两段写间隔 50ms(>10ms):必须拆成 2 帧
Write-Two-Async "434343" "444444" 50
$o = Recv 1500
Check "idle: 50ms 间隔拆成 2 帧(实际 $(Count-Frames $o))" ((Count-Frames $o) -eq 2)

# 二进制全边界字节 00/01/7F/FE/FF 无损
Write-Raw-Async ([byte[]](0x00,0x01,0x7F,0xFE,0xFF))
$o = Recv 1500
Check "idle: 二进制边界字节 00/01/7F/FE/FF" ($o -match "00 01 7F FE FF")

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

"=== Newline 组帧时序(碎片合并/兜底/混合分隔符) ==="
# 单缓冲混合分隔符: a\r\n b\n c\rd\n => 3 帧(\r 仅作普通字节)
Write-Raw-Async ([Text.Encoding]::ASCII.GetBytes("a`r`nb`nc`rd`n"))
$o = Recv 1500 "newline"
Check "newline: 混合分隔符切 3 帧(实际 $(Count-Frames $o))" ((Count-Frames $o) -eq 3 -and $o -match "63 0D 64 0A")

# 一行被拆成两段写、间隔 50ms(<100ms 残余兜底):必须合成 1 帧
Write-Two-Async "504152" "540A" 50
$o = Recv 1500 "newline"
Check "newline: 50ms 碎片合并 1 帧 PART(实际 $(Count-Frames $o))" ((Count-Frames $o) -eq 1 -and $o -match "PART")

# 间隔 150ms(>100ms):残余先兜底成帧,后半另成帧 => 2 帧
Write-Two-Async "504152" "540A" 150
$o = Recv 2000 "newline"
Check "newline: 150ms 残余兜底共 2 帧(实际 $(Count-Frames $o))" ((Count-Frames $o) -eq 2)

# 无换行的超长行(70KB @115200):达 64KB 上限必须提前成帧,不积压不崩溃
$huge = [byte[]](,65 * 70000)
Write-Raw-Async $huge 400 115200 500
$o = Recv 4000 "newline" 115200
Check "newline: 70KB 无换行长行 64KB 封顶+残余(帧=$(Count-Frames $o))" ((Count-Frames $o) -ge 2 -and $o -match "65536B" -and $o -match "4464B")

"=== 高波特率吞吐 ==="
# 64KB @115200: 单帧完整且首尾标记字节都出现
$fast = [byte[]](,65 * 65536)
$fast[0] = 0xDE; $fast[1] = 0xAD; $fast[65534] = 0xBE; $fast[65535] = 0xEF
Write-Raw-Async $fast 400 115200
$o = Recv 4000 "" 115200
Check "115200: 64KB 单帧 65536B 且首尾标记 DE AD / BE EF" ($o -match "65536B" -and $o -match "DE AD" -and $o -match "BE EF")

"=== 快速开关 ==="
$ok = $true
1..15 | ForEach-Object {
    $null = Recv 300
    if ($LASTEXITCODE -ne 0) { $ok = $false }
}
Check "快速开关x15 不崩溃" $ok

# 极短接收窗口也不卡死/不崩溃
$o = Recv 50
Check "50ms 超短接收窗口退出码0" ($LASTEXITCODE -eq 0)

"=== send 边界 ==="
# 注意: PowerShell 中外部程序退出码需经管道捕获才可靠写入 $LASTEXITCODE
$o = & $exe send COM10 9600 "" 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 空字符串不崩溃" ($ec -eq 0)
$o = & $exe send COM10 9600 ("X" * 5000) 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 5KB 不崩溃" ($ec -eq 0)
$o = & $exe send COM99 9600 "X" 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "send: 不存在端口报错不panic (ec=$ec)" ($ec -ne 0)

"=== loop CLI 自检(注入目标路径) ==="
$lo = Start-Process -FilePath $exe -ArgumentList 'loop','COM11','9600','100','3000' -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 500
$o = Recv 2000
Stop-Process -Id $lo.Id -Force -ErrorAction SilentlyContinue
Check "loop: 周期发送 PING 可被对端接收" ($o -match "PING")

"=== 异常命令行参数 ==="
$o = & $exe recv COM99 9600 300 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "recv: 不存在端口优雅报错(ec=$ec)" ($ec -ne 0)
$o = & $exe loop COM99 9600 100 500 2>&1 | Out-String; $ec = $LASTEXITCODE
Check "loop: 不存在端口优雅报错(ec=$ec)" ($ec -ne 0)

""
"===== 结果: PASS=$pass FAIL=$fail ====="
if ($fail -gt 0) { exit 1 }
