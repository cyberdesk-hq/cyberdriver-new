#!/usr/bin/env bash
# apply-branding.sh — copy Cyberdriver branding overlay into target
# locations. Idempotent. Run before `cargo build --features cyberdesk`
# or any branded packaging step.
#
# Source of truth: branding/ at the repo root.
# Targets: res/, flutter/lib/cyberdesk_branding.dart, flutter/assets/
#
# This script does NOT modify the libs/hbb_common/ submodule.
# Runtime branding (APP_NAME, BUILTIN_SETTINGS) is applied by
# src/cyberdesk_branding.rs at app startup, gated by the `cyberdesk`
# Cargo feature.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BRANDING_DIR="$REPO_ROOT/branding"
RES_DIR="$REPO_ROOT/res"
FLUTTER_LIB_DIR="$REPO_ROOT/flutter/lib"
FLUTTER_ASSETS_DIR="$REPO_ROOT/flutter/assets"

if [[ ! -d "$BRANDING_DIR" ]]; then
  echo "error: $BRANDING_DIR not found" >&2
  exit 1
fi

mkdir -p "$FLUTTER_ASSETS_DIR"

copy_if_exists() {
  local src="$1"
  local dst="$2"
  if [[ -f "$src" ]]; then
    cp "$src" "$dst"
    echo "  copied $(basename "$src") -> $dst"
  else
    echo "  skipped $(basename "$src") (not present in branding/; upstream RustDesk asset retained)"
  fi
}

echo "[apply-branding] Icons:"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver.ico"      "$RES_DIR/icon.ico"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver.icns"     "$RES_DIR/icon.icns"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-tray.png" "$RES_DIR/tray-icon.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-512.png"  "$FLUTTER_ASSETS_DIR/logo.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-1024.png" "$FLUTTER_ASSETS_DIR/logo-1024.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-icon.png" "$FLUTTER_ASSETS_DIR/icon.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-icon.svg" "$FLUTTER_ASSETS_DIR/icon.svg"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-logo.svg" "$RES_DIR/logo.svg"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-logo.svg" "$RES_DIR/scalable.svg"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-logo-header.svg" "$RES_DIR/logo-header.svg"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-app-icon.png" "$RES_DIR/icon.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-app-icon.png" "$RES_DIR/mac-icon.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-32.png" "$RES_DIR/32x32.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-64.png" "$RES_DIR/64x64.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-128.png" "$RES_DIR/128x128.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-256.png" "$RES_DIR/128x128@2x.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver.ico" "$RES_DIR/tray-icon.ico"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-tray-template.png" "$RES_DIR/mac-tray-dark-x2.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver-tray-template.png" "$RES_DIR/mac-tray-light-x2.png"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver.icns" "$REPO_ROOT/flutter/macos/Runner/AppIcon.icns"
copy_if_exists "$BRANDING_DIR/icons/cyberdriver.ico" "$REPO_ROOT/flutter/windows/runner/resources/app_icon.ico"

echo "[apply-branding] Flutter constants:"
copy_if_exists "$BRANDING_DIR/flutter/cyberdesk_branding.dart" \
               "$FLUTTER_LIB_DIR/cyberdesk_branding.dart"

# Sanity check: ensure the Rust branding module is in the expected place.
# (We do not mutate it from this script — it lives under src/ and is
# version-controlled. This check just warns the user if it's missing.)
if [[ ! -f "$REPO_ROOT/src/cyberdesk_branding.rs" ]]; then
  echo "[apply-branding] WARNING: src/cyberdesk_branding.rs is missing." >&2
  echo "                 The cyberdesk Cargo feature will fail to compile." >&2
  echo "                 Restore from git: git checkout src/cyberdesk_branding.rs" >&2
fi

# If branding/hbbs_pubkey.txt has been filled in (M2), warn the user
# that they need to keep src/cyberdesk_branding.rs::HBBS_PUBKEY in sync.
HBBS_PUBKEY_FILE="$BRANDING_DIR/hbbs_pubkey.txt"
if [[ -f "$HBBS_PUBKEY_FILE" ]]; then
  HBBS_PUBKEY_LINE="$(grep -E '^[A-Za-z0-9+/]{40,}={0,2}$' "$HBBS_PUBKEY_FILE" | head -1 || true)"
  if [[ -n "$HBBS_PUBKEY_LINE" && "$HBBS_PUBKEY_LINE" != "PLACEHOLDER_HBBS_ED25519_PUBKEY_BASE64" ]]; then
    if ! grep -q "$HBBS_PUBKEY_LINE" "$REPO_ROOT/src/cyberdesk_branding.rs"; then
      echo "[apply-branding] NOTE: branding/hbbs_pubkey.txt has a value but src/cyberdesk_branding.rs::HBBS_PUBKEY does not match." >&2
      echo "                 Update HBBS_PUBKEY in src/cyberdesk_branding.rs to: $HBBS_PUBKEY_LINE" >&2
    fi
  fi
fi

echo "[apply-branding] Done."
