# 0094 — Release v0.4.18 (v0.4.17 recovery)

Date: 2026-11-05. Branch `agent/release-0.4.18` from main
9929406 (the v0.4.17 release merge).

## What happened

The v0.4.17 Release run (42147300522) failed on Windows only;
Linux and macOS succeeded — the macOS dead-owner test that
failed the v0.4.15 run now passes, confirming the `kill(pid, 0)`
probe fix (validation 0093). The v0.4.17 tag stays unpublished
as a tombstone; the release ships as v0.4.18.

## Windows failures, all in Windows-only code or test setup

1. Clippy `doc_markdown` ×2 (epg.rs): the thread-priority doc
   comment named `THREAD_MODE_BACKGROUND_BEGIN` and
   `THREAD_PRIORITY_LOWEST` without backticks. Backticked.
2. Clippy `unnecessary_wraps` (epgstore.rs Windows
   `pid_exists`): the Windows variant always returns `Some`;
   the shared signature keeps `Option` because other platforms
   may not know. Allow added with that reason, matching the
   pre-existing pattern on the old Linux variant.
3. Clippy `unnecessary_wraps` (proxy.rs Windows
   `ensure_firewall_rule`, from N20): same shared-signature
   shape — the non-Windows variant returns `None`. Allow added.
   This one predates the release attempts; it only surfaces
   now because the Gates step compiles the crate on Windows.
4. Test `import_lock_reclaims_a_dead_owners_lock` panicked:
   `set_modified` unwraps an Access Denied error. The test
   opened the lock file read-only; Windows refuses `SetFileTime`
   on a read-only handle. The test now opens it with write
   access. Test-only change; the lock logic was already proven
   by the same test passing on Linux and macOS.

## Gates

Local (Linux) on this branch: fmt clean, clippy `-D warnings`
clean, workspace tests pass, playlist parity PASS. The
Windows leg is verified by the v0.4.18 Release run's Gates
step, whose conclusion is read directly before the release
counts as shipped.
