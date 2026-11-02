"""Check shared goldens against the exact Python reference without importing its UI.
Usage: python tools/check-playlist-parity.py <Python checkout> [--update]
Only --update writes fixture goldens. Never reads user configuration or fetches URLs.
"""
import ast
from pathlib import Path
import re
import subprocess
import sys
from typing import Dict, List

REVISION = "9aa89e053225ef2c83c4f3659775a00723012ff7"
FIXTURES = Path(__file__).resolve().parents[1] / "crates/iptv-core/tests/fixtures"
SOURCES = ("local.m3u", "recovery.m3u", "empty.m3u", "invalid-utf8.m3u", "categories.m3u")
QUERIES = ("", "c", " CAT ", "news", "missing", "\u6620\u753b")


def main():
    reference = Path(sys.argv[1]).resolve()
    assert subprocess.check_output(["git", "-C", str(reference), "rev-parse", "HEAD"], text=True).strip() == REVISION, "Wrong Python reference revision"
    # Read committed bytes, so a dirty Python checkout cannot alter the oracle.
    def committed(name):
        return subprocess.check_output(["git", "-C", str(reference), "show", f"{REVISION}:{name}"]).decode("utf-8-sig")

    def node(name, symbol):
        return next(n for n in ast.walk(ast.parse(committed(name))) if isinstance(n, ast.FunctionDef) and n.name == symbol)

    namespace = {"re": re, "Dict": Dict, "List": List}
    headers = ast.parse(committed("playlist_headers.py"))
    pattern = next(n for n in headers.body if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "_M3U_ATTR_RE" for t in n.targets))
    # media_type is widget/config independent; execute committed source in its own scope.
    media = type(sys)("parity_media_type")
    exec(compile(committed("media_type.py"), "media_type.py", "exec"), media.__dict__)
    namespace["media_type"] = media
    nodes = [pattern, node("playlist_headers.py", "_extinf_name_comma"), node("client_m3u.py", "_parse_m3u_return"), node("main.py", "search_rank")]
    exec(compile(ast.Module(body=nodes, type_ignores=[]), "pinned-playlist-oracle", "exec"), namespace)
    probe = type("Probe", (), {"_extract_stream_id": staticmethod(lambda url: "")})()
    channels_output = [f"# Python {REVISION}; source, name, group, url, tvg-id, tvg-name"]
    search_output = [f"# Python {REVISION}; source, query, ordered source indexes"]
    # Extract actual grouping fallback, UI ordering and projection from main.py.
    main_tree = ast.parse(committed("main.py"))
    grouping = next(n.value for n in ast.walk(main_tree) if isinstance(n, ast.Assign)
                    and any(isinstance(t, ast.Name) and t.id == "grp" for t in n.targets)
                    and ast.unparse(n.value) == "ch.get('group') or 'Uncategorized'")
    ordering = next(n.iter for n in ast.walk(node("main.py", "_refresh_group_ui"))
                    if isinstance(n, ast.For) and ast.unparse(n.iter) == "sorted(scoped_groups)")
    exec(compile(ast.Module(body=[node("main.py", "_source_for_group")], type_ignores=[]), "pinned-category-projection", "exec"), namespace)
    namespace["favorites"] = type("Favorites", (), {"FAVORITES_GROUP": "Favorites"})
    category_output = [f"# Python {REVISION}; source, kind, key, ordered source indexes"]
    outcomes = [f"# Python {REVISION}; source, outcome"]
    for source in SOURCES:
        # Same decoding policy as main.py local source loading, with universal newlines.
        text = (FIXTURES / source).read_bytes().decode("utf-8-sig", errors="ignore")
        channels = namespace["_parse_m3u_return"](probe, text)
        groups = {}
        for channel in channels:
            group = eval(compile(ast.Expression(grouping), "pinned-group-key", "eval"), {}, {"ch": channel})
            groups.setdefault(group, []).append(channel)
        ordered = eval(compile(ast.Expression(ordering), "pinned-category-order", "eval"), {}, {"scoped_groups": groups})
        scoped = type("Scoped", (), {"scoped_all_channels": lambda self: channels, "scoped_channels_by_group": lambda self: groups})()
        for kind, key in [("all", "All Channels")] + [("group", group) for group in ordered]:
            # Scope-aware reference keys distinguish literal groups from sentinels.
            source_key = ("playlist-group", "", key) if kind == "group" else key
            projected = namespace["_source_for_group"](scoped, source_key)
            indexes = [i for i, channel in enumerate(channels) if any(channel is entry for entry in projected)]
            category_output.append(f"{source}\t{kind}\t{key}\t{','.join(map(str, indexes))}\t.")
        outcomes.append(f"{source}\t{'loaded' if channels else 'empty'}")
        for channel in channels:
            fields = [source] + [channel.get(key, "") for key in ("name", "group", "url", "tvg-id", "tvg-name")]
            assert all("\t" not in f and "\n" not in f for f in fields)
            channels_output.append("\t".join(fields + ["."]))
        for query in QUERIES:
            normalized = query.strip().lower()
            rows = [i for i, ch in enumerate(channels) if normalized in ch["name"].lower()]
            if normalized:
                rows.sort(key=lambda i: namespace["search_rank"](channels[i]["name"], normalized))
            search_output.append(f"{source}\t{query}\t{','.join(map(str, rows))}\t.")
    for name, lines in (("python-channels.tsv", channels_output), ("python-search.tsv", search_output), ("python-outcomes.tsv", outcomes), ("python-categories.tsv", category_output)):
        expected = "\n".join(lines) + "\n"
        path = FIXTURES / name
        if "--update" in sys.argv[2:]:
            path.write_text(expected, encoding="utf-8", newline="\n")
        else:
            assert path.read_text(encoding="utf-8") == expected, f"Parity mismatch: {name}"
    print(f"PASS: five shared fixtures, parsed fields/order, 30 searches, category projections and load/empty outcomes; Python {REVISION}. Domain evidence only.")


if __name__ == "__main__":
    main()
