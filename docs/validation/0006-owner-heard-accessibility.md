# Issue #6 (ticket 05): owner-heard accessibility acceptance

Status: **PASS - owner session reported complete**. All nine manual criteria passed.
Prepared 2026-11-01 on branch `agent/ticket-6` for application revision
`60cd621d7cff013da895b463e083b38d84381ea8` (playlist recovery integrated).
This record adds documentation only; it does not close issue #6 or the
replacement gate. Python remains the shipping product.

## Evidence boundary

The [ticket 04 record](0005-playlist-recovery.md) reports 25 passing workspace
tests, formatting, Clippy, native build and pinned Python parity checks. Those
are prior automated results, not rerun for this documentation-only slice and
not evidence of NVDA speech, keyboard traversal or practical UI responsiveness.

Computer Use was retried on 2026-11-01. Inventory returned no native apps and:
`Computer Use native pipe is unavailable: failed to connect native pipe: The system cannot find the file specified. (os error 2)`.
The owner nevertheless completed the NVDA and keyboard session manually and
reported all nine cases passed with no exceptions. The corrected
`tools/smoke-foundation.ps1` also passed its native MSAA checks locally.

The comparison reference is Python revision
`9aa89e053225ef2c83c4f3659775a00723012ff7`. Its
`tools/smoke_accessible_names_nvda.py` was read from committed source: it checks
adjacent label/control association and actual MSAA names for Search and
Channels. It does not itself prove audible speech. The Rust pass below applies
that native-name expectation alongside the search/focus and recovery criteria
from [the spec](../specs/0001-rust-port-foundation.md) and tickets 03/04.
Out-of-slice Python dialogs are not part of this local browse pass.

## Owner session identification

Complete these fields before recording results. If the application changes,
record the new revision and rerun the affected manual cases.

- Tester: Brandon Bracey; session date/time: 2026-11-01, exact time not supplied.
- Application revision: `0e20a37cb1bb870358ca3ecc463e499a65fa20d4`.
- Executable path: `target\debug\iptv-app.exe`; SHA-256:
  `9FC0BE8441FAEA183B3742BD0DC6542583522B6E4169A1CC0ADC8FFB7B860BFB`.
- Windows edition/build: not supplied.
- NVDA version, voice and relevant speech settings: not supplied.
- Display scale, text size and contrast settings used: not supplied.
- Overall result: **Pass**. Owner reported all nine cases passed with no exceptions.

## Preparation

Use an interactive Windows desktop with Brandon's usual NVDA settings. Do not
change screen-reader or system settings on his behalf. Use only synthetic local
playlists; no provider credentials, personal playlists or stream connections are
needed. The slice browses entries; playback is outside this gate.

From this checkout, build the pinned application with the documented prerequisites:

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
$env:PATH = 'C:\Program Files\CMake\bin;' + $env:PATH
$env:CARGO_TARGET_DIR = (Resolve-Path ..\..\target).Path
cargo build -p iptv-app -j 2
git rev-parse HEAD
Get-FileHash (Join-Path $env:CARGO_TARGET_DIR 'debug\iptv-app.exe') -Algorithm SHA256
cargo run -p iptv-app -j 2
```

Record the build exit code and hash. A shared target directory can be overwritten
by another worktree; build immediately before the session and avoid concurrent
application builds. Optional native inspection is a separate automated result:
run `powershell -NoProfile -File tools/smoke-foundation.ps1` after closing the
manual instance. Record its exit/output separately; it cannot pass the rows below.

Fixtures are under `crates/iptv-core/tests/fixtures`: `local.m3u` (four rows),
`empty.m3u` (zero rows), `recovery.m3u` (six usable rows with malformed guidance).
For a large source, create a synthetic file in a tester-chosen scratch directory
without overwriting an existing file. Stream 500,000 entries to disk before the
session; names `Channel 000000` through `Channel 499999`, each followed by
`https://example.invalid/<number>`, with an `#EXTM3U` header and an `#EXTINF:-1,`
prefix on each name. Record the path, row count and size used. File creation time
is not application load latency. Do not delete or alter real files to force an
error: type a nonexistent scratch-file path in the chooser. If the chooser
rejects it before the app receives it, record that limitation; app-level missing
or unreadable recovery remains untested until a safe fixture can reach it.

## Hands-on pass

For every row, record Pass, Fail or Blocked and add what Brandon
actually heard and did. Record speech omissions, duplicates and interruptions,
not just visible text. A blocked case is never a pass.

| Case | Keyboard action and pass condition | Observed result |
| --- | --- | --- |
| Initial names and traversal | On launch, NVDA identifies Channels as an empty list. Tab/Shift+Tab reaches Open playlist, Cancel loading, Search channels, Channels and read-only Playlist status in logical cyclic order. All names/roles are clear; no trap or fake channel row. | Pass - owner reported no exceptions. |
| Open and rows | Use Alt+O, enter the absolute `local.m3u` path and confirm without a mouse. Hear loading/completion; reach Channels with Tab or Enter from Search. Arrows/Home/End read four real rows, including the privacy-safe `Unnamed channel` fallback. No unchanged rows are repeatedly announced by background work. | Pass - owner reported no exceptions. |
| Search and selection | Type `ABC` in Search; hear the result change and find ABC East HD. Type rapidly and replace the query; only current results survive. Caret and focus stay in Search. Enter moves to Channels; Shift+Tab returns to Search. Selected channel remains selected when still in the results. | Pass - owner reported no exceptions. |
| Clear and no results | Clear Search with Ctrl+A/Backspace to restore four rows. Search `zzzz-no-match`; hear zero-result recovery guidance and find no selectable placeholder. Clear again; rows return without a background focus jump. | Pass - owner reported no exceptions. |
| Empty and malformed | Open `empty.m3u`; hear empty/recovery guidance once, with zero rows and reachable status. Open `recovery.m3u`; six usable rows remain browsable, malformed guidance is spoken, and search/clear still works. | Pass - owner reported no exceptions. |
| Failure and retry | Attempt a missing/unreadable synthetic source after a successful load. If the app receives the failure, hear useful choose/retry guidance without raw paths/errors, retain previous rows, then use Open playlist to recover. A chooser-only rejection does not pass app recovery. | Pass - owner reported no exceptions. |
| Cancel and replace | Start the large load; Alt+C cancels. Repeat while opening `local.m3u`, and while typing a search over the previously loaded playlist. No late rows/status from superseded work, no focus theft. Cancel a file chooser during loading; the existing operation continues. Record any race that could not be exercised. | Pass - owner reported no exceptions. |
| Large-list responsiveness | Load 500,000 rows; navigate, resize and type while work proceeds. Measure observed load/search intervals and longest input pause; record machine/fixture details and Brandon's usability judgment. Home/End reach real rows; rapid search/clear remains usable; closing during work does not hang. Correctness tests alone cannot pass this case. | Pass - owner reported no exceptions; timing and machine details not supplied. |
| System presentation | Under the owner's chosen large-text/high-contrast settings, controls and status remain readable, reachable and usable. Record settings actually tested; leave other combinations untested. | Pass - owner reported no exceptions; settings not supplied. |

## Failure and follow-up record

No manual defects were reported in the completed owner session. For each
failure, append: case ID, revision, fixture, starting focus/state, exact keys,
expected speech/focus, actual speech/focus, reproducibility, severity and follow-up
issue URL. Keep credentials, private source paths and personal URLs out of notes.
Prepare follow-up issue text locally; publishing it requires user authorization.
Retest the same steps after a fix and retain the original failure evidence.

Issue #6 may now be considered manually accepted based on the owner's report.
The missing environment metadata should be filled in if the session is rerun;
the recorded pass is limited to the reported session and does not claim testing
of other Windows, NVDA, display, or speech configurations.
