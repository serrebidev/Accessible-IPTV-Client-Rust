//! Scored guide matching, port of the Python fuzzy matcher in
//! `playlist.py` (`get_matching_channel_ids` / `resolve_best_channel_id`)
//! with its vocabulary from `channel_names.py`.
//!
//! `epg::match_guide_channel` keeps its exact id and exact stripped-name
//! paths; this module supplies the pieces Python adds on top: playlist
//! region derivation, guide group tags, zones, time shifts, call signs,
//! brand families (including HBO variants), affiliate markets and the
//! per-candidate score. The candidate *pool* still comes from the Rust
//! name indexes (Python gathers SQL `LIKE` rows); every pooled candidate
//! is scored here and the best score wins, mirroring Python's resolve.
//!
//! One Python step is deliberately not ported: `resolve_best_channel_id`
//! prefers, among near-ties, a channel that has schedule data from now.
//! The Rust indexes carry no programme data, so ties fall back to the
//! exact-name tiebreak and then guide order. Recorded in validation
//! 0077.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use crate::epg::{
    canonicalize_name, is_word_char, replace_word, significant_channel_numbers, strip_noise_words,
    NOISE_WORDS, STRIP_TAGS,
};
use crate::guide_match_data::{AFFILIATE_MARKETS, GROUP_SYNONYMS, MAJOR_US_CITIES, US_STATE_NAMES};
use crate::Channel;

// ---------------------------------------------------------------------------
// Word-boundary helpers (Python `\b` semantics for literal phrases)
// ---------------------------------------------------------------------------

/// Whether `phrase` occurs in `text` with regex `\b` boundaries at both
/// ends. Case-insensitive, like the Python patterns it mirrors.
#[must_use]
pub fn contains_word_phrase(text: &str, phrase: &str) -> bool {
    if phrase.is_empty() {
        return false;
    }
    let hay = text.to_lowercase();
    let needle = phrase.to_lowercase();
    let first_word = needle.chars().next().is_some_and(is_word_char);
    let last_word = needle.chars().last().is_some_and(is_word_char);
    let mut start = 0_usize;
    while let Some(pos) = hay[start..].find(&needle) {
        let pos = start + pos;
        let prev_word = hay[..pos].chars().last().is_some_and(is_word_char);
        let next_word = hay[pos + needle.len()..]
            .chars()
            .next()
            .is_some_and(is_word_char);
        if prev_word != first_word && last_word != next_word {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Maximal runs of word characters (Python `\w+` findall pieces).
fn word_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_word_char(c) {
            current.push(c);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Tokens of the `\b[a-z]{2,3}\b` shape, one entry per occurrence.
fn short_letter_tokens(lowered: &str) -> Vec<String> {
    word_runs(lowered)
        .into_iter()
        .filter(|run| {
            (2..=3).contains(&run.chars().count()) && run.chars().all(|c| c.is_ascii_lowercase())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Country/group vocabulary (channel_names.py)
// ---------------------------------------------------------------------------

/// Variant -> canonical code, first writer wins (Python `_variant_tags`).
fn variant_tags() -> &'static HashMap<String, String> {
    static TAGS: OnceLock<HashMap<String, String>> = OnceLock::new();
    TAGS.get_or_init(|| {
        let mut map = HashMap::new();
        for (code, variants) in GROUP_SYNONYMS {
            for variant in *variants {
                map.entry((*variant).to_string())
                    .or_insert_with(|| (*code).to_string());
            }
        }
        map
    })
}

/// Variant -> canonical code, last writer wins (Python
/// `_build_reverse_country_lookup`, used by `_norm_country`).
fn country_lookup() -> &'static HashMap<String, String> {
    static LOOKUP: OnceLock<HashMap<String, String>> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let mut map = HashMap::new();
        for (code, variants) in GROUP_SYNONYMS {
            map.insert((*code).to_string(), (*code).to_string());
            for variant in *variants {
                map.insert((*variant).to_string(), (*code).to_string());
            }
        }
        map
    })
}

fn norm_country(token: &str) -> String {
    let key = token.trim().to_lowercase().replace('.', "");
    country_lookup().get(&key).cloned().unwrap_or_default()
}

fn is_synonym_code(token: &str) -> bool {
    GROUP_SYNONYMS.iter().any(|(code, _)| *code == token)
}

/// Whether the title's first token is written the way a code is
/// (Python `_looks_like_a_code`): at least two characters, upper case.
fn looks_like_a_code(original: &str) -> bool {
    let Some(token) = original.split_whitespace().next() else {
        return false;
    };
    if token.chars().count() < 2 {
        return false;
    }
    let mut any_cased = false;
    for c in token.chars() {
        if c.is_lowercase() {
            return false;
        }
        if c.is_uppercase() {
            any_cased = true;
        }
    }
    any_cased
}

/// `^([a-z]{2,3})\b` at the start of an already-lowercased string.
fn leading_code(lowered: &str) -> Option<String> {
    let run: String = lowered
        .chars()
        .take_while(char::is_ascii_lowercase)
        .collect();
    if !(2..=3).contains(&run.len()) {
        return None;
    }
    let next = lowered.chars().nth(run.len());
    if next.is_some_and(is_word_char) {
        return None;
    }
    Some(run)
}

/// `\(([a-z]{2,3})\)` search.
fn paren_code(lowered: &str) -> Option<String> {
    let chars: Vec<char> = lowered.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '(' {
            continue;
        }
        let mut code = String::new();
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_lowercase() && code.len() < 4 {
            code.push(chars[j]);
            j += 1;
        }
        if (2..=3).contains(&code.len()) && j < chars.len() && chars[j] == ')' {
            return Some(code);
        }
    }
    None
}

/// Port of `extract_group`: the country/group code a title names, or "".
#[must_use]
pub fn extract_group(title: &str) -> String {
    if title.is_empty() {
        return String::new();
    }
    let lowered = title.to_lowercase();
    for (tag, variants) in GROUP_SYNONYMS {
        for variant in *variants {
            if *variant == *tag {
                continue;
            }
            if ["can", "car", "nor", "per", "ind"].contains(&variant.to_lowercase().as_str()) {
                continue;
            }
            if contains_word_phrase(&lowered, variant) {
                return (*tag).to_string();
            }
        }
    }
    if let Some(code) = leading_code(&lowered) {
        if let Some(tag) = variant_tags().get(&code) {
            if looks_like_a_code(title) {
                return tag.clone();
            }
        }
    }
    if let Some(code) = paren_code(&lowered) {
        if let Some(tag) = variant_tags().get(&code) {
            return tag.clone();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Region from a guide/playlist id (playlist.py `_detect_region_from_id`)
// ---------------------------------------------------------------------------

fn split_on_chars(text: &str, separators: &[char]) -> Vec<String> {
    // Python `re.split` with a `+` separator run: consecutive separators
    // count as one split point, a leading one yields a leading "".
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_sep_run = false;
    for c in text.chars() {
        if separators.contains(&c) {
            if !in_sep_run {
                parts.push(std::mem::take(&mut current));
                in_sep_run = true;
            }
        } else {
            current.push(c);
            in_sep_run = false;
        }
    }
    parts.push(current);
    parts
}

/// Python `_token_region_variants`: wrapper-stripped, letters-only forms
/// of one id token.
fn region_token_variants(token: &str) -> Vec<String> {
    let core = token.trim_matches(['(', ')', '[', ']', '{', '}']);
    let letters: String = core.chars().filter(char::is_ascii_alphabetic).collect();
    let mut out: Vec<String> = Vec::new();
    if !letters.is_empty() {
        out.push(letters);
    }
    let trimmed: String = core
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .chars()
        .filter(char::is_ascii_alphabetic)
        .collect();
    if !trimmed.is_empty() && !out.contains(&trimmed) {
        out.push(trimmed);
    }
    out
}

/// Port of `_detect_region_from_id`.
#[must_use]
pub fn detect_region_from_id(channel_id: &str) -> String {
    if channel_id.is_empty() {
        return String::new();
    }
    let lowered = channel_id.to_lowercase();
    let base = lowered.split('@').next().unwrap_or("");
    let base_parts = split_on_chars(base, &['.', '-', '_', ':', '|', '/']);
    for part in base_parts.iter().rev() {
        for candidate in region_token_variants(part) {
            let region = norm_country(&candidate);
            if !region.is_empty() {
                return region;
            }
        }
    }
    let parts = split_on_chars(&lowered, &['.', '-', '_', ':', '|', '/', '@']);
    for part in parts.iter().rev().chain(parts.iter()) {
        for candidate in region_token_variants(part) {
            let region = norm_country(&candidate);
            if !region.is_empty() {
                return region;
            }
        }
    }
    if let Some(last) = base_parts.last() {
        let chars: Vec<char> = last.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_ascii_lowercase() {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_lowercase() {
                    j += 1;
                }
                if j - i >= 2 {
                    let run: String = chars[i..(i + 3).min(j)].iter().collect();
                    return norm_country(&run);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    String::new()
}

/// The guide-side group tag Python stores per guide channel at import:
/// the display-name region and the id region, the id winning a conflict.
#[must_use]
pub fn guide_group_tag(channel_id: &str, display_name: &str) -> String {
    let name_region = extract_group(display_name);
    let id_region = detect_region_from_id(channel_id);
    if !name_region.is_empty() && !id_region.is_empty() && name_region != id_region {
        return id_region;
    }
    if !name_region.is_empty() {
        return name_region;
    }
    id_region
}

// ---------------------------------------------------------------------------
// Playlist region derivation (playlist.py `_derive_playlist_region`)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Votes {
    map: HashMap<String, (i64, usize)>,
    counter: usize,
}

impl Votes {
    fn add(&mut self, code: &str, weight: i64) {
        if code.is_empty() {
            return;
        }
        let order = self.counter;
        self.counter += 1;
        self.map
            .entry(code.to_string())
            .and_modify(|(total, _)| *total += weight)
            .or_insert((weight, order));
    }

    fn winner(&self) -> String {
        let mut best: Option<(&String, &(i64, usize))> = None;
        for (code, vw) in &self.map {
            let better = match best {
                None => true,
                Some((_, (bw, bo))) => vw.0 > *bw || (vw.0 == *bw && vw.1 < *bo),
            };
            if better {
                best = Some((code, vw));
            }
        }
        best.map_or_else(String::new, |(code, _)| code.clone())
    }
}

fn normalize_for_prefix(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Python `_strip_quality_prefix`: eat quality tags off the front until
/// nothing changes.
fn strip_quality_prefix(text: &str) -> String {
    let mut rem = text.to_string();
    loop {
        let mut changed = false;
        for tag in STRIP_TAGS {
            let clean: String = tag.chars().filter(|c| c.is_alphanumeric()).collect();
            if !clean.is_empty() && rem.starts_with(&clean) {
                rem = rem[clean.len()..].to_string();
                changed = true;
            }
        }
        if !changed {
            return rem;
        }
    }
}

/// Python `_votes_from_prefix`: the text begins with a country variant
/// and the rest is only quality tags.
fn votes_from_prefix(text: &str, weight: i64, votes: &mut Votes) {
    let compact = normalize_for_prefix(text);
    for (code, variants) in GROUP_SYNONYMS {
        for variant in *variants {
            let prefix = normalize_for_prefix(variant);
            if prefix.is_empty() {
                continue;
            }
            if let Some(rest) = compact.strip_prefix(&prefix) {
                if strip_quality_prefix(rest).is_empty() {
                    votes.add(code, weight);
                    return;
                }
            }
        }
    }
}

/// Python `_votes_from_text`.
fn votes_from_text(text: &str, base: i64, votes: &mut Votes) {
    if text.is_empty() {
        return;
    }
    let lowered = text.to_lowercase();
    let trimmed = lowered.trim_start();
    if let Some(code) = leading_code(trimmed) {
        let after = trimmed[code.len()..].trim_start();
        if (after.starts_with(':') || after.starts_with('-')) && is_synonym_code(&code) {
            votes.add(&code, base + 3);
        }
        if is_synonym_code(&code) {
            votes.add(&code, base + 2);
        }
    }
    if lowered.contains("usa") {
        votes.add("us", base + 2);
    }
    votes_from_prefix(text, base + 2, votes);
    let group = extract_group(text);
    if !group.is_empty() {
        votes.add(&group, base);
    }
}

/// `[.\-_:|/@]([a-z]{2,3})` findall pieces of an id, in order.
fn id_separator_tokens(id: &str) -> Vec<String> {
    let chars: Vec<char> = id.to_lowercase().chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if ['.', '-', '_', ':', '|', '/', '@'].contains(&chars[i]) {
            let mut j = i + 1;
            let mut token = String::new();
            while j < chars.len() && chars[j].is_ascii_lowercase() && token.len() < 3 {
                token.push(chars[j]);
                j += 1;
            }
            if token.len() >= 2 {
                out.push(token);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Port of `_derive_playlist_region`: weighted votes over the group,
/// tvg-id, tvg-name and name, ties broken by earliest vote.
#[must_use]
pub fn derive_playlist_region(channel: &Channel) -> String {
    let mut votes = Votes::default();
    if !channel.group.is_empty() {
        let group = extract_group(&channel.group);
        if !group.is_empty() {
            votes.add(&group, 4);
        }
        for token in short_letter_tokens(&channel.group.to_lowercase()) {
            if is_synonym_code(&token) {
                votes.add(&token, 3);
            }
        }
        votes_from_prefix(&channel.group, 5, &mut votes);
    }
    if !channel.tvg_id.is_empty() {
        let region = detect_region_from_id(&channel.tvg_id);
        if !region.is_empty() {
            votes.add(&region, 8);
        }
        for token in id_separator_tokens(&channel.tvg_id) {
            if is_synonym_code(&token) {
                votes.add(&token, 4);
            }
        }
    }
    votes_from_text(&channel.tvg_name, 6, &mut votes);
    votes_from_text(&channel.name, 7, &mut votes);
    votes.winner()
}

// ---------------------------------------------------------------------------
// Zones and time shifts (playlist.py `_detect_zone`, `_detect_timeshift`)
// ---------------------------------------------------------------------------

/// Feed zones in Python's dict order: the first zone with a whole-word
/// token hit wins.
const ZONE_SYNONYMS: &[(&str, &[&str])] = &[
    ("east", &["east", "e", "eastern"]),
    (
        "west",
        &[
            "west", "w", "western", "pacific", "p", "pt", "pst", "pdt", "pac",
        ],
    ),
    ("central", &["central", "c", "ct", "ctr"]),
    ("mountain", &["mountain", "mtn"]),
    ("atlantic", &["atlantic", "atl"]),
];

/// Port of `_detect_zone`.
#[must_use]
pub fn detect_zone(text: &str) -> String {
    let lowered = text.to_lowercase();
    for (zone, tokens) in ZONE_SYNONYMS {
        for token in *tokens {
            if contains_word_phrase(&lowered, token) {
                return (*zone).to_string();
            }
        }
    }
    String::new()
}

/// One `\+\s*(\d{1,2})\s*(?:h|hr|hour|hours)?(?!\w)` match starting at
/// the `+` at `plus`: the captured hours, when the pattern matches.
fn timeshift_plus_at(chars: &[char], plus: usize) -> Option<i64> {
    if plus > 0 && is_word_char(chars[plus - 1]) {
        return None;
    }
    let mut j = plus + 1;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    let digit_start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    let run = j - digit_start;
    if run == 0 || run > 2 {
        return None;
    }
    let value: i64 = chars[digit_start..j]
        .iter()
        .collect::<String>()
        .parse()
        .ok()?;
    let mut k = j;
    while k < chars.len() && chars[k].is_whitespace() {
        k += 1;
    }
    for suffix in ["hours", "hour", "hr", "h", ""] {
        let suffix_chars: Vec<char> = suffix.chars().collect();
        if chars[k..].starts_with(&suffix_chars) {
            let after = k + suffix_chars.len();
            if after >= chars.len() || !is_word_char(chars[after]) {
                return Some(value);
            }
        }
    }
    None
}

/// One `\bplus\s*(\d{1,2})\b` match: the first whole-word "plus" and its
/// number, when the pattern matches.
fn timeshift_plus_word(chars: &[char]) -> Option<i64> {
    let needle: Vec<char> = "plus".chars().collect();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        if chars[i..].starts_with(&needle) && (i == 0 || !is_word_char(chars[i - 1])) {
            let mut j = i + needle.len();
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let digit_start = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let run = j - digit_start;
            if (1..=2).contains(&run) && (j >= chars.len() || !is_word_char(chars[j])) {
                return chars[digit_start..j]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .ok();
            }
        }
        i += 1;
    }
    None
}

/// Port of `_detect_timeshift`: hours of shift (0 when none). Only the
/// first match of each pattern is considered, as in Python.
#[must_use]
pub fn detect_timeshift(text: &str) -> i64 {
    let lowered = text.to_lowercase();
    let chars: Vec<char> = lowered.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '+' {
            if let Some(value) = timeshift_plus_at(&chars, i) {
                if (1..=24).contains(&value) {
                    return value;
                }
                break;
            }
        }
    }
    if let Some(value) = timeshift_plus_word(&chars) {
        if (1..=24).contains(&value) {
            return value;
        }
    }
    0
}

/// Remove every `(?<!\w)\+\d{1,2}(?!\w)` span (Python `_brand_key` step).
fn remove_timeshift_marks(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '+' && (i == 0 || !is_word_char(chars[i - 1])) {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let run = j - (i + 1);
            if (1..=2).contains(&run) && (j >= chars.len() || !is_word_char(chars[j])) {
                out.push(' ');
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Call signs (playlist.py `extract_callsigns`)
// ---------------------------------------------------------------------------

const CALLSIGN_EXCLUDED: &[&str] = &["NEWS", "SPORT", "LIVE", "PLUS", "MAX", "WEST", "EAST"];

/// The call-sign core of one `[A-Z0-9]+` piece: 3-5 leading letters,
/// optionally followed by a DT/TV/HD suffix (Python's two regexes given
/// a piece with no spaces or dashes).
fn callsign_core(part: &str) -> Option<&str> {
    let bytes = part.as_bytes();
    if part.len() >= 3 && part.len() <= 5 && bytes.iter().all(u8::is_ascii_uppercase) {
        return Some(part);
    }
    for group_len in (3..=5).rev() {
        if part.len() <= group_len {
            continue;
        }
        let (group, rest) = part.split_at(group_len);
        if !group.bytes().all(|b| b.is_ascii_uppercase()) {
            continue;
        }
        let rest_ok = rest == "TV"
            || rest == "HD"
            || rest == "DT"
            || (rest.len() == 3 && rest.starts_with("DT") && rest.as_bytes()[2].is_ascii_digit());
        if rest_ok {
            return Some(group);
        }
    }
    None
}

/// Port of `extract_callsigns`.
#[must_use]
pub fn extract_callsigns(text: &str) -> HashSet<String> {
    let spaced: String = text
        .to_uppercase()
        .chars()
        .map(|c| {
            if ['(', ')', '[', ']', '{', '}'].contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let chars: Vec<char> = spaced.chars().collect();
    let in_class = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-';
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if in_class(chars[i]) {
            let mut j = i;
            while j < chars.len() && in_class(chars[j]) {
                j += 1;
            }
            // `[A-Z0-9\-]{3,8}` findall: greedy 8-char chunks.
            let mut pos = i;
            while j - pos >= 3 {
                let take = (j - pos).min(8);
                tokens.push(chars[pos..pos + take].iter().collect());
                pos += take;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let mut out = HashSet::new();
    for token in &tokens {
        for part in token.split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit())) {
            if part.is_empty() {
                continue;
            }
            if let Some(core) = callsign_core(part) {
                let first = core.chars().next().unwrap_or(' ');
                if ['K', 'W', 'C'].contains(&first)
                    && core.len() >= 3
                    && !CALLSIGN_EXCLUDED.contains(&core)
                {
                    out.insert(core.to_string());
                }
            }
        }
    }
    out
}

/// Port of `callsign_overlap_score`.
#[must_use]
pub fn callsign_overlap_score<S: std::hash::BuildHasher>(
    playlist: &HashSet<String, S>,
    epg: &HashSet<String, S>,
) -> (i32, &'static str) {
    if playlist.is_empty() || epg.is_empty() {
        return (0, "");
    }
    if !playlist.is_disjoint(epg) {
        return (100, "+callsign-exact");
    }
    for a in playlist {
        for b in epg {
            if a != b && (b.contains(a.as_str()) || a.contains(b.as_str())) {
                return (70, "+callsign-core");
            }
        }
    }
    (0, "")
}

// ---------------------------------------------------------------------------
// Brand families (playlist.py `_brand_key`, `_reverse_brand_lookup`, HBO)
// ---------------------------------------------------------------------------

/// Port of `_brand_key`: the canonical, zone-free, shift-free brand
/// string with every non-alphanumeric character removed.
#[must_use]
pub fn brand_key(name: &str) -> String {
    let mut text = canonicalize_name(&strip_noise_words(name));
    for (_, tokens) in ZONE_SYNONYMS {
        for token in *tokens {
            text = replace_word(&text, token, " ");
        }
    }
    let text = remove_timeshift_marks(&text);
    text.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// Port of `_reverse_brand_lookup`, run on the canonicalized,
/// noise-stripped, lowercased name.
#[must_use]
pub fn reverse_brand_lookup(text: &str) -> String {
    let has = |phrase: &str| contains_word_phrase(text, phrase);
    let sub = |needle: &str| text.contains(needle);
    if sub("bbc one") {
        return "bbc one".to_string();
    }
    if sub("bbc two") {
        return "bbc two".to_string();
    }
    if sub("itv") {
        return "itv".to_string();
    }
    if has("fxx") {
        return "fxx".to_string();
    }
    if sub("citytv") || has("city") {
        return "citytv".to_string();
    }
    if sub("ctv2") {
        return "ctv2".to_string();
    }
    if has("ctv") {
        return "ctv".to_string();
    }
    if has("cbc") {
        return "cbc".to_string();
    }
    if sub("global") {
        return "global".to_string();
    }
    if has("tsn") {
        return "tsn".to_string();
    }
    if sub("sportsnet") || sub("sn1") || has("sn") {
        return "sportsnet".to_string();
    }
    if has("abc") {
        return "abc".to_string();
    }
    if has("nbc") {
        return "nbc".to_string();
    }
    if has("cbs") {
        return "cbs".to_string();
    }
    if has("fox") && !sub("fox news") {
        return "fox".to_string();
    }
    if has("hgtv") || (sub("hogar") && sub("hgtv")) {
        return "hgtv".to_string();
    }
    if has("pbs") {
        return "pbs".to_string();
    }
    if has("cw") {
        return "cw".to_string();
    }
    if sub("my network") || has("myn") {
        return "mynetwork".to_string();
    }
    if sub("telemundo") {
        return "telemundo".to_string();
    }
    if sub("univision") {
        return "univision".to_string();
    }
    if sub("wdr") {
        return "wdr".to_string();
    }
    if sub("ndr") {
        return "ndr".to_string();
    }
    if sub("mdr") {
        return "mdr".to_string();
    }
    if sub("rbb") {
        return "rbb".to_string();
    }
    if sub("swr") {
        return "swr".to_string();
    }
    if sub("sky crime") {
        return "sky crime".to_string();
    }
    if sub("sky mix") || sub("skymix") || has("sky mix") {
        return "sky mix".to_string();
    }
    if sub("sky max") || sub("skymax") || has("sky max") {
        return "sky max".to_string();
    }
    if has("br") {
        return "br".to_string();
    }
    if has("hr") {
        return "hr".to_string();
    }
    if has("hbo") {
        return "hbo".to_string();
    }
    String::new()
}

/// `hbo[\s\-]*<digit>\b` anywhere in the (lowercased) text.
fn hbo_numbered(text: &str, digit: char) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if chars[i] == 'h' && chars.get(i + 1) == Some(&'b') && chars.get(i + 2) == Some(&'o') {
            let mut j = i + 3;
            while j < chars.len() && (chars[j].is_whitespace() || chars[j] == '-') {
                j += 1;
            }
            if j < chars.len()
                && chars[j] == digit
                && chars.get(j + 1).is_none_or(|c| !is_word_char(*c))
            {
                return true;
            }
        }
    }
    false
}

/// Port of `_extract_hbo_variant`.
#[must_use]
pub fn extract_hbo_variant(text: &str) -> String {
    let lowered = text.to_lowercase();
    for word in [
        "family",
        "latino",
        "signature",
        "comedy",
        "hits",
        "zone",
        "plus",
    ] {
        if contains_word_phrase(&lowered, word) {
            return word.to_string();
        }
    }
    if hbo_numbered(&lowered, '1') || contains_word_phrase(&lowered, "1") {
        return "1".to_string();
    }
    if hbo_numbered(&lowered, '2') || contains_word_phrase(&lowered, "2") {
        return "2".to_string();
    }
    if contains_word_phrase(&lowered, "hbo") {
        return "base".to_string();
    }
    String::new()
}

/// Port of `_normalize_hbo_variant`: in the US, HBO 1 is the base feed.
#[must_use]
pub fn normalize_hbo_variant(country: &str, variant: &str) -> String {
    let variant = variant.trim().to_lowercase();
    if variant.is_empty() {
        return variant;
    }
    if country == "us" && variant == "1" {
        return "base".to_string();
    }
    variant
}

// ---------------------------------------------------------------------------
// Name tokens (playlist.py `tokenize_channel_name`)
// ---------------------------------------------------------------------------

fn token_bad_set() -> &'static HashSet<String> {
    static BAD: OnceLock<HashSet<String>> = OnceLock::new();
    BAD.get_or_init(|| {
        let mut bad: HashSet<String> = HashSet::new();
        for word in STRIP_TAGS.iter().chain(NOISE_WORDS.iter()) {
            bad.insert((*word).to_string());
        }
        for word in [
            "channel",
            "tv",
            "the",
            "and",
            "for",
            "with",
            "on",
            "in",
            "f",
            "geo",
            "geoblocked",
            "blocked",
            "not",
            "only",
        ] {
            bad.insert(word.to_string());
        }
        for digit in ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "hd", "sd"] {
            bad.remove(digit);
        }
        bad
    })
}

/// `^\d{3,4}p$` or `^\d+k$`.
fn is_resolution_token(word: &str) -> bool {
    if let Some(digits) = word.strip_suffix('p') {
        return (3..=4).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(digits) = word.strip_suffix('k') {
        return !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

fn collect_tokens(text: &str, bad: &HashSet<String>, out: &mut Vec<String>) {
    for word in word_runs(text) {
        let word = word.to_lowercase();
        if word.chars().count() <= 1 && !word.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if bad.contains(&word) || is_resolution_token(&word) {
            continue;
        }
        if !out.contains(&word) {
            out.push(word);
        }
    }
}

/// Port of `tokenize_channel_name`: significant tokens, words outside
/// parentheses first, then the parenthesised ones, order preserved.
#[must_use]
pub fn channel_tokens(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut outside = String::new();
    let mut parens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '(' {
            if let Some(rel) = chars[i..].iter().position(|c| *c == ')') {
                parens.push(chars[i + 1..i + rel].iter().collect());
                outside.push(' ');
                i += rel + 1;
                continue;
            }
        }
        outside.push(chars[i]);
        i += 1;
    }
    let bad = token_bad_set();
    let mut out = Vec::new();
    collect_tokens(&outside, bad, &mut out);
    for content in &parens {
        collect_tokens(content, bad, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Affiliate markets (playlist.py `_market_tokens_for`)
// ---------------------------------------------------------------------------

const US_STATE_PROVINCES: &[&str] = &[
    "al", "ak", "az", "ar", "ca", "co", "ct", "de", "fl", "ga", "hi", "id", "il", "in", "ia", "ks",
    "ky", "la", "me", "md", "ma", "mi", "mn", "ms", "mo", "mt", "ne", "nv", "nh", "nj", "nm", "ny",
    "nc", "nd", "oh", "ok", "or", "pa", "ri", "sc", "sd", "tn", "tx", "ut", "vt", "va", "wa", "wv",
    "wi", "wy", "dc",
];

const CA_PROVINCES: &[&str] = &[
    "bc", "ab", "sk", "mb", "on", "qc", "ns", "nl", "nb", "pe", "yt", "nt", "nu",
];

fn normalize_str(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port of `_market_tokens_for`: (markets, provinces, cities) a text
/// names for one country/brand pair.
#[must_use]
pub fn market_tokens_for(
    country: &str,
    brand: &str,
    text: &str,
) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
    let mut markets = HashSet::new();
    let mut provinces = HashSet::new();
    let mut cities = HashSet::new();
    if country.is_empty() || brand.is_empty() {
        return (markets, provinces, cities);
    }
    let normalized = normalize_str(text);
    let tokens: HashSet<String> = short_letter_tokens(&normalized).into_iter().collect();
    let province_list: &[&str] = match country {
        "ca" => CA_PROVINCES,
        "us" => US_STATE_PROVINCES,
        _ => &[],
    };
    for province in province_list {
        if tokens.contains(*province) {
            provinces.insert((*province).to_string());
        }
    }
    if country == "uk" && tokens.contains("ni") {
        provinces.insert("ni".to_string());
    }
    if country == "us" {
        for (full, abbr) in US_STATE_NAMES {
            if contains_word_phrase(&normalized, full) {
                provinces.insert((*abbr).to_string());
            }
        }
    }
    if let Some((_, brands)) = AFFILIATE_MARKETS.iter().find(|(c, _)| *c == country) {
        if let Some((_, market_map)) = brands.iter().find(|(b, _)| *b == brand) {
            for (market, synonyms) in *market_map {
                for synonym in *synonyms {
                    if contains_word_phrase(&normalized, synonym) {
                        markets.insert((*market).to_string());
                        if let Some((city, _)) = market.split_once('-') {
                            cities.insert(city.to_string());
                        }
                        break;
                    }
                }
            }
        }
    }
    if country == "us" {
        for callsign in extract_callsigns(text) {
            markets.insert(callsign.to_lowercase());
        }
        for city in MAJOR_US_CITIES {
            if contains_word_phrase(&normalized, city) {
                cities.insert((*city).to_string());
            }
        }
    }
    (markets, provinces, cities)
}

/// Port of `id_number_text`: purely numeric ids are row keys, not
/// channel numbers, and read as "".
#[must_use]
pub fn id_number_text(channel_id: &str) -> String {
    let trimmed = channel_id.trim();
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return String::new();
    }
    channel_id.to_string()
}

// ---------------------------------------------------------------------------
// The scored match (playlist.py `get_matching_channel_ids`, fuzzy pass)
// ---------------------------------------------------------------------------

/// `\bus\s*[:\-]` on one text (the US region override scan).
fn us_colon_hit(lowered: &str) -> bool {
    let chars: Vec<char> = lowered.chars().collect();
    for i in 0..chars.len() {
        if chars[i] == 'u'
            && chars.get(i + 1) == Some(&'s')
            && (i == 0 || !is_word_char(chars[i - 1]))
        {
            let mut j = i + 2;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == ':' || chars[j] == '-') {
                return true;
            }
        }
    }
    false
}

/// Everything the scorer needs to know about the playlist channel,
/// computed once per channel (Python computes the same bundle inside
/// `get_matching_channel_ids`).
#[derive(Clone, Debug)]
pub struct PlaylistSide {
    region: String,
    zone: String,
    timeshift: i64,
    numbers: BTreeSet<String>,
    family: String,
    brand_key: String,
    hbo_variant: String,
    callsigns: HashSet<String>,
    tokens: HashSet<String>,
    markets: HashSet<String>,
    provinces: HashSet<String>,
    text_lower: String,
    tvg_id_region: String,
}

impl PlaylistSide {
    #[must_use]
    pub fn new(channel: &Channel) -> Self {
        let name = channel.name.as_str();
        let tvg_name = channel.tvg_name.as_str();
        let id_text = id_number_text(&channel.tvg_id);
        let mut region = derive_playlist_region(channel);
        if region != "us" {
            let forced = [tvg_name, name, channel.group.as_str()].iter().any(|text| {
                let lowered = text.to_lowercase();
                us_colon_hit(&lowered)
                    || lowered.contains("usa")
                    || lowered.contains("united states")
            });
            if forced {
                region = "us".to_string();
            }
        }
        // Python reads the brand family, brand key and zone from the
        // channel's own name (plus group for zone and call signs), never
        // from the tvg-name.
        let brand_text = canonicalize_name(&strip_noise_words(name));
        let family = reverse_brand_lookup(&brand_text);
        let (markets, provinces, _) =
            market_tokens_for(&region, &family, &format!("{tvg_name} {name}"));
        Self {
            zone: detect_zone(&format!("{} {tvg_name} {name}", channel.group)),
            timeshift: detect_timeshift(&format!("{tvg_name} {name}")),
            numbers: significant_channel_numbers(&[id_text.as_str(), tvg_name, name]),
            brand_key: brand_key(name),
            hbo_variant: normalize_hbo_variant(
                &region,
                &extract_hbo_variant(&format!("{tvg_name} {name} {} {id_text}", channel.group)),
            ),
            callsigns: extract_callsigns(&format!(
                "{tvg_name} {name} {} {}",
                channel.group, channel.tvg_id
            )),
            tokens: channel_tokens(name).into_iter().collect(),
            markets,
            provinces,
            text_lower: format!("{tvg_name} {name} {}", channel.group).to_lowercase(),
            tvg_id_region: detect_region_from_id(&channel.tvg_id),
            region,
            family,
        }
    }

    /// The playlist region the scorer derived (diagnostics and tests).
    #[must_use]
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Python's exact-name score: the base (96 for an exact tvg-name,
    /// 92 for an exact name) with the number adjustment, the tvg-id
    /// region-hint penalty and the wrong-region penalty applied.
    #[must_use]
    pub fn exact_name_score(&self, base: i32, epg_id: &str, display: &str, group_tag: &str) -> i32 {
        let candidate_numbers =
            significant_channel_numbers(&[id_number_text(epg_id).as_str(), display]);
        let mut score = base;
        if !self.numbers.is_empty() {
            if !self.numbers.is_disjoint(&candidate_numbers) {
                score += 12;
            } else if !candidate_numbers.is_empty() {
                score = 20;
            } else {
                score -= 30;
            }
        } else if !candidate_numbers.is_empty() {
            score -= 15;
        }
        if !self.tvg_id_region.is_empty() {
            score = (score - 60).max(1);
        }
        if !self.region.is_empty() && !group_tag.is_empty() && group_tag != self.region {
            score = (score - 30).max(1);
        }
        score
    }

    /// Python's fuzzy score for one guide candidate, or None when the
    /// candidate fails the gate (no family alignment, no call-sign
    /// overlap, no shared token, and no US market overlap).
    #[must_use]
    pub fn score_candidate(&self, epg_id: &str, display: &str, group_tag: &str) -> Option<i32> {
        let epg_norm = canonicalize_name(&strip_noise_words(display));
        let epg_family = reverse_brand_lookup(&epg_norm);
        let epg_callsigns = extract_callsigns(&format!("{display} {epg_id}"));
        let epg_tokens: HashSet<String> = channel_tokens(display).into_iter().collect();
        let overlap = self.tokens.intersection(&epg_tokens).count();
        let families_align =
            !self.family.is_empty() && !epg_family.is_empty() && self.family == epg_family;
        let (cs_delta, _) = callsign_overlap_score(&self.callsigns, &epg_callsigns);
        if !(families_align || cs_delta >= 60 || overlap >= 1) {
            if self.region != "us" {
                return None;
            }
            let (epg_markets, epg_provinces, _) = market_tokens_for("us", &epg_family, display);
            if self.markets.is_disjoint(&epg_markets) && self.provinces.is_disjoint(&epg_provinces)
            {
                return None;
            }
        }
        let mut score = cs_delta;
        if families_align {
            score += 40;
            let epg_brand_key = brand_key(display);
            if !self.brand_key.is_empty()
                && !epg_brand_key.is_empty()
                && self.brand_key == epg_brand_key
            {
                score += 10;
            }
        }
        score += self.region_score(group_tag, families_align, &epg_family);
        score += self.zone_score(display);
        score += self.timeshift_score(display, epg_id);
        score += self.numbers_score(epg_id, display);
        score += self.hbo_score(epg_id, display, group_tag, &epg_family);
        score += self.sky_score(display);
        score += i32::try_from(overlap).unwrap_or(0).min(5) * 4;
        Some(score)
    }

    fn region_score(&self, group_tag: &str, families_align: bool, epg_family: &str) -> i32 {
        if self.region.is_empty() {
            return 0;
        }
        let mut score = if group_tag == self.region {
            18
        } else if group_tag.is_empty() {
            6
        } else {
            -40
        };
        if families_align && !group_tag.is_empty() && group_tag != self.region {
            if epg_family == "hbo" {
                score -= 20;
            } else if epg_family == "hgtv" {
                score -= 25;
            }
        }
        score
    }

    fn zone_score(&self, display: &str) -> i32 {
        // Python detects the guide zone from the display name alone.
        let epg_zone = detect_zone(display);
        if !self.zone.is_empty() && !epg_zone.is_empty() {
            if self.zone == epg_zone {
                return 8;
            }
            return -15;
        }
        if self.region == "us" && self.zone.is_empty() && epg_zone == "east" {
            return 38;
        }
        0
    }

    fn timeshift_score(&self, display: &str, epg_id: &str) -> i32 {
        let epg_ts = detect_timeshift(&format!("{display} {epg_id}"));
        if self.timeshift == 0 && epg_ts == 0 {
            return 0;
        }
        if self.timeshift != 0 && epg_ts != 0 {
            let delta = (self.timeshift - epg_ts).abs();
            return match delta {
                0 => 22,
                1 => 9,
                2 => 5,
                _ => -10 * i32::try_from(delta).unwrap_or(0),
            };
        }
        if epg_ts == 0 {
            -15
        } else {
            -8
        }
    }

    fn numbers_score(&self, epg_id: &str, display: &str) -> i32 {
        if self.numbers.is_empty() {
            return 0;
        }
        let epg_numbers = significant_channel_numbers(&[id_number_text(epg_id).as_str(), display]);
        if !self.numbers.is_disjoint(&epg_numbers) {
            16
        } else if !epg_numbers.is_empty() {
            -30
        } else {
            -6
        }
    }

    fn hbo_score(&self, epg_id: &str, display: &str, group_tag: &str, epg_family: &str) -> i32 {
        if self.family != "hbo" || epg_family != "hbo" {
            return 0;
        }
        let epg_raw = extract_hbo_variant(&format!("{display} {}", id_number_text(epg_id)));
        let epg_variant = normalize_hbo_variant(group_tag, &epg_raw);
        if self.hbo_variant.is_empty() && epg_variant.is_empty() {
            return 0;
        }
        let mine = normalize_hbo_variant(
            &self.region,
            if self.hbo_variant.is_empty() {
                "base"
            } else {
                &self.hbo_variant
            },
        );
        let theirs = normalize_hbo_variant(
            group_tag,
            if epg_variant.is_empty() {
                "base"
            } else {
                &epg_variant
            },
        );
        if mine == theirs {
            return 40;
        }
        if self.region == "us" {
            let pl_baseish = self.hbo_variant.is_empty()
                || self.hbo_variant == "base"
                || self.hbo_variant == "1";
            if (pl_baseish && (epg_variant == "base" || epg_variant == "1"))
                || ((epg_variant.is_empty() || epg_variant == "base")
                    && (self.hbo_variant == "base" || self.hbo_variant == "1"))
            {
                return 18;
            }
            return -10;
        }
        -12
    }

    fn sky_score(&self, display: &str) -> i32 {
        let epg_lower = display.to_lowercase();
        let mut score = 0;
        // Python keeps the two mix-ups in one if/elif; both cost 60.
        let sports_clash =
            self.text_lower.contains("sky mix") && epg_lower.contains("sky sports mix");
        let mix_clash = self.text_lower.contains("sky sports mix")
            && epg_lower.contains("sky mix")
            && !epg_lower.contains("sky sports mix");
        if sports_clash || mix_clash {
            score -= 60;
        }
        if self.text_lower.contains("sky mix") {
            if epg_lower.contains("sky mix") {
                score += 35;
            } else {
                score -= 35;
            }
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn channel(name: &str, group: &str, tvg_id: &str, tvg_name: &str) -> Channel {
        Channel {
            name: name.to_string(),
            group: group.to_string(),
            url: "https://example.com/s".to_string(),
            tvg_id: tvg_id.to_string(),
            tvg_name: tvg_name.to_string(),
            tvg_logo: String::new(),
            unknown_attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn extract_group_reads_codes_like_python() {
        assert_eq!(extract_group("US: CNN"), "us");
        assert_eq!(extract_group("AT Sports 1"), "at");
        assert_eq!(extract_group("UK: Drama"), "uk");
        assert_eq!(extract_group("(UK) Sky Cinema"), "uk");
        assert_eq!(extract_group("CANADA: CBC"), "ca");
        // An ordinary capitalized word is not a country code.
        assert_eq!(extract_group("No Limits"), "");
        assert_eq!(extract_group("By Example"), "");
        assert_eq!(extract_group("HBO"), "");
    }

    #[test]
    fn region_from_id_matches_python() {
        assert_eq!(detect_region_from_id("CNN.us"), "us");
        assert_eq!(detect_region_from_id("ESPNHD.us@City"), "us");
        assert_eq!(detect_region_from_id("bbc2.uk"), "uk");
        // The documented misfire: whole-id scan must not read Brazil
        // out of "Bravo.PEACOCK".
        assert_eq!(detect_region_from_id("Bravo.PEACOCK"), "");
        assert_eq!(detect_region_from_id("12345"), "");
        assert_eq!(detect_region_from_id(""), "");
    }

    #[test]
    fn guide_group_tag_id_wins_conflicts() {
        assert_eq!(guide_group_tag("hbo.us", "HBO"), "us");
        assert_eq!(guide_group_tag("itvgranada.uk", "ITV Granada"), "uk");
        assert_eq!(guide_group_tag("something", "No Region Here"), "");
    }

    #[test]
    fn playlist_region_votes() {
        assert_eq!(
            derive_playlist_region(&channel("Anything", "UK: Drama", "", "")),
            "uk"
        );
        assert_eq!(
            derive_playlist_region(&channel("CNN", "", "espn.us", "")),
            "us"
        );
        assert_eq!(
            derive_playlist_region(&channel("US: CNN", "", "", "")),
            "us"
        );
        assert_eq!(
            derive_playlist_region(&channel("Plain Name", "", "", "")),
            ""
        );
    }

    #[test]
    fn zones_and_timeshifts() {
        assert_eq!(detect_zone("HBO East"), "east");
        assert_eq!(detect_zone("HBO West"), "west");
        assert_eq!(detect_zone("CTV Atlantic"), "atlantic");
        assert_eq!(detect_zone("Plain"), "");
        assert_eq!(detect_timeshift("ESPN +1"), 1);
        assert_eq!(detect_timeshift("Sky One +1"), 1);
        assert_eq!(detect_timeshift("Channel Plus 2"), 2);
        // Python considers only each pattern's first match: +25 is out
        // of range, so the channel reads as unshifted.
        assert_eq!(detect_timeshift("HBO +25"), 0);
        assert_eq!(detect_timeshift("No Shift"), 0);
    }

    #[test]
    fn callsigns_extract_and_score() {
        let calls = extract_callsigns("KABC-TV Los Angeles");
        assert!(calls.contains("KABC"), "{calls:?}");
        let calls = extract_callsigns("WABC News at Ten");
        assert!(calls.contains("WABC"), "{calls:?}");
        assert!(!calls.contains("NEWS"), "{calls:?}");
        let a: HashSet<String> = ["KABC".to_string()].into_iter().collect();
        let b: HashSet<String> = ["KABC".to_string()].into_iter().collect();
        assert_eq!(callsign_overlap_score(&a, &b).0, 100);
        let c: HashSet<String> = ["KABCD".to_string()].into_iter().collect();
        assert_eq!(callsign_overlap_score(&a, &c).0, 70);
        let d: HashSet<String> = ["WXYZ".to_string()].into_iter().collect();
        assert_eq!(callsign_overlap_score(&a, &d).0, 0);
    }

    #[test]
    fn brands_and_hbo_variants() {
        assert_eq!(brand_key("HBO East"), "hbo");
        assert_eq!(brand_key("CTV Atlantic"), "ctv");
        assert_eq!(reverse_brand_lookup("bbc one"), "bbc one");
        assert_eq!(reverse_brand_lookup("itv granada"), "itv");
        assert_eq!(reverse_brand_lookup("city news"), "citytv");
        assert_eq!(reverse_brand_lookup("fox news"), "");
        assert_eq!(reverse_brand_lookup("hbo"), "hbo");
        assert_eq!(extract_hbo_variant("HBO Family"), "family");
        assert_eq!(extract_hbo_variant("HBO2"), "2");
        assert_eq!(extract_hbo_variant("HBO"), "base");
        assert_eq!(normalize_hbo_variant("us", "1"), "base");
        assert_eq!(normalize_hbo_variant("ca", "1"), "1");
    }

    #[test]
    fn tokens_drop_noise_but_keep_digits_and_hd() {
        let tokens = channel_tokens("Sky One +1");
        assert_eq!(tokens, vec!["sky", "one", "1"]);
        let tokens = channel_tokens("ITV (HD)");
        assert_eq!(tokens, vec!["itv", "hd"]);
    }

    #[test]
    fn hbo_zone_scoring_prefers_the_matching_feed() {
        let side = PlaylistSide::new(&channel("HBO W", "US: HBO", "", ""));
        assert_eq!(side.region(), "us");
        let west = side.score_candidate("hbowest.us", "HBO West", "us");
        let east = side.score_candidate("hboeast.us", "HBO East", "us");
        assert!(west.is_some() && east.is_some());
        assert!(
            west.unwrap() > east.unwrap(),
            "west {west:?} should beat east {east:?}"
        );
    }

    #[test]
    fn callsign_candidate_scores_strongly() {
        let side = PlaylistSide::new(&channel("KABC", "US", "", ""));
        let score = side.score_candidate("kabc.us", "KABC-TV", "us");
        assert!(score.is_some_and(|s| s >= 100), "{score:?}");
    }

    #[test]
    fn market_tokens_find_us_states_and_cities() {
        let (markets, provinces, cities) = market_tokens_for("us", "abc", "ABC 7 New York");
        assert!(provinces.contains("ny"), "{provinces:?}");
        assert!(cities.contains("new york"), "{cities:?}");
        assert!(markets.is_empty(), "{markets:?}");
    }
}
