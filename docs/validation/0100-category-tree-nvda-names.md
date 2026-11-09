# 0100 — Category names spoken again in the Categories tree (v0.4.24)

Brandon's report (2026-11-08): with an NVDA debug log
(12:54:27–12:54:34 PST) showing tabbing into the Categories
tree and arrowing through three levels. NVDA spoke only
"Categories, tree view, selected, N of M, level L" for every
item — never a category name — and logged
`accRole failed: (-2147024809, 'The parameter is incorrect.')`
(E_INVALIDARG) on essentially every navigation. His order:
"Categories are read wrong. Brainstorm, and come up with a
fix. Then commit and release."

## Root cause

`category_controls` built the tree with only
`TreeCtrlStyle::HideRoot` and named it with wxdragon's
`set_accessibility_label("Categories")`.

That call reaches wxWidgets 3.3's
`wxWindowBase::SetAccessibleName`. The wx source (3.3.4,
`src/common/wincmn.cpp`) says in its own comment that native
controls have no accessible object by default "but we need
one for our name to be used", so it calls
`GetOrCreateAccessible()`. The default
`wxWindow::CreateAccessible()` returns nullptr
(`include/wx/window.h`) and wxTreeCtrl does not override it,
so wx installs a plain `wxAccessible` — every method in
`include/wx/access.h` returns NOT_IMPLEMENTED. That stub
shadows the native SysTreeView32 MSAA that serves tree item
names and roles. Result, matching the log exactly: the
control name is announced for every item, item roles fail
with E_INVALIDARG, and only the structure (position, level)
leaks through from the native control underneath.

The item text was never missing: `sync_category_control`
appends items with real text, and wxdragon's `append_item` /
`add_root` wrappers pass text correctly into a genuine
`wxTreeCtrl`.

Python A/B proof: the Python client's `_AccessibleCategoryTree`
does the same-looking thing (`SetName`, plus a
`SetAccessibleName` guarded by `hasattr`) — but wxPython
4.2.1 (wxWidgets 3.2) has no `SetAccessibleName` on any
window class (verified: `hasattr` is False for wx.Window,
wx.Control, wx.TreeCtrl, wx.ListCtrl, wx.ListBox), so the
call is silently skipped and the Python tree keeps pure
native MSAA. That is why the Python tree reads correctly
and the Rust tree did not.

Scope decision: the same override call sits on other item
controls (main channel ListCtrl, VOD groups/items lists,
cast devices list, a Recording Problem Times list). Those
read fine in Brandon's daily use — the listview's standard
delegation evidently survives where the treeview's does
not — so they were left untouched rather than risk
regressing working controls. Rule recorded for all the
wx 3.3 apps: never put an accessible-name/description
override on a native item control (tree/list); childless
controls are fine.

## Fix (PR #215, merged d0e77c89)

- Style is now `HideRoot | HasButtons | LinesAtRoot` —
  Python parity (`TR_HIDE_ROOT | TR_HAS_BUTTONS |
  TR_LINES_AT_ROOT | TR_SINGLE`).
- `set_accessibility_label` removed; the tree takes its
  name via `set_name("Categories")`, the way the Python
  client effectively does, with a comment in the code
  explaining the wx 3.3 trap.
- Regression test `category_tree_keeps_native_item_accessibility`:
  source scan of `category_controls` asserting the
  `set_name("Categories")` call is present and neither
  override string appears.

Gates, all green: `cargo fmt --check`, clippy
`--workspace --all-targets -D warnings`, `cargo test
--workspace` 395 passed / 0 failed (394 prior + the new
regression test), playlist parity PASS against the pinned
Python clone.

## Release state

The GitHub Release run for v0.4.24 (42524669771) was cut
off mid-run by the suspension of the serrebidev GitHub
account on 2026-11-08; the tag v0.4.24 itself had already
landed at d0e77c89 and was never re-tagged. v0.4.24 was
published instead on the project forge (Forgejo,
git.serrebiradio.com): Linux tar.gz built on the VPS at
d0e77c89 with libVLC bundled, `.minisig` signature verified
valid against the compiled-in `minisign.pub`, plus
`update-manifest.json` and `SHA256SUMS`.

v0.4.25 superseded it the same day (Brandon's commits
b4261aa + 96098a7, Forgejo only): the release signing key
rotated to the per-product IPTV key (32F554A2744755F4) and
the updater moved from GitHub to the Forgejo API
(`https://git.serrebiradio.com/api/v1`). v0.4.25 is
published on Forgejo with the Windows package; installs on
0.4.24 or older need one manual install because both the
update source and the signing key changed.

## Acceptance

Brandon's NVDA pass on the Categories tree in v0.4.24+:
arrowing through the tree should speak each category name
with its position and level. Outstanding at the time of
writing.
