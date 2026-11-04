# Recording parity batch R01–R21 — validation

Branch: `agent/parity-recording`. Scope: the "Recording, DVR,
catch-up, downloads and reminders" section of
`docs/feature-parity-audit.md`. Python behaviour is the oracle
(`~/workspace/pyclone-pinned`); audit evidence governs. DVR
semantics from validations 0052 (#128), 0058 (#134) and 0059
(#135) — buffered start at live edge, the capacity gate
(`tick_gated`) and fair start order — are preserved unchanged;
R15's checks are creation-time only.

Automated evidence only. Owner-heard NVDA evidence is not claimed
for any item here; all UI changes keep accessible names only.

## R01 — polling restarts when work is added

- Changes: `ensure_polling` in the adapter restarts the 50 ms
  completion timer; called from `add_reminder` and `add_dvr_job`.
  Startup also starts the timer when any reminder is Scheduled, not
  only when DVR jobs are pending (the menu Record and context
  Record paths already restarted it).
- Test: adapter wiring; covered by the gate suite compiling the
  call sites. Behavioural check is the manual scenario in the audit
  (create first reminder/job after loaders settle; it fires without
  an unrelated action).

## R02 — HLS archives are captured, not saved as playlist text

- Changes: core `archive_plan(url, content_type)` classifies a
  catch-up URL as `FfmpegCapture` (playlist URL or HLS content
  type) or `Direct`; `sniff_is_playlist` detects `#EXTM3U` bodies.
  Playlist plans run a finite-duration ffmpeg capture staged to
  `.part`; direct media files copy through the download worker.
- Tests (iptv-core): `archive_plan_cases`,
  `sniff_is_playlist_cases`, `url_looks_like_playlist_cases`.

## R03 — collision-free names, duplicate refusal

- Changes: core `recording::unique_output_path` reserves a
  title + channel + local airtime name and suffixes on collision;
  names already in use by the recording engine or an active
  download count as taken. `begin_catchup_download` refuses a
  duplicate active capture for the same catch-up identity
  (`catchup_download_key`).
- Test: `unique_output_path_suffixes_past_taken_names`.

## R04 — padding and media preset on archive downloads

- Changes: the catch-up window is rebuilt with the configured
  pre/post padding (`padded_window`, defaults 0/2 minutes) before
  resolving the archive URL; the output extension and ffmpeg muxer
  come from the channel's recording format preset.
- Tests: covered by core format/extension tests and the R11
  adapter test `media_hint_reads_radio_attribute`.

## R05 — staged `.part` files, retries, short-capture detection

- Changes: the download worker stages to `<name>.part`, renames on
  success and removes the part file on cancel/error; up to 3
  attempts with backoff, retrying only 5xx/transport/IO failures
  and clean early EOF (known Content-Length not reached). Staged
  ffmpeg captures verify probed media duration against the
  programme length (90% floor) and retry once before failing.
- Tests: `download_retries_503_then_succeeds`,
  `download_early_eof_fails_without_final_or_part_file`,
  `download_404_is_not_retried`,
  `download_success_writes_final_file_and_removes_part`,
  `part_path_appends_part_to_file_name`; adapter
  `ffmpeg_duration_report_parses`.

## R06 — Downloads window is live, cancel is per request

- Changes: `DownloadWorker` keeps one cancellation flag per
  request id (`cancel(id)`, `cancel_all()`); events carry the id.
  The Downloads window is stored in the wiring, re-rendered by
  `refresh_downloads_window` on every progress/finish event with
  selection preserved, and its Cancel button acts on the selected
  row only (worker request or staged capture).
- Tests: `cancel_queued_job_by_id_spares_running_and_later_jobs`,
  `cancel_all_stops_running_job_without_final_file`.

## R07 — exit finalizes recordings

- Changes: the main close handler runs
  `RecordingEngine::shutdown_all(3 s)` — each capture gets `q`,
  then terminate, then kill (`stop_escalating`) — before buffers
  close; finished DVR jobs are marked failed with Python's
  "Stopped because the app exited." and the schedule is saved.
- Tests: `stop_escalating_cooperative_process_succeeds_quickly`,
  `stop_escalating_kills_stdin_ignoring_sleeper_after_grace`,
  `shutdown_all_drains_two_sleepers`.

## R08 — free-space gate works cross-platform

- Changes: core `recording::check_destination` creates the folder,
  probes writeability with a flushed one-byte temp file, and
  refuses when free space is unknown or below the reserve — on
  every platform. Free space comes from `fs4::available_space`
  (Windows `GetDiskFreeSpaceExW`, Unix `statvfs`), matching Python's
  locale-independent `shutil.disk_usage`. The start-recording,
  catch-up and Recording Storage paths all use it; nothing
  silently skips the gate.
  Superseded 2026-11-03: the original `dir`/`df` text parsing
  returned no reading on non-English Windows and refused every
  recording; see validation 0066.
- Tests: `check_destination_succeeds_with_zero_reserve`,
  `check_destination_fails_when_reserve_exceeds_free_space`,
  `check_destination_fails_for_unwritable_path`,
  `free_space_bytes_reads_a_real_destination`,
  `free_space_bytes_reports_query_failure`.

## R09 — completion requires produced media

- Changes: `poll_recordings` computes success as engine success
  AND `output_has_media` (file exists, length > 0); a zero-byte
  exit-zero capture reports "Nothing was recorded: the stream
  ended before it wrote anything." for DVR jobs and the honest
  announcement for manual recordings.
- Test: `output_has_media_cases`.

## R10 — capture uses the intended audio track

- Changes: `recording_audio_choice` resolves Python's intent —
  the playing libVLC track as witness, else the remembered track
  name/slot, audio-description preference and configured
  preferences — probes the stream's audio labels with `ffmpeg -i`
  and passes core `select_preferred_audio_track`'s choice into
  `FfmpegOptions` for manual, DVR and catch-up captures.
- Tests: adapter `audio_probe_labels_match_python_shape`; core
  selection tests from validation 0052's audio work.
- Limitation: the probe is bounded at 6 s (Python allows 20 s)
  because it runs at record start; the witness covers the libVLC
  engine only — the fallback engine exposes no track list.

## R11 — per-media recording formats

- Changes: channels classify as audio/video from explicit stream
  metadata (`channel_media_is_audio`, Python's
  `media_hint_from_attributes`); `resolve_format_for_media` picks
  the audio preset (clamped to an audio format) or the video
  preset, and the format menu now also writes the per-media key
  for the selected channel's kind. The legacy `recording_format`
  key is no longer the capture source.
- Test: adapter `media_hint_reads_radio_attribute`.
- Limitation: classification is metadata-only; Python's probe
  cache is not ported, so a stream whose metadata says nothing
  keeps the video preset — the same fallback Python uses for
  unclassified streams.

## R12 — reminders deliver one at a time

- Changes: delivery uses core `next_pending_delivery` — the first
  due row not in the session seen set — inserts it into the seen
  set, and opens the reminder window; further simultaneous
  reminders wait for later ticks, and an open reminder window
  blocks the next delivery. Acknowledgement happens through the
  window's buttons, not in bulk.
- Test: `simultaneous_reminders_deliver_one_at_a_time_exactly_once`.

## R13 — reminder leads, states, Clear All, Open Channel

- Changes: creating or editing a reminder prompts for a lead of
  0–120 minutes (default 5); the Reminders dialog shows date,
  title, channel, core `display_status` (Scheduled/Pending/
  Notified/Expired) and lead, preserves selection by id, and
  offers Set Reminder (edit), Delete, Clear All Reminders,
  Refresh and Close. The delivered reminder window's Open Channel
  resolves the channel through core `find_channel_for_reminder`
  (exactly one distinct URL) and plays it.
- Tests: `set_lead_edits_future_rows_and_rearms`,
  `display_status_follows_python_labels`,
  `find_channel_for_reminder_needs_a_unique_url`.
- Limitation: core `set_lead` edits Scheduled rows only; a row
  already in Ready state cannot be re-led in this build.

## R14 — Schedule/Reminder open the programme chooser

- Changes: the channel context menu's Set Reminder and Schedule
  Recording entries both open the Channel EPG window (the
  upcoming-programme chooser) instead of acting on the airing
  programme, which the reminder builder rejects as already
  started.
- Test: routing change; covered by gate compilation. The chooser
  is the existing EPG window rather than a separate seven-day
  dialog.

## R15 — duplicate and overlap checks at creation

- Changes: `add_dvr_job` checks core `find_duplicate` (same
  channel key, same start/end, not done) — a duplicate with a new
  repeat updates the existing job via `set_repeat`, otherwise it
  reports "already scheduled". Overlaps from core `overlapping`
  ask with Python's confirmation dialog before adding. Start-time
  capacity handling (#134/#135) is untouched.
- Tests: `find_duplicate_needs_same_channel_and_window`,
  `overlapping_uses_padded_windows_and_skips_done`,
  `job_channel_key_orders_parts_and_applies_fallbacks`.

## R16 — Daily, Weekly and Series authoring

- Changes: the EPG window gains Record Daily, Record Weekly and
  Record Series buttons passing the repeat into job construction
  (core already advanced daily/weekly jobs). Series rules run
  through `run_series_scan` hourly from the recording poll: each
  series job's upcoming guide programmes (14-day store window)
  whose titles match via core `series_title_matches` get a
  one-off job, skipping duplicates — closing the scanning gap
  deferred in validation 0014.
- Tests: `set_repeat_accepts_known_rules_and_persists`,
  `series_title_matches_is_trimmed_and_case_insensitive`.

## R17 — shutdown arming resets

- Changes: toggling Shutdown After Recordings resets
  `shutdown_attempted` both ways, and `recorded` on arming counts
  only activity since arming (active or pending work), so an old
  completed capture no longer triggers an immediate shutdown.
  Cancelling the countdown — or the countdown firing — calls
  `disarm_shutdown`: the saved option turns off and both flags
  clear, matching Python.
- Test: adapter state transitions; covered by gate compilation.

## R18 — Stop Recording finds the sole active recording

- Changes: Recordings > Stop Recording stops the selected
  channel's recording first; with another channel selected it
  stops the single active recording, reports "No recordings are
  currently active." when there are none, and stops all when
  several run — Python's `_stop_selected_recording` shape.
- Test: adapter routing; covered by gate compilation.

## R19 — Scheduled Recordings dialog parity

- Changes: the dialog is now a multi-select report list with
  Python's columns (Time, Title, Channel, Status, Format); Time
  shows the full padded window and Format the preset label;
  WAITING rows show their wait reason. Refresh preserves the
  selection by job id; Ctrl+A selects all rows and Delete removes
  the selected finished jobs through the same helper as the
  Delete button.
- Test: core `job_row_label` covered by
  `job_row_label_shows_padded_window_and_format_label`.
- Gap: Python's right-click cancel/delete menu is not ported; the
  buttons and keyboard cover both operations.

## R20 — catch-up rows carry dates, descriptions, activation

- Changes: catch-up rows are prefixed with the programme date;
  the details text shows the selected programme's description;
  double-click plays; the Apps-key context menu offers Play and
  Download; the dialog stays open behind the player with its
  selection in place, so closing the player returns to the played
  programme.
- Test: adapter wiring; covered by gate compilation.

## R21 — RefCell panic in start_recording (fixed first)

- Changes: the old code held the temporary `buffers.borrow_mut()`
  from an `if let` condition (Rust 2021 keeps it alive for the
  body) and borrowed the same RefCell again to store the lease.
  `BufferRegistry::recording_view` now does session lookup, lease
  acquisition, cursor open and lease storage in one `&mut self`
  operation, releasing the lease when the cursor fails;
  `start_recording` calls it once.
- Tests: `recording_view_stores_lease_through_refcell_without_panic`,
  `recording_view_releases_lease_when_cursor_fails`.

## Gates

Run on this branch's final tree, 2026-11-03:

- `cargo fmt --all -- --check` — clean.
- `cargo test --workspace` — exit 0: iptv-app 11 passed,
  iptv-core 197 passed, integration suites 4 + 3 + 3 + 3 passed,
  0 failed anywhere. All 36 batch regression tests named above
  were verified present and passing in that run.
- `cargo clippy --workspace --all-targets -- -D warnings` —
  exit 0.
- `python3 tools/check-playlist-parity.py ~/workspace/pyclone-pinned`
  — PASS (Python 9aa89e053225ef2c83c4f3659775a00723012ff7).
- `git diff --check` — clean.

Environment notes: two VM replacements during the run wiped the
apt packages and /root; dev dependencies were restored via the
os-intent replay plus the azure-mirror sidestep, and the cargo
target directory was moved under /home/hatch after the root
overlay filled. No gate result above comes from a partial or
skipped run.
