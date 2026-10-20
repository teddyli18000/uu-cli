$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$pageUrl = 'https://uuyc.163.com/download/'
Write-Host "Fetching official UU download page: $pageUrl"
$response = Invoke-WebRequest -Uri $pageUrl -MaximumRedirection 8 -UseBasicParsing
if ($response.StatusCode -ne 200) {
    throw "Unexpected HTTP status: $($response.StatusCode)"
}

$html = [string]$response.Content
Write-Host "Page bytes: $([Text.Encoding]::UTF8.GetByteCount($html))"

$baseUri = [Uri]$pageUrl
$candidates = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)

# The official page currently exposes a stable NetEase redirect in its JSON-LD,
# e.g. downloadUrl=https://adl.netease.com/d/g/uuremote/c/gw?type=pc.
# Do not hard-code the resolved installer file itself; rediscover the public link each run.
$patterns = @(
    '(?i)https?://[^\s\"''<>]+?\.exe(?:\?[^\s\"''<>]*)?',
    '(?i)[\"'']downloadUrl[\"'']\s*:\s*[\"'']([^\"'']+)[\"'']',
    '(?i)(?:href|url|download_url)\s*[:=]\s*[\"'']([^\"'']+\.exe(?:\?[^\"'']*)?)[\"'']'
)

foreach ($pattern in $patterns) {
    foreach ($match in [regex]::Matches($html, $pattern)) {
        $raw = if ($match.Groups.Count -gt 1 -and $match.Groups[1].Success) { $match.Groups[1].Value } else { $match.Value }
        $raw = $raw.Trim('"', "'", ' ')
        try {
            $uri = [Uri]::new($baseUri, $raw)
            if ($uri.Scheme -in @('http', 'https')) {
                [void]$candidates.Add($uri.AbsoluteUri)
            }
        } catch {
            # Keep scanning; malformed unrelated JS strings are expected.
        }
    }
}

Write-Host "Download candidates: $($candidates.Count)"
$candidates | Sort-Object | ForEach-Object { Write-Host "  $_" }

if ($candidates.Count -eq 0) {
    throw 'Could not discover any download URL from the official page HTML.'
}

function Test-NetEaseHost([string]$url) {
    try {
        $hostName = ([Uri]$url).DnsSafeHost.ToLowerInvariant()
        return (
            $hostName -eq '163.com' -or $hostName.EndsWith('.163.com') -or
            $hostName -eq 'netease.com' -or $hostName.EndsWith('.netease.com')
        )
    } catch {
        return $false
    }
}

# Prefer exactly the Windows route advertised by the official page.
$preferred = $candidates |
    Where-Object {
        if (-not (Test-NetEaseHost $_)) { return $false }
        $uri = [Uri]$_
        $path = $uri.AbsolutePath.TrimEnd('/').ToLowerInvariant()
        $query = $uri.Query.TrimStart('?').ToLowerInvariant()
        return (
            $path.EndsWith('/uuremote/c/gw') -and
            (($query -split '&') -contains 'type=pc')
        )
    } |
    Sort-Object |
    Select-Object -First 1

if (-not $preferred) {
    throw 'Official page did not expose the NetEase Windows download endpoint (uuremote/c/gw?type=pc).'
}

Write-Host "Selected Windows endpoint: $preferred"
$installer = Join-Path $env:RUNNER_TEMP 'uuyc-installer.exe'
Invoke-WebRequest -Uri $preferred -OutFile $installer -MaximumRedirection 8 -UseBasicParsing

$hash = (Get-FileHash -Path $installer -Algorithm SHA256).Hash
$file = Get-Item $installer
$signature = Get-AuthenticodeSignature -FilePath $installer
$version = $file.VersionInfo.FileVersion
$product = $file.VersionInfo.ProductName
$subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { '<none>' }

Write-Host "Downloaded bytes: $($file.Length)"
Write-Host "SHA256: $hash"
Write-Host "Version: $version"
Write-Host "Product: $product"
Write-Host "Signature status: $($signature.Status)"
Write-Host "Signer: $subject"

if ($file.Length -lt 1MB) {
    throw "Downloaded payload is unexpectedly small: $($file.Length) bytes"
}
if ($signature.Status -ne 'Valid') {
    throw "Official installer candidate does not have a valid Authenticode signature: $($signature.Status)"
}
if (-not $signature.SignerCertificate) {
    throw 'Installer has no signer certificate.'
}

"installer_url=$preferred" >> $env:GITHUB_OUTPUT
"sha256=$hash" >> $env:GITHUB_OUTPUT
"version=$version" >> $env:GITHUB_OUTPUT
"product=$product" >> $env:GITHUB_OUTPUT
"signer=$subject" >> $env:GITHUB_OUTPUT
