# Issue #30 (parity slice 7): recording/DVR + shutdown after recordings

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/recording`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::recording` ports recorder.py's core:
  - the ten format presets with labels, extensions and kinds, the
    muxer map for `.part` outputs, and the faststart set;
  - `sanitize_filename` (illegal/control characters to spaces, collapse,
    edge trim, 120-char cap, "Recording" fallback);
  - `ffmpeg_command`: log level (+datetime variant), stats period,
    rw_timeout, HTTP reconnect flags plus header input args
    (`-user_agent`/`-referer` dedicated, the rest as one CRLF
    `-headers` blob, with the proxy's default upstream User-Agent when
    the channel has none), rtsp TCP transport, `-t` duration, the
    per-format map/codec blocks (provider MP4's aac re-encode and
    teletext drop, provider MKA copy, x264 settings, the five audio
    presets with named-track picking), audio disposition marking for
    video formats, faststart, forced muxer, and the copy-to-stdout
    second output;
  - `unique_output_path`: `<Name> - <stamp>.<ext>` with ` (2)`, ` (3)`
    suffixes while the file or its `.part` is taken;
  - `RecordingEngine`: spawn ffmpeg (stdin piped), stop with `q` on
    stdin like the reference, poll finishes with success from the exit
    status;
  - power.py: `should_shutdown` (armed + something recorded + nothing
    active or pending) and the platform shutdown command candidates,
    run best-first.
- `iptv-core::dvr` ports dvr.py: `build_job` with padding and the
  reference statuses/titles and its invalid-window error; the
  `scheduled_recordings.json` store in the same version-1 shape beside
  the config file; load-time re-arming of recording/stopping jobs
  (failed when the window closed, scheduled again when still open);
  the tick state machine (start due, stop over, missed, stop timeout
  at 30 minutes); `mark_finished` keeping a canceled job canceled and
  scheduling the next daily/weekly occurrence with the reference's
  local-day advancement.
- Adapter: a Record button toggles a manual recording of the selected
  channel (config `recording_format`, default folder
  ~/Videos/Accessible IPTV Recordings or config `recordings_dir`);
  finishes announce saved/failed; the DVR ticks every 10 s on the poll
  timer, which now stays alive while recordings run or DVR jobs are
  pending; the shutdown gate fires once when its conditions first hold.

## Evidence

- 89 workspace tests pass (6 new: sanitize rules, provider-copy and
  audio-pick ffmpeg commands, the shutdown gate matrix, build_job
  padding/titles, the tick start/stop/missed/finish flow).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

- No live ffmpeg recording was made on this VM (no channel handy in
  this lane); the command builder is unit-pinned and the engine uses
  the reference's spawn/stop mechanics.
- Scheduled recordings from Stalker portal channels fail their job with
  a stated reason: they need a portal resolve at start time, which is
  not wired into the DVR path yet.
- Not ported: the recording log summary/statistics surface, the
  storage-reserve gate, the live-buffer recording relay
  (share-with-player), and guide-wide series-link scanning (its UI is
  the EPG viewer, slice 12 territory); `repeat: "series"` jobs are
  accepted by build_job like the reference but only daily/weekly
  auto-reschedule.
- Adding jobs from the UI likewise waits for the EPG viewer; the store,
  engine and tick are complete and consume jobs written by anything
  that speaks the file format (including Python's own file).
