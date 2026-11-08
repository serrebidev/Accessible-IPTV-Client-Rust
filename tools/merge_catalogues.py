#!/usr/bin/env python3
"""Merge the two gettext domains into patois's flat po/ layout.

The catalogues in this repo came from the Python client, which keeps
two domains per language: `iptvclient` (the app) and `release_notes`
(What's New bullets). patois compiles one domain per app from flat
`po/<lang>.po` files, so each language's two files are merged with
msgcat (first file wins on a duplicate msgid).

Usage: python3 tools/merge_catalogues.py [locale-tree]

`locale-tree` defaults to `locale/` and must hold
`<lang>/LC_MESSAGES/{iptvclient,release_notes}.po`. Output goes to
`crates/iptv-app/po/<lang>.po` for every non-English language found.
Requires the gettext tools (msgcat) on PATH.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LANGS = ["es", "ar", "pt", "fr", "de", "ru", "tr", "it", "pl", "hi", "zh", "ja", "hu"]


def main() -> int:
    tree = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "locale"
    out_dir = ROOT / "crates" / "iptv-app" / "po"
    out_dir.mkdir(parents=True, exist_ok=True)
    for lang in LANGS:
        src_dir = tree / lang / "LC_MESSAGES"
        app_po = src_dir / "iptvclient.po"
        notes_po = src_dir / "release_notes.po"
        if not app_po.exists():
            print(f"{lang}: no iptvclient.po under {src_dir}, skipped")
            continue
        out = out_dir / f"{lang}.po"
        cmd = ["msgcat", "--use-first", "-o", str(out), str(app_po)]
        if notes_po.exists():
            cmd.append(str(notes_po))
        subprocess.run(cmd, check=True)
        print(f"{lang}: wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
