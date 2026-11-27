param([string]$Binary = '.\target\debug\kb.exe')

$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path $Binary).Path
$name = "windows-smoke-$PID"
$second = "$name-second"
$folder = Join-Path ([System.IO.Path]::GetTempPath()) "kb-windows-smoke-$PID"
New-Item -ItemType Directory -Path $folder | Out-Null
$page = Join-Path $folder 'page # with spaces.html'
$shot = Join-Path $folder 'page.png'
$started = @()

function Invoke-Kb {
    param([string[]]$Arguments)
    $result = & $Binary @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "kb $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    return ($result -join "`n")
}

function Get-InstanceProcesses {
    param([string]$Instance)
    $root = @(Get-CimInstance Win32_Process -Filter "Name = 'kb.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains('"' + $Instance + '"') })
    if ($root.Count -ne 1) { throw "Expected one background process for $Instance, found $($root.Count)" }
    $ids = @([uint32]$root[0].ProcessId)
    $snapshot = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    do {
        $next = @($snapshot | Where-Object { $_.ParentProcessId -in $ids -and $_.ProcessId -notin $ids })
        if ($next.Count -gt 0) { $ids += @($next.ProcessId) }
    } while ($next.Count -gt 0)
    return $ids
}

function Wait-ForExit {
    param([uint32[]]$Ids)
    $deadline = (Get-Date).AddSeconds(5)
    do {
        $alive = @(foreach ($processId in $Ids) { Get-Process -Id $processId -ErrorAction SilentlyContinue })
        if ($alive.Count -eq 0) { return }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "Test processes did not exit: $($alive.Id -join ', ')"
}

try {
    @'
<!doctype html>
<meta charset="utf-8">
<title>Native Windows smoke test</title>
<h1>Native Windows</h1>
<input id="field">
<button id="button" onclick="document.querySelector('h1').textContent='Clicked'">Click</button>
'@ | Set-Content -LiteralPath $page -Encoding utf8

    Invoke-Kb @('--headless', '--name', $name, $page)
    $started += $name
    # An installed extension can open its welcome tab in a fresh Edge profile.
    Invoke-Kb @('ctl', '-i', $name, 'window', '1')
    Invoke-Kb @('ctl', '-i', $name, 'open', $page)
    $status = Invoke-Kb @('ctl', '-i', $name, 'status')
    if ($status -notmatch 'title: Native Windows smoke test' -or $status -notmatch 'loading: no') {
        throw "Unexpected status: $status"
    }
    Invoke-Kb @('ctl', '-i', $name, 'check')
    $unicode = 'Windows Unicode: caf' + [char]0xe9 + ' ' + [char]::ConvertFromUtf32(0x1f43e)
    Invoke-Kb @('ctl', '-i', $name, 'type', '#field', $unicode, '--clear')
    $value = Invoke-Kb @('ctl', '-i', $name, 'text', '#field')
    if ($value -notmatch [regex]::Escape($unicode)) { throw "Unicode input failed: $value" }
    Invoke-Kb @('ctl', '-i', $name, 'click', '#button')
    if ((Invoke-Kb @('ctl', '-i', $name, 'text', 'h1')) -notmatch 'Clicked') { throw 'Click did not land' }
    Invoke-Kb @('ctl', '-i', $name, 'viewport', '375x800')
    Invoke-Kb @('ctl', '-i', $name, 'screenshot', '--out', $shot)
    $bytes = [System.IO.File]::ReadAllBytes($shot)
    if ($bytes.Length -lt 24 -or $bytes[0] -ne 137 -or $bytes[1] -ne 80) { throw 'Screenshot is not a PNG' }
    if ([System.Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($bytes, 16)) -ne 375) {
        throw 'Screenshot did not use the requested viewport'
    }
    Invoke-Kb @('--headless', '--name', $second, 'about:blank')
    $started += $second
    if ((Invoke-Kb @('list')) -notmatch [regex]::Escape($second)) { throw 'Second instance was not listed' }
    $processes = @(Get-InstanceProcesses $second)
    if ($processes.Count -lt 2) { throw 'No browser child was found' }
    Stop-Process -Id $processes[0]
    Wait-ForExit $processes
    Invoke-Kb @('--headless', '--name', $second, 'about:blank')
    & $Binary ctl -i $name click '#missing' 2>$null
    if ($LASTEXITCODE -eq 0) { throw 'Missing elements must fail' }

    Write-Output 'Native Windows smoke test passed: IPC, local paths, Unicode input, clicks, viewport, screenshots, multiple instances, crash cleanup, restart and failure reporting.'
} finally {
    try {
        foreach ($instance in $started) {
            $processes = @(Get-InstanceProcesses $instance)
            & $Binary ctl -i $instance quit
            if ($LASTEXITCODE -ne 0) { throw "Could not stop test instance $instance" }
            Wait-ForExit $processes
        }
    } finally {
        # Delete only this test's explicitly named files and empty directory.
        foreach ($file in @($page, $shot)) {
            if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file }
        }
        Remove-Item -LiteralPath $folder
    }
}
