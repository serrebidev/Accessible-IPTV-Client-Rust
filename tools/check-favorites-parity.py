"""Check widget-free favorites goldens using committed, pinned Python source.

Usage: python tools/check-favorites-parity.py <Python checkout> [--update]
Only --update writes goldens. Inputs are synthetic; no config or URLs are opened.
"""
import ast
from pathlib import Path
import re
import subprocess
import sys
from typing import Dict, List

REVISION = "9aa89e053225ef2c83c4f3659775a00723012ff7"
FIXTURES = Path(__file__).resolve().parents[1] / "crates/iptv-core/tests/fixtures"


def records(name):
    for line in (FIXTURES / name).read_text(encoding="utf-8").splitlines():
        if not line.startswith("#"):
            yield line.split("\t")[:-1]


def main():
    reference = Path(sys.argv[1]).resolve()
    # Committed bytes make the oracle independent of any local Python edits.
    source = subprocess.check_output(
        ["git", "-C", str(reference), "show", f"{REVISION}:favorites.py"]
    ).decode("utf-8-sig")
    oracle = {}
    exec(compile(source, "pinned-favorites.py", "exec"), oracle)
    rows = iter(records("favorite-channels.tsv"))
    fields = next(rows)[1:]
    channels = []
    labels = []
    output = [f"# Python {REVISION}; favorites.py observable outcomes"]
    for row in rows:
        label, *values = row
        # Empty strings model missing optional fields at Rust's typed boundary.
        channel = dict(zip(fields, values))
        channels.append(channel)
        labels.append(label)
        output.append(f"key\t{label}\t{oracle['channel_key'](channel)}\t.")
    for label, container, *values in records("favorite-stored.tsv"):
        invalid = {"null": None, "int:7": 7, "bool:true": True, "map": {}, "list": []}
        decoded = [v[2:] if v.startswith("s:") else invalid[v] for v in values]
        stored = decoded if container == "list" else {"null": None, "string": "a,b", "bytes": b"a,b"}[container]
        output.append("\t".join(["normalize", label, *oracle["normalize"](stored), "."]))
    keys = ["gone"]
    # Two passes cover add/remove, namesakes, escaped keys, and unkeyable rows.
    for index in list(range(len(channels))) * 2:
        channel = channels[index]
        keys, added = oracle["toggle"](keys, channel)
        outcome = "unkeyable" if not oracle["channel_key"](channel) else "added" if added else "removed"
        output.append("\t".join(["toggle", labels[index], outcome, *keys, "."]))
        filtered = oracle["filter_channels"](channels, keys)
        found = [labels[i] for i, ch in enumerate(channels) if any(ch is item for item in filtered)]
        output.append("\t".join(["filter", labels[index], ",".join(found), "."]))
    expected = "\n".join(output) + "\n"
    path = FIXTURES / "python-favorites.tsv"
    if "--update" in sys.argv[2:]:
        path.write_text(expected, encoding="utf-8", newline="\n")
    else:
        assert path.read_text(encoding="utf-8") == expected, "Favorites parity mismatch"

    def committed(name):
        return subprocess.check_output(["git", "-C", str(reference), "show", f"{REVISION}:{name}"]).decode("utf-8-sig")

    def node(name, symbol):
        return next(n for n in ast.walk(ast.parse(committed(name))) if isinstance(n, ast.FunctionDef) and n.name == symbol)

    # Execute the reference's parser and Favorites category routing, not a copy.
    media = type(sys)("parity_media_type")
    exec(compile(committed("media_type.py"), "media_type.py", "exec"), media.__dict__)
    namespace = {"re": re, "Dict": Dict, "List": List, "media_type": media,
                 "favorites": type("Favorites", (), oracle)}
    headers = ast.parse(committed("playlist_headers.py"))
    pattern = next(n for n in headers.body if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "_M3U_ATTR_RE" for t in n.targets))
    nodes = [pattern, node("playlist_headers.py", "_extinf_name_comma"),
             node("client_m3u.py", "_parse_m3u_return"), node("main.py", "_favorite_channels"),
             node("main.py", "_source_for_group")]
    exec(compile(ast.Module(body=nodes, type_ignores=[]), "pinned-favorite-projection", "exec"), namespace)
    parser = type("Parser", (), {"_extract_stream_id": staticmethod(lambda url: "")})()

    def playlist(name):
        return namespace["_parse_m3u_return"](parser, (FIXTURES / name).read_text(encoding="utf-8-sig"))

    projections = [f"# Python {REVISION}; case, ordered source indexes"]
    for label, source_name, seed_name, seed_rows in records("favorite-projection-cases.tsv"):
        source_channels, seeds = playlist(source_name), playlist(seed_name)
        keys = []
        for index in filter(None, seed_rows.split(",")):
            keys, _ = oracle["toggle"](keys, seeds[int(index)])
        scoped = type("Scoped", (), {
            "_favorites_cache": None, "favorite_keys": keys,
            "scoped_all_channels": lambda self: source_channels,
            "_favorite_channels": namespace["_favorite_channels"],
        })()
        projected = namespace["_source_for_group"](scoped, oracle["FAVORITES_GROUP"])
        indexes = [str(i) for i, ch in enumerate(source_channels) if any(ch is item for item in projected)]
        projections.append(f"{label}\t{','.join(indexes)}\t.")
    projection_path = FIXTURES / "python-favorite-projections.tsv"
    projection_expected = "\n".join(projections) + "\n"
    if "--update" in sys.argv[2:]:
        projection_path.write_text(projection_expected, encoding="utf-8", newline="\n")
    else:
        assert projection_path.read_text(encoding="utf-8") == projection_expected, "Favorite category parity mismatch"
    print(f"PASS: {len(channels)} identities, 6 stored cases, {len(channels) * 2} toggle/filter steps; Python {REVISION}. Domain evidence only.")
    print(f"PASS: {len(projections) - 1} Favorites category projections through pinned main.py routing.")


if __name__ == "__main__":
    main()
