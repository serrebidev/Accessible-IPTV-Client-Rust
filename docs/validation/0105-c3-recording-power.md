# 0105 — C3: recording power (Prevent Automatic Sleep, Wake for Scheduled Recordings) (round 3)

Port target: Python v1.151.6 `recording_power.py` and the
Recordings menu toggles in `accessible_tools_ui.py`
(`_add_recording_power_menu`, `_start_recording_power`,
`_recording_power_failed`), `main.py` lifecycle. Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C3).

## What landed

- Core (`recording_power.rs`, new): `supported()` (Windows
  only), Python's `desired_state` exactly — strict `is True`
  option checks, scheduled jobs in `now - 15 .. now + 366d`,
  stay awake up to 90 s before the next start, wake timer at
  start minus 60 s (at least `now + 1`). The Windows backend
  drives `SetThreadExecutionState` (`ES_CONTINUOUS` /
  `ES_SYSTEM_REQUIRED`) and a waitable wake timer
  (`CreateWaitableTimerW` / `SetWaitableTimer` /
  `CancelWaitableTimer`, manual-reset, FILETIME due time);
  `ERROR_NOT_SUPPORTED` after a successful set is still a
  failure, as in Python. The manager thread (`"RecordingPower"`)
  polls the snapshot every 5 s, wakes early on `refresh()`,
  reports one failure per failure streak, and releases the
  backend on its own thread before joining.
- App (`main.rs`): Recordings menu gains `Prevent Automatic
  Sleep While Recording` (id 5227, checked by default) and
  `Wake for Scheduled Recordings (Windows)` (id 5228, off by
  default), both disabled off Windows, help topic
  `recordings`, no shortcuts (Python has none). A toggle
  saves via `set_and_save`, starts the manager if needed and
  syncs it. The manager starts at startup before the playlist
  load (Python deferred startup order), the housekeeping
  tick syncs the snapshot (config, active recordings, non-done
  DVR jobs) every second and drains failures onto the UI as
  `Recording power management failed:\n{error}` under title
  `Recording`, and app exit closes the manager, removing the
  wake timer and the execution-state request.
- Config defaults and normalization carry
  `prevent_sleep_while_recording` (true) and
  `wake_for_recordings` (false), as Python `options.py` does.
- English help (`docs/help/en.md`, Scheduled recordings):
  both Python paragraphs verbatim. All 13 translations of
  the labels already ship in the catalogues; the help
  paragraphs join English only, per the translator-sync
  design in validation 0091.

## Evidence

- Core tests beside the code: `recording_power.rs` ports
  Python's `test_features_round2.py` power tests —
  `desired_state_follows_only_pending_jobs_and_guards_the_wake_interval`
  (940 alarm, 950 imminent awake, active awake, defaults,
  canceled ignored, stale/distant ignored, prevent off),
  `desired_state_reads_only_json_true` ("yes"/1 refused),
  `backend_is_released_on_the_same_thread_after_failure`
  (one report, `update` then `close` on the manager thread),
  `platforms_agree_on_support`.
- `cargo test -p iptv-core`: 410 lib tests pass, plus all
  integration binaries. `cargo clippy -p iptv-core -p
  iptv-app --all-targets -- -D warnings` green on Windows.

## Brandon's NVDA check

1. Recordings menu: `Prevent Automatic Sleep While Recording`
   is checked, `Wake for Scheduled Recordings (Windows)` is
   unchecked; both say so when you land on them, and both
   work as check boxes (Space toggles, the check state is
   what Alt+type would announce).
2. F1 on either item opens the Recordings help topic with
   the two sleep/wake paragraphs.
3. Start a recording: the computer stays awake; stop it and
   the request clears (audible only in that sleep would
   otherwise happen — this needs a real overnight schedule
   for full proof).
4. Enable Wake, schedule a recording in ~2 minutes: the
   computer wakes from sleep just before the padded start
   (needs wake timers permitted in the power plan — an
   honest system-level test, like Python's).
5. Exit the app: the wake timer is removed (a second run
   with no schedule does not wake the machine).

## Residuals

- Off Windows both items are disabled, as in Python; the
  whole feature is a no-op there.
- The snapshot syncs every 1 s from the housekeeping tick
  instead of Python's event-driven `refresh()` calls, so a
  schedule change or recording start lands within a second
  rather than immediately.
- A failed config write is silent here, as everywhere else
  in Rust; Python would revert the check box and show
  `error_box`.
