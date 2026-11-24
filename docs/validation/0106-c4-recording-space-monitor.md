# 0106 — C4: recording space monitor (round 3)

Port target: Python v1.151.6 `storage_monitor.py` and the
Recordings menu item plus `SpaceSettingsDialog` in
`accessible_tools_ui.py`, the recorder's recording-space
thread in `recorder.py` (`_ensure_space_monitor`,
`_monitor_space`), and `_recording_space_warning` in
`accessible_tools_ui.py`. Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C4).

## What landed

- Core (`storage_monitor.rs`, new): Python's `settings`
  verbatim — thresholds are ints in 16..=1048576, warning
  defaults to 1024 MiB and is raised to at least the stop
  threshold, the stop default is 256 MiB, monitoring is on
  unless the config says JSON false (`is not False`),
  automatic stopping is on only for JSON true (`is True`).
  `SpaceMonitor::check` is Python's `check` verbatim:
  recordings group by drive (`st_dev` plus drive letter,
  Python's group key), one report per drive, unavailable
  query warns without stopping, warning below the warning
  threshold, graceful stop of every not-already-stopping
  recording on a critically full drive when stopping is
  on, and forgetting a drive only after free space
  recovers past 110% of the warning threshold (or after
  an unavailable report). The free-space probe reuses
  `recording::free_space_bytes` (fs4), which asks the
  platform like Python's `shutil.disk_usage`.
  `RecordingEngine::active_recordings` snapshots the
  running recordings (key, written path, stopping),
  skipping processes that already exited, as Python's
  recorder does.
- App (`main.rs`): the Recordings menu gains `Recording
  Space Monitor...` after `Check Recording Storage...`
  (id 5229, no shortcut — Python has none), F1 on it
  opens the `recordings` help topic. The dialog carries
  Python's labels verbatim — `Recording Space Monitor`,
  `Monitor free space while recording`, `Warn below
  (MiB):`, `Stop recordings safely when space is
  critically low`, `Stop below (MiB):` — with
  `OK`/`Cancel`, both spin controls on the 16..=1048576
  range named by their labels, help topic `recordings`,
  and Python's validation: OK with the warning below the
  stop threshold keeps the dialog open with
  `The operation failed:` /
  `The warning threshold must be at least as high as the
  stop threshold.` A successful OK saves all four keys
  (`recording_monitor_space`, `recording_warn_free_mib`,
  `recording_stop_free_mib`, `recording_stop_low_space`)
  in one config save, as Python does. The housekeeping
  tick runs `poll_space_monitor` every second and checks
  every 5 s while recordings are active; with none left
  the per-drive memory clears, as when Python's thread
  exits. A warning, stopping or unavailable report
  shows Python's exact message under `Recording Storage`
  as a queued warning box; a critical drive stops its
  recordings with reason `storage` and releases their
  buffer leases.
- English help (`docs/help/en.md`, Recordings): the
  Python space-monitor paragraph verbatim, between the
  stopping paragraph and the Recording Library paragraph,
  as in Python. All dialog and message strings already
  ship in every patois catalogue; the help paragraph
  joins English only, per the translator-sync design in
  validation 0091.

## Evidence

- Core tests beside the code: `settings_use_defaults`,
  `settings_validate_ranges_and_keep_warning_above_stop`
  (bounds, floats, bools, warning raised to the stop
  threshold), `settings_read_only_json_bools` (only real
  JSON bools count), ports of
  `test_space_monitor_warns_once_per_drive_and_stops_all_affected_recordings`
  and
  `test_space_query_failure_warns_without_stopping`
  (the injected free-space probe stands in for Python's
  `shutil.disk_usage` monkeypatch), plus
  `disabled_clears_the_notified_state`,
  `only_a_real_recovery_forgets_the_warning`,
  `an_unavailable_drive_recovers_without_the_full_recovery`
  and the Windows-only
  `different_drives_warn_separately`.
- `cargo test -p iptv-core`: all tests pass.
  `cargo clippy -p iptv-core -p iptv-app --all-targets
  -- -D warnings` green on Windows.

## Brandon's NVDA check

1. Recordings menu: `Recording Space Monitor...` sits
   after `Check Recording Storage...`; F1 on it opens the
   Recordings help topic with the space-monitor
   paragraph.
2. Open the dialog: the two check boxes and two spin
   controls read `Monitor free space while recording`,
   `Warn below (MiB):`, `Stop recordings safely when
   space is critically low`, `Stop below (MiB):`; Tab
   reaches each once.
3. Set Warn above Stop and press OK: the dialog stays
   open with the threshold error and focus stays in the
   dialog; set them valid and OK saves and closes.
4. Start a recording on a drive whose free space is
   below 1024 MiB: one `Recording Storage` warning
   (private drive count) shows, and repeats only after
   space recovers past 110% of the threshold.
5. Enable `Stop recordings safely when space is
   critically low`, and fill the drive below the stop
   threshold while recording: the recordings on that
   drive stop gracefully with the critical message.

## Residuals

- The check runs on the 1 s housekeeping tick with a
  5 s gate, so the first check after a recording starts
  lands within a second; Python's thread checks in the
  same moment.
- Python's `_suppress_recording_notifications` has no
  Rust equivalent, so space warnings always show, even
  while other recording notifications are suppressed.
- A failed config write is silent here, as everywhere
  else in Rust; Python would show `error_box`.
- The `_save_recent_broadcast` reserve bump (warning/
  stop thresholds raising the buffer-clip reserve) is
  C5's ground (`client_timeshift.py`); the settings keys
  it reads are now stored, so C5 can use them.
