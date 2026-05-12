# Cyberdriver Complete Removal Script
# This script removes all traces of Cyberdriver from Windows
# Run as Administrator

Write-Host "================================" -ForegroundColor Cyan
Write-Host "Cyberdriver Complete Removal" -ForegroundColor Cyan
Write-Host "================================" -ForegroundColor Cyan
Write-Host ""

# Check for admin privileges
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "ERROR: This script must be run as Administrator!" -ForegroundColor Red
    Write-Host "Right-click PowerShell and select 'Run as Administrator', then run this script again." -ForegroundColor Yellow
    pause
    exit 1
}

Write-Host "[1/8] Stopping Cyberdriver processes..." -ForegroundColor Yellow

# Kill all Cyberdriver processes
$processNames = @("Cyberdriver", "cyberdriver")
foreach ($procName in $processNames) {
    $processes = Get-Process -Name $procName -ErrorAction SilentlyContinue
    if ($processes) {
        Write-Host "  Killing $($processes.Count) $procName process(es)..." -ForegroundColor Gray
        $processes | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }
}

Write-Host "[2/8] Stopping and removing Cyberdriver service..." -ForegroundColor Yellow

# Stop and remove Windows service
$serviceNames = @("Cyberdriver", "Cyberdriver Service")
foreach ($svcName in $serviceNames) {
    $service = Get-Service -Name $svcName -ErrorAction SilentlyContinue
    if ($service) {
        Write-Host "  Found service: $svcName" -ForegroundColor Gray
        if ($service.Status -eq 'Running') {
            Write-Host "  Stopping service..." -ForegroundColor Gray
            Stop-Service -Name $svcName -Force -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 2
        }
        Write-Host "  Removing service..." -ForegroundColor Gray
        sc.exe delete $svcName | Out-Null
    }
}

Write-Host "[3/8] Removing installation directories..." -ForegroundColor Yellow

# Remove program files
$programDirs = @(
    "$env:ProgramFiles\Cyberdriver",
    "${env:ProgramFiles(x86)}\Cyberdriver"
)

foreach ($dir in $programDirs) {
    if (Test-Path $dir) {
        Write-Host "  Removing: $dir" -ForegroundColor Gray
        Remove-Item -Path $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "[4/8] Removing user data directories..." -ForegroundColor Yellow

# Remove AppData directories (current user)
$appDataDirs = @(
    "$env:APPDATA\Cyberdriver",
    "$env:APPDATA\Cyberdesk",
    "$env:LOCALAPPDATA\Cyberdriver",
    "$env:LOCALAPPDATA\Cyberdesk"
)

foreach ($dir in $appDataDirs) {
    if (Test-Path $dir) {
        Write-Host "  Removing: $dir" -ForegroundColor Gray
        Remove-Item -Path $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Remove ProgramData directories (all users)
$programDataDirs = @(
    "$env:ProgramData\Cyberdriver",
    "$env:ProgramData\Cyberdesk"
)

foreach ($dir in $programDataDirs) {
    if (Test-Path $dir) {
        Write-Host "  Removing: $dir" -ForegroundColor Gray
        Remove-Item -Path $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Remove service account AppData (for service installations)
$systemProfileDirs = @(
    "C:\Windows\System32\config\systemprofile\AppData\Roaming\Cyberdriver",
    "C:\Windows\System32\config\systemprofile\AppData\Roaming\Cyberdesk",
    "C:\Windows\System32\config\systemprofile\AppData\Local\Cyberdriver",
    "C:\Windows\System32\config\systemprofile\AppData\Local\Cyberdesk"
)

foreach ($dir in $systemProfileDirs) {
    if (Test-Path $dir) {
        Write-Host "  Removing: $dir" -ForegroundColor Gray
        Remove-Item -Path $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "[5/8] Removing registry entries..." -ForegroundColor Yellow

# Remove registry keys
$registryPaths = @(
    "HKCU:\Software\Cyberdriver",
    "HKCU:\Software\Cyberdesk",
    "HKLM:\Software\Cyberdriver",
    "HKLM:\Software\Cyberdesk",
    "HKLM:\Software\WOW6432Node\Cyberdriver",
    "HKLM:\Software\WOW6432Node\Cyberdesk",
    "HKCU:\Software\Classes\cyberdriver",
    "HKLM:\Software\Classes\cyberdriver"
)

foreach ($regPath in $registryPaths) {
    if (Test-Path $regPath) {
        Write-Host "  Removing: $regPath" -ForegroundColor Gray
        Remove-Item -Path $regPath -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Remove uninstall entries
$uninstallPaths = @(
    "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*",
    "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*"
)

foreach ($uninstallPath in $uninstallPaths) {
    Get-ChildItem -Path $uninstallPath -ErrorAction SilentlyContinue | 
        Where-Object { $_.GetValue("DisplayName") -like "*Cyberdriver*" -or $_.GetValue("DisplayName") -like "*Cyberdesk*" } | 
        ForEach-Object {
            Write-Host "  Removing uninstall entry: $($_.GetValue('DisplayName'))" -ForegroundColor Gray
            Remove-Item -Path $_.PSPath -Recurse -Force -ErrorAction SilentlyContinue
        }
}

Write-Host "[6/8] Removing startup items..." -ForegroundColor Yellow

# Remove startup shortcuts
$startupDirs = @(
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup",
    "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Startup",
    "$env:APPDATA\Microsoft\Windows\Start Menu\Programs",
    "$env:ProgramData\Microsoft\Windows\Start Menu\Programs"
)

foreach ($startupDir in $startupDirs) {
    if (Test-Path $startupDir) {
        Get-ChildItem -Path $startupDir -Filter "*Cyberdriver*" -Recurse -ErrorAction SilentlyContinue | 
            ForEach-Object {
                Write-Host "  Removing: $($_.FullName)" -ForegroundColor Gray
                Remove-Item -Path $_.FullName -Force -ErrorAction SilentlyContinue
            }
        Get-ChildItem -Path $startupDir -Filter "*Cyberdesk*" -Recurse -ErrorAction SilentlyContinue | 
            ForEach-Object {
                Write-Host "  Removing: $($_.FullName)" -ForegroundColor Gray
                Remove-Item -Path $_.FullName -Force -ErrorAction SilentlyContinue
            }
    }
}

# Remove desktop shortcuts
$desktopPaths = @(
    "$env:USERPROFILE\Desktop",
    "$env:PUBLIC\Desktop"
)

foreach ($desktopPath in $desktopPaths) {
    Get-ChildItem -Path $desktopPath -Filter "*Cyberdriver*.lnk" -ErrorAction SilentlyContinue | 
        ForEach-Object {
            Write-Host "  Removing desktop shortcut: $($_.Name)" -ForegroundColor Gray
            Remove-Item -Path $_.FullName -Force -ErrorAction SilentlyContinue
        }
    Get-ChildItem -Path $desktopPath -Filter "*Cyberdesk*.lnk" -ErrorAction SilentlyContinue | 
        ForEach-Object {
            Write-Host "  Removing desktop shortcut: $($_.Name)" -ForegroundColor Gray
            Remove-Item -Path $_.FullName -Force -ErrorAction SilentlyContinue
        }
}

Write-Host "[7/8] Removing scheduled tasks..." -ForegroundColor Yellow

# Remove scheduled tasks
Get-ScheduledTask -TaskName "*Cyberdriver*" -ErrorAction SilentlyContinue | 
    ForEach-Object {
        Write-Host "  Removing scheduled task: $($_.TaskName)" -ForegroundColor Gray
        Unregister-ScheduledTask -TaskName $_.TaskName -Confirm:$false -ErrorAction SilentlyContinue
    }

Get-ScheduledTask -TaskName "*Cyberdesk*" -ErrorAction SilentlyContinue | 
    ForEach-Object {
        Write-Host "  Removing scheduled task: $($_.TaskName)" -ForegroundColor Gray
        Unregister-ScheduledTask -TaskName $_.TaskName -Confirm:$false -ErrorAction SilentlyContinue
    }

Write-Host "[8/8] Cleaning temporary files..." -ForegroundColor Yellow

# Clean temp files
$tempDirs = @(
    "$env:TEMP\Cyberdriver*",
    "$env:TEMP\Cyberdesk*"
)

foreach ($tempPattern in $tempDirs) {
    Get-ChildItem -Path (Split-Path $tempPattern) -Filter (Split-Path $tempPattern -Leaf) -ErrorAction SilentlyContinue | 
        ForEach-Object {
            Write-Host "  Removing: $($_.FullName)" -ForegroundColor Gray
            Remove-Item -Path $_.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
}

Write-Host ""
Write-Host "================================" -ForegroundColor Green
Write-Host "Cleanup Complete!" -ForegroundColor Green
Write-Host "================================" -ForegroundColor Green
Write-Host ""
Write-Host "All traces of Cyberdriver have been removed from your system." -ForegroundColor Green
Write-Host "You can now install the old version of Cyberdriver without conflicts." -ForegroundColor Green
Write-Host ""
Write-Host "Note: If you still see 'Cyberdriver is running' message:" -ForegroundColor Yellow
Write-Host "  1. Restart your computer" -ForegroundColor Yellow
Write-Host "  2. Or log out and log back in" -ForegroundColor Yellow
Write-Host ""
pause
