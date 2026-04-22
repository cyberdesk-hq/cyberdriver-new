# Cyberdriver icon assets

Drop branded icon artwork into this directory. `apply-branding.sh` will
copy each file to its respective install location during build.

## Required files

| File | Use |
|---|---|
| `cyberdriver.ico` | Windows app icon (≥ 256×256 multi-resolution). Copied to `res/icon.ico`. |
| `cyberdriver.icns` | macOS app icon. Copied to `res/icon.icns`. |
| `cyberdriver-tray.png` | Tray icon (32×32 transparent PNG). Copied to `res/tray-icon.png`. |
| `cyberdriver-512.png` | App logo (512×512 transparent PNG). Copied to `flutter/assets/logo.png`. |
| `cyberdriver-1024.png` | Hi-res logo for Retina/macOS dock (1024×1024). Copied to `flutter/assets/logo-1024.png`. |

## Until real artwork exists

The apply script is a no-op for icon files that don't exist in this
directory — the upstream RustDesk icons remain in place. This keeps M1
builds running without blocking on design.

Once the real branded icons land here, **rerun `apply-branding.sh`** and
rebuild.
