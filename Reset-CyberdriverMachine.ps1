[CmdletBinding()]
param(
    [string]$LogPath = (Join-Path $PSScriptRoot ("cyberdriver-reset-{0}.log" -f (Get-Date -Format "yyyyMMdd-HHmmss")))
)

$ErrorActionPreference = "Continue"
$script:Removed = New-Object System.Collections.Generic.List[object]
$script:Failures = New-Object System.Collections.Generic.List[string]
$script:RebootRecommended = $false

function Write-Section {
    param([string]$Name)
    Write-Host ""
    Write-Host ("==== {0} ====" -f $Name)
}

function Add-Removed {
    param(
        [string]$Category,
        [string]$Detail
    )
    $script:Removed.Add([pscustomobject]@{ Category = $Category; Detail = $Detail }) | Out-Null
    Write-Host ("[REMOVED] {0}: {1}" -f $Category, $Detail)
}

function Add-Failure {
    param([string]$Message)
    $script:Failures.Add($Message) | Out-Null
    Write-Warning $Message
}

function Test-IsAdministrator {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-TextForProcess {
    param($Process)
    return (($Process.Name, $Process.ExecutablePath, $Process.CommandLine) -join " ")
}

function Stop-TargetProcesses {
    Write-Section "Stopping Cyberdriver/RustDesk processes"
    $targetNames = @("cyberdriver", "rustdesk", "runtimebroker")
    $targetPathRegex = "(?i)(cyberdriver|rustdesk|\.cyberdriver|Program Files\\Cyberdriver|Program Files \(x86\)\\Cyberdriver|ProgramData\\Cyberdriver|ProgramData\\RustDesk|ServiceProfiles\\LocalService\\AppData\\(Roaming|Local)\\(Cyberdriver|RustDesk))"
    $leftoverRegex = "(?i)(cyberdriver-updater|cyberdriver-update\.exe|launch-hidden\.(ps1|vbs))"

    $processes = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        $name = [IO.Path]::GetFileNameWithoutExtension($_.Name).ToLowerInvariant()
        $text = Get-TextForProcess $_
        (($targetNames -contains $name) -and ($text -match $targetPathRegex)) -or ($text -match $leftoverRegex)
    }

    foreach ($process in $processes) {
        if ($process.ProcessId -eq $PID) {
            continue
        }
        try {
            Stop-Process -Id $process.ProcessId -Force -ErrorAction Stop
            Add-Removed "Process" ("{0} (PID {1})" -f $process.Name, $process.ProcessId)
        }
        catch {
            Add-Failure ("Failed to stop process {0} (PID {1}): {2}" -f $process.Name, $process.ProcessId, $_.Exception.Message)
            $script:RebootRecommended = $true
        }
    }
}

function Get-TargetServices {
    return @(Get-CimInstance Win32_Service -ErrorAction SilentlyContinue | Where-Object {
        ($_.Name -match "(?i)(cyberdriver|rustdesk)") -or ($_.DisplayName -match "(?i)(cyberdriver|rustdesk)")
    })
}

function Stop-TargetServices {
    Write-Section "Stopping Cyberdriver/RustDesk services"
    foreach ($service in Get-TargetServices) {
        if ($service.State -eq "Stopped") {
            Write-Host ("Service already stopped: {0}" -f $service.Name)
            continue
        }
        try {
            Stop-Service -Name $service.Name -Force -ErrorAction Stop
            Add-Removed "Stopped service" $service.Name
        }
        catch {
            Add-Failure ("Failed to stop service {0}: {1}" -f $service.Name, $_.Exception.Message)
            $script:RebootRecommended = $true
        }
    }
}

function Remove-TargetServices {
    Write-Section "Deleting Cyberdriver/RustDesk services"
    foreach ($service in Get-TargetServices) {
        try {
            if ($service.State -ne "Stopped") {
                Stop-Service -Name $service.Name -Force -ErrorAction SilentlyContinue
            }
            $output = & sc.exe delete $service.Name 2>&1
            $exitCode = $LASTEXITCODE
            if ($exitCode -eq 0) {
                Add-Removed "Service" $service.Name
            }
            else {
                Add-Failure ("Failed to delete service {0}: {1}" -f $service.Name, (($output | Out-String).Trim()))
                $script:RebootRecommended = $true
            }
        }
        catch {
            Add-Failure ("Failed to delete service {0}: {1}" -f $service.Name, $_.Exception.Message)
            $script:RebootRecommended = $true
        }
    }
}

function Invoke-CyberdriverStop {
    Write-Section "Trying cyberdriver stop"
    $command = Get-Command cyberdriver -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $command) {
        Write-Host "cyberdriver command was not found on PATH."
        return
    }

    try {
        $commandPath = if ($command.Path) { $command.Path } else { $command.Source }
        & $commandPath stop --force --timeout 3 2>&1 | ForEach-Object { Write-Host $_ }
        Write-Host ("cyberdriver stop exited with code {0}" -f $LASTEXITCODE)
    }
    catch {
        Add-Failure ("cyberdriver stop failed: {0}" -f $_.Exception.Message)
    }
}

function Get-CyberdriverUninstallEntries {
    # This reset script runs elevated, so do not consume HKCU uninstall commands.
    # A non-admin user can plant HKCU uninstall entries; elevated cleanup must
    # only use machine-wide MSI product codes, never registry command strings.
    $roots = @(
        "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"
    )

    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) {
            continue
        }
        Get-ChildItem -LiteralPath $root -ErrorAction SilentlyContinue | ForEach-Object {
            $item = $_
            $props = Get-ItemProperty -LiteralPath $item.PSPath -ErrorAction SilentlyContinue
            if ($props.DisplayName -match "(?i)cyberdriver") {
                [pscustomobject]@{
                    DisplayName = $props.DisplayName
                    DisplayVersion = $props.DisplayVersion
                    Publisher = $props.Publisher
                    ProductCode = $item.PSChildName
                    UninstallString = $props.UninstallString
                    RegistryPath = $item.PSPath
                }
            }
        }
    }
}

function Invoke-CyberdriverUninstall {
    Write-Section "Uninstalling Cyberdriver MSI entries"
    $entries = @(Get-CyberdriverUninstallEntries | Sort-Object RegistryPath -Unique)
    if ($entries.Count -eq 0) {
        Write-Host "No Cyberdriver uninstall entries found."
        return
    }

    foreach ($entry in $entries) {
        Write-Host ("Found uninstall entry: {0} {1}" -f $entry.DisplayName, $entry.DisplayVersion)
        $productCode = $null
        if ($entry.ProductCode -match "^\{[0-9A-Fa-f-]{36}\}$") {
            $productCode = $entry.ProductCode
        }
        elseif ($entry.UninstallString -match "\{[0-9A-Fa-f-]{36}\}") {
            $productCode = $Matches[0]
        }

        if ($productCode) {
            try {
                $process = Start-Process -FilePath "msiexec.exe" -ArgumentList @("/x", $productCode, "/qn", "/norestart") -Wait -PassThru
                if ($process.ExitCode -in @(0, 1605, 1614)) {
                    Add-Removed "MSI uninstall" ("{0} ({1}) exit {2}" -f $entry.DisplayName, $productCode, $process.ExitCode)
                }
                elseif ($process.ExitCode -in @(1641, 3010)) {
                    Add-Removed "MSI uninstall" ("{0} ({1}) exit {2}" -f $entry.DisplayName, $productCode, $process.ExitCode)
                    $script:RebootRecommended = $true
                }
                else {
                    Add-Failure ("MSI uninstall failed for {0} ({1}) with exit code {2}" -f $entry.DisplayName, $productCode, $process.ExitCode)
                    $script:RebootRecommended = $true
                }
            }
            catch {
                Add-Failure ("MSI uninstall threw for {0}: {1}" -f $entry.DisplayName, $_.Exception.Message)
                $script:RebootRecommended = $true
            }
        }
        else {
            Add-Failure ("No MSI product code found for {0}; skipping registry command execution" -f $entry.DisplayName)
        }
    }
}

function Remove-TargetScheduledTasks {
    Write-Section "Removing Cyberdriver/RustDesk scheduled tasks"
    try {
        $tasks = @(Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object {
            $actionText = ($_.Actions | ForEach-Object { "{0} {1}" -f $_.Execute, $_.Arguments }) -join " "
            $taskText = "{0} {1} {2} {3}" -f $_.TaskName, $_.TaskPath, $_.Description, $actionText
            ($_.TaskName -like "Cyberdriver*") -or
            ($_.TaskName -like "CyberdriverRestart_*") -or
            ($taskText -match "(?i)(cyberdriver|rustdesk|\.cyberdriver|cyberdriver-updater|cyberdriver-update|launch-hidden\.(ps1|vbs))")
        })

        foreach ($task in $tasks) {
            try {
                Unregister-ScheduledTask -TaskName $task.TaskName -TaskPath $task.TaskPath -Confirm:$false -ErrorAction Stop
                Add-Removed "Scheduled task" ("{0}{1}" -f $task.TaskPath, $task.TaskName)
            }
            catch {
                Add-Failure ("Failed to remove scheduled task {0}{1}: {2}" -f $task.TaskPath, $task.TaskName, $_.Exception.Message)
            }
        }
    }
    catch {
        Add-Failure ("Scheduled task cleanup failed: {0}" -f $_.Exception.Message)
    }
}

function Remove-PathIfExists {
    param(
        [string]$Path,
        [string]$Category = "Path"
    )
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    try {
        Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
        Add-Removed $Category $Path
    }
    catch {
        Add-Failure ("Failed to remove {0}: {1}" -f $Path, $_.Exception.Message)
        $script:RebootRecommended = $true
    }
}

function Add-UniquePath {
    param(
        [System.Collections.Generic.List[string]]$List,
        [string]$Path
    )
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return
    }
    if (-not $List.Contains($Path)) {
        $List.Add($Path) | Out-Null
    }
}

function Get-WindowsDir {
    foreach ($path in @($env:windir, $env:SystemRoot)) {
        if (-not [string]::IsNullOrWhiteSpace($path)) {
            return $path
        }
    }
    return (Join-Path $env:SystemDrive "Windows")
}

function Get-ProgramDataDir {
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramData)) {
        return $env:ProgramData
    }
    return (Join-Path $env:SystemDrive "ProgramData")
}

function Get-ProgramFilesDirs {
    $paths = New-Object System.Collections.Generic.List[string]
    foreach ($path in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        Add-UniquePath -List $paths -Path $path
    }
    return @($paths)
}

function Get-ServiceProfileAppDataDir {
    $windowsDir = Get-WindowsDir
    return (Join-Path $windowsDir "ServiceProfiles\LocalService\AppData")
}

function Get-UserProfileRoots {
    $roots = New-Object System.Collections.Generic.List[string]

    if ($env:USERPROFILE) {
        Add-UniquePath -List $roots -Path (Split-Path -Parent $env:USERPROFILE)
    }
    if ($env:SystemDrive) {
        Add-UniquePath -List $roots -Path (Join-Path $env:SystemDrive "Users")
    }

    Get-CimInstance Win32_UserProfile -ErrorAction SilentlyContinue | Where-Object {
        -not $_.Special -and -not [string]::IsNullOrWhiteSpace($_.LocalPath)
    } | ForEach-Object {
        Add-UniquePath -List $roots -Path (Split-Path -Parent $_.LocalPath)
    }

    return @($roots)
}

function Get-UserProfileDirs {
    $candidatePaths = New-Object System.Collections.Generic.List[string]
    if ($env:USERPROFILE) {
        $candidatePaths.Add($env:USERPROFILE) | Out-Null
    }

    Get-CimInstance Win32_UserProfile -ErrorAction SilentlyContinue | Where-Object {
        -not $_.Special -and -not [string]::IsNullOrWhiteSpace($_.LocalPath)
    } | ForEach-Object {
        $candidatePaths.Add($_.LocalPath) | Out-Null
    }

    foreach ($root in Get-UserProfileRoots) {
        if (Test-Path -LiteralPath $root) {
            Get-ChildItem -LiteralPath $root -Directory -Force -ErrorAction SilentlyContinue | ForEach-Object {
                $candidatePaths.Add($_.FullName) | Out-Null
            }
        }
    }

    $seen = @{}
    foreach ($path in $candidatePaths) {
        if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path -LiteralPath $path -PathType Container)) {
            continue
        }
        $item = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        if (-not $item) {
            continue
        }
        $key = $item.FullName.ToLowerInvariant()
        if (-not $seen.ContainsKey($key)) {
            $seen[$key] = $true
            $item
        }
    }
}

function Remove-ConfigAndInstallPaths {
    Write-Section "Removing config, install, and data folders"
    $paths = New-Object System.Collections.Generic.List[string]
    $programDataDir = Get-ProgramDataDir
    $serviceProfileAppData = Get-ServiceProfileAppDataDir

    @(
        (Join-Path $env:USERPROFILE ".cyberdriver"),
        (Join-Path $env:LOCALAPPDATA ".cyberdriver"),
        (Join-Path $env:APPDATA ".cyberdriver"),
        (Join-Path $programDataDir "Cyberdriver"),
        (Join-Path $programDataDir "RustDesk"),
        (Join-Path $serviceProfileAppData "Roaming\Cyberdriver"),
        (Join-Path $serviceProfileAppData "Local\Cyberdriver"),
        (Join-Path $serviceProfileAppData "Roaming\RustDesk"),
        (Join-Path $serviceProfileAppData "Local\RustDesk")
    ) | ForEach-Object {
        if ($_ -and -not $paths.Contains($_)) {
            $paths.Add($_) | Out-Null
        }
    }

    foreach ($programFilesDir in Get-ProgramFilesDirs) {
        $path = Join-Path $programFilesDir "Cyberdriver"
        if (-not $paths.Contains($path)) {
            $paths.Add($path) | Out-Null
        }
    }

    foreach ($profile in Get-UserProfileDirs) {
        @(
            (Join-Path $profile.FullName ".cyberdriver"),
            (Join-Path $profile.FullName "AppData\Local\.cyberdriver"),
            (Join-Path $profile.FullName "AppData\Roaming\.cyberdriver"),
            (Join-Path $profile.FullName "AppData\Local\Cyberdriver"),
            (Join-Path $profile.FullName "AppData\Roaming\Cyberdriver"),
            (Join-Path $profile.FullName "AppData\Local\RustDesk"),
            (Join-Path $profile.FullName "AppData\Roaming\RustDesk")
        ) | ForEach-Object {
            if ($_ -and -not $paths.Contains($_)) {
                $paths.Add($_) | Out-Null
            }
        }
    }

    foreach ($path in $paths) {
        Remove-PathIfExists -Path $path -Category "Folder"
    }
}

function Remove-UpdaterLeftovers {
    Write-Section "Removing old updater/launcher leftovers"
    $nameRegex = "(?i)^(cyberdriver-updater.*|cyberdriver-update\.exe|launch-hidden\.(ps1|vbs))$"
    $roots = New-Object System.Collections.Generic.List[string]
    @(Get-ProgramDataDir) | ForEach-Object {
        if ($_ -and -not $roots.Contains($_)) { $roots.Add($_) | Out-Null }
    }
    foreach ($profile in Get-UserProfileDirs) {
        @(
            (Join-Path $profile.FullName "AppData\Local"),
            (Join-Path $profile.FullName "AppData\Roaming"),
            (Join-Path $profile.FullName "AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup")
        ) | ForEach-Object {
            if ($_ -and (Test-Path -LiteralPath $_) -and -not $roots.Contains($_)) {
                $roots.Add($_) | Out-Null
            }
        }
    }

    foreach ($root in $roots) {
        Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue | Where-Object {
            $_.Name -match $nameRegex
        } | Sort-Object FullName -Descending | ForEach-Object {
            Remove-PathIfExists -Path $_.FullName -Category "Updater leftover"
        }
    }
}

function Normalize-PathEntry {
    param([string]$Entry)
    if ($null -eq $Entry) {
        return ""
    }
    $trimmed = $Entry.Trim().Trim('"').TrimEnd("\")
    try {
        $expanded = [Environment]::ExpandEnvironmentVariables($trimmed).TrimEnd("\")
        return ([IO.Path]::GetFullPath($expanded)).TrimEnd("\")
    }
    catch {
        return $trimmed
    }
}

function Test-IsTargetPathEntry {
    param([string]$Entry)
    if ([string]::IsNullOrWhiteSpace($Entry)) {
        return $false
    }

    $raw = $Entry.Trim().Trim('"').TrimEnd("\")
    $normalized = Normalize-PathEntry $raw
    $targets = New-Object System.Collections.Generic.List[string]
    Add-UniquePath -List $targets -Path (Normalize-PathEntry (Join-Path $env:USERPROFILE ".cyberdriver"))
    foreach ($programFilesDir in Get-ProgramFilesDirs) {
        Add-UniquePath -List $targets -Path (Normalize-PathEntry (Join-Path $programFilesDir "Cyberdriver"))
    }

    if ($raw -match "(?i)^%USERPROFILE%\\\.cyberdriver$") {
        return $true
    }
    if ($targets -contains $normalized) {
        return $true
    }
    return $false
}

function Update-PathScope {
    param([string]$Scope)
    $current = [Environment]::GetEnvironmentVariable("Path", $Scope)
    if ($null -eq $current) {
        return
    }

    $entries = @($current -split ";" | Where-Object { $_ -ne "" })
    $kept = @($entries | Where-Object { -not (Test-IsTargetPathEntry $_) })
    if ($kept.Count -ne $entries.Count) {
        try {
            [Environment]::SetEnvironmentVariable("Path", ($kept -join ";"), $Scope)
            Add-Removed "$Scope PATH entries" (($entries | Where-Object { Test-IsTargetPathEntry $_ }) -join "; ")
        }
        catch {
            Add-Failure ("Failed to update {0} PATH: {1}" -f $Scope, $_.Exception.Message)
        }
    }
}

function Remove-PathEntries {
    Write-Section "Removing Cyberdriver PATH entries"
    Update-PathScope -Scope "User"
    Update-PathScope -Scope "Machine"

    $processEntries = @($env:Path -split ";" | Where-Object { $_ -ne "" })
    $env:Path = (@($processEntries | Where-Object { -not (Test-IsTargetPathEntry $_) }) -join ";")

    Remove-Item Alias:cyberdriver -ErrorAction SilentlyContinue
    Remove-Item Function:cyberdriver -ErrorAction SilentlyContinue
}

function Remove-TempCyberdriverFiles {
    Write-Section "Removing temp files"
    $roots = New-Object System.Collections.Generic.List[string]
    @($env:TEMP, (Join-Path (Get-WindowsDir) "Temp")) | ForEach-Object {
        if ($_ -and (Test-Path -LiteralPath $_) -and -not $roots.Contains($_)) {
            $roots.Add($_) | Out-Null
        }
    }
    foreach ($profile in Get-UserProfileDirs) {
        $tempPath = Join-Path $profile.FullName "AppData\Local\Temp"
        if ((Test-Path -LiteralPath $tempPath) -and -not $roots.Contains($tempPath)) {
            $roots.Add($tempPath) | Out-Null
        }
    }

    foreach ($root in $roots) {
        Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue | Where-Object {
            $_.Name -match "(?i)(cyberdriver|rustdesk|\.cyberdriver)"
        } | Sort-Object FullName -Descending | ForEach-Object {
            Remove-PathIfExists -Path $_.FullName -Category "Temp item"
        }
    }
}

function Remove-UserCacheTraces {
    Write-Section "Removing user cache traces"
    foreach ($profile in Get-UserProfileDirs) {
        $roots = @(
            (Join-Path $profile.FullName "AppData\Local\CrashDumps"),
            (Join-Path $profile.FullName "AppData\Roaming\Microsoft\Windows\Recent"),
            (Join-Path $profile.FullName "AppData\Local\Pub\Cache\git")
        )

        foreach ($root in $roots) {
            if (-not (Test-Path -LiteralPath $root)) {
                continue
            }
            Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue | Where-Object {
                $_.Name -match "(?i)(cyberdriver|rustdesk|\.cyberdriver)"
            } | Sort-Object FullName -Descending | ForEach-Object {
                Remove-PathIfExists -Path $_.FullName -Category "User cache trace"
            }
        }

        $packagesRoot = Join-Path $profile.FullName "AppData\Local\Packages"
        if (Test-Path -LiteralPath $packagesRoot) {
            Get-ChildItem -LiteralPath $packagesRoot -Directory -Force -ErrorAction SilentlyContinue -Filter "Microsoft.Windows.Search_*" | ForEach-Object {
                $iconCacheRoot = Join-Path $_.FullName "LocalState\AppIconCache"
                if (Test-Path -LiteralPath $iconCacheRoot) {
                    Get-ChildItem -LiteralPath $iconCacheRoot -Recurse -Force -ErrorAction SilentlyContinue | Where-Object {
                        $_.Name -match "(?i)(cyberdriver|rustdesk|\.cyberdriver)"
                    } | Sort-Object FullName -Descending | ForEach-Object {
                        Remove-PathIfExists -Path $_.FullName -Category "User cache trace"
                    }
                }
            }
        }
    }
}

function Show-RemainingProcesses {
    Write-Section "Verification: remaining processes"
    $items = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        (Get-TextForProcess $_) -match "(?i)(cyberdriver|rustdesk)"
    } | Select-Object ProcessId, Name, ExecutablePath, CommandLine)
    if ($items.Count -eq 0) {
        Write-Host "None found."
    }
    else {
        $items | Format-List
        $script:RebootRecommended = $true
    }
}

function Show-RemainingServices {
    Write-Section "Verification: remaining services"
    $items = @(Get-TargetServices | Select-Object Name, DisplayName, State, StartMode, PathName)
    if ($items.Count -eq 0) {
        Write-Host "None found."
    }
    else {
        $items | Format-List
        $script:RebootRecommended = $true
    }
}

function Show-RemainingFiles {
    Write-Section "Verification: remaining files/folders"
    $roots = New-Object System.Collections.Generic.List[string]
    foreach ($programFilesDir in Get-ProgramFilesDirs) {
        if ($programFilesDir -and (Test-Path -LiteralPath $programFilesDir) -and -not $roots.Contains($programFilesDir)) {
            $roots.Add($programFilesDir) | Out-Null
        }
    }
    @((Get-ProgramDataDir), (Get-ServiceProfileAppDataDir)) | ForEach-Object {
        if ($_ -and (Test-Path -LiteralPath $_) -and -not $roots.Contains($_)) {
            $roots.Add($_) | Out-Null
        }
    }
    foreach ($profile in Get-UserProfileDirs) {
        @(
            (Join-Path $profile.FullName "AppData\Local"),
            (Join-Path $profile.FullName "AppData\Roaming")
        ) | ForEach-Object {
            if ($_ -and (Test-Path -LiteralPath $_) -and -not $roots.Contains($_)) {
                $roots.Add($_) | Out-Null
            }
        }
    }

    $remaining = New-Object System.Collections.Generic.List[string]
    foreach ($root in $roots) {
        Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue | Where-Object {
            $_.Name -match "(?i)(cyberdriver|rustdesk|\.cyberdriver)"
        } | Select-Object -First 500 | ForEach-Object {
            $remaining.Add($_.FullName) | Out-Null
        }
    }

    if ($remaining.Count -eq 0) {
        Write-Host "None found."
    }
    else {
        $remaining | Sort-Object -Unique | ForEach-Object { Write-Host $_ }
        $script:RebootRecommended = $true
    }
}

function Show-RemainingPathEntries {
    Write-Section "Verification: remaining PATH entries"
    foreach ($scope in @("User", "Machine")) {
        $value = [Environment]::GetEnvironmentVariable("Path", $scope)
        $matches = @($value -split ";" | Where-Object { $_ -match "(?i)(cyberdriver|\.cyberdriver|rustdesk)" })
        if ($matches.Count -eq 0) {
            Write-Host ("{0} PATH: none found." -f $scope)
        }
        else {
            Write-Host ("{0} PATH:" -f $scope)
            $matches | ForEach-Object { Write-Host ("  {0}" -f $_) }
            $script:RebootRecommended = $true
        }
    }
}

function Show-Summary {
    Write-Section "Summary"
    if ($script:Removed.Count -eq 0) {
        Write-Host "Removed: nothing found to remove."
    }
    else {
        Write-Host "Removed:"
        $script:Removed | Group-Object Category | Sort-Object Name | ForEach-Object {
            Write-Host ("  {0}: {1}" -f $_.Name, $_.Count)
        }
    }

    if ($script:Failures.Count -eq 0) {
        Write-Host "Could not remove: none reported."
    }
    else {
        Write-Host "Could not remove:"
        $script:Failures | Sort-Object -Unique | ForEach-Object { Write-Host ("  {0}" -f $_) }
    }

    Write-Host ("Reboot recommended before testing: {0}" -f $script:RebootRecommended)
    Write-Host ("Log path: {0}" -f $LogPath)
}

$transcriptStarted = $false
try {
    Start-Transcript -Path $LogPath -Force | Out-Null
    $transcriptStarted = $true
}
catch {
    Write-Warning ("Could not start transcript at {0}: {1}" -f $LogPath, $_.Exception.Message)
}

try {
    Write-Host ("Cyberdriver machine reset started at {0}" -f (Get-Date -Format "s"))
    Write-Host ("Running as: {0}" -f [Security.Principal.WindowsIdentity]::GetCurrent().Name)
    Write-Host ("Administrator: {0}" -f (Test-IsAdministrator))

    if (-not (Test-IsAdministrator)) {
        throw "This cleanup must run as Administrator."
    }

    Invoke-CyberdriverStop
    Stop-TargetServices
    Stop-TargetProcesses
    Invoke-CyberdriverUninstall
    Stop-TargetProcesses
    Remove-TargetServices
    Remove-TargetScheduledTasks
    Remove-ConfigAndInstallPaths
    Remove-UpdaterLeftovers
    Remove-PathEntries
    Remove-TempCyberdriverFiles
    Remove-UserCacheTraces
    Show-RemainingProcesses
    Show-RemainingServices
    Show-RemainingFiles
    Show-RemainingPathEntries
    Show-Summary
}
catch {
    Add-Failure $_.Exception.Message
    $script:RebootRecommended = $true
    Show-Summary
    exit 1
}
finally {
    if ($transcriptStarted) {
        Stop-Transcript | Out-Null
    }
}
