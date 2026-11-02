# Issue #24 (parity slice 4): EPG core

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/epg-core`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::epg`: streaming XMLTV parser (quick-xml) with gzip decoding
  (`decode_epg_bytes`), XMLTV timestamp parsing with timezone offsets,
  channels/programmes in document order, programmes sorted per channel.
  `EpgWorker` fetches and parses off the UI thread, per-source failure
  results only when every source fails, sources merging into one guide.
- The Python matching policy is ported whole: guide indexes by id, by
  canonical name and by noise-stripped name (both as written and with word
  numbers as digits); `match_guide_channel` tries expanded tvg-id
  candidates first (the `@City` suffix, quality-suffix and sport→sports
  variants, one extra dotted segment), then the known-ids guard, then name
  passes protected by the number-disagreement rule — BBC 2 can never land
  on BBC Three's guide. The `channel_names.py` vocabulary (strip tags,
  noise words, word numbers, ignored resolution numbers) is ported with
  it.
- Now/next lookup and chronological programme search (titles and
  descriptions) over the parsed guide.
- `iptv-core::reminders` ports `programme_reminders.py`: reminder channel
  keys and ids are sha256 over the same Python `json.dumps` payloads —
  golden tests pin the exact Python-computed hashes; stored rows are
  revalidated by recomputing their ids; active-first ordering, 500-row
  cap, lead window 0..=120 minutes.
- Adapter: guides listed in the config's `epg_urls` load at startup in
  the background; highlighting a channel announces its now/next; stored
  reminders fire on the poll tick (announcement + notified state saved,
  ended ones expire), and an active reminder keeps the poll timer alive.

## Evidence

- 71 workspace tests pass (10 new: XMLTV parse/times/offset handling,
  number-guard matching, tvg-id variants, name normalization, reminder
  build/normalize/tamper-rejection/due windows, and Python golden hashes
  for the reminder keys).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against pinned
  Python 9aa89e0, `git diff --check`.

## Evidence boundary

- Storage is in-memory. Python keeps the guide in SQLite (`epg.db`) so
  multi-million-programme guides stay cheap; the in-memory guide is the
  seam, the SQLite store is the recorded scale endgame.
- The EPG viewer/search dialog and the add-reminder gesture are not wired
  yet: core search exists, reminders fire, but creating one needs the
  programme UI, which lands with the shell work.
- No owner-heard NVDA pass is claimed for the now/next announcements.
