<#
.SYNOPSIS
  Opens a Certum SimplySign cloud-signing session on a Windows CI runner and
  publishes the resulting certificate thumbprint for scripts/sign-windows.ps1.

.DESCRIPTION
  Certum's cloud code-signing key (the "Open Source Code Signing in the Cloud"
  product) is never exportable, so the PFX path in scripts/sign-windows.ps1 cannot
  be used with it. The key lives in Certum's HSM and is reachable only through
  **SimplySign Desktop**, which mounts it as a *virtual smart card*: once a
  session is open the certificate appears in Cert:\CurrentUser\My and signtool
  signs with it exactly as it would with a physical token.

  SimplySign Desktop exposes an unattended `/autologin <user> <otp>` mode. It
  is not documented in Certum's public user guide, but it is the application's
  own non-interactive login entry point. This script:

    1. installs SimplySign Desktop if it isn't already present,
    2. derives the current TOTP from the enrolment secret,
    3. launches the app in its direct auto-login mode,
    4. polls Cert:\CurrentUser\My until the cloud certificate shows up,
    5. writes its thumbprint to $GITHUB_ENV as WINDOWS_CERT_SHA1.

  The login runs without a visible desktop, so it works in the non-interactive
  service session used by GitHub-hosted Windows runners. See the signing
  notes at the top of .github/workflows/release.yml.

  By default failures warn and return for local experimentation. Pass -Required
  (the tagged-release workflow does) to terminate when the session cannot be
  opened, preventing an unsigned release fallback.

  Environment:

    SIMPLYSIGN_USER          SimplySign account ID / e-mail            (required)
    SIMPLYSIGN_TOTP_SECRET   base32 TOTP secret, or the whole          (required)
                             otpauth:// URI from the enrolment QR code
    SIMPLYSIGN_TIMEOUT_SEC   seconds to wait for the card to mount     (optional,
                             default 180)

  SIMPLYSIGN_TOTP_SECRET is the *second factor* for the signing account. Putting
  it in CI necessarily collapses 2FA to 1FA for anyone holding repo secrets.
  That is inherent to unattended signing, not something this script introduces,
  but it is worth knowing before you configure it.

.EXAMPLE
  ./scripts/simplysign-login.ps1
#>
[CmdletBinding()]
param([switch]$Required)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

function Write-Warn([string]$Message) { Write-Host "::warning::$Message" }
function Stop-OrWarn([string]$Message) {
    if ($Required) { throw $Message }
    Write-Warn $Message
}

if ([string]::IsNullOrWhiteSpace($env:SIMPLYSIGN_USER) -or
    [string]::IsNullOrWhiteSpace($env:SIMPLYSIGN_TOTP_SECRET)) {
    Stop-OrWarn "SIMPLYSIGN_USER / SIMPLYSIGN_TOTP_SECRET are not both set."
    exit 0
}

$timeoutSec = 180
if (-not [string]::IsNullOrWhiteSpace($env:SIMPLYSIGN_TIMEOUT_SEC)) {
    [int]::TryParse($env:SIMPLYSIGN_TIMEOUT_SEC, [ref]$timeoutSec) | Out-Null
}

# --- TOTP ------------------------------------------------------------------
# The enrolment QR code is a standard otpauth:// URI, so accept either the URI
# (what you get by revealing the QR in a password manager) or the bare base32
# secret. The URI is strongly preferred: Certum code-signing enrolments use
# HMAC-SHA256 rather than RFC 6238's SHA1 default, and only the URI preserves
# that parameter.
function Get-TotpParams([string]$Value) {
    $v = $Value.Trim()
    $params = @{ Secret = $v; Algorithm = 'SHA1'; Digits = 6; Period = 30 }

    if ($v -match '^otpauth://') {
        if ($v -notmatch '[?&]secret=([^&]+)') { throw "otpauth:// URI has no secret= parameter" }
        $params.Secret = [Uri]::UnescapeDataString($Matches[1])
        if ($v -match '[?&]algorithm=([^&]+)') {
            $params.Algorithm = [Uri]::UnescapeDataString($Matches[1]).ToUpperInvariant()
        }
        if ($v -match '[?&]digits=(\d+)') { $params.Digits = [int]$Matches[1] }
        if ($v -match '[?&]period=(\d+)') { $params.Period = [int]$Matches[1] }
    }

    if ($params.Digits -lt 6 -or $params.Digits -gt 8) {
        throw "Unsupported TOTP digit count '$($params.Digits)'"
    }
    if ($params.Period -le 0) { throw "TOTP period must be positive" }
    return $params
}

function ConvertFrom-Base32([string]$Value) {
    $alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'
    $clean = ($Value -replace '[=\s-]', '').ToUpperInvariant()
    if ($clean.Length -eq 0) { throw "TOTP secret is empty" }

    $bits = New-Object System.Text.StringBuilder
    foreach ($ch in $clean.ToCharArray()) {
        $idx = $alphabet.IndexOf($ch)
        if ($idx -lt 0) { throw "TOTP secret is not valid base32 (bad character '$ch')" }
        [void]$bits.Append([Convert]::ToString($idx, 2).PadLeft(5, '0'))
    }

    # Base32 packs 5 bits per character, so the tail is padding unless the total
    # lands on a byte boundary; drop whatever doesn't fill a full byte.
    $s = $bits.ToString()
    $bytes = New-Object System.Collections.Generic.List[byte]
    for ($i = 0; $i + 8 -le $s.Length; $i += 8) {
        $bytes.Add([Convert]::ToByte($s.Substring($i, 8), 2))
    }
    return $bytes.ToArray()
}

function Get-Totp([byte[]]$Secret, [string]$Algorithm = 'SHA1', [int]$Digits = 6, [int]$Period = 30) {
    # RFC 6238. Certum specifies the PRF in its enrolment URI.
    $counter = [BitConverter]::GetBytes([long][Math]::Floor([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() / $Period))
    if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($counter) }

    $hmac = switch ($Algorithm) {
        'SHA1'   { [System.Security.Cryptography.HMACSHA1]::new($Secret) }
        'SHA256' { [System.Security.Cryptography.HMACSHA256]::new($Secret) }
        'SHA512' { [System.Security.Cryptography.HMACSHA512]::new($Secret) }
        default  { throw "Unsupported TOTP algorithm '$Algorithm'" }
    }
    try { $hash = $hmac.ComputeHash($counter) } finally { $hmac.Dispose() }

    $offset = $hash[$hash.Length - 1] -band 0x0F
    $binary = ((($hash[$offset] -band 0x7F) -shl 24) -bor
               (($hash[$offset + 1] -band 0xFF) -shl 16) -bor
               (($hash[$offset + 2] -band 0xFF) -shl 8) -bor
                ($hash[$offset + 3] -band 0xFF))
    return ($binary % [int][Math]::Pow(10, $Digits)).ToString().PadLeft($Digits, '0')
}

# --- SimplySign Desktop ----------------------------------------------------
function Find-SimplySign {
    $roots = @(
        "${env:ProgramFiles(x86)}\Certum",
        "$env:ProgramFiles\Certum",
        "${env:ProgramFiles(x86)}\proCertum SmartSign",
        "$env:ProgramFiles\proCertum SmartSign"
    ) | Where-Object { $_ -and (Test-Path $_) }

    foreach ($root in $roots) {
        $exe = Get-ChildItem -Path $root -Recurse -Filter 'SimplySign*.exe' -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($exe) { return $exe.FullName }
    }
    return $null
}

function Install-SimplySign {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        Write-Host "Installing SimplySign Desktop via winget..."
        & $winget.Source install --id Certum.SmartSignSimplySignDesktop --exact --silent `
            --accept-package-agreements --accept-source-agreements --disable-interactivity
        if ($LASTEXITCODE -eq 0) { return }
        Write-Warn "winget install returned $LASTEXITCODE; trying Certum's pinned MSI."
    }

    # GitHub's windows-2022 image intentionally has no Microsoft Store and no
    # winget. Certum publishes the same 64-bit installer directly; pin its hash
    # so a mutable download URL can never silently change release signing code.
    $version = '9.4.4.92'
    $uri = "https://files.certum.eu/software/SimplySignDesktop/Windows/$version/SimplySignDesktop-$version-64-bit-en.msi"
    $expectedSha256 = '8ec420fc27798b86078b7bd02fe7152097e1b3005bab51820eaca8e57df84da3'
    $msi = Join-Path $env:RUNNER_TEMP "SimplySignDesktop-$version-64-bit-en.msi"
    Write-Host "Downloading SimplySign Desktop $version from Certum..."
    Invoke-WebRequest -Uri $uri -OutFile $msi -UseBasicParsing
    $actualSha256 = (Get-FileHash -Path $msi -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualSha256 -ne $expectedSha256) {
        throw "SimplySign Desktop MSI checksum mismatch (got $actualSha256)."
    }
    $signature = Get-AuthenticodeSignature -FilePath $msi
    if ($signature.Status -ne 'Valid') {
        throw "SimplySign Desktop MSI Authenticode signature is $($signature.Status)."
    }
    $process = Start-Process msiexec.exe -ArgumentList @('/i', $msi, '/qn', '/norestart') -Wait -PassThru
    Write-Host "SimplySign Desktop MSI install returned $($process.ExitCode)."
    if ($process.ExitCode -notin @(0, 3010)) {
        throw "SimplySign Desktop MSI install returned $($process.ExitCode)."
    }
    if ($process.ExitCode -eq 3010) {
        Write-Warn "SimplySign Desktop requested a reboot; the virtual card may not be available in this runner session."
    }
}

# SimplySign writes its own authentication result under Documents. Read only a
# small set of known status phrases and never emit the raw log: vendor logs may
# contain account identifiers or other authentication metadata.
function Get-SimplySignLoginStatus([datetime]$Since) {
    $documentRoots = @(
        [Environment]::GetFolderPath([Environment+SpecialFolder]::MyDocuments),
        $(if ($env:USERPROFILE) { Join-Path $env:USERPROFILE 'Documents' })
    ) | Where-Object { $_ } | Select-Object -Unique

    $logs = foreach ($root in $documentRoots) {
        $logDir = Join-Path $root 'SimplySignLog'
        if (Test-Path $logDir) {
            Get-ChildItem -Path $logDir -Filter 'SimplySign*log.txt' -File -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -ge $Since.AddSeconds(-5) }
        }
    }

    $latest = $logs | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $latest) { return 'no-recent-log' }

    $content = Get-Content -Path $latest.FullName -Raw -ErrorAction SilentlyContinue
    if ([string]::IsNullOrWhiteSpace($content)) { return 'empty-log' }

    # Select the last authentication event in case the app retried internally.
    $events = [regex]::Matches(
        $content,
        'login ok\. token valid for|login failed\([^\r\n)]*\)|access_token not found|server configuration incomplete',
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($events.Count -eq 0) { return 'unclassified-log' }

    $last = $events[$events.Count - 1].Value
    if ($last -match '^login ok') { return 'authenticated' }
    if ($last -match '^login failed') { return 'rejected' }
    if ($last -match '^access_token') { return 'missing-token' }
    if ($last -match '^server configuration') { return 'server-configuration' }
    return 'unclassified-log'
}

# Certificates the cloud card is about to add. Diffing against this is more
# reliable than matching on subject: the OSS certificate's subject is issued to
# an individual ("Open Source Developer, <name>") and we'd rather not hardcode
# a person's name in the repo.
function Get-CodeSigningCerts {
    Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object {
            $_.HasPrivateKey -and
            $_.EnhancedKeyUsageList.ObjectId -contains '1.3.6.1.5.5.7.3.3'
        }
}

try {
    $before = @(Get-CodeSigningCerts | Select-Object -ExpandProperty Thumbprint)
    Write-Host "Code-signing certificates present before login: $($before.Count)"

    $exe = Find-SimplySign
    if (-not $exe) {
        Install-SimplySign
        $exe = Find-SimplySign
    }
    if (-not $exe) {
        Stop-OrWarn "SimplySign Desktop could not be found or installed."
        exit 0
    }
    Write-Host "Using SimplySign Desktop: $exe"

    $totp = Get-TotpParams $env:SIMPLYSIGN_TOTP_SECRET
    $otp = Get-Totp (ConvertFrom-Base32 $totp.Secret) $totp.Algorithm $totp.Digits $totp.Period
    Write-Host "Generated a $($otp.Length)-digit TOTP (HMAC-$($totp.Algorithm), $($totp.Period)s)."

    # `/autologin` requires both values as arguments. Passing only the switch
    # makes the app ignore auto-login and start as a tray process with no login
    # window, which is why the old GitHub-hosted release job timed out. The OTP
    # is short-lived and is never printed, but it is briefly visible to local
    # process-inspection tools on this single-tenant ephemeral runner.
    $loginStarted = Get-Date
    Start-Process -FilePath $exe `
        -ArgumentList @('/autologin', $env:SIMPLYSIGN_USER, $otp) | Out-Null

    Write-Host "Auto-login started; waiting up to ${timeoutSec}s for the virtual card to mount..."

    $deadline = (Get-Date).AddSeconds($timeoutSec)
    $found = $null
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 5
        $found = Get-CodeSigningCerts | Where-Object { $before -notcontains $_.Thumbprint } | Select-Object -First 1
        if ($found) { break }
    }

    if (-not $found) {
        $loginStatus = Get-SimplySignLoginStatus -Since $loginStarted
        Write-Host "SimplySign redacted authentication status: $loginStatus"
        $message = switch ($loginStatus) {
            'authenticated' { 'SimplySign accepted the login, but its virtual card did not expose a code-signing certificate.' }
            'rejected' { 'SimplySign rejected the account or one-time password.' }
            'missing-token' { 'SimplySign returned no access token after login.' }
            'server-configuration' { 'SimplySign reported incomplete server configuration.' }
            default { "SimplySign did not expose a code-signing certificate within ${timeoutSec}s, and its log did not contain a recognized authentication result." }
        }
        Stop-OrWarn $message
        exit 0
    }

    Write-Host "Cloud certificate mounted: $($found.Subject)"
    Write-Host "  expires: $($found.NotAfter.ToString('yyyy-MM-dd'))"

    # Hand the thumbprint to the signing steps. It is not a secret (it's derived
    # from the public certificate) and keeping it out of the repo secrets means
    # renewals don't need a secret rotation.
    if ($env:GITHUB_ENV) {
        "WINDOWS_CERT_SHA1=$($found.Thumbprint)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
        Write-Host "Exported WINDOWS_CERT_SHA1 for subsequent steps."
    } else {
        Write-Host "GITHUB_ENV is unset (running locally); export WINDOWS_CERT_SHA1=$($found.Thumbprint) yourself."
    }
}
catch {
    if ($Required) { throw }
    Write-Warn "SimplySign login failed: $($_.Exception.Message)"
    exit 0
}
