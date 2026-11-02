"""Check widget-free favorites goldens using committed, pinned Python source.

Usage: python tools/check-favorites-parity.py <Python checkout> [--update]
Only --update writes goldens. Inputs are synthetic; no config or URLs are opened.
"""
from pathlib import Path
import subprocess
import sys

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
    print(f"PASS: {len(channels)} identities, 6 stored cases, {len(channels) * 2} toggle/filter steps; Python {REVISION}. Domain evidence only.")


if __name__ == "__main__":
    main()
