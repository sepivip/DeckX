[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $Version,

    [Parameter(Mandatory)]
    [string] $PayloadDirectory,

    [Parameter(Mandatory)]
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'

$compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
if (!$compiler) {
    $compiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
}
if (!(Test-Path -LiteralPath $compiler -PathType Leaf)) {
    throw 'Inno Setup 6 is required to build the Windows installer.'
}

$payload = (Resolve-Path -LiteralPath $PayloadDirectory).Path
$output = (New-Item -ItemType Directory -Path $OutputDirectory -Force).FullName
$recipe = Join-Path $PSScriptRoot 'DeckX.iss'
& $compiler "/DDeckXVersion=$Version" "/DDeckXPayload=$payload" "/O$output" $recipe
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup exited with code $LASTEXITCODE."
}
