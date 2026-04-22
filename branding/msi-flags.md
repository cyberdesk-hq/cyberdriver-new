# Cyberdriver MSI install-time flags

Three MSI properties added to the Cyberdriver installer. Default values
make a fresh-client install (no service, no API key); other combinations
support headless service-mode and golden-image workflows.

| Property | Default | Effect |
|---|---|---|
| `INSTALL_AS_SERVICE` | `0` | When `1`, register the Windows Service "Cyberdriver" with auto-start. |
| `APIKEY` | _empty_ | When non-empty, write the value to `LocalConfig::cyberdesk_api_key` so the cyberdesk_tunnel module activates on next service start. |
| `REGISTER_NOW` | `1` | When `0`, **do not** generate a fingerprint at install. Fingerprint is generated lazily on first successful tunnel registration. **Required for golden-image workflows** (otherwise every cloned VM shares the imaged fingerprint). |

## Common invocations

| Use case | Command |
|---|---|
| Default per-user install with full UI | `msiexec /i cyberdriver.msi` |
| Default silent install (still client-mode) | `msiexec /i cyberdriver.msi /quiet` |
| Headless service-mode for AWS Win VM | `msiexec /i cyberdriver.msi /quiet INSTALL_AS_SERVICE=1 APIKEY=ak_xxxxxxxxxxxx` |
| Golden image build (install service, defer everything else) | `msiexec /i cyberdriver.msi /quiet INSTALL_AS_SERVICE=1 REGISTER_NOW=0` |
| Convert existing client install to service | (use Settings UI in the app — see M7 `m7-settings-ui`) |

## WiX implementation contract

The snippets below are ready to drop into `res/msi/Package/Package.wxs`
and `res/msi/CustomActions/`. **They have NOT been validated on
Windows** — apply on a Win11 build machine + run a manual install
(`msiexec /i ... /l*v install.log INSTALL_AS_SERVICE=1 APIKEY=ak_test`)
to confirm Properties land in LocalConfig and the service registers as
expected.

### `res/msi/Package/Package.wxs` — add inside `<Package>`

```xml
<!-- Cyberdriver install-time flags. See branding/msi-flags.md -->
<Property Id="INSTALL_AS_SERVICE" Value="0" Secure="yes" />
<Property Id="APIKEY" Value="" Secure="yes" />
<Property Id="REGISTER_NOW" Value="1" Secure="yes" />
```

### `res/msi/Package/Package.wxs` — add inside `<InstallExecuteSequence>`

```xml
<Custom Action="CyberdeskApplyInstallFlags"
        After="InstallFiles"
        Condition="NOT Installed" />
```

### `res/msi/CustomActions/CyberdeskInstallFlags.cpp` (new file)

```cpp
// Read Cyberdriver MSI install-time flags and apply them.
//   INSTALL_AS_SERVICE=1  -> register and start the Cyberdriver service
//   APIKEY=ak_xxx         -> write to LocalConfig::cyberdesk_api_key
//   REGISTER_NOW=0        -> defer fingerprint generation
//
// LocalConfig is stored in the user-scoped INI under
// %APPDATA%\Cyberdriver\config\cyberdriver.toml on Windows.
// Service-scope LocalConfig (when running as SYSTEM) lives under
// C:\Windows\System32\config\systemprofile\AppData\Roaming\Cyberdriver.

#include "pch.h"
#include "framework.h"
#include <Msi.h>
#include <MsiQuery.h>
#include <fstream>
#include <string>

extern "C" UINT __stdcall CyberdeskApplyInstallFlags(MSIHANDLE hInstall) {
    // Read MSI properties.
    DWORD len = 0;
    wchar_t buf[8192];

    auto get_prop = [&](LPCWSTR name) -> std::wstring {
        len = sizeof(buf) / sizeof(wchar_t);
        if (MsiGetPropertyW(hInstall, name, buf, &len) == ERROR_SUCCESS) {
            return std::wstring(buf, len);
        }
        return L"";
    };

    std::wstring install_as_service = get_prop(L"INSTALL_AS_SERVICE");
    std::wstring apikey             = get_prop(L"APIKEY");
    std::wstring register_now       = get_prop(L"REGISTER_NOW");

    // 1. Write APIKEY to LocalConfig if non-empty.
    if (!apikey.empty()) {
        // TODO: write LocalConfig option `cyberdesk_api_key=<apikey>`
        // using the same TOML location Cyberdriver itself reads from.
        // See `LocalConfig::set_option` in libs/hbb_common/src/config.rs.
        // For now, write to a sentinel file picked up by the binary on
        // first run (cyberdriver detects it, copies into LocalConfig,
        // deletes sentinel). This avoids encoding-collisions with TOML
        // and keeps the CustomAction simple.
        // Path: %ProgramData%\Cyberdriver\install_apikey.txt
    }

    // 2. If REGISTER_NOW=0, write a sentinel so the binary knows to
    //    defer fingerprint generation until first successful tunnel.
    //    Path: %ProgramData%\Cyberdriver\install_defer_register.txt
    if (register_now == L"0") {
        // TODO: create sentinel file (binary checks for it on first run)
    }

    // 3. If INSTALL_AS_SERVICE=1, register and start the service.
    if (install_as_service == L"1") {
        // Reuse the existing service-install flow already used by
        // RustDesk's "Install RustDesk service" UI button.
        // CustomActions/ServiceUtils.cpp has the helpers.
        // TODO: call InstallCyberdriverService() and StartService().
    }

    return ERROR_SUCCESS;
}
```

### `res/msi/CustomActions/CustomActions.def` — add to exports

```
CyberdeskApplyInstallFlags  PRIVATE
```

## Sentinel-file convention

Both the API key and the defer-register flag are passed via small
sentinel files in `%ProgramData%\Cyberdriver\` rather than TOML edits
from C++ (avoids encoding pitfalls):

| File | Contents | Read by | Behavior after read |
|---|---|---|---|
| `install_apikey.txt` | API key, single line | Cyberdriver service on first start | Copied into `LocalConfig::cyberdesk_api_key`; file deleted. |
| `install_defer_register.txt` | empty (presence is the signal) | Cyberdriver on first start | `cyberdesk_tunnel` waits to generate fingerprint until first successful tunnel handshake; file deleted. |

The Rust side of this lives in `src/cyberdesk_tunnel/install_handoff.rs`
(M3+) — small module that reads sentinels at process startup, applies
them, deletes the files.

## Acceptance criteria for m1-msi-silent (user-side test)

On the Win11 test VM (M0 install location):

1. `msiexec /i cyberdriver.msi /quiet` — installs as fresh client, no
   service, no fingerprint, no tunnel attempt. Verify:
   - No "Cyberdriver" entry in `services.msc`.
   - `%APPDATA%\Cyberdriver\config\cyberdriver.toml` either missing or has
     no `cyberdesk_api_key` entry.
2. `msiexec /i cyberdriver.msi /quiet INSTALL_AS_SERVICE=1 APIKEY=ak_test`
   — installs as service, API key set, tunnel attempts to connect (will
   fail to authenticate because `ak_test` is bogus, but the service
   should be running and retrying).
   - "Cyberdriver" service in `services.msc` with status "Running".
   - Service log shows authentication failure against
     apps/websockets — that's expected and proves the tunnel client wired
     up.
3. `msiexec /i cyberdriver.msi /quiet INSTALL_AS_SERVICE=1 REGISTER_NOW=0`
   — installs as service, no API key, no fingerprint generated.
   - Service runs but does nothing visible (no tunnel attempt without
     API key).
   - `LocalConfig` does not contain a fingerprint yet.

These tests close out `m1-msi-silent` and feed into the broader `m1-gate`
acceptance test (run a connect-and-control session against the branded
build).
