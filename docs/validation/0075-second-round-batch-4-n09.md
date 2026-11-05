# 0075 — Second-round batch 4: N09 Playlist view preview/commit

Date: 2026-11-04. Scope: audit finding N09 — the Playlist view
choice acted on every arrow press and forgot the scope.

## What was wrong

Python treats the Playlist view combo as preview-then-commit:
arrow keys move the combo's selection and NVDA announces each
scope, but nothing reloads until the user activates (Enter,
Tab away, or a mouse choice), and Escape abandons the preview
and restores the committed selection. Committing saves
`playlist_scope`, clears the search, resets the category to
All Channels, and the saved scope is restored at startup.
Rust committed on every `COMMAND_CHOICE_SELECTED` event —
each arrow press re-ran the search and rebuilt the lists —
and never saved the scope, so every restart began at All
playlists. For a screen-reader user arrowing through scopes,
each step cost a full list reload.

## What changed

- Core `BrowserWorker::commit_scope`: one re-run with the
  scope set, the category reset to `Category::All`, and an
  empty query. `set_scope_silent` restores a saved scope
  before the first load lands (no submit, no cancel — loads
  apply scoped).
- `bind_scope_choice` mirrors Python's handlers
  (`on_playlist_scope_choice`, `on_playlist_scope_key`): a
  left-button press arms the mouse flag and only an armed
  selection event commits; any key disarms it; Enter/Numpad
  Enter commits, Escape reverts the picker to the committed
  scope, Tab commits and lets focus move on. Arrows do
  nothing but preview.
- Commit (`commit_scope_selection`) is a no-op when the
  selection already is the committed scope; otherwise it
  saves `playlist_scope` through `set_and_save`, clears the
  search field, and commits in the browser.
- `fill_scope_choices` now selects the committed scope
  instead of always All, and startup restores the saved
  `playlist_scope` silently before the picker is filled.

## Verification

- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the
  pinned Python oracle, `git diff --check`.
- The preview/commit split is event wiring in the app shell;
  like the other shell behaviors it is verified by
  construction against Python's handlers. The check from the
  audit — arrow through scopes with no reload, press Enter,
  restart, and the scope is kept — is Brandon's hands-on pass
  on Windows.

## Incident note

During this batch the working copy of `main.rs` was found
truncated on disk (a partial write, cut mid-token at line
3354 of ~15,200). No committed work was lost: the file was
restored from HEAD (the batch-3 merge) and the batch's three
edits were re-applied and re-verified; the final diff against
HEAD contains only this batch's changes.
