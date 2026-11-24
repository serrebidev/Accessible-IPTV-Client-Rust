# 0107 — C2: Recover from Channel Archive (round 3)

Port target: Python v1.151.6 (`a29ad9a3`) `main.py`
`ScheduledRecordingsDialog._recover_archive`, with
`programme_reminders.resolve_channel` and
`recording_limits.check_start`. Plan:
`docs/superpowers/specs/2026-11-23-python-parity-round3-design.md`
(C2).

## What landed

- Core (`archive_recovery.rs`, new): `recoverable_status` is
  Python's menu gate (missed, failed, partial only);
  `resolve_recovery_channel` finds the booked channel through
  the reminder scope key with exactly one distinct media
  identity, as Python's `resolve_channel` does;
  `recovery_show` is Python's `prepare` — the programme window
  from the job's own padding (`start + pre`, `stop - post`),
  refused with Python's messages when the status is wrong,
  the programme is still running, the channel has no archive
  or the programme is older than the catch-up window; the
  recording's own padding rides along as the download window
  (Python's `schedule_padding`), so the copy covers the whole
  booked span; `check_start` is Python's
  recording-connection-limit refusal with its exact message.
- App (`main.rs`): the Scheduled Recordings context menu gains
  `Recover from Channel Archive...` (id 6057, no shortcut —
  Python has none) for one selected missed, failed or partial
  recording. Choosing it resolves the channel, checks the
  archive window, the one-stream slot and the connection
  budget, re-resolves the channel list and re-checks the
  budget before starting the download, with Python's boxes
  throughout: `Channel Not Found`, the unavailable message,
  `The channel list changed. Try again.` and the limit
  refusal all under `Catch-up Download` (information, or a
  warning for the second budget check) — Python's titles and
  icons. The download itself is the existing catch-up download
  flow (duplicate refusal, destination check, direct-file
  probe, ffmpeg capture, retries, Stalker link resolution),
  widened by the recording's own padding instead of the
  configured padding, carried through the probe and the
  capture launch.
- English help (`docs/help/en.md`, Scheduled recordings): the
  Python recovery sentence in the section, in Python's
  wording. All dialog and message strings already ship in
  every patois catalogue; the help sentence joins English
  only, per the translator-sync design in validation 0091.

## Evidence

- Core tests beside the code: `archive_recovery.rs` (status
  gate, channel resolution incl. ambiguity and unknown
  snapshot, programme times and kept padding with the
  window-round-trip to the booked span, every refusal gate,
  limit counting by account).
- `cargo test -p iptv-core` (425 passed) and
  `cargo clippy -p iptv-core -p iptv-app --all-targets
  -- -D warnings` green on Windows; `cargo fmt --check` clean.

## Brandon's NVDA check

1. Scheduled Recordings: select a missed, failed or partial
   recording, open the Applications-key menu — `Recover from
   Channel Archive...` is there; on a scheduled or completed
   recording it is not.
2. Recover a programme still in the archive: the download
   starts (Downloads window), named title - channel with the
   airtime, covering the recording's padded span.
3. Recover an unavailable programme: `This programme is
   unavailable in the channel archive.` under `Catch-up
   Download`.
4. Recover when the channel no longer resolves: `Channel Not
   Found` under `Catch-up Download`.

## Residuals

- The budget re-check counts the recordings the app can
  attribute to an account (scheduled recordings through their
  job snapshot, manual recordings through their channel key).
  Python reads them from recorder metadata, which Rust's
  engine does not keep, so a running catch-up capture is not
  counted until its ffmpeg session carries the account; the
  first check and the retained window behaviour are
  unaffected.
- Python's `ready` leg runs after a background prepare; Rust
  runs both legs synchronously, so the "channel list changed"
  guard compares a re-resolution instead of a list identity.
