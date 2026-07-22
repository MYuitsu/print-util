[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$CertificateThumbprint,

    [Parameter(Mandatory, ValueFromPipeline)]
    [string[]]$Path,

    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

begin {
    $ErrorActionPreference = 'Stop'
    $signTool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Filter signtool.exe -Recurse |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1

    if (-not $signTool) {
        throw 'signtool.exe was not found. Install the Windows 10/11 SDK.'
    }
}

process {
    foreach ($item in $Path) {
        $resolvedPath = Resolve-Path $item -ErrorAction Stop
        $signature = Get-AuthenticodeSignature -FilePath $resolvedPath
        if ($signature.Status -eq [System.Management.Automation.SignatureStatus]::Valid) {
            Write-Host "Already signed: $resolvedPath"
            continue
        }

        & $signTool.FullName sign `
            /sha1 $CertificateThumbprint `
            /fd SHA256 `
            /td SHA256 `
            /tr $TimestampUrl `
            $resolvedPath

        if ($LASTEXITCODE -ne 0) {
            throw "signtool failed for $resolvedPath with exit code $LASTEXITCODE"
        }

        $signature = Get-AuthenticodeSignature -FilePath $resolvedPath
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
            throw "Signature validation failed for ${resolvedPath}: $($signature.StatusMessage)"
        }
    }
}
