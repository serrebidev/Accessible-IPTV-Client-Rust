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

fn playable_item(name: String, group: String, url: String, provider_id: Option<String>) -> VodItem {
    let channel = Channel {
        name: name.clone(),
        group: group.clone(),
        url,
        tvg_id: String::new(),
        tvg_name: String::new(),
        tvg_logo: String::new(),
        unknown_attributes: BTreeMap::new(),
    };
    VodItem {
        kind: VodKind::Movie,
        name,
        group,
        channel: Some(channel),
        series_id: None,
        provider_id,
        episodes: Vec::new(),
        season_episode: None,
    }
}

/// Build one Xtream provider's catalogue (port of
/// `build_xtream_catalog`): movies eagerly, series lazily. Section
/// failures skip that section, like the reference.
#[must_use]
pub fn xtream_catalog(client: &XtreamClient, provider_id: &str) -> VodCatalog {
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
                let item = playable_item(name, group.clone(), url, Some(provider_id.to_string()));
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
            let mut item = playable_item(label, String::new(), url, Some(provider_id.to_string()));
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

/// Jobs for the VOD worker.
pub enum VodJob {
    Build {
        accounts: Vec<ProviderAccount>,
        channels: Vec<Channel>,
    },
    Episodes {
        account: ProviderAccount,
        series_id: String,
    },
}

/// Events from the VOD worker.
#[derive(Clone, Debug)]
pub enum VodEvent {
    Catalog(VodCatalog),
    Episodes(Result<Vec<VodItem>, String>),
}

/// Background VOD catalogue/episodes worker.
pub struct VodWorker {
    jobs: std::sync::mpsc::Sender<VodJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<VodEvent>>,
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
        std::thread::spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                let event = match job {
                    VodJob::Build { accounts, channels } => {
                        let mut catalogs: Vec<VodCatalog> = Vec::new();
                        for account in &accounts {
                            if let ProviderAccount::Xtream(creds) = account {
                                let client = XtreamClient::new(creds.clone());
                                let pid =
                                    account.provider_id().unwrap_or_else(|| account.describe());
                                catalogs.push(xtream_catalog(&client, &pid));
                            }
                        }
                        if !channels.is_empty() {
                            catalogs.push(categorize_m3u_vod(&channels));
                        }
                        VodEvent::Catalog(merge_catalogs(catalogs))
                    }
                    VodJob::Episodes { account, series_id } => {
                        let result = match &account {
                            ProviderAccount::Xtream(creds) => {
                                let client = XtreamClient::new(creds.clone());
                                let pid =
                                    account.provider_id().unwrap_or_else(|| account.describe());
                                xtream_series_episodes(&client, &series_id, &pid)
                            }
                            ProviderAccount::Stalker(_) => {
                                Err("Series episodes need an Xtream account.".to_string())
                            }
                        };
                        VodEvent::Episodes(result)
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
        }
    }

    pub fn build(&self, accounts: Vec<ProviderAccount>, channels: Vec<Channel>) {
        let _ = self.jobs.send(VodJob::Build { accounts, channels });
    }

    pub fn episodes(&self, account: ProviderAccount, series_id: String) {
        let _ = self.jobs.send(VodJob::Episodes { account, series_id });
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
}
