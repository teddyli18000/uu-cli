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

function Get-AbsoluteUrls([string]$text, [Uri]$relativeTo) {
    $set = New-Object System.Collections.Generic.HashSet[string] ([StringComparer]::OrdinalIgnoreCase)
    foreach ($match in [regex]::Matches($text, '(?i)https?://[^\s\"''<>]+')) {
        $raw = $match.Value.TrimEnd(')', ']', '}', ',', ';')
        try {
            $uri = [Uri]::new($relativeTo, $raw)
            if ($uri.Scheme -in @('http', 'https')) {
                [void]$set.Add($uri.AbsoluteUri)
            }
        } catch {
        }
    }
    return @($set)
}

function Save-IfPe([byte[]]$bytes, [string]$destination) {
    if ($bytes.Length -ge 2 -and $bytes[0] -eq 0x4D -and $bytes[1] -eq 0x5A) {
        [IO.File]::WriteAllBytes($destination, $bytes)
        return $true
    }
    return $false
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
$downloadResponse = Invoke-WebRequest -Uri $preferred -MaximumRedirection 8 -UseBasicParsing
$finalUri = $downloadResponse.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
$contentType = [string]$downloadResponse.Headers['Content-Type']
$contentDisposition = [string]$downloadResponse.Headers['Content-Disposition']
Write-Host "Endpoint final URI: $finalUri"
Write-Host "Endpoint content type: $contentType"
Write-Host "Endpoint content disposition: $contentDisposition"

$payloadBytes = $downloadResponse.RawContentStream.ToArray()
$installer = Join-Path $env:RUNNER_TEMP 'uuyc-installer.exe'
$resolvedInstallerUrl = $null

if (Save-IfPe $payloadBytes $installer) {
    $resolvedInstallerUrl = $finalUri
} else {
    $landing = [Text.Encoding]::UTF8.GetString($payloadBytes)
    Write-Host "Endpoint returned HTML/text bytes: $($payloadBytes.Length)"

    $landingUrls = @(Get-AbsoluteUrls $landing ([Uri]$finalUri))
    Write-Host "Landing page URLs: $($landingUrls.Count)"
    $landingUrls | Sort-Object | ForEach-Object { Write-Host "  $_" }

    # The official landing page currently points at NetEase's release API. Probe those
    # trusted endpoints first. They may redirect to a binary or return JSON/text that
    # contains the versioned CDN URL. Nothing is executed during discovery.
    $releaseApis = $landingUrls |
        Where-Object {
            (Test-NetEaseHost $_) -and ([Uri]$_).AbsolutePath.ToLowerInvariant().Contains('/api/v1/release/dl/')
        } |
        Sort-Object

    foreach ($apiUrl in $releaseApis) {
        Write-Host "Probing official release API: $apiUrl"
        try {
            $apiResponse = Invoke-WebRequest -Uri $apiUrl -MaximumRedirection 8 -UseBasicParsing
            $apiFinalUri = $apiResponse.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
            $apiType = [string]$apiResponse.Headers['Content-Type']
            $apiBytes = $apiResponse.RawContentStream.ToArray()
            Write-Host "  final URI: $apiFinalUri"
            Write-Host "  content type: $apiType"
            Write-Host "  bytes: $($apiBytes.Length)"

            if (Save-IfPe $apiBytes $installer) {
                $resolvedInstallerUrl = $apiFinalUri
                break
            }

            $apiText = [Text.Encoding]::UTF8.GetString($apiBytes)
            $apiUrls = @(Get-AbsoluteUrls $apiText ([Uri]$apiFinalUri))
            Write-Host "  response URLs: $($apiUrls.Count)"
            $apiUrls | Sort-Object | ForEach-Object { Write-Host "    $_" }

            foreach ($childUrl in $apiUrls) {
                Write-Host "  probing release child: $childUrl"
                try {
                    $child = Invoke-WebRequest -Uri $childUrl -MaximumRedirection 8 -UseBasicParsing
                    $childFinal = $child.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
                    $childBytes = $child.RawContentStream.ToArray()
                    Write-Host "    final URI: $childFinal"
                    Write-Host "    bytes: $($childBytes.Length)"
                    if (Save-IfPe $childBytes $installer) {
                        $resolvedInstallerUrl = $childFinal
                        break
                    }
                } catch {
                    Write-Host "    probe failed: $($_.Exception.Message)"
                }
            }
            if ($resolvedInstallerUrl) { break }

            $excerpt = ($apiText -replace '\s+', ' ')
            if ($excerpt.Length -gt 1200) { $excerpt = $excerpt.Substring(0, 1200) }
            Write-Host "  response excerpt: $excerpt"
        } catch {
            Write-Host "  API probe failed: $($_.Exception.Message)"
        }
    }

    if (-not $resolvedInstallerUrl) {
        $excerpt = ($landing -replace '\s+', ' ')
        if ($excerpt.Length -gt 2500) { $excerpt = $excerpt.Substring(0, 2500) }
        Write-Host "Landing excerpt: $excerpt"
        throw 'Official Windows endpoint was found, but the signed installer could not yet be resolved safely.'
    }
}

Write-Host "Resolved installer URL: $resolvedInstallerUrl"
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

"installer_url=$resolvedInstallerUrl" >> $env:GITHUB_OUTPUT
"sha256=$hash" >> $env:GITHUB_OUTPUT
"version=$version" >> $env:GITHUB_OUTPUT
"product=$product" >> $env:GITHUB_OUTPUT
"signer=$subject" >> $env:GITHUB_OUTPUT
