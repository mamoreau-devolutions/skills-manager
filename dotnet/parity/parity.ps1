#Requires -Version 7.4
<#
.SYNOPSIS
    Runs the shared parity harness (rust/parity/parity.ps1) against the C# port.

.DESCRIPTION
    The cases, sandboxes and comparison logic live in rust/parity/parity.ps1 so
    both ports are held to the same checks. This wrapper only points the harness
    at the published C# binary.

.EXAMPLE
    pwsh dotnet/parity/parity.ps1 -Reference ../skills           # offline cases
.EXAMPLE
    pwsh dotnet/parity/parity.ps1 -Reference ../skills -Network  # also GitHub / skills.sh cases
.EXAMPLE
    pwsh dotnet/parity/parity.ps1 -Filter add -ShowOutput        # uses $env:SKILLS_REFERENCE
#>
[CmdletBinding()]
param(
    [switch]$Network,
    # Checkout of the reference TypeScript CLI (default: $env:SKILLS_REFERENCE).
    [string]$Reference,
    [string]$Filter = '',
    # Path to the C# binary (default: dotnet/publish/skills[.exe]).
    [string]$Bin,
    [switch]$ShowOutput,
    [switch]$Keep,
    [int]$TimeoutSec = 180
)

$ErrorActionPreference = 'Stop'
$DotnetDir = Split-Path -Parent $PSScriptRoot
if (-not $Bin) {
    $Bin = Join-Path $DotnetDir 'publish' ($IsWindows ? 'skills.exe' : 'skills')
}
if (-not (Test-Path $Bin)) {
    throw "C# binary not found at $Bin (run: dotnet publish dotnet/src/Skills.csproj -c Release -r <rid> -o dotnet/publish)"
}

$forward = @{} + $PSBoundParameters
$forward.Bin = (Resolve-Path $Bin).Path
& (Join-Path (Split-Path -Parent $DotnetDir) 'rust' 'parity' 'parity.ps1') @forward
exit $LASTEXITCODE
