# 0062 — NVDA terseness: control descriptions, hotkey consumption, search selection

Owner report, 2026-11-02 23:48, with an NVDA debug log from
v0.4.3: landing on Categories, Search, or Channels spoke long
instructional sentences and status text; Ctrl+Up/Down moved
the selection as well as (or instead of) changing volume;
the search field's select-all behavior was unreliable.

## Causes and fixes

- Wordiness — descriptions. The pinned Python client gives
  its controls accessible names only (SetName /
  SetAccessibleName; it sets no help text or descriptions
  anywhere). The Rust client had set instructional
  descriptions on the search field, the categories tree, the
  playlist-view choice, and in the VOD, cast, help,
  subtitle-file, and What's-on-Now dialogs; NVDA speaks a
  control's description on every focus entry. All are
  removed. The channels list was worse: every refresh
  overwrote its description with the full status message
  (for example "Playlist contains malformed entries; usable
  channels kept. 6 matching channels."), and load errors did
  the same. Refresh and error text now lives only on the
  status line; the list's description carries only its two
  short load states ("No playlist loaded. Channel list is
  empty." before any load, "Loading playlist..." during one)
  and is cleared when results or an error land.
- Ctrl+Up/Down moved the arrows. The main-window hotkey
  binding ran the matched action (volume, record) and then
  unconditionally skipped the event, so the focused list or
  tree ALSO processed the key as navigation. The dispatcher
  now reports whether it handled the key and the binding
  consumes handled keys — the same pattern the player window
  already used. Unmatched keys still skip, so plain arrows
  and editing keys behave natively.
- Search selection. Entering the search field now selects
  the whole query by default (typing replaces it), and
  Ctrl+A selects all and is consumed.

## Automated evidence (2026-11-02)

- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, `cargo test --workspace`: 185 passed, 0 failed,
  playlist parity PASS against the pinned Python.
- The description removals and the consumption change are
  adapter wiring; the behaviors they produce are screen
  reader behaviors and are not unit-testable headlessly.

## Owner-heard evidence still required

Brandon's NVDA retest on the shipped build: Categories,
Search, and Channels should announce name and role only
(plus the list's load state), Ctrl+Up/Down should change
volume without moving the selection, and entering Search
should present the query selected. This is the same
owner-heard class tracked by #86 and is not claimed here.
