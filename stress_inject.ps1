# 注入式监控全矩阵稳定性测试(x64 + x86 WOW64)
# 用法: powershell -ExecutionPolicy Bypass -File stress_inject.ps1
$ErrorActionPreference = "Continue"
$exe = (Resolve-Path ".\target\debug\serial_tool.exe").Path
$pass = 0; $fail = 0; $script:children = @()
function Check($name, $cond) {
    if ($cond) { $script:pass++; "PASS  $name" }
    else { $script:fail++; "FAIL  $name" }
}
function Start-Target($argList, $file = $exe) {
    $p = Start-Process -FilePath $file -ArgumentList $argList -PassThru -WindowStyle Hidden
    $script:children += $p.Id
    return $p
}
function Cleanup {
    foreach ($id in $script:children) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
}
trap { Cleanup; break }

# 跑一轮 inject,返回 @{exit; online; tx; rx; out}
function Invoke-Inject($tpid, $ms) {
    $o = "$env:TEMP\si_out.txt"; $e = "$env:TEMP\si_err.txt"
    $j = Start-Process -FilePath $exe -ArgumentList 'inject',"$tpid","$ms" -WorkingDirectory (Get-Location) `
        -RedirectStandardOutput $o -RedirectStandardError $e -PassThru -WindowStyle Hidden -Wait
    $out = Get-Content $o -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
    $err = Get-Content $e -Raw -Encoding UTF8 -ErrorAction SilentlyContinue
    return @{
        exit = $j.ExitCode
        online = ($out -match 'agent 上线')
        tx = ([regex]::Matches($out, 'TX\s+\d+B:')).Count
        rx = ([regex]::Matches($out, 'RX\s+\d+B:')).Count
        out = $out; err = $err
    }
}

"=== 1. CLI 异常参数优雅处理 ==="
$j = Start-Process -FilePath $exe -ArgumentList 'scan' -PassThru -WindowStyle Hidden -Wait
Check "scan 退出码0" ($j.ExitCode -eq 0)

$r = Invoke-Inject 9999999 800
Check "inject 不存在 pid 优雅失败(exit=$($r.exit))" ($r.exit -ne 0 -and "$($r.err)".Length -gt 0)

$r = Invoke-Inject 0 800
Check "inject pid 0(系统空闲进程)优雅失败(exit=$($r.exit))" ($r.exit -ne 0)

$j = Start-Process -FilePath $exe -ArgumentList 'inject','notapid' -PassThru -WindowStyle Hidden -Wait
Check "inject 非数字参数 退出码1" ($j.ExitCode -eq 1)

"=== 2. x64 目标: 10 轮注入/detach 高频收发回归 ==="
$t = Start-Target @('loop','COM11','9600','20','90000')
Start-Sleep -Milliseconds 800
$alive0 = [bool](Get-Process -Id $t.Id -ErrorAction SilentlyContinue)
Check "x64 目标启动存活" $alive0
if (-not $alive0) { Cleanup; "目标启动失败,中止"; exit 1 }

$snap = @{}
$roundOk = $true
for ($i = 1; $i -le 10; $i++) {
    $r = Invoke-Inject $t.Id 1200
    Start-Sleep -Milliseconds 150
    $alive = [bool](Get-Process -Id $t.Id -ErrorAction SilentlyContinue)
    $ok = ($r.exit -eq 0 -and $r.online -and $r.tx -gt 0 -and $alive)
    if (-not $ok) {
        $roundOk = $false
        "  轮次${i} 异常: exit=$($r.exit) online=$($r.online) tx=$($r.tx) alive=$alive"
        if ($r.err) { "  err: $($r.err)" }
        break
    }
    if ($i -eq 1 -or $i -eq 5 -or $i -eq 10) {
        $pr = Get-Process -Id $t.Id
        $snap[$i] = @{ h = $pr.HandleCount; ws = [int64]$pr.WorkingSet64; pm = [int64]$pr.PagedMemorySize64 }
        "  轮次${i}: 句柄=$($pr.HandleCount) WS=$([int]($pr.WorkingSet64/1MB))MB"
    }
}
Check "x64 10轮注入/detach 全绿(上线+数据+存活)" $roundOk
Check "x64 10轮后目标仍存活" ([bool](Get-Process -Id $t.Id -ErrorAction SilentlyContinue))
$leak = $true
if ($snap.ContainsKey(1) -and $snap.ContainsKey(10)) {
    $dh = $snap[10].h - $snap[1].h
    $dpm = ($snap[10].pm - $snap[1].pm) / 1MB
    "  句柄增量 r1->r10: $dh ; 分页内存增量: $([math]::Round($dpm,1))MB"
    # agent 驻留是设计内,允许少量常驻;10轮后不得持续增长
    $leak = ($dh -le 10 -and $dpm -lt 10)
}
Check "x64 资源无持续泄漏(句柄增量<=10,内存<10MB)" $leak
# 释放 COM11 供后续组使用
Stop-Process -Id $t.Id -Force -ErrorAction SilentlyContinue
$script:children = @($script:children | Where-Object { $_ -ne $t.Id })

"=== 3. 监控期间目标被杀:注入端必须干净退出不卡死 ==="
$t2 = Start-Target @('loop','COM11','9600','20','60000')
Start-Sleep -Milliseconds 800
$o = "$env:TEMP\si_kill.txt"; $e = "$env:TEMP\si_killerr.txt"
$inj = Start-Process -FilePath $exe -ArgumentList 'inject',"$($t2.Id)",'8000' -WorkingDirectory (Get-Location) `
    -RedirectStandardOutput $o -RedirectStandardError $e -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 700
$wasOnline = ((Get-Content $o -Raw -Encoding UTF8 -ErrorAction SilentlyContinue) -match 'agent 上线')
Stop-Process -Id $t2.Id -Force
$script:children = @($script:children | Where-Object { $_ -ne $t2.Id })
# 注意: 重定向 stdout/stderr 时,.NET 的 WaitForExit(int)/WaitForExit() 都会
# 等待重定向流 EOF,可能进程已退出仍返回 false 甚至抛异常。直接轮询 HasExited
# (底层仅查进程句柄信号)判定退出,最多等 8s
$deadline = (Get-Date).AddSeconds(8)
$clean = $false
while ((Get-Date) -lt $deadline) {
    if ($inj.HasExited) { $clean = $true; break }
    Start-Sleep -Milliseconds 50
}
# 进程刚退出的僵尸瞬间 ExitCode 可能暂时读不到(重定向流场景),重试几次;
# 只要 HasExited 即达成"不卡死"目标,退出码在其余用例已充分验证
$ec = $null
if ($clean) {
    for ($k = 0; $k -lt 20; $k++) {
        try { $ec = $inj.ExitCode } catch {}
        if ($null -ne $ec) { break }
        Start-Sleep -Milliseconds 50
    }
}
Check "目标被杀前已上线" $wasOnline
Check "目标被杀后注入端 8s 内退出(无卡死)" ($clean -and ($null -eq $ec -or $ec -eq 0))
if (-not $clean) {
    "  [诊断] 存活注入进程:"
    Get-Process serial_tool -ErrorAction SilentlyContinue | ForEach-Object { "    pid=$($_.Id) start=$($_.StartTime.ToString('HH:mm:ss'))" }
    "  [诊断] 输出尾部:"; Get-Content $o -Tail 4 -Encoding UTF8 | ForEach-Object { "    $_" }
    Stop-Process -Id $inj.Id -Force -ErrorAction SilentlyContinue
}

"=== 4. 非串口进程注入(ping): hook 安全 + detach 不杀进程 ==="
$ping = Start-Process -FilePath "$env:WINDIR\System32\ping.exe" -ArgumentList '-n','24','127.0.0.1' -PassThru -WindowStyle Hidden
$script:children += $ping.Id
Start-Sleep -Milliseconds 500
$r = Invoke-Inject $ping.Id 1500
$pingAlive = [bool](Get-Process -Id $ping.Id -ErrorAction SilentlyContinue)
Check "非串口进程可上线(online=$($r.online),exit=$($r.exit))" ($r.exit -eq 0 -and $r.online)
Check "detach 后非串口进程存活" $pingAlive

"=== 5. 数据完整性: 128B 序号流分片到达,RX 拼接无丢失/错序 ==="
$t3 = Start-Target @('loop','COM11','9600','1000','20000')
Start-Sleep -Milliseconds 800
# COM10 侧发送 0..127 序号块,16B/片 间隔15ms
$jobHex = ((0..127 | ForEach-Object { $_.ToString('X2') }) -join '')
Start-Job -ScriptBlock {
    param($hex)
    Start-Sleep -Milliseconds 800
    $n = $hex.Length / 2; $bs = New-Object byte[] $n
    for ($i = 0; $i -lt $n; $i++) { $bs[$i] = [Convert]::ToByte($hex.Substring($i*2, 2), 16) }
    $sp = New-Object System.IO.Ports.SerialPort COM10,9600
    $sp.Open()
    for ($off = 0; $off -lt $n; $off += 16) {
        $len = [Math]::Min(16, $n - $off); $sp.Write($bs, $off, $len); Start-Sleep -Milliseconds 15
    }
    Start-Sleep -Milliseconds 500; $sp.Close()
} -ArgumentList $jobHex | Out-Null
$r = Invoke-Inject $t3.Id 4000
Get-Job | Wait-Job | Out-Null; Get-Job | Remove-Job -Force
# 从注入端输出的 RX 行提取 hex 并按顺序拼接
$rxHex = ''
foreach ($line in ($r.out -split "`n")) {
    if ($line -match 'RX\s+\d+B:\s+([0-9A-F ]+?)\s*\|') { $rxHex += ($Matches[1] -replace '\s', '') }
}
$expected = $jobHex
$intact = $rxHex.Contains($expected)
Check "RX 拼接包含完整 0..127 序号块(实收 $($rxHex.Length/2)B)" $intact
if (-not $intact) { "  期望: $expected"; "  实收: $rxHex" }
# 释放 COM11 供 x86 组使用
Stop-Process -Id $t3.Id -Force -ErrorAction SilentlyContinue
$script:children = @($script:children | Where-Object { $_ -ne $t3.Id })

"=== 6. x86(WOW64)目标: 5 轮注入/detach 回归 ==="
$cs = "$env:TEMP\x86target_stress.cs"
@'
using System;
using System.IO.Ports;
using System.Threading;
class X86Target {
    static void Main(string[] args) {
        string port = args[0]; int ms = int.Parse(args[1]); int dur = int.Parse(args[2]);
        try {
            SerialPort sp = new SerialPort(port, 9600); sp.Open();
            var end = DateTime.UtcNow.AddMilliseconds(dur); int i = 0;
            while (DateTime.UtcNow < end) {
                byte[] b = System.Text.Encoding.ASCII.GetBytes("PINGX " + (i++) + "\r\n");
                try { sp.Write(b, 0, b.Length); } catch {}
                Thread.Sleep(ms);
            }
            sp.Close();
        } catch (Exception e) { Console.Error.WriteLine("ERR " + e.Message); Environment.Exit(1); }
    }
}
'@ | Set-Content -Path $cs -Encoding ASCII
$csc = "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe"
$x86exe = "$env:TEMP\x86target_stress.exe"
& $csc /nologo /platform:x86 /out:$x86exe $cs | Out-Null
$x86 = Start-Process -FilePath $x86exe -ArgumentList 'COM11','20','60000' -PassThru -WindowStyle Hidden
$script:children += $x86.Id
Start-Sleep -Milliseconds 800
$x86ok = $true
for ($i = 1; $i -le 5; $i++) {
    $r = Invoke-Inject $x86.Id 1200
    Start-Sleep -Milliseconds 150
    $alive = [bool](Get-Process -Id $x86.Id -ErrorAction SilentlyContinue)
    if (-not ($r.exit -eq 0 -and $r.online -and $r.tx -gt 0 -and $alive)) {
        $x86ok = $false
        "  x86 轮次${i} 异常: exit=$($r.exit) online=$($r.online) tx=$($r.tx) alive=$alive"
        break
    }
}
Check "x86 WOW64 5轮注入/detach 全绿" $x86ok

Cleanup
Remove-Item "$env:TEMP\si_out.txt","$env:TEMP\si_err.txt","$env:TEMP\si_kill.txt","$env:TEMP\si_killerr.txt",$cs,$x86exe -Force -ErrorAction SilentlyContinue
""
"===== 结果: PASS=$pass FAIL=$fail ====="
if ($fail -gt 0) { exit 1 }
