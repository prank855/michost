[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$JuceDir = $env:MICHOST_JUCE_DIR,
    [string]$AsioDir = $env:ASIO_SDK_DIR,
    [switch]$NoAsio,
    [switch]$SkipRust,
    [switch]$SkipNative,
    [switch]$Driver,
    [switch]$RequireDriver,
    [string[]]$DriverArchitectures = @("x64"),
    [string]$DriverVersion = "0.1.0.0"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$profileName = if ($Configuration -eq "Debug") { "debug" } else { "release" }

function Test-WdkInstalled {
    $roots = New-Object System.Collections.Generic.List[string]
    $registry = Get-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots" `
        -ErrorAction SilentlyContinue
    if ($null -ne $registry) {
        $value = $registry.PSObject.Properties["KitsRoot10"]
        if ($null -ne $value -and -not [string]::IsNullOrWhiteSpace([string]$value.Value)) {
            $roots.Add([string]$value.Value)
        }
    }
    $roots.Add((Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10"))

    foreach ($candidate in ($roots | Select-Object -Unique)) {
        $include = Join-Path $candidate "Include"
        if (-not (Test-Path -LiteralPath $include -PathType Container)) { continue }
        $found = @(Get-ChildItem -LiteralPath $include -Directory |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "km\wdm.h") -PathType Leaf })
        if ($found.Count -gt 0) { return $true }
    }
    return $false
}

function Find-CMake {
    $command = Get-Command cmake -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $candidates = @(
        "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
        "C:\Program Files\Microsoft Visual Studio\18\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
        "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe",
        "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    throw "CMake was not found. Install CMake or the Visual Studio C++ workload."
}

if (-not $SkipRust) {
    $rustArgs = @("build", "-p", "michost")
    if ($Configuration -eq "Release") { $rustArgs += "--release" }
    Write-Host "cargo $($rustArgs -join ' ')"
    & cargo @rustArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

if (-not $SkipNative) {
    if (-not $AsioDir -and (Test-Path "C:\kilo\asio-sdk\sdk\ASIOSDK")) {
        $AsioDir = "C:\kilo\asio-sdk\sdk\ASIOSDK"
    }

    $cmake = Find-CMake
    $targetDir = Join-Path $root "target\$profileName"
    New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
    $buildDir = Join-Path $root ("target\cpp-" + $profileName)

    $configure = @(
        "-S", (Join-Path $root "cpp"),
        "-B", $buildDir,
        "-DMICHOST_RUST_TARGET_DIR=$targetDir"
    )
    if ($JuceDir) { $configure += "-DMICHOST_JUCE_DIR=$JuceDir" }
    if ($NoAsio) {
        $configure += "-DMICHOST_ASIO=OFF"
    } elseif ($AsioDir) {
        $configure += "-DASIO_SDK_DIR=$AsioDir"
    } else {
        throw "ASIO SDK not found. Set ASIO_SDK_DIR or pass -AsioDir, or pass -NoAsio."
    }

    Write-Host "cmake $($configure -join ' ')"
    & $cmake @configure
    if ($LASTEXITCODE -ne 0) { throw "CMake configure failed" }

    & $cmake --build $buildDir --config $Configuration --parallel
    if ($LASTEXITCODE -ne 0) { throw "native build failed" }
}

$dll = Join-Path $root "target\$profileName\mic_host.dll"
if (Test-Path $dll) {
    Write-Host "Built $dll"
} elseif (-not $SkipNative) {
    throw "mic_host.dll was not copied to $dll"
}

$probe = Join-Path $root "target\$profileName\michost-probe.exe"
if (Test-Path $probe) {
    Write-Host "Built $probe"
} elseif (-not $SkipRust) {
    throw "michost-probe.exe was not built next to michost.exe"
} else {
    Write-Warning "michost-probe.exe is missing; isolated scanning will fall back to in-process description."
}

$runDriver = $Driver -or $RequireDriver -or (Test-WdkInstalled)
if ($runDriver) {
    $driverScript = Join-Path $PSScriptRoot "build-driver.ps1"
    if (-not (Test-Path -LiteralPath $driverScript -PathType Leaf)) {
        if ($Driver -or $RequireDriver) { throw "Driver build script not found: $driverScript" }
        Write-Warning "Driver build script not found: $driverScript"
    } else {
        $driverArgs = @(
            "-Configuration", $Configuration,
            "-Architecture", $DriverArchitectures,
            "-DriverVersion", $DriverVersion
        )
        if ($Driver -or $RequireDriver) { $driverArgs += "-RequireDriver" }
        try {
            $global:LASTEXITCODE = 0
            & $driverScript @driverArgs
            if ($LASTEXITCODE -ne 0) {
                throw "Driver build failed with exit code $LASTEXITCODE"
            }
        } catch {
            if ($Driver -or $RequireDriver) { throw }
            Write-Warning "Driver build skipped or failed; the application build is unaffected: $_"
        }
    }
}
