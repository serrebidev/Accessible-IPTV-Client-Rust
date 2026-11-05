//! Provider account discovery (port of Python
//! `account_info.py`, audit N17).
//!
//! Xtream Codes and Stalker Portal accounts added through
//! the Playlist Manager are known outright. On top of those,
//! accounts are autodetected from ordinary playlist entries:
//! a plain `get.php?username=…&password=…` URL, and the
//! `/live/USER/PASS/1234.ts` stream URLs inside an
//! already-parsed playlist (which is how an M3U file
//! downloaded from an Xtream panel gives its own credentials
//! away). So a user who never used the Xtream dialog still
//! gets an expiry date.
//!
//! The fetch does blocking network I/O and must be called
//! from a worker thread, never from the UI thread. Passwords
//! are never written into a report (see `providers::
//! format_account_report`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::providers::{
    format_account_report, ProviderAccount, StalkerClient, StalkerConfig, XtreamClient,
    XtreamConfig, DEFAULT_UA,
};
use crate::Channel;

pub const KIND_XTREAM: &str = "xtream";
pub const KIND_STALKER: &str = "stalker";

pub const FROM_PLAYLIST_URL: &str = "playlist_url";
pub const FROM_STREAM_URL: &str = "stream_url";

/// Path prefixes Xtream panels put in front of the
/// credential pair.
const STREAM_PREFIXES: [&str; 7] = ["live", "movie", "series", "timeshift", "vod", "hls", "hlsr"];

/// One checkable provider account (Python's `Account`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub kind: String,
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub mac: String,
    pub name: String,
    pub provider_id: String,
    pub user_agent: String,
    /// True when the account was inferred from a URL rather
    /// than configured.
    pub detected: bool,
    pub detected_from: String,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            kind: String::new(),
            base_url: String::new(),
            username: String::new(),
            password: String::new(),
            mac: String::new(),
            name: String::new(),
            provider_id: String::new(),
            user_agent: DEFAULT_UA.to_string(),
            detected: false,
            detected_from: String::new(),
        }
    }
}

impl Account {
    /// The host without the credentials a pasted URL may
    /// carry.
    #[must_use]
    pub fn host(&self) -> String {
        redact_server_host(&self.base_url)
    }

    /// Same provider host + same username == same account
    /// (Python's `dedupe_key`). Deliberately ignores the rest
    /// of the base URL: panels routinely serve `get.php` and
    /// the streams themselves from different paths. Stalker
    /// accounts are MAC-authenticated and frequently have an
    /// empty username, so the MAC is the identity there.
    #[must_use]
    pub fn dedupe_key(&self) -> (String, String, String) {
        let identity = if self.username.is_empty() {
            self.mac.to_lowercase()
        } else {
            self.username.to_lowercase()
        };
        (self.kind.clone(), self.host().to_lowercase(), identity)
    }

    /// Short one-line label for the accounts list (Python's
    /// `account_label`).
    #[must_use]
    pub fn label(&self) -> String {
        let name = self.name.trim();
        let who = if !name.is_empty() {
            name.to_string()
        } else if !self.username.is_empty() {
            format!("{}@{}", self.username, self.host())
        } else if !self.mac.is_empty() {
            format!("{}@{}", self.mac, self.host())
        } else {
            self.host()
        };
        if self.detected {
            format!("{} – {who} (detected)", kind_label(&self.kind))
        } else {
            format!("{} – {who}", kind_label(&self.kind))
        }
    }

    /// The fetchable provider account for this record
    /// (Python's `build_client` inputs).
    #[must_use]
    pub fn to_provider_account(&self) -> ProviderAccount {
        let name = if self.name.is_empty() {
            None
        } else {
            Some(self.name.clone())
        };
        let provider_id = if self.provider_id.is_empty() {
            None
        } else {
            Some(self.provider_id.clone())
        };
        if self.kind == KIND_STALKER {
            ProviderAccount::Stalker(StalkerConfig {
                base_url: self.base_url.clone(),
                username: self.username.clone(),
                password: self.password.clone(),
                mac: self.mac.clone(),
                name,
                auto_epg: false,
                provider_id,
                ..StalkerConfig::default()
            })
        } else {
            ProviderAccount::Xtream(XtreamConfig {
                base_url: self.base_url.clone(),
                username: self.username.clone(),
                password: self.password.clone(),
                name,
                auto_epg: false,
                provider_id,
                user_agent: if self.user_agent.is_empty() {
                    DEFAULT_UA.to_string()
                } else {
                    self.user_agent.clone()
                },
                ..XtreamConfig::default()
            })
        }
    }
}

/// Python's `kind_label`.
#[must_use]
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        KIND_XTREAM => "Xtream Codes",
        KIND_STALKER => "Stalker Portal",
        _ => "Provider",
    }
}

fn detected_from_label(token: &str) -> &'static str {
    match token {
        FROM_PLAYLIST_URL => "playlist URL",
        FROM_STREAM_URL => "channel stream URL",
        _ => "unknown source",
    }
}

/// The host of a server URL, without the credentials in it
/// (Python's `_redact_server_host`).
#[must_use]
pub fn redact_server_host(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("").to_string();
    let host = netloc
        .rsplit_once('@')
        .map_or(netloc.as_str(), |(_, host)| host);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

/// Percent-decode (Python's `urllib.parse.unquote`): invalid
/// sequences stay literal.
fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `(scheme, netloc, path, query)` for an http(s) URL, the
/// pieces of Python's `urlparse` this module needs. None for
/// anything that is not an http(s) URL with a netloc.
fn split_http_url(url: &str) -> Option<(String, String, String, String)> {
    let text = url.trim();
    let lower = text.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let (scheme, rest) = text.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..authority_end];
    if netloc.is_empty() {
        return None;
    }
    let tail = &rest[authority_end..];
    let (path, query) = match tail.split_once('?') {
        Some((path, query)) => (
            path.to_string(),
            query.split('#').next().unwrap_or("").to_string(),
        ),
        None => (
            tail.split('#').next().unwrap_or("").to_string(),
            String::new(),
        ),
    };
    Some((scheme.to_string(), netloc.to_string(), path, query))
}

/// First values of a query string (Python's `parse_qs`:
/// `+` is a space, percent-decoded).
fn query_params(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = unquote(&key.replace('+', " "));
        params
            .entry(key)
            .or_insert_with(|| unquote(&value.replace('+', " ")));
    }
    params
}

/// The last path segment of a stream URL is a numeric stream
/// id, optionally with a container extension (Python's
/// `_STREAM_ID_RE` fullmatch): `\d+(\.[A-Za-z0-9]{1,5})?`.
fn is_stream_id(segment: &str) -> bool {
    let (digits, ext) = match segment.split_once('.') {
        Some((digits, ext)) => (digits, Some(ext)),
        None => (segment, None),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match ext {
        None => true,
        Some(ext) => {
            !ext.is_empty() && ext.len() <= 5 && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        }
    }
}

/// `(base_url, username, password)` if the URL looks like
/// Xtream (Python's `xtream_credentials_from_url`).
///
/// Recognizes both shapes a panel hands out: the query form
/// (`get.php?username=U&password=P`, also `player_api.php`
/// and friends, where the base URL is everything above the
/// `.php` file) and the path form (`/live/U/P/1234.ts`,
/// `/U/P/1234`, and the six-segment `timeshift` variant,
/// where the API lives at the server root). False positives
/// are worse than misses here: they would show the user an
/// account that does not exist.
#[must_use]
pub fn xtream_credentials_from_url(url: &str) -> Option<(String, String, String)> {
    let (scheme, netloc, path, query) = split_http_url(url)?;
    let root = format!("{scheme}://{netloc}");
    let segments: Vec<&str> = path.split('/').filter(|seg| !seg.is_empty()).collect();

    // Query form: credentials in the query string of a
    // panel endpoint.
    if !query.is_empty() {
        if let Some(last) = segments.last() {
            if last.to_lowercase().ends_with(".php") {
                let params = query_params(&query);
                let user = params
                    .get("username")
                    .map_or("", String::as_str)
                    .trim()
                    .to_string();
                let password = params
                    .get("password")
                    .map_or("", String::as_str)
                    .trim()
                    .to_string();
                if !user.is_empty() && !password.is_empty() {
                    let mut base = root.clone();
                    for segment in &segments[..segments.len() - 1] {
                        base.push('/');
                        base.push_str(segment);
                    }
                    return Some((base, user, password));
                }
            }
        }
    }

    // Path form: credentials as path segments, stream id
    // last.
    if segments.len() >= 3 && is_stream_id(segments[segments.len() - 1]) {
        let first = segments[0].to_lowercase();
        let offset = if segments.len() == 3 && !STREAM_PREFIXES.contains(&first.as_str()) {
            Some(0)
        } else if segments.len() == 4 && STREAM_PREFIXES.contains(&first.as_str()) {
            Some(1)
        } else if segments.len() == 6 && first == "timeshift" {
            // /timeshift/USER/PASS/duration/start/1234.ts
            Some(1)
        } else {
            None
        };
        if let Some(offset) = offset {
            let user = unquote(segments[offset]).trim().to_string();
            let password = unquote(segments[offset + 1]).trim().to_string();
            if !user.is_empty() && !password.is_empty() {
                return Some((root, user, password));
            }
        }
    }
    None
}

fn value_text(entry: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = entry.get(*key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

/// Build an Account from a configured provider entry
/// (Python's `_account_from_source`).
#[must_use]
pub fn account_from_source(entry: &Value) -> Option<Account> {
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let base_url = value_text(entry, &["base_url", "url"]).trim().to_string();
    if base_url.is_empty() || (kind != KIND_XTREAM && kind != KIND_STALKER) {
        return None;
    }
    let user_agent = value_text(entry, &["user_agent"]);
    Some(Account {
        kind,
        base_url,
        username: value_text(entry, &["username"]).trim().to_string(),
        password: value_text(entry, &["password"]).trim().to_string(),
        mac: value_text(entry, &["mac"]).trim().to_string(),
        name: value_text(entry, &["name"]).trim().to_string(),
        provider_id: {
            let id = value_text(entry, &["id", "provider_id"]);
            if id.is_empty() {
                entry
                    .get("id")
                    .and_then(Value::as_i64)
                    .map(|n| n.to_string())
                    .unwrap_or_default()
            } else {
                id
            }
        },
        user_agent: if user_agent.is_empty() {
            DEFAULT_UA.to_string()
        } else {
            user_agent
        },
        detected: false,
        detected_from: String::new(),
    })
}

/// Cheap cache key covering the host plus the first two
/// path segments (Python's `_scan_prefix`): every channel of
/// one account shares it, so a huge playlist costs a handful
/// of real URL parses. It can never merge two accounts: the
/// username always falls inside those first two segments.
fn scan_prefix(url: &str) -> String {
    url.splitn(6, '/').take(5).collect::<Vec<_>>().join("/")
}

/// Every checkable account, configured ones first, deduped
/// (Python's `discover_accounts`). The channel scan walks
/// the whole list, so call this from a worker thread.
#[must_use]
pub fn discover_accounts(sources: &[Value], channels: &[Channel]) -> Vec<Account> {
    let mut accounts: Vec<Account> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut add = |account: Option<Account>, accounts: &mut Vec<Account>| {
        if let Some(account) = account {
            if seen.insert(account.dedupe_key()) {
                accounts.push(account);
            }
        }
    };
    for source in sources {
        match source {
            Value::Object(_) => add(account_from_source(source), &mut accounts),
            Value::String(text) => {
                if let Some((base, user, password)) = xtream_credentials_from_url(text) {
                    add(
                        Some(Account {
                            kind: KIND_XTREAM.to_string(),
                            base_url: base,
                            username: user,
                            password,
                            detected: true,
                            detected_from: FROM_PLAYLIST_URL.to_string(),
                            ..Account::default()
                        }),
                        &mut accounts,
                    );
                }
            }
            _ => {}
        }
    }
    let mut scanned_prefixes: HashSet<String> = HashSet::new();
    for channel in channels {
        let url = &channel.url;
        if url.is_empty() {
            continue;
        }
        let prefix = scan_prefix(url);
        if !scanned_prefixes.insert(prefix) {
            continue;
        }
        if let Some((base, user, password)) = xtream_credentials_from_url(url) {
            add(
                Some(Account {
                    kind: KIND_XTREAM.to_string(),
                    base_url: base,
                    username: user,
                    password,
                    detected: true,
                    detected_from: FROM_STREAM_URL.to_string(),
                    ..Account::default()
                }),
                &mut accounts,
            );
        }
    }
    accounts
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Insert the `Detected from` header line into a formatted
/// report for a detected account (Python's `_header_lines`
/// puts it last in the header block, before the blank line
/// that opens the subscription section).
fn with_detected_line(report: String, account: &Account) -> String {
    if !account.detected {
        return report;
    }
    let line = format!(
        "Detected from: {}",
        detected_from_label(&account.detected_from)
    );
    match report.find("\n\n") {
        Some(at) => {
            let mut out = String::with_capacity(report.len() + line.len() + 1);
            out.push_str(&report[..at]);
            out.push('\n');
            out.push_str(&line);
            out.push_str(&report[at..]);
            out
        }
        None => format!("{report}\n{line}"),
    }
}

/// Query the account and return its formatted report
/// (Python's `fetch_account_report`). Blocking network I/O:
/// worker threads only.
///
/// # Errors
/// Returns the provider's error text when the account
/// cannot be checked.
pub fn fetch_account_report(account: &Account) -> Result<String, String> {
    let provider = account.to_provider_account();
    let payload = match &provider {
        ProviderAccount::Xtream(cfg) => XtreamClient::new(cfg.clone()).account_info(),
        ProviderAccount::Stalker(cfg) => StalkerClient::new(cfg.clone()).account_info(now_epoch()),
    };
    payload.map(|payload| {
        with_detected_line(
            format_account_report(&provider, &payload, now_epoch()),
            account,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(url: &str) -> Channel {
        Channel {
            name: "One".to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::default(),
        }
    }

    #[test]
    fn credentials_from_query_form() {
        let found = xtream_credentials_from_url(
            "http://panel.example:8080/get.php?username=u1&password=p1&type=m3u_plus",
        );
        assert_eq!(
            found,
            Some((
                "http://panel.example:8080".to_string(),
                "u1".to_string(),
                "p1".to_string()
            ))
        );
        // A panel in a subfolder keeps the folder in the base.
        let found = xtream_credentials_from_url(
            "https://h.example/sub/player_api.php?username=u&password=p",
        );
        assert_eq!(
            found,
            Some((
                "https://h.example/sub".to_string(),
                "u".to_string(),
                "p".to_string()
            ))
        );
        // Missing one credential is not an account.
        assert_eq!(
            xtream_credentials_from_url("http://h/get.php?username=u"),
            None
        );
    }

    #[test]
    fn credentials_from_path_forms() {
        assert_eq!(
            xtream_credentials_from_url("http://h:8080/live/u2/p2/1234.ts"),
            Some((
                "http://h:8080".to_string(),
                "u2".to_string(),
                "p2".to_string()
            ))
        );
        assert_eq!(
            xtream_credentials_from_url("http://h/u3/p3/99"),
            Some(("http://h".to_string(), "u3".to_string(), "p3".to_string()))
        );
        assert_eq!(
            xtream_credentials_from_url("http://h/timeshift/u4/p4/60/2024-01-01:10-00/55.ts"),
            Some(("http://h".to_string(), "u4".to_string(), "p4".to_string()))
        );
        // Ordinary CDN paths are not credentials.
        assert_eq!(xtream_credentials_from_url("http://cdn/a/b/c/1234"), None);
        assert_eq!(
            xtream_credentials_from_url("http://h/live/u/p/notanid"),
            None
        );
        assert_eq!(xtream_credentials_from_url("ftp://h/live/u/p/1.ts"), None);
        assert_eq!(
            xtream_credentials_from_url("http://h/movie/u/p/12345.mkv"),
            Some(("http://h".to_string(), "u".to_string(), "p".to_string()))
        );
    }

    #[test]
    fn discovery_orders_dedupes_and_marks_detected() {
        let sources = vec![
            serde_json::json!({"type": "xtream", "base_url": "http://panel:8080", "username": "cfg", "password": "pw", "name": "Main"}),
            Value::String("http://other:8080/get.php?username=pl&password=pw2".to_string()),
        ];
        let channels = vec![
            channel("http://panel:8080/live/cfg/pw/100.ts"),
            channel("http://panel:8080/live/cfg/pw/101.ts"),
            channel("http://streams:9090/live/str/pw3/7.ts"),
        ];
        let accounts = discover_accounts(&sources, &channels);
        // Configured, playlist-URL, then stream-detected; the
        // stream URL for the configured user dedupes away.
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].name, "Main");
        assert!(!accounts[0].detected);
        assert_eq!(accounts[1].detected_from, FROM_PLAYLIST_URL);
        assert_eq!(accounts[2].detected_from, FROM_STREAM_URL);
        assert_eq!(accounts[2].username, "str");
        assert_eq!(accounts[0].label(), "Xtream Codes – Main");
        assert_eq!(
            accounts[2].label(),
            "Xtream Codes – str@streams:9090 (detected)"
        );
    }

    #[test]
    fn stalker_source_and_mac_identity() {
        let sources = vec![serde_json::json!({
            "type": "stalker", "base_url": "http://portal/", "mac": "00:1A:79:00:00:01"
        })];
        let accounts = discover_accounts(&sources, &[]);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].kind, KIND_STALKER);
        assert_eq!(
            accounts[0].label(),
            "Stalker Portal – 00:1A:79:00:00:01@portal"
        );
        // A non-provider entry yields nothing.
        assert!(account_from_source(&serde_json::json!({"type": "file"})).is_none());
    }

    #[test]
    fn detected_line_lands_in_the_header() {
        let account = Account {
            detected: true,
            detected_from: FROM_STREAM_URL.to_string(),
            ..Account::default()
        };
        let report = "Account: a\nType: Xtream Codes\n\nSubscription".to_string();
        let out = with_detected_line(report, &account);
        assert_eq!(
            out,
            "Account: a\nType: Xtream Codes\nDetected from: channel stream URL\n\nSubscription"
        );
        let plain = Account::default();
        assert_eq!(with_detected_line("A\n\nB".to_string(), &plain), "A\n\nB");
    }
}
