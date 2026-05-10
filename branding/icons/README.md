# Cyberdriver icon assets

Drop branded icon artwork into this directory. `apply-branding.sh` will
copy each file to its respective install location during build.

## Required files

| File | Use |
|---|---|
| `cyberdriver-app-icon.svg` | Rounded app icon source. White background with black Cyberdesk mark. |
| `cyberdriver.ico` | Windows app icon (multi-resolution). Copied to `res/icon.ico` and `flutter/windows/runner/resources/app_icon.ico`. |
| `cyberdriver.icns` | macOS app icon. Copied to `res/icon.icns` and `flutter/macos/Runner/AppIcon.icns`. |
| `cyberdriver-tray.png` | Tray icon (transparent PNG). Copied to `res/tray-icon.png`. |
| `cyberdriver-tray-template.png` | macOS tray template source. Copied to `res/mac-tray-*-x2.png`. |
| `cyberdriver-512.png` | In-app logo (512×512 transparent PNG). Copied to `flutter/assets/logo.png`. |
| `cyberdriver-1024.png` | Hi-res in-app logo (1024×1024 transparent PNG). Copied to `flutter/assets/logo-1024.png`. |
| `cyberdriver-icon.png` / `cyberdriver-icon.svg` | Generic Flutter icon asset. Copied to `flutter/assets/icon.*`. |
| `cyberdriver-logo.svg` | Generic vector logo. Copied to `res/logo.svg` and `res/scalable.svg`. |

## Until real artwork exists

The apply script is a no-op for icon files that don't exist in this
directory — the upstream RustDesk icons remain in place. This keeps M1
builds running without blocking on design.

Once the real branded icons land here, **rerun `apply-branding.sh`** and
rebuild.
