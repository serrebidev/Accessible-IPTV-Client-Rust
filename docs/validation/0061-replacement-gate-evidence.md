# 0061 — Replacement-gate evidence pack (issue #137)

Scope: Farhan's approved 2026-11-02 specification, tickets
#128–#136, assessed against Rust v0.4.0 (4c1695a) and the
pinned Python reference 9aa89e053225ef2c83c4f3659775a00723012ff7.

This pack records evidence; it does not authorize anything.
Per ADR 0004 (replacement-readiness threshold) and ADR 0001,
the Python client remains the shipping product and this
evidence does not authorize switching the shipping product,
a release decision, or publication. Releases v0.4.1–v0.4.3
shipped the work below under the standing release rule; the
replacement question itself stays with the owner.

Evidence kinds are kept distinct throughout: **automated**
(machine-run, reproducible), **live** (real environment,
real services), **owner-heard** (the owner's own NVDA/keyboard
pass), **historical** (true when recorded, not re-run),
**blocked** (cannot run yet, owner named), **waived**
(owner explicitly excused — none in this pack), **unrun**
(not attempted).

## Final gate record

Run after the last related edit (PR #149, the core-owned
fallback stop decision), on the branch tree that merged
unchanged: `git diff a91c7f3 f4b3e71` is empty, so this record
applies exactly to revision
**f4b3e71826c9de547edb10c99c099a84c3eb5132** (main, post-#149).
Dirty-file set at gate time: none (`git status --porcelain`
empty). Environment: this workspace's Linux VM,
LIBCLANG_PATH=/usr/lib/llvm-18/lib, vendored wxDragon 0.9.24.

- `cargo fmt --all --check` — exit 0
- `cargo check --workspace` — exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0
- `cargo test --workspace` — exit 0; 185 passed, 0 failed
- `python3 tools/check-playlist-parity.py <pinned Python 9aa89e0>` — exit 0, PASS
- `git diff --check` — exit 0

Publication evidence (live): v0.4.3 (tag at 778b7c5) Release
run 41583941706 completed success — Build linux, windows,
macos and Publish release all success — with exactly the 7
expected signed assets, verified by direct API read on
2026-11-03. v0.4.1 and v0.4.2 were verified the same way
(runs and asset lists in their slice records).

## Ticket results

- **#128 playback confirmation and bounded recovery** —
  implemented, PR #139, validation 0052. Automated: session
  tests on injected time (confirmation-only Playing, 6-retry
  budget, 30 s deadline anchored at choice, no-media and
  prolonged-buffering restarts, Lost state). Live real-provider
  playback: **unrun** here; tracked by #87.
- **#129 fallback controls and errors** — implemented,
  PR #144, validation 0057; the withholding decision moved
  into the core with an inversion regression in PR #149
  (`fallback_stop`: unconfirmed Stopped reports withhold at
  any position; confirmed stops classify Ended/Stalled).
  Automated. Installed-libVLC-present and libVLC-absent
  desktop checks: **blocked** on the Windows environment,
  tracked by #87.
- **#130 late-start and cancellation outcomes** —
  implemented, PR #140, validation 0053. Automated: buffered
  start at live edge, padded stop, expired/Missed, late flag
  on all three start paths, dialog cancel stopping capture.
  Live recording against a real provider: **unrun**; #87.
- **#131 SAPI and screen-reader subtitle speech** —
  implemented with #132, PR #142, validations 0055/0056.
  Automated: routing, purge triggers, backend refusal when
  the application voice is unavailable, Windows-target compile
  of the SAPI FFI (`cargo check -p iptv-sapi --target
  x86_64-pc-windows-gnu`). Live SAPI speech on Windows:
  **blocked** on the Windows environment; #87.
- **#132 device loss and voice failures** — implemented with
  #131, PR #142, validation 0056. Automated: router migration
  (exact ID, unique-name legacy), policies (pause-until-return,
  system default), failure-note texts, saved selection retained
  across fallback. Physical device loss: **blocked** on
  hardware; #87/#88.
- **#133 settings-backup interoperability** — implemented,
  PR #141, validation 0054. Automated plus one **live**
  cross-check performed in-session: a fresh backup generated
  by the pinned Python (synthetic credentials only) decrypts
  in Rust byte-for-byte, and a Rust-written backup decrypts
  in Python; both directions recorded in 0054. This is the
  only live-environment item in the pack that is complete.
- **#134 recording conflicts** — implemented, PR #145,
  validation 0058. Owner gate: the proposed behavior was
  **accepted** by Brandon on 2026-11-02 ("Accept both
  behaviours as proposed", recorded on the issue) — accepted,
  not deferred and not waived. Automated: capacity gate,
  waiting lifecycle, holds, account identity and limits.
  Live single-connection account conflict: **unrun**; #87.
  Owner-heard conflict discovery without focus theft:
  **blocked** on the owner's pass; #86.
- **#135 fair waiting queue and restart recovery** —
  implemented, PR #146, validation 0059. Automated: fair
  order, tie-breaking, same-tick accounting, boundary race,
  restart reassessment, provider independence.
- **#136 recovery backups before imports** — implemented,
  PR #147, validation 0060. Owner gate: accepted with #134
  (above). Automated: verified-copy round trip, wrong
  confirmation, mismatch, tamper, empty password; ordering
  structural in the adapter. Owner-heard dialog sequence and
  focus restoration: **blocked** on the owner's pass; #86.

## Outstanding evidence (explicit, not passes)

- Owner-heard NVDA/keyboard evidence — labels, states,
  traversal, conflict discovery without focus theft, focus
  restoration after the import dialogs, error recovery —
  **blocked**, owner: Brandon, tracked by #86. Nothing in
  this pack substitutes for it.
- Live Windows evidence — controlled-media and real-provider
  playback and recording, installed-libVLC and fallback
  behavior, Windows SAPI output, physical device loss —
  **blocked**, tracked by #87 (Windows VM seams) and #88
  (physical renderer). #89 (Windows packaging rehearsal)
  remains the packaging gate.
- Historical evidence from earlier slices (validations
  0001–0051, the 12-slice parity program, the GUI parity
  program) is **historical**: true when recorded against its
  own revisions, not re-run for this pack beyond the final
  gate above and the playlist parity check it includes.

## Verifier

A fresh-context read-only verifier reviewed this pack
against primary sources only — the repository files and the
GitHub API — before publication. Result: no mismatches. All
nine ticket-to-PR-to-validation mappings, the gate revision
and its merged-unchanged claim, the v0.4.3 run and its seven
assets, the ADR 0004 paraphrase, and the `fallback_stop`
function and its inversion test were each confirmed against
their sources, and no unrun or blocked item is phrased as a
pass. The verifier's one note — that this section was a
placeholder at review time — is discharged by this paragraph.
