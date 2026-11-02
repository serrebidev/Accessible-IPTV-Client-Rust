# 0032 — Stalker DVR correlation and retry hardening (#67–#70)

Date: 2026-11-01. Branch: agent/stalker-dvr-hardening.

## What shipped

Farhan's spec (#67) hardened in three parts:

- **#68 Correlation.** Provider worker Resolve / ResolveCatchup jobs
  and the Resolved event now carry an opaque `u64` request id end to
  end. The adapter keeps a pending map keyed by that id
  (`PendingResolve::{Play, Dvr, Download}`); a result only ever
  reaches the entry that dispatched it, and unknown or late results
  are dropped silently. Manual playback, DVR recording and catch-up
  downloads can be in flight simultaneously without consuming each
  other's results. Core test `resolution_results_carry_their_request_id`
  proves ids round-trip in dispatch order with no live portal.
- **#69 Fresh links.** A due Stalker DVR job dispatches a correlated
  resolution instead of being rejected; ffmpeg starts only from
  `record_dvr_channel` after a correlated success, under the
  display-safe identity `dvr:<job id>`.
- **#70 Retries and recovery.** A failed resolution keeps the job
  pending while its padded window is open and retries at most once
  per 10 s DVR tick (`retry_dvr_resolves`); at most one in-flight
  request per job (duplicate suppression in the start path and the
  retry state). Window expiry fails the job durably with the
  display-safe reason "Could not resolve the stream link in time.";
  a link that arrives after the window closed is never recorded.
  Cancellation drops the job's retry state and pending entries, so
  late results are unknown and ignored. Retry state is memory-only:
  resolved URLs, credentials and headers never enter durable job
  data or status text. Restart therefore cannot resurrect a
  resolution; persisted jobs follow the normal schedule lifecycle.
- Bonus closure in the same system: Stalker catch-up **downloads**
  (previously "not supported yet") now resolve through the same
  correlated path (`PendingResolve::Download`).

## Automated evidence

- `cargo fmt --all --check` clean, `cargo clippy --workspace
  --all-targets` zero warnings, `cargo test --workspace` all passing
  (including the new correlation test), playlist parity PASS,
  `git diff --check` clean.

## Manual evidence still owed (owner, on his setup)

- End-to-end Stalker DVR: schedule a recording on a portal channel,
  hear it start from a fresh link; kill the portal connection once
  and confirm the retry message and eventual start; NVDA pass over
  the Scheduled Recordings dialog while a job is resolving.
