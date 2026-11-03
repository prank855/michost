[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [ValidateSet("x64", "ARM64", "arm64")]
    [string[]]$Architecture = @("x64"),
    [ValidatePattern("^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$")]
    [string]$DriverVersion = "0.1.0.0",
    [switch]$RequireDriver
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")

function Stop-Or-SkipDriver([string]$Reason) {
    Write-Host "skipped: WDK not installed ($Reason)"
    if ($RequireDriver) {
        throw "The driver build was required, but $Reason."
    }
    exit 0
}

function Find-VisualStudio {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        return $null
    }

    $vsPath = & $vswhere -latest -version "[17.0,18.0)" -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ([string]::IsNullOrWhiteSpace($vsPath)) {
        return $null
    }

    $msbuild = Join-Path $vsPath "MSBuild\Current\Bin\MSBuild.exe"
    if (-not (Test-Path -LiteralPath $msbuild -PathType Leaf)) {
        $found = & $vswhere -latest -version "[17.0,18.0)" -products * `
            -requires Microsoft.Component.MSBuild -find "MSBuild\**\Bin\MSBuild.exe" |
            Select-Object -First 1
        if ([string]::IsNullOrWhiteSpace($found)) {
            return $null
        }
        $msbuild = $found
    }

    return [pscustomobject]@{
        InstallPath = $vsPath
        MSBuild     = $msbuild
    }
}

function Find-Wdk {
    $roots = New-Object System.Collections.Generic.List[string]
    $registry = Get-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots" `
        -ErrorAction SilentlyContinue
    if ($null -ne $registry) {
        $value = $registry.PSObject.Properties["KitsRoot10"]
        if ($null -ne $value -and -not [string]::IsNullOrWhiteSpace([string]$value.Value)) {
            $roots.Add([string]$value.Value)
        }
    }
    $defaultRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
    if (-not $roots.Contains($defaultRoot)) {
        $roots.Add($defaultRoot)
    }

    foreach ($candidate in $roots) {
        $include = Join-Path $candidate "Include"
        if (-not (Test-Path -LiteralPath $include -PathType Container)) {
            continue
        }
        $versions = @(Get-ChildItem -LiteralPath $include -Directory |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName "km\wdm.h") -PathType Leaf } |
            Sort-Object -Property Name -Descending)
        if ($versions.Count -gt 0) {
            return [pscustomobject]@{
                Root    = $candidate
                Version = $versions[0].Name
            }
        }
    }

    return $null
}

$vs = Find-VisualStudio
if ($null -eq $vs) {
    Stop-Or-SkipDriver "Visual Studio 2022 with the C++ desktop workload was not found"
}

$wdk = Find-Wdk
if ($null -eq $wdk) {
    Stop-Or-SkipDriver "no Windows Kits 10 km headers (km\wdm.h) were found"
}

$solution = Join-Path $root "driver\mh_vmic.sln"
$project = Join-Path $root "driver\mh_vmic.vcxproj"
$inf = Join-Path $root "driver\mh_vmic.inf"
foreach ($path in @($solution, $project, $inf)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Driver build input not found: $path"
    }
}

$platforms = @()
foreach ($arch in $Architecture) {
    if ($arch -ieq "arm64") {
        $platforms += "ARM64"
    } else {
        $platforms += "x64"
    }
}
$platforms = @($platforms | Select-Object -Unique)

foreach ($platform in $platforms) {
    $outDir = Join-Path $root ("target\driver\" + $Configuration + "\" + $platform)
    $intDir = Join-Path $root ("target\driver\obj\" + $Configuration + "\" + $platform)
    Remove-Item -LiteralPath $outDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $intDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    New-Item -ItemType Directory -Force -Path $intDir | Out-Null

    $msbuildArgs = @(
        $solution,
        "/m",
        "/nologo",
        ("/p:Configuration=" + $Configuration),
        ("/p:Platform=" + $platform),
        ("/p:WindowsTargetPlatformVersion=" + $wdk.Version),
        ("/p:OutDir=" + $outDir + "\"),
        ("/p:IntDir=" + $intDir + "\"),
        "/p:SignMode=Off"
    )

    Write-Host ("msbuild " + ($msbuildArgs -join " "))
    $global:LASTEXITCODE = 0
    $msbuild = $vs.MSBuild
    & $msbuild @msbuildArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Driver build failed for $platform (exit code $LASTEXITCODE)."
    }

    $sys = Join-Path $outDir "mh_vmic.sys"
    $infOut = Join-Path $outDir "mh_vmic.inf"
    $cat = Join-Path $outDir "mh_vmic.cat"

    if (-not (Test-Path -LiteralPath $sys -PathType Leaf)) {
        throw "Driver build for $platform did not produce $sys."
    }
    Copy-Item -LiteralPath $inf -Destination $infOut -Force
    $stampedDate = (Get-Date).ToString("MM/dd/yyyy", [Globalization.CultureInfo]::InvariantCulture)
    $stampedInf = [System.IO.File]::ReadAllText($infOut)
    $stampedInf = [regex]::Replace(
        $stampedInf,
        '(?m)^DriverVer\s*=.*$',
        "DriverVer       = $stampedDate, $DriverVersion"
    )
    [System.IO.File]::WriteAllText($infOut, $stampedInf, [System.Text.Encoding]::ASCII)

    if (-not (Test-Path -LiteralPath $cat -PathType Leaf)) {
        throw "Inf2Cat did not produce $cat for $platform. Check the WDK Inf2Cat/signing integration."
    }

    Write-Host "Built $Configuration/$platform driver: $sys"
}

exit 0
