# 0117 — Python PR #47 port: Hungarian polish for v1.152.4

Python PR #47 (htibcsike, "Polish Hungarian localization for
v1.152.4", merged 55d393be, shipped as Python v1.152.5) ported
under Brandon's standing Python→Rust parity rule, following the
PR #45 port's shape (validation 0115).

## What Python changed

- `locale/hu/LC_MESSAGES/iptvclient.po`: 18 Hungarian string
  revisions. Every one of the 18 msgids is a string the PR #46
  port added (recording notifications, saved queues, schedule
  filter views, transcript/excerpt export), so all 18 already
  existed in the Rust catalogue with the pre-#47 wording.
- `docs/help/hu.md`: 9 Hungarian user-guide paragraph edits.
- `locale/hu/LC_MESSAGES/release_notes.po` (1 revision) and the
  synchronized hu value in
  `docs/release-note-translations-20261127.json`: Python-only
  release-note mechanisms with no Rust counterpart — not
  ported, as with PR #45.

## What was ported

- `crates/iptv-app/po/hu.po`: the 18 msgstr blocks replaced
  with Python's post-#47 blocks. Verified by parsed-value
  equality against the Python post-#47 catalogue (18/18) and
  `msgfmt --check`.
- `docs/help/hu.md`: 8 of the 9 edited paragraphs now match
  Python post-#47. Six applied directly; two applied by hand
  onto the Rust subset's shorter paragraphs (the "by default"
  wording in the Scheduled Recordings paragraph, and the two
  guide-import series-scan sentences appended to the matching
  bullet, whose surrounding Rust wording is the subset's own).
  The ninth edited paragraph — the Recording Space Monitor
  paragraph — is not carried in the Rust help subset in any
  language, so there is no Rust text to polish; nothing was
  added.

No code changes; no persisted data shapes change. Shipped as
v0.4.42.

## Gate

Recorded 2026-11-27 on the final tree: `cargo fmt --all --check`
clean; `cargo clippy --locked --workspace --all-targets -- -D warnings`
clean; `cargo test --locked --workspace` 565 passed / 0 failed;
playlist parity PASS against the pinned Python checkout
(a29ad9a3); `git diff --check` clean; `msgfmt --check` clean on
all 13 catalogues; hu.po parsed values match Python post-#47
for all 18 revised strings.

One test fix rides in this release: the first gate run flaked
`providers::tests::resolution_results_carry_their_request_id`
(left [41, 43, 42], right [41, 42, 43]). The test still asserted
the pre-#46 single-lane dispatch-order arrival; the PR #46
two-lane design lets lanes complete out of order and callers
rely on the request-id tag instead. The test now asserts the
id set, with the comment recording why; it passed 5/5 repeat
runs and the full suite is green.
