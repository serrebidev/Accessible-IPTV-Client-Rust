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
    response
        .body_mut()
        .read_to_string()
        .map_err(|_| LoadFailure::Unreadable)
}

/// The merged outcome: the combined model plus how many sources failed.
#[derive(Debug)]
pub struct MergedLoad {
    pub model: BrowseModel,
    pub failed_sources: usize,
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
    let mut channels: Vec<Channel> = Vec::new();
    let mut malformed = 0usize;
    let mut failed = 0usize;
    let mut first_failure: Option<LoadFailure> = None;
    for source in sources {
        if !current() {
            return Ok(None);
        }
        match fetcher.fetch(source, &current) {
            Ok(text) => {
                let Some(model) = parse_cancellable(&text, &current) else {
                    return Ok(None);
                };
                let (mut parsed, bad) = model.into_parts();
                malformed += bad;
                let scope = source.scope_id();
                for channel in &mut parsed {
                    channel
                        .unknown_attributes
                        .insert(PLAYLIST_ID_ATTRIBUTE.to_string(), scope.clone());
                }
                channels.append(&mut parsed);
            }
            Err(failure) => {
                failed += 1;
                first_failure.get_or_insert(failure);
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
    }))
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
    fn untagged_channels_stay_out_of_a_specific_scope() {
        let channels = parse_m3u(ONE);
        assert_eq!(
            scope_indexes(&channels, "m3u:anything"),
            Vec::<usize>::new()
        );
        assert_eq!(scope_indexes(&channels, ""), vec![0]);
    }
}
