# Validation 0031: owner-heard replacement acceptance gate

Issue: #81. This is the manual acceptance protocol for the Rust replacement
gate. It is not a claim that this session has been heard or passed by the owner.

## Evidence boundary

Source review, automated tests, MSAA/UIA inspection, and hierarchy dumps may
support a row, but they cannot pass a row that requires NVDA speech, keyboard
operation, focus recovery, or practical timing. The tester must record what was
heard and which keys were used. A blocked or unrun row is not a pass.

The session must use the owner’s normal NVDA configuration without changing
screen-reader, display, contrast, or other accessibility settings. Use only
sanitized local fixtures; do not enter credentials or private URLs.

## Session identity

Record these fields before testing:

- tester and session date/time;
- application revision, executable path, and SHA-256;
- Windows edition/build, NVDA version, voice, and relevant speech settings;
- display scale, text size, contrast/high-contrast state, and reduced-motion
  state actually used;
- fixture names and any VM or hardware identity.

## Manual cases

For each case record `Pass`, `Fail`, `Blocked`, or `Not run`, plus observed
speech/focus and a follow-up issue URL when applicable.

| Case | Required path | Pass condition |
| --- | --- | --- |
| Browse and focus | Launch, Tab/Shift+Tab, Search, Channels, status, Open, Cancel | Names and roles are clear; order is logical; no focus theft or trap. |
| Playlist recovery | Open local, empty, malformed, and missing synthetic fixtures | Status and recovery guidance are spoken once; usable rows remain usable. |
| Search | Type, replace, clear, no-match, Enter and Shift+Tab | Current results only; focus and selection remain coherent; no duplicate speech storm. |
| Playback controls | Start, pause/resume, stop, seek, volume, track choice where available | Controls are named and keyboard-operable; state changes are announced. |
| Recording/DVR | Start/stop manual recording and inspect scheduled-recording outcomes | Start, success, failure, retry, cancellation, and stale-result outcomes are understandable. |
| Error recovery | Remove/disable optional media dependency or use the documented missing-dependency path | Failure is actionable, focus remains recoverable, and the app does not falsely claim success. |
| Casting | Open cast window, refresh, choose, Play, Stop | Device/status controls are named; unavailable renderer is reported as unavailable. |
| Shutdown/close | Close during idle and background work | The window closes without a hang, stray focus jump, or misleading completion announcement. |
| Presentation | Repeat a representative path under the chosen presentation settings | Text and controls remain readable, reachable, and usable; record untested combinations. |

## Failure record

For every failure append: case ID, revision, fixture, starting focus/state, exact
keys, expected speech/focus, actual speech/focus, reproducibility, severity, and
follow-up issue URL. Keep credentials, private source paths, and personal URLs
out of the record.

## Result rule

The owner-heard gate is complete only when every in-scope row is `Pass` or has a
separate explicitly accepted waiver. Automated evidence remains linked beside
the manual result, never substituted for it.
