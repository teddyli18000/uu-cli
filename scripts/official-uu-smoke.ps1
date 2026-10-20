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
        }
    }
}

Write-Host "Download candidates: $($candidates.Count)"
$candidates | Sort-Object | ForEach-Object { Write-Host "  $_" }

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

# The public adl endpoint can itself be a small HTML redirect/landing page. Resolve it
# without executing anything, then only accept a signed PE as the installer.
$downloadResponse = Invoke-WebRequest -Uri $preferred -MaximumRedirection 8 -UseBasicParsing
$finalUri = $downloadResponse.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
$contentType = [string]$downloadResponse.Headers['Content-Type']
$contentDisposition = [string]$downloadResponse.Headers['Content-Disposition']
Write-Host "Endpoint final URI: $finalUri"
Write-Host "Endpoint content type: $contentType"
Write-Host "Endpoint content disposition: $contentDisposition"

$payloadBytes = $downloadResponse.RawContentStream.ToArray()
$installer = Join-Path $env:RUNNER_TEMP 'uuyc-installer.exe'

# MZ means we already have a PE installer.
$isPe = $payloadBytes.Length -ge 2 -and $payloadBytes[0] -eq 0x4D -and $payloadBytes[1] -eq 0x5A
if ($isPe) {
    [IO.File]::WriteAllBytes($installer, $payloadBytes)
} else {
    $landing = [Text.Encoding]::UTF8.GetString($payloadBytes)
    Write-Host "Endpoint returned HTML/text bytes: $($payloadBytes.Length)"

    $landingUrls = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
    foreach ($match in [regex]::Matches($landing, '(?i)https?://[^\s\"''<>]+')) {
        $raw = $match.Value.TrimEnd(')', ']', '}', ',', ';')
        try {
            $uri = [Uri]$raw
            if ($uri.Scheme -in @('http', 'https')) {
                [void]$landingUrls.Add($uri.AbsoluteUri)
            }
        } catch {
        }
    }

    Write-Host "Landing page URLs: $($landingUrls.Count)"
    $landingUrls | Sort-Object | ForEach-Object { Write-Host "  $_" }

    $binaryCandidates = $landingUrls |
        Where-Object {
            if (-not (Test-NetEaseHost $_)) { return $false }
            $u = [Uri]$_
            $s = $u.AbsoluteUri.ToLowerInvariant()
            return $s.Contains('uuremote') -or $s.EndsWith('.exe') -or $s.Contains('.exe?')
        } |
        Sort-Object

    $resolved = $null
    foreach ($candidate in $binaryCandidates) {
        Write-Host "Probing landing candidate: $candidate"
        try {
            $probe = Invoke-WebRequest -Uri $candidate -MaximumRedirection 8 -UseBasicParsing
            $bytes = $probe.RawContentStream.ToArray()
            if ($bytes.Length -ge 2 -and $bytes[0] -eq 0x4D -and $bytes[1] -eq 0x5A) {
                $resolved = $candidate
                [IO.File]::WriteAllBytes($installer, $bytes)
                break
            }
        } catch {
            Write-Host "  probe failed: $($_.Exception.Message)"
        }
    }

    if (-not $resolved) {
        $excerpt = ($landing -replace '\s+', ' ')
        if ($excerpt.Length -gt 2500) { $excerpt = $excerpt.Substring(0, 2500) }
        Write-Host "Landing excerpt: $excerpt"
        throw 'Official Windows endpoint returned a landing page, but no signed installer URL could yet be resolved from it.'
    }

    Write-Host "Resolved installer candidate: $resolved"
}

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
