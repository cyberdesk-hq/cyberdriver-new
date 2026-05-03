# Cyberdriver Windows MSI Install

## Test Release

This installs the current `test` prerelease MSI from GitHub Releases. Use this for Cyberdesk dev validation.

Run PowerShell as Administrator:

```powershell
$msi="$env:TEMP\Cyberdriver.msi"; Invoke-WebRequest "https://github.com/cyberdesk-hq/cyberdriver-new/releases/download/test/Cyberdriver-1.4.6-windows-x64.msi" -OutFile $msi; Start-Process msiexec.exe -Wait -ArgumentList "/i `"$msi`" /qn INSTALL_AS_SERVICE=1 APIKEY=`"<YOUR_API_KEY>`" CYBERDESK_API_BASE=`"https://cyberdesk-api-dev.fly.dev`""
```

Replace `<YOUR_API_KEY>` with a Cyberdesk API key.

## Production

Production install instructions are not published yet.
