[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Version,
    [Parameter(Mandatory)] [string]$Publisher,
    [Parameter(Mandatory)] [string]$OutputPath,
    [string]$PackageName = 'MYuitsu.PrintUtil',
    [string]$PublisherDisplayName,
    [string]$BinaryDirectory
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$stage = Join-Path $repoRoot 'msix\stage'
$manifest = Join-Path $repoRoot 'msix\AppxManifest.xml'
$BinaryDirectory = if ($BinaryDirectory) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($BinaryDirectory)
} else {
    Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release'
}
$PublisherDisplayName = if ($PublisherDisplayName) { $PublisherDisplayName } else { $Publisher -replace '^CN=', '' }
$makeAppx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Filter makeappx.exe -Recurse |
    Where-Object { $_.FullName -match '\\x64\\makeappx\.exe$' } |
    Sort-Object FullName -Descending | Select-Object -First 1

if (-not $makeAppx) { throw 'makeappx.exe was not found. Install the Windows 10/11 SDK.' }
if ($Version -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') { throw "Invalid MSIX version: $Version" }
if ($Publisher -match '[\r\n"]' -or $Publisher -notmatch '^CN=') { throw 'Publisher must be the certificate Subject, beginning with CN=' }
$versionParts = @($Version -split '\.')
while ($versionParts.Count -lt 4) { $versionParts += '0' }
$msixVersion = $versionParts -join '.'

$requiredFiles = @(
    (Join-Path $BinaryDirectory 'print-util.exe'),
    (Join-Path $BinaryDirectory 'print-util-tray.exe'),
    (Join-Path $repoRoot 'installer\vendor\SumatraPDF.exe'),
    (Join-Path $repoRoot 'installer\vendor\gswin64c.exe'),
    (Join-Path $repoRoot 'installer\vendor\gsdll64.dll'),
    (Join-Path $repoRoot 'installer\vendor\gs_lib\gs_init.ps')
)
foreach ($file in $requiredFiles) {
    if (-not (Test-Path $file -PathType Leaf)) { throw "Missing MSIX payload file: $file" }
}

Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item (Join-Path $BinaryDirectory 'print-util.exe') $stage
Copy-Item (Join-Path $BinaryDirectory 'print-util-tray.exe') $stage
Copy-Item (Join-Path $repoRoot 'installer\vendor\SumatraPDF.exe') $stage
Copy-Item (Join-Path $repoRoot 'installer\vendor\gswin64c.exe') $stage
Copy-Item (Join-Path $repoRoot 'installer\vendor\gsdll64.dll') $stage
Copy-Item (Join-Path $repoRoot 'installer\vendor\gs_lib') (Join-Path $stage 'gs_lib') -Recurse
& (Join-Path $PSScriptRoot 'create-msix-assets.ps1') -OutputDirectory (Join-Path $stage 'Assets')

[xml]$xml = Get-Content $manifest -Raw
$xml.Package.Identity.Name = $PackageName
$xml.Package.Identity.Publisher = $Publisher
$xml.Package.Identity.Version = $msixVersion
$xml.Package.Properties.PublisherDisplayName = $PublisherDisplayName
$xml.Save((Join-Path $stage 'AppxManifest.xml'))

New-Item -ItemType Directory -Force -Path (Split-Path $OutputPath) | Out-Null
Remove-Item $OutputPath -Force -ErrorAction SilentlyContinue
& $makeAppx.FullName pack /d $stage /p $OutputPath /o
if ($LASTEXITCODE -ne 0) { throw "makeappx failed with exit code $LASTEXITCODE" }