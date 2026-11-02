# Issue #46 (parity slice 12): shortcuts, menus, tray + updater + packaging

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/shortcuts-updater`. Python remains the shipping product and
the behavior oracle. Final slice of parent #16.

## Implemented behavior

- `iptv-core::shortcuts` ports shortcuts.py:
  - the MAIN table (19 commands with wx command ids 4001–4026) and
    PLAYER table (16 commands), plus both label maps;
  - accelerator parsing/canonicalization for the vocabulary wx
    produces for these bindings (Ctrl/Alt/Shift modifiers, single
    characters, named keys and F-keys with wx key codes);
  - the validity law: F1 reserved for Help in every context, and
    main-window bindings require Ctrl or Alt;
  - `effective()` merging config `shortcuts` overrides with invalid
    hand-edits ignored; `set_shortcut` with the reference's exact
    error strings ("Invalid keyboard shortcut.", "Unknown
    command.", "F1 is reserved for Help.", "Main-window shortcuts
    need Ctrl or Alt.", "Shortcut conflicts with {label}."),
    default-value reset semantics, and conflict detection naming
    the other command's label;
  - `main_entries()` (flags, key, command id) and
    `searchable_commands` with the "Built-in Player — " prefix rule.
- `iptv-core::updater` ports updater.py's check path:
  - version parsing/normalization/comparison over the reference's
    anchored shape;
  - the GitHub-host rule: the environment token is attached only
    for github.com / *.github.com / *.githubusercontent.com;
  - latest-release manifest asset selection (case-insensitive) and
    `UpdateManifest` extraction with the reference's three error
    strings and the installer's three filename key spellings;
  - an `UpdateWorker` (check off the UI thread) and a SHA-256
    verified download helper returning the digest of what landed.
- Adapter: a menu bar (File → Exit carrying the effective
  `Ctrl+Q`-style label from the shortcuts engine; Help → User
  Guide, Check for Updates), menu dispatch by the MAIN command ids,
  a tray icon (tooltip, generated bitmap, Show/Exit popup menu),
  and Check for Updates announcing "Version X is available" or
  up-to-date on the status line. The guide window's open path is
  now shared between its button and the menu.
- PACKAGING.md records the release layout and the manifest
  contract.

## Evidence

- 112 workspace tests pass (5 new: accelerator parse/canonical
  forms, validity law + override ignoring + entries/search,
  set_shortcut errors/reset/conflict naming, version comparison,
  manifest extraction incl. installer spellings and error strings).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against
  pinned Python 9aa89e0, `git diff --check`.

## Evidence boundary

- The release workflow itself is NOT in this change: adding or
  editing `.github/workflows` needs Brandon's direct OK (standing
  rule). PACKAGING.md is the agreed layout the workflow will
  implement; that OK is the only missing step.
- The in-app install flow is not ported: the Python installer is a
  Windows PowerShell helper with Authenticode thumbprint pinning
  and a cross-process status protocol; this slice lands check +
  verified download only, and no release exists yet for the check
  to find (it reports the manifest-not-found failure cleanly until
  one does).
- Tray popup selections route through the same menu-id dispatch as
  the menu bar; tray behavior cannot be exercised headless on this
  VM (no system tray), so the icon install is compile-verified
  only.
- Menu accelerators beyond the items present (Record/Cast/VOD menu
  entries) ride the landed shortcuts engine as follow-up; the
  engine itself — validation, overrides, conflicts, search — is
  complete against the reference.
