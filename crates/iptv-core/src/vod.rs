//! Video-on-demand catalogues, ported from Python's `vod.py`: the M3U
//! group-hint heuristic and the Xtream `player_api` catalogue, with
//! season/episode parsing, series folders and lazy episode expansion.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::providers::{ProviderAccount, XtreamClient};
use crate::Channel;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VodKind {
    Movie,
    Series,
}

/// One catalogue entry: a playable item, or a series folder.
#[derive(Clone, Debug)]
pub struct VodItem {
    pub kind: VodKind,
    pub name: String,
    pub group: String,
    /// The playable channel for movies and episodes.
    pub channel: Option<Channel>,
    /// Xtream series id for lazy episode loading.
    pub series_id: Option<String>,
    pub provider_id: Option<String>,
    /// Stable source identity (spec 0006): the M3U playlist scope
    /// id carried by the channel's playlist-id attribute, or the
    /// Xtream provider id. Never a stream URL.
    pub source_id: Option<String>,
    /// Safe source label stamped at catalogue build time: the
    /// configured friendly name when one exists, else the host
    /// with userinfo removed, the local file name, or a generic
    /// label. Never carries credentials.
    pub source_label: Option<String>,
    /// Episodes for M3U-built series folders.
    pub episodes: Vec<VodItem>,
    /// Parsed (season, episode) for ordering.
    pub season_episode: Option<(u32, u32)>,
}

/// A catalogue: group labels in first-appearance order, items per group.
#[derive(Clone, Debug, Default)]
pub struct VodCatalog {
    pub order: Vec<String>,
    pub groups: BTreeMap<String, Vec<VodItem>>,
}

impl VodCatalog {
    fn add(&mut self, label: &str, item: VodItem) {
        if !self.groups.contains_key(label) {
            self.order.push(label.to_string());
        }
        self.groups.entry(label.to_string()).or_default().push(item);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

fn digits_at(bytes: &[u8], start: usize, max: usize) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    let mut count = 0;
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() && count < max {
        value = value * 10 + u32::from(bytes[i] - b'0');
        count += 1;
        i += 1;
    }
    if count == 0 {
        None
    } else {
        Some((value, i))
    }
}

/// Find the `SxxExx` marker: returns (season, episode, marker start).
fn find_se_marker(name: &str) -> Option<(u32, u32, usize)> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b's' || bytes[i] == b'S' {
            if let Some((season, after_season)) = digits_at(bytes, i + 1, 3) {
                let mut j = after_season;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'.' | b'_' | b'x' | b'-') {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'e' || bytes[j] == b'E') {
                    if let Some((episode, _)) = digits_at(bytes, j + 1, 3) {
                        return Some((season, episode, i));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Find the `NxNN` marker with digit boundaries: (season, episode, start).
fn find_alt_marker(name: &str) -> Option<(u32, u32, usize)> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let boundary_before = i == 0 || !bytes[i - 1].is_ascii_digit();
        if boundary_before && bytes[i].is_ascii_digit() {
            if let Some((season, after)) = digits_at(bytes, i, 2) {
                if season_len(bytes, i) <= 2
                    && after < bytes.len()
                    && (bytes[after] == b'x' || bytes[after] == b'X')
                {
                    if let Some((episode, end)) = digits_at(bytes, after + 1, 3) {
                        let boundary_after = end >= bytes.len() || !bytes[end].is_ascii_digit();
                        if boundary_after {
                            return Some((season, episode, i));
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn season_len(bytes: &[u8], start: usize) -> usize {
    let mut n = 0;
    while start + n < bytes.len() && bytes[start + n].is_ascii_digit() {
        n += 1;
    }
    n
}

/// Find an episode-only marker ("Episode 5", "Ep. 5"): (episode, start).
fn find_ep_marker(name: &str) -> Option<(u32, usize)> {
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if boundary && bytes[i] == b'e' {
            let rest = &lower[i + 1..];
            let after_ep = if rest.starts_with("pisode") {
                i + 1 + 6
            } else if rest.starts_with('p') {
                i + 2
            } else {
                i + 1
            };
            if rest.starts_with('p') {
                let mut j = after_ep;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'.' | b'_' | b'-') {
                    j += 1;
                }
                if let Some((episode, end)) = digits_at(bytes, j, 3) {
                    if end >= bytes.len() || !bytes[end].is_ascii_digit() {
                        return Some((episode, i));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Port of `parse_season_episode`: (season, episode), episode-only
/// matches report season 0.
#[must_use]
pub fn parse_season_episode(name: &str) -> Option<(u32, u32)> {
    if name.is_empty() {
        return None;
    }
    if let Some((season, episode, _)) = find_se_marker(name) {
        return Some((season, episode));
    }
    if let Some((season, episode, _)) = find_alt_marker(name) {
        return Some((season, episode));
    }
    find_ep_marker(name).map(|(episode, _)| (0, episode))
}

/// Port of `_series_title`: cut at the first marker, trim separators.
#[must_use]
pub fn series_title(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let cut = find_se_marker(name)
        .map(|(_, _, start)| start)
        .or_else(|| find_alt_marker(name).map(|(_, _, start)| start))
        .or_else(|| find_ep_marker(name).map(|(_, start)| start));
    let head = match cut {
        Some(start) => {
            name[..start].trim_matches(|c| matches!(c, ' ' | '-' | '_' | '.' | '|' | ':'))
        }
        None => name.trim_matches(|c| matches!(c, ' ' | '-' | '_' | '.' | '|' | ':')),
    };
    if head.is_empty() {
        name.trim_matches(|c| matches!(c, ' ' | '-' | '_' | '.' | '|' | ':'))
            .to_string()
    } else {
        head.to_string()
    }
}

/// Order episodes by (season, episode); untagged keep insertion order
/// last (port of `_sort_episodes`).
pub fn sort_episodes(episodes: &mut Vec<VodItem>) {
    let mut keyed: Vec<((u32, u32), usize, VodItem)> = episodes
        .drain(..)
        .enumerate()
        .map(|(i, item)| {
            let key = item
                .season_episode
                .unwrap_or((1_000_000, u32::try_from(i).unwrap_or(0)));
            (key, i, item)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    episodes.extend(keyed.into_iter().map(|(_, _, item)| item));
}

const SERIES_HINTS: &[&str] = &["series", "tv show", "tv shows", "shows", "sezon", "season"];
const MOVIE_HINTS: &[&str] = &["vod", "movie", "movies", "film", "films", "cinema", "peli"];

/// category -> show key -> [(season/episode, channel)]
type SeriesBuckets = BTreeMap<String, BTreeMap<String, Vec<(Option<(u32, u32)>, Channel)>>>;

fn flat_item(channel: &Channel, category: &str) -> VodItem {
    VodItem {
        kind: VodKind::Movie,
        name: channel.name.clone(),
        group: category.to_string(),
        channel: Some(channel.clone()),
        series_id: None,
        provider_id: None,
        source_id: crate::sources::channel_scope(channel).map(str::to_owned),
        source_label: None,
        episodes: Vec::new(),
        season_episode: None,
    }
}

/// Split movies and series out of a flat channel list by group-title
/// hints (port of `categorize_m3u_vod`): only explicit hints qualify,
/// and a series folder needs 2+ episodes with 2+ distinct markers.
#[must_use]
pub fn categorize_m3u_vod(channels: &[Channel]) -> VodCatalog {
    let mut order: Vec<String> = Vec::new();
    let mut movie_by_cat: BTreeMap<String, Vec<VodItem>> = BTreeMap::new();
    // category -> show key -> [(se, channel)]
    let mut series_by_cat: SeriesBuckets = BTreeMap::new();
    let mut show_display: BTreeMap<(String, String), String> = BTreeMap::new();
    for channel in channels {
        let name = if channel.name.trim().is_empty() {
            channel.tvg_name.trim()
        } else {
            channel.name.trim()
        };
        if name.is_empty() {
            continue;
        }
        let category = channel.group.trim().to_string();
        let gl = category.to_lowercase();
        let is_series = SERIES_HINTS.iter().any(|h| gl.contains(h));
        let is_movie = MOVIE_HINTS.iter().any(|h| gl.contains(h));
        if !is_series && !is_movie {
            continue;
        }
        if !order.contains(&category)
            && !movie_by_cat.contains_key(&category)
            && !series_by_cat.contains_key(&category)
        {
            order.push(category.clone());
        }
        if is_series && !is_movie {
            let show = series_title(name);
            let key = show.to_lowercase();
            show_display
                .entry((category.clone(), key.clone()))
                .or_insert(show);
            series_by_cat
                .entry(category)
                .or_default()
                .entry(key)
                .or_default()
                .push((parse_season_episode(name), channel.clone()));
        } else {
            movie_by_cat
                .entry(category)
                .or_default()
                .push(flat_item(channel, channel.group.trim()));
        }
    }
    finish_m3u_catalog(&order, &movie_by_cat, &series_by_cat, &show_display)
}

/// The second phase of [`categorize_m3u_vod`]: decide series folders
/// vs flat items, dedupe URLs per category, and assemble the catalogue.
fn finish_m3u_catalog(
    order: &[String],
    movie_by_cat: &BTreeMap<String, Vec<VodItem>>,
    series_by_cat: &SeriesBuckets,
    show_display: &BTreeMap<(String, String), String>,
) -> VodCatalog {
    let mut catalog = VodCatalog::default();
    let mut seen_urls: BTreeMap<String, std::collections::HashSet<String>> = BTreeMap::new();
    let add_flat = |catalog: &mut VodCatalog,
                    seen: &mut BTreeMap<String, std::collections::HashSet<String>>,
                    category: &str,
                    item: VodItem| {
        let url = item
            .channel
            .as_ref()
            .map_or(String::new(), |c| c.url.clone());
        let urls = seen.entry(category.to_string()).or_default();
        if !url.is_empty() && urls.contains(&url) {
            return;
        }
        if !url.is_empty() {
            urls.insert(url);
        }
        catalog.add(category, item);
    };
    for category in order {
        if let Some(shows) = series_by_cat.get(category) {
            for (key, entries) in shows {
                let markers: std::collections::HashSet<(u32, u32)> =
                    entries.iter().filter_map(|(se, _)| *se).collect();
                if entries.len() >= 2 && markers.len() >= 2 {
                    let mut episodes: Vec<VodItem> = entries
                        .iter()
                        .map(|(se, ch)| {
                            let mut item = flat_item(ch, category);
                            item.season_episode = *se;
                            item
                        })
                        .collect();
                    sort_episodes(&mut episodes);
                    catalog.add(
                        category,
                        VodItem {
                            kind: VodKind::Series,
                            name: show_display
                                .get(&(category.clone(), key.clone()))
                                .cloned()
                                .unwrap_or_else(|| key.clone()),
                            group: category.clone(),
                            channel: None,
                            series_id: None,
                            provider_id: None,
                            // A folder's source is its episodes'
                            // source (one playlist per folder).
                            source_id: episodes
                                .first()
                                .and_then(|episode| episode.source_id.clone()),
                            source_label: None,
                            episodes,
                            season_episode: None,
                        },
                    );
                } else {
                    for (_, ch) in entries {
                        let item = flat_item(ch, category);
                        add_flat(&mut catalog, &mut seen_urls, category, item);
                    }
                }
            }
        }
        if let Some(items) = movie_by_cat.get(category) {
            for item in items.clone() {
                add_flat(&mut catalog, &mut seen_urls, category, item);
            }
        }
    }
    catalog
}

fn category_map(rows: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(list) = rows.as_array() {
        for row in list {
            let Some(cid) = row.get("category_id") else {
                continue;
            };
            let id = match cid {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            let name = row
                .get("category_name")
                .and_then(Value::as_str)
                .unwrap_or("Uncategorized");
            out.insert(id, name.to_string());
        }
    }
    out
}

fn str_field(row: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match row.get(key) {
            Some(Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn playable_item(
    name: String,
    group: String,
    url: String,
    provider_id: Option<String>,
    source_label: Option<String>,
    resume_type: &str,
    resume_id: &str,
) -> VodItem {
    // The channel carries the same identity keys Python's
    // vod.py sets, so playback bookmarks (P21) key Xtream
    // items by their account-scoped locator, not by URL.
    let mut unknown_attributes = BTreeMap::new();
    unknown_attributes.insert("provider-type".to_string(), "xtream".to_string());
    if let Some(id) = &provider_id {
        unknown_attributes.insert("provider-id".to_string(), id.clone());
    }
    unknown_attributes.insert("resume-type".to_string(), resume_type.to_string());
    unknown_attributes.insert("resume-id".to_string(), resume_id.to_string());
    let channel = Channel {
        name: name.clone(),
        group: group.clone(),
        url,
        tvg_id: String::new(),
        tvg_name: String::new(),
        tvg_logo: String::new(),
        unknown_attributes,
    };
    VodItem {
        kind: VodKind::Movie,
        name,
        group,
        channel: Some(channel),
        series_id: None,
        source_id: provider_id.clone(),
        source_label,
        provider_id,
        episodes: Vec::new(),
        season_episode: None,
    }
}

/// Build one Xtream provider's catalogue (port of
/// `build_xtream_catalog`): movies eagerly, series lazily. Section
/// failures skip that section, like the reference.
#[must_use]
pub fn xtream_catalog(client: &XtreamClient, provider_id: &str, source_label: &str) -> VodCatalog {
    let mut catalog = VodCatalog::default();
    if let Ok(streams) = client.vod_streams(None) {
        let categories = category_map(&client.vod_categories().unwrap_or(Value::Null));
        if let Some(rows) = streams.as_array() {
            for row in rows {
                let Some(sid) = str_field(row, &["stream_id"]) else {
                    continue;
                };
                let name = str_field(row, &["name", "title"]).unwrap_or_else(|| sid.clone());
                let ext =
                    str_field(row, &["container_extension"]).unwrap_or_else(|| "mp4".to_string());
                let group = row
                    .get("category_id")
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    })
                    .and_then(|id| categories.get(&id).cloned())
                    .unwrap_or_else(|| "Movies".to_string());
                let url = client.vod_stream_url(&sid, &ext);
                let item = playable_item(
                    name,
                    group.clone(),
                    url,
                    Some(provider_id.to_string()),
                    Some(source_label.to_string()),
                    "movie",
                    &sid,
                );
                catalog.add(&format!("Movies — {group}"), item);
            }
        }
    }
    if let Ok(series_rows) = client.series(None) {
        let categories = category_map(&client.series_categories().unwrap_or(Value::Null));
        if let Some(rows) = series_rows.as_array() {
            for row in rows {
                let Some(sid) = str_field(row, &["series_id"]) else {
                    continue;
                };
                let name = str_field(row, &["name", "title"]).unwrap_or_else(|| sid.clone());
                let category = row
                    .get("category_id")
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    })
                    .and_then(|id| categories.get(&id).cloned())
                    .unwrap_or_else(|| "Series".to_string());
                catalog.add(
                    &format!("Series — {category}"),
                    VodItem {
                        kind: VodKind::Series,
                        name,
                        group: category,
                        channel: None,
                        series_id: Some(sid),
                        provider_id: Some(provider_id.to_string()),
                        source_id: Some(provider_id.to_string()),
                        source_label: Some(source_label.to_string()),
                        episodes: Vec::new(),
                        season_episode: None,
                    },
                );
            }
        }
    }
    catalog
}

/// Expand one Xtream series into playable episodes (port of
/// `xtream_series_episodes`): season maps or lists, episode numbers
/// from the row or the position, labels `S01E02 - Title`.
///
/// # Errors
///
/// Returns the provider error when the info request fails.
pub fn xtream_series_episodes(
    client: &XtreamClient,
    series_id: &str,
    provider_id: &str,
    source_label: &str,
) -> Result<Vec<VodItem>, String> {
    let info = client.series_info(series_id)?;
    let empty = Value::Object(serde_json::Map::new());
    let episodes_node = info.get("episodes").unwrap_or(&empty);
    let mut episodes: Vec<VodItem> = Vec::new();
    let mut push_season = |season_no: u32, entries: &Vec<Value>| {
        for (idx, ep) in entries.iter().enumerate() {
            let Some(eid) = str_field(ep, &["id"]) else {
                continue;
            };
            let e_no = str_field(ep, &["episode_num"])
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or_else(|| u32::try_from(idx + 1).unwrap_or(0));
            let ext = str_field(ep, &["container_extension"]).unwrap_or_else(|| "mp4".to_string());
            let title = str_field(ep, &["title"]).unwrap_or_else(|| format!("Episode {e_no}"));
            let label = format!("S{season_no:02}E{e_no:02} - {title}");
            let url = client.series_stream_url(&eid, &ext);
            let mut item = playable_item(
                label,
                String::new(),
                url,
                Some(provider_id.to_string()),
                Some(source_label.to_string()),
                "episode",
                &eid,
            );
            if let Some(channel) = &mut item.channel {
                channel
                    .unknown_attributes
                    .insert("series_id".to_string(), series_id.to_string());
            }
            item.season_episode = Some((season_no, e_no));
            item.series_id = Some(series_id.to_string());
            episodes.push(item);
        }
    };
    match episodes_node {
        Value::Object(map) => {
            for (key, entries) in map {
                let season_no = if key.chars().all(|c| c.is_ascii_digit()) && !key.is_empty() {
                    key.parse::<u32>().unwrap_or(0)
                } else {
                    0
                };
                if let Some(list) = entries.as_array() {
                    push_season(season_no, list);
                }
            }
        }
        Value::Array(seasons) => {
            for (i, entries) in seasons.iter().enumerate() {
                if let Some(list) = entries.as_array() {
                    push_season(u32::try_from(i + 1).unwrap_or(0), list);
                }
            }
        }
        _ => {}
    }
    sort_episodes(&mut episodes);
    Ok(episodes)
}

/// Merge catalogues, preserving group order (port of `merge_catalogs`).
#[must_use]
pub fn merge_catalogs(catalogs: Vec<VodCatalog>) -> VodCatalog {
    let mut merged = VodCatalog::default();
    for catalog in catalogs {
        for label in &catalog.order {
            if let Some(items) = catalog.groups.get(label) {
                for item in items.clone() {
                    merged.add(label, item);
                }
            }
        }
    }
    merged
}

/// The label used when no safe source label can be derived
/// (spec 0006).
pub const GENERIC_SOURCE_LABEL: &str = "Unknown source";

/// The short type word for rows and details.
#[must_use]
pub fn kind_label(kind: VodKind) -> &'static str {
    match kind {
        VodKind::Movie => "Movie",
        VodKind::Series => "Series",
    }
}

/// One top-level search result (spec 0006): the item's group and
/// its position in that group, plus the display fields. Episodes
/// are never top-level results; they stay inside series drill-in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VodSearchHit {
    pub group: String,
    pub index: usize,
    pub name: String,
    pub kind: VodKind,
    pub source_label: Option<String>,
}

impl VodSearchHit {
    /// The accessible row text: title and type first (the short
    /// announcement), then group and source.
    #[must_use]
    pub fn row_label(&self) -> String {
        let mut label = format!("{}, {}", self.name, kind_label(self.kind));
        if !self.group.is_empty() {
            label.push_str(", ");
            label.push_str(&self.group);
        }
        if let Some(source) = &self.source_label {
            label.push_str(", ");
            label.push_str(source);
        }
        label
    }
}

/// Search the loaded catalogue snapshot (spec 0006): a
/// case-insensitive substring match on top-level movie and series
/// titles across all groups, in catalogue group order and item
/// order. The query is trimmed; a blank query yields no hits
/// (the adapter treats it as clearing the filter). The catalogue
/// is not mutated.
#[must_use]
pub fn search_catalog(catalog: &VodCatalog, query: &str) -> Vec<VodSearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for group in &catalog.order {
        let Some(items) = catalog.groups.get(group) else {
            continue;
        };
        for (index, item) in items.iter().enumerate() {
            if item.name.to_lowercase().contains(&needle) {
                hits.push(VodSearchHit {
                    group: group.clone(),
                    index,
                    name: item.name.clone(),
                    kind: item.kind,
                    source_label: item.source_label.clone(),
                });
            }
        }
    }
    hits
}

/// The detail surface for one item (spec 0006): only fields that
/// are actually available. The stream URL appears here and only
/// here; the caller supplies the EPG description when one is
/// already available for the item's channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VodDetails {
    pub title: String,
    pub kind: VodKind,
    pub group: String,
    pub source_label: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

impl VodDetails {
    /// Labelled lines for the read-only detail control, omitting
    /// unavailable fields.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Title: {}", self.title),
            format!("Type: {}", kind_label(self.kind)),
        ];
        if !self.group.is_empty() {
            lines.push(format!("Group: {}", self.group));
        }
        if let Some(source) = &self.source_label {
            lines.push(format!("Source: {source}"));
        }
        if let Some(description) = &self.description {
            lines.push(format!("Description: {description}"));
        }
        if let Some(url) = &self.url {
            lines.push(format!("URL: {url}"));
        }
        lines
    }
}

/// Build the detail surface for `item`.
#[must_use]
pub fn item_details(item: &VodItem, epg_description: Option<&str>) -> VodDetails {
    VodDetails {
        title: item.name.clone(),
        kind: item.kind,
        group: item.group.clone(),
        source_label: item.source_label.clone(),
        description: epg_description
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        url: item
            .channel
            .as_ref()
            .map(|channel| channel.url.clone())
            .filter(|url| !url.is_empty()),
    }
}

/// Stamp M3U source labels onto a merged catalogue from the
/// scope-id → label map the adapter derived from the configured
/// playlists. Items that already carry a label (Xtream) keep it;
/// an item whose source is missing from the map gets the generic
/// label. Episodes inherit through their own stamped identity.
fn stamp_source_labels(catalog: &mut VodCatalog, labels: &[(String, String)]) {
    let label_for = |item: &VodItem| -> Option<String> {
        if item.source_label.is_some() {
            return item.source_label.clone();
        }
        let id = item.source_id.as_ref()?;
        Some(labels.iter().find(|(scope, _)| scope == id).map_or_else(
            || GENERIC_SOURCE_LABEL.to_string(),
            |(_, label)| label.clone(),
        ))
    };
    for items in catalog.groups.values_mut() {
        for item in items.iter_mut() {
            item.source_label = label_for(item);
            for episode in &mut item.episodes {
                episode.source_label = label_for(episode);
            }
        }
    }
}

/// Jobs for the VOD worker.
pub enum VodJob {
    Build {
        generation: u64,
        accounts: Vec<ProviderAccount>,
        model: std::sync::Arc<crate::BrowseModel>,
        extras: Vec<Channel>,
        /// The active playlist scope id; None = all playlists (B17).
        scope: Option<String>,
        /// Scope-id → safe source label for M3U items (spec 0006),
        /// derived by the adapter from the configured playlists.
        source_labels: Vec<(String, String)>,
    },
    Search {
        generation: u64,
        query: String,
    },
    Episodes {
        token: (u64, u64),
        account: ProviderAccount,
        series_id: String,
    },
}

/// Events from the VOD worker (B18): catalogue events carry their
/// build generation; episode events carry a (build, navigation)
/// token. The adapter drops anything no longer current, so a late
/// reply can never replace newer navigation.
#[derive(Clone, Debug)]
pub enum VodEvent {
    Catalog {
        generation: u64,
        catalog: VodCatalog,
    },
    SearchResults {
        generation: u64,
        query: String,
        hits: Vec<VodSearchHit>,
    },
    Episodes {
        token: (u64, u64),
        result: Result<Vec<VodItem>, String>,
    },
}

/// Assemble one build's catalogues on the worker thread (B17/B19):
/// only the scoped source contributes, provider channels never enter
/// the plain-M3U heuristic, and only scoped Xtream accounts are
/// queried.
fn build_catalog(
    accounts: &[ProviderAccount],
    model: &crate::BrowseModel,
    extras: &[Channel],
    scope: Option<&str>,
    source_labels: &[(String, String)],
) -> VodCatalog {
    let in_scope = |channel: &Channel| match scope {
        None => true,
        Some(id) => crate::sources::channel_scope(channel) == Some(id),
    };
    let mut catalogs: Vec<VodCatalog> = Vec::new();
    for account in accounts {
        if scope.is_some() && account.provider_id().as_deref() != scope {
            continue;
        }
        if let ProviderAccount::Xtream(creds) = account {
            let client = XtreamClient::new(creds.clone());
            let pid = account.provider_id().unwrap_or_else(|| account.describe());
            catalogs.push(xtream_catalog(&client, &pid, &account.source_label()));
        }
    }
    let m3u_channels: Vec<Channel> = model
        .channels()
        .iter()
        .chain(extras.iter())
        .filter(|channel| in_scope(channel))
        .filter(|channel| {
            !matches!(
                channel
                    .unknown_attributes
                    .get("provider-type")
                    .map(String::as_str),
                Some("xtream" | "stalker")
            )
        })
        .cloned()
        .collect();
    if !m3u_channels.is_empty() {
        catalogs.push(categorize_m3u_vod(&m3u_channels));
    }
    let mut merged = merge_catalogs(catalogs);
    stamp_source_labels(&mut merged, source_labels);
    merged
}

/// Background VOD catalogue/episodes worker.
pub struct VodWorker {
    jobs: std::sync::mpsc::Sender<VodJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<VodEvent>>,
    /// Catalogue build generation: a newer build supersedes older
    /// builds and every episode request made against them.
    build_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Navigation generation: group changes, Back and window close
    /// supersede in-flight episode replies without touching an
    /// in-flight catalogue build.
    nav_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Default for VodWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl VodWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<VodJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<VodEvent>();
        // The newest built catalogue, retained so searches run on
        // this thread against the loaded snapshot (spec 0006).
        let retained: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<VodCatalog>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let retained_in_thread = std::sync::Arc::clone(&retained);
        std::thread::spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                let event = match job {
                    VodJob::Build {
                        generation,
                        accounts,
                        model,
                        extras,
                        scope,
                        source_labels,
                    } => {
                        let catalog = build_catalog(
                            &accounts,
                            &model,
                            &extras,
                            scope.as_deref(),
                            &source_labels,
                        );
                        if let Ok(mut slot) = retained_in_thread.lock() {
                            *slot = Some(std::sync::Arc::new(catalog.clone()));
                        }
                        VodEvent::Catalog {
                            generation,
                            catalog,
                        }
                    }
                    VodJob::Search { generation, query } => {
                        let hits = retained_in_thread
                            .lock()
                            .ok()
                            .and_then(|slot| {
                                slot.as_ref().map(|catalog| search_catalog(catalog, &query))
                            })
                            .unwrap_or_default();
                        VodEvent::SearchResults {
                            generation,
                            query,
                            hits,
                        }
                    }
                    VodJob::Episodes {
                        token,
                        account,
                        series_id,
                    } => {
                        let result = match &account {
                            ProviderAccount::Xtream(creds) => {
                                let client = XtreamClient::new(creds.clone());
                                let pid =
                                    account.provider_id().unwrap_or_else(|| account.describe());
                                xtream_series_episodes(
                                    &client,
                                    &series_id,
                                    &pid,
                                    &account.source_label(),
                                )
                            }
                            ProviderAccount::Stalker(_) => {
                                Err("Series episodes need an Xtream account.".to_string())
                            }
                        };
                        VodEvent::Episodes { token, result }
                    }
                };
                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs: job_sender,
            events: std::sync::Mutex::new(event_receiver),
            build_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            nav_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn bump(counter: &std::sync::atomic::AtomicU64) -> u64 {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// Whether a catalogue event is from the newest build.
    #[must_use]
    pub fn is_current_catalog(&self, generation: u64) -> bool {
        generation
            == self
                .build_generation
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether an episode event's token is still current: same
    /// build, and navigation has not moved since the request.
    #[must_use]
    pub fn is_current_episodes(&self, token: (u64, u64)) -> bool {
        token.0
            == self
                .build_generation
                .load(std::sync::atomic::Ordering::Relaxed)
            && token.1
                == self
                    .nav_generation
                    .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Navigation moved on (group change, Back, window close):
    /// in-flight episode replies become stale.
    pub fn invalidate(&self) {
        Self::bump(&self.nav_generation);
    }

    pub fn build(
        &self,
        accounts: Vec<ProviderAccount>,
        model: std::sync::Arc<crate::BrowseModel>,
        extras: Vec<Channel>,
        scope: Option<String>,
        source_labels: Vec<(String, String)>,
    ) {
        let generation = Self::bump(&self.build_generation);
        // A rebuild also supersedes episode requests in flight.
        Self::bump(&self.nav_generation);
        let _ = self.jobs.send(VodJob::Build {
            generation,
            accounts,
            model,
            extras,
            scope,
            source_labels,
        });
    }

    /// Search the retained catalogue snapshot on the worker
    /// thread (spec 0006). The reply carries the current build
    /// generation, so a rebuild supersedes it like any other
    /// catalogue work.
    pub fn search(&self, query: String) {
        let generation = self
            .build_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let _ = self.jobs.send(VodJob::Search { generation, query });
    }

    pub fn episodes(&self, account: ProviderAccount, series_id: String) {
        let token = (
            self.build_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            Self::bump(&self.nav_generation),
        );
        let _ = self.jobs.send(VodJob::Episodes {
            token,
            account,
            series_id,
        });
    }

    /// The next VOD event, when one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<VodEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, group: &str, url: &str) -> Channel {
        Channel {
            name: name.to_string(),
            group: group.to_string(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn season_episode_parsing_matches_reference() {
        assert_eq!(parse_season_episode("Show S01E02 - Pilot"), Some((1, 2)));
        assert_eq!(parse_season_episode("Show 2x05"), Some((2, 5)));
        assert_eq!(parse_season_episode("Show Episode 7"), Some((0, 7)));
        assert_eq!(parse_season_episode("Plain Movie"), None);
        assert_eq!(series_title("Breaking Bad S01E02 - Pilot"), "Breaking Bad");
        assert_eq!(series_title("Plain Movie"), "Plain Movie");
    }

    fn scoped_channel(name: &str, group: &str, url: &str, scope: &str, provider: bool) -> Channel {
        let mut channel = channel(name, group, url);
        channel.unknown_attributes.insert(
            crate::sources::PLAYLIST_ID_ATTRIBUTE.to_string(),
            scope.to_string(),
        );
        if provider {
            channel
                .unknown_attributes
                .insert("provider-type".to_string(), "xtream".to_string());
        }
        channel
    }

    #[test]
    fn build_catalog_respects_scope_and_excludes_provider_channels() {
        // B17 regression: one scoped playlist contributes; provider
        // channels never enter the plain-M3U heuristic.
        let model = crate::BrowseModel::from_parts(
            vec![
                scoped_channel("Film A", "Movies", "http://x/1", "m3u:a", false),
                scoped_channel("Film B", "Movies", "http://x/2", "m3u:b", false),
                scoped_channel("Provider Film", "Movies", "http://x/3", "m3u:a", true),
            ],
            0,
        );
        let scoped = build_catalog(&[], &model, &[], Some("m3u:a"), &[]);
        let movies = &scoped.groups["Movies"];
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].name, "Film A");
        let all = build_catalog(&[], &model, &[], None, &[]);
        let names: Vec<&str> = all.groups["Movies"]
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, ["Film A", "Film B"], "provider film excluded");
    }

    #[test]
    fn worker_events_carry_generations_and_invalidate_stales_them() {
        // B18 regression (core half): a superseded build's event is
        // recognizable as stale; navigation invalidates episode
        // tokens but not an in-flight catalogue build.
        let worker = VodWorker::new();
        let model = std::sync::Arc::new(crate::BrowseModel::from_parts(
            vec![channel("Film", "Movies", "http://x/1")],
            0,
        ));
        worker.build(
            Vec::new(),
            std::sync::Arc::clone(&model),
            Vec::new(),
            None,
            Vec::new(),
        );
        let first = loop {
            if let Some(event) = worker.try_recv() {
                break event;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let VodEvent::Catalog { generation, .. } = first else {
            panic!("expected a catalogue event");
        };
        assert!(worker.is_current_catalog(generation));
        worker.build(Vec::new(), model, Vec::new(), None, Vec::new());
        assert!(
            !worker.is_current_catalog(generation),
            "newer build supersedes"
        );
        // Episode token mechanics: a token is current until
        // navigation moves or a rebuild lands.
        let worker2 = VodWorker::new();
        worker2.build(
            Vec::new(),
            std::sync::Arc::new(crate::BrowseModel::default()),
            Vec::new(),
            None,
            Vec::new(),
        );
        let token = (
            worker2
                .build_generation
                .load(std::sync::atomic::Ordering::Relaxed),
            worker2
                .nav_generation
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        assert!(worker2.is_current_episodes(token));
        worker2.invalidate();
        assert!(!worker2.is_current_episodes(token));
    }

    #[test]
    fn m3u_heuristic_builds_series_only_when_genuine() {
        let channels = vec![
            channel("Show S01E01", "Series", "http://x/1"),
            channel("Show S01E02", "Series", "http://x/2"),
            channel("One-off S01E01", "Series", "http://x/3"),
            channel("A Film", "Movies", "http://x/4"),
            channel("News", "News", "http://x/5"),
        ];
        let catalog = categorize_m3u_vod(&channels);
        let series_group = catalog.groups.get("Series").expect("series group");
        let folders: Vec<&VodItem> = series_group
            .iter()
            .filter(|i| i.kind == VodKind::Series)
            .collect();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Show");
        assert_eq!(folders[0].episodes.len(), 2);
        assert!(catalog.groups.contains_key("Movies"));
        assert!(!catalog.groups.contains_key("News"));
    }

    fn discovery_channels() -> Vec<Channel> {
        vec![
            scoped_channel(
                "Dune",
                "Movies",
                "http://user:secret@films.example.com/dune.mp4",
                "m3u:a",
                false,
            ),
            scoped_channel(
                "Dune Part Two",
                "Movies",
                "http://films.example.com/dune2.mp4",
                "m3u:a",
                false,
            ),
            scoped_channel(
                "The Dunwich Horror",
                "Movies",
                "http://other.example.org/dunwich.mp4",
                "m3u:b",
                false,
            ),
            scoped_channel("Dune S01E01", "Series", "http://x/d1", "m3u:a", false),
            scoped_channel("Dune S01E02", "Series", "http://x/d2", "m3u:a", false),
            scoped_channel("Other Film", "Movies", "http://x/o", "m3u:b", false),
        ]
    }

    #[test]
    fn search_matches_top_level_items_across_groups() {
        // Spec 0006: case-insensitive substring, trimmed, all
        // groups, catalogue order preserved; episode titles are
        // not top-level results.
        let catalog = categorize_m3u_vod(&discovery_channels());
        let hits = search_catalog(&catalog, "  DUNE ");
        let names: Vec<&str> = hits.iter().map(|hit| hit.name.as_str()).collect();
        assert_eq!(names, ["Dune", "Dune Part Two", "Dune"], "{hits:?}");
        // The third hit is the series folder, not an episode.
        assert_eq!(hits[2].kind, VodKind::Series);
        assert_eq!(hits[2].group, "Series");
        assert!(search_catalog(&catalog, "s01e01").is_empty());
        assert!(search_catalog(&catalog, "").is_empty());
        assert!(search_catalog(&catalog, "   ").is_empty());
        assert!(search_catalog(&catalog, "absent").is_empty());
        // Searching does not mutate the catalogue.
        assert_eq!(catalog.groups["Movies"].len(), 4);
    }

    #[test]
    fn m3u_source_identity_and_labels_propagate() {
        // Spec 0006: movies, series folders, and episodes carry
        // the playlist scope id (never the stream URL); labels
        // come from the adapter's scope map, generic when unmapped,
        // and never contain URL credentials.
        let model = crate::BrowseModel::from_parts(discovery_channels(), 0);
        let labels = vec![("m3u:a".to_string(), "My Films".to_string())];
        let catalog = build_catalog(&[], &model, &[], None, &labels);
        let movies = &catalog.groups["Movies"];
        let dune = &movies[0];
        assert_eq!(dune.source_id.as_deref(), Some("m3u:a"));
        assert_eq!(dune.source_label.as_deref(), Some("My Films"));
        let dunwich = movies
            .iter()
            .find(|item| item.name == "The Dunwich Horror")
            .unwrap();
        assert_eq!(dunwich.source_id.as_deref(), Some("m3u:b"));
        assert_eq!(
            dunwich.source_label.as_deref(),
            Some(GENERIC_SOURCE_LABEL),
            "unmapped scope falls back to the generic label"
        );
        let series = &catalog.groups["Series"][0];
        assert_eq!(series.source_id.as_deref(), Some("m3u:a"));
        assert_eq!(series.source_label.as_deref(), Some("My Films"));
        assert!(series
            .episodes
            .iter()
            .all(|ep| ep.source_id.as_deref() == Some("m3u:a")
                && ep.source_label.as_deref() == Some("My Films")));
        // No label or row text carries the credential-bearing URL.
        let hit = &search_catalog(&catalog, "dune")[0];
        assert!(!hit.row_label().contains("secret"), "{}", hit.row_label());
        assert!(!hit.row_label().contains("user"), "{}", hit.row_label());
    }

    #[test]
    fn details_carry_only_available_fields_and_the_url() {
        // Spec 0006: the URL lives in the detail surface only;
        // unavailable fields are omitted, not invented.
        let catalog = categorize_m3u_vod(&discovery_channels());
        let movie = &catalog.groups["Movies"][0];
        let details = item_details(movie, Some("  A desert epic. "));
        assert_eq!(
            details.url.as_deref(),
            Some(movie.channel.as_ref().unwrap().url.as_str())
        );
        assert_eq!(details.description.as_deref(), Some("A desert epic."));
        let lines = details.lines();
        assert!(
            lines.iter().any(|line| line.starts_with("URL: http")),
            "{lines:?}"
        );
        let series = &catalog.groups["Series"][0];
        let folder_details = item_details(series, None);
        assert!(folder_details.url.is_none(), "folders have no stream URL");
        assert!(folder_details.description.is_none());
        assert_eq!(folder_details.lines().len(), 3, "title, type, group only");
    }

    #[test]
    fn worker_search_answers_from_the_retained_catalogue() {
        // Spec 0006: searches run on the worker against the built
        // snapshot and carry the build generation.
        let worker = VodWorker::new();
        let model = std::sync::Arc::new(crate::BrowseModel::from_parts(discovery_channels(), 0));
        worker.build(Vec::new(), model, Vec::new(), None, Vec::new());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut catalog_generation = None;
        while std::time::Instant::now() < deadline {
            if let Some(VodEvent::Catalog { generation, .. }) = worker.try_recv() {
                catalog_generation = Some(generation);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let generation = catalog_generation.expect("catalogue event");
        worker.search("dune".to_string());
        let mut found = None;
        while std::time::Instant::now() < deadline {
            if let Some(VodEvent::SearchResults {
                generation: gen,
                query,
                hits,
            }) = worker.try_recv()
            {
                found = Some((gen, query, hits));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (gen, query, hits) = found.expect("search results");
        assert_eq!(gen, generation);
        assert_eq!(query, "dune");
        assert_eq!(hits.len(), 3);
    }
}
