# 0113 — M1 to M11: muse.ai port, merged and reviewed (round 3)

Port targets: Python v1.151.6 per the ledger `M1`–`M11` entries
(`channel_preferences.py`, `media_navigation.py`,
`named_bookmarks.py`, `programme_reminders.py`,
`planning_tools.py`, `session_tools_ui.py`,
`recording_limits_ui.py`, `recording_library_ui.py`,
`favorite_collections.py`, `favourite_transfer.py`,
`personal_tools_ui.py`, `favorites.py`, `playlist_index.py`).

## What landed

Each group arrived as a one-commit Forgejo PR (`muse/m<N>-<slug>`)
and was reviewed against the Python source, merged onto `main`
keeping both sides, and gated before push:

- M1: Mute/Unmute (Ctrl+Shift+M / M) and Remember Volume for Each
  Channel; `channel_preferences` volume memory merged with the
  C10 guide overrides; Player menu ids 5116/5117, player mute id
  5341, shared `is_seekable` kept single.
- M2: Back/Forward 10/60 s, Go to Time (Ctrl+T), Playback Time (T);
  `media_navigation` core, player seek keys, seekable-duration
  guard; duplicate VLC `is_seekable` removed.
- M3: Add Named Bookmark / Named Bookmarks (unique per item,
  1000 cap, separate from resume); `named_bookmarks` core.
- M4: Remind Again in 5 Minutes, Remind at Programme Start,
  Export to Calendar (.ics); merged earlier via #93.
- M5: View > Notification History (latest 200, Details,
  Refresh, Copy, Clear); `notifications` core.
- M6: Per-event announcement switches (Playback, Subtitles,
  Guide) under Automatic Announcements.
- M7: Recording Connection Limits dialog; discovery + per-account
  budgets.
- M8: Recording Library Export List to CSV; `recording_csv`.
- M9: Favourite Lists (50×2000), portable export/import with
  ambiguous choice, Relink Missing Channel.
- M10: Programme Guide by Time + Programme Details, with
  reminders, scheduling and calendar export from guide rows.
- M11: Recently Watched keyed per playlist source
  (`m3u-history:<sha256>:<key>`), legacy fallback, refused
  ambiguity, source-aliased labels, canonical channel numbers.

Conflicts kept both sides throughout: C7/C8 queue and auto-next
against M1/M2 player work, C6 chapters against M2 seeks, M1/M2
menu ids re-numbered into free slots (MUTE 5116, REMEMBER 5117,
PM_MUTE 5341), and po catalogues kept on `main` — new strings
fall back to English pending translator sync, the same residual
as the C batch.

## Evidence

- `cargo test --workspace`: 493 core + 24 app + integration +
  9 VLC passed at the M10 merge; `cargo clippy --workspace
  --all-targets -- -D warnings` green after each merge.
- Python fidelity spot checks: identical menu labels,
  shortcuts, help topics and key caps (5000 volumes, 1000
  bookmarks, 30 searches, 50×2000 lists).

## Brandon's NVDA check

1. Mute in the player (M): playback continues silently;
   unmute restores the channel's remembered volume.
2. Seek keys and Go to Time on on-demand media; T announces
   elapsed/total/remaining.
3. Add a named bookmark, then Go to it after restart.
4. Notification History shows the session's boxes and masks
   credentials.
5. Favourite Lists: add, reorder, export, import, relink.
6. Guide by Time at a local instant, then Details / Reminder /
   Schedule / Calendar.
7. Recently Watched written by Python resolves after the swap.

## Residuals

- 13-language po sync for the M strings.
- NVDA live passes per ADR 0004 (no display session here).
