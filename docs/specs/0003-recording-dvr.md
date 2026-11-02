# Rust Recording, DVR, and Shutdown Parity

## Problem Statement

The Rust port can browse channels and resolve playback streams, but it cannot
yet record a selected channel or run scheduled recordings. The wxPython product
has a mature recording path whose observable behavior includes format-specific
FFmpeg commands, safe names and output paths, header-aware stream input,
graceful stop/finalization, durable DVR jobs, restart recovery, recurring
schedules, and guarded shutdown after recordings.

Recording is also an accessibility boundary. A blind keyboard user must be able
to start or stop a recording from the selected channel, hear completion or
failure once, understand scheduled-job state, and cancel a pending shutdown.
Background FFmpeg and DVR work must never steal focus or expose credentials and
raw stream URLs in spoken status.

## Solution

Add a wx-free recording/DVR seam that turns a channel, recording configuration,
resolved stream information, and clock/process events into deterministic
recording plans, job transitions, persistence updates, and shutdown decisions.
The desktop adapter owns wxDragon controls, process handles, ten-second polling,
file dialogs, and live-region announcements.

The Python implementation remains the behavioral oracle. The Rust behavior must
match the observable contracts in `recorder.py`, `client_recording.py`,
`power.py`, the scheduled-recording persistence code, and their focused tests,
while preserving the existing Rust boundary that keeps domain logic independent
of wxDragon.

## User Stories

1. As a blind IPTV user, I want a clearly named Record action for the selected channel, so that I can start recording without a mouse.
2. As a keyboard user, I want the Record action to operate on the selected or focused Channels row, so that focus and selection behavior remain predictable.
3. As a user, I want recording to refuse an invalid or unavailable channel safely, so that no empty or misleading FFmpeg process is created.
4. As a user, I want the active recording state exposed in an accessible status, so that I know whether recording is starting, active, stopping, completed, or failed.
5. As a user, I want a second activation to stop the matching recording using FFmpeg stdin `q`, so that the output can finalize cleanly.
6. As a user, I want stopping one recording not to stop another channel's recording, so that independent captures remain safe.
7. As a user, I want a completion announcement with the privacy-safe output name, so that I know the recording is ready without hearing stream credentials.
8. As a user, I want a failure announcement with recovery guidance, so that I can retry or choose another channel.
9. As a user, I want provider-quality video copied into MKV or MP4 according to my chosen format, so that the output matches the reference format contract.
10. As a user, I want provider-quality audio copied into MKA, so that audio-only provider streams do not receive video-only assumptions.
11. As a user, I want x264 video re-encoding available in MKV and MP4, so that incompatible source codecs can be recorded into a broadly playable format.
12. As a user, I want audio-only WAV, FLAC, MP3 V0, AAC/M4A, and Opus choices, so that recordings fit different listening and storage needs.
13. As a user, I want each format's codec, mapping, muxer, and extension to be deterministic, so that a saved format choice produces the same FFmpeg contract every time.
14. As a user, I want MP4 faststart behavior preserved where the reference uses it, so that completed files begin playback efficiently.
15. As a user, I want HTTP headers from channel metadata and stream URL modifiers passed to FFmpeg, so that protected streams record successfully.
16. As a user, I want RTSP recording to use the reference transport option, so that RTSP providers receive the expected connection behavior.
17. As a user, I want stream dispositions and audio/video mapping preserved, so that the chosen recording contains the expected media streams.
18. As a user, I want recording paths sanitized for Windows and other supported platforms, so that channel names cannot create invalid or unsafe filenames.
19. As a user, I want recordings written to a temporary `.part` path until FFmpeg completes, so that interrupted captures are not mistaken for finished recordings.
20. As a user, I want a timestamped filename with collision suffixes, so that a new recording never silently overwrites an earlier one.
21. As a user, I want FFmpeg stderr retained in a diagnostic log, so that failures can be investigated without making raw URLs part of spoken UI text.
22. As a user, I want MP4 finalization to receive enough time to write its index, so that stopping a long recording does not leave an unplayable file.
23. As a user, I want a wedged FFmpeg process escalated only after the clean-stop grace period, so that data-safe finalization is preferred over abrupt termination.
24. As a user, I want to schedule a channel recording with start time, duration, and pre/post padding, so that the capture includes the intended programme window.
25. As a user, I want a scheduled recording to have an explicit status, so that pending, recording, stopping, finished, missed, failed, and cancelled jobs are distinguishable.
26. As a user, I want scheduled jobs persisted in a versioned JSON store, so that closing and reopening the application does not lose them.
27. As a user, I want jobs that were recording or stopping at shutdown to re-arm safely on restart, so that a crash or restart does not leave a permanently dead schedule.
28. As a user, I want a missed job handled according to the Python rule, so that stale work does not start unexpectedly after its intended window.
29. As a user, I want daily and weekly recurring jobs to calculate their next occurrence deterministically, so that a finished job continues only when recurrence is configured.
30. As a user, I want a scheduled job that fails to report the failure and retain enough state for retry or inspection, so that the scheduler does not silently discard it.
31. As a user, I want the DVR scheduler to tick through the existing application timer every ten seconds, so that scheduling is responsive without blocking the UI thread.
32. As a user, I want the scheduler to avoid starting duplicate jobs when multiple ticks observe the same window, so that one programme creates one capture.
33. As a user, I want the scheduler to stop a job at its padded end time, so that recordings do not run indefinitely.
34. As a user, I want shutdown-after-recordings to arm only when explicitly enabled, so that normal exit behavior is unchanged by default.
35. As a user, I want shutdown-after-recordings to require that at least one recording was made and that no active or pending work remains, so that the computer never shuts down prematurely.
36. As a user, I want an explicit countdown or cancel path before shutdown, so that I can prevent shutdown when I need the computer to remain available.
37. As a user, I want platform shutdown commands selected safely, so that the adapter does not invoke destructive or unsupported commands on the wrong operating system.
38. As a user, I want recording completion and shutdown decisions to survive a failed persistence write without crashing the application, so that a storage problem remains recoverable.
39. As a privacy-conscious user, I want credentials, raw stream URLs, and HTTP header values excluded from row labels, announcements, normal status text, and durable job titles.
40. As a keyboard user, I want background recording completion, DVR ticks, and process exits to leave my current focus unchanged, so that NVDA navigation is not interrupted.
41. As a maintainer, I want recording plans, schedules, and shutdown policy tested without FFmpeg, wxWidgets, provider credentials, or live streams, so that regression tests are fast and deterministic.
42. As a maintainer, I want one core coordinator shared by manual recording and DVR scheduling, so that stop, finalization, persistence, and announcements cannot diverge between paths.
43. As a product owner, I want automated evidence and owner-heard NVDA/keyboard evidence recorded separately, so that passing tests is not mistaken for audible accessibility acceptance.

## Implementation Decisions

- Add wx-free recording and DVR modules to the core crate. They own format metadata, filename/path planning, FFmpeg argument construction, recording/job state transitions, scheduled-job persistence models, recurrence calculations, and shutdown eligibility.
- Keep process creation, stdin writes, polling, filesystem dialogs, native timers, and platform shutdown invocation in the application adapter. The core receives abstract process/file/clock outcomes and returns commands or state changes.
- Reuse the existing `Channel`, stream-header, configuration, and playback identity boundaries. Recording consumes the same normalized stream URL/header contract as playback and must not duplicate header parsing.
- Define a recording format table with stable keys, user-facing labels, extensions, media kind, codec/copy policy, muxer, stream mapping, faststart policy, and any format-specific FFmpeg options. Unknown format keys fall back safely to the configured default rather than constructing an arbitrary command.
- Build FFmpeg arguments as structured data. The command builder must include input URL, header arguments, reconnect policy, RTSP transport where applicable, media mapping/dispositions, codec options, muxer, and temporary output path in deterministic order.
- Keep sensitive values available to the process plan but out of display-oriented diagnostics. Tests must assert that accessible announcements and durable job labels do not contain credentials, headers, or raw URLs.
- Sanitize names using the Python-compatible invalid-character, reserved-name, whitespace, and length rules. Add a timestamp and collision suffix without overwriting an existing output or an existing `.part` path.
- Write to a `.part` path while recording. On clean completion, finalize or rename atomically to the intended output extension. On failure or cancellation, preserve only the diagnostic information and partial-file behavior specified by the Python reference.
- Model a recording session with explicit states such as Planned, Starting, Recording, Stopping, Finalizing, Finished, Failed, and Cancelled. Process exit status, stdin-stop acknowledgement, timeout escalation, and filesystem finalization must produce deterministic transitions.
- Prefer a graceful FFmpeg stop through stdin `q`. Use format-aware finalization grace, then terminate only when the process remains wedged. Never report success until the final output is in its completed state.
- Model a DVR job with a stable identifier, channel identity, display-safe title, start/end window, padding, recurrence, output settings, and status. The version-1 JSON store must preserve unknown fields where practical and reject malformed top-level shapes safely.
- Re-arm only recording/stopping jobs on startup according to the Python restart rule. Do not restart finished, cancelled, or missed jobs without a new scheduled occurrence.
- Implement scheduler tick rules as a pure function over current time, job state, active recordings, and configured recording directory. Start each eligible job once, stop jobs at padded end, mark stale jobs missed, and calculate the next daily/weekly occurrence after a successful recurring completion.
- Use the existing application poll timer at a ten-second cadence for DVR work. The timer must remain active while browser, EPG, provider, reminder, recording, or scheduled work is pending, and must stop when no background work remains.
- Centralize completion/failure events as UI-neutral events. The app adapter announces deliberate state changes once through the existing live region and updates a named status surface without moving focus.
- Add an accessible Record toggle/action with an explicit name and state description. It must operate from the selected or focused Channels row, preserve list focus, and expose a keyboard-reachable stop path.
- Implement shutdown eligibility as a pure safety predicate: shutdown-after-recordings enabled, at least one recording completed in the relevant run, no active recordings, no pending scheduled work, no queued finalization, and no explicit cancellation. The adapter owns the countdown and platform command.
- Make platform shutdown execution an injected adapter boundary. Unsupported platforms return a safe “not available” result; tests never invoke a real shutdown command.
- Keep series-link scanning and guide-wide recording discovery outside this slice. A schedule may use an already-resolved channel/programme window, while EPG integration remains the caller's responsibility.
- Record the pinned Python revision and fixture provenance for format tables, filename rules, FFmpeg arguments, DVR persistence, power predicates, and completion behavior. The Python application remains the shipping product until the replacement gate is separately satisfied.

## Testing Decisions

- Test the highest wx-free recording/DVR seam: given a channel, configuration, fake clock, fake filesystem, fake process result, and job store, assert the resulting FFmpeg plan, output path, state transition, persisted job, completion event, or shutdown decision.
- Add format tests for all supported keys, extension/muxer mapping, copy and re-encode codecs, audio-only mapping, faststart, dispositions, default fallback, and deterministic argument ordering.
- Add header/input tests for User-Agent, Referer, Cookie, Origin, Authorization, extra headers, pipe modifiers, reconnect flags, RTSP transport, and credential-safe display diagnostics.
- Add filename tests for invalid characters, reserved names, empty/fallback names, Unicode, length bounds, timestamp format, existing completed files, existing `.part` files, and collision suffixes.
- Add recording-engine tests with fake process handles for start, stdin `q`, clean exit, nonzero exit, missing output, `.part` handling, finalization grace, terminate escalation, and repeated polling after completion.
- Add DVR-store tests for version-1 serialization, malformed/corrupt store recovery, unknown-field preservation, restart re-arm, status transitions, duplicate-tick suppression, and recurrence normalization.
- Add scheduler tests with a fake clock for pre-padding start, padded end, missed jobs, daily recurrence, weekly recurrence, timezone/UTC boundary decisions defined by the reference, simultaneous jobs, and a newer job superseding no existing recording.
- Add shutdown tests for every safety predicate: disabled flag, no completed recordings, active recording, pending job, finalizing process, cancelled countdown, unsupported platform, and the one safe eligible case.
- Add an application-seam test that activates Record from the selected Channels row and asserts one core command, unchanged keyboard focus, an explicit status event, and a safe stop/retry path.
- Add adapter/native smoke coverage for the Record action, recording status, scheduled-recording controls if exposed in this slice, and the shutdown-cancel control. Native MSAA/UIA evidence supports names and roles but does not prove NVDA speech.
- Reuse the Python recorder, DVR, power, and accessibility tests as fixture and behavior sources. Keep all process paths, URLs, headers, and output directories synthetic in Rust tests.
- Required automated gates are `cargo fmt --all -- --check`, focused recording/DVR tests, `cargo test --workspace`, `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `git diff --check`.
- Record owner-heard NVDA and physical keyboard validation separately. The manual gate must cover Record/Stop, completion/failure announcements, scheduled-job navigation, shutdown cancellation, focus stability during background ticks, and large-text/high-contrast presentation.

## Out of Scope

- Series-link scanning, guide-wide recording discovery, recording log summaries/statistics, and a recording-history browser.
- New provider authentication, playlist loading, EPG import/matching, catch-up resolution, playback engine changes, casting, subtitles, or audio-track selection except where an existing shared stream/header seam is required.
- Real FFmpeg/provider integration tests, live credentials, real platform shutdown, release packaging of FFmpeg, and deployment changes.
- A new configuration format or destructive migration of Python user data.
- Automatic shutdown without the explicit configured safety predicate and cancel path.
- Replacing the Python shipping product or closing the Rust replacement gate.

## Further Notes

The primary seam is one wx-free recording/DVR coordinator shared by manual and
scheduled recording. The adapter should translate its commands into FFmpeg
process operations, ten-second timer ticks, accessible state changes, and a
platform-specific shutdown request without reimplementing policy.

The strongest Python references are `recorder.py`, `client_recording.py`,
`power.py`, the scheduled-recording persistence and scheduler code, and focused
tests for recorder formats, recording outcomes, DVR scheduling, power safety,
recording accessibility controls, modal completion reporting, and shutdown
guards. The relevant Rust constraints are `agents.md`, the workspace boundary
ADR, the playback stream/header seam, and the existing config persistence
contract.

Split this spec into dependency-aware tickets: format/path/FFmpeg planning;
recording process lifecycle; DVR store and scheduler; shutdown safety; app
Record control and timer integration; then the separate owner-heard
accessibility gate.
