# Issue #44 (parity slice 11): i18n (gettext) + User Guide

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/i18n-guide`. Python remains the shipping product and the
behavior oracle.

## Implemented behavior

- `iptv-core::i18n` ports i18n.py:
  - the 15-entry language label list and the 13 shipped catalogue
    codes, verbatim;
  - system-language detection via the POSIX environment chain
    (LANGUAGE, LC_ALL, LC_MESSAGES, LANG) with the reference's
    two-letter normalization;
  - preference coercion ("automatic"/"system"/"default"/empty →
    auto) and candidate resolution (auto resolves to the system
    language; English is the source and yields no catalogue; other
    languages chain [language, "en"]);
  - a `.mo` reader for both gettext domains (`iptvclient`,
    `release_notes`) handling both endiannesses, plural entries, and
    malformed files (empty catalogue, never a crash);
  - `ngettext` driven by the catalogue's own `Plural-Forms`
    expression, evaluated by a small recursive-descent parser for the
    full gettext plural grammar (arithmetic, comparisons, boolean
    operators, ternary) — verified against the Russian 3-form rule;
  - init-from-config through the `language` key.
- `iptv-core::guide` ports user_guide.py:
  - Markdown rendering to plain text: headings levels 1–3 with
    `{#anchor}` topics (fallback `section-N`), the title block
    offsets recorded as character positions, paragraph joining, list
    blocks with continuation lines, numbered markers preserved, HTML
    comments removed, and inline stripping (code spans, links —
    in-page anchors keep only the label, external links keep
    "label (url)" — bold, and guarded emphasis);
  - `Guide::find` / `section_at` / `topics`, language candidates
    with English fallback, and `open_topic`'s exact fallback rules
    (a missing topic in a translation opens the English guide at
    that section; the manual's start never falls back).
  - The English guide ships in-repo at `docs/help/en.md` (the Python
    product's own manual) and is embedded in the binary as the
    always-available fallback; on-disk `docs/help/<lang>.md` files
    (or `IPTV_GUIDE_DIR`) take precedence per language.
- Adapter: the language activates from config at startup
  (catalogues from `IPTV_LOCALE_DIR` or a `locale/` directory); a
  User Guide button opens the guide window in the resolved
  language — table of contents, full text with the caret moved to
  the chosen section, and Find with spoken results.

## Evidence

- 107 workspace tests pass (6 new: `.mo` parse + translate +
  pluralize, the Russian plural rule end-to-end, preference
  resolution vs the reference, guide rendering incl. anchors/lists/
  comments/inline stripping, the embedded English guide renders
  with its `user-guide` topic, open_topic fallback).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against
  pinned Python 9aa89e0, `git diff --check`.

## Evidence boundary

- UI strings are not yet routed through `gettext` calls across the
  app: the engine, activation, and catalogue compatibility are here
  (the Python app's own catalogues load unchanged); the per-string
  sweep is mechanical follow-up, and catalogue files themselves
  ship with packaging (slice 12).
- The Windows UI-language API path in `detect_system_language` is
  represented by the environment chain in this port.
- F1 as a global in-app shortcut waits on the shortcut framework in
  slice 12; the guide opens from its button here, and the guide
  window's own F1 topic (`using-help`) is reachable in the table of
  contents.
- Emphasis stripping approximates the reference regex's lookarounds
  with equivalent guard checks; pathological nested-marker text can
  differ in edge cases.
