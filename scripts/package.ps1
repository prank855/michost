[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [ValidatePattern("^[0-9A-Za-z]([0-9A-Za-z._+-]*[0-9A-Za-z])?$")]
    [string]$Version = "0.2.0",
    [string]$OutputDir = "target\dist",
    [switch]$SkipBuild,
    [switch]$IncludeDriver,
    [switch]$NoBuildStamp
)

$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$profileName = if ($Configuration -eq "Debug") { "debug" } else { "release" }
$targetDir = Join-Path $root "target\$profileName"

function Read-BuildStamp([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    try {
        $stamp = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        throw "Could not read the build stamp '$Path': $($_.Exception.Message)"
    }
    if ([string]::IsNullOrWhiteSpace([string]$stamp.build_version)) {
        throw "The build stamp '$Path' has no build_version. Rebuild with scripts/build.ps1."
    }
    return $stamp
}

function Trim-DirectorySeparators([string]$Path) {
    return $Path.TrimEnd([char[]]@('\', '/'))
}

function Format-Size([long]$Bytes) {
    if ($Bytes -ge 1MB) { return "{0:N2} MB" -f ($Bytes / 1MB) }
    if ($Bytes -ge 1KB) { return "{0:N1} KB" -f ($Bytes / 1KB) }
    return "$Bytes B"
}

function Get-TreeSize([string]$Path) {
    $sum = (Get-ChildItem -LiteralPath $Path -Recurse -File | Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) { return [long]0 }
    return [long]$sum
}

function Remove-StageDirectory([string]$Path, [string]$ExpectedName) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $full = Trim-DirectorySeparators ([System.IO.Path]::GetFullPath($Path))
    $parent = [System.IO.Path]::GetDirectoryName($full)
    $leaf = [System.IO.Path]::GetFileName($full)
    $outputFull = Trim-DirectorySeparators ([System.IO.Path]::GetFullPath($outputPath))
    if ($leaf -ne $ExpectedName) {
        throw "Refusing to remove '$full': the leaf name is not '$ExpectedName'."
    }
    if ($parent -ne $outputFull) {
        throw "Refusing to remove '$full': it is not directly inside '$outputFull'."
    }
    if ($full -eq (Trim-DirectorySeparators ([System.IO.Path]::GetPathRoot($full)))) {
        throw "Refusing to remove '$full': it is a filesystem root."
    }
    Remove-Item -LiteralPath $full -Recurse -Force
}

if (-not $SkipBuild) {
    $buildScript = Join-Path $PSScriptRoot "build.ps1"
    if (-not (Test-Path -LiteralPath $buildScript)) {
        throw "Build script not found: $buildScript"
    }
    Write-Host "Building $Configuration via $buildScript"
    $buildArgs = @{ Configuration = $Configuration }
    if ($PSBoundParameters.ContainsKey("Version")) {
        $buildArgs["Version"] = $Version
    }
    $global:LASTEXITCODE = 0
    & $buildScript @buildArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Build failed with exit code $LASTEXITCODE"
    }
}

$stampPath = Join-Path $targetDir "build-stamp.json"
$stamp = Read-BuildStamp $stampPath
if ($null -ne $stamp) {
    $buildVersion = [string]$stamp.build_version
    $buildTimeUtc = [string]$stamp.build_time
    $gitHash = [string]$stamp.git_hash
    $gitDirty = [bool]$stamp.git_dirty
    Write-Host "Using the build stamp from $stampPath"
} elseif (-not [string]::IsNullOrWhiteSpace($env:MICHOST_BUILD_VERSION)) {
    $buildVersion = $env:MICHOST_BUILD_VERSION
    $buildTimeUtc = [string]$env:MICHOST_BUILD_TIME
    $gitHash = [string]$env:MICHOST_GIT_HASH
    $gitDirty = $env:MICHOST_GIT_DIRTY -eq "1"
    Write-Host "Using MICHOST_BUILD_VERSION from the environment; no persisted build stamp was found."
} elseif ($SkipBuild) {
    throw ("package.ps1 -SkipBuild found no build stamp. Run scripts/build.ps1 -Configuration $Configuration " +
        "first (it writes '$stampPath'), or set MICHOST_BUILD_VERSION, then run -SkipBuild again.")
} else {
    throw "scripts/build.ps1 did not write the build stamp '$stampPath'. Rebuild and try again."
}
if ([string]::IsNullOrWhiteSpace($buildTimeUtc)) { $buildTimeUtc = "unknown" }
if ([string]::IsNullOrWhiteSpace($gitHash)) { $gitHash = "nogit" }

if (-not $PSBoundParameters.ContainsKey("Version") -and
    $null -ne $stamp -and
    -not [string]::IsNullOrWhiteSpace([string]$stamp.version)) {
    $Version = [string]$stamp.version
}

$stampSuffix = "${buildTimeUtc}-${gitHash}"
if ($gitDirty) {
    $stampSuffix += "-dirty"
}
$stageVersion = $Version
if (-not $NoBuildStamp) {
    $stageVersion = "${Version}-" + [regex]::Replace($stampSuffix, "[^0-9A-Za-z._-]", "-")
}
$stageName = if ($Configuration -eq "Release") { "MicHost-$stageVersion" } else { "MicHost-$stageVersion-$Configuration" }
$outputPath = if ([System.IO.Path]::IsPathRooted($OutputDir)) {
    [System.IO.Path]::GetFullPath($OutputDir)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $root $OutputDir))
}
$stageDir = Join-Path $outputPath $stageName
$zipPath = Join-Path $outputPath "$stageName.zip"

$requiredArtifacts = @(
    (Join-Path $targetDir "michost.exe"),
    (Join-Path $targetDir "michost-probe.exe"),
    (Join-Path $targetDir "mic_host.dll")
)
$missing = @($requiredArtifacts | Where-Object { -not (Test-Path -LiteralPath $_ -PathType Leaf) })
if ($missing.Count -gt 0) {
    $list = ($missing | ForEach-Object { "  - $_" }) -join [Environment]::NewLine
    throw ("Missing $Configuration build artifacts. Build with scripts/build.ps1 -Configuration $Configuration, " +
        "or pass -Configuration Debug. Missing:" + [Environment]::NewLine + $list)
}

$installerSource = Join-Path $root "scripts\install.ps1"
$installCmdSource = Join-Path $root "scripts\Install.cmd"
foreach ($installerFile in @($installerSource, $installCmdSource)) {
    if (-not (Test-Path -LiteralPath $installerFile -PathType Leaf)) {
        throw "Installer file not found: $installerFile"
    }
}

$manifestSource = Join-Path $root "packaging\michost.exe.manifest"
if (-not (Test-Path -LiteralPath $manifestSource -PathType Leaf)) {
    throw "Manifest not found: $manifestSource"
}
$licenseFiles = @("LICENSE", "THIRD_PARTY_NOTICES.md")
foreach ($name in $licenseFiles) {
    $source = Join-Path $root $name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Required file not found: $source"
    }
}

New-Item -ItemType Directory -Force -Path $outputPath | Out-Null
Remove-StageDirectory $stageDir $stageName
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

foreach ($artifact in $requiredArtifacts) {
    Copy-Item -LiteralPath $artifact -Destination (Join-Path $stageDir ([System.IO.Path]::GetFileName($artifact)))
}
Copy-Item -LiteralPath $installerSource -Destination (Join-Path $stageDir "install.ps1")
Copy-Item -LiteralPath $installCmdSource -Destination (Join-Path $stageDir "Install.cmd")

$vcRuntimeNames = @("VCRUNTIME140.dll", "VCRUNTIME140_1.dll", "MSVCP140.dll")
$stagedRuntime = @()
$missingRuntime = @($vcRuntimeNames)
if (-not [string]::IsNullOrWhiteSpace($env:VCToolsRedistDir) -and
    (Test-Path -LiteralPath $env:VCToolsRedistDir -PathType Container)) {
    $crtPattern = Join-Path $env:VCToolsRedistDir "x64\Microsoft.VC*.CRT"
    $crtDir = Get-ChildItem -Path $crtPattern -Directory -ErrorAction SilentlyContinue |
        Sort-Object -Property FullName | Select-Object -Last 1
    if ($null -ne $crtDir) {
        $missingRuntime = @()
        foreach ($runtimeName in $vcRuntimeNames) {
            $runtimeSource = Join-Path $crtDir.FullName $runtimeName
            if (Test-Path -LiteralPath $runtimeSource -PathType Leaf) {
                try {
                    Copy-Item -LiteralPath $runtimeSource -Destination (Join-Path $stageDir $runtimeName)
                    $stagedRuntime += $runtimeName
                } catch {
                    $missingRuntime += $runtimeName
                }
            } else {
                $missingRuntime += $runtimeName
            }
        }
    }
}
if ($stagedRuntime.Count -gt 0) {
    Write-Host "Staged VC++ runtime: $($stagedRuntime -join ', ')"
}
if ($missingRuntime.Count -gt 0) {
    Write-Host "VC++ runtime not staged: $($missingRuntime -join ', ')"
}

foreach ($name in $licenseFiles) {
    Copy-Item -LiteralPath (Join-Path $root $name) -Destination (Join-Path $stageDir $name)
}

$driverSource = Join-Path $root ("target\driver\" + $Configuration)
if ($IncludeDriver -and (Test-Path -LiteralPath $driverSource -PathType Container)) {
    $driverStage = Join-Path $stageDir "driver"
    $stagedDriverArchitectures = @()
    foreach ($archDir in @(Get-ChildItem -LiteralPath $driverSource -Directory)) {
        $driverFiles = @("mh_vmic.sys", "mh_vmic.inf", "mh_vmic.cat")
        $present = @($driverFiles | Where-Object {
            Test-Path -LiteralPath (Join-Path $archDir.FullName $_) -PathType Leaf
        })
        if ($present.Count -ne $driverFiles.Count) {
            Write-Host "Driver artifacts incomplete for $($archDir.Name); not staged."
            continue
        }

        $archStage = Join-Path $driverStage $archDir.Name
        New-Item -ItemType Directory -Force -Path $archStage | Out-Null
        foreach ($name in $driverFiles) {
            Copy-Item -LiteralPath (Join-Path $archDir.FullName $name) -Destination (Join-Path $archStage $name)
        }
        $stagedDriverArchitectures += $archDir.Name
    }

    $driverReadme = Join-Path $root "driver\README.md"
    if ($stagedDriverArchitectures.Count -gt 0) {
        if (Test-Path -LiteralPath $driverReadme -PathType Leaf) {
            Copy-Item -LiteralPath $driverReadme -Destination (Join-Path $driverStage "README.md")
        } else {
            Write-Warning "driver\README.md is missing; the staged driver package has no README."
        }
        Write-Host ("Staged driver package: driver (" + ($stagedDriverArchitectures -join ", ") + ")")
    } elseif (Test-Path -LiteralPath $driverStage -PathType Container) {
        Remove-Item -LiteralPath $driverStage -Recurse -Force
    }
}

$manifestText = [System.IO.File]::ReadAllText($manifestSource)
if ($Version -match '^\d+\.\d+\.\d+$') {
    $manifestVersion = "$Version.0"
    $manifestText = $manifestText -replace '(?<=version=")[0-9]+(\.[0-9]+){3}(?=")', $manifestVersion
} else {
    $manifestVersion = "0.1.0.0 (unchanged; -Version is not a plain three-part number)"
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $stageDir "michost.exe.manifest"), $manifestText, $utf8NoBom)

$runtimeNote = if ($missingRuntime.Count -eq 0) {
    "  The VC++ runtime DLLs (VCRUNTIME140.dll, VCRUNTIME140_1.dll and" + "`n" +
    "  MSVCP140.dll) are included in this folder."
} else {
    "  Microsoft Visual C++ 2015-2022 Redistributable (x64) is required." + "`n" +
    "  Not staged: $($missingRuntime -join ', ')." + "`n" +
    "  Install the redistributable before running michost.exe."
}

$dirtyText = if ($gitDirty) { " (dirty working tree)" } else { "" }
$runText = @"
MicHost $buildVersion
=============

Build: $buildTimeUtc UTC, commit $gitHash$dirtyText

Easy install (current user):
  Double-click Install.cmd. It copies this folder to
  %LOCALAPPDATA%\Programs\MicHost\<build>, adds one MicHost Start Menu
  shortcut, and registers an uninstall entry
  in Windows Settings > Apps. Add -VersionedShortcut for a second,
  build-versioned shortcut when keeping several builds side by side.
  Run Install.cmd -LaunchAtLogin to make this build start in the tray at login.

Run:
  1. Keep michost.exe, michost-probe.exe and mic_host.dll together in this
     folder. mic_host.dll is required next to michost.exe, and
     michost-probe.exe provides isolated VST3 scanning. Do not rename or move
     any of the three.
  2. Double-click michost.exe (or run it from a terminal). The settings window
     opens; --hidden and --autostart start in the notification tray instead.
  3. Right-click the tray icon to open the menu. Launching michost.exe again
     signals the running instance to show its window.

Prerequisites:
$runtimeNote

Paths:
  Settings and chain : %APPDATA%\MicHost\project.json
  Device cache       : %LOCALAPPDATA%\MicHost\devices.json
  Plugin cache       : %LOCALAPPDATA%\MicHost\plugins.json
  Log                : %LOCALAPPDATA%\MicHost\michost.log

Warning:
  Monitoring a microphone to speakers can cause acoustic feedback (howling).
  Use headphones or route the output to a virtual cable / non-speaker device,
  and start with the output level low.

Notices:
  THIRD_PARTY_NOTICES.md is included. michost.exe.manifest documents the app's
  DPI, privilege and Windows 10/11 compatibility settings. This build does not
  embed it as a resource.
"@
[System.IO.File]::WriteAllText((Join-Path $stageDir "RUN.txt"), ($runText -replace "`r?`n", "`r`n"), $utf8NoBom)

$builtDate = Get-Date
try {
    if ($buildTimeUtc -match '^\d{8}T\d{6}Z$') {
        $builtDate = [datetime]::ParseExact($buildTimeUtc, "yyyyMMdd'T'HHmmss'Z'", [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::AssumeUniversal)
    } else {
        $builtDate = [datetime]::Parse($buildTimeUtc, [System.Globalization.CultureInfo]::InvariantCulture, [System.Globalization.DateTimeStyles]::RoundtripKind)
    }
} catch {
    $builtDate = (Get-Date).ToUniversalTime()
}
$buildInfo = [ordered]@{
    version = $Version
    build_version = $buildVersion
    build_time = $buildTimeUtc
    built_utc = $builtDate.ToUniversalTime().ToString("o")
    git_commit = $gitHash
    git_dirty = $gitDirty
    configuration = $Configuration
}
[System.IO.File]::WriteAllText((Join-Path $stageDir "build-info.json"), ($buildInfo | ConvertTo-Json), $utf8NoBom)

$tempZip = Join-Path $outputPath (".$stageName." + [guid]::NewGuid().ToString("N") + ".tmp.zip")
Add-Type -AssemblyName System.IO.Compression | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null
$fixedZipTime = New-Object -TypeName System.DateTimeOffset -ArgumentList @(1980, 1, 1, 0, 0, 0, [System.TimeSpan]::Zero)
$stageFull = Trim-DirectorySeparators ([System.IO.Path]::GetFullPath($stageDir))
$relativePaths = New-Object "System.Collections.Generic.List[string]"
Get-ChildItem -LiteralPath $stageDir -Recurse -File | ForEach-Object {
    $relativePaths.Add($_.FullName.Substring($stageFull.Length).TrimStart([char[]]@('\', '/')))
}
$relativePaths.Sort([System.StringComparer]::Ordinal)
try {
    $zipStream = [System.IO.File]::Open($tempZip, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $archive = New-Object -TypeName System.IO.Compression.ZipArchive -ArgumentList @($zipStream, [System.IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($relativePath in $relativePaths) {
                $entryName = "$stageName/" + $relativePath.Replace('\', '/')
                $entry = $archive.CreateEntry($entryName, [System.IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime = $fixedZipTime
                $entryStream = $entry.Open()
                try {
                    $sourceStream = [System.IO.File]::OpenRead((Join-Path $stageDir $relativePath))
                    try {
                        $sourceStream.CopyTo($entryStream)
                    } finally {
                        $sourceStream.Dispose()
                    }
                } finally {
                    $entryStream.Dispose()
                }
            }
        } finally {
            $archive.Dispose()
        }
    } finally {
        $zipStream.Dispose()
    }
    Move-Item -LiteralPath $tempZip -Destination $zipPath -Force
} finally {
    if (Test-Path -LiteralPath $tempZip) {
        Remove-Item -LiteralPath $tempZip -Force
    }
}

$stageSize = Get-TreeSize $stageDir
$zipSize = (Get-Item -LiteralPath $zipPath).Length
$zipHash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
$fileCount = @(Get-ChildItem -LiteralPath $stageDir -Recurse -File).Count

Write-Host ""
Write-Host "Package: $stageDir"
Write-Host "  Files: $fileCount"
Write-Host "  Size : $(Format-Size $stageSize)"
Write-Host "Zip    : $zipPath"
Write-Host "  Size : $(Format-Size $zipSize) ($zipSize bytes)"
Write-Host "  SHA256: $zipHash"
Write-Host "Build  : $buildVersion"
Write-Host "Commit : $gitHash$(if ($gitDirty) { ' (dirty)' } else { '' })"
Write-Host "Manifest identity: $manifestVersion"
