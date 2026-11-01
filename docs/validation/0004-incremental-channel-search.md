# Issue #4: incremental channel search

Branch: `agent/ticket-4`, based on `3757e95` (merged local browse seam).
Implementation and this record are committed together; that commit identifies
this gate. No GitHub issue state was changed.

## Implemented behavior

- Native Search channels field, visible mnemonic label, explicit accessible
  name and instructions; keyboard order Open playlist, Search channels, Channels.
  Enter explicitly moves to Channels; Tab/Shift+Tab use native traversal.
- Every text edit queues a shared immutable playlist snapshot. One worker
  serializes scans, discards obsolete queued requests and checks generation
  every 256 channels and before/after ranking. Completed results are checked
  again on publication, including old results queued before a new keystroke.
- Case-insensitive substring matching and Python ranking: exact, word start,
  then general match; shorter names first, source order for ties. Clear restores
  source order; no matches has zero native rows and clear/change-search guidance.
- Selected and focused item identities survive filtering, reordering and clear
  when present; missing identities fall back to the first row, or none when empty.
  Identity is the source index within the shared playlist, including duplicate
  names/URLs. Replacing the playlist resets identity. This is local browsing
  identity, not the future provider-stable favorites identity.
- Worker-built reverse indexes keep selection/focus restoration constant-time
  on the UI thread. File reads, parsing, scanning, ranking, index construction
  and retirement of old snapshots run off the UI thread.
- Native active state is cleared before an identity changes or goes out of
  range. Shrink publishes native count before swapping the backing rows; grow
  swaps rows before native count, so synchronous MSAA row queries remain valid.
  Unchanged active rows do not cycle native selected/focused state. Completion
  never calls keyboard focus APIs or rewrites search text/caret state.

Python reference inspected locally at
`9aa89e053225ef2c83c4f3659775a00723012ff7`:
`main.py::search_rank`, `_search_channel_rows`, `apply_filter`,
`_on_filter_char_hook`, `_focus_after_filter`, and
`channel_widgets.py::_VirtualChannelList`.
The shared Rust ranking helper previously inspected only the first substring
occurrence for word starts and used UTF-8 byte length. Both now match Python's
any-word-start test and Unicode character length; regression cases include a
later word-start match and a multibyte channel name.

## Automated local gate, 2026-11-01

Rust 1.97.1; existing pinned dependencies and Cargo.lock unchanged.
Environment: `CARGO_TARGET_DIR` resolved from `../../target` in this worktree;
`LIBCLANG_PATH=C:\Program Files\LLVM\bin`.
Baseline `cargo test --workspace -j 2`: exit 0, nine tests before changes.

| Final command | Exit | Evidence |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Formatting clean. |
| `cargo test --workspace -j 2` | 0 | 15 tests, zero failures; zero doctests. |
| `cargo clippy --workspace --all-targets -j 2 -- -D warnings` | 0 | Both crates and tests clean. |
| `cargo build -p iptv-app -j 2` | 0 | Native executable linked. |
| `git diff --check` | 0 | No whitespace errors. |

Tests cover incremental queries, case, rank/ties, Unicode length, empty/clear,
duplicate identities, selection/item-focus restoration, unchanged-row signals,
checkpoint cancellation, already-queued stale completion suppression, a full
500,000-result search and supersession by a different playlist/query. Adapter
checks retain name-only rows, unnamed-channel fallback and out-of-range safety.
The large fixture is correctness evidence, not a measured UI latency budget.
Initial clippy findings in newly added code were corrected before the final gate.

Changed scope: core search and shared ranking, desktop search integration and
adapter checks, README and this record. No dependencies, workflows, Python
sources, user configuration or release metadata changed. Final commit is the
validated slice; working tree is clean after committing these files.

## Native and owner-heard checks: pending

Computer Use initialization succeeded, but `sky.list_apps()` failed before
launch with `Computer Use native pipe is unavailable: failed to connect native
pipe: The system cannot find the file specified. (os error 2)`. Kernel reset
and one retry failed identically. No UI automation or native inspection was
completed. Unit tests prove row/focus signals, not actual focus events or speech.

Run the linked executable with NVDA and keyboard, recording revision and versions:

| Pattern | Pass condition | Failure example |
| --- | --- | --- |
| Naming/order | Open playlist, Search channels and Channels are named and reachable in order, including reverse Tab. | Unnamed edit or keyboard trap. |
| Incremental typing | Type mixed-case partial names; results update while caret stays in Search. | Frozen window, stale results or lost caret. |
| Ranking | Exact names precede word-start and general matches, stable source-order ties. | Event slots precede exact network name. |
| Selection/focus | Select a channel, refine/clear the query, then Tab or Enter to Channels; surviving identity stays selected and focused. | Same row index points at a different channel or unexpected focus jump. |
| Re-entry | Start a large search, Tab to list then Shift+Tab back and type again before completion. | Old completion steals focus or replaces newer results. |
| No results/clear | No-match state has no stale rows, exposes recovery text; clearing restores all rows. | Phantom MSAA rows or unclear empty state. |
| Unchanged rows | Refinement with the same active channel at the same index avoids redundant row speech. | Unchanged active row repeatedly re-announced. |
| Large playlist | Load/search 500,000 channels while navigating/resizing/closing. | UI stalls or blocked shutdown. |
| System presentation | Large text/high contrast keep the search label, edit and status usable. | Clipping or hard-coded unreadable colors. |

Dynamic spoken status via a live region and richer load recovery/cancellation
remain ticket 04, as recorded in the previous slice. This ticket exposes search
status text and list descriptions; it does not claim live announcements or
owner-heard NVDA acceptance. Python remains the shipping product.
