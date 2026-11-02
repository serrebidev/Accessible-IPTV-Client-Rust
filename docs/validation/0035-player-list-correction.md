# 0035 — Player list correction (32 players) and audit addendum

Date: 2026-11-01. Corrects validation 0021 (G2).

## What was wrong

Validation 0021 recorded the Options > Media Player to Use radio list
as ten labels "in Python order". That was a misread of the Python
source. The full Python GUI audit (delivered after the v0.2.0
release) lists 32 players, and direct verification against the
pinned Python source confirms it: `PLAYER_KEYS` in `main.py`
(9aa89e0) has 32 entries, and `external_player.py` carries
per-platform executable tables for them.

## Fix

- Adapter: PLAYER_LABELS is now all 32 labels in Python's exact
  order, followed by Custom Player... Menu ids renumbered to make
  room (radios 5131-5162, custom 5163, preferred audio 5164,
  announce 5165-5168, language 5170-5184, checks 5195-5198,
  export 5199, import 5200; Recordings range unchanged).
- Core: `candidate_executables` now mirrors Python's tables
  verbatim — Windows paths for 17 players, macOS app paths for 6,
  Linux commands for 26; a player with no entry for the platform
  yields no candidates, matching Python's "could not locate" /
  "not configured for Linux" outcomes (the previous generic
  lowercase-command fallback on Linux was a deviation and is gone).
  A doubled-backslash typo in the Windows path literals was also
  found and fixed while porting the tables.
- New core test `candidate_tables_match_python` pins samples from
  all three platform tables.
- User guide: the Media Player sentence again names PotPlayer
  (a docs-audit edit had removed it based on the same misread).

## Other full-audit deltas (tracked as issues, not in this change)

Player Sleep Timer; player Audio Output Device choice; Local Live
Buffer / Rewind Live TV / Return to Live; What's on Now filter
field; Continue Watching "Check Availability"; settings backup
password (Python encrypts exports with Fernet/PBKDF). The Rust
player's seek slider and time label are a superset of Python's
player (Python has neither) and are kept.

## Evidence class

Automated: fmt clean, clippy zero warnings, 132 workspace tests
pass, playlist parity PASS. Owner-heard NVDA evidence remains
issue #86.
