# Validation 0024: GUI parity G5 — Playlist Manager + EPG Manager

Date: 2026-11-01. Issues: #59 and #51 (program #54).

## Landed

- File > Playlist Manager is live: list of the `playlists` config
  entries with Add File, Add URL, Add Xtream Codes, Add Stalker
  Portal (account forms writing the same dict shape
  ProviderAccount::from_config reads, with a generated stable id
  for scope tagging), Edit (text entry for locations, the account
  form for accounts), and Remove. OK writes the config, refills
  the Playlist view scope picker, and reloads sources/accounts;
  Cancel discards.
- File > EPG Manager is live: Add URL / Add File / Remove over
  `epg_urls`; OK saves and reloads the guide.
- Startup loading now shares reload_sources / reload_epg with the
  managers.
- The main window's Open playlist and Cancel loading buttons are
  retired — the Playlist Manager owns loading now, as in Python.
  The Record button remains until the player window gains its own
  (G4); recording is also on the channel context menu.

## Automated evidence

- cargo fmt --check, clippy zero warnings, 113 workspace tests
  pass, playlist parity PASS, git diff --check clean.

## Owner-heard evidence still owed

NVDA pass over both managers and the account forms (field labels,
password field behavior, OK/Cancel flow). Compile-verified here.
