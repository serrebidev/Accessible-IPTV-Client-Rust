# 0082 — Second-round batch 10: account discovery and Copy Details (audit N17)

## Scope

- Audit finding N17 in `docs/feature-parity-audit.md`, against
  Python `account_info.py` and `dialogs_misc.py`'s
  `AccountInfoDialog` in the pinned oracle.
- The Rust Account Info dialog previously listed only
  configured provider accounts and fetched through the
  provider worker by provider id, with a manual Fetch Info
  button and no copy action.

## What changed

- Core `account_info.rs`, a port of the Python module:
  - `Account` record with `host`, `dedupe_key` (kind + host +
    username, MAC for Stalker) and the list `label`
    ("{kind} – {who}", plus "(detected)").
  - `xtream_credentials_from_url`: both panel shapes — the
    query form on a `.php` endpoint (base = everything above
    the file) and the path form (`/live/U/P/id`, `/U/P/id`,
    the six-segment `timeshift` variant), requiring a numeric
    stream id so ordinary CDN paths never become accounts.
  - `discover_accounts`: configured sources first (objects
    via `account_from_source`, plain playlist URL strings
    detected as `playlist_url`), then a channel scan keyed
    by the host-plus-two-segments prefix cache (the username
    always falls inside those segments, so accounts cannot
    merge), deduped throughout.
  - `fetch_account_report`: builds the Xtream/Stalker client
    from the record (Python's `build_client`) and formats
    with the existing `providers::format_account_report`,
    inserting the `Detected from` header line for detected
    accounts.
- App (the dialog, Python's flow):
  - File > Account Info runs discovery on a worker thread
    (it walks every loaded channel URL); the result is
    presented only while the playlist pointer it was
    discovered from is still loaded — a stale discovery is
    dropped silently, exactly like Python's channel-identity
    and load-token guard.
  - No accounts: Python's "No Accounts" information box,
    verbatim. Discovery failure: Python's "Could not look
    for provider accounts: …" error box.
  - The window auto-selects the first account and starts its
    lookup; selecting another account looks it up; reports
    are cached per account, errors are not (re-selecting
    retries); Refresh forces a fetch. A request token retires
    stale lookups. Status and details texts are Python's:
    "Checking account, please wait...", "Checking {label}...",
    "Ready.", "Check failed.", "Could not check this
    account.\n\n{error}"; an applied report resets the
    details insertion point to the top.
  - Copy Details writes the details text to the clipboard
    and reports "Account details copied to the clipboard."
    (or "Could not open the clipboard.") on the dialog's
    status line.
  - The dialog's fetches no longer ride the provider worker,
    so the old `account_text` routing field is gone; the
    provider worker's account path remains for the DVR
    connection-limit checks that use it.

## Divergence

- The window is the house modeless list-dialog frame rather
  than a modal dialog, matching every other Rust list window
  (recorded pattern from earlier slices).

## Evidence

- Core tests (5 new): query-form and path-form credential
  extraction including subfolder bases and CDN negatives,
  discovery ordering/dedupe/prefix behavior with detected
  flags, Stalker MAC identity and labels, and the
  `Detected from` header placement.
- Gates: `cargo fmt --all` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test
  --workspace` (378 tests), playlist parity check PASS,
  `git diff --check` clean.

## Residuals

- Brandon's NVDA pass on the dialog (list focus on open,
  status announcements) remains his, as with the other
  windows.
- No live provider account was available to this session, so
  the fetch path is covered by the formatter's existing core
  tests plus the discovery tests here, not an end-to-end
  panel query.
