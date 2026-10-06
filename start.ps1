param(
    [switch]$Release,
    [switch]$Admin,
    [string[]]$AppArgs = @()
)

$ErrorActionPreference = 'Stop'
if ($Admin) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        # Quote PowerShell literals before encoding; preserve paths and application arguments.
        $invocation = "& '" + $PSCommandPath.Replace("'", "''") + "'"
        if ($Release) {
            $invocation += ' -Release'
        }
        if ($AppArgs.Count -gt 0) {
            $quotedArgs = @($AppArgs | ForEach-Object { "'" + $_.Replace("'", "''") + "'" })
            $invocation += ' -AppArgs @(' + ($quotedArgs -join ', ') + ')'
        }
        $invocation += '; exit $LASTEXITCODE'
        $encodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        Write-Host 'Requesting administrator access. Close existing DeskUnify windows and daemons first.'
        try {
            $elevated = Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -WorkingDirectory $PSScriptRoot -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encodedCommand) -Wait -PassThru
            exit $elevated.ExitCode
        }
        catch {
            Write-Error $_ -ErrorAction Continue
            exit 1
        }
    }
}

$previousLinker = $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER
$scriptExitCode = 1

try {
    Get-Command rustc, cargo -ErrorAction Stop | Out-Null
    $rustInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to inspect the Rust toolchain.'
    }

    if ($rustInfo -contains 'host: x86_64-pc-windows-gnu') {
        $rustSysroot = & rustc --print sysroot
        if ($LASTEXITCODE -ne 0) {
            throw 'Failed to locate the Rust toolchain.'
        }
        $linker = Join-Path $rustSysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained\x86_64-w64-mingw32-gcc.exe'
        if (-not (Test-Path -LiteralPath $linker -PathType Leaf)) {
            throw "Rust's bundled GNU linker was not found: $linker"
        }
        $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = $linker
        Write-Host 'Using the GNU linker bundled with Rust.'
    }

    $cargoArgs = @(
        'run', '--manifest-path', (Join-Path $PSScriptRoot 'Cargo.toml'),
        '-p', 'lan-mouse', '--no-default-features', '--features', 'egui', '--locked'
    )
    if ($Release) {
        $cargoArgs += '--release'
    }
    if ($AppArgs.Count -gt 0) {
        $cargoArgs += '--'
        $cargoArgs += $AppArgs
    }

    & cargo @cargoArgs
    $scriptExitCode = $LASTEXITCODE
}
catch {
    Write-Error $_ -ErrorAction Continue
}
finally {
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = $previousLinker
}

exit $scriptExitCode
