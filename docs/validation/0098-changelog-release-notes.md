# 0098 — Release notes from CHANGELOG.md (v0.4.22)

Brandon's order (2026-11-07): "I need the release notes
and change log to be like how BlindRadio's is."

## Before

Every published release of this app (25 of them, v0.1.0
through v0.4.21) had an empty body. The Release workflow's
Publish step used softprops/action-gh-release with no
body and no body_path, so nothing was ever set. The only
human-readable notes were the in-app What's New text,
compiled into the binary.

## The BlindRadio system, ported

- `CHANGELOG.md` at the repo root, BlindRadio's format:
  one plain-language section per release under a bare
  `## X.Y.Z` heading, newest first. Sections for
  0.3.0-0.4.21 were generated from the app's own
  WHATS_NEW_TEXT entries (verb-led entries reworded to
  start with a subject; one internal CI sentence dropped
  from 0.4.8); 0.2.0 and 0.1.0 were written from their
  merge history.
- The Publish job extracts the tag's section with a
  small python step (the pattern BlindRSS adopted from
  BlindRadio), writes `release-notes.md`, and passes it
  as `body_path`. A missing or empty section exits 1 —
  the publish refuses to ship an empty release page.
  The extractor was tested locally both ways before the
  PR: the 0.4.22 section extracts exactly, and a
  nonexistent version fails with the refusal message.
- This is a workflow edit made under Brandon's direct
  request, which the workflow header requires.

## Release

PR #210 merged as a60b05dd (local origin/main and the
API branch head verified equal before tagging). Tag
v0.4.22 on that SHA. Release run 42400777181 completed
success — all three platform builds and the Publish job.
Verified by API: v0.4.22 published 2026-11-07T18:44:57Z,
draft=false, Latest, exactly 7 assets, and its body is
byte-identical to the CHANGELOG.md 0.4.22 section.

Local gates: cargo fmt clean, clippy -D warnings clean,
cargo test --workspace 394 passed / 0 failed, playlist
parity check OK, git diff --check clean. One gate run
had to be repeated: an over-broad `pkill` on this shared
VM SIGTERM'd the first attempt's wxdragon-sys build
script mid-build; the re-run on the warm target dir is
the green one recorded here.

## Backfill

All 25 older releases were PATCHed with their CHANGELOG
sections. Verified after: zero releases with an empty
body across the repo, and the v0.3.0 body spot-checked
byte-identical to its section.
