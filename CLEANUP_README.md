# Cyberdriver Cleanup Scripts

This directory contains PowerShell scripts to completely remove all traces of Cyberdriver (the new version) from Windows machines.

## Problem

If you installed Cyberdriver but couldn't run it due to admin permission issues, and now you're getting "Cyberdriver is already running" when trying to install the old version, these scripts will help.

## Solution

### Option 1: Full Cleanup Script (Recommended)

Use the comprehensive script with detailed progress output:

1. **Right-click PowerShell** and select **"Run as Administrator"**
2. Navigate to this directory or download `cleanup-cyberdriver.ps1`
3. Run:
   ```powershell
   .\cleanup-cyberdriver.ps1
   ```

This script will:
- Stop all Cyberdriver processes
- Remove the Cyberdriver Windows service
- Delete all installation directories
- Remove user data and configuration files
- Clean registry entries
- Remove shortcuts and startup items
- Delete scheduled tasks
- Clean temporary files

### Option 2: One-Liner (Quick)

If you prefer a quick one-liner:

1. **Right-click PowerShell** and select **"Run as Administrator"**
2. Copy and paste the entire command from `cleanup-cyberdriver-oneliner.ps1`
3. Press Enter

### Option 3: Manual Cleanup

If scripts don't work, here's what to clean manually:

#### 1. Stop Processes
- Open Task Manager (Ctrl+Shift+Esc)
- End all "Cyberdriver" processes

#### 2. Remove Service
- Open Command Prompt as Administrator
- Run: `sc delete Cyberdriver`

#### 3. Delete Directories
- `C:\Program Files\Cyberdriver\`
- `C:\Program Files (x86)\Cyberdriver\`
- `%APPDATA%\Cyberdriver\`
- `%LOCALAPPDATA%\Cyberdriver\`
- `%PROGRAMDATA%\Cyberdriver\`

#### 4. Clean Registry
- Open Registry Editor (Win+R, type `regedit`)
- Delete these keys if they exist:
  - `HKEY_CURRENT_USER\Software\Cyberdriver`
  - `HKEY_LOCAL_MACHINE\Software\Cyberdriver`
  - `HKEY_LOCAL_MACHINE\Software\WOW6432Node\Cyberdriver`
  - `HKEY_CURRENT_USER\Software\Classes\cyberdriver`

## After Cleanup

1. **Restart your computer** (recommended)
   - OR log out and log back in

2. Install the old version of Cyberdriver

3. The "already running" message should be gone

## Troubleshooting

### "Script execution is disabled"
If you get an execution policy error, run this first:
```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
```

### "Access Denied" Errors
Make sure you're running PowerShell **as Administrator**:
- Right-click PowerShell icon
- Select "Run as Administrator"

### Still Getting "Already Running" Message
1. Restart your computer
2. Check Task Manager for any remaining Cyberdriver processes
3. Check Services (services.msc) for any Cyberdriver services

## Technical Details

### What Gets Removed

**Processes:**
- Cyberdriver.exe
- Any related background processes

**Services:**
- Cyberdriver
- Cyberdriver Service

**Directories:**
- Program Files installation
- AppData configuration and data
- ProgramData shared data
- System profile AppData (for service installations)

**Registry:**
- Software keys in HKCU and HKLM
- Uninstall entries
- URL protocol handlers

**Shortcuts:**
- Start menu items
- Desktop shortcuts
- Startup shortcuts

**Scheduled Tasks:**
- Any Cyberdriver-related tasks

## Safety

These scripts only remove Cyberdriver-related files and settings. They do not affect:
- Other applications
- System files
- User documents
- Other remote desktop software

## Support

If you continue to have issues after running these scripts:
1. Check the Cyberdriver documentation
2. Contact Cyberdesk support at https://cyberdesk.io/support
3. Provide details about any error messages

---

**Note:** These scripts are designed for Cyberdriver (the RustDesk fork by Cyberdesk, Inc.). They will not remove standard RustDesk installations unless specifically modified to do so.
