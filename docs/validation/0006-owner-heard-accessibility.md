# Issue #6 (ticket 05): owner-heard accessibility acceptance

Status: **PENDING - owner session required**. No manual criterion has passed.
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
No native tree inspection, keyboard automation or audible validation ran.
The existing `tools/smoke-foundation.ps1` has not been run in this session.
This is an environment limitation, not an observed product accessibility defect.

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

- Tester: Brandon Bracey; session date/time: pending.
- Application revision: pending confirmation of the revision above.
- Executable path and SHA-256: pending build identification.
- Windows edition/build: pending.
- NVDA version, voice and relevant speech settings: pending.
- Display scale, text size and contrast settings used: pending.
- Overall result: pending. Owner acceptance statement: not supplied.

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

For every row, replace Pending with Pass, Fail or Blocked and add what Brandon
actually heard and did. Record speech omissions, duplicates and interruptions,
not just visible text. A blocked case is never a pass.

| Case | Keyboard action and pass condition | Observed result |
| --- | --- | --- |
| Initial names and traversal | On launch, NVDA identifies Channels as an empty list. Tab/Shift+Tab reaches Open playlist, Cancel loading, Search channels, Channels and read-only Playlist status in logical cyclic order. All names/roles are clear; no trap or fake channel row. | Pending |
| Open and rows | Use Alt+O, enter the absolute `local.m3u` path and confirm without a mouse. Hear loading/completion; reach Channels with Tab or Enter from Search. Arrows/Home/End read four real rows, including the unnamed entry's URL fallback. No unchanged rows are repeatedly announced by background work. | Pending |
| Search and selection | Type `ABC` in Search; hear the result change and find ABC East HD. Type rapidly and replace the query; only current results survive. Caret and focus stay in Search. Enter moves to Channels; Shift+Tab returns to Search. Selected channel remains selected when still in the results. | Pending |
| Clear and no results | Clear Search with Ctrl+A/Backspace to restore four rows. Search `zzzz-no-match`; hear zero-result recovery guidance and find no selectable placeholder. Clear again; rows return without a background focus jump. | Pending |
| Empty and malformed | Open `empty.m3u`; hear empty/recovery guidance once, with zero rows and reachable status. Open `recovery.m3u`; six usable rows remain browsable, malformed guidance is spoken, and search/clear still works. | Pending |
| Failure and retry | Attempt a missing/unreadable synthetic source after a successful load. If the app receives the failure, hear useful choose/retry guidance without raw paths/errors, retain previous rows, then use Open playlist to recover. A chooser-only rejection does not pass app recovery. | Pending |
| Cancel and replace | Start the large load; Alt+C cancels. Repeat while opening `local.m3u`, and while typing a search over the previously loaded playlist. No late rows/status from superseded work, no focus theft. Cancel a file chooser during loading; the existing operation continues. Record any race that could not be exercised. | Pending |
| Large-list responsiveness | Load 500,000 rows; navigate, resize and type while work proceeds. Measure observed load/search intervals and longest input pause; record machine/fixture details and Brandon's usability judgment. Home/End reach real rows; rapid search/clear remains usable; closing during work does not hang. Correctness tests alone cannot pass this case. | Pending |
| System presentation | Under the owner's chosen large-text/high-contrast settings, controls and status remain readable, reachable and usable. Record settings actually tested; leave other combinations untested. | Pending |

## Failure and follow-up record

No manual defects have been observed because the session has not run. For each
failure, append: case ID, revision, fixture, starting focus/state, exact keys,
expected speech/focus, actual speech/focus, reproducibility, severity and follow-up
issue URL. Keep credentials, private source paths and personal URLs out of notes.
Prepare follow-up issue text locally; publishing it requires user authorization.
Retest the same steps after a fix and retain the original failure evidence.

Issue #6 remains open until the owner session results are recorded separately
from automated checks and every failed criterion has a reproducible note and
follow-up ticket. Unexercised criteria remain explicit blockers to acceptance.
