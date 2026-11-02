# 0046 — Hidden-root expand assert (owner report, Windows v0.3.0)

Date: 2026-11-02.

The owner ran the v0.3.0 Windows build and got a wxWidgets
debug dialog at startup:

    treectrl.cpp(1798): assert "!IsHiddenRoot(item)" failed in
    DoExpand(): Can't expand/collapse hidden root node!

Root cause: the Categories tree is built with
TreeCtrlStyle::HideRoot, but two call sites asked wx to
expand that hidden root — category_controls when the tree is
created (so the dialog appeared at startup) and
sync_category_control after every category rebuild (so it
would have reappeared whenever the category set changed).
wxWidgets forbids expanding or collapsing a hidden root;
the MSW build asserts, and the release packages ship with
wx asserts compiled in.

Fix: both expand calls are removed. A hidden root's children
are always visible, and the tree's top level is flat
(categories are leaf items), so the calls did nothing
visually — they only produced the assert. A comment at the
first site records the rule so the call does not come back.
A workspace-wide search confirms no other expand or
collapse is aimed at a tree root.

No widget-level regression test exists for this on purpose:
the workspace's tests never instantiate wx widgets (they
need a display and a running app), which is why GUI slices
carry automated gates plus the owner's NVDA pass (#86)
instead. The regression guard here is the recorded rule in
the code comment plus this document.

Evidence class: automated — fmt clean, clippy zero
warnings (-D warnings), workspace tests pass, playlist
parity PASS. The owner re-running the Windows build without
the dialog is the closing check; it ships as v0.3.1.
