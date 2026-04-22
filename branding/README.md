# branding/ — Cyberdriver branding overlay

Single source of truth for everything that distinguishes the Cyberdriver
build of RustDesk from upstream. Changes here are picked up by
`scripts/apply-branding.sh` and the feature-gated `cyberdesk_branding`
Rust module.

## Files

| File | Purpose |
|---|---|
| `app_strings.json` | App display name, support URL, AGPL source URLs. Source of truth for both Rust and Flutter sides. |
| `hbbs_pubkey.txt` | HBBS Ed25519 public key (base64). Filled in during M2 once Fly is provisioned. |
| `icons/` | Branded icons (Windows .ico, macOS .icns, tray PNG, app logo). Replace placeholders with real artwork. |
| `flutter/cyberdesk_branding.dart` | Flutter constants — app title, colors, asset paths. Copied to `flutter/lib/cyberdesk_branding.dart` by the apply script. |
| `runbook.md` | M0 manual test runbook (vanilla RustDesk service-mode + login-screen access verification). |

## Crate-name strategy (m1-naming)

**Keep upstream Cargo crate names.** Specifically:

- Root `Cargo.toml`: `name = "rustdesk"`, `default-run = "rustdesk"`,
  `[lib] name = "librustdesk"`. Do **not** rename.
- `libs/hbb_common/Cargo.toml`: `name = "hbb_common"`. Do **not** rename.
- `libs/scrap`, `libs/enigo`, `libs/clipboard`, etc.: do **not** rename.

Reason: renaming the root crate would cascade through:

- `flutter_rust_bridge` codegen (`flutter/lib/generated_bridge.dart` is
  re-generated against the crate name).
- Hundreds of `use rustdesk::...` statements across the source.
- Per-platform packaging scripts (`build.py`, `flutter/build_*.sh`,
  `flatpak/`, `appimage/`, `fastlane/`).
- F-Droid metadata.
- The `librustdesk` cdylib name baked into JNI / Cocoa loader paths.

The cost-to-value ratio is awful — the user-visible name is set at the
display layer, not the crate layer.

## Where the brand actually shows up

| Surface | Mechanism |
|---|---|
| App window title, menu bar | `APP_NAME` (`RwLock<String>`) at runtime — set by `cyberdesk_branding::init()` |
| Rendezvous server | `BUILTIN_SETTINGS["custom-rendezvous-server"]` — same init |
| Relay server | `BUILTIN_SETTINGS["relay-server"]` |
| HBBS public key | `BUILTIN_SETTINGS["key"]` |
| Cyberdesk API server | `BUILTIN_SETTINGS["api-server"]` |
| Installer file name | Build-time output rename in CI workflow |
| Installer display strings | `res/msi/Package.wxs` and similar (overlaid by apply script) |
| MSI install behavior flags | `INSTALL_AS_SERVICE`, `APIKEY`, `REGISTER_NOW` properties added in M1 |
| Flutter UI strings (login button, tray menu, etc.) | `flutter/lib/cyberdesk_branding.dart` (overlaid) |
| Icons | `res/`, `flutter/assets/` (overlaid) |

## How to apply

```bash
./scripts/apply-branding.sh
cargo build --release --features cyberdesk
```

Re-running `apply-branding.sh` is safe (idempotent).

## Why a runtime initializer instead of patching the submodule

`BUILTIN_SETTINGS` and `APP_NAME` are already runtime-mutable
(`RwLock<HashMap>` and `RwLock<String>`). Setting them at startup via a
feature-gated init function keeps `libs/hbb_common` (a submodule) and the
core `src/` files completely untouched. Upstream rebases stay clean: our
diff only touches `Cargo.toml` (one feature line), `src/lib.rs` (one
`mod` declaration), `src/core_main.rs` (one init call), plus `res/`
icons that don't have semantic meaning.
