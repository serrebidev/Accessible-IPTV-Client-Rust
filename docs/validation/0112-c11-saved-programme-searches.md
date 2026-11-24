# 0112 — C11: Saved Programme Searches (round 3)

Port target: Python v1.151.x `saved_searches.py`
(`normalize`, `new_rule`, `signature`, `scan`, `normalize_seen`,
`read_seen`, `commit_seen`), `personal_tools_ui.py`
(`SearchRuleDialog`, `SavedSearchesDialog`,
`_scan_saved_searches`, the View menu item), and the
`personal-tools` entry in Python `docs/help/en.md`. Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C11).

## What landed

- Core (`saved_searches.rs`): `Rule` (32-hex id, 2-100 trimmed
  query, channel scopes, description flag, enabled flag) with
  Python's `normalize`/`new_rule`/`signature` exactly, including
  the 30-rule and 2000-channel caps, duplicate-id and
  control-character drops, and the "Enter a search of 2 to 100
  characters and choose at most 2000 channels." message. `scan`
  maps playlist channels to guide ids through the shared
  `GuideIndexes` matcher, queries `EpgStore::saved_search_programmes`
  (new; `instr(lower(...))` containment over the next 14 days,
  ordered by start) per enabled rule, scopes by channel key,
  dedupes by Python's sha256 slot identity, caps at 500 matches
  with the truncated flag, and caps descriptions at 16,000
  characters. `read_seen`/`commit_seen` are Python's 16-day
  `MAX_SEEN` history with the temp-file-plus-rename write and
  the current-matches-first eviction ordering, so a failure
  cannot erase state or recount. `resolve_channels` fills the
  dialog's key-to-channel lookup in key order.
- App (`main.rs`): View > "Saved Programme Searches..."
  (id 5132, help topic `personal-tools`) opens the
  `SavedSearchesDialog` port: rules list (`query` plus
  " — Disabled"), results list (`%Y-%m-%d %H:%M — title —
  channel`), read-only Description field, status line with the
  truncated note, and New.../Edit.../Delete/Refresh/Set
  Reminder.../Schedule Recording.../Close. The rule editor
  carries Python's Search field, "Search descriptions too" and
  "Enabled" checkboxes, and the Channels choice (keep chosen
  set on edit, all, selected channel). Delete confirms with
  "Delete saved search '{name}'?" (No by default). Rule saves
  go through `Persist::set_and_save` under
  `saved_programme_searches` and trigger a rescan.
- Scanning: a background thread opens the guide store, scans,
  commits the seen-history, and hands the outcome (with the
  rules signature) to the 60-second tick, which rejects stale
  outcomes, publishes results, and announces "New programme
  matches: {count}." when the announcement level allows it
  (Python's `_speak` gate). Scans run 30 s after start, every
  15 minutes, after every guide import, after rule edits, and
  from Refresh; a running scan is never duplicated.
- Results create reminders/recordings through the app's
  existing `add_reminder`/`add_dvr_job` builders, resolving
  channel keys against the current catalogue and asking
  "Choose Channel" when several channels share the slot.
- Help (`docs/help/en.md`): the `personal-tools` section with
  Python's text verbatim (English only, as in C5/C8/C10).
- Strings: every msgid already had catalogue entries, so no
  new translations were needed.

## Evidence

- Core tests beside the code: `saved_searches.rs` (normalize
  valid/dropped/capped rows, new-rule round trip and
  rejection, signature stability/sensitivity, seen window,
  commit counting + persistence + second-commit recount,
  `scan` title/description/scope/disabled behaviour,
  `resolve_channels` ordering). `EpgStore::saved_search_programmes`
  is covered through the scan test against a real store.
- `cargo test -p iptv-core --lib` green (458 tests, 8 new);
  `cargo check -p iptv-app` green; `cargo fmt --check` green.
- `cargo clippy -p iptv-core -p iptv-app --all-targets --
  -D warnings` still fails on pre-existing dirty-tree
  warnings outside this group (`recording_queue.rs`,
  `recording_library.rs`); this group's files report no
  findings.

## Brandon's NVDA check

1. View > Saved Programme Searches: empty rules list, status
   "No enabled saved searches."
2. New...: type a search, leave "Search descriptions too"
   off, choose All Channels, OK. The rule appears; after a
   moment the status reads "New programme matches: N." and
   results list `YYYY-MM-DD HH:MM — title — channel` rows.
3. Edit...: change the text or disable the rule; the label
   gains " — Disabled" and the next scan skips it.
4. Select a result: the Description field shows the
   programme description. Set Reminder... and Schedule
   Recording... act on the resolved channel; with several
   channels sharing the slot, "Choose Channel" asks first.
5. Delete asks "Delete saved search '…'?" and keeps the rule
   on No.

## Residuals

- The rule editor's scope choice has no named-favourite-list
  snapshot entry: M9 (Favourite Lists) has not landed in the
  Rust tree, so `favorite_collections` has no reader yet.
  The help text still says so verbatim (Python's words).
- `saved_search_programmes` uses `instr(lower(...))`;
  SQLite `lower()` folds ASCII only, where Python's
  `search_fold` casefolds Unicode. Non-ASCII queries can
  miss matches Python finds.
- A scan cancelled by a rules change finishes on its thread
  and is then discarded by the signature check (Python
  cancels mid-scan); the observable window is unchanged
  except for one wasted scan.
- Non-English help sections for `personal-tools` are not yet
  translated (English only, as in C5/C8/C10).
