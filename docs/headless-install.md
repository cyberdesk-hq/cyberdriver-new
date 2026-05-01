# Cyberdriver headless install and golden images

This guide covers the supported headless provisioning flow for Cyberdriver
service-mode machines, including AWS-style golden images.

## Goals

- Install Cyberdriver as a Windows service without opening the UI.
- Keep the golden image fingerprint-free.
- Start each cloned VM with the same Cyberdesk API key.
- Attach a per-instance correlation name, such as an AWS instance ID.
- Let Cyberdesk create a distinct Machine row per clone.

## Current support matrix

| Capability | Status |
| --- | --- |
| Silent MSI install | Supported by upstream MSI (`/quiet` or `/qn`). |
| Service registration during MSI install | Supported by current MSI behavior. |
| Fingerprint generation deferred until tunnel startup | Supported when no API key is configured at install time. |
| Runtime API key via `CYBERDESK_AGENT_KEY` | Supported. |
| Runtime machine name via `CYBERDRIVER_MACHINE_NAME` | Supported. |
| CLI `join --secret ... --name ...` | Supported for non-service bootstrap/runtime. Do not use it as the primary Windows service bootstrap until the service-account config path is validated in your image. |
| CLI `reset-fingerprint` | Supported. |
| MSI `APIKEY=...` property | Supported. Stores an encrypted local API key via Cyberdriver. |
| MSI `REGISTER_NOW=0` property | Supported with `INSTALL_AS_SERVICE=1`. Creates the service but does not start it. |

## Golden image creation

Start from a clean Windows VM and install Cyberdriver silently:

```powershell
msiexec /i .\cyberdriver.msi /qn /norestart INSTALL_AS_SERVICE=1 APIKEY=ak_replace_me REGISTER_NOW=0
```

This stores the API key through Cyberdriver's encrypted local secret storage,
creates the Windows service with automatic startup, and leaves the service
stopped during image creation.

If you want to install the binary without service registration during image
creation, omit `INSTALL_AS_SERVICE=1`:

```powershell
msiexec /i .\cyberdriver.msi /qn /norestart
```

Before sealing the image, make sure no accidental fingerprint exists:

```powershell
$Cyberdriver = "${env:ProgramFiles}\Cyberdriver\Cyberdriver.exe"
if (Test-Path $Cyberdriver) {
  & $Cyberdriver reset-fingerprint
}
```

Then Sysprep and create the AMI or golden image.

## Per-instance first boot

On each new instance, inject the API key and a unique machine name before
starting or restarting the service.

Example EC2 user-data PowerShell using Machine environment variables:

```powershell
$ErrorActionPreference = "Stop"

$ApiKey = "ak_replace_me"
$MachineName = Invoke-RestMethod -Uri "http://169.254.169.254/latest/meta-data/instance-id"

[Environment]::SetEnvironmentVariable("CYBERDESK_AGENT_KEY", $ApiKey, "Machine")
[Environment]::SetEnvironmentVariable("CYBERDRIVER_MACHINE_NAME", $MachineName, "Machine")

Set-Service Cyberdriver -StartupType Automatic
Restart-Service Cyberdriver -Force
```

The service reads:

- `CYBERDESK_AGENT_KEY` for tunnel authentication.
- `CYBERDRIVER_MACHINE_NAME` for `X-CYBERDRIVER-NAME`.

The machine name is not persisted in Cyberdriver config. Change the environment
variable and restart the service if the provisioner needs to send a different
name on a future handshake.

If you prefer encrypted LocalConfig storage for the API key, run the CLI
bootstrap from the same Windows account that will run the service:

```powershell
& "${env:ProgramFiles}\Cyberdriver\Cyberdriver.exe" join `
  --secret "ak_replace_me" `
  --name $MachineName
```

The `join` bootstrap stores the API key using RustDesk's encrypted local secret
storage and starts the runtime without keeping the secret in argv or environment.
For Windows service installs, validate this path on your image before adopting
it broadly: service-account config paths differ from interactive-user config
paths on Windows.

## Fingerprint behavior

Cyberdriver stores its Cyberdesk tunnel fingerprint in `cyberdesk_tunnel.toml`
under the app config directory. If no fingerprint exists when the tunnel starts,
Cyberdriver generates a fresh UUID and persists it.

This is why the golden image must not connect before cloning: a pre-generated
fingerprint in the image would make all clones register as the same machine.

Use this reset path if a VM accidentally registered before imaging:

```powershell
& "${env:ProgramFiles}\Cyberdriver\Cyberdriver.exe" reset-fingerprint
```

Do not set `CYBERDRIVER_RESET_FINGERPRINT` as a persistent Machine environment
variable. Persistent reset env vars can cause a new fingerprint on every service
restart.

## Binding clones back to Cyberdesk rows

Use a stable provisioner-known name, such as the AWS instance ID:

```powershell
[Environment]::SetEnvironmentVariable("CYBERDRIVER_MACHINE_NAME", $InstanceId, "Machine")
Restart-Service Cyberdriver -Force
```

Cyberdriver sends this as:

```text
X-CYBERDRIVER-NAME: i-0123456789abcdef0
```

Cyberdesk can then expose or query the machine by name so customers can map
parallel-provisioned VMs back to Machine rows without serial provisioning.

## Local development

For local development against a non-TLS tunnel endpoint, use loopback only:

```powershell
cyberdriver join --secret ak_dev --api-base ws://localhost:8080 --name dev-vm-1
```

Non-loopback plaintext endpoints are rejected. Production endpoints must use
`wss://` or `https://` (which Cyberdriver normalizes to `wss://`).

## Validation checklist

Before publishing a golden image, validate the following on a disposable clone:

1. Install Cyberdriver silently with no API key.
2. Confirm no Cyberdesk machine row is created during image build.
3. Confirm `cyberdriver reset-fingerprint` reports success.
4. Clone the image at least three times.
5. On each clone, set the same API key and a distinct `CYBERDRIVER_MACHINE_NAME`.
6. Restart the Cyberdriver service on each clone.
7. Confirm each clone creates a distinct Cyberdesk Machine row.
8. Confirm each Machine row has the expected name.
9. Confirm the dashboard/API can query or filter by the provided machine name.

## MSI property reference

The Windows installer supports:

```powershell
msiexec /i .\cyberdriver.msi /qn INSTALL_AS_SERVICE=1 APIKEY=ak_xxx REGISTER_NOW=0
```

Properties:

- `INSTALL_AS_SERVICE=1` or `Y`: create the Cyberdriver Windows service.
- `APIKEY=ak_xxx`: store the Cyberdesk API key via encrypted local config.
- `CYBERDESK_API_BASE=wss://...`: optional custom Cyberdesk tunnel API base.
- `REGISTER_NOW=0`: create the service but do not start it during install.

If `REGISTER_NOW` is omitted, the service starts immediately after install.
