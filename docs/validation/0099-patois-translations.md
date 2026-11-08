# 0099 — Translations on patois (v0.4.23)

Brandon's order (2026-11-07): "I want to use this for
translations" — patois, Quin Gillespie's gettext crates
for Rust desktop apps (accessware/patois 0.3), the library
that translates Paperback. Followed by "commit and release".

## Before

Translations ran on a hand-rolled gettext in iptv-core:
its own `.mo` parser and Plural-Forms expression evaluator
(~500 lines), reading compiled catalogues from a `locale/`
folder that release staging copied beside the executable.
Two domains per language, ported byte-identical from the
Python client: `iptvclient` (797 strings) and
`release_notes` (What's New bullets).

## The migration

- patois 0.3.0 (runtime, `wx` feature) + patois-build
  0.1.18 (build-dependency). Both crates marked
  `[package.metadata.patois] translatable = true`.
- The two domains are merged per language with msgcat by
  the committed `tools/merge_catalogues.py` into
  `crates/iptv-app/po/<lang>.po` — 13 languages, 801
  entries each. The app build script calls
  `patois_build::compile_translations("po", "locale")`;
  `patois::embed_domain!()` embeds the compiled catalogues
  in the binary. The repo-root `locale/` tree is deleted
  and release staging no longer copies one.
- wx stock strings: `embed_wx_translations!()` plus a
  `WxTranslationManager` in a OnceLock, initialized as the
  first statement inside the wx main closure (before any
  UI) and re-synced at both language-apply points (config
  init and the language menu), so standard dialog strings
  follow the app's language.
- iptv-core's `I18n` keeps its exact public API and the
  Python preference rules as a facade over patois:
  `gettext`/`notes_gettext` → `patois::t`, `ngettext` →
  `patois::nt`. One subtlety the new test caught:
  `patois::t` resolves against the *active domain*, which
  nothing sets in a test binary — the facade calls
  `set_default_domain(DOMAIN)` itself in `set_language`,
  so lookups never depend on the wx manager having run.
- iptv-core gained a build.rs with the app crate's Linux
  `gstplayer-1.0` link line: workspace feature unification
  pulls patois/wx (and wxdragon-sys) into core's test
  binaries, which otherwise fail to link.
- String extraction going forward: `examples/gen_pot.rs`
  (`cargo run -p iptv-app --example gen_pot`) regenerates
  the template from both translatable crates.
- Tests: the `.mo`-parser and plural-evaluator tests went
  with the code they tested. New coverage: a main.rs test
  proves the embedded German catalogue resolves both an
  app string and a release-notes-only string through the
  facade (and that English restores); core tests assert
  the po bundle's size and known translation pairs, and
  that the facade is a safe English passthrough with no
  catalogues embedded.

## CI lesson (why the first release run failed)

patois-build compiles catalogues with the system `msgfmt`
and only *warns* when it is missing — a build without
gettext tools succeeds and silently ships no translations.
Both workflows therefore install gettext. The first
v0.4.23 Release run (42428172756) proved the point: Linux
and macOS went green, but on Windows choco's gettext
package had not put `msgfmt.exe` on the PATH later steps
see, no catalogues were compiled, and the new embedded-
catalogue test failed the Windows Gates — the test doing
exactly its job. PR #213 rewrote the Windows step to
locate msgfmt.exe, publish its folder on GITHUB_PATH, and
fail the job if the binary cannot run. The tag had never
published (Publish was skipped; no release object
existed), so it was moved to the fix merge.

## Release

PR #212 merged as 2c4b9255; PR #213 (the Windows msgfmt
fix) merged as d5164685. Local origin/main and the API
branch head verified equal at each step. Tag v0.4.23 on
d5164685. Release run 42430459500 completed success —
all three platform builds and Publish. Verified by API:
v0.4.23 published 2026-11-08T00:55:57Z, draft=false,
Latest, exactly 7 assets (zip/tar.gz/dmg, each with
.minisig, plus update-manifest.json), and its body is
byte-identical to the CHANGELOG.md 0.4.23 section.

Local gates on the final tree: cargo fmt clean, clippy
-D warnings clean, cargo test --workspace 394 passed /
0 failed, playlist parity PASS against the pinned Python
oracle, git diff --check clean. (The mid-work VM restart
wiped the sandbox's apt packages; deps were restored via
the azure sidestep plus the libzstd pin fix before the
final gates ran.)

## Still Brandon's

A non-English NVDA pass: pick German in the language
menu, restart, and confirm menus/dialogs speak German —
and that a built-in dialog (for example a file picker)
shows its stock buttons in German too.
