[CmdletBinding()]
param(
    [string]$PackageDir,
    [string]$InstallDir,
    [switch]$LaunchAtLogin,
    [switch]$DesktopShortcut,
    [switch]$NoStartMenuShortcut,
    [switch]$NoLatestShortcut,
    [switch]$VersionedShortcut,
    [switch]$Uninstall,
    [switch]$Force,
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function Write-Status([string]$Text) {
    if (-not $Quiet) {
        Write-Host $Text
    }
}

function Get-FullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    $trimmed = $full.TrimEnd([char[]]@('\', '/'))
    if ($trimmed.Length -lt $root.Length) {
        return $root
    }
    return $trimmed
}

function Test-SamePath([string]$Left, [string]$Right) {
    try {
        return [string]::Equals(
            (Get-FullPath $Left),
            (Get-FullPath $Right),
            [System.StringComparison]::OrdinalIgnoreCase)
    } catch {
        return $false
    }
}

function Test-PathUnder([string]$Path, [string]$Parent) {
    try {
        if (Test-SamePath $Path $Parent) { return $false }
        $inner = (Get-FullPath $Path).TrimEnd([char[]]@('\', '/')) + "\"
        $outer = (Get-FullPath $Parent).TrimEnd([char[]]@('\', '/')) + "\"
        return $inner.StartsWith($outer, [System.StringComparison]::OrdinalIgnoreCase)
    } catch {
        return $false
    }
}

function Test-SafePathSegment([string]$Name) {
    if ([string]::IsNullOrWhiteSpace($Name)) { return $false }
    if ($Name -eq "." -or $Name -eq "..") { return $false }
    if ($Name.IndexOfAny([char[]]@('\', '/')) -ge 0) { return $false }
    if ([System.IO.Path]::IsPathRooted($Name)) { return $false }
    if ($Name -ne $Name.Trim()) { return $false }
    foreach ($invalid in [System.IO.Path]::GetInvalidFileNameChars()) {
        if ($Name.IndexOf($invalid) -ge 0) { return $false }
    }
    return $true
}

function Test-MicHostInstall([string]$Directory) {
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { return $false }
    $exe = Join-Path $Directory "michost.exe"
    $info = Join-Path $Directory "build-info.json"
    return ((Test-Path -LiteralPath $exe -PathType Leaf) -and
        (Test-Path -LiteralPath $info -PathType Leaf))
}

function Get-ProgramsFolder() {
    $programs = [Environment]::GetFolderPath("Programs")
    if ([string]::IsNullOrWhiteSpace($programs)) {
        $programs = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    }
    return $programs
}

function Get-LocalProgramsRoot() {
    $local = [Environment]::GetFolderPath("LocalApplicationData")
    if ([string]::IsNullOrWhiteSpace($local)) {
        $local = $env:LOCALAPPDATA
    }
    if ([string]::IsNullOrWhiteSpace($local)) {
        throw "Cannot resolve the per-user LocalAppData folder."
    }
    return Join-Path (Join-Path $local "Programs") "MicHost"
}

function Get-RunningInstall([string]$ExePath) {
    $full = Get-FullPath $ExePath
    return @(Get-Process -Name michost -ErrorAction SilentlyContinue | Where-Object {
        try {
            $_.Path -and ([string]::Equals(
                (Get-FullPath $_.Path),
                $full,
                [System.StringComparison]::OrdinalIgnoreCase))
        } catch {
            $false
        }
    })
}

function New-Shortcut([string]$Path, [string]$Target, [string]$WorkingDirectory, [string]$Description) {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($Path)
    $shortcut.TargetPath = $Target
    $shortcut.WorkingDirectory = $WorkingDirectory
    $shortcut.IconLocation = "$Target,0"
    $shortcut.Description = $Description
    $shortcut.Save()
}

function Remove-ShortcutsTargeting([string]$Folder, [string]$Target) {
    if (-not (Test-Path -LiteralPath $Folder -PathType Container)) {
        return
    }
    $shell = New-Object -ComObject WScript.Shell
    Get-ChildItem -LiteralPath $Folder -Filter "*.lnk" -File -ErrorAction SilentlyContinue |
        ForEach-Object {
            try {
                $shortcut = $shell.CreateShortcut($_.FullName)
                if ($shortcut.TargetPath -and (Test-SamePath $shortcut.TargetPath $Target)) {
                    Remove-Item -LiteralPath $_.FullName -Force
                }
            } catch {
            }
        }
}

function Remove-EmptyFolder([string]$Folder) {
    if (-not (Test-Path -LiteralPath $Folder -PathType Container)) {
        return
    }
    $remaining = @(Get-ChildItem -LiteralPath $Folder -Force -ErrorAction SilentlyContinue)
    if ($remaining.Count -eq 0) {
        Remove-Item -LiteralPath $Folder -Force
    }
}

function Read-BuildInfo([string]$Directory) {
    $path = Join-Path $Directory "build-info.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return $null
    }
    return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
}

function Get-BuildId($Info) {
    if ($null -ne $Info) {
        if ([string]::IsNullOrWhiteSpace([string]$Info.build_version)) {
            throw "build-info.json has no build_version. Refusing to derive an install path from it."
        }
        $candidate = [regex]::Replace([string]$Info.build_version, "[^0-9A-Za-z._-]", "-").Trim([char[]]@('-', '.'))
        if (-not (Test-SafePathSegment $candidate)) {
            throw ("build-info.json contains an unsafe build_version '$([string]$Info.build_version)'. " +
                "Refusing to derive an install path from it; repackage with scripts/package.ps1.")
        }
        return $candidate
    }
    return "local-" + (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
}

function Get-ShortcutLabel($Info) {
    $version = "0.1.0"
    $commit = "local"
    $dirty = $false
    $built = Get-Date
    if ($null -ne $Info) {
        if (-not ([string]::IsNullOrWhiteSpace([string]$Info.version))) { $version = [string]$Info.version }
        if (-not ([string]::IsNullOrWhiteSpace([string]$Info.git_commit))) { $commit = [string]$Info.git_commit }
        if ($null -ne $Info.git_dirty) { $dirty = [bool]$Info.git_dirty }
        if (-not ([string]::IsNullOrWhiteSpace([string]$Info.built_utc))) {
            try {
                $built = [datetime]::Parse(
                    [string]$Info.built_utc,
                    [System.Globalization.CultureInfo]::InvariantCulture,
                    [System.Globalization.DateTimeStyles]::RoundtripKind)
            } catch {
                $built = Get-Date
            }
        }
    }
    $dirtyLabel = ""
    if ($dirty) { $dirtyLabel = ", dirty" }
    return "MicHost $version ($($built.ToLocalTime().ToString('yyyy-MM-dd HHmm')), $commit$dirtyLabel)"
}

function Remove-UninstallEntry([string]$BuildId) {
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MicHost-$BuildId"
    if (Test-Path -LiteralPath $key) {
        Remove-Item -LiteralPath $key -Recurse -Force
    }
}

if ($Uninstall) {
    $explicitInstallDir = -not ([string]::IsNullOrWhiteSpace($InstallDir))
    if (-not $explicitInstallDir) {
        $InstallDir = $PSScriptRoot
        $programsRoot = Get-LocalProgramsRoot
        $rootPrefix = (Get-FullPath $programsRoot) + "\"
        $insideRoot = (Get-FullPath $InstallDir).StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)
        if (-not (Test-SamePath $InstallDir $programsRoot) -and -not $insideRoot) {
            throw "Refusing to uninstall '$InstallDir' outside the per-user MicHost install root. Pass -InstallDir explicitly."
        }
    }
    $InstallDir = Get-FullPath $InstallDir
    $exe = Join-Path $InstallDir "michost.exe"
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw "MicHost is not installed at '$InstallDir'."
    }
    $running = Get-RunningInstall $exe
    if ($running.Count -gt 0) {
        throw "MicHost is running from '$InstallDir'. Quit it from the tray, then run the uninstaller again."
    }

    $info = Read-BuildInfo $InstallDir
    if ($null -eq $info) {
        throw "Refusing to uninstall '$InstallDir': build-info.json is missing, so it does not look like a MicHost install."
    }
    $buildId = Get-BuildId $info
    $label = Get-ShortcutLabel $info
    $group = Join-Path (Get-ProgramsFolder) "MicHost"
    Remove-ShortcutsTargeting $group $exe
    Remove-ShortcutsTargeting ([Environment]::GetFolderPath("Desktop")) $exe
    Remove-EmptyFolder $group

    $runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    $runValue = (Get-ItemProperty -Path $runKey -Name MicHost -ErrorAction SilentlyContinue).MicHost
    if ($runValue -and ([string]$runValue).IndexOf($exe, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
        Remove-ItemProperty -Path $runKey -Name MicHost -Force
    }

    Remove-UninstallEntry $buildId
    Write-Status "Uninstalled $label"
    Write-Status "  Folder: $InstallDir"

    $tempDir = $env:TEMP
    if ([string]::IsNullOrWhiteSpace($tempDir) -or -not (Test-Path -LiteralPath $tempDir -PathType Container)) {
        $tempDir = [System.IO.Path]::GetTempPath()
    }
    $cleanupPath = Join-Path $tempDir ("michost-uninstall-" + [guid]::NewGuid().ToString("N") + ".ps1")
    $escapedInstallDir = $InstallDir.Replace("'", "''")
    $cleanup = @"
`$ErrorActionPreference = 'SilentlyContinue'
`$target = '$escapedInstallDir'
if (-not (Test-Path -LiteralPath (Join-Path `$target 'michost.exe') -PathType Leaf) -or
    -not (Test-Path -LiteralPath (Join-Path `$target 'build-info.json') -PathType Leaf)) {
    Remove-Item -LiteralPath `$PSCommandPath -Force
    exit 1
}
for (`$attempt = 0; `$attempt -lt 40; `$attempt++) {
    if (-not (Test-Path -LiteralPath `$target)) { break }
    Remove-Item -LiteralPath `$target -Recurse -Force
    if (-not (Test-Path -LiteralPath `$target)) { break }
    Start-Sleep -Milliseconds 500
}
Remove-Item -LiteralPath `$PSCommandPath -Force
"@
    $cleanupEncoding = New-Object System.Text.UTF8Encoding($true)
    [System.IO.File]::WriteAllText($cleanupPath, $cleanup, $cleanupEncoding)
    Start-Process -FilePath "powershell.exe" `
        -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$cleanupPath`"") `
        -WorkingDirectory $tempDir -WindowStyle Hidden | Out-Null
    exit 0
}

if ([string]::IsNullOrWhiteSpace($PackageDir)) {
    if (-not [string]::IsNullOrWhiteSpace($PSScriptRoot)) {
        $PackageDir = $PSScriptRoot
    } else {
        $PackageDir = (Get-Location).Path
    }
}
$PackageDir = Get-FullPath $PackageDir
$exeSource = Join-Path $PackageDir "michost.exe"
if (-not (Test-Path -LiteralPath $exeSource -PathType Leaf)) {
    $distRoot = Join-Path (Split-Path -Parent $PackageDir) "target\dist"
    if (Test-Path -LiteralPath $distRoot -PathType Container) {
        $candidate = Get-ChildItem -LiteralPath $distRoot -Directory |
            Where-Object {
                (Test-Path -LiteralPath (Join-Path $_.FullName "michost.exe") -PathType Leaf) -and
                (Test-Path -LiteralPath (Join-Path $_.FullName "build-info.json") -PathType Leaf)
            } |
            Sort-Object -Property LastWriteTime -Descending |
            Select-Object -First 1
        if ($null -ne $candidate) {
            $PackageDir = $candidate.FullName
            $exeSource = Join-Path $PackageDir "michost.exe"
            Write-Status "Using the newest package: $PackageDir"
        }
    }
}
if (-not (Test-Path -LiteralPath $exeSource -PathType Leaf)) {
    throw "michost.exe was not found in '$PackageDir'. Run Install.cmd from the extracted MicHost folder, or build a package with scripts/package.ps1."
}
$info = Read-BuildInfo $PackageDir
if ($null -eq $info) {
    throw "build-info.json was not found in '$PackageDir'. Rebuild the package with scripts/package.ps1."
}

$buildId = Get-BuildId $info
$installRoot = Get-LocalProgramsRoot
$usingDefaultInstallDir = [string]::IsNullOrWhiteSpace($InstallDir)
if ($usingDefaultInstallDir) {
    $InstallDir = Join-Path $installRoot $buildId
}
$InstallDir = Get-FullPath $InstallDir
if (Test-SamePath $InstallDir ([System.IO.Path]::GetPathRoot($InstallDir))) {
    throw "Refusing to install into the filesystem root '$InstallDir'."
}
if (Test-SamePath $InstallDir $installRoot) {
    throw "Refusing to install into the per-user MicHost install root itself ('$InstallDir'); choose a build subfolder."
}
if ($usingDefaultInstallDir -and
    -not (Test-SamePath ([System.IO.Path]::GetDirectoryName($InstallDir)) $installRoot)) {
    throw "Refusing default install directory '$InstallDir': it is not a direct child of '$installRoot'."
}
$exe = Join-Path $InstallDir "michost.exe"

$running = Get-RunningInstall $exe
if ($running.Count -gt 0) {
    throw "MicHost is already running from '$InstallDir'. Quit it from the tray, then run the installer again."
}

if ((Test-SamePath $PackageDir $InstallDir) -eq $false) {
    if (Test-PathUnder $InstallDir $PackageDir) {
        throw ("Refusing to install into '$InstallDir' because it is inside the package folder '$PackageDir'. " +
            "Extract the package elsewhere or choose a different -InstallDir.")
    }
    $installParent = [System.IO.Path]::GetDirectoryName($InstallDir)
    $installLeaf = [System.IO.Path]::GetFileName($InstallDir)
    New-Item -ItemType Directory -Force -Path $installParent | Out-Null

    $targetExists = Test-Path -LiteralPath $InstallDir
    if ($targetExists) {
        if (-not (Test-Path -LiteralPath $InstallDir -PathType Container)) {
            throw "Cannot install to '$InstallDir': a file already exists at that path."
        }
        if (-not (Test-MicHostInstall $InstallDir)) {
            throw ("Refusing to replace '$InstallDir': it exists but does not look like a MicHost install " +
                "(build-info.json and michost.exe are required). Choose a different -InstallDir or remove it manually.")
        }
        if (Test-PathUnder $PackageDir $InstallDir) {
            throw "Refusing to replace '$InstallDir': the package folder '$PackageDir' is inside it."
        }
        if (-not $Force) {
            Write-Status "Reinstalling the existing build in '$InstallDir'."
        }
    }

    $stagingDir = Join-Path $installParent (".$installLeaf.staging-" + [guid]::NewGuid().ToString("N"))
    $backupDir = $null
    try {
        New-Item -ItemType Directory -Path $stagingDir | Out-Null
        Get-ChildItem -LiteralPath $PackageDir -Force | ForEach-Object {
            Copy-Item -LiteralPath $_.FullName -Destination $stagingDir -Recurse -Force
        }
        foreach ($required in @("michost.exe", "michost-probe.exe", "mic_host.dll", "build-info.json", "install.ps1", "Install.cmd")) {
            if (-not (Test-Path -LiteralPath (Join-Path $stagingDir $required) -PathType Leaf)) {
                throw "The staged copy is incomplete: '$required' is missing from '$PackageDir'."
            }
        }

        if ($targetExists) {
            $backupDir = Join-Path $installParent (".$installLeaf.previous-" + [guid]::NewGuid().ToString("N"))
            Move-Item -LiteralPath $InstallDir -Destination $backupDir
            try {
                Move-Item -LiteralPath $stagingDir -Destination $InstallDir
            } catch {
                $swapError = $_
                $restored = $false
                try {
                    Move-Item -LiteralPath $backupDir -Destination $InstallDir -ErrorAction Stop
                    $restored = $true
                } catch {
                }
                if (-not $restored) {
                    throw ("Installing to '$InstallDir' failed and the previous build could not be restored. " +
                        "The previous build is kept at '$backupDir'. Error: $($swapError.Exception.Message)")
                }
                throw
            }
            if (Test-MicHostInstall $backupDir) {
                try {
                    Remove-Item -LiteralPath $backupDir -Recurse -Force
                    $backupDir = $null
                } catch {
                    Write-Warning "The previous build could not be removed from '$backupDir': $($_.Exception.Message)"
                }
            }
        } else {
            Move-Item -LiteralPath $stagingDir -Destination $InstallDir
        }
    } finally {
        if (Test-Path -LiteralPath $stagingDir) {
            $stagingParent = [System.IO.Path]::GetDirectoryName($stagingDir)
            $stagingLeaf = [System.IO.Path]::GetFileName($stagingDir)
            if ((Test-SamePath $stagingParent $installParent) -and
                $stagingLeaf.StartsWith(".$installLeaf.staging-", [System.StringComparison]::Ordinal)) {
                Remove-Item -LiteralPath $stagingDir -Recurse -Force
            }
        }
    }
} else {
    Write-Status "Refreshing shortcuts for the existing install in '$InstallDir'."
}

$label = Get-ShortcutLabel $info
$buildVersion = [string]$info.build_version
$group = Join-Path (Get-ProgramsFolder) "MicHost"

if (-not $NoStartMenuShortcut) {
    New-Item -ItemType Directory -Force -Path $group | Out-Null
    if ($VersionedShortcut) {
        $shortcutName = [regex]::Replace($label, '[<>:"/\\|?*\x00-\x1F]', "-")
        $shortcutPath = Join-Path $group "$shortcutName.lnk"
        New-Shortcut $shortcutPath $exe $InstallDir "MicHost $buildVersion"
        Write-Status "Start Menu: $shortcutPath"
    }
    if (-not $NoLatestShortcut) {
        $latestPath = Join-Path $group "MicHost.lnk"
        New-Shortcut $latestPath $exe $InstallDir "MicHost $buildVersion"
        Write-Status "MicHost:    $latestPath"
    }
}
if ($DesktopShortcut) {
    $desktop = [Environment]::GetFolderPath("Desktop")
    $shortcutName = [regex]::Replace($label, '[<>:"/\\|?*\x00-\x1F]', "-")
    $desktopPath = Join-Path $desktop "$shortcutName.lnk"
    New-Shortcut $desktopPath $exe $InstallDir "MicHost $buildVersion"
    Write-Status "Desktop:   $desktopPath"
}

if ($LaunchAtLogin) {
    $runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    Set-ItemProperty -Path $runKey -Name MicHost -Value "`"$exe`" --autostart"
    Write-Status "Login:     this build starts in the tray at login"
}

$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\MicHost-$buildId"
New-Item -Path $uninstallKey -Force | Out-Null
$sizeBytes = (Get-ChildItem -LiteralPath $InstallDir -Recurse -File | Measure-Object -Property Length -Sum).Sum
$sizeKb = [int](($sizeBytes + 1023) / 1024)
$uninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$InstallDir\install.ps1`" -Uninstall -InstallDir `"$InstallDir`" -Quiet"
Set-ItemProperty -Path $uninstallKey -Name DisplayName -Value "MicHost $buildVersion"
Set-ItemProperty -Path $uninstallKey -Name DisplayVersion -Value ([string]$info.version)
Set-ItemProperty -Path $uninstallKey -Name Publisher -Value "MicHost"
Set-ItemProperty -Path $uninstallKey -Name InstallLocation -Value $InstallDir
Set-ItemProperty -Path $uninstallKey -Name DisplayIcon -Value "$exe,0"
Set-ItemProperty -Path $uninstallKey -Name UninstallString -Value $uninstallString
Set-ItemProperty -Path $uninstallKey -Name EstimatedSize -Value $sizeKb -Type DWord
New-ItemProperty -Path $uninstallKey -Name NoModify -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $uninstallKey -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null

$uninstallCmd = @"
@echo off
cd /d "%TEMP%"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" -Uninstall -Quiet
if not "%MICHOST_NO_PAUSE%"=="1" pause
"@
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $InstallDir "Uninstall.cmd"), $uninstallCmd, $utf8NoBom)

Write-Status ""
Write-Status "Installed $label"
Write-Status "  Folder: $InstallDir"
Write-Status "  Build:  $buildVersion"
Write-Status ""
Write-Status "Start Menu: search for 'MicHost'; the shortcut is available from the Start Menu."
Write-Status "If the new shortcut does not appear yet, sign out and back in."
Write-Status "MicHost allows one running instance; launching another copy signals the running one to show."
Write-Status "Uninstall from Windows Settings > Apps, or run Uninstall.cmd in the install folder."
