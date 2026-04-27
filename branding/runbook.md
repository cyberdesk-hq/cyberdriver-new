# M0 manual validation runbook

This runbook captures the M0 milestone (validating that vanilla RustDesk
in Windows service mode can show the Windows login screen to a remote
viewer) and any quirks discovered. Update this file as you walk through
M0; future milestones reference it.

## VM provisioning

| Field | Value (fill in when provisioned) |
|---|---|
| VM platform (Parallels / UTM / AWS / etc.) | _e.g. Parallels Desktop 19 on M2 MacBook_ |
| Windows edition | _e.g. Windows 11 Pro 23H2_ |
| vCPU / RAM | _e.g. 4 vCPU / 8 GB_ |
| Display resolution | _e.g. 1920 × 1080_ |
| Network mode | _e.g. NAT (default)_ |
| AV / Defender state | _Enabled by default; note any exclusions you add_ |

## Local hbbs/hbbr

| Field | Value |
|---|---|
| Host machine | _e.g. M2 MacBook Air_ |
| `hbbs` build | `cargo build --release` in `cyberdriver-server/` (commit `XXXXX`) |
| `hbbs` invocation | `./target/release/hbbs -k <pubkey>` (capture exact flags) |
| `hbbr` invocation | `./target/release/hbbr` |
| HBBS public key (base64) | _Capture from `id_ed25519.pub` after first hbbs boot_ |
| Local LAN IP exposed to VM | _e.g. 192.168.1.42_ |

## Vanilla RustDesk install on VM

1. Download MSI from <https://github.com/rustdesk/rustdesk/releases>
   (note version: ____).
2. Run installer with admin elevation; check "Install RustDesk service".
3. Open RustDesk → ID/Relay Server settings:
   - ID Server: `<host LAN IP>:21116`
   - Relay Server: `<host LAN IP>:21117`
   - Key: `<HBBS pubkey>`
4. Note assigned RustDesk ID for the VM: ____.

## Viewer install (laptop)

1. Same RustDesk version on the host laptop.
2. Same ID/Relay/Key settings as the VM.
3. Connect to VM ID; set a password on the VM the first time.

## The headline tests

| Test | Expected | Outcome | Notes |
|---|---|---|---|
| **Connect to logged-in desktop**, move mouse | Visible + controllable | _PASS / FAIL_ | |
| **Lock with Win+L**, attempt control | Lock screen visible + remote unlock works | _PASS / FAIL_ | |
| **Log out of Windows**, attempt control | Login screen visible + remote login works | _PASS / FAIL_ | |
| **Reboot VM**, RustDesk auto-reconnects | Reconnects after ~30s (service mode) | _PASS / FAIL_ | |
| **Switch user (Win+L → Switch User)** | New session visible + controllable | _PASS / FAIL_ | |

## Recordings

- Login-screen access video: _attach link / file path_
- Lock-screen access video: _attach link / file path_

## Discoveries / quirks

Document anything surprising. Examples that have bitten others:

- Windows Defender quarantining `RustDesk.exe` post-install
- IDD virtual display driver missing → no display when no monitor attached
- Network mode (Bridged vs NAT) affecting hbbs reachability
- Windows screen-saver / screen-off vs lock-screen distinction
- Multi-display behavior

## Decision

**Date**: ____  
**Decision**: GO / PIVOT  
**Rationale**:
