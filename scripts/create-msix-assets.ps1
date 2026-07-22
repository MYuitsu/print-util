[CmdletBinding()]
param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\msix\stage\Assets')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

foreach ($size in @(44, 150)) {
    $bitmap = New-Object System.Drawing.Bitmap($size, $size)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.Clear([System.Drawing.Color]::FromArgb(37, 99, 235))
        $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, [Math]::Max(2, [int]($size / 14)))
        $body = New-Object System.Drawing.Rectangle([int]($size * .2), [int]($size * .35), [int]($size * .6), [int]($size * .4))
        $graphics.DrawRectangle($pen, $body)
        $graphics.DrawLine($pen, [int]($size * .3), [int]($size * .35), [int]($size * .3), [int]($size * .2))
        $graphics.DrawLine($pen, [int]($size * .7), [int]($size * .35), [int]($size * .7), [int]($size * .2))
        $graphics.DrawLine($pen, [int]($size * .35), [int]($size * .65), [int]($size * .65), [int]($size * .65))
        $name = if ($size -eq 150) { 'Square150x150Logo.png' } else { 'StoreLogo.png' }
        $bitmap.Save((Join-Path $OutputDirectory $name), [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}