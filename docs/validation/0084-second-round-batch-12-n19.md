# 0084 — Second-round batch 12: update-check cadence and last-check record (audit N19)

## Scope

- Audit finding N19 in `docs/feature-parity-audit.md`, against
  Python `main.py:650`–`652`, `:1235`–`1282` and
  `:4662`–`4737` in the pinned oracle for the update-check
  behavior, and `installer/`, `tools/build_deb.py`,
  `updater.py:909`–`969`, `main.py:4774`–`5191` for the
  packaging half.

## Verdict on the packaging half (no code)

- The Windows installer, the Debian package and the
  Authenticode verification are superseded by the ship-shape
  packaging Brandon approved on 2026-11-01 (`PACKAGING.md`,
  status: live): one fixed-name portable archive per
  platform (`accessible-iptv-client.zip` / `.tar.gz` /
  `.dmg`, contents at the root), each with a `.minisig`
  sibling that ship-shape verifies against the public key
  compiled into the app before anything installs; Windows
  and Linux self-install and relaunch, macOS opens the
  verified dmg for a manual install. Minisign verification
  is this design's answer to Authenticode; no installer or
  .deb will be built.
- The one update window Python drives through download,
  install and restart is ship-shape's UI
  (`ship_shape::ui::run_update_check`), in place since
  v0.4.0. Python's report of the update result on the next
  start belongs to its own updater's flow; ship-shape owns
  install/restart and offers the app no next-start report
  hook, so that piece is covered by the same verdict.
- What remained genuinely missing was cadence bookkeeping:
  Rust fired the launch check immediately at frame build
  and never recorded `update_last_auto_check_epoch`.
  (Hourly in-session repeats already existed — the S07
  housekeeping block, whose handler re-reads the option
  each time like Python's timer handler.)

## What changed

- App (`main.rs`), Python's `_schedule_auto_update_check`
  shape:
  - `run_auto_update_check(wiring, frame)` is one automatic
    attempt: it fires `check_updates` with the Automatic
    trigger and records `update_last_auto_check_epoch`
    (unix seconds, via `now_epoch()`) with
    `persist.set_and_save`. Python writes the same key in
    `_record_auto_update_check_attempt`, called from its
    check worker's `finally` block for non-interactive
    checks only — so every automatic attempt is recorded
    (failed ones included) and manual checks never touch
    it; the Help-menu manual path is unchanged.
  - The startup check is delayed: `maybe_auto_check_updates`
    no longer fires at frame build. When the option is
    enabled it sets the new Wiring field `auto_check_due`
    to now + 5 s (Python's
    `_AUTO_UPDATE_DELAY_AFTER_PLAYLIST_MS = 5000`, "a few
    seconds in so playlist loading is not disturbed"), and
    the always-on 1 s housekeeping tick fires it when it
    falls due, clearing the slot first so it fires once.
  - The hourly housekeeping checks now route through
    `run_auto_update_check`, so hourly attempts record the
    epoch too, as every Python automatic attempt does.

## Evidence

- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (378 tests), playlist parity check PASS,
  `git diff --check` clean.
- The cadence is timer/GUI state around ship-shape's own
  thread; it is exercised structurally by the call sites
  rather than unit tests (wx handles cannot be built
  headless), the same position as N18's queue machinery.

## Residuals

- Recorded at launch, not at completion: Python records in
  the worker's `finally`, ship-shape reports no completion
  back to the app, so the epoch is written as the attempt
  is launched. The stored meaning — when the last
  automatic check was attempted — is the same, and failure
  behavior matches (Python records failed attempts too).
  A launch that ship-shape deduplicates internally still
  records an attempt.
- The startup fire re-reads the option: disabling
  auto-check within the first 5 s of a launch suppresses
  that launch's check, where Python's already-scheduled
  `CallLater` lambda still fires once. This matches the
  established S07 hourly handler semantics in this app
  (its timer handler re-reads the option, exactly like
  Python's `_on_update_check_timer`).
- ship-shape's update window remains its own UI on its own
  thread (the N18 residual stands: the modal-depth counter
  cannot see inside it).
