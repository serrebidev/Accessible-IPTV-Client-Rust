# 0040 — What's on Now filter (issue #97)

Date: 2026-11-02.

What's on Now now matches the Python dialog:

- Filter field ("Filter (Tab):" label, accessible name
  "Filter programs") filtering by case-insensitive substring
  over programme title or channel name — Python's exact rule
  from dialogs_misc.py. Live on text change (rebuild only
  when the filtered set actually changes); Enter applies and
  moves focus to the list, as Python's handler does.
- The count label reads "{count} programs", Python's format,
  updated on every filter change.
- The list's accessible name is Python's "Currently airing
  programs", and the Schedule Recording button Python has is
  now present (it schedules the selected programme through
  the existing DVR path). Play and Close unchanged.
- This dialog also gained a real sizer layout (filter row,
  count, list, status field, button row) — the generic
  list_dialog shell previously laid nothing out.

Evidence class: automated — fmt clean, clippy zero warnings,
132 workspace tests pass, playlist parity PASS. Owner-heard
NVDA evidence remains #86.
