# Stalker DVR Start-Time Resolution

## Problem Statement

Scheduled recordings for Stalker Portal channels currently reach their due
window with no usable stream URL. The channel snapshot contains the portal
command metadata, but the temporary stream link is only created when the
ProviderWorker resolves it. The DVR path currently marks such jobs failed
instead of resolving the link at start time. This makes scheduled recording
unreliable for a supported provider and can miss the programme window.

The Rust port must preserve the Python reference's provider and recording
boundaries: credentials and portal tokens remain inside the provider client,
temporary links are not durable job data, and background work must not block
the wxDragon UI or move keyboard focus.

## Solution

When a Stalker DVR job becomes due, the shared wx-free DVR/recording coordinator
requests an asynchronous stream resolution before starting FFmpeg. The desktop
adapter correlates the provider result to the requesting job and feeds the
outcome back to the coordinator. A successful result starts the existing
recording engine with the resolved URL. A transient failure leaves the job
pending and retries on the existing ten-second DVR tick while the padded window
is still open. Once the window closes, the job becomes failed with a
display-safe reason.

Provider readiness after application startup follows the same pending/retry
path. The temporary resolved URL remains memory-only and is never written to
the version-1 scheduled-recording JSON store.

## User Stories

1. As a user, I want a scheduled Stalker recording to resolve its stream when it is due, so that temporary portal links are fresh when FFmpeg starts.
2. As a user, I want the portal request to run in the background, so that the application remains responsive while the portal answers.
3. As a user, I want FFmpeg not to start until a usable stream link exists, so that a missing portal link cannot create a misleading recording process.
4. As a user, I want a temporary portal failure to retry during the padded window, so that a short outage does not automatically lose the recording.
5. As a user, I want retry timing to use the existing ten-second DVR cadence, so that scheduler behavior remains predictable.
6. As a user, I want a job whose provider account is still loading at startup to remain pending, so that normal startup ordering does not mark it failed.
7. As a user, I want a job to fail clearly when its padded window closes without a resolved link, so that the schedule does not remain stuck forever.
8. As a user, I want the failure reason to be safe and useful, so that it explains recovery without exposing credentials, cookies, headers, or raw portal URLs.
9. As a user, I want a successful resolved link to use the same recording format, output naming, headers, and stop behavior as a manually started recording, so that scheduled and manual captures behave consistently.
10. As a user, I want one scheduled job's portal result to update only that job, so that concurrent manual playback or DVR requests cannot start the wrong channel.
11. As a user, I want background resolution and recording transitions to leave my current keyboard focus unchanged, so that NVDA navigation is not interrupted.
12. As a user, I want the schedule to retain its durable job metadata across restart, so that the application can retry using the provider account and channel command without persisting an expiring link.
13. As a privacy-conscious user, I want temporary stream URLs and provider credentials kept out of the scheduled-recording JSON store, accessible status text, and durable job titles.
14. As a maintainer, I want the resolution/retry state machine tested without a live portal or FFmpeg, so that transient and permanent failures are deterministic.
15. As a maintainer, I want the existing ProviderWorker and recording engine boundaries reused, so that portal authentication and recording policy are not duplicated.
16. As the product owner, I want automated evidence and owner-heard NVDA/keyboard evidence kept separate, so that passing provider-worker tests is not mistaken for hands-on accessibility acceptance.

## Implementation Decisions

- Keep one highest-level wx-free DVR/recording coordinator responsible for job
  state, provider-resolution requests, retry eligibility, start decisions, and
  safe failure outcomes. It receives abstract provider-resolution results and
  emits commands/effects for the application adapter.
- Extend provider work with an opaque request identifier carried through the
  provider job and result event. The identifier must distinguish manual
  playback resolution from one or more DVR resolutions; a single shared
  pending slot is insufficient.
- Reuse the existing ProviderWorker and StalkerClient. The provider worker
  remains the only owner of portal tokens, credentials, cookies, and network
  I/O. The coordinator receives only success/failure outcomes and the resolved
  stream value needed for the in-memory recording plan.
- A due Stalker job enters an explicit resolving/pending state rather than
  being marked recording before a link exists. The exact persisted status must
  remain compatible with the version-1 store; transient request ownership may
  be runtime-only state.
- Provider readiness and transient resolve failures use the existing ten-second
  DVR tick. The coordinator may issue another request only when the prior
  request has completed or been abandoned according to the current tick; it
  must not create duplicate in-flight requests for one job.
- Retry remains eligible only while the job's padded recording window is open.
  At or after the padded end, the coordinator records one durable failed result
  with a display-safe reason and no recording process is started.
- A successful resolve creates an in-memory channel/stream input for the
  existing recording engine. It reuses the existing format, headers, output
  path, `.part` handling, completion, and shutdown policy.
- The resolved temporary URL is never serialized into the scheduled-recording
  store. Restart recovery reuses the persisted provider ID and channel command
  metadata, then performs a fresh resolution.
- Provider results are applied only when the request ID still maps to the same
  job and expected state. Late results for canceled, missed, failed, or already
  started jobs are ignored safely.
- Accessible status events remain UI-neutral and display-safe. They may state
  that a scheduled recording is resolving, retrying, saved, or failed, but must
  not include raw stream URLs, portal endpoints, cookies, authorization data,
  or other sensitive headers.
- The wxDragon adapter owns timer polling, provider-worker dispatch, FFmpeg
  process operations, live-region/status updates, and focus preservation. Core
  policy remains independent of wxDragon and native process handles.
- Existing non-Stalker channels keep their current direct recording path. This
  slice changes the unsupported Stalker scheduled-recording path only.
- The Python implementation and its pinned fixtures remain the behavioral
  oracle. Any new observable rule must record the Python revision or fixture
  provenance used to establish it.

## Testing Decisions

- Test the coordinator at its highest wx-free seam with synthetic jobs, clocks,
  provider outcomes, and recording effects. Assert external state transitions
  and emitted effects rather than private fields.
- Cover a due Stalker job waiting for provider readiness, successful async
  resolution, transient failure followed by retry, duplicate-tick suppression,
  window expiry, cancellation before a result, and a late result after the job
  has moved on.
- Cover request-ID isolation between manual playback resolution and multiple
  DVR jobs, including results arriving out of order.
- Cover restart recovery: persisted Stalker command/provider metadata is enough
  to request a fresh link, while a prior resolved URL is absent from the JSON
  store and never reused.
- Cover privacy boundaries for status text, failure messages, durable job
  titles, and serialized store output using synthetic credentials, URLs,
  cookies, and headers.
- Reuse the existing ProviderWorker tests with a fake Stalker response and
  assert request/result correlation without making a live portal request.
- Reuse the existing recording-engine tests to prove that a successful DVR
  resolve feeds the same deterministic FFmpeg plan and completion behavior as a
  manual recording.
- Add adapter-level tests for timer-driven dispatch, no focus movement, and
  safe handling of late provider events. Native MSAA/UIA evidence supports
  names and roles but does not prove NVDA speech.
- Required automated gates remain `cargo fmt --all -- --check`, focused
  provider/DVR/recording tests, `cargo test --workspace`, `cargo check
  --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `git diff --check`.
- Record owner-heard NVDA and physical keyboard validation separately. The
  manual gate covers a scheduled Stalker resolve, retry/failure announcement,
  focus stability during the ten-second tick, and shutdown behavior after the
  resolved recording completes.

## Out of Scope

- New Stalker authentication, portal account management, MAC registration, or
  token protocol changes.
- Persisting temporary stream URLs, credentials, cookies, headers, or portal
  tokens.
- Changes to Xtream resolution, manual playback behavior, catch-up resolution,
  FFmpeg format policy, output naming, or the recording engine except where a
  shared request/result seam is required.
- A new scheduler cadence, background timer system, or UI toolkit.
- Series-link discovery, EPG viewer scheduling UI, recording history, storage
  reserve policy, live-buffer relay, or real-provider/real-FFmpeg integration
  tests.
- Deployment, release packaging, issue closure, merge, or pull-request
  creation.

## Further Notes

The current validation record identifies this exact gap: scheduled Stalker
recordings fail because portal resolution is not wired into the DVR path. The
existing provider worker already resolves a Stalker channel asynchronously for
playback, while the DVR path currently rejects an empty channel URL. This spec
connects those seams without persisting the expiring result or moving provider
credentials into the scheduler.
