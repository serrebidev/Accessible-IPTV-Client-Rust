# 0104 — C10 and C12: guide assignment and per-source guide ids (round 3)

Port target: Python v1.151.6 `accessible_tools_ui.py`
(`EpgMappingDialog`, `_assign_programme_guide`), `main.py`,
`playlist.py` (`insert_channel`, `choose_source_collision`),
`channel_identity.py`, `channel_preferences.py`
(`epg_channel_overrides`). Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C10, C12).

## What landed

- C12 (core, previous commit): a guide id that two sources define
  under different names is split at import, the later source's
  copy stored as `epg:<source_key>:<id>` with the guide's own id
  kept in `channels.xmltv_id`; a feed's programmes for a channel
  it never declared no longer overwrite another source's guide;
  matching resolves a colliding id by one exact, number-safe
  name and takes a qualified copy that is the only one carrying
  an id.
- C10 (core, previous commit): `channel_preferences` holds
  Python's `epg_channel_overrides`; an assignment wins in
  `match_guide_channel` everywhere, one whose guide id vanished
  stays unavailable, and an assigned channel's guide is only
  that channel.
- C10 (app, this commit): View > Assign Programme Guide...
  (id 5130, help topic `epg`) opens the `EpgMappingDialog`
  port: channel label, search prefilled with the current
  assignment, guide channels ordered by display name then id
  as `name [id]`, case-folded substring filter over name plus
  id capped at 500 rows with
  "Showing {count} of {total} matching channels.
  Refine the search if needed.", double-click to accept,
  OK needs a selection ("Select a programme guide channel,
  or choose automatic matching."), Use Automatic Matching
  clears the override, Cancel and Escape close without
  saving. With no channel selected the view status reads
  "Select a live channel first."; Rust `Channel` has no
  movie kind, as Python's dict does, and the list holds live
  channels here, so the guard covers Python's non-live
  refusal. A playlist change while the dialog is open is
  refused with "The playlists changed. Select the channel
  again." on save.
- A save persists the override, reloads the process-wide
  table, and advances the guide generation so the suffix
  worker rebuilds its matches instead of reusing the cached
  ones, the Rust equivalent of Python clearing
  `_program_match_cache` and `_epg_match_cache` and forcing
  `_refresh_now_playing_labels(force=True)`. The guide shown
  on channel rows, the guide views, search and scheduled
  recordings all follow the assignment.

## Evidence

- Core: `epgstore.rs::colliding_guide_ids_are_scoped_per_source`,
  `channel_preferences.rs::an_assignment_wins_and_a_vanished_one_stays_unavailable`
  (406 core tests).
- `cargo test --workspace` and `cargo clippy --workspace
  --all-targets -- -D warnings` green on Windows; `cargo
  check -p iptv-app` green.
- App compile covers the new dialog; no UI test in the
  workspace drives the wx dialog end to end.

## Brandon's NVDA check

1. With a live channel selected, View > Assign Programme
   Guide...: the search holds the current assignment (empty
   for automatic), the list shows `name [id]`, typing
   filters by name and id and the count line says how many
   matched.
2. Choose a guide channel and press OK: the channel's row
   suffix, View EPG and What's on Now switch to the assigned
   guide without reopening the app.
3. Use Automatic Matching: the override clears and the rows
   return to automatic matching.

## Residuals

- Python loads the guide list on a background thread with
  "Loading guide channels..."; Rust reads the channels
  synchronously before showing the dialog, so the loading
  label is only the initial value.
