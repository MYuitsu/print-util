[CmdletBinding()]
param(
    [string]$InstallerPath,
    [switch]$RequireInstaller
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$expectedVersion = (Select-String -Path (Join-Path $repoRoot 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"$').Matches.Groups[1].Value
$setupVersion = (Select-String -Path (Join-Path $repoRoot 'installer/setup.iss') -Pattern '^#define AppVersion\s+"([^"]+)"$').Matches.Groups[1].Value
$setupPublisher = (Select-String -Path (Join-Path $repoRoot 'installer/store-config.iss') -Pattern '^#define StorePublisher\s+"([^"]+)"$').Matches.Groups[1].Value
$failed = $false

if ($expectedVersion -ne $setupVersion) {
    Write-Error "Version mismatch: Cargo.toml=$expectedVersion, installer/setup.iss=$setupVersion" -ErrorAction Continue
    $failed = $true
}

if ($setupPublisher -eq 'REPLACE_WITH_PARTNER_CENTER_PUBLISHER_NAME') {
    Write-Error 'Replace StorePublisher with the exact Partner Center publisher name.' -ErrorAction Continue
    $failed = $true
}

$portableExecutables = @(
    'target/release/print-util.exe',
    'target/release/print-util-tray.exe',
    'installer/vendor/SumatraPDF.exe',
    'installer/vendor/gswin64c.exe',
    'installer/vendor/gsdll64.dll'
) | ForEach-Object { Join-Path $repoRoot $_ }

if ($InstallerPath) {
    $portableExecutables += $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallerPath)
} elseif ($RequireInstaller) {
    throw 'InstallerPath is required when RequireInstaller is set.'
}

foreach ($file in $portableExecutables) {
    if (-not (Test-Path $file -PathType Leaf)) {
        Write-Error "Missing required file: $file" -ErrorAction Continue
        $failed = $true
        continue
    }

    $signature = Get-AuthenticodeSignature -FilePath $file
    $signer = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { '<none>' }
    Write-Host "$($signature.Status.ToString().PadRight(12)) $file [$signer]"

    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        $failed = $true
    }
}

if ($failed) {
    throw 'Microsoft Store readiness check failed. Resolve every error and invalid signature reported above.'
}

Write-Host "Store readiness check passed for version $expectedVersion."
