# 0063 — Feature-parity audit closure (E01–E12, B01–B21, P01–P21, R01–R21, S01–S10)

Date: 2026-11-03. Scope: the 85 findings in `docs/feature-parity-audit.md`
(Python v1.150.58 `6afe601` versus Rust v0.4.4 `ea9131d`). Method: five
parallel batch branches, one per area, each landing regression tests first and
gating on its own tree; serial integration on `agent/parity-integration` with
conflict resolutions reviewed hunk by hunk; final gates on the merged tree.

## Merged-tree gates (this record's evidence)

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0, zero errors.
- `cargo test --workspace`: **299 passed, 0 failed** (268 iptv-core lib, 16
  iptv-app, integration suites 4+1+3+3, iptv-vlc 4).
- `python3 tools/check-playlist-parity.py` against the pinned Python oracle
  `9aa89e0`: PASS (pin and goldens unchanged).
- `git diff --check`: clean.

Per-item evidence lives in the batch records, which are part of this merge:
`docs/validation/batch-epg.md` (E01–E12), `batch-playlists.md` (B01–B21),
`batch-playback.md` (P01–P21), `batch-recording.md` (R01–R21),
`batch-platform.md` (S01–S10). Each audit entry in
`docs/feature-parity-audit.md` now carries a `**Fix:**` line linking its
evidence. Integration-specific decisions: the context-menu Reminder/Schedule
actions act on an EPG search row's own programme and open the Channel EPG
chooser from a plain channel row (satisfies E08 and R14 together); the guide
source computation is the single `effective_epg_sources` (own `epgs` ∪
`epg_urls`, provider extras, playlist-header guides with the opt-out);
`selected_channel` resolves EPG rows everywhere including Stop Recording.

## Not fully closed (recorded, not hidden)

- **B19 partial:** catalogue clone removed, O(1) favorite membership, VOD
  filtering off the UI thread. Python's background navigation index was not
  built; 500k-playlist latency is unmeasured.
- **E12 algorithmic only:** bulk now/next snapshots; no scale benchmark.
- **E10 deviation:** no-guide-match channels keep the existing placeholder
  text rather than Python's exact wording.
- **P18 residual:** the VLC engine's final release at app exit still runs on
  the UI thread (handle created once, never replaced).
- **R10 bound:** preferred-audio probe capped at 6 s (Python allows 20 s).
- **R19 omission:** Python's right-click menu on Scheduled Recordings not
  ported; buttons and keyboard cover the actions.
- **S05 deviation:** logs live in the config directory's `logs` folder on all
  platforms.
- **S10 limit:** wxdragon exposes no API to identify an open menu's
  highlighted item; F1 from an open menu uses the last focused control's
  topic.

## Acceptance boundary

This is automated and source-level evidence. Owner-heard NVDA acceptance of
the changed surfaces remains Brandon's, under ADR 0004 and the still-open
gate pack #86–#89 / #137. The Python client remains the shipping product; no
swap is authorized by this record.
