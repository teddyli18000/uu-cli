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

# Absolute and relative executable links embedded directly in HTML/JS.
$patterns = @(
    '(?i)https?://[^\s\"''<>]+?\.exe(?:\?[^\s\"''<>]*)?',
    '(?i)(?:href|url|downloadUrl|download_url)\s*[:=]\s*[\"'']([^\"'']+\.exe(?:\?[^\"'']*)?)[\"'']'
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

Write-Host "Executable candidates: $($candidates.Count)"
$candidates | Sort-Object | ForEach-Object { Write-Host "  $_" }

if ($candidates.Count -eq 0) {
    Write-Host 'No direct .exe URL was present in the initial HTML.'
    Write-Host 'Download-related excerpts:'
    [regex]::Matches($html, '(?is).{0,100}(?:windows|download|下载|exe).{0,180}') |
        Select-Object -First 20 |
        ForEach-Object { Write-Host ($_.Value -replace '\s+', ' ') }
    throw 'Could not discover a Windows installer URL from the official page HTML.'
}

# Prefer candidates still under NetEase-controlled domains. Authenticode remains the final binary check.
$preferred = $candidates |
    Where-Object {
        try {
            $hostName = ([Uri]$_).DnsSafeHost.ToLowerInvariant()
            $hostName -eq '163.com' -or $hostName.EndsWith('.163.com') -or
            $hostName -eq 'netease.com' -or $hostName.EndsWith('.netease.com')
        } catch { $false }
    } |
    Sort-Object |
    Select-Object -First 1

if (-not $preferred) {
    throw 'Found executable links, but none are on a recognized NetEase-controlled domain.'
}

Write-Host "Selected candidate: $preferred"
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

if ($signature.Status -ne 'Valid') {
    throw "Official installer candidate does not have a valid Authenticode signature: $($signature.Status)"
}

"installer_url=$preferred" >> $env:GITHUB_OUTPUT
"sha256=$hash" >> $env:GITHUB_OUTPUT
"version=$version" >> $env:GITHUB_OUTPUT
