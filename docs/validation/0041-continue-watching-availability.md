# 0041 — Continue Watching: Check Availability (issue #98)

Date: 2026-11-02.

Implemented from the line-referenced Python spec
(resume_dialogs.py / playback_resume.py):

- The dialog now shows every saved position, not just ones
  whose channel is still in the catalogue. Resume entries
  gained a title captured at checkpoint time (core
  ResumeEntry.title, persisted in resume_positions; older
  files read back with an empty title), falling back to the
  channel name, then to Python's exact fallback "Previously
  saved content (title unavailable)".
- Rows carry Python's columns as one line: title, position
  of length watched, Last Watched (new core
  epg::local_datetime, "%Y-%m-%d %H:%M", "Unknown time" when
  unset), and Availability — "Not checked" until a check runs.
- Check Availability acts on the selected row only (Python
  has no check-all): it announces "Checking saved content…",
  re-resolves the bookmark identity against the current full
  catalogue (all sources, never probes the stream URL), and
  sets the row to "Available" (storing the fresh channel, so
  Resume plays the current URL) or "Unavailable or
  ambiguous", speaking the result through the dialog status.
  Resume on an unavailable row stays in the dialog with that
  status, as Python does.
- Buttons now match Python: Resume (was Play), Check
  Availability, Remove, Clear All, Close. Remove and Clear
  All confirm with Python's exact prompts ("Remove this
  saved playback position?" / "Remove all saved playback
  positions?"). The empty state speaks Python's "No saved
  playback positions."
- Recorded deviation: Python's third outcome, "Could not
  check availability.", covers provider/network errors from
  its Xtream locator path; the Rust resume model has no such
  locators (bookmarks are playlist channel keys), so local
  checks cannot produce it. Stalker VOD bookmarks likewise
  resolve through the catalogue path only.

Evidence class: automated — fmt clean, clippy zero warnings,
133 workspace tests pass (title capture and the datetime
format covered by new core tests), playlist parity PASS.
Owner-heard NVDA evidence remains #86.
