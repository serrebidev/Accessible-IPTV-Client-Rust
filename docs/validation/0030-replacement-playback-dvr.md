# Validation 0030: deterministic playback and DVR replacement gates

Issue: #80. This record closes the deterministic portion of the replacement
gate. It does not claim owner-heard speech, live provider playback, or a live
ffmpeg recording.

## Fixture and privacy boundary

The playback and provider tests use the committed synthetic playlist and core
fixtures. URLs are inert test data; no fixture contains credentials, private
portal addresses, or a personal stream. Provider resolution is exercised at the
typed request/result seam, so the test suite does not contact a portal.

## Automated evidence

- The core recording tests cover sanitized filenames, provider-copy and named
  audio-track ffmpeg command shapes, and the shutdown gate matrix.
- The libVLC crate covers state mapping and graceful construction failure when
  the optional library is absent. The installed-library path remains an
  environment-dependent VM gate.
- DVR tests cover due-job resolution requests, resolving-state suppression,
  retry before the recording window ends, expiry failure, and both late
  failure and late success after cancellation.
- The workspace regression gate is `cargo fmt --all -- --check`,
  `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --workspace --quiet`, `tools/check-playlist-parity.py`,
  and `git diff --check`.

## Environment-dependent evidence

- A Windows VM with libVLC installed must exercise actual playback, track
  enumeration, seek/volume, and end-of-stream behavior.
- A VM with ffmpeg and a sanitized media source must exercise a real recording
  start, graceful stop, successful completion, and failed-process report.
- Those live checks must record tool versions and artifact identity. A missing
  dependency is `blocked`, never `pass`.

## Result boundary

Automated evidence proves deterministic state and command behavior only. It does
not prove audible accessibility, physical input, real network playback, or a
provider account integration.
