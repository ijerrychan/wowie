[CmdletBinding()]
param(
    [switch]$UpdateWindowsSettings,
    [string]$SettingsRef = 'latest',
    [ValidateSet('dev', 'release')]
    [string]$Profile = 'release'
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location -LiteralPath $projectRoot
try {
    if ($UpdateWindowsSettings) {
        & python (Join-Path $PSScriptRoot 'update_windows_settings.py') --ref $SettingsRef
        if ($LASTEXITCODE -ne 0) {
            throw 'Settings update failed; build was not started. Vendored data remains the fallback for plain cargo builds.'
        }
    }
    & cargo build --locked --profile $Profile
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed with exit code $LASTEXITCODE." }
} finally {
    Pop-Location
}
