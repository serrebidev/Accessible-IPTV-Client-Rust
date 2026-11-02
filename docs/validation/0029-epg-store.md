# Validation 0029: SQLite EPG store

Date: 2026-11-01. Issue: #52.

## Landed

- iptv-core::epgstore: an EpgStore over rusqlite (bundled
  SQLite) with channels/programmes tables and indexes.
  import_guide replaces the stored guide in one transaction;
  queries: counts, now_next, programmes_between, search_titles
  (LIKE with escaped wildcards, joined to channel names).
- The adapter mirrors every freshly loaded guide into
  epg.db beside the config (failures surface as a status suffix,
  never blocking the in-memory guide).
- Channel EPG (and everything built on guide_programmes) falls
  back to the store when the in-memory guide has nothing for
  the channel: tvg-id candidates in match order, from an hour
  ago, capped at 200 rows — so schedules still answer after a
  restart before the next guide fetch, and for channels the
  matcher missed.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 116 workspace tests
  pass (1 new: import/query/re-import round trip on an
  in-memory database), playlist parity PASS, git diff --check
  clean.

## Owner-heard evidence still owed

None specific; exercised indirectly through the Channel EPG
window's NVDA pass.
