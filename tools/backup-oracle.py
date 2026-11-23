"""Cross-check full backups (v2) with the Python client.

Usage:
  python -I tools/backup-oracle.py <python-checkout> export <dir>
      Python writes <dir>/python.aiptv (password "pw").
  IPTV_BACKUP_ORACLE=<dir> cargo test -p iptv-core python_full_backup_oracle
      Rust imports it and writes <dir>/rust.aiptv.
  python -I tools/backup-oracle.py <python-checkout> import <dir>
      Python imports <dir>/rust.aiptv and prints what it found.
Never reads user configuration.
"""
import json
import sys
import time
import types
from pathlib import Path

checkout, mode, folder = sys.argv[1], sys.argv[2], Path(sys.argv[3])
sys.modules["i18n"] = types.SimpleNamespace(gettext=lambda s: s)
sys.path.insert(0, checkout)
import dvr  # noqa: E402
import full_backup  # noqa: E402
import programme_reminders  # noqa: E402

now = time.time()
if mode == "export":
    channel = {"name": "News", "url": "http://example.com/s", "playlist-id": "m3u:abc"}
    program = {"title": "Show", "start": time.strftime("%Y%m%d%H%M%S", time.gmtime(now + 7200)),
               "end": time.strftime("%Y%m%d%H%M%S", time.gmtime(now + 10800)), "episode": "0.4."}
    job = dvr.build_job(channel, program, "provider_mkv", repeat="weekly")
    job.update(repeat="weekdays", weekdays=[0, 3], future_padding=[1, 2])
    start = float(int(now) + 3600)
    key = programme_reminders.channel_key(channel)
    reminder = {"id": __import__("hashlib").sha256(json.dumps([key, start]).encode()).hexdigest(),
                "channel_key": key, "start": start, "end": start + 1800, "lead": 5,
                "title": "Film", "channel": "News", "status": "scheduled"}
    bundle = {"config": {"playlists": [], "epgs": []}, "jobs": [job], "reminders": [reminder],
              "search_seen": {"b" * 64: now + 600}}
    full_backup.export_bundle(str(folder / "python.aiptv"), bundle, "pw")
    print("python.aiptv written")
elif mode == "import":
    bundle = full_backup.import_bundle(str(folder / "rust.aiptv"), "pw")
    print(json.dumps({"jobs": [(j["id"], j["status"]) for j in bundle["jobs"]],
                      "reminders": len(bundle["reminders"]), "seen": len(bundle["search_seen"])}))
