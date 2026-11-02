# Issue #17 (parity slice 1): configuration and persistence

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/config-persistence`. Python remains the shipping product and the
behavior oracle.

## Implemented behavior

- `iptv-core::config` ports the Python `options.py` contract: the same
  JSON format in `iptvclient.conf`, the same default key set, and unknown
  keys preserved across load/save round trips.
- Load scans candidates (per-user config dir, then a portable
  `./iptvclient.conf`); an unreadable or non-object file is copied to
  `<name>.corrupt` and the next candidate is tried; defaults are the last
  resort and startup never fails on config.
- Normalization matches the reference: internal-player buffer bounds
  (including the legacy max == 18 migrating to 30, base clamped to max,
  variant cap 0.25–500 or 0), announcement level clamped 0–3, recording
  format clamped to the known presets with the per-media migration,
  recording padding clamped 0–180 minutes, string-list coercion
  (comma-split bare strings, trim, dedupe, order preserved), and bool
  coercion for the hand-edited string forms.
- Save is atomic: normalize, write a unique temp file in the same
  directory, fsync, rename; the temp file is removed on failure.
- The desktop adapter loads the config at startup, seeds the browse seam's
  favorites from the stored keys, and saves after every successful toggle.
  Save failures are best-effort and never break toggling (the reference
  logs them the same way).

## Evidence

- 48 workspace tests pass (5 new config unit tests covering defaults,
  buffer bounds and the 18-migration, list/bool coercion, wrong-shape
  rejection, corrupt preservation and a save/load round trip).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

Adapter behavior is covered at the core seam (load → seed → toggle → save
→ reload). An owner-heard NVDA pass is not part of this slice; the only
user-visible change is that Favorites now survive a restart.
