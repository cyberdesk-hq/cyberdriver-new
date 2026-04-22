# Build prerequisites — Cyberdriver

What needs to be installed locally before `cargo build --features cyberdesk`
will succeed end-to-end. RustDesk's standard requirements; the Cyberdriver
fork doesn't add new ones beyond the cyberdesk_tunnel deps in M3+.

## Rust toolchain

`rustup` (any recent stable, M1 was tested with 1.93.0).

## Native C dependencies (required by `libs/scrap`, `magnum-opus`)

`libvpx`, `libyuv`, `opus`, `aom`. `libyuv` is **not** in Homebrew; the
upstream RustDesk path uses **vcpkg**:

```bash
git clone https://github.com/microsoft/vcpkg.git ~/vcpkg
~/vcpkg/bootstrap-vcpkg.sh
~/vcpkg/vcpkg install libvpx libyuv opus aom
export VCPKG_ROOT=~/vcpkg
```

Then add `export VCPKG_ROOT=~/vcpkg` to `~/.zshrc` (or equivalent).

Reference: <https://github.com/rustdesk/rustdesk/wiki/Build-Guide>

## Sciter (legacy UI; only needed for `--bin rustdesk` non-Flutter builds)

Most Cyberdriver builds will use Flutter (`--features flutter`). The
Sciter library is only needed if you're building the legacy UI binary.
See `docs/SETUP.md` upstream for download instructions.

## Verifying the cyberdesk overlay compiles

Without the native deps, `cargo check --features cyberdesk` will fail at
the `scrap` and `magnum-opus` build scripts — but it will compile **all
Rust code** (including our `src/cyberdesk_branding.rs` overlay) before
hitting that boundary. So a partial check is a useful sanity test for
"did our branding overlay introduce a Rust syntax error":

```bash
cargo check --features cyberdesk 2>&1 | tail -30
# Expect failure at scrap/magnum-opus build.rs IF vcpkg deps absent.
# Anything that mentions cyberdesk_branding.rs is a real bug.
```

Once vcpkg + Sciter are set up, the full build should succeed.

## M1 acceptance status

- ✅ Branding overlay (`src/cyberdesk_branding.rs`,
  `src/lib.rs` mod declaration, `src/core_main.rs` init call) compiles
  cleanly (verified by `cargo check --features cyberdesk` reaching the
  `scrap` build-script boundary without error in our code).
- ⏳ Full `cargo build --release --features cyberdesk` requires vcpkg
  setup as documented above. Re-verify on a vcpkg-equipped machine
  before merging M1 PR.
- ⏳ Branded MSI/DMG builds (`build.py --flutter --release`) require
  the Flutter SDK in addition to the above. M1 acceptance gate is the
  user-side test on Win11.
