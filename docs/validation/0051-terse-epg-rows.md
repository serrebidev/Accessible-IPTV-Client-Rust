# 0051 — Terse speech: the guide belongs in the row, after the name

Date: 2026-11-02. Owner report: "The app has too much
wordiness in it. It is not required. It also says the EPG
way too easily. It just needs to say it after the channel
name."

Diagnosis, against the Python oracle (main.py):
- Python's channel rows carry the guide IN the row text:
  "{name} — {now title} (HH:MM–HH:MM) — Next: {next title}
  (HH:MM)", built by _now_playing_suffix/_programme_label
  and refreshed on a timer. A screen reader moving through
  the list hears the channel name first, then the guide.
- The Rust app had the rows bare AND a Rust-only behaviour,
  bind_epg_highlight, that spoke "Now: … until …. Next: …
  at …." through the live region on EVERY selection change —
  the guide volunteered eagerly, without the channel name,
  fighting the row speech. That is the wordiness.

Fixes:
- Channel rows now carry Python's suffix. A suffix map
  (playlist index → suffix) is built from the loaded guide
  when the guide lands, when the playlist changes, and when
  the data ages past five minutes, and the list re-renders.
  Format matches Python exactly, including the "Next:"
  segment carrying only the start time.
- bind_epg_highlight no longer speaks. Selection only fills
  the Tab-reachable panels (stream URL, episode description)
  silently, with Python's placeholder "No description
  available for this programme." when there is nothing.
- The libVLC-missing status (0048) is trimmed to one line.

Tests: row text with a suffix reads name-then-guide; a
channel without guide data keeps the bare name; the suffix
shape matches Python's (start–end for now, start only for
next, empty when neither).

Evidence class: automated — fmt clean, clippy zero
warnings (-D warnings), workspace tests pass, playlist
parity PASS. The owner's ear is the closing check; ships
as v0.4.1.
