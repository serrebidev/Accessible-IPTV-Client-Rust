# Validation 0034: replacement-readiness evidence ledger

Issue: #84. Current revision: `19cc678` on
`integration/stalker-dvr-resolution`.

This ledger closes the documentation and deterministic implementation work for
the replacement-gate pack. It is an evidence summary, not a release approval.

## Evidence matrix

| Area | Status | Evidence | Boundary / next action |
| --- | --- | --- | --- |
| Workspace source gates | Pass | `cargo fmt --all -- --check`; `cargo check --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace --quiet` (122 tests); `git diff --check` | Automated evidence only. |
| Python playlist parity | Pass | `tools/check-playlist-parity.py` against pinned Python revision `9aa89e053225ef2c83c4f3659775a00723012ff7` | Domain evidence; not desktop accessibility evidence. |
| Playback/DVR deterministic behavior | Pass | [Validation 0030](0030-replacement-playback-dvr.md), including stale/canceled DVR resolution regression | Live libVLC and ffmpeg recording checks remain open. |
| Owner-heard accessibility | Not run for this revision | [Validation 0031](0031-replacement-owner-heard-accessibility.md) defines the gate; [Validation 0006](0006-owner-heard-accessibility.md) records an older limited owner session | Owner must rerun the checklist against this revision with NVDA and record speech/focus. |
| Casting protocol | Pass | Existing cast-core tests and [Validation 0032](0032-replacement-casting-renderer.md) | Protocol evidence does not prove physical playback. |
| Physical renderer | Blocked | [Validation 0032](0032-replacement-casting-renderer.md) | Requires a compatible renderer on the test network; no hardware evidence is recorded here. |
| Packaging/updater deterministic behavior | Pass | Existing updater tests, `PACKAGING.md`, [Validation 0033](0033-replacement-packaging-updater.md) | Full VM artifact inspection still needs to be run and recorded. |
| libVLC-present playback | Not run | libVLC absence/fallback test passes; [Validation 0028](0028-libvlc-engine.md) records no libVLC on the earlier VM | Run the Windows VM lane with libVLC installed. |
| Live ffmpeg recording | Not run | Command builder and lifecycle code are covered by deterministic tests; [Validation 0030](0030-replacement-playback-dvr.md) defines the live lane | Run against a sanitized media source and record tool/artifact identity. |

## Readiness recommendation

**Not replacement-ready yet.** The automated and deterministic gates are green,
but the owner-heard NVDA session, installed-libVLC playback, live ffmpeg
recording, and physical-renderer evidence are not complete for this revision.
The packaging VM procedure is defined but its artifact run is also still open.

This recommendation does not claim that explicitly unported Python features are
complete. Chromecast, AirPlay/RAOP, Sonos native control, Yamaha side-channel
control, the Python installer flow, DVR statistics/storage-reserve/live-buffer
relay/series-link surfaces, and release publication remain outside this pack.

## Next actions

1. Build the current revision in the controlled Windows VM and record package,
   updater, libVLC, and ffmpeg evidence.
2. Have the owner run [Validation 0031](0031-replacement-owner-heard-accessibility.md)
   with the normal NVDA configuration.
3. Run the physical-renderer section of [Validation 0032](0032-replacement-casting-renderer.md)
   when compatible hardware is available.
4. Update this ledger with each artifact and reassess readiness; do not publish
   a release solely because automated tests remain green.
