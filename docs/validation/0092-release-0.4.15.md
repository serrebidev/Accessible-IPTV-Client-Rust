# 0092 — Release v0.4.15

Date: 2026-11-05. Branch `agent/release-0.4.15` from main 0d2f2f5.

## Contents

Second-round audit closeout release. Batches 8–11 and the parallel
fan-out, all merged to main before this release branch was cut:

- Batch 8 (PR #191): N15 catch-up/download details; wxMenu::Delete
  assert fix (`menu_delete_if_present` guard at every rebuild site).
- Batch 9 (PR #192): N16 stream-type probing and per-type recording
  formats.
- Batch 10 (PR #193): N17 Account Info detection + Copy Details.
- Batch 11 (PR #194): N18 modal message queue.
- Batch 12 (PR #195): N19 update-check cadence (5 s delayed startup
  check, hourly re-checks, `update_last_auto_check_epoch` on automatic
  attempts only); packaging half recorded superseded by ship-shape.
- Batch 13 (PR #201): N20 Chromecast casting (mdns-sd discovery,
  `chromecast` crate session on the cast worker thread, cast proxy,
  connected prompt, saved-position/buffer refusals,
  `single_stream_busy` caster arm, Windows firewall rule).
- Batch 14 (PR #202): N21 EPG import details (gzip resume,
  cross-process import lock, background thread priority, old-schema
  repair prompt, import started/finished messages).
- Batch 15 (PR #200): N22 browsing details (per-playlist category
  branches with "Other playlists", account-type scope labels,
  Favorites messages, Delete removes a favorite).
- Batch 16 (PR #199): N23 player details (Python status-line format,
  spoken track names, Reconnecting/Stream lost, subtitle load
  messages, mouse-wheel volume).
- Batch 17 (PR #196): N24 dialog explanations (Python's texts as
  dialog text, never control descriptions).
- Batch 18 (PR #197): N25 smaller dialogs (About with community
  links, Custom Player typed path + Browse, What Is Playing wording,
  Go to Channel errors).
- Translations (PR #198): Python's gettext catalogues (14 languages,
  both domains) and translated help guides bundled byte-identical;
  `release.yml` staging copies `locale/` (staging-only change).

## Dependency pass

`cargo update -w` run. mdns-sd spec raised 0.20 → 0.21, locked at
0.21.7 (the spec was the only hold). Held back, both pinned by
upstream crates: protobuf 3.2.0 (chromecast 0.18.2's requirement;
0.18.2 is the newest chromecast published), generic-array 0.14.7
(block-buffer/digest chain via minisign/scrypt — standing hold,
rechecked).

## Gates

Run on the release branch in a fresh isolated target dir
(`rel0415-target`), because the shared target dir cross-contaminated
worktree fingerprints during the fan-out:

- `cargo fmt --all` + `--check` clean
- `cargo clippy --workspace --all-targets -- -D warnings` clean
- `cargo test --workspace`: 394 passed, 0 failed
- playlist parity vs pinned Python 9aa89e05: PASS
- `git diff --check` clean
- ALL-GATES-OK

## Residuals

- Chromecast verified by compile/tests only; no device in this
  environment. Brandon's on-device pass is the acceptance step.
- N21's agent ended before its final summary; the PR was verified
  directly (PR body, validation 0086, files) and this release gate
  run is the authority for the merged tree.
- Brandon's NVDA passes remain his: status-line wording and wheel
  (N23), reworded dialogs (N24), About links (N25), Account Info
  window (N17), a non-English language pass (translations), and the
  wxMenu::Delete assert retest — the assert he hit on 2026-11-05 was
  on a v0.4.14 CI build, which predates the PR #191 fix.
