# Issue #5 (ticket 04): playlist recovery and parity evidence

Branch: `agent/ticket-5`, based on `83c9ac0` (incremental search integration).
Implementation and this record are committed together; that local commit identifies
the validated slice. GitHub issue state and remotes are unchanged.

## Implemented behavior

- A wx-free `BrowserWorker` coordinates load and search generations. File reads,
  UTF-8 decoding, parsing, ranking and retirement of replaced snapshots run on
  workers. Cancellation checks occur between 64 KiB reads and every 256 parser
  lines. An already blocked OS read cannot be interrupted, but never blocks the
  UI. A later load waits for that read to return before its worker can proceed.
- The current playlist stays usable until a complete replacement result is ready
  for publication. Cancel loading also invalidates a replacement's pending search
  after parsing has finished. Failures, cancellation and superseded completions
  cannot replace current rows or publish stale status. Closing invalidates both
  lanes without joining workers on the UI thread.
- Open playlist replaces a pending load; cancelling its native file chooser leaves
  the active operation alone. Alt+C/Cancel loading cancels the load. Typing in Search
  supersedes a pending load and filters the last published playlist. Search text and
  caret remain untouched by completion.
- Missing/unreadable states retain previous channels and give Open playlist
  choose/retry guidance. Errors do not expose source paths, stream URLs or raw
  exception messages. Retrying uses the same chooser and worker path.
- Empty playlists publish zero rows, empty guidance and a live-region announcement.
  A named, read-only Playlist status edit is reachable after Channels, so recovery
  is reviewable from the keyboard without placeholder channel rows. Tab order is
  Open playlist, Cancel loading, Search channels, Channels, Playlist status;
  Enter in Search explicitly moves to Channels. Background work never sets keyboard
  focus. Native traversal and audible output still require the checks below.
- `live-region = 0.3.3` is pinned in workspace dependencies and Cargo.lock. Its
  medium-priority API announces load, completion, empty, malformed and recovery
  messages through the wxDragon status label. Intermediate keystrokes update text
  without announcing each scan. No dependency/toolchain baseline was upgraded.
- The parser retains usable channels around overwritten/dangling or malformed
  EXTINF metadata and reports malformed entries in UI-neutral result status.
  CR-only/Unicode line boundaries and invalid UTF-8 local-file bytes now follow
  the pinned Python reference. Unknown attributes and logos remain retained.

## Pinned Python parity

Python reference: `serrebidev/Accessible-IPTV-Client`, revision
`9aa89e053225ef2c83c4f3659775a00723012ff7`.

`tools/check-playlist-parity.py` reads committed source from that exact revision,
extracts the real parser, regex, separator and ranking function using AST, and
checks shared goldens. It imports no desktop UI, reads no user configuration,
and fetches no stream URLs. Fixture details and regeneration instructions are
in `crates/iptv-core/tests/fixtures/README.md`. `.gitattributes` preserves raw
M3U bytes across checkouts, including BOM, CR-only and invalid UTF-8 cases.

Four shared fixtures cover five common channel fields and source order,
24 searches including empty/clear, mixed case, Unicode, ties and no matches,
and loaded/empty outcomes. Rust tests consume the same goldens. Logos and
unknown attributes are extra Rust retention requirements; they are not claimed
as identical Python output. Missing configured sources are skipped by Python's
load path; exceptions become load-result errors. Explicit missing/unreadable
messages, choose/retry guidance and malformed warnings implement this ticket's
recovery policy. These strings are not byte-identical Python UI parity.

## Automated gate, 2026-11-01

Rust 1.97.1; wxDragon 0.9.24. Session-only environment:
`CARGO_TARGET_DIR` is the absolute path resolved from `../../target`,
`LIBCLANG_PATH=C:\Program Files\LLVM\bin`,
`C:\Program Files\CMake\bin` prepended to PATH. No global configuration changed.

Baseline `cargo test --workspace -j 2`: exit 0, 15 tests.

| Final command | Exit | Evidence |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Formatting clean. |
| `cargo test --workspace -j 2` | 0 | 25 tests: 23 domain tests and two adapter row-label tests; zero failures. |
| `cargo clippy --workspace --all-targets -j 2 -- -D warnings` | 0 | Workspace and all test targets clean. |
| `cargo build -p iptv-app -j 2` | 0 | Native executable linked. |
| `python tools/check-playlist-parity.py C:/Users/farhan/Documents/Codex/Accessible-IPTV-Client` | 0 | Four fixtures, shared fields/order, 24 queries, loaded/empty outcomes match committed Python. |
| PowerShell AST parse of `tools/smoke-foundation.ps1` | 0 | Syntax only; no native inspection or speech claim. |
| `git diff --cached --check` | 0 | Staged whitespace clean. |

Worker/controller tests additionally cover missing and unreadable paths, retry,
prior-channel retention, queued stale successes/errors, parsing checkpoints,
load/search supersession, and cancellation between parsing and publication.
The existing 500,000-channel test remains correctness evidence, not UI latency
measurement. The unreadable-source test opens a directory; permission-denied
classification follows the same branch but no filesystem ACLs were changed.

During development an initial unreadable fixture used a child of a regular file,
which Windows reports as NotFound; the fixture was corrected to an existing
unreadable-as-file directory. CMake was installed but absent from PATH; a
session PATH correction resolved the native build. New-code Clippy findings
were fixed before the final gate. No test was weakened or skipped.

Changed scope: core parsing/loading/browse/search coordination, wxDragon adapter,
shared fixture bytes/goldens, parity and native smoke tools, dependency lock,
README and this evidence. No workflows, Python source, user data, global settings
or release metadata changed. Working tree is clean after the local slice commit.

## Desktop accessibility and owner-heard gate: pending

Computer Use initialization returned:
`Computer Use native pipe is unavailable: failed to connect native pipe: The system
cannot find the file specified. (os error 2)`.
No native accessibility-tree or keyboard automation ran. The updated smoke tool
checks names/focusability/read-only empty status when run on an interactive
desktop; here only its syntax was checked. Adapter unit tests guard row labels
and bounds. Neither proves native traversal, announcement delivery or speech.

Run the built executable with Brandon's NVDA and keyboard, recording revision,
Windows/NVDA versions, results and any defects:

| Pattern | Pass condition | Failure example |
| --- | --- | --- |
| Naming/traversal | All five controls named and reachable with Tab/Shift+Tab; status read-only and readable. | Unnamed status, trap or unexpected order. |
| Empty playlist | Open empty.m3u; zero channels, empty guidance announced once; status remains reachable. | Phantom row, silence or focus jump. |
| Missing/unreadable | Load a removed/unreadable file; prior rows retained, error announced; choose/retry works. | Lost rows, raw path speech or unusable recovery. |
| Malformed | Open recovery.m3u; six usable rows and malformed guidance, then search/clear. | Valid rows hidden, crash or stale metadata. |
| Cancel/replace | Begin a large load; cancel, open another file, or type a search. Repeat after parsing while results prepare. | Late channels/status from cancelled work. |
| File chooser cancel | Cancel chooser while another load is pending. | Active load or previous rows discarded. |
| Search/focus | Type quickly, Tab/Shift+Tab/Enter and clear; late work never moves keyboard focus. | Lost caret, stale results or redundant unchanged-row speech. |
| System presentation | Large text/high contrast keep all controls, status and recovery usable. | Clipping or unreadable hard-coded colors. |
| Large source/close | Load/search 500,000 channels while navigating/resizing/closing. | UI freeze or blocked shutdown. |

Python remains the shipping product. This record does not close the replacement
or owner-heard acceptance gate.
