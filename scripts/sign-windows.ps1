<#
.SYNOPSIS
  Authenticode-signs Windows release binaries (mogen.exe, mogen-studio.exe and the
  WiX .msi installer), using either a PFX/PKCS#12 file or a certificate
  already present in the Windows store (hardware token / cloud virtual card).

.DESCRIPTION
  Called by .github/workflows/release.yml. Everything is driven by env vars so
  no secret ever appears on a command line (and therefore never in a log).

  Two credential modes, checked in this order:

    WINDOWS_CERT_SHA1         thumbprint of a certificate in
                              Cert:\CurrentUser\My — signs via the store, so
                              the private key never has to be exportable. This
                              is the mode used with Certum's cloud key after
                              scripts/simplysign-login.ps1 has mounted it (that
                              script sets this variable), and it equally covers
                              a USB token on a self-hosted runner.

    WINDOWS_CERT_PFX_BASE64   base64 of a code-signing .pfx, with
    WINDOWS_CERT_PASSWORD     its password (optional). Public CAs stopped
                              issuing exportable keys in June 2023, so in
                              practice this is for self-signed rehearsal certs
                              and pre-2023 certificates.

    WINDOWS_TIMESTAMP_URL     RFC-3161 timestamp server            (optional,
                              defaults to http://timestamp.digicert.com;
                              Certum certificates want http://time.certum.pl)

  Local invocations remain permissive when no credential is configured. Pass
  -Required (as the tagged-release workflow does) to fail when credentials,
  targets, signing, timestamping, or trusted Authenticode verification fail.

  Timestamping is not optional: without it every signature stops validating the
  day the certificate expires. The timestamp server is the flakiest part of the
  process, so it is retried a few times before giving up.

.PARAMETER Path
  One or more files to sign. Wildcards are allowed
  (e.g. `target\x86_64-pc-windows-msvc\release\*.exe`). Files that already carry a
  valid signature (bundled, vendor-signed DLLs) are left alone.

.PARAMETER Required
  Enables fail-closed release behavior. Missing credentials/targets and failed
  trust verification terminate with a non-zero exit code.

.EXAMPLE
  ./scripts/sign-windows.ps1 target/x86_64-pc-windows-msvc/release/mogen.exe
#>
[CmdletBinding()]
param(
    [switch]$Required,
    [Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)]
    [string[]]$Path
)

$ErrorActionPreference = 'Stop'
# PowerShell 7.4+ turns a non-zero exit code from a native command into a
# terminating error while $ErrorActionPreference is 'Stop'. We want to inspect
# $LASTEXITCODE ourselves (retry the timestamp, treat `verify` as advisory), so
# opt out. Harmless on Windows PowerShell 5.1, where the variable is unused.
$PSNativeCommandUseErrorActionPreference = $false

# Thumbprints are copied out of certmgr, which pads them with spaces and an
# invisible left-to-right mark; normalise before comparing or passing to
# signtool, which wants bare hex.
$certSha1 = ($env:WINDOWS_CERT_SHA1 -replace '[^0-9a-fA-F]', '')
$useStore = -not [string]::IsNullOrWhiteSpace($certSha1)
$usePfx = -not [string]::IsNullOrWhiteSpace($env:WINDOWS_CERT_PFX_BASE64)

if (-not $useStore -and -not $usePfx) {
    $message = "Neither WINDOWS_CERT_SHA1 nor WINDOWS_CERT_PFX_BASE64 is set."
    if ($Required) { throw $message }
    Write-Host "::warning::$message Skipping optional local signing."
    exit 0
}

$timestampUrl = $env:WINDOWS_TIMESTAMP_URL
if ([string]::IsNullOrWhiteSpace($timestampUrl)) {
    $timestampUrl = 'http://timestamp.digicert.com'
}

# signtool.exe ships with the Windows SDK; its path is version-stamped and the
# runner image can carry several SDKs at once, so pick the newest x64 build
# rather than hardcoding a version that a future image bump would break.
function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $roots = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "$env:ProgramFiles\Windows Kits\10\bin"
    ) | Where-Object { $_ -and (Test-Path $_) }

    $found = foreach ($root in $roots) {
        Get-ChildItem -Path $root -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' }
    }
    # Sort on the parsed SDK version (…\bin\10.0.22621.0\x64\signtool.exe), not
    # on the string: lexically "10.0.9" beats "10.0.22621".
    $best = $found | Sort-Object -Property @{ Expression = {
        $name = $_.Directory.Parent.Name
        $parsed = $null
        if ([version]::TryParse($name, [ref]$parsed)) { $parsed } else { [version]'0.0.0.0' }
    } } -Descending | Select-Object -First 1
    if (-not $best) { throw "signtool.exe not found (is the Windows SDK installed on this runner?)" }
    return $best.FullName
}

# Resolve the requested paths (wildcards included) down to concrete files, and
# drop anything that is already validly signed so we don't strip a third-party
# vendor's signature off a prebuilt DLL we merely redistribute.
$targets = @()
$missingPatterns = @()
foreach ($pattern in $Path) {
    $items = @(Get-Item -Path $pattern -ErrorAction SilentlyContinue)
    if ($items.Count -eq 0) {
        $missingPatterns += $pattern
        continue
    }
    foreach ($item in $items) {
        if ($item.PSIsContainer) { continue }
        $existing = Get-AuthenticodeSignature -FilePath $item.FullName -ErrorAction SilentlyContinue
        if ($existing -and $existing.Status -eq 'Valid') {
            Write-Host "Already signed, skipping: $($item.Name)"
            continue
        }
        $targets += $item.FullName
    }
}

if ($missingPatterns.Count -gt 0) {
    $message = "No file matched required signing pattern(s): $($missingPatterns -join ', ')"
    if ($Required) { throw $message }
    Write-Host "::warning::$message"
}

if ($targets.Count -eq 0) {
    if ($missingPatterns.Count -eq $Path.Count -and $Required) {
        throw "No requested Windows signing targets exist."
    }
    Write-Host "Nothing to sign (all existing targets already have valid signatures)."
    exit 0
}

$signtool = Find-SignTool
Write-Host "Using signtool: $signtool"

$pfxPath = $null
try {
    if ($useStore) {
        # Store mode. The key stays where it is — in a cloud HSM behind
        # SimplySign's virtual card, or on a USB token — and signtool reaches it
        # through the CSP/minidriver, so there is no key material on disk and no
        # password to pass. Fail loudly here rather than letting signtool emit
        # its much vaguer "no certificates were found" later.
        $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
            Where-Object { $_.Thumbprint -eq $certSha1 } | Select-Object -First 1
        if (-not $cert) {
            throw "No certificate with thumbprint $certSha1 in Cert:\CurrentUser\My (is the SimplySign session still open?)"
        }
        Write-Host "Signing with store certificate: $($cert.Subject)"
        $signArgs = @('sign', '/fd', 'sha256', '/sha1', $certSha1)
    } else {
        $pfxPath = Join-Path ([System.IO.Path]::GetTempPath()) ("mogen-codesign-{0}.pfx" -f ([guid]::NewGuid()))
        [System.IO.File]::WriteAllBytes(
            $pfxPath,
            [System.Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64.Trim())
        )

        $signArgs = @('sign', '/fd', 'sha256', '/f', $pfxPath)
        if (-not [string]::IsNullOrEmpty($env:WINDOWS_CERT_PASSWORD)) {
            # Passed as an argument to signtool only; the value comes from the
            # environment so it is never written into the workflow file or the log.
            $signArgs += @('/p', $env:WINDOWS_CERT_PASSWORD)
        }
    }
    $signArgs += @('/tr', $timestampUrl, '/td', 'sha256', '/v')
    $signArgs += $targets

    $maxAttempts = 3
    for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
        & $signtool @signArgs
        if ($LASTEXITCODE -eq 0) { break }
        if ($attempt -eq $maxAttempts) {
            throw "signtool failed after $maxAttempts attempts (exit $LASTEXITCODE)"
        }
        $delay = $attempt * 15
        Write-Host "::warning::signtool attempt $attempt failed (exit $LASTEXITCODE); retrying in ${delay}s - usually a flaky timestamp server."
        Start-Sleep -Seconds $delay
    }

    # Release mode requires a signature that chains through the Authenticode
    # policy. Local rehearsals may use a self-signed certificate and receive a
    # warning instead.
    & $signtool verify /pa /all /v @targets
    if ($LASTEXITCODE -ne 0) {
        $message = "signtool trust verification failed (exit $LASTEXITCODE)."
        if ($Required) { throw $message }
        Write-Host "::warning::$message Expected for a self-signed local rehearsal certificate."
    }

    Write-Host "Signed $($targets.Count) file(s)."
}
finally {
    if ($pfxPath -and (Test-Path $pfxPath)) { Remove-Item $pfxPath -Force -ErrorAction SilentlyContinue }
}
