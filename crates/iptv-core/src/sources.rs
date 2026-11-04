//! Playlist sources and multi-source merging, porting the Python model.
//!
//! A configured source is a string: a local path or an http(s) URL. Every
//! channel a source produces is tagged `playlist-id` with the source's
//! scope id (`m3u:` + SHA-256 of the source string, as in Python's
//! `_source_scope_id`). The merged view concatenates sources in order —
//! the reference does not dedupe channels across playlists. The scope
//! filter mirrors `_scoped_channels`: the empty scope (All playlists)
//! accepts everything; a specific scope accepts only channels whose tag
//! matches exactly, so untagged legacy channels stay out of it.
//!
//! Fetch failures are per-source: one dead source never sinks the others.
//! Only when every source fails does the load report a failure (the first).

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::loading::LoadFailure;
use crate::{parse_cancellable, BrowseModel, Channel};

/// The channel attribute that carries the owning source's scope id.
pub const PLAYLIST_ID_ATTRIBUTE: &str = "playlist-id";

/// One configured playlist source: a local path or an http(s) URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistSource {
    location: String,
}

impl PlaylistSource {
    #[must_use]
    pub fn new(location: impl Into<String>) -> Self {
        Self {
            location: location.into(),
        }
    }

    #[must_use]
    pub fn location(&self) -> &str {
        &self.location
    }

    #[must_use]
    pub fn is_url(&self) -> bool {
        self.location.starts_with("http://") || self.location.starts_with("https://")
    }

    /// Python `_source_scope_id` for string sources.
    #[must_use]
    pub fn scope_id(&self) -> String {
        use std::fmt::Write as _;
        let digest = Sha256::digest(self.location.as_bytes());
        let mut id = String::with_capacity(68);
        id.push_str("m3u:");
        for byte in digest {
            let _ = write!(id, "{byte:02x}");
        }
        id
    }
}

/// Fetches one source's playlist text. Implementations must stay off the
/// UI thread and honor the cancellation probe between steps; an
/// in-progress transfer cannot be interrupted, matching the local reader.
pub trait SourceFetcher: Send + Sync {
    /// # Errors
    ///
    /// Returns the UI-neutral failure kind for this source.
    fn fetch(
        &self,
        source: &PlaylistSource,
        current: &dyn Fn() -> bool,
    ) -> Result<String, LoadFailure>;
}

/// Local files through the existing decoding rules, URLs over HTTP.
pub struct DefaultFetcher;

impl SourceFetcher for DefaultFetcher {
    fn fetch(
        &self,
        source: &PlaylistSource,
        current: &dyn Fn() -> bool,
    ) -> Result<String, LoadFailure> {
        if source.is_url() {
            fetch_http(source.location(), current)
        } else {
            let model_path = Path::new(source.location());
            match crate::loading::read_local_text(model_path, current) {
                Ok(Some(text)) => Ok(text),
                Ok(None) => Err(LoadFailure::Unreadable),
                Err(error) => Err(if error.kind() == std::io::ErrorKind::NotFound {
                    LoadFailure::Missing
                } else {
                    LoadFailure::Unreadable
                }),
            }
        }
    }
}

/// Reads a whole HTTP body without ureq's default 10 MiB
/// `read_to_string` / `read_to_vec` cap. Real playlists and
/// guides run far past it (a 115k-channel playlist is ~28 MB,
/// its XMLTV ~263 MB) and Python imposes no cap, so the body
/// streams through the unbounded reader instead.
pub(crate) fn read_body_unbounded(body: &mut ureq::Body) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    body.as_reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn fetch_http(url: &str, current: &dyn Fn() -> bool) -> Result<String, LoadFailure> {
    if !current() {
        return Err(LoadFailure::Unreadable);
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_mins(5)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("User-Agent", "AccessibleIPTVClient")
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(404) => LoadFailure::Missing,
            _ => LoadFailure::Unreadable,
        })?;
    if !current() {
        return Err(LoadFailure::Unreadable);
    }
    let bytes = read_body_unbounded(response.body_mut()).map_err(|_| LoadFailure::Unreadable)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The merged outcome: the combined model plus how many sources failed.
#[derive(Debug)]
pub struct MergedLoad {
    pub model: BrowseModel,
    pub failed_sources: usize,
    /// Locations of sources that failed this load and whose previous
    /// channels were retained instead (B12).
    pub stale_sources: Vec<String>,
    /// Playlist-header guide URLs per source actually read this load
    /// (B09): (source location, guides). A read source replaces its
    /// previous entry even with an empty list.
    pub guide_urls: Vec<(String, Vec<String>)>,
}

/// Knobs for [`load_merged_with`]: the on-disk caches (B11), the
/// previous catalogue for per-source retention (B12), and a forced
/// refresh that bypasses the URL download cache (B13).
#[derive(Clone, Copy, Default)]
pub struct MergeOptions<'a> {
    pub cache_dir: Option<&'a Path>,
    pub previous: &'a [Channel],
    pub force_refresh: bool,
}

/// Fetch and parse every source in order, tagging channels with their
/// scope id. Returns `Ok(None)` when cancelled.
///
/// # Errors
///
/// Returns the first source's failure when every source failed and no
/// channel or malformed entry was produced at all.
pub fn load_merged(
    sources: &[PlaylistSource],
    fetcher: &dyn SourceFetcher,
    current: impl Fn() -> bool,
) -> Result<Option<MergedLoad>, LoadFailure> {
    load_merged_with(sources, fetcher, MergeOptions::default(), current)
}

/// [`load_merged`] with caches, retention and refresh control.
///
/// Identical entries within one source are deduplicated (B21) by
/// (scope, name, URL, provider id); entries from distinct sources
/// are never merged away.
///
/// # Errors
///
/// Returns the first source's failure when nothing at all could be
/// produced: no channels, no malformed entries, nothing retained.
pub fn load_merged_with(
    sources: &[PlaylistSource],
    fetcher: &dyn SourceFetcher,
    options: MergeOptions<'_>,
    current: impl Fn() -> bool,
) -> Result<Option<MergedLoad>, LoadFailure> {
    let mut channels: Vec<Channel> = Vec::new();
    let mut malformed = 0usize;
    let mut failed = 0usize;
    let mut first_failure: Option<LoadFailure> = None;
    let mut stale_sources: Vec<String> = Vec::new();
    let mut guide_urls: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();
    let mut push_unique = |channels: &mut Vec<Channel>, parsed: Vec<Channel>, scope: &str| {
        for channel in parsed {
            let key = (
                scope.to_string(),
                channel.name.clone(),
                channel.url.clone(),
                channel
                    .unknown_attributes
                    .get("provider-id")
                    .cloned()
                    .unwrap_or_default(),
            );
            if seen.insert(key) {
                channels.push(channel);
            }
        }
    };
    for source in sources {
        if !current() {
            return Ok(None);
        }
        let scope = source.scope_id();
        match load_source_text(source, fetcher, &options, &current) {
            Ok(Some(text)) => {
                guide_urls.push((
                    source.location().to_string(),
                    crate::playlist_header_epg_urls(&text),
                ));
                let Some(mut parsed) = parse_source_text(source, &text, &options, &current) else {
                    return Ok(None);
                };
                malformed += parsed.1;
                for channel in &mut parsed.0 {
                    channel
                        .unknown_attributes
                        .insert(PLAYLIST_ID_ATTRIBUTE.to_string(), scope.clone());
                }
                push_unique(&mut channels, parsed.0, &scope);
            }
            Ok(None) => return Ok(None),
            Err(failure) => {
                // B12: a source that cannot be fetched keeps the
                // channels it had; only a source with nothing to
                // retain counts as failed.
                let retained: Vec<Channel> = options
                    .previous
                    .iter()
                    .filter(|channel| channel_scope(channel) == Some(scope.as_str()))
                    .cloned()
                    .collect();
                if retained.is_empty() {
                    failed += 1;
                    first_failure.get_or_insert(failure);
                } else {
                    stale_sources.push(source.location().to_string());
                    push_unique(&mut channels, retained, &scope);
                }
            }
        }
    }
    if channels.is_empty() && malformed == 0 {
        if let Some(failure) = first_failure {
            return Err(failure);
        }
    }
    Ok(Some(MergedLoad {
        model: BrowseModel::from_parts(channels, malformed),
        failed_sources: failed,
        stale_sources,
        guide_urls,
    }))
}

/// Where one URL source's downloaded text is cached (B11).
#[must_use]
pub fn url_download_cache_path(dir: &Path, location: &str) -> std::path::PathBuf {
    crate::cache::file_for(dir, &crate::cache::cache_key(&["url-download", location]))
}

/// Drop one URL source's downloaded copy so the next load refetches
/// it (B13 refresh: Python deletes the cache file for the refreshed
/// sources and leaves the others' caches alone).
pub fn drop_url_download_cache(dir: &Path, location: &str) {
    let _ = std::fs::remove_file(url_download_cache_path(dir, location));
}

/// One source's playlist text: the 15-minute URL download cache
/// first (B11, skipped on a forced refresh), then the fetcher,
/// refreshing the download cache with what came back.
fn load_source_text(
    source: &PlaylistSource,
    fetcher: &dyn SourceFetcher,
    options: &MergeOptions<'_>,
    current: &dyn Fn() -> bool,
) -> Result<Option<String>, LoadFailure> {
    if !current() {
        return Ok(None);
    }
    if source.is_url() {
        if let Some(dir) = options.cache_dir {
            let path = url_download_cache_path(dir, source.location());
            if !options.force_refresh {
                if let Some(text) = crate::cache::read(
                    &path,
                    Some(std::time::Duration::from_mins(15)),
                    std::time::SystemTime::now(),
                ) {
                    return Ok(Some(text));
                }
            }
            let text = fetcher.fetch(source, current)?;
            let _ = crate::cache::write(&path, &text);
            return Ok(Some(text));
        }
    }
    fetcher.fetch(source, current).map(Some)
}

/// Parse one source's text, reusing the parsed cache when the text
/// hash still matches (B11) and storing fresh parses. Returns
/// (channels, malformed); None when cancelled.
fn parse_source_text(
    source: &PlaylistSource,
    text: &str,
    options: &MergeOptions<'_>,
    current: &dyn Fn() -> bool,
) -> Option<(Vec<Channel>, usize)> {
    let Some(dir) = options.cache_dir else {
        return parse_cancellable(text, current).map(BrowseModel::into_parts);
    };
    let hash = playlist_text_hash(text);
    let path = parsed_cache_path(dir, source);
    if let Some(cached) = read_parsed_cache(&path, &hash) {
        return Some(cached);
    }
    let model = parse_cancellable(text, current)?;
    let (channels, malformed) = model.into_parts();
    write_parsed_cache(&path, &hash, &channels, malformed);
    Some((channels, malformed))
}

/// Salts the parsed-cache hash so a parser change cannot be hidden
/// behind channels cached by an older build (Python's version "5"
/// plays the same role). Bump when parse output changes shape.
const PARSED_CACHE_VERSION: &str = "rust-parse-1";

fn playlist_text_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(PARSED_CACHE_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(text.as_bytes());
    crate::hex_lower(&hasher.finalize())
}

fn parsed_cache_path(dir: &Path, source: &PlaylistSource) -> std::path::PathBuf {
    let key = if source.is_url() {
        source.location().to_string()
    } else {
        format!("file:{}", source.location())
    };
    crate::cache::file_for(dir, &crate::cache::cache_key(&["parsed-playlist", &key]))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ParsedCache {
    hash: String,
    malformed: usize,
    channels: Vec<Channel>,
}

fn read_parsed_cache(path: &Path, expected_hash: &str) -> Option<(Vec<Channel>, usize)> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached: ParsedCache = serde_json::from_str(&text).ok()?;
    if cached.hash != expected_hash {
        return None;
    }
    Some((cached.channels, cached.malformed))
}

fn read_parsed_cache_any(path: &Path) -> Option<(Vec<Channel>, usize)> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached: ParsedCache = serde_json::from_str(&text).ok()?;
    Some((cached.channels, cached.malformed))
}

fn write_parsed_cache(path: &Path, hash: &str, channels: &[Channel], malformed: usize) {
    let payload = ParsedCache {
        hash: hash.to_string(),
        malformed,
        channels: channels.to_vec(),
    };
    if let Ok(text) = serde_json::to_string(&payload) {
        let _ = crate::cache::write(path, &text);
    }
}

/// The parsed-cache prefill (B11): channels already parsed from a
/// previous load, tagged and deduplicated, displayable before any
/// network work happens. None when no source has a parsed cache.
#[must_use]
pub fn prefill_model(sources: &[PlaylistSource], cache_dir: &Path) -> Option<BrowseModel> {
    let mut channels: Vec<Channel> = Vec::new();
    let mut malformed = 0usize;
    let mut seen: std::collections::HashSet<(String, String, String, String)> =
        std::collections::HashSet::new();
    for source in sources {
        let path = parsed_cache_path(cache_dir, source);
        let Some((mut parsed, bad)) = read_parsed_cache_any(&path) else {
            continue;
        };
        malformed += bad;
        let scope = source.scope_id();
        for channel in &mut parsed {
            channel
                .unknown_attributes
                .insert(PLAYLIST_ID_ATTRIBUTE.to_string(), scope.clone());
        }
        for channel in parsed {
            let key = (
                scope.clone(),
                channel.name.clone(),
                channel.url.clone(),
                channel
                    .unknown_attributes
                    .get("provider-id")
                    .cloned()
                    .unwrap_or_default(),
            );
            if seen.insert(key) {
                channels.push(channel);
            }
        }
    }
    if channels.is_empty() && malformed == 0 {
        return None;
    }
    Some(BrowseModel::from_parts(channels, malformed))
}

/// The config key a plain source's friendly name is stored under
/// (B13): SHA-256 of the source string, so credentials in a URL
/// never become a mapping key verbatim (Python `source_name_key`).
#[must_use]
pub fn source_name_key(source: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::hex_lower(&Sha256::digest(source.as_bytes()))
}

/// Clean a stored friendly-name map (Python `normalize_source_names`):
/// string keys and values only, values trimmed, blanks dropped.
#[must_use]
pub fn normalize_source_names(
    names: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    names
        .iter()
        .map(|(key, value)| (key.clone(), value.trim().to_string()))
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

/// The display label for a plain source: its friendly name when one
/// is stored, else the URL host or file name (Python's label rule).
#[must_use]
pub fn source_display_label(
    source: &str,
    names: &std::collections::BTreeMap<String, String>,
) -> String {
    if let Some(name) = names.get(&source_name_key(source)) {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        let rest = source.split("://").nth(1).unwrap_or(source);
        let authority = rest.split('/').next().unwrap_or(rest);
        // Labels never carry URL credentials (spec 0006): drop any
        // userinfo before the host.
        return authority
            .rsplit('@')
            .next()
            .unwrap_or(authority)
            .to_string();
    }
    Path::new(source).file_name().map_or_else(
        || source.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The scope id a channel carries, if any.
#[must_use]
pub fn channel_scope(channel: &Channel) -> Option<&str> {
    channel
        .unknown_attributes
        .get(PLAYLIST_ID_ATTRIBUTE)
        .map(String::as_str)
}

/// Indexes of the channels visible under `scope`, in playlist order.
/// The empty scope is All playlists and accepts every channel.
#[must_use]
pub fn scope_indexes(channels: &[Channel], scope: &str) -> Vec<usize> {
    channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| scope.is_empty() || channel_scope(channel) == Some(scope))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {

    /// Serves `body` once over loopback HTTP and returns its URL.
    fn serve_once(body: Vec<u8>) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap() == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        format!("http://127.0.0.1:{port}/list.m3u")
    }

    #[test]
    fn http_playlist_over_ureq_default_cap_loads() {
        // Regression: ureq's read_to_string caps at 10 MiB, which
        // silently failed real playlists (28 MB for a 115k-channel
        // provider list). The fetch must take the whole body.
        let mut body = String::from("#EXTM3U\n");
        let line = "#EXTINF:-1 tvg-id=\"1\",Channel One\nhttp://x/1\n";
        while body.len() <= 11 * 1024 * 1024 {
            body.push_str(line);
        }
        let url = serve_once(body.into_bytes());
        let source = PlaylistSource::new(&url);
        let text = DefaultFetcher.fetch(&source, &|| true).unwrap();
        assert!(text.len() > 10 * 1024 * 1024);
        assert!(text.starts_with("#EXTM3U"));
    }
    use super::*;
    use crate::parse_m3u;
    use std::collections::BTreeMap;

    struct FakeFetcher(BTreeMap<String, Result<String, LoadFailure>>);

    impl SourceFetcher for FakeFetcher {
        fn fetch(
            &self,
            source: &PlaylistSource,
            _current: &dyn Fn() -> bool,
        ) -> Result<String, LoadFailure> {
            self.0
                .get(source.location())
                .cloned()
                .unwrap_or(Err(LoadFailure::Unreadable))
        }
    }

    fn fetcher(entries: &[(&str, Result<&str, LoadFailure>)]) -> FakeFetcher {
        FakeFetcher(
            entries
                .iter()
                .map(|(key, value)| {
                    (
                        (*key).to_string(),
                        value.map_or_else(Err, |text| Ok(text.to_string())),
                    )
                })
                .collect(),
        )
    }

    const ONE: &str = "#EXTM3U\n#EXTINF:-1 group-title=\"News\",Alpha\nhttp://a/1\n";
    const TWO: &str =
        "#EXTM3U\n#EXTINF:-1 group-title=\"News\",Alpha\nhttp://b/2\n#EXTINF:-1,Beta\nhttp://b/3\n";

    #[test]
    fn display_label_strips_url_userinfo() {
        // Spec 0006: labels never carry URL credentials.
        let names = std::collections::BTreeMap::new();
        assert_eq!(
            source_display_label("http://user:secret@host.example:8080/list.m3u", &names),
            "host.example:8080"
        );
        assert_eq!(
            source_display_label("https://host.example/list.m3u", &names),
            "host.example"
        );
        assert_eq!(
            source_display_label("/home/user/lists/films.m3u", &names),
            "films.m3u"
        );
        let mut friendly = std::collections::BTreeMap::new();
        friendly.insert(
            source_name_key("http://user:secret@host.example/list.m3u"),
            "  My List  ".to_string(),
        );
        assert_eq!(
            source_display_label("http://user:secret@host.example/list.m3u", &friendly),
            "My List",
            "the friendly name wins and is trimmed"
        );
    }

    #[test]
    fn scope_id_is_the_python_sha256_form() {
        // sha256("abc") is a published test vector.
        assert_eq!(
            PlaylistSource::new("abc").scope_id(),
            "m3u:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(PlaylistSource::new("https://x/y.m3u").is_url());
        assert!(!PlaylistSource::new("/tmp/y.m3u").is_url());
    }

    #[test]
    fn merge_concatenates_tags_and_keeps_duplicates() {
        let sources = vec![PlaylistSource::new("one"), PlaylistSource::new("two")];
        let merged = load_merged(
            &sources,
            &fetcher(&[("one", Ok(ONE)), ("two", Ok(TWO))]),
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(merged.failed_sources, 0);
        let channels = merged.model.channels();
        assert_eq!(channels.len(), 3, "duplicate names across sources stay");
        assert_eq!(
            channel_scope(&channels[0]),
            Some(PlaylistSource::new("one").scope_id().as_str())
        );
        assert_eq!(scope_indexes(channels, ""), vec![0, 1, 2]);
        assert_eq!(
            scope_indexes(channels, &PlaylistSource::new("two").scope_id()),
            vec![1, 2]
        );
        assert_eq!(
            scope_indexes(channels, &PlaylistSource::new("one").scope_id()),
            vec![0]
        );
    }

    #[test]
    fn one_dead_source_does_not_sink_the_others() {
        let sources = vec![PlaylistSource::new("dead"), PlaylistSource::new("two")];
        let merged = load_merged(
            &sources,
            &fetcher(&[("dead", Err(LoadFailure::Missing)), ("two", Ok(TWO))]),
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(merged.failed_sources, 1);
        assert_eq!(merged.model.channels().len(), 2);
    }

    #[test]
    fn all_sources_dead_reports_the_first_failure() {
        let sources = vec![PlaylistSource::new("a"), PlaylistSource::new("b")];
        let outcome = load_merged(
            &sources,
            &fetcher(&[
                ("a", Err(LoadFailure::Missing)),
                ("b", Err(LoadFailure::Unreadable)),
            ]),
            || true,
        );
        assert_eq!(outcome.unwrap_err(), LoadFailure::Missing);
    }

    #[test]
    fn default_fetcher_reads_local_files_and_reports_missing() {
        let dir = std::env::temp_dir().join(format!("iptv-sources-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("one.m3u");
        std::fs::write(&file, ONE).expect("write");
        let sources = vec![
            PlaylistSource::new(file.to_string_lossy().into_owned()),
            PlaylistSource::new(dir.join("gone.m3u").to_string_lossy().into_owned()),
        ];
        let merged = load_merged(&sources, &DefaultFetcher, || true)
            .expect("ok")
            .expect("some");
        assert_eq!(merged.model.channels().len(), 1);
        assert_eq!(merged.failed_sources, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_entries_dedupe_within_one_source_only() {
        // B21 regression: repeat in A collapses; the same entry in B stays.
        let dup = "#EXTM3U\n#EXTINF:-1,Alpha\nhttp://a/1\n#EXTINF:-1,Alpha\nhttp://a/1\n";
        let sources = vec![PlaylistSource::new("one"), PlaylistSource::new("two")];
        let merged = load_merged(
            &sources,
            &fetcher(&[("one", Ok(dup)), ("two", Ok(ONE))]),
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(merged.model.channels().len(), 2);
    }

    #[test]
    fn failed_source_retains_its_previous_channels() {
        // B12 regression.
        let sources = vec![PlaylistSource::new("one"), PlaylistSource::new("two")];
        let first = load_merged(
            &sources,
            &fetcher(&[("one", Ok(ONE)), ("two", Ok(TWO))]),
            || true,
        )
        .expect("ok")
        .expect("some");
        let previous: Vec<Channel> = first.model.channels().to_vec();
        let second = load_merged_with(
            &sources,
            &fetcher(&[("one", Err(LoadFailure::Unreadable)), ("two", Ok(TWO))]),
            MergeOptions {
                previous: &previous,
                ..MergeOptions::default()
            },
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(second.model.channels().len(), 3);
        assert_eq!(second.stale_sources, ["one"]);
        assert_eq!(second.failed_sources, 0);
        // A failing source with nothing retained still counts.
        let third = load_merged_with(
            &sources,
            &fetcher(&[("one", Err(LoadFailure::Missing)), ("two", Ok(TWO))]),
            MergeOptions::default(),
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(third.failed_sources, 1);
        assert!(third.stale_sources.is_empty());
    }

    #[test]
    fn header_guides_are_reported_per_read_source() {
        // B09 regression (loader half).
        let with_guide =
            "#EXTM3U x-tvg-url=\"https://g.example/a.xml\"\n#EXTINF:-1,Alpha\nhttp://a/1\n";
        let sources = vec![PlaylistSource::new("one"), PlaylistSource::new("two")];
        let merged = load_merged(
            &sources,
            &fetcher(&[("one", Ok(with_guide)), ("two", Ok(TWO))]),
            || true,
        )
        .expect("ok")
        .expect("some");
        assert_eq!(
            merged.guide_urls,
            [
                (
                    "one".to_string(),
                    vec!["https://g.example/a.xml".to_string()]
                ),
                ("two".to_string(), Vec::new()),
            ]
        );
    }

    #[test]
    fn parsed_cache_prefill_and_reuse() {
        // B11 regression: a second load of an unchanged source is
        // served by the parsed cache, and the prefill exposes the
        // cached channels without any fetch.
        let dir = std::env::temp_dir().join(format!("iptv-b11-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let cache_dir = dir.join("cache");
        let file = dir.join("list.m3u");
        std::fs::write(&file, ONE).expect("write");
        let source = PlaylistSource::new(file.to_string_lossy().into_owned());
        let sources = vec![source.clone()];
        let options = MergeOptions {
            cache_dir: Some(&cache_dir),
            ..MergeOptions::default()
        };
        let first = load_merged_with(&sources, &DefaultFetcher, options, || true)
            .expect("ok")
            .expect("some");
        assert_eq!(first.model.channels().len(), 1);
        let prefill = prefill_model(&sources, &cache_dir).expect("prefill");
        assert_eq!(prefill.channels().len(), 1);
        assert_eq!(
            channel_scope(&prefill.channels()[0]),
            Some(source.scope_id().as_str())
        );
        // Change the file: the hash no longer matches, so the parsed
        // cache must not hide the new content.
        std::fs::write(&file, TWO).expect("rewrite");
        let second = load_merged_with(&sources, &DefaultFetcher, options, || true)
            .expect("ok")
            .expect("some");
        assert_eq!(second.model.channels().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn friendly_names_labels_and_keys() {
        // B13 regression (core half).
        let names = normalize_source_names(&BTreeMap::from([
            (
                source_name_key("http://h/list.m3u"),
                "  My List  ".to_string(),
            ),
            ("blank".to_string(), "   ".to_string()),
        ]));
        assert_eq!(names.len(), 1);
        assert_eq!(source_display_label("http://h/list.m3u", &names), "My List");
        assert_eq!(
            source_display_label("http://h/list.m3u", &BTreeMap::new()),
            "h"
        );
        assert_eq!(
            source_display_label("/tmp/dir/list.m3u", &BTreeMap::new()),
            "list.m3u"
        );
        // sha256("abc") test vector — same key shape as Python.
        assert_eq!(
            source_name_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn untagged_channels_stay_out_of_a_specific_scope() {
        let channels = parse_m3u(ONE);
        assert_eq!(
            scope_indexes(&channels, "m3u:anything"),
            Vec::<usize>::new()
        );
        assert_eq!(scope_indexes(&channels, ""), vec![0]);
    }
}
