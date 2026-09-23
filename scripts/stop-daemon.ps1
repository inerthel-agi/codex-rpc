param([string]$LockPath = (Join-Path $env:LOCALAPPDATA 'codex-rich-presence\instance.lock'))

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $LockPath)) {
    Write-Host 'No running instance found.'
    exit 0
}

try {
    $raw = (Get-Content -Raw -LiteralPath $LockPath).Trim()
    $record = $null
    try {
        $record = ConvertFrom-Json -InputObject $raw -ErrorAction Stop
        if ($record -is [pscustomobject]) { $candidate = $record.pid }
        else { $record = $null; $candidate = $raw }
    }
    catch { $candidate = $raw } # Legacy plain-PID locks.
    $daemonPid = 0
    if (-not [int]::TryParse([string]$candidate, [ref]$daemonPid) -or $daemonPid -le 0) {
        throw 'Invalid PID in instance.lock.'
    }

    $daemon = Get-CimInstance Win32_Process -Filter "ProcessId=$daemonPid"
    $entry = Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\index.js'
    if ($null -eq $daemon -or
        $null -eq $daemon.CommandLine -or
        $daemon.CommandLine.IndexOf($entry, [StringComparison]::OrdinalIgnoreCase) -lt 0 -or
        $daemon.CommandLine.Contains('--status')) {
        throw 'Lock PID does not identify this daemon.'
    }
    if ($null -ne $record -and
        ($daemon.ExecutablePath -ne $record.exe -or
         [Math]::Abs(([DateTimeOffset]$daemon.CreationDate).ToUnixTimeMilliseconds() -
             [double]$record.startTimeMs) -gt 30000)) {
        throw 'Lock process identity does not match.'
    }

    Stop-Process -Id $daemonPid -Force
    Remove-Item -LiteralPath $LockPath -Force
    Write-Host 'Stopped.'
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
