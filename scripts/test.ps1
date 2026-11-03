[CmdletBinding()]
param(
    [switch]$Native,
    [switch]$Streams,
    [switch]$Asio,
    [switch]$ScanIsolated,
    [switch]$Safety,
    [switch]$SecondInput,
    [string]$PluginPath = "",
    [string]$ChainPath = "",
    [switch]$SkipClippy
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")

Write-Host "cargo test --workspace"
& cargo test --workspace
if ($LASTEXITCODE -ne 0) { throw "tests failed" }

if (-not $SkipClippy) {
    Write-Host "cargo clippy --workspace --all-targets -- -D warnings"
    & cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "clippy failed" }
}

if ($Native) {
    & (Join-Path $PSScriptRoot "build.ps1") -Configuration Debug
    if ($LASTEXITCODE -ne 0) { throw "native build failed" }
}

$exe = Join-Path $root "target\debug\michost.exe"
$dll = Join-Path $root "target\debug\mic_host.dll"
if ((Test-Path $exe) -and (Test-Path $dll)) {
    Write-Host "running --selftest"
    & $exe --selftest
    if ($LASTEXITCODE -ne 0) { throw "device selftest failed" }
    if ($Streams) {
        & $exe --selftest-streams
        if ($LASTEXITCODE -ne 0) { throw "stream selftest failed" }
    }
    if ($Asio) {
        & $exe --selftest-asio
        if ($LASTEXITCODE -ne 0) { throw "ASIO selftest failed" }
    }
    if ($ScanIsolated) {
        & $exe --selftest-scan-isolated
        if ($LASTEXITCODE -ne 0) { throw "isolated scan selftest failed" }
    }
    if ($Safety) {
        & $exe --selftest-safety 5
        if ($LASTEXITCODE -ne 0) { throw "safety-buffer selftest failed" }
    }
    if ($SecondInput) {
        if ($PluginPath) {
            & $exe --selftest-second-input $PluginPath
        } else {
            & $exe --selftest-second-input
        }
        if ($LASTEXITCODE -ne 0) { throw "second-input selftest failed" }
    }
    if ($PluginPath) {
        & $exe --selftest-plugin $PluginPath
        if ($LASTEXITCODE -ne 0) { throw "plugin selftest failed" }
    }
    if ($ChainPath) {
        & $exe --selftest-chain $ChainPath
        if ($LASTEXITCODE -ne 0) { throw "chain selftest failed" }
    }
} else {
    Write-Host "Skipping native selftests (build with -Native first)."
}
