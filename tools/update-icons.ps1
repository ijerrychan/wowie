param(
    [string]$ManifestPath = "assets/icons/iconify.json"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifestFullPath = (Resolve-Path (Join-Path $repoRoot $ManifestPath)).Path
$manifest = Get-Content -LiteralPath $manifestFullPath -Raw -Encoding utf8 | ConvertFrom-Json
if ($manifest.version -ne 1 -or $manifest.icons.Count -eq 0) {
    throw "Unsupported or empty Iconify manifest."
}

$stageRoot = Join-Path $repoRoot ("assets/icons/.iconify-stage-" + [guid]::NewGuid().ToString("N"))
$downloads = Join-Path $stageRoot "downloads"
$backups = Join-Path $stageRoot "backups"
New-Item -ItemType Directory -Path $downloads, $backups | Out-Null
$prepared = [System.Collections.Generic.List[object]]::new()
$replaced = [System.Collections.Generic.List[object]]::new()
$cleanupStage = $false

function Test-IconifySvg([byte[]]$Bytes) {
    if ($Bytes.Length -eq 0 -or $Bytes.Length -gt 65536) { throw "SVG size is outside the 1..65536 byte limit." }
    $settings = [System.Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [System.Xml.DtdProcessing]::Prohibit
    $settings.XmlResolver = $null
    $stream = [System.IO.MemoryStream]::new($Bytes)
    try {
        $reader = [System.Xml.XmlReader]::Create($stream, $settings)
        try {
            $document = [System.Xml.XmlDocument]::new()
            $document.XmlResolver = $null
            $document.Load($reader)
        } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
    $root = $document.DocumentElement
    if ($root.LocalName -ne "svg" -or $root.NamespaceURI -ne "http://www.w3.org/2000/svg") {
        throw "Response is not an SVG document."
    }
    foreach ($node in $document.SelectNodes("//*")) {
        if ($node.LocalName -in @("script", "foreignObject", "iframe", "object", "embed")) {
            throw "SVG contains a forbidden active/external element: $($node.LocalName)."
        }
        foreach ($attribute in $node.Attributes) {
            if ($attribute.Name -match "^on") { throw "SVG contains an event handler attribute." }
            if ($attribute.LocalName -in @("href", "src") -and $attribute.Value -notmatch "^#") {
                throw "SVG contains an external resource reference."
            }
            if ($attribute.Value -match 'url\(\s*.*https?:') {
                throw "SVG contains an external URL reference."
            }
        }
    }
    foreach ($style in $document.GetElementsByTagName("style", "http://www.w3.org/2000/svg")) {
        if ($style.InnerText -match "@import|url\(\s*.*https?:") {
            throw "SVG contains external CSS content."
        }
    }
}

try {
    $seenFiles = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $manifest.icons) {
        $relative = ([string]$entry.file).Replace("\", "/")
        if ($relative -notmatch "^(assets/icons|plugins/google-translate/icons)/[^/]+\.svg$") {
            throw "Icon file is outside the allowed SVG directories: $relative"
        }
        if (-not $seenFiles.Add($relative)) { throw "Duplicate manifest target: $relative" }
        if ($entry.icon -notmatch "^[a-z0-9-]+:[a-z0-9-]+$") { throw "Invalid Iconify ID: $($entry.icon)" }
        if ($entry.color -and $entry.color -notmatch "^#[0-9a-fA-F]{6}$") { throw "Invalid color for $relative" }
        if (-not $entry.license) { throw "Missing license for $relative" }

        $uri = "https://api.iconify.design/$($entry.icon.Replace(':', '/')).svg"
        if ($entry.color) { $uri += "?color=%23$($entry.color.Substring(1))" }
        if ($entry.source -ne $uri) { throw "Source URL does not match the declared Iconify ID/color for $relative" }
        $response = Invoke-WebRequest -Uri $entry.source -UseBasicParsing -TimeoutSec 30 -Headers @{ Accept = "image/svg+xml" }
        $bytes = [System.Text.Encoding]::UTF8.GetBytes([string]$response.Content)
        Test-IconifySvg $bytes

        $target = Join-Path $repoRoot $relative.Replace("/", [IO.Path]::DirectorySeparatorChar)
        $targetParent = (Resolve-Path (Split-Path -Parent $target)).Path
        $targetRoot = if ($relative.StartsWith("assets/")) { Join-Path $repoRoot "assets/icons" } else { Join-Path $repoRoot "plugins/google-translate/icons" }
        if (-not $targetParent.StartsWith((Resolve-Path $targetRoot).Path, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Resolved target escaped its allowed icon directory: $relative"
        }
        $stageFile = Join-Path $downloads ([guid]::NewGuid().ToString("N") + ".svg")
        [System.IO.File]::WriteAllBytes($stageFile, $bytes)
        $prepared.Add([pscustomobject]@{ Relative = $relative; Target = $target; Stage = $stageFile })
        Write-Host "Validated $($entry.icon) -> $relative ($($bytes.Length) bytes)"
    }

    foreach ($directory in @("assets/icons", "plugins/google-translate/icons")) {
        $absoluteDirectory = Join-Path $repoRoot $directory
        foreach ($file in Get-ChildItem -LiteralPath $absoluteDirectory -File -Filter "*.svg") {
            $relative = $file.FullName.Substring($repoRoot.Length + 1).Replace("\", "/")
            if (-not $seenFiles.Contains($relative)) { throw "SVG is missing from the Iconify manifest: $relative" }
        }
    }

    # All requests and validations complete before the first target is changed.
    foreach ($item in $prepared) {
        $backup = Join-Path $backups ([guid]::NewGuid().ToString("N") + ".svg")
        if ([System.IO.File]::Exists($item.Target)) {
            [System.IO.File]::Replace($item.Stage, $item.Target, $backup, $true)
        } else {
            [System.IO.File]::Move($item.Stage, $item.Target)
            $backup = $null
        }
        $replaced.Add([pscustomobject]@{ Target = $item.Target; Backup = $backup })
    }
    Write-Host "Updated $($prepared.Count) Iconify SVG files."
    $cleanupStage = $true
} catch {
    $rollbackErrors = [System.Collections.Generic.List[string]]::new()
    for ($index = $replaced.Count - 1; $index -ge 0; $index--) {
        $entry = $replaced[$index]
        try {
            if ($entry.Backup -and [System.IO.File]::Exists($entry.Backup)) {
                [System.IO.File]::Replace($entry.Backup, $entry.Target, $null, $true)
            } elseif ([System.IO.File]::Exists($entry.Target)) {
                [System.IO.File]::Delete($entry.Target)
            }
        } catch {
            $rollbackErrors.Add("$($entry.Target): $($_.Exception.Message)")
        }
    }
    if ($rollbackErrors.Count -eq 0) { $cleanupStage = $true }
    else { Write-Warning "Rollback was incomplete; staging backups are preserved at $stageRoot. $($rollbackErrors -join '; ')" }
    throw
} finally {
    if ($cleanupStage -and [System.IO.Directory]::Exists($stageRoot)) {
        $resolvedStage = [System.IO.Path]::GetFullPath($stageRoot)
        $allowedStageParent = [System.IO.Path]::GetFullPath((Join-Path $repoRoot "assets/icons")) + [System.IO.Path]::DirectorySeparatorChar
        $stageName = [System.IO.Path]::GetFileName($resolvedStage)
        if (-not $resolvedStage.StartsWith($allowedStageParent, [System.StringComparison]::OrdinalIgnoreCase) -or $stageName -notmatch "^\.iconify-stage-[a-f0-9]{32}$") {
            throw "Refusing to remove an unexpected staging directory: $resolvedStage"
        }
        [System.IO.Directory]::Delete($resolvedStage, $true)
    }
}
