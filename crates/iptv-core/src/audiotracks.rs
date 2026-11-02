//! Audio-track preference matching, ported from Python's
//! `audio_tracks.py`: word-token keyword matching, the audio-description
//! keyword set, and preferred-track selection.

/// Keywords that mark an audio-description track (the reference's set,
/// in its order).
pub const AUDIO_DESCRIPTION_KEYWORDS: &[&str] = &[
    "audio description",
    "audiodescription",
    "audio descriptive",
    "descriptive audio",
    "described video",
    "description",
    "descriptive",
    "described",
    "audiodeskription",
    "hoerfilm",
    "hörfilm",
    "audiodeskrypcja",
    "audiovision",
    "descripcion",
    "descripción",
    "descrizione",
    "descrição",
    "dvs",
    "ad",
    "visual impaired",
    "visually impaired",
];

const COMPOUND_MATCH_MIN_CHARS: usize = 6;

/// Lower-cased word tokens of a track name (port of
/// `audio_track_tokens`): split on anything that is not a Unicode
/// letter or digit, underscore included.
#[must_use]
pub fn audio_track_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Whether a track name satisfies one preference keyword (port of
/// `audio_track_matches`): whole-token-span match, plus a substring
/// fallback for keywords long enough that a compound word cannot be a
/// coincidence.
#[must_use]
pub fn audio_track_matches(name: &str, keyword: &str) -> bool {
    let tokens = audio_track_tokens(name);
    let wanted = audio_track_tokens(keyword);
    if tokens.is_empty() || wanted.is_empty() {
        return false;
    }
    let span = wanted.len();
    if span <= tokens.len() {
        for start in 0..=(tokens.len() - span) {
            if tokens[start..start + span] == wanted[..] {
                return true;
            }
        }
    }
    let joined_wanted: String = wanted.concat();
    if joined_wanted.chars().count() < COMPOUND_MATCH_MIN_CHARS {
        return false;
    }
    let joined: String = tokens.concat();
    joined.contains(&joined_wanted)
}

fn dedupe_keywords(keywords: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for keyword in keywords {
        let text = keyword.trim().to_string();
        let folded = text.to_lowercase();
        if text.is_empty() || seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        out.push(text);
    }
    out
}

/// The keyword list to match against, user wording first, then the
/// audio-description set when that preference is on (port of
/// `preferred_audio_keywords`).
#[must_use]
pub fn preferred_audio_keywords(
    keywords: &[String],
    prefer_audio_description: bool,
) -> Vec<String> {
    let mut all: Vec<String> = keywords.to_vec();
    if prefer_audio_description {
        all.extend(AUDIO_DESCRIPTION_KEYWORDS.iter().map(ToString::to_string));
    }
    dedupe_keywords(&all)
}

/// Whether a track name advertises audio description.
#[must_use]
pub fn names_audio_description(name: &str) -> bool {
    AUDIO_DESCRIPTION_KEYWORDS
        .iter()
        .any(|keyword| audio_track_matches(name, keyword))
}

/// The id of the preferred track (port of
/// `select_preferred_audio_track`): highest-priority keyword first
/// (several matches on an AD keyword take the last, where providers
/// append it), then the position fallback, then — with the AD
/// preference on and several tracks — the last described track, or
/// simply the last track.
#[must_use]
pub fn select_preferred_audio_track(
    tracks: &[(i64, String)],
    keywords: &[String],
    fallback_index: Option<i64>,
    prefer_ad: bool,
) -> Option<i64> {
    if tracks.is_empty() {
        return None;
    }
    for keyword in keywords {
        let matches: Vec<i64> = tracks
            .iter()
            .filter(|(_, name)| audio_track_matches(name, keyword))
            .map(|(id, _)| *id)
            .collect();
        if matches.is_empty() {
            continue;
        }
        if matches.len() > 1
            && AUDIO_DESCRIPTION_KEYWORDS
                .iter()
                .any(|ad| ad.eq_ignore_ascii_case(keyword.trim()))
        {
            return matches.last().copied();
        }
        return matches.first().copied();
    }
    if let Some(index) = fallback_index {
        if index >= 0 {
            if let Some((id, _)) = tracks.get(usize::try_from(index).unwrap_or(usize::MAX)) {
                return Some(*id);
            }
        }
    }
    if prefer_ad && tracks.len() > 1 {
        let described: Vec<i64> = tracks
            .iter()
            .filter(|(_, name)| audio_track_matches(name, "audio description"))
            .map(|(id, _)| *id)
            .collect();
        if let Some(id) = described.last() {
            return Some(*id);
        }
        return tracks.last().map(|(id, _)| *id);
    }
    None
}

/// Which track the control should sit on (port of
/// `active_audio_track_index`): the wanted name wins over the reported
/// current id, which may lag or be foreign.
#[must_use]
pub fn active_audio_track_index(
    tracks: &[(i64, String)],
    current_id: Option<i64>,
    wanted_name: Option<&str>,
) -> usize {
    if tracks.is_empty() {
        return 0;
    }
    let wanted = wanted_name.unwrap_or("").trim();
    if !wanted.is_empty() {
        for (index, (_, name)) in tracks.iter().enumerate() {
            if name == wanted {
                return index;
            }
        }
    }
    if let Some(current) = current_id {
        for (index, (id, _)) in tracks.iter().enumerate() {
            if *id == current {
                return index;
            }
        }
    }
    0
}

/// The full priority assembly (port of `ordered_preference_keywords`):
/// this channel's remembered track first, then the AD set when the
/// preference is on, else the last manual pick, then user keywords.
#[must_use]
pub fn ordered_preference_keywords(
    channel_track: Option<&str>,
    prefer_audio_description: bool,
    last_manual: Option<&str>,
    preferred: &[String],
) -> Vec<String> {
    let mut keywords: Vec<String> = Vec::new();
    let channel_track = channel_track.unwrap_or("").trim();
    if !channel_track.is_empty() {
        keywords.push(channel_track.to_string());
    }
    if prefer_audio_description {
        keywords.extend(AUDIO_DESCRIPTION_KEYWORDS.iter().map(ToString::to_string));
    } else {
        let manual = last_manual.unwrap_or("").trim();
        if !manual.is_empty() {
            keywords.push(manual.to_string());
        }
        keywords.extend(preferred_audio_keywords(preferred, false));
    }
    dedupe_keywords(&keywords)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks() -> Vec<(i64, String)> {
        vec![
            (1, "English".to_string()),
            (2, "English AD".to_string()),
            (3, "Radio Drama".to_string()),
        ]
    }

    #[test]
    fn short_keywords_match_whole_words_only() {
        assert!(audio_track_matches("eng AD", "ad"));
        assert!(audio_track_matches("AD (English)", "AD"));
        assert!(!audio_track_matches("Radio", "ad"));
        assert!(audio_track_matches(
            "Audiodescription-Ton",
            "audiodescription"
        ));
    }

    #[test]
    fn selection_prefers_keywords_then_ad_last() {
        let keywords = vec!["German".to_string()];
        assert_eq!(
            select_preferred_audio_track(&tracks(), &keywords, None, false),
            None
        );
        let ad = preferred_audio_keywords(&[], true);
        assert_eq!(
            select_preferred_audio_track(&tracks(), &ad, None, true),
            Some(2)
        );
        // Position fallback restores the third slot by position.
        assert_eq!(
            select_preferred_audio_track(&tracks(), &[], Some(2), false),
            Some(3)
        );
        // AD preference with no named track takes the last track.
        let plain = vec![(1, "A".to_string()), (2, "B".to_string())];
        assert_eq!(
            select_preferred_audio_track(&plain, &[], None, true),
            Some(2)
        );
    }

    #[test]
    fn active_index_wants_name_over_reported_id() {
        assert_eq!(
            active_audio_track_index(&tracks(), Some(1), Some("Radio Drama")),
            2
        );
        assert_eq!(active_audio_track_index(&tracks(), Some(3), None), 2);
        assert_eq!(active_audio_track_index(&tracks(), Some(99), None), 0);
    }
}
