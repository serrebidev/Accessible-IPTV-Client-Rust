# Issue #3: local playlist browse seam

Branch: `agent/ticket-3`, based on foundation merge `a244eaa`.
The implementation and this record are committed together; use that commit
as the validated revision. No GitHub issue state was changed.

## Implemented

- `BrowseModel::open_local` reads a local UTF-8 M3U/M3U Plus playlist without
  desktop dependencies and returns ordered channel records.
- Common identity, name, group, logo and stream metadata remain available;
  unknown attributes are retained. Quoted commas, single-quoted attributes,
  case-insensitive directives, whitespace and BOM handling follow the pinned
  Python parser's common field behavior.
- A native Open playlist button has an explicit accessible name and Alt+O
  mnemonic. A native file picker supplies the path. The native Channels
  virtual list exposes row names and single selection; unnamed entries get
  an explicit fallback label rather than exposing stream credentials.
- File I/O, parsing and retirement of a previous large model run on workers.
  A UI-thread timer only publishes the completed result; it stops afterward.
  Only one load runs at a time. Completion never calls `set_focus`.
- Successful replacement clears old native row/selection state and selects
  the first new row. Cancel preserves current state; read failures preserve
  existing rows and expose a status message.

Fixture provenance and the deliberate logo/unknown-attribute retention
extension are recorded in `crates/iptv-core/tests/fixtures/README.md`.

## Automated local gate, 2026-11-01

Rust 1.97.1; pinned dependencies unchanged. Reused the foundation build cache:
`$env:CARGO_TARGET_DIR = (Resolve-Path ../../target).Path` from this worktree.
The initial isolated check was interrupted while compiling native dependencies;
the checks below use the current worktree sources and the shared target cache.

| Command | Exit | Evidence |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Formatting clean. |
| `cargo test --workspace -j 2` | 0 | Nine tests: four core, three local-file seam, two adapter; zero doctests. |
| `cargo clippy --workspace --all-targets -j 2 -- -D warnings` | 0 | Both crates clean. |
| `cargo build -p iptv-app -j 2` | 0 | Native Windows executable linked. |
| `git diff --check` | 0 | No whitespace errors. |

Changed files: core and adapter implementations, local browse integration
tests and fixture/provenance, README and this evidence record. No dependency,
lockfile, workflow, Python checkout or user configuration changes.

## Desktop and owner-heard gate: pending

Computer Use failed before launch with `Computer Use native pipe is
unavailable ... The system cannot find the file specified. (os error 2)`.
A retry and JS kernel reset produced the same error. No native accessibility
inspection or keyboard interaction was completed. Source and adapter unit
tests do not prove Windows accessible names or audible output.

Run the app with NVDA, recording the commit, Windows/NVDA versions and observed
speech for each pass/fail:

| Pattern | Pass condition | Failure example |
| --- | --- | --- |
| File open | Tab/Shift+Tab or Alt+O reaches named Open playlist; picker can be completed and cancelled by keyboard. | Unnamed button, mouse required, or cancellation changes rows. |
| Populated list | Open the synthetic fixture; Channels exposes four rows in source order with the expected display names and native selected state. | Blank names, quoted metadata in row text, wrong order or phantom rows. |
| Selection | Tab to Channels; arrows, Home and End select and speak the intended row. | Focus trap, stale selection or incorrect row speech. |
| Replacement | Open the fixture again; four rows remain, old selection is cleared, first new row selected; completion does not move control focus. | Appended rows, out-of-range selection or focus pulled away. |
| Responsiveness | A large local playlist loads while the window remains operable; resizing and closing do not wait for parsing. | Frozen UI or blocking shutdown. |
| Native semantics | Inspect MSAA for button/list names, focusability, row names and selected state. | Source labels exist but native names/state do not. |
| Presentation | Labels and controls remain usable with system large text/high contrast. | Clipping or unreadable controls. |

Search is ticket 03. Live announcements, richer malformed/empty/error recovery,
cancel/replacement generations and the full parity suite are ticket 04.
No 500,000-channel timing budget or owner-heard acceptance is claimed here.
