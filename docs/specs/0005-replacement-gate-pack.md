# Replacement-gate pack

## Problem Statement

The Rust port has broad automated coverage, but several important boundaries
remain represented only by source review, unit tests, or validation notes. The
project cannot claim a replacement-ready v0.2.0 gate until it has repeatable
evidence for owner-heard accessibility, live playback and recording seams,
renderer/casting boundaries, and packaging/update behavior.

The pack must close evidence gaps without silently expanding into every Python
feature that is explicitly not ported. Python remains the behavior oracle and
the shipping product until the owner authorizes a swap.

## Solution

Create a four-part replacement-gate pack using the existing validation-document
and crate/app-test seams:

1. Define a repeatable NVDA and keyboard acceptance checklist, with owner-heard
   results recorded separately from automated evidence.
2. Add sanitized local fixtures and focused checks for playback, libVLC
   fallback/absence, ffmpeg command and recording lifecycle behavior, and the
   Stalker DVR resolution path.
3. Define protocol-level casting evidence and a separate physical-renderer
   gate, without treating parser/builder tests as proof of audible or physical
   device behavior.
4. Validate the documented packaging and update layout in a test VM, while
   leaving workflow changes and real release publication outside this spec.

## User Stories

1. As a blind owner, I want a repeatable NVDA and keyboard checklist, so that I
   can judge whether the Rust app is usable without relying on memory.
2. As a blind owner, I want focus, names, announcements, and recovery steps
   stated for each acceptance path, so that failures are actionable.
3. As a maintainer, I want owner-heard results recorded separately from source
   and test results, so that automated evidence is not overstated.
4. As a maintainer, I want sanitized fixtures for provider, playlist, EPG, and
   media-boundary cases, so that tests do not require credentials or private
   URLs.
5. As a maintainer, I want playback checks to cover the libVLC-present and
   libVLC-absent paths, so that fallback behavior remains safe.
6. As a maintainer, I want fixture-backed recording checks to cover start,
   stop, failure, and DVR resolution outcomes, so that live recording behavior
   is not inferred solely from command construction.
7. As a maintainer, I want late, stale, and canceled provider results covered,
   so that asynchronous DVR resolution cannot start the wrong recording.
8. As a maintainer, I want protocol-level casting checks to remain distinct from
   physical-renderer checks, so that wire-format confidence is not mistaken for
   device acceptance.
9. As a blind owner, I want a physical renderer path identified where one is
   available, so that cast discovery, control, and spoken status can be judged
   in practice.
10. As a maintainer, I want packaging and update-manifest checks to run in a
    controlled Windows VM, so that release layout regressions are caught before
    publication.
11. As a maintainer, I want missing external dependencies to produce explicit
    skipped or blocked evidence, so that absence of VLC, ffmpeg, or a renderer
    cannot be reported as a pass.
12. As a release owner, I want the pack to state which gates are automated,
    owner-heard, environment-dependent, or still open, so that a v0.2.0
    decision has a trustworthy evidence ledger.

## Implementation Decisions

- The primary seam is the existing validation-document layer plus focused tests
  at the highest existing core/app seams. No new cross-cutting runtime API is
  required solely for this pack.
- Sanitized local fixtures are the default test data. Credentials, private
  portal URLs, and secrets must not be committed, logged, or transmitted.
- The Windows test VM is the environment for repeatable VLC/libVLC, ffmpeg, and
  packaging checks. The owner’s setup is the authority for NVDA and physical
  accessibility acceptance.
- Every gate records status as pass, fail, blocked, or not-run, with the
  environment and evidence artifact named.
- Automated tests may support playback, recording, protocol, and packaging
  claims, but cannot prove audible screen-reader output, physical keyboard/D-pad
  behavior, or real renderer playback.
- Existing Stalker DVR request identity and stale-result protections remain the
  contract for asynchronous provider resolution.
- The pack must preserve the current fallback behavior when libVLC is absent
  and must not make startup depend on optional native media libraries.
- Workflow edits, release publication, installer signing, and an application
  swap remain outside this spec and require separate authorization.

## Testing Decisions

- Tests assert externally observable behavior at existing core and app seams,
  not private implementation details.
- Fixture-backed tests cover deterministic provider responses, media metadata,
  recording lifecycle outcomes, stale/canceled DVR resolution, cast protocol
  messages, manifest selection, and packaging layout.
- The existing workspace format, check, clippy, test, and playlist-parity gates
  remain required regression gates.
- The owner-heard checklist covers NVDA names, focus order, announcements,
  keyboard operation, playback controls, recording status, and recovery from
  failed or unavailable optional dependencies.
- Live VM evidence covers installed-libVLC playback, ffmpeg recording against a
  sanitized source, and package/update validation where the required tools are
  available.
- Physical-renderer evidence is recorded independently from protocol tests and
  is allowed to remain blocked when no compatible renderer is available.

## Out of Scope

- Porting Chromecast, AirPlay/RAOP, Sonos native control, Yamaha side-channel
  control, or other casting protocols already documented as unported.
- Porting the full Python installer, Authenticode trust workflow, or a release
  workflow in `.github/workflows`.
- Adding DVR recording-log statistics, storage-reserve policy, live-buffer
  recording relay, series-link scanning, or an EPG viewer.
- Replacing Python as the shipping product or publishing a v0.2.0 release.
- Using private credentials or provider accounts as committed test fixtures.

## Further Notes

The result should be decomposable into dependency-ordered GitHub tickets after
this spec is published. The ticket graph should keep owner-heard validation,
fixture/test work, live-VM work, and release-readiness evidence reviewable as
separate slices.
