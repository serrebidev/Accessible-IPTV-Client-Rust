# 0066 — Locale-independent free-space gate

Date: 2026-11-03. Branch: `fix/windows-free-space-locale`. Follow-up
to R08, recorded in `docs/feature-parity-audit.md` and
`docs/validation/batch-recording.md`.

## What was wrong

R08 added a pre-recording free-space gate on every platform. Its
Windows reading shelled out to `cmd /C dir /-C` and parsed the
English text `bytes free` with comma thousands separators. Windows
localizes both that text and the separators, so the parse returned
no reading and `check_destination` refused every recording with
"Free space could not be determined". The Unix `df` path had the
same class of problem: it depended on command output rather than a
filesystem API.

Python reads `shutil.disk_usage(target).free`, which is
locale-independent. Rust now does the same through `fs4`.

## Change

- `recording::free_space_bytes` calls `fs4::available_space` and
  returns a user-facing error when the filesystem cannot be read.
- Deleted `parse_df_kp_avail`, `parse_dir_free_bytes`, and the
  `df`/`dir` invocations, with their fixture tests.
- Reserve semantics are unchanged: 0–65536 MiB accepted, anything
  else falls back to 256 MiB, unknown free space still refuses.
- New workspace dependency: `fs4` 1.1.0 (MIT OR Apache-2.0), used
  for this call only. It asks the platform directly: Windows
  `GetDiskFreeSpaceExW`, Unix `statvfs`, through safe wrappers.
- `README.md` no longer lists Windows enforcement as a gap.

## Evidence

- Tests: `free_space_bytes_reads_a_real_destination`,
  `free_space_bytes_reports_query_failure`, plus the existing
  `check_destination_*` tests.
- `cargo test -p iptv-core` passed on Windows: 268 unit tests and
  all integration tests passed. CI is Ubuntu-only
  (`.github/workflows/pull-request.yml`), so CI does not exercise
  the Windows path.

## Limits

- The localized-Windows defect is argued from the removed parser
  and Python's oracle, not reproduced on a non-English Windows
  install.
- A free-space reading is a pre-start floor, not a quota
  reservation; other processes can consume space afterwards, as
  Python documents.
- No measurement of quotas or network filesystems.
