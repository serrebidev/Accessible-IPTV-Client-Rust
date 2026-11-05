# 0086 — Second-round batch 14: EPG import details (audit N21)

## Scope

- Audit finding N21 in `docs/feature-parity-audit.md`, against
  Python `playlist.py` (`_http_download_gz_with_resume`, the
  cross-process import lock), `main.py:746`
  (`_lower_current_thread_priority`), `main.py:2825`
  (`_offer_epg_schema_repair`), and `main.py:6637`–`6760`,
  `6863` (the hand-started import and its messages) in the
  pinned oracle. The import-finished boxes were left to this
  batch by validation 0083.

## What changed

- Gzip resume (core `epg.rs`): an HTTP(S) source ending in
  `.gz` (and `EPG_GZ_DOWNLOAD` not set to 0/false) now goes
  through `download_gz_with_resume`, the port of Python's
  robust path: a stable per-URL temp file
  (`epg_{sha256}.xml.gz`; Python names it by MD5 — the name is
  an implementation detail, the stability is the behavior),
  a per-path in-process lock, partials older than 30 minutes
  discarded, up to four attempts resuming with
  `Range: bytes=N-`, a 416 or a Range-ignoring 200 restarting
  clean, completeness proven by the server-declared byte
  count (Content-Range total, else Content-Length), a 16 KiB
  header probe when the size was declared and a full
  stream-to-EOF probe when it was not, and the temp file
  deleted on success unless `EPG_KEEP_TEMP_GZ` is set. The
  verified bytes then take the existing decode + parse path.
- Cross-process import lock (core `epgstore.rs`):
  `wait_for_import_lock` / `ImportLockGuard` port
  playlist.py's file lock — `{stem}.import.lock` +
  `{stem}.import.pid` beside the store, stale reclamation
  only for a checked-dead owner (after 1 s) or an uncheckable
  owner after two hours, release only of one's own lock.
  `run_epg_load` holds it for the whole store-mirroring run,
  where Python's importer takes it; a lock that never frees
  fails the load after 30 minutes with Python's message,
  "Timed out waiting for the EPG import lock".
- Background thread priority (core `epg.rs`):
  `lower_current_thread_priority` ports `main.py:746` —
  Windows `SetThreadPriority(THREAD_MODE_BACKGROUND_BEGIN)`
  with `THREAD_PRIORITY_LOWEST` as fallback, a no-op
  elsewhere — and runs at the start of every `run_epg_load`,
  where Python's import worker calls it. (No other Rust
  worker had a priority pattern to match; this introduces
  the platform call in the same shape Python makes it.)
- Import started/finished messages (app `main.rs`):
  `start_manual_epg_import` ports
  `start_epg_import_background(force=True, notify=True)` —
  forced start, refusal when there is nothing to import, the
  guide is disabled, or a load is already running — and the
  `epg_import_notify` flag carries Python's
  `_epg_import_notify` to `poll_epg`, which reports the
  outcome through the N18 queue: "The EPG import has
  finished. The programme guide is up to date." ("Import
  Complete") on success, Python's failure text ("Import
  Failed") when no source loaded. File > Import EPG to DB
  gained Python's guard boxes — "EPG is not enabled." ("EPG
  Not Available"), "EPG import is already in progress." ("In
  Progress"; the picked source is still queued through the
  existing re-check so it loads when the running import
  lands), "The EPG import could not be started." ("Import
  Failed") — and the "Import Started" box, shown only after
  the import actually began, promising the completion
  message.
- Old-schema repair prompt (core `epgstore.rs`, app
  `main.rs`): `store_missing_columns` ports
  `_check_schema` (a read-only open; `programmes.description`
  is the required column; a missing or unreadable file is not
  a repair case). When the Channel EPG view would fall back
  to a store that reports missing columns, it offers
  Python's prompt instead of opening a window that can only
  fail: "The EPG database was created by an older version
  and is missing newer fields. Import the guide again to
  update it?\n\n(Without this, the EPG views cannot show
  programme descriptions.)" ("EPG Database Update Needed",
  Yes/No). Yes starts the repair import through
  `start_manual_epg_import` (notify on, as in Python); a
  failed start shows "The EPG import could not be started."

## Evidence

- Core tests: the import lock (acquire excludes a second
  acquirer, release removes both files and frees the lock, a
  dead owner's lock is reclaimed), `store_missing_columns`
  (an old-schema database reports `description`, a current
  store and a missing file report nothing), and the gzip
  resume server tests (a transfer cut mid-body resumes with
  the recorded `Range: bytes=N-` request and the verified
  download deletes its temp file; a Range-ignoring server
  restarts clean over a seeded partial).
- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (383 tests), playlist parity check PASS,
  `git diff --check` clean.

## Residuals

- The repair prompt can only trigger on a store whose
  `programmes` table lacks `description`. Every database the
  Rust client has ever created carries that column (it is in
  the store's first schema), and `EpgStore::open` migrates
  the columns it owns, so the prompt is parity armor for
  foreign or hand-damaged databases rather than a path a
  Rust-created store can reach — the same defensive shape as
  Python's, whose prompt exists for pre-description Python
  databases.
- Python downloads the gzip with a 300 s request timeout;
  the resume path uses the EPG fetch's existing ureq
  defaults, like the non-gzip path beside it.
- Python's `_offer_epg_schema_repair` is reached from its
  channel EPG view's dedicated read-only open, so it checks
  on every view. The Rust view reads the in-memory snapshot
  first (the #181 fast path), so the check runs when the
  view would fall back to the store — the only case where
  the missing column can bite.
- The repair import's completion is reported by the same
  notify flag as a menu import (Python sets notify there
  too); nothing else about the load changes.
- Brandon's NVDA pass on the new boxes remains his, as with
  the other batches.
