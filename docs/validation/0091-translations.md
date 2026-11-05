# 0091 — Translations: Python catalogues and guides bundled

## Scope

- The translations item in Brandon's "complete everything"
  scope, recorded in `PACKAGING.md` (release staging ships no
  gettext catalogues; `docs/help` is English only) and in the
  audit's "previously documented limits" (translation
  resources), against the pinned Python oracle
  (`~/workspace/pyclone-pinned`, commit
  9aa89e053225ef2c83c4f3659775a00723012ff7).

## Oracle inventory

- Gettext catalogues (`locale/<lang>/LC_MESSAGES/`): 14
  language directories — ar, de, en, es, fr, hi, hu, it, ja,
  pl, pt, ru, tr, zh — each holding compiled `iptvclient.mo`
  and `release_notes.mo` beside their `.po` sources, plus the
  `iptvclient.pot` / `release_notes.pot` templates and the
  two `identical_ok.json` files at the tree root. The 13
  non-English languages match Python's `SHIPPED_CATALOGS`
  exactly; en ships too because Python's gettext chain uses
  it as the final fallback. Every `iptvclient.mo` carries
  the full 797-string catalogue (798 entries including the
  header). The `release_notes` catalogues are deliberately
  sparse — most bullets are untranslated, which is Python's
  recorded design (the notes domain may lag a release) — and
  hu's is header-only. The `.mo` files are already compiled,
  so no msgfmt step was needed.
- Python runtime discovery (`i18n.py`): the `locale`
  directory beside the app (frozen bundle or source tree),
  standard gettext layout, languages resolved as
  `[resolved, "en"]`.
- Help guides (`docs/help/<lang>.md`): one file per language
  for all 14 languages. `docs/help-sync.json` records the
  English revision each translation was synced to; the
  translations lag the current English text by design.
  Python's `user_guide.load_guide` tries
  `<lang>.md` then `en.md` in `docs/help` beside the app.
- The Rust guide loader already implements exactly that
  behaviour (`crates/iptv-core/src/guide.rs`: `guide_path`,
  `language_candidates`, exe-relative `guide_dir`), and the
  app already opens topics in the resolved language. Only
  the files were missing.

## What shipped

- `locale/` at the repo root: the oracle's catalogue tree,
  byte-identical (`diff -rq` clean) — all 14 languages, both
  domains, `.po` sources and templates included, so future
  translation updates land the same way they do in Python.
- `docs/help/`: the 13 translated guides (ar, de, es, fr,
  hi, hu, it, ja, pl, pt, ru, tr, zh) copied byte-identical
  from the oracle. The existing Rust `en.md` stays: it is
  newer than the oracle's English file (it documents the
  six-hour playlist cache and the finished-load message).
- Release staging (`.github/workflows/release.yml`, all
  three Package steps): the `locale` directory is now copied
  beside the executable, next to `docs/help`, which staging
  already copied recursively — so the translated guides
  needed no workflow change. The archives now carry
  `locale/<lang>/LC_MESSAGES/{iptvclient,release_notes}.mo`
  at the root, exactly where `iptv_core::i18n::locale_dir()`
  probes (exe-relative, then `IPTV_LOCALE_DIR`, then the
  working directory).

## Tests

- `i18n::tests::bundled_catalogues_cover_shipped_languages`:
  every `SHIPPED_CATALOGS` language loads a non-empty main
  catalogue from the bundled tree and its notes catalogue
  parses; spot checks resolve real strings (de "Cancel" →
  "Abbrechen", hu "User Guide" → "Felhasználói
  útmutató").
- `guide::tests::bundled_guides_cover_pythons_languages`:
  `available_languages` on the bundled `docs/help` lists 14
  codes with English first, and each language's guide loads
  in that language with sections.
- Full gates: fmt, clippy `-D warnings`, workspace tests
  (380 passed, verified in an isolated target dir — the
  shared one is contended by parallel lanes), playlist
  parity, `git diff --check`.

## Residuals

- The translations are the oracle's, synced to the English
  revision recorded in `docs/help-sync.json`; strings added
  since fall back to English at runtime, as they do in
  Python. Re-syncing translation content is translator work,
  not part of this mechanical port.
- Brandon's NVDA pass in a non-English language (menu labels
  end-to-end, guide window in-language) remains his
  acceptance step, as with the other parity work; the S06
  finding (some app strings bypass the catalogue) is a
  separate audit item and is not closed by shipping the
  files.
