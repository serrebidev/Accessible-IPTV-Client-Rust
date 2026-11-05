# 0083 — Second-round batch 11: notifications queue behind modal dialogs (audit N18)

## Scope

- Audit finding N18 in `docs/feature-parity-audit.md`, against
  Python `modal_guard.py` and `main.py:4056`–`4118` in the
  pinned oracle.

## What changed

- App-side modal tracking in `main.rs`:
  - A thread-local modal depth plus a FIFO queue of deferred
    notifications (cap 8, dropped beyond — Python's
    `_DEFERRED_NOTIFICATION_LIMIT`) mirror `modal_guard.py`'s
    module-global design: every modal in the app counts, so a
    notification can never open inside another box's message
    loop and desynchronize wxMSW's parent-enable bookkeeping
    (the failure that left the main frame "unavailable" to
    NVDA in Python).
  - `ModalRun::show_modal_tracked` wraps `show_modal` for
    every dialog type the app uses (MessageDialog, Dialog,
    FileDialog, TextEntryDialog, SingleChoiceDialog,
    DirDialog); all 69 modal call sites in `main.rs` go
    through it, including the shared `slider_dialog` helper,
    so custom dialogs count too. When the depth returns to
    zero the queue drains (Python's closed-hook via
    `wx.CallAfter`; here the drain chain runs inline, bounded
    by the cap).
  - `show_or_queue_notification(parent, message, caption,
    style, on_dismiss)` is Python's
    `_show_or_queue_message_box`: show now (with the
    dismissal callback after) or queue behind the open modal.
- Notification sources converted: the four recording-outcome
  reports (N12) and the catch-up download outcomes — complete
  (N15), ended-early (N15), and the failure explanation (N14)
  — all of which arrive from worker polls while any modal may
  be up.

## Evidence

- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (378 tests), playlist parity check PASS,
  `git diff --check` clean.
- The queue/depth machinery is thread-local GUI state; it is
  exercised structurally by the tracked call sites rather
  than unit tests (wx handles cannot be built headless).

## Residuals

- ship-shape's update UI runs on its own thread with its own
  windows (since v0.4.0); the depth counter cannot see inside
  it, so an app notification could still land while the
  updater window is up. Python's updater was in-process, so
  its guard applied. Noted, not fixable from this crate.
- Python's deferred notes also cover its EPG-import-finished
  and failed/missed scheduled-recording boxes; the Rust app
  reports both of those on the status line today (no boxes
  exist to queue). The import-finished message itself is
  N21's scope.
- Catch-up's "download canceled, incomplete file discarded"
  box (Python) has no Rust counterpart yet; cancels here ask
  confirmation first (N15) and close silently.
