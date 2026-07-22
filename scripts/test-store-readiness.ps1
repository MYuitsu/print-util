[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$MsixPath
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$expectedVersion = (Select-String -Path (Join-Path $repoRoot 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"$').Matches.Groups[1].Value
$failed = $false
$resolvedMsix = Resolve-Path $MsixPath -ErrorAction Stop
$signature = Get-AuthenticodeSignature -FilePath $resolvedMsix

if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    Write-Error "MSIX signature is not valid: $($signature.StatusMessage)" -ErrorAction Continue
    $failed = $true
}

$makeAppx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Filter makeappx.exe -Recurse |
    Where-Object { $_.FullName -match '\\x64\\makeappx\.exe$' } |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $makeAppx) { throw 'makeappx.exe was not found. Install the Windows 10/11 SDK.' }

$unpackDirectory = Join-Path $env:TEMP "print-util-msix-$([guid]::NewGuid())"
try {
    & $makeAppx.FullName unpack /p $resolvedMsix /d $unpackDirectory /o
    if ($LASTEXITCODE -ne 0) { throw "makeappx unpack failed with exit code $LASTEXITCODE" }

    [xml]$manifest = Get-Content (Join-Path $unpackDirectory 'AppxManifest.xml') -Raw
    $manifestVersion = [version]$manifest.Package.Identity.Version
    $cargoVersion = [version]$expectedVersion
    if ($manifestVersion.Major -ne $cargoVersion.Major -or
        $manifestVersion.Minor -ne $cargoVersion.Minor -or
        $manifestVersion.Build -ne $cargoVersion.Build) {
        Write-Error "Version mismatch: Cargo.toml=$expectedVersion, MSIX=$manifestVersion" -ErrorAction Continue
        $failed = $true
    }

    if ($manifest.Package.Identity.Publisher -match 'REPLACE_WITH') {
        Write-Error 'MSIX Publisher still contains a placeholder.' -ErrorAction Continue
        $failed = $true
    }

    $portableExecutables = @(
        'print-util.exe',
        'print-util-tray.exe',
        'SumatraPDF.exe',
        'gswin64c.exe',
        'gsdll64.dll'
    ) | ForEach-Object { Join-Path $unpackDirectory $_ }

    foreach ($file in $portableExecutables) {
        if (-not (Test-Path $file -PathType Leaf)) {
            Write-Error "Missing required MSIX payload: $file" -ErrorAction Continue
            $failed = $true
            continue
        }

        $payloadSignature = Get-AuthenticodeSignature -FilePath $file
        $signer = if ($payloadSignature.SignerCertificate) { $payloadSignature.SignerCertificate.Subject } else { '<none>' }
        Write-Host "$($payloadSignature.Status.ToString().PadRight(12)) $file [$signer]"

        if ($payloadSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
            $failed = $true
        }
    }
}
finally {
    Remove-Item $unpackDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failed) {
    throw 'Microsoft Store readiness check failed. Resolve every error and invalid signature reported above.'
}

Write-Host "MSIX Store readiness check passed for version $expectedVersion."
