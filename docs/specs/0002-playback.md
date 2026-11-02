# Rust Playback Parity: Internal and External Player Seam

## Problem Statement

The Rust port can browse and search channels, but activating a channel does not
yet provide the playback behavior users rely on in the wxPython product. A
playback implementation that only opens a URL would lose important behavior:
per-channel HTTP headers and URL modifiers, external-player command-line
contracts, duplicate-launch suppression, stale-request cancellation, live
buffering and reconnect rules, and resume checkpoint compatibility.

For a blind keyboard user, playback is also an accessibility boundary. Enter
must play the selected channel without losing the browse context, failures must
be announced without exposing credentials or raw stream URLs, and background
player work must never steal focus or announce stale state.

## Solution

Add a wx-free playback core seam that converts a selected `Channel` and saved
configuration into deterministic playback commands, state transitions, and
resume decisions. The desktop adapter consumes those commands and owns native
wxDragon controls: the built-in media control, external process handles, focus,
and live-region status announcements.

The Python implementation remains the behavioral oracle. The Rust seam must
match the observable rules from `http_headers.py`, `external_player.py`,
`internal_player.py`, `playback_resume.py`, and the playback entry points in
`main.py`, while retaining the Rust workspace boundary that keeps domain logic
independent of wxDragon.

## User Stories

1. As a blind IPTV user, I want Enter on a selected channel to start playback, so that I can play content without using a mouse.
2. As a keyboard user, I want the selected channel and list focus preserved while playback starts, so that I can return to browsing predictably.
3. As a user, I want stream URL pipe modifiers converted into HTTP headers, so that providers requiring User-Agent, Referer, Cookie, Origin, Authorization, or other headers work as in Python.
4. As a user, I want channel metadata headers merged with URL modifiers deterministically, so that explicit stream modifiers and channel fields have the same precedence as the reference.
5. As a user, I want header values applied to internal playback and passed to external-player launches where supported, so that the selected stream is requested consistently.
6. As a privacy-conscious user, I want headers, credentials, and stream URLs kept out of accessible row labels and ordinary status text, so that sensitive data is not spoken accidentally.
7. As a user who chooses VLC, I want the Rust client to construct the Python-compatible one-instance VLC command, so that an existing VLC process is reused as expected.
8. As a user who chooses MPV, I want the Rust client to construct the required MPV IPC and playback arguments, so that MPV integration and later control remain available.
9. As a user who chooses MPC, I want the Rust client to use the plain MPC launch contract, so that playback starts without MPV/VLC-only flags.
10. As a user, I want a custom external player command to receive the stream in the documented position, so that configured players remain usable.
11. As a user, I want repeated activation of the same stream within 0.75 seconds to be debounced, so that one keyboard action cannot launch duplicate players.
12. As a user, I want a deliberate launch of a different stream to supersede an older pending launch, so that stale playback cannot replace my newer choice.
13. As a user, I want a failed external-player launch to be retryable immediately, so that a transient process failure does not create an invisible cooldown.
14. As a live-stream user, I want playback to begin with a half-second cache, so that first audio is not delayed by the full recovery buffer.
15. As a live-stream user, I want the cache to fill at the source's natural playback rate, so that speech is never slowed to grow a client-side cushion.
16. As a user, I want saved buffer durations preserved and missing values defaulted, so that configuration choices survive the Rust transition.
17. As a user, I want legacy saved maximum-buffer values migrated according to the Python compatibility rule, so that an old 18-second value does not cap the new 30-second default.
18. As a Dispatcharr user, I want the client to recognize the playlist marker and avoid increasing the client cache unnecessarily, so that the server-side buffer remains authoritative.
19. As a user, I want normal live segment rollovers to reopen with the half-second opening cache, so that ordinary rollovers do not feel like full stall recovery.
20. As a user, I want genuine stalls to reconnect with backed-off timing and gradual cache increases, so that repeated outages do not create a tight reconnect loop.
21. As a user, I want a newer playback request or stop action to invalidate pending reconnect work, so that old streams cannot restart after I moved on.
22. As a user, I want the playback session state to distinguish opening, playing, paused, buffering, stopped, ended, failed, and reconnecting, so that status and recovery behavior are understandable.
23. As a VOD or catch-up user, I want resume checkpoints keyed by stable content identity rather than a changing stream URL, so that token rotation does not lose progress.
24. As a VOD or catch-up user, I want positions below 30 seconds ignored, so that trivial progress does not create resume prompts.
25. As a VOD or catch-up user, I want positions in the configured end window removed, so that completed content does not ask to resume.
26. As a user, I want resume metadata bounded and privacy-safe, so that credentials, raw URLs, and unbounded titles do not enter persistent state.
27. As a user, I want an internal-player failure to leave the browse surface usable, so that I can retry or choose another channel.
28. As a user, I want external-player and internal-player failures announced once through the status live region, so that NVDA does not repeat or interrupt itself with duplicate messages.
29. As a keyboard user, I want Tab, Shift+Tab, Escape, and normal list navigation to remain predictable while playback is opening or reconnecting, so that asynchronous work never steals focus.
30. As a maintainer, I want playback decisions testable without wxWidgets, VLC, MPV, MPC, network access, or a real stream, so that parity regressions are fast and deterministic.
31. As a maintainer, I want the app adapter to translate core commands into native playback actions without duplicating playback policy, so that core and desktop behavior cannot drift.
32. As a product owner, I want automated playback evidence and owner-heard NVDA/keyboard evidence recorded separately, so that passing unit tests is not mistaken for audible accessibility acceptance.

## Implementation Decisions

- Add a wx-free playback module in the core crate. It owns stream normalization, header merging, external-player argument construction, launch debounce, request generations, session state, buffer policy, reconnect decisions, and resume-key/checkpoint rules.
- Keep the existing `Channel`, `Config`, and `BrowserWorker` boundaries. Playback receives an immutable selected-channel snapshot and configuration values; it does not reach into wxDragon or mutate browse state.
- Represent playback policy as explicit data and commands rather than callbacks tied to widgets. The core should be able to answer “what should happen next?” for a deterministic input and clock/event sequence.
- Normalize a stream URL by splitting the base URL from pipe modifiers, canonicalizing recognized header names, preserving supported extra headers, and merging channel-provided headers using the Python precedence rules. Credentials must never be used for display text or stable resume identity.
- Provide deterministic argv builders for VLC, MPV, MPC, and custom external players. The builders return structured launch plans containing executable, arguments, environment/IPC intent where required, and privacy-safe diagnostics; process creation remains in the app crate.
- Apply the Python 0.75-second same-stream launch debounce at the launch coordinator boundary. A different stream or a newer generation supersedes a pending request. A failed spawn does not poison future retries.
- Model a playback request with a monotonically increasing generation. Every asynchronous completion, reconnect timer, and process result must be ignored when its generation is no longer current.
- Define the playback state machine around observable states: Idle, Opening, Playing, Paused, Buffering, Reconnecting, Ended, Stopped, and Failed. Invalid or stale transitions are ignored or converted to a safe failure without changing focus.
- Keep the opening cache separate from the saved recovery buffer. Live playback starts with a 0.5-second opening cache; normal playback never changes media playback rate; genuine recovery may increase cache gradually up to the normalized maximum.
- Preserve the `agents.md` playback law: saved buffer values are explicit choices, missing values use defaults, the default maximum is 30 seconds, the legacy 18-second maximum migrates to 30, Dispatcharr leaves the saved cache alone, and segment rollovers do not use full stall recovery.
- Make reconnect scheduling a pure policy decision with bounded exponential/backed-off delays and a generation check. The wxDragon adapter owns timers and invokes the next command only if the session is still current.
- Implement resume identity and checkpoint normalization against the Python `playback_resume.py` rules. Use provider/content identity and stable metadata before falling back to a safe legacy identity; never key on resolved stream URL. Ignore positions under 30 seconds, delete entries in the end window, and bound persisted metadata.
- Add an app-level playback coordinator that connects the browse selection to the core playback plan. Enter on Channels submits a play request; the adapter chooses internal MediaCtrl or external process execution according to configuration.
- Keep native playback controls and status text explicitly named. Playback status uses the existing live-region path for deliberate state changes, while background acknowledgements are silent or deduplicated. No playback worker may call `set_focus` on a browse control.
- Keep wxDragon MediaCtrl behind the app adapter. The core must not depend on libVLC, wx MediaCtrl types, process handles, or platform-specific window APIs. MediaCtrl capability gaps are reported as UI-neutral playback failures.
- Treat network access and real external executables as adapter/manual boundaries. Core tests use fake clocks, fake process plans, fake media events, and fixture channels with non-sensitive example URLs.
- Record the Python revision and fixture provenance used for header, argv, buffer, reconnect, and resume parity. The Python app remains the shipping product until the replacement gate is separately satisfied.

## Testing Decisions

- Test the highest wx-free playback seam: given a channel, configuration, clock, and playback event sequence, assert the resulting normalized request, launch plan, state transition, status event, reconnect decision, or checkpoint mutation. Do not assert private helper layout.
- Add header tests derived from `http_headers.py` for canonical names, URL pipe splitting, authorization normalization, extra-header preservation, channel/header precedence, empty values, and credential-safe diagnostics.
- Add external-player tests derived from `external_player.py` for VLC one-instance arguments, MPV IPC flags, MPC plain arguments, custom commands, same-stream debounce at 0.749/0.750/0.751 seconds, different-stream supersede, stale generations, and immediate retry after failed spawn.
- Add state-machine tests for opening/playing/paused/buffering/reconnecting/ended/stopped/failed transitions, invalid events, duplicate events, stop during opening, and a newer play request during reconnect.
- Add buffer-law tests for 0.5-second live startup, full-rate filling with no playback-rate mutation, saved 2/4/12-second choices, default 25/30-second values, legacy 18-second migration, maximum clamping, Dispatcharr behavior, rollover behavior, and gradual recovery.
- Add reconnect tests with a fake clock that assert bounded backoff, generation cancellation, no restart after manual stop, and no stale restart after selecting another channel.
- Add resume tests ported from `playback_resume.py` for stable identity, URL/token rotation, provider/content identity, 30-second floor, end-window deletion, metadata bounds, legacy fallback, and normalization idempotence.
- Add an application-seam test that feeds a browse selection through Enter and asserts one playback command, unchanged browse focus, one deliberate status event, and safe recovery on a failed plan.
- Add adapter-level tests or a native smoke extension for explicit names and keyboard reachability of the playback status/control surface. Native smoke supports MSAA/UIA claims but does not replace owner-heard NVDA validation.
- Reuse the repository's existing Rust fixture style and pinned Python parity fixtures. Keep all stream URLs example-only and assert that credentials never appear in row labels, announcements, diagnostics, argv logs, or resume metadata.
- Required automated gates for the implementation are `cargo fmt --all -- --check`, focused core/app playback tests, `cargo test --workspace`, `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `git diff --check`.
- Report owner-heard NVDA and physical keyboard checks separately. The manual gate must cover Enter-to-play, focus retention, state announcements, cancellation, retry/reconnect messaging, and no duplicate speech. Automated playback tests do not close that gate.

## Out of Scope

- Porting recording/DVR, casting, EPG, VOD browsing, provider authentication, or the full libVLC feature surface beyond the playback seam needed by this slice.
- Real network integration tests, live provider credentials, real VLC/MPV/MPC process launches, and release packaging of native player runtimes.
- A new configuration format or destructive migration of existing Python user data.
- Background playback that changes focus, automatic selection movement, or screen-reader speech based solely on visual media events.
- Replacing the Python shipping product or closing the replacement gate.
- Remote playlist loading, catch-up URL construction, subtitles, audio-track selection, recording relay, and casting-specific buffering unless a narrow compatibility seam is required by the playback contract.

## Further Notes

The primary seam is the wx-free playback coordinator, with one thin wxDragon
adapter seam above it. This avoids duplicating playback policy between internal
MediaCtrl and external process paths while still allowing native accessibility
and platform integration to remain in the app crate.

The strongest Python references are `http_headers.py`, `external_player.py`,
`internal_player.py`, `playback_resume.py`, the playback entry points in
`main.py`, and their focused tests including `test_internal_player.py`,
`test_live_cushion.py`, `test_playback_resume.py`, and the external-player/MPV
tests. The relevant Rust constraints are `agents.md`, ADR 0002, ADR 0003, and
the existing browse/search/config seams.

This spec is intended to be split into dependency-aware implementation tickets:
header and stream normalization; external-player plans and launch generation;
session/buffer/reconnect policy; resume compatibility; app playback adapter and
Enter flow; then the separate owner-heard accessibility gate.
