# AGPL Compliance — Cyberdriver

This document explains how the Cyberdriver fork of RustDesk satisfies its
GNU Affero General Public License v3.0 (AGPLv3) obligations.

## License inheritance

Cyberdriver is a fork of [RustDesk](https://github.com/rustdesk/rustdesk),
which is licensed under AGPL-3.0. The fork itself, including all our
modifications (the `cyberdesk` Cargo feature, branding overlay, install
flow changes, and any new modules), inherits AGPL-3.0. The full license
text is in [LICENCE](LICENCE).

The same applies to our companion fork [cyberdriver-server](https://github.com/cyberdesk-hq/cyberdriver-server)
(forked from `rustdesk/rustdesk-server`).

## What is and isn't AGPL'd

- **AGPL-3.0 (public, source-available):**
  - This repository (`cyberdesk-hq/cyberdriver-new`) — the Rust + Flutter client.
  - `cyberdesk-hq/cyberdriver-server` — the Rust hbbs/hbbr rendezvous + relay servers.
  - All binaries built from these two repositories that ship to end users.
- **NOT AGPL'd:**
  - The `cyberdesk-hq/cyberdesk-new` cloud platform. It is a separate program that
    talks to AGPL'd hbbs over the network as a client. Putting AGPL'd source
    code in the same git repository would not change this; sharing a network
    socket does not combine programs into one work for AGPL purposes.

## Section 13 obligation (network interaction)

AGPLv3 §13 requires that "if you modify the Program, your modified version
must prominently offer all users interacting with it remotely through a
computer network ... an opportunity to receive the Corresponding Source of
your version."

Cyberdriver is **interacted with remotely**:

1. End users connect to Cyberdriver-running machines via Cyberdriver's
   WebRTC/peer protocol (through hbbs).
2. The Cyberdesk cloud control plane interacts with Cyberdriver via the
   `cyberdesk_tunnel` WebSocket.

Both groups of remote users must be offered access to the modified source.

### How we satisfy §13

The branded Cyberdriver binary exposes the source link in three places:

1. **In-app "About" dialog** — added by the branding overlay, shows:
   > Cyberdriver (forked from RustDesk by Cyberdesk Inc.). Licensed under
   > AGPL-3.0. Source code: <https://github.com/cyberdesk-hq/cyberdriver-new>
   > and <https://github.com/cyberdesk-hq/cyberdriver-server>.

2. **`--source` / `--license` CLI flag** — running `cyberdriver --source`
   prints the URLs and exits 0. Same flag exists on the hbbs/hbbr binaries
   in `cyberdriver-server`.

3. **Status response on `cyberdriver_tunnel` and hbbs HTTP endpoints** —
   the `Server` HTTP response header on hbbs's web-client port (21118) and
   on Cyberdriver's local HTTP listener includes
   `Server: Cyberdriver (AGPL source: https://github.com/cyberdesk-hq/cyberdriver-new)`.

All three are populated from constants in the branding overlay
(`branding/source_links.rs`) so they stay in sync and do not drift on
upstream rebases.

## Section 6 obligation (conveying object code)

AGPLv3 §6 requires that when we distribute the binary form (MSI, .pkg,
.deb), we also offer the corresponding source. We satisfy this via §6.b:
public access on a network server at no charge. The two GitHub
repositories above are the canonical "Corresponding Source" locations.

## Upstream attribution

Per GPL §5.a, we preserve all upstream notices. Specific behaviors:

- The `LICENCE` file at the repository root (RustDesk's spelling) is
  unmodified.
- The branded "About" dialog credits both Cyberdesk and the original
  RustDesk authors.
- The `README.md` is updated to describe Cyberdriver, but adds a
  "Forked from RustDesk" note linking back to the upstream.
- Per-file copyright headers from upstream are preserved.

## Cargo feature isolation

Our modifications are isolated to:

- The new `cyberdesk` Cargo feature (Phase 4 of the integration plan).
- The branding overlay in `branding/`, applied by `scripts/apply-branding.sh`.
- A small set of conflict-zone files documented in
  [branding/README.md](branding/README.md).

This isolation is for upstream-merge hygiene, not legal effect. The
binary as a whole is AGPL-3.0 regardless of which Cargo features are
enabled.

## Internal contributor guidance

If you are a Cyberdesk employee contributing to this repository:

- Do **not** copy code from this repository into any non-AGPL project
  (including `cyberdesk-hq/cyberdesk-new`). The license boundary is
  enforced by being a different repository — keep it that way.
- The cyberdriver agent talks to the cyberdesk-new cloud over a network
  socket. That network boundary is what keeps cloud code out of the AGPL
  scope. Do not introduce build-time linkage between the two.
- If you are unsure whether a change crosses the line, ask in the
  internal #legal channel before opening the PR.

## Status

- AGPL compliance design: **drafted in this document**.
- Legal sign-off (Cyberdesk leadership / counsel): **Signed by Alan Duong, CTO of Cyberdesk**.
- In-binary source-link wiring: implemented as part of M1
  (`branding/source_links.rs` + About dialog + `--source` flag +
  `Server:` HTTP header).
