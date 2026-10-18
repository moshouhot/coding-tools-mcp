# Default is read-only. Use -Apply only after approval for this HKLM value.
[CmdletBinding()]
param([switch]$Apply)

$ErrorActionPreference = 'Stop'
$subkey = 'SOFTWARE\Microsoft\EdgeUpdate\ClientState\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$expectedOld = 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application\103.0.1264.77'
$target = 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application\154.0.4258.53'
$base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine,
    [Microsoft.Win32.RegistryView]::Registry32
)
try {
    $key = $base.OpenSubKey($subkey, $Apply.IsPresent)
    if ($null -eq $key) { throw 'Expected WebView2 ClientState key was not found.' }
    try {
        $old = $key.GetValue('EBWebView', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $kind = $key.GetValueKind('EBWebView')
        if ($old -ne $expectedOld -and $old -ne $target) {
            throw 'EBWebView changed since diagnosis; stop and inspect the new state.'
        }
        if ($key.GetValue('pv') -ne '154.0.4258.53') { throw 'Installed version changed; stop.' }
        foreach ($relative in @('msedgewebview2.exe', 'icudtl.dat', 'msedge.dll', 'EBWebView\x64\EmbeddedBrowserWebView.dll')) {
            if (-not (Test-Path -LiteralPath (Join-Path $target $relative) -PathType Leaf)) {
                throw "Target Runtime is incomplete: $relative"
            }
        }
        $exe = Join-Path $target 'msedgewebview2.exe'
        if ((Get-Item -LiteralPath $exe).VersionInfo.ProductVersion -ne '154.0.4258.53') {
            throw 'Target executable version does not match.'
        }
        if ((Get-AuthenticodeSignature -LiteralPath $exe).Status -ne 'Valid') {
            throw 'Target executable signature is invalid.'
        }
        [pscustomobject]@{
            RegistryView = '32-bit HKLM'
            Key = $subkey
            Value = 'EBWebView'
            Before = $old
            After = $target
            Apply = $Apply.IsPresent
        } | Format-List
        if (-not $Apply -or $old -eq $target) { return }
        $backupDirectory = Join-Path (Split-Path $PSScriptRoot -Parent) 'tmp-run\tray-window\backup'
        New-Item -ItemType Directory -Path $backupDirectory -Force | Out-Null
        $backupFile = Join-Path $backupDirectory ('webview2-EBWebView-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '.json')
        @{
            hive = 'HKLM'; view = 'Registry32'; subkey = $subkey
            name = 'EBWebView'; value = $old; kind = $kind.ToString(); replacement = $target
        } | ConvertTo-Json | Set-Content -LiteralPath $backupFile -Encoding utf8
        # Read the backup back before changing the single approved registry value.
        if ((Get-Content -Raw -LiteralPath $backupFile | ConvertFrom-Json).value -ne $old) {
            throw 'Backup verification failed.'
        }
        $key.SetValue('EBWebView', $target, $kind)
        $key.Flush()
        if ($key.GetValue('EBWebView') -ne $target) { throw 'Registry readback failed.' }
        Write-Output "Changed only EBWebView. Backup: $backupFile"
    } finally { $key.Dispose() }
} finally { $base.Dispose() }
