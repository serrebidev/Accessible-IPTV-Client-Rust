# 0093 — Release v0.4.17 (v0.4.15 recovery)

Date: 2026-11-05. Fix branch `agent/fix-release-0.4.16` from main
a28e9c0 (the v0.4.15 release merge), merged as PR #204; version
bump branch `agent/release-0.4.17`.

## Tag incident (why .17, not .16)

The fixes were merged under version 0.4.16 (PR #204, main
7453997), but the v0.4.16 tag was then placed from a stale local
clone onto a28e9c0 — the v0.4.15 commit — because the clone's
`origin/main` had not been re-fetched after the API merge. The
Release run it triggered (42146701072) was cancelled within a
minute, before any build finished; nothing published. Per the
no-re-tag rule the misplaced tag stays as a tombstone alongside
v0.4.15's, and the release ships as v0.4.17. Lesson recorded:
always `git fetch` and read the merge SHA from the API before
tagging — the local clone does not learn about API merges on
its own.

## What happened

The v0.4.15 Release run (42044440609) failed: Linux success,
Windows and macOS failure, Publish skipped. The v0.4.15 tag stays
as a tombstone, never published, per the no-re-tag rule (same as
v0.4.5). The fixed release is v0.4.16.

## Root causes — both in N21's import lock (PR #202)

1. Windows Gates, clippy: `iptv-core` carries
   `[lints.rust] unsafe_code = "forbid"`, and N21 added the
   crate's first `unsafe` blocks — `pid_exists`'
   OpenProcess/CloseHandle (epgstore.rs) and
   `lower_current_thread_priority`'s SetThreadPriority
   (epg.rs). Being `#[cfg(windows)]`, they never compile on
   Linux, so local gates and the PR's Linux checks could not
   see them.
2. macOS Gates, one test failure:
   `import_lock_reclaims_a_dead_owners_lock`. `pid_exists` had
   only Windows and Linux (/proc) implementations; every other
   platform returned `None` ("cannot be checked"), so on macOS
   a dead owner's lock was treated as uncheckable and held for
   the two-hour age fallback instead of being reclaimed.

## Fixes

- `pid_exists` on unix (Linux and macOS alike) now probes with
  `kill(pid, 0)`: success or EPERM means the process exists,
  ESRCH means it does not, anything else stays "unknown". This
  is the check psutil's `pid_exists` makes, so behavior now
  matches the Python oracle on all three platforms.
- The crate lint is `unsafe_code = "deny"` instead of
  `"forbid"`, with the reason recorded in Cargo.toml: the two
  audited FFI functions carry per-function
  `#[allow(unsafe_code)]` and SAFETY comments (handle closed
  exactly once; pseudo-handle needs no closing; signal 0
  delivers nothing). Every other item in the crate still
  errors on unsafe by default.
- libc added as a unix-only dependency of iptv-core.

## Gates

Local (Linux) on the fix branch: fmt clean, clippy
`-D warnings` clean, workspace tests pass (the dead-owner lock
test now exercises the kill probe), playlist parity PASS. The
Windows and macOS legs are verified by the v0.4.16 Release
run's Gates step, whose conclusion is read directly before the
release counts as shipped.
