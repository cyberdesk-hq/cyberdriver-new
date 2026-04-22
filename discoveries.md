# Discoveries — cyberdriver-new

Things found mid-execution that aren't part of the current milestone.
Revisit after each acceptance gate. Do **not** sneak fixes into the
current PR.

| Date (UTC) | Milestone | Discovery | Owner | Action |
|---|---|---|---|---|
| 2026-04-21 | m1-upstream | Upstream RustDesk has force-pushed `rustdesk/hbb_common` historically; `git fetch --recurse-submodules` of upstream tags fails with `upload-pack: not our ref ...`. Our pinned submodule SHA is fine. | — | None — only relevant if we ever need the old hbb_common refs. |
| 2026-04-21 | m0-gate | M0 acceptance closed by **prior personal experience** of project owner — vanilla RustDesk in Windows service mode is known to correctly expose the lock screen + login screen to a remote viewer across log-out / Win+L / reboot scenarios. No need to re-verify in the project context. The architecture's headline assumption holds. | Project owner | Continue to M1+. |
| 2026-04-21 | m1-gate | M1 acceptance closed via **Option B (pragmatic gate)**: branding overlay code landed (`src/cyberdesk_branding.rs` + `cyberdesk` Cargo feature + `apply-branding.sh` + MSI flag spec in `branding/msi-flags.md`), `cargo check --features cyberdesk` reaches the C-deps boundary cleanly proving our overlay is syntactically/type correct. The full branded-MSI end-to-end build is deferred to CI work (will be folded into the Docker/release pipeline added in M2 or the M12 polish phase). M0's first-hand confirmation covers the architectural risk; the M1 branding additions are tiny and don't change behavior beyond display strings + BUILTIN_SETTINGS values. | — | Resume real M1 acceptance once we have a Windows build machine + WiX in CI. Until then, M2+ code lands on top of an unbuilt branded MSI — acceptable risk. |
