# 0073 — Second-round batch 2: N13 catch-up direct-file probing

Date: 2026-11-04. Scope: the probing half of audit finding N13
(catch-up on one-connection providers), plus the truncation
alignment from the same Python flow. The recording-relay /
busy-refusal half of N13 is separate work and stays open in
the audit.

## What was wrong

Rust catch-up always captured the HLS playlist with ffmpeg for
the padded window. Python first probes for the provider's
direct file (`catchup_direct.py`): on backends that expose
`index-<utc>-<duration>.mp4` next to the playlist, the file
downloads as fast as HTTP allows while the HLS pipeline paces
itself segment by segment. Python's probe is careful on
one-stream accounts: it never follows a redirect (rewrites HEAD
into GET on some stacks and opens the media), it probes
candidates around each redirect target before requesting the
target, it never spends a ranged GET on a 403 (busy account) or
404, and it waits out a 2.5 s settle after touching the media
before ffmpeg asks — an unsettled open makes the provider
refuse the real download. Rust also judged truncation against
the programme-exact duration at a 0.9 ratio; Python expects
the padded window minus its padding at 0.95.

## What changed

- New wx-free core module `catchup_direct`: pure candidate
  building (server-stamped numbers first, caller window second,
  first-path-segment home third, query preserved, deduped),
  host comparison with scheme default ports, `headers_for_host`
  (credentials and custom extra headers never follow a redirect
  to another host), the media content-type gate, the redirect
  walk (at most 5 hops, candidates probed around each target
  before it is requested, loop-safe), and the settle registry
  (`note_media_released`, `media_settle_remaining`,
  `settle_media_session`). One `ureq` transport: redirects
  refused, every HTTP status surfaced, response bodies dropped
  unread. A one-job-at-a-time `DirectProbeWorker` runs the
  probe (and its settle) off the UI thread.
- `begin_catchup_download`: the ffmpeg-capture branch now
  stages the entry and starts a probe with both windows in one
  request — padded first, programme-exact second, matching
  Python's order. The row reads "Checking for a faster
  download..." while probing. `poll_direct_probes` (completion
  tick; the tick stays alive on `direct_probe_pending`) launches
  the capture when the result lands: a verified file retargets
  the capture at it with host-filtered headers, no file means
  the playlist URL as before, and a capture cancelled while
  probing ends unlaunched.
- `finish_staged_download`: truncation expectation is now the
  padded window minus padding seconds (floored at 1), ratio
  0.95. The written-seconds source is an ffprobe of the
  produced file rather than Python's ffmpeg-log parse; the log
  route arrives with N12 (recording outcome reports), which
  needs the same capture.

## Verification

- 7 new core tests over a scripted transport: candidate
  layouts, credential stripping cross-host (and preservation
  same-host with a spelled-out default port), the media-type
  gate, redirect joins, the walk probing around a redirect
  target without opening it, 403 earning no second touch, and
  the 405→ranged-GET fallback.
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (337 tests, 0 failed), playlist
  parity against the pinned Python oracle, `git diff --check`.

## Not verified / residual

- No live one-connection provider was available to this build
  machine, so the probe is verified against the scripted
  transport and Python's source semantics, not a real account.
  Brandon's real-provider use is the live check.
- When a recording audio intent exists, Rust probes audio
  labels inside `launch_staged_capture` after the probe-phase
  settle; Python settles after its audio probe. The common
  path (no audio intent) probes nothing before ffmpeg, so the
  exposure is limited to the preferred-track path.
- The other half of N13 — sharing the playing channel's
  connection with a recording (`recording_relay.py`) and the
  one-stream busy refusal (`_single_stream_provider_busy`) —
  is not in this batch and remains open in the audit.
