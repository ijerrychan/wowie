# Run only while Cargo/rustc and the application are stopped when cleaning.
[CmdletBinding(SupportsShouldProcess = $true)]
param([switch]$CleanIncremental)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = [System.IO.Path]::GetFullPath((Join-Path $projectRoot 'target'))
if (-not (Test-Path -LiteralPath $targetRoot)) {
    Write-Output 'No local target directory.'
    return
}

# Reject junctions/symlinks rather than following them outside this project.
function Get-SafeChildren([string]$Path) {
    $resolved = [System.IO.Path]::GetFullPath($Path)
    if ($resolved -ne $targetRoot -and
        -not $resolved.StartsWith($targetRoot + [System.IO.Path]::DirectorySeparatorChar,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Path is outside the local target directory: $resolved"
    }
    $item = Get-Item -LiteralPath $resolved -Force
    if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw "Refusing to follow a reparse point: $resolved"
    }
    $children = @(Get-ChildItem -LiteralPath $resolved -Force)
    foreach ($child in $children) {
        if ($child.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw "Refusing to follow a reparse point: $($child.FullName)"
        }
    }
    return $children
}

function Get-SafeTreeBytes([string]$Path) {
    [long]$bytes = 0
    foreach ($child in (Get-SafeChildren $Path)) {
        if ($child.PSIsContainer) {
            $bytes += Get-SafeTreeBytes $child.FullName
        } else {
            $bytes += $child.Length
        }
    }
    return $bytes
}

$topLevel = @(Get-SafeChildren $targetRoot)
$report = foreach ($item in $topLevel) {
    $bytes = if ($item.PSIsContainer) { Get-SafeTreeBytes $item.FullName } else { $item.Length }
    [PSCustomObject]@{ Name = $item.Name; Bytes = $bytes; GiB = [math]::Round($bytes / 1GB, 3) }
}
$report | Sort-Object Bytes -Descending | Format-Table -AutoSize

if ($CleanIncremental) {
    # Cargo stores host caches in target/<profile>/incremental and cross-target
    # caches in target/<triple>/<profile>/incremental. Never remove other outputs.
    $profiles = @($topLevel | Where-Object PSIsContainer)
    foreach ($directory in @($profiles)) {
        $profiles += @(Get-SafeChildren $directory.FullName | Where-Object PSIsContainer)
    }
    foreach ($profile in $profiles) {
        $cache = Join-Path $profile.FullName 'incremental'
        if (-not (Test-Path -LiteralPath $cache)) { continue }
        $bytes = Get-SafeTreeBytes $cache
        if ($PSCmdlet.ShouldProcess($cache, "Remove incremental cache ($bytes bytes)")) {
            Remove-Item -LiteralPath $cache -Recurse -Force
        }
    }
}
