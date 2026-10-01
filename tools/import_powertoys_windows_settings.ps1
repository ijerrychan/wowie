param(
    [Parameter(Mandatory = $true)]
    [string]$PowerToysPluginPath
)

$ErrorActionPreference = 'Stop'
$destination = Join-Path $PSScriptRoot '..\src\providers\windows_settings_data'
$destination = [System.IO.Path]::GetFullPath($destination)
$sourceFiles = @(
    'WindowsSettings.json',
    'WindowsSettings.schema.json'
)

foreach ($file in $sourceFiles) {
    Copy-Item -LiteralPath (Join-Path $PowerToysPluginPath $file) `
        -Destination (Join-Path $destination $file) -Force
}

$resourcePath = Join-Path $PowerToysPluginPath 'Properties\Resources.resx'
Copy-Item -LiteralPath $resourcePath `
    -Destination (Join-Path $destination 'Resources.en.resx') -Force

[xml]$resources = Get-Content -Raw -Encoding UTF8 -LiteralPath $resourcePath
$strings = [ordered]@{}
foreach ($entry in $resources.SelectNodes('/root/data')) {
    $value = $entry.SelectSingleNode('value')
    if ($null -ne $value) {
        $strings[$entry.GetAttribute('name')] = $value.InnerText
    }
}

$json = $strings | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText(
    (Join-Path $destination 'Resources.en.json'),
    $json,
    [System.Text.UTF8Encoding]::new($false)
)

$settings = Get-Content -Raw -Encoding UTF8 `
    -LiteralPath (Join-Path $destination 'WindowsSettings.json') | ConvertFrom-Json
$localized = Get-Content -Raw -Encoding UTF8 `
    -LiteralPath (Join-Path $destination 'Resources.zh-CN.json') | ConvertFrom-Json
$microsoftLearnLocalized = Get-Content -Raw -Encoding UTF8 `
    -LiteralPath (Join-Path $destination 'Resources.zh-CN.microsoft-learn.json') | ConvertFrom-Json
$english = Get-Content -Raw -Encoding UTF8 `
    -LiteralPath (Join-Path $destination 'Resources.en.json') | ConvertFrom-Json
$requiredKeys = @(
    $settings.Settings | ForEach-Object { $_.Name; $_.Type; $_.Areas }
) | Where-Object { $_ } | Sort-Object -Unique
$missing = @($requiredKeys | Where-Object { $null -eq $localized.PSObject.Properties[$_] })
if ($missing.Count -gt 0) {
    throw "Resources.zh-CN.json is missing upstream setting translations: $($missing -join ', ')"
}

$unknownMicrosoftLearnKeys = @(
    $microsoftLearnLocalized.PSObject.Properties.Name |
        Where-Object { $null -eq $english.PSObject.Properties[$_] }
)
if ($unknownMicrosoftLearnKeys.Count -gt 0) {
    throw "Resources.zh-CN.microsoft-learn.json contains keys absent from upstream English resources: $($unknownMicrosoftLearnKeys -join ', ')"
}
