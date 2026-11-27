# 0115 — Python PR #45 port: Hungarian localization polish

Date: 2026-11-27. Branch: port-pr45. Ships as: v0.4.40.
Python spec: serrebi/Accessible-IPTV-Client PR #45 (htibcsike),
"Polish Hungarian localization for v1.152.2", merged 4ed5adc9,
shipped as Python v1.152.3. Port rule: Brandon, 2026-11-27 — every
accepted Python PR is ported to the Rust client.

## What PR #45 changed in Python

Four files, all Hungarian text:

- `locale/hu/LC_MESSAGES/iptvclient.po` — 17 msgstr corrections.
- `docs/help/hu.md` — 14 polished paragraphs.
- `locale/hu/LC_MESSAGES/release_notes.po` and
  `docs/release-note-translations-20261127.json` — the Hungarian
  rendering of the PR #44 release note. The Rust client has no
  translated-release-notes mechanism (What's New is a single
  in-app text), so these two have no Rust counterpart; not ported.

## What landed here

Catalogue (`crates/iptv-app/po/hu.po`): all 17 changed msgids now
carry the Python post-#45 Hungarian text, verified mechanically
17/17 against the Python catalogue at the PR #45 merge. 12 replaced
in place, 2 replaced in the wrapped-entry form, and 3 appended —
"No local network address is available for casting. Reconnect and
try again.", "The application is closing." and "The postponed alert
falls outside the new time window." were missing from the Rust
catalogue entirely. One repair during the sync: the entry for "The
end must be in the future..." had a wrapped msgstr whose first line
was empty; the replacement initially left the old continuation lines
dangling after the new text — caught by the verification pass and
removed; a sweep for the same shape over the whole file found no
other case. `msgfmt --check` clean.

Help (`docs/help/`): applying the PR #45 Hungarian paragraphs
exposed a gap in the v0.4.39 port — the PR #44 help passages had not
been ported at all. They are now: the four new paragraphs (Review
Guide Changes; Find Text in Recording Subtitles / Recording Playback
Speed; Pause Recording Rule + Export Selected Recordings to
Calendar + exit note; report-issue Try Again / Copy Report) and the
replaced recording-format bullet, applied from the Python post-#44
files to all 14 languages (5 blocks each; 11 for Hungarian, whose
PR #44 merge also caught the file up). Placement is by section,
matching the Python files; the English file's two section-inserted
paragraphs were hand-checked and moved to the exact Python spots
(end of Scheduled recordings; after the About paragraph in Getting
help). The PR #45 Hungarian polish (14 paragraphs) is applied on
top: 13 replaced verbatim, and the system-tray paragraph took the
new exit wording as a tail replacement because the Rust file's
earlier sentences carry an older menu name than Python's. Net per
non-Hungarian file: +10/−1 lines (the one deletion is the old MP4
bullet).

## Gate

Local gate, all green: fmt --check clean; clippy --locked
--workspace --all-targets -D warnings exit 0; cargo test --locked
--workspace 555 passed / 0 failed; playlist parity PASS against
Python v1.151.6 (a29ad9a3); git diff --check clean. One environment
note: the first test run failed at link time (libgtk-3 /
libgstplayer dev symlinks gone — the VM had been replaced again);
~/workspace/bin/vm-dev-deps.sh restored them and the rerun passed in
full. No code changed in this port — catalogue and help text only,
plus the release version bump.
