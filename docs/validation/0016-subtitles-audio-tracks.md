# Issue #40 (parity slice 9): subtitles, speech, audio-track preferences

Status: **automated gates PASS**. Implemented 2026-11-01 on branch
`agent/subtitles`. Python remains the shipping product and the behavior
oracle.

## Implemented behavior

- `iptv-core::subtitles` ports subtitle_cues.py:
  - SRT/WebVTT block parsing with the reference's timing grammar
    (optional hours, comma or dot fractions, settings after the end
    time), WebVTT metadata blocks (WEBVTT/NOTE/STYLE/REGION) skipped,
    a timing line accepted in the first two lines of a block;
  - markup stripping that preserves unclosed fragments as literal
    text, HTML entity unescaping (named common set plus numeric
    references), and whitespace-collapsed spoken text;
  - ASS/SSA parsing through the declared `[Events]` Format (last
    field must be Text, fields unique, Start/End required), with the
    plain-text rules: override blocks dropped, drawing mode (`\pN`)
    suppressing text until `\p0`, `\N`/`\n`/`\h` as spaces;
  - malformed timestamps rejected per-cue (minutes/seconds < 60,
    fraction bounded) instead of aborting the file;
  - `load_cues`: 20 MB cap, BOM-aware decoding (UTF-32 and UTF-16
    before UTF-8), format hint from the file suffix plus content
    sniffing for ASS;
  - cue lookup: latest started cue, and the active cue with
    latest-start-wins overlap handling.
- `iptv-core::audiotracks` ports audio_tracks.py: Unicode word
  tokenization, whole-token-span keyword matching with the ≥6-char
  compound fallback (so "ad" never fires on "Radio"), the full
  audio-description keyword set, preferred keyword assembly
  (channel track, then AD set or last-manual, then user keywords,
  deduped), preferred-track selection (keyword priority, AD ties take
  the last match, position fallback, AD-preference last-described /
  last-track rule), and the active-index rule where the wanted name
  beats the reported current id.
- Adapter: the built-in player gains a Load subtitles button (SRT /
  VTT / ASS / SSA file dialog); cues are held per playback and cleared
  on channel change; the player's 1-second timer announces the active
  cue through the live region whenever it changes and the
  `speak_subtitles` config is on — the reference's opt-in spoken
  subtitle path.

## Evidence

- 97 workspace tests pass (6 new: token matching incl. the Radio/ad
  guard, selection priority + fallbacks, active index, SRT/VTT
  parsing incl. metadata skipping and markup/entity cleanup, ASS
  declared-format parsing, active-cue overlap lookup).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
  (zero warnings), `tools/check-playlist-parity.py` PASS against
  pinned Python 9aa89e0, `git diff --check`.

## Evidence boundary

- wxDragon's MediaCtrl exposes no audio/subtitle track enumeration or
  selection, so the ported selection logic cannot drive the built-in
  player yet; it lands with the libVLC endgame (recorded in the audit).
  External players receive no track flags either — Python drives those
  through its own probe/session machinery, also libVLC-shaped.
- Python's load falls back to chardet-detected encodings for non-UTF
  files without BOMs; this port decodes those as UTF-8 lossy.
  BOM-carrying files (the common case for authored subtitles) decode
  exactly.
- The TTS voice engines (subtitle_speech.py / app_tts.py) are not
  ported; speech rides the app's screen-reader announcements, which is
  how the reference's wx path surfaces cues to NVDA as well.
- Entity unescaping covers the named entities subtitle files actually
  use plus all numeric references; Python's html.unescape knows the
  full HTML5 named table.
