# Issue #2: toolkit foundation evidence

Scope: the empty desktop shell, not playlist loading, row updates, playback,
packaging acceptance, or Python replacement. Upstream issue #2 and the local
ticket `.scratch/rust-port-foundation/issues/01-toolkit-and-workspace-foundation.md`
share the acceptance criteria. No GitHub issue state is changed here.

## Automated acceptance

- Rust toolchain and workspace MSRV both specify stable release 1.97.1.
- `iptv-core` has no dependencies and can be tested without wxWidgets.
- Only `iptv-app` depends on wxDragon; `Cargo.lock` remains committed.
- The adapter uses a native virtual list, zero rows, a Channels label and
  accessible name, and a description of the empty state. Its row callback
  returns no text for invalid/stale indexes, covered by an adapter unit test.
- Native panel/sizers replace fixed control dimensions. Only startup sets
  focus; no timer, worker, modal dialog, or synthetic selection moves it.
- The toolkit comparison and selected/rejected options are in ADR 0003.

Unit tests do not inspect the native accessibility tree. The Windows smoke
script reads the list's MSAA client object directly, following the Python
reference's acceptance model; UI Automation only locates the native window,
list handle and static empty-state text. A coarse UI Automation provider that
reports generic panes is not treated as evidence about MSAA names or roles.

## Verified local gate, 2026-11-01

Revision: `9baeec2` plus the foundation changes in this commit, on
`agent/ticket-2`. Toolchain: `rustc 1.97.1 (8bab26f4f 2026-07-14)`;
CMake 4.4.4 on PATH; `LIBCLANG_PATH=C:\Program Files\LLVM\bin`.

| Command | Exit | Observed result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | Formatting clean. |
| `cargo test --workspace` | 0 | Five unit tests passed (one adapter, four core); zero doctests. |
| `cargo check --workspace` | 0 | Both crates checked with the native build prerequisites available. |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | No unresolved Rust warnings. |
| `cargo build -p iptv-app` | 0 | Runnable Windows executable linked against wxDragon/wxWidgets. |
| `powershell -NoProfile -File tools/smoke-foundation.ps1` | 0 | MSAA name Channels, list role, focusable, initial focus true, zero native rows, empty-state text/description; normal shutdown. |
| `git diff --check` | 0 | No whitespace errors. |

MSAA's child count includes the native report header even with `LC_NO_HEADER`;
the smoke test separately asserts zero native channel rows and no MSAA children
beyond that header. It does not equate all accessible children with rows.

The baseline Clippy check found a redundant `continue` in the core parser.
The equivalent negative-comment condition removes the warning without changing
parsing behavior. The first shell compilation also caught an unsupported
`ListCtrl::on_size` call; the final adapter binds the existing safe event API
because wxDragon 0.9.24 lacks `WindowEvents` on that widget.

Changed scope: `Cargo.toml`, `rust-toolchain.toml`, `README.md`, the desktop
adapter, the core's comment-line branch, ADR 0003, this evidence record, and
`tools/smoke-foundation.ps1`. No dependency, lockfile, workflow, Python-reference,
or GitHub issue changes. Release packaging and owner-heard acceptance are not
claimed by these local checks.

## Manual Windows / NVDA gate: pending

Run `cargo run -p iptv-app` with NVDA running. Record Windows version, NVDA
version, Rust commit, observed speech, and pass/fail for each check:

| Pattern | Pass condition | Failure example |
| --- | --- | --- |
| Initial focus and name | Keyboard focus starts on the list; NVDA identifies Channels and its list role. | Bare unnamed list or focus left on an unreachable frame. |
| Empty list | Inspection shows zero channel items; the empty-state text is discoverable by NVDA. | A selectable No playlist loaded channel or an unexplained blank area. |
| Keyboard operation | Tab/Shift+Tab retain a reachable list; arrows/Home/End in the empty list do not invent a selection or crash; Alt+F4 closes normally. | Mouse required, trapped focus, phantom row, or crash. |
| Stable focus | Waiting, resizing, and switching away/back do not force focus elsewhere or repeatedly speak unchanged rows. | Background work resets focus or repeats the list. |
| System presentation | Resize and test system large text/high contrast; labels and empty state remain available. | Clipped controls or hard-coded colors hide content. |
| Accessibility inspection | Use Accessibility Insights/Inspect (MSAA/UIA) to check the native list name, role, keyboard-focusable state and zero channel items. | Explicit source label exists but Windows exposes no accessible name. |

The startup static text/description is not a live-region implementation. Dynamic
load/search/status announcements belong to later tickets and need their own
owner-heard validation. Populated-list navigation and unchanged-row announcement
behavior cannot be accepted from an empty-list prototype.
