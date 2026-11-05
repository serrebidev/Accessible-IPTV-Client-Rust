# 0078 — Second-round batch 7: N14 provider errors in plain words

Date: 2026-11-04. Scope: audit finding N14 — Python turns an
ffmpeg exit code into a sentence saying what happened and
what to do; Rust reported bare numbers.

## What was wrong

ffmpeg exits with the `AVERROR` of whatever stopped it, so a
provider refusal surfaced as "code 3436169992" (Windows'
unsigned spelling of the HTTP 403 error tag). Python's
`_FFMPEG_EXIT_REASONS` table in `catchup_urls.py` maps the
common codes — 401, 403, 404, generic 4xx, 5xx, invalid
data, connection timeouts and refusals in their Windows CRT,
Linux and Winsock numberings — to plain sentences. Rust had
exactly one of them (the 403 text, as the one-stream
refusal); a failed catch-up download just marked its row
"Failed".

## What changed

- Core `catchup::ffmpeg_exit_reason` ports the table and the
  lookup, including Python's unsigned-32-bit wrap for codes
  reported the Windows way. Every code was checked against
  the pinned oracle's own answers, and a test pins the 403
  sentence to `catchup_direct::ONE_STREAM_REFUSAL` so the
  two copies cannot drift.
- App: when a catch-up download fails for good (retries
  spent, not cancelled), `finish_staged_download` now shows
  Python's failure box (title "Catch-up Download"): with a
  known code, "Download failed:" + the final path + the
  plain-words reason + the failure detail (the last six
  ffmpeg tail lines, or the N12 failure detail when the tail
  is empty); with an unknown code, Python's
  "Download failed (code N):" form.

## Verification

- Core test covers every code in Python's table, the
  unsigned wrap (3436169992 → the 403 sentence), and
  unknown codes (0, 1, −1) returning None.
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the
  pinned Python oracle, `git diff --check`.
- The dialog is shell wiring, verified by construction
  against main.py's download-finish handler; hearing it on
  a real provider refusal is Brandon's hands-on pass.

## Residuals

- Python also puts the reason at the head of the Downloads
  window's failure detail (`show_failure`); the Rust
  downloads window is the row list from audit R06, and its
  per-download detail view is N15.
- Python's retry rule (up to 3 attempts, doubling delay,
  only for retryable failures matched against the ffmpeg
  tail) and the kept-partial "Download ended early" report
  remain divergences of the download flow, also N15.
- The box shows immediately, like the N12 recording
  reports; queueing behind an open modal is N18.
