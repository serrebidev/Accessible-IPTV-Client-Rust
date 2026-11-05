# 0077 — Second-round batch 6: N11 scored guide matching

Date: 2026-11-04. Scope: audit finding N11 — Rust's guide
matching knew exact ids and canonical names only; Python
scores candidates with call signs, brand families, HBO
variants, time-shift and zone markers, and regions, so
regional channels and +1 siblings matched wrongly or not at
all.

## What was wrong

`match_guide_channel` returned the first name-index bucket
hit whose numbers did not disagree. Nothing in the Rust tree
derived a playlist region, tagged guide channels with a
region, detected a zone or a time shift, extracted call
signs, or knew that HBO Family is not HBO. A playlist asking
for "Granada" found nothing (no exact bucket), and two
siblings that strip to the same name ("Sky One +1" and
"Sky One +2") were decided by bucket order.

## What changed

- New core module `guide_match` porting the Python matcher
  vocabulary: `extract_group`, `_detect_region_from_id`,
  `_derive_playlist_region` (weighted votes, earliest-vote
  tiebreak, the `us:` / "usa" / "united states" override),
  `_detect_zone`, `_detect_timeshift` (including Python's
  first-match-only quirk: an out-of-range "+25" reads as no
  shift), `extract_callsigns` with `callsign_overlap_score`,
  `_brand_key`, `_reverse_brand_lookup`, the HBO variant
  extractor and US normalizer, `tokenize_channel_name`,
  `_market_tokens_for`, and `id_number_text`. The vocabulary
  tables (group synonyms, affiliate markets, US state names,
  major US cities) are generated from the pinned Python
  oracle into `guide_match_data.rs`, not transcribed.
- `GuideIndexes` now stores a per-channel `group_tag`
  (display-name region vs id region, the id winning a
  conflict — Python's import rule) and a token index over
  display names, mirroring the token `LIKE` rows Python
  gathers its fuzzy candidates from (first five tokens).
- `match_guide_channel` keeps its exact id phase and the
  known-ids guard exactly as before. The name phase pools
  candidates from the canonical, stripped-canonical and
  token indexes, remembers Python's exact stripped-name
  hits (base 96 via the tvg-name, 92 via the name), and
  scores every pooled candidate: exact-name score with
  Python's number adjustment, the −60 tvg-id region-hint
  penalty and the −30 wrong-region penalty; fuzzy score
  with Python's gate (family alignment, call-sign overlap
  ≥ 60, a shared token, or a US market/province overlap)
  and its components (call signs 100/70, family +40 and
  brand key +10, region +18/+6/−40 with the HBO/HGTV
  wrong-region extras, zone +8/−15 and the US implicit-east
  +38, time shift +22/+9/+5/−10×Δ/−15/−8, numbers
  +16/−30/−6, HBO variant +40/+18/−10/−12, the Sky Mix
  rules, tokens up to +20). The best score wins; ties fall
  to Python's identity tiebreak (exact tvg-name before
  exact name before fuzzy), then pool order.

## Verification

- Core unit tests for every ported helper (group codes
  including the "No Limits" false-code guard and the
  "Bravo.PEACOCK" region misfire, zones, time shifts,
  call signs, brands, HBO variants, tokens, US markets).
- Matcher tests: the +1/+2 sibling pair decided by channel
  numbers, the bare regional name "Granada" reaching
  "ITV Granada" through the scored token pool while plain
  "ITV" still lands on ITV, and an HBO West/East pair where
  the zone score prefers the matching feed. The pre-existing
  matcher tests (BBC Two/Three number guard, tvg-id
  variants) still pass.
- Full gates green locally: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, playlist parity against the
  pinned Python oracle, `git diff --check`.

## Residuals

- Python's `resolve_best_channel_id` prefers, among
  near-ties, a candidate with schedule data from now; the
  Rust indexes carry no programme data at match time, so
  ties use the identity tiebreak and pool order only.
- Python's candidate rows also include a same-region sample
  when no name or token row matches; such rows can only win
  through the US-local gate, which the token pool covers
  whenever a token is shared. A channel sharing no token,
  no brand and no call sign with any guide name matches in
  neither client in practice.
- The expanded-tvg-id phase still returns before the region
  filter Python applies to expanded ids in
  `get_matching_channel_ids`; an expanded id hit names the
  channel's own guide id modulo case and suffixes, and the
  first-round audit accepted the exact phase as-is.
- Live-guide judgement (a real provider playlist against a
  real guide) is Brandon's hands-on pass, as with every
  matcher change; the scores above are unit-verified against
  the Python formulas.
