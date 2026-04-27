# Upstream tracking — cyberdriver-new

This fork tracks [rustdesk/rustdesk](https://github.com/rustdesk/rustdesk).
The upstream remote is wired as `upstream`:

```
origin    https://github.com/cyberdesk-hq/cyberdriver-new.git
upstream  https://github.com/rustdesk/rustdesk.git
```

## Cadence

Rebase `master` onto `upstream/master` weekly. Owner: TBD (single named
owner per fork).

## Conflict zones

Most upstream commits do not touch our changes. Conflicts only happen in
the following files; treat any conflict outside this list as a signal to
investigate:

- `Cargo.toml` (root) — only the `cyberdesk` feature line and the
  `default = [..., "cyberdesk"]` array entry.
- `src/lib.rs` — only the `#[cfg(feature = "cyberdesk")] pub mod cyberdesk_tunnel;`
  declaration (added in M4).
- `src/server.rs` — only the `cyberdesk_tunnel::spawn_if_enabled();` call
  in service-mode bootstrap (added in M4).
- `flutter/lib/common.dart` — branding strings (added via overlay).
- `libs/hbb_common/src/config.rs` — `BUILTIN_SETTINGS` defaults overlay.
  Note: this lives in the submodule. We do **not** fork hbb_common; the
  branding overlay applies the changes via `scripts/apply-branding.sh`
  rather than committing to the submodule directly.
- `res/` — branded icons and installer strings.

## Rebase procedure

```bash
git fetch upstream
git checkout master
git rebase upstream/master
# Resolve in conflict zones above. If anything else conflicts, STOP and
# investigate before continuing.
git push --force-with-lease origin master
```

## CI gate

`.github/workflows/upstream-rebase.yml` (added in M12) runs
`git fetch upstream && git rebase upstream/master --dry-run` against the
PR base; failure means the PR has drifted outside the conflict zones and
needs review.

## Known upstream-side issues

- `libs/hbb_common` submodule pointer drift: upstream RustDesk has
  occasionally force-pushed `rustdesk/hbb_common` and orphaned old refs.
  When this happens, `git fetch --recurse-submodules` may fail with
  `upload-pack: not our ref <sha>`. Resolution: only fetch the submodule
  at the SHA pinned by our current upstream HEAD, not at every historical
  ref. We do not need ancient hbb_common commits.
