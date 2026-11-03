# 0059 — Resume waiting recordings fairly and recover after restart (issue #135)

Builds on the capacity model in 0058. Owner gate: Brandon
accepted the behavior on 2026-11-02; the acceptance is
recorded on #134/#135.

## What this change adds

- Fair order in the core tick: jobs are processed earliest
  scheduled start first, ties by saved job order
  (`fair_order_ids`), and the capacity gate is consulted in
  that order — a freed connection goes to the job that was due
  first, not the job that happened to be saved first.
- Same-tick accounting in the adapter: a start granted by the
  gate earlier in a tick counts against later jobs in that
  tick (the in-use snapshot cannot see a start that has not
  executed yet). Two waiting jobs can never both take one
  freed slot, including the expiry/capacity race at a job's
  stop boundary, where expiry wins and the job is Missed with
  its conflict reason.
- A running recording is never interrupted by the queue: the
  gate can only delay a start, and the only stop path remains
  the user's own conflict choice in #134.
- Providers stay independent: the gate is per account, so a
  saturated account never delays another provider's jobs.
- Restart recovery stays reassessment, not replay (built in
  #134, pinned here): waiting jobs re-arm to scheduled on
  load with their reason persisted; a job whose window closed
  during downtime is Missed on the first tick with the reason
  retained on the job, and #130's outcomes for work that did
  record are untouched.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 183 passed, 0 failed. New tests:
  a freed slot goes to the earliest start regardless of saved
  order; equal starts fall back to saved order; a blocked
  account leaves other jobs starting; capacity freeing exactly
  at the stop boundary expires the job Missed with its reason
  instead of starting it; a waiting job that expired during
  downtime reloads scheduled and is Missed on the first tick
  with its reason retained.
- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, playlist parity PASS against the pinned Python.

## Not covered here

Live multi-connection contention on a real account remains
with the replacement-gate pack (#137), as in 0058.
