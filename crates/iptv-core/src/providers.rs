//! Provider accounts: Xtream Codes and Stalker/Ministra portal clients,
//! plus the spoken account report. Ports Python's `providers.py` and
//! `account_info.py` (pinned revision in the parity tool).
//!
//! Credentials live only in the user's own config; reports redact them
//! exactly like the reference (server URL credentials stripped, MAC
//! masked, secret payload fields never echoed).

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::Channel;

pub const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36";
const STALKER_UA: &str = "Mozilla/5.0 (QtEmbedded; U; Linux; C) AppleWebKit/535.21 (KHTML, like Gecko) MAG250 stbapp Safari/535.21";
const PORTAL_FILENAMES: &[&str] = &[
    "portal.php",
    "load.php",
    "get.php",
    "xmltv.php",
    "player_api.php",
    "panel_api.php",
    "api.php",
];

type ProviderResult<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Port of `_normalize_base_url`: default the scheme, drop userinfo, and
/// strip a trailing portal filename so endpoints can be appended cleanly.
#[must_use]
pub fn normalize_base_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    let with_scheme;
    let url = if url.contains("://") {
        url
    } else {
        with_scheme = format!("http://{url}");
        &with_scheme
    };
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    // B16: Python parses and rebuilds the URL, dropping query and
    // fragment before any endpoint is appended; a pasted
    // `get.php?username=...` or `#section` must not leak into paths.
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let mut path = path.to_string();
    if !path.is_empty() && path != "/" {
        let filename = path.rsplit('/').next().unwrap_or("").to_lowercase();
        if PORTAL_FILENAMES.contains(&filename.as_str()) {
            path = path
                .rsplit_once('/')
                .map_or(String::new(), |(head, _)| head.to_string());
        }
    }
    let path = path.trim_end_matches('/');
    format!("{scheme}://{host}{path}")
}

/// `application/x-www-form-urlencoded` encoding (Python `urlencode`).
fn form_encode(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode a URL path segment (Python `quote(..., safe="")`).
fn quote_segment(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

fn http_get_text(url: &str, user_agent: &str) -> ProviderResult<Vec<u8>> {
    let mut response = ureq::get(url)
        .header("User-Agent", user_agent)
        .call()
        .map_err(|e| e.to_string())?;
    crate::sources::read_body_unbounded(response.body_mut()).map_err(|e| e.to_string())
}

fn decode_text(bytes: &[u8]) -> String {
    let without_bom = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(without_bom).into_owned()
}

// ---------------------------------------------------------------------------
// Xtream Codes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct XtreamConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub stream_type: String,
    pub output: String,
    pub name: Option<String>,
    pub auto_epg: bool,
    pub provider_id: Option<String>,
    pub user_agent: String,
}

impl Default for XtreamConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            username: String::new(),
            password: String::new(),
            stream_type: "m3u_plus".to_string(),
            output: "ts".to_string(),
            name: None,
            auto_epg: true,
            provider_id: None,
            user_agent: DEFAULT_UA.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct XtreamClient {
    cfg: XtreamConfig,
    base: String,
}

impl XtreamClient {
    #[must_use]
    pub fn new(cfg: XtreamConfig) -> Self {
        let base = normalize_base_url(&cfg.base_url);
        Self { cfg, base }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        let label = self.cfg.name.clone().unwrap_or_else(|| host_of(&self.base));
        format!("Xtream Codes ({label})")
    }

    #[must_use]
    pub fn playlist_url(&self) -> String {
        let params = query(&[
            ("username", &self.cfg.username),
            ("password", &self.cfg.password),
            (
                "type",
                if self.cfg.stream_type.is_empty() {
                    "m3u_plus"
                } else {
                    &self.cfg.stream_type
                },
            ),
            (
                "output",
                if self.cfg.output.is_empty() {
                    "ts"
                } else {
                    &self.cfg.output
                },
            ),
        ]);
        format!("{}/get.php?{params}", self.base)
    }

    #[must_use]
    pub fn epg_urls(&self) -> Vec<String> {
        if !self.cfg.auto_epg {
            return Vec::new();
        }
        let params = query(&[
            ("username", &self.cfg.username),
            ("password", &self.cfg.password),
        ]);
        vec![format!("{}/xmltv.php?{params}", self.base)]
    }

    /// Download the get.php playlist; a 200 response that is not a
    /// playlist means the credentials were rejected.
    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn fetch_playlist(&self) -> ProviderResult<String> {
        let bytes = http_get_text(&self.playlist_url(), &self.cfg.user_agent)
            .map_err(|e| format!("Could not reach the Xtream server: {e}"))?;
        let text = decode_text(&bytes);
        let head: String = text.chars().take(4096).collect();
        if !head.contains("#EXTM3U") {
            return Err(
                "Authentication failed – the server rejected these credentials".to_string(),
            );
        }
        Ok(text)
    }

    fn player_api_url(&self, action: Option<&str>, extra: &[(&str, String)]) -> String {
        let mut params: Vec<(&str, String)> = vec![
            ("username", self.cfg.username.clone()),
            ("password", self.cfg.password.clone()),
        ];
        if let Some(action) = action {
            params.push(("action", action.to_string()));
        }
        for (k, v) in extra {
            params.push((k, v.clone()));
        }
        let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        format!("{}/player_api.php?{}", self.base, query(&pairs))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn player_api(
        &self,
        action: Option<&str>,
        extra: &[(&str, String)],
    ) -> ProviderResult<Value> {
        let bytes = http_get_text(&self.player_api_url(action, extra), &self.cfg.user_agent)
            .map_err(|e| format!("Could not reach the Xtream server: {e}"))?;
        serde_json::from_slice(&strip_bom(&bytes))
            .map_err(|_| "Invalid response from Xtream player API".to_string())
    }

    /// The raw `player_api` authentication payload (account state).
    ///
    /// # Errors
    ///
    /// Returns the provider error text when the portal is unreachable, the handshake fails, or the answer is invalid.
    pub fn account_info(&self) -> ProviderResult<Value> {
        let data = self.player_api(None, &[])?;
        if !data.is_object() {
            return Err("Unexpected account response from the Xtream server".to_string());
        }
        Ok(data)
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn vod_categories(&self) -> ProviderResult<Value> {
        Ok(list_or_empty(
            self.player_api(Some("get_vod_categories"), &[])?,
        ))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn vod_streams(&self, category_id: Option<&str>) -> ProviderResult<Value> {
        let extra: Vec<(&str, String)> = category_id
            .map(|id| ("category_id", id.to_string()))
            .into_iter()
            .collect();
        Ok(list_or_empty(
            self.player_api(Some("get_vod_streams"), &extra)?,
        ))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn series_categories(&self) -> ProviderResult<Value> {
        Ok(list_or_empty(
            self.player_api(Some("get_series_categories"), &[])?,
        ))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn series(&self, category_id: Option<&str>) -> ProviderResult<Value> {
        let extra: Vec<(&str, String)> = category_id
            .map(|id| ("category_id", id.to_string()))
            .into_iter()
            .collect();
        Ok(list_or_empty(self.player_api(Some("get_series"), &extra)?))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the server is unreachable, rejects the credentials, or answers invalid data.
    pub fn series_info(&self, series_id: &str) -> ProviderResult<Value> {
        let data = self.player_api(
            Some("get_series_info"),
            &[("series_id", series_id.to_string())],
        )?;
        Ok(if data.is_object() {
            data
        } else {
            Value::Object(serde_json::Map::new())
        })
    }

    #[must_use]
    pub fn vod_stream_url(&self, stream_id: &str, ext: &str) -> String {
        let ext = if ext.is_empty() {
            "mp4"
        } else {
            ext.trim_start_matches('.')
        };
        format!(
            "{}/movie/{}/{}/{}.{}",
            self.base,
            quote_segment(&self.cfg.username),
            quote_segment(&self.cfg.password),
            stream_id,
            ext
        )
    }

    #[must_use]
    pub fn series_stream_url(&self, episode_id: &str, ext: &str) -> String {
        let ext = if ext.is_empty() {
            "mp4"
        } else {
            ext.trim_start_matches('.')
        };
        format!(
            "{}/series/{}/{}/{}.{}",
            self.base,
            quote_segment(&self.cfg.username),
            quote_segment(&self.cfg.password),
            episode_id,
            ext
        )
    }

    #[must_use]
    pub fn live_stream_url(&self, stream_id: &str) -> String {
        let output = if self.cfg.output.is_empty() {
            "ts"
        } else {
            &self.cfg.output
        };
        format!(
            "{}/live/{}/{}/{}.{}",
            self.base,
            quote_segment(&self.cfg.username),
            quote_segment(&self.cfg.password),
            stream_id,
            output
        )
    }
}

fn list_or_empty(value: Value) -> Value {
    if value.is_array() {
        value
    } else {
        Value::Array(Vec::new())
    }
}

fn strip_bom(bytes: &[u8]) -> Vec<u8> {
    bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(bytes)
        .to_vec()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or(rest);
    // Display labels never carry URL credentials (spec 0006).
    authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_string()
}

// ---------------------------------------------------------------------------
// Stalker / Ministra portal
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct StalkerConfig {
    pub base_url: String,
    pub mac: String,
    pub username: String,
    pub password: String,
    pub name: Option<String>,
    pub auto_epg: bool,
    pub provider_id: Option<String>,
    pub user_agent: String,
    pub profile_user_agent: String,
    pub timezone: String,
}

impl Default for StalkerConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            mac: String::new(),
            username: String::new(),
            password: String::new(),
            name: None,
            auto_epg: true,
            provider_id: None,
            user_agent: STALKER_UA.to_string(),
            profile_user_agent: "Model: MAG420; Link: WiFi".to_string(),
            timezone: "UTC".to_string(),
        }
    }
}

pub struct StalkerClient {
    cfg: StalkerConfig,
    base: String,
    endpoint: String,
    token: RefCell<Option<(String, i64)>>,
}

impl StalkerClient {
    #[must_use]
    pub fn new(cfg: StalkerConfig) -> Self {
        let base = normalize_base_url(&cfg.base_url);
        let raw = cfg.base_url.trim().to_lowercase();
        let endpoint = if raw.ends_with("portal.php") || raw.ends_with("load.php") {
            cfg.base_url.trim().to_string()
        } else {
            format!("{base}/portal.php")
        };
        Self {
            cfg,
            base,
            endpoint,
            token: RefCell::new(None),
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        let label = self.cfg.name.clone().unwrap_or_else(|| host_of(&self.base));
        format!("Stalker Portal ({label})")
    }

    fn portal_call(&self, params: &[(&str, &str)], include_token: bool) -> ProviderResult<Value> {
        let pairs: Vec<(&str, &str)> = params.to_vec();
        let url = format!("{}?{}", self.endpoint, query(&pairs));
        let mut request = ureq::get(&url)
            .header("User-Agent", &self.cfg.user_agent)
            .header("Accept", "application/json")
            .header("Connection", "keep-alive")
            .header("X-User-Agent", &self.cfg.profile_user_agent)
            .header("Referer", &format!("{}/c/", self.base))
            .header(
                "Cookie",
                &format!(
                    "stb_lang=en; timezone={}; mac={}",
                    self.cfg.timezone, self.cfg.mac
                ),
            );
        if include_token {
            if let Some((token, _)) = self.token.borrow().as_ref() {
                request = request.header("Authorization", &format!("Bearer {token}"));
            }
        }
        let mut response = request.call().map_err(|e| {
            if matches!(e, ureq::Error::StatusCode(401 | 403)) {
                // The portal dropped the token; forgetting it costs one
                // handshake, keeping it fails every later call for 25 min.
                *self.token.borrow_mut() = None;
            }
            format!("Could not reach the portal: {e}")
        })?;
        let bytes = crate::sources::read_body_unbounded(response.body_mut())
            .map_err(|e| format!("Could not reach the portal: {e}"))?;
        let data: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Invalid response from portal".to_string())?;
        if !data.is_object() {
            return Err("Invalid response from portal".to_string());
        }
        Ok(data)
    }

    fn ensure_token(&self, now: i64) -> ProviderResult<()> {
        if let Some((_, issued)) = self.token.borrow().as_ref() {
            if now - *issued < 60 * 25 {
                return Ok(());
            }
        }
        let data = self.portal_call(
            &[
                ("type", "stb"),
                ("action", "handshake"),
                ("token", ""),
                ("prehash", "0"),
                ("JsHttpRequest", "1-xml"),
            ],
            false,
        )?;
        let token = portal_token(&data)
            .ok_or_else(|| "Portal handshake failed: no token returned".to_string())?;
        *self.token.borrow_mut() = Some((token.to_string(), now));
        if !self.cfg.username.is_empty() || !self.cfg.password.is_empty() {
            let auth = self.portal_call(
                &[
                    ("type", "stb"),
                    ("action", "login"),
                    ("login", &self.cfg.username),
                    ("password", &self.cfg.password),
                    ("JsHttpRequest", "1-xml"),
                ],
                true,
            )?;
            if let Some(new_token) = portal_token(&auth) {
                *self.token.borrow_mut() = Some((new_token.to_string(), now));
            }
        }
        Ok(())
    }

    /// All portal channels as playlist channels with unresolved URLs,
    /// plus the best-effort EPG source list.
    ///
    /// # Errors
    ///
    /// Returns the provider error text when the portal is unreachable, the handshake fails, or the answer is invalid.
    pub fn fetch_channels(&self, now: i64) -> ProviderResult<(Vec<Channel>, Vec<String>)> {
        self.ensure_token(now)?;
        let payload = self.portal_call(
            &[
                ("type", "itv"),
                ("action", "get_all_channels"),
                ("include", "genres"),
                ("force_ch_link_check", "1"),
                ("JsHttpRequest", "1-xml"),
            ],
            true,
        )?;
        let rows = payload
            .get("js")
            .and_then(|js| js.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut channels = Vec::new();
        for row in &rows {
            let name = string_field(row, "name")
                .or_else(|| string_field(row, "tv_genre_title"))
                .unwrap_or_default();
            let cmd = string_field(row, "cmd").unwrap_or_default();
            if name.is_empty() || cmd.is_empty() {
                continue;
            }
            let group = string_field(row, "tv_genre_title")
                .or_else(|| string_field(row, "tv_genre_id"))
                .unwrap_or_else(|| "Stalker".to_string());
            let mut attrs = BTreeMap::new();
            attrs.insert("provider-type".to_string(), "stalker".to_string());
            if let Some(id) = &self.cfg.provider_id {
                attrs.insert("provider-id".to_string(), id.clone());
                // B01: scope tag, as with the Xtream mapper.
                attrs.insert(
                    crate::sources::PLAYLIST_ID_ATTRIBUTE.to_string(),
                    id.clone(),
                );
            }
            attrs.insert("cmd".to_string(), cmd);
            for key in [
                "use_http_tmp_link",
                "id",
                "number",
                "allow_timeshift",
                "archive",
            ] {
                if let Some(v) = row.get(key) {
                    attrs.insert(key.to_string(), value_text(v));
                }
            }
            if let Some(days) = row.get("tv_archive_duration") {
                if !value_text(days).is_empty() {
                    attrs.insert("catchup-days".to_string(), value_text(days));
                }
            }
            if truthy(row.get("allow_timeshift")) || truthy(row.get("archive")) {
                attrs.insert("catchup".to_string(), "stalker".to_string());
            }
            channels.push(Channel {
                name: name.clone(),
                group,
                url: String::new(),
                tvg_id: string_field(row, "epg_id").unwrap_or_default(),
                tvg_name: string_field(row, "display_name").unwrap_or(name),
                tvg_logo: String::new(),
                unknown_attributes: attrs,
            });
        }
        let epg_urls = if self.cfg.auto_epg {
            vec![format!("{}/xmltv.php", self.base)]
        } else {
            Vec::new()
        };
        Ok((channels, epg_urls))
    }

    ///
    /// # Errors
    ///
    /// Returns the provider error text when the portal is unreachable, the handshake fails, or the answer is invalid.
    pub fn account_info(&self, now: i64) -> ProviderResult<Value> {
        self.ensure_token(now)?;
        let payload = self.portal_call(
            &[
                ("type", "account_info"),
                ("action", "get_main_info"),
                ("JsHttpRequest", "1-xml"),
            ],
            true,
        )?;
        let data = payload.get("js").cloned().unwrap_or(Value::Null);
        if !data.is_object() {
            return Err("Portal did not return account information".to_string());
        }
        Ok(data)
    }

    /// Resolve a channel's temporary stream link (`create_link`).
    ///
    /// # Errors
    ///
    /// Returns the provider error text when the portal is unreachable, the handshake fails, or the answer is invalid.
    pub fn resolve_stream(
        &self,
        provider_data: &BTreeMap<String, String>,
        now: i64,
    ) -> ProviderResult<String> {
        self.ensure_token(now)?;
        let cmd = provider_data
            .get("cmd")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "Channel missing command reference".to_string())?;
        let payload = self.portal_call(
            &[
                ("type", "itv"),
                ("action", "create_link"),
                ("cmd", cmd),
                ("JsHttpRequest", "1-xml"),
            ],
            true,
        )?;
        portal_link(&payload).ok_or_else(|| "Portal did not return stream URL".to_string())
    }

    /// Resolve a catch-up (archive) link for a programme start + duration.
    ///
    /// # Errors
    ///
    /// Returns the provider error text when the portal is unreachable, the handshake fails, or the answer is invalid.
    pub fn resolve_catchup(
        &self,
        provider_data: &BTreeMap<String, String>,
        start: &str,
        duration: i64,
        now: i64,
    ) -> ProviderResult<String> {
        self.ensure_token(now)?;
        let cmd = provider_data
            .get("cmd")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "Channel missing command reference".to_string())?;
        let duration_text = duration.to_string();
        let payload = self.portal_call(
            &[
                ("type", "itv"),
                ("action", "create_link"),
                ("cmd", cmd),
                ("JsHttpRequest", "1-xml"),
                ("download", "0"),
                ("save", "0"),
                ("series", "1"),
                ("forced_storage", "0"),
                ("start", start),
                ("duration", &duration_text),
            ],
            true,
        )?;
        portal_link(&payload).ok_or_else(|| "Portal did not provide catch-up link".to_string())
    }
}

/// The session token of a handshake/login response (B15): Python
/// accepts it at the top level or nested under `js`; portals in the
/// wild use both shapes.
#[must_use]
pub fn portal_token(payload: &Value) -> Option<&str> {
    payload
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .or_else(|| {
            payload
                .get("js")
                .and_then(|js| js.get("token"))
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
        })
}

/// Normalize a MAC address the way Python's Stalker form does (B06):
/// dashes become colons, dots are dropped, letters uppercase, and a
/// bare 12-hex-digit string gains its colons.
#[must_use]
pub fn sanitize_mac(value: &str) -> String {
    let mut cleaned: String = value
        .trim()
        .replace('-', ":")
        .replace('.', "")
        .to_uppercase();
    if !cleaned.contains(':') && cleaned.len() == 12 {
        cleaned = cleaned
            .as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(":");
    }
    cleaned
}

/// Whether a (sanitized) MAC has the six octets the portal expects.
#[must_use]
pub fn mac_is_valid(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A fresh MAG-style MAC: Python's default prefix `00:1A:79` plus
/// three random octets (also the Randomise MAC action's output).
#[must_use]
#[allow(clippy::cast_possible_truncation)] // only the low bits seed the PRNG
pub fn random_mag_mac() -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x5eed, |d| d.as_nanos() as u64);
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        // xorshift64*: ample for a default MAC suggestion.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 56) as u8
    };
    format!("00:1A:79:{:02X}:{:02X}:{:02X}", next(), next(), next())
}

/// Validate the account-form fields the way Python's dialogs do
/// (B06): values are trimmed by the caller-side form; returns the
/// user-facing problem, or None when the account may be saved.
#[must_use]
pub fn account_form_error(
    kind: &str,
    base_url: &str,
    username: &str,
    password: &str,
    mac: &str,
) -> Option<String> {
    if kind == "stalker" {
        if base_url.trim().is_empty() {
            return Some("Portal URL is required.".to_string());
        }
        if !mac_is_valid(&sanitize_mac(mac)) {
            return Some(
                "MAC address must contain six octets (e.g., 00:1A:79:12:34:56).".to_string(),
            );
        }
        None
    } else {
        if base_url.trim().is_empty() || username.trim().is_empty() || password.trim().is_empty() {
            return Some("Username, password, and URL are required.".to_string());
        }
        None
    }
}

/// Merge account-form values into an existing config entry (B04):
/// fields the form does not know about — Stalker credentials on an
/// Xtream-shaped edit, output/type keys, auto-EPG, unknown retained
/// settings — survive a name-only edit. `updates` wins per key.
#[must_use]
pub fn merge_account_fields(existing: &Value, updates: serde_json::Map<String, Value>) -> Value {
    let mut merged = match existing {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    for (key, value) in updates {
        merged.insert(key, value);
    }
    Value::Object(merged)
}

fn portal_link(payload: &Value) -> Option<String> {
    let raw = payload
        .get("js")
        .and_then(|js| js.get("cmd"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("cmd").and_then(Value::as_str))?;
    let mut link = raw.to_string();
    for prefix in ["ffmpeg ", "auto "] {
        if let Some(rest) = link.strip_prefix(prefix) {
            link = rest.to_string();
        }
    }
    if link.is_empty() {
        None
    } else {
        Some(link)
    }
}

fn string_field(row: &Value, key: &str) -> Option<String> {
    row.get(key).map(value_text).filter(|s| !s.is_empty())
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().is_some_and(|i| i != 0),
        Some(Value::String(s)) => matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Config entries and the background provider worker
// ---------------------------------------------------------------------------

/// One provider account from a `playlists` config entry.
#[derive(Clone, Debug)]
pub enum ProviderAccount {
    Xtream(XtreamConfig),
    Stalker(StalkerConfig),
}

impl ProviderAccount {
    /// The safe source label for catalogue rows (spec 0006): the
    /// configured friendly account name when one is set, else the
    /// base URL's host with any userinfo removed.
    #[must_use]
    pub fn source_label(&self) -> String {
        let (name, base_url) = match self {
            ProviderAccount::Xtream(cfg) => (&cfg.name, &cfg.base_url),
            ProviderAccount::Stalker(cfg) => (&cfg.name, &cfg.base_url),
        };
        name.as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(|| host_of(base_url), str::to_owned)
    }

    #[must_use]
    pub fn provider_id(&self) -> Option<String> {
        match self {
            Self::Xtream(cfg) => cfg.provider_id.clone(),
            Self::Stalker(cfg) => cfg.provider_id.clone(),
        }
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Xtream(cfg) => XtreamClient::new(cfg.clone()).describe(),
            Self::Stalker(cfg) => {
                let label = cfg.name.clone().unwrap_or_else(|| host_of(&cfg.base_url));
                format!("Stalker Portal ({label})")
            }
        }
    }

    /// The playlist-scope picker label (Python `_scope_choice_label`):
    /// "Xtream Codes – {name}" / "Stalker Portal – {name}", the name
    /// falling back name → username → base URL → "Provider". Distinct
    /// from [`Self::describe`], which keys provider state and keeps
    /// its parenthesised form.
    #[must_use]
    pub fn scope_label(&self) -> String {
        let (kind, name, username, base_url) = match self {
            Self::Xtream(cfg) => (
                "Xtream Codes",
                cfg.name.as_deref(),
                cfg.username.as_str(),
                cfg.base_url.as_str(),
            ),
            Self::Stalker(cfg) => (
                "Stalker Portal",
                cfg.name.as_deref(),
                cfg.username.as_str(),
                cfg.base_url.as_str(),
            ),
        };
        let who = name
            .filter(|value| !value.is_empty())
            .or_else(|| (!username.is_empty()).then_some(username))
            .or_else(|| (!base_url.is_empty()).then_some(base_url))
            .unwrap_or("Provider");
        format!("{kind} – {who}")
    }

    /// Parse one config dict entry; None for non-provider entries.
    #[must_use]
    pub fn from_config(entry: &Value) -> Option<Self> {
        let obj = entry.as_object()?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let text = |keys: &[&str]| -> String {
            keys.iter()
                .find_map(|k| obj.get(*k).and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        };
        let provider_id = text(&["id", "provider_id"]);
        let provider_id = if provider_id.is_empty() {
            None
        } else {
            Some(provider_id)
        };
        let auto_epg = obj.get("auto_epg").and_then(Value::as_bool).unwrap_or(true);
        match kind.as_str() {
            "xtream" => Some(Self::Xtream(XtreamConfig {
                base_url: text(&["base_url", "url"]),
                username: text(&["username"]),
                password: text(&["password"]),
                stream_type: {
                    let v = text(&["stream_type"]);
                    if v.is_empty() {
                        "m3u_plus".to_string()
                    } else {
                        v
                    }
                },
                output: {
                    let v = text(&["output"]);
                    if v.is_empty() {
                        "ts".to_string()
                    } else {
                        v
                    }
                },
                name: {
                    let v = text(&["name"]);
                    if v.is_empty() {
                        None
                    } else {
                        Some(v)
                    }
                },
                auto_epg,
                provider_id,
                user_agent: DEFAULT_UA.to_string(),
            })),
            "stalker" => Some(Self::Stalker(StalkerConfig {
                base_url: text(&["base_url", "url"]),
                mac: text(&["mac"]),
                username: text(&["username"]),
                password: text(&["password"]),
                name: {
                    let v = text(&["name"]);
                    if v.is_empty() {
                        None
                    } else {
                        Some(v)
                    }
                },
                auto_epg,
                provider_id,
                ..StalkerConfig::default()
            })),
            _ => None,
        }
    }
}

enum ProviderJob {
    SetCacheDir(std::path::PathBuf),
    Load {
        accounts: Vec<ProviderAccount>,
        /// Bypass the fresh provider cache (B13 forced refresh).
        force: bool,
    },
    Resolve {
        request: u64,
        provider_id: String,
        data: BTreeMap<String, String>,
    },
    ResolveCatchup {
        request: u64,
        provider_id: String,
        data: BTreeMap<String, String>,
        start_text: String,
        duration_minutes: i64,
    },
    Account {
        provider_id: String,
    },
}

/// What the provider worker reports back to the UI thread.
pub enum ProviderEvent {
    Loaded {
        channels: Vec<Channel>,
        epg_urls: Vec<String>,
        failures: Vec<String>,
    },
    /// A resolution finished; `request` correlates the result
    /// with the caller's dispatch.
    Resolved {
        /// The opaque request identifier from dispatch.
        request: u64,
        /// The resolved URL or the failure message.
        result: ProviderResult<String>,
    },
    Account {
        /// The formatted account report for display.
        report: ProviderResult<String>,
        /// The account's declared connection limit, when the
        /// payload carried one (issue #134's capacity cache).
        max_connections: Option<i64>,
        /// The account the fetch was for.
        provider_id: String,
    },
}

enum Client {
    Xtream(XtreamClient),
    Stalker(StalkerClient),
}

/// Background worker owning the provider clients (Stalker tokens live
/// here), mirroring the other workers' shape.
pub struct ProviderWorker {
    jobs: std::sync::mpsc::Sender<ProviderJob>,
    events: std::sync::Mutex<std::sync::mpsc::Receiver<ProviderEvent>>,
}

impl Default for ProviderWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Run one provider load job against the worker's client table.
/// Cached provider data stays fresh for six hours; a stale copy
/// still serves when a fetch fails.
const CACHE_TTL: std::time::Duration = std::time::Duration::from_hours(6);

fn handle_load(
    accounts: Vec<ProviderAccount>,
    clients: &mut BTreeMap<String, (ProviderAccount, Client)>,
    cache_dir: Option<&std::path::Path>,
    force: bool,
) -> ProviderEvent {
    let mut channels = Vec::new();
    let mut epg_urls = Vec::new();
    let mut failures = Vec::new();
    for account in accounts {
        let now = now_epoch();
        let key = account.provider_id().unwrap_or_else(|| account.describe());
        match &account {
            ProviderAccount::Xtream(cfg) => {
                let client = XtreamClient::new(cfg.clone());
                let (found, urls, fails) = load_xtream_account(&client, cfg, cache_dir, force);
                channels.extend(found);
                epg_urls.extend(urls);
                failures.extend(fails);
                clients.insert(key, (account, Client::Xtream(client)));
            }
            ProviderAccount::Stalker(cfg) => {
                let client = StalkerClient::new(cfg.clone());
                let (found, urls, fails) =
                    load_stalker_account(&client, cfg, cache_dir, now, force);
                channels.extend(found);
                epg_urls.extend(urls);
                failures.extend(fails);
                clients.insert(key, (account, Client::Stalker(client)));
            }
        }
    }
    ProviderEvent::Loaded {
        channels,
        epg_urls,
        failures,
    }
}

/// Tag parsed channels with their provider identity.
fn tag_xtream_channels(parsed: &mut [Channel], cfg: &XtreamConfig) {
    for channel in parsed {
        channel
            .unknown_attributes
            .insert("provider-type".to_string(), "xtream".to_string());
        if let Some(id) = &cfg.provider_id {
            channel
                .unknown_attributes
                .insert("provider-id".to_string(), id.clone());
            // B01: Python tags every source's channels with the
            // source's scope id; the Playlist view scope filter keys
            // on it, so provider channels need it too.
            channel.unknown_attributes.insert(
                crate::sources::PLAYLIST_ID_ATTRIBUTE.to_string(),
                id.clone(),
            );
        }
        // B10: Python extracts the stream id from the provider URL;
        // both favorite-key algorithms include it.
        if let Some(stream_id) = crate::extract_stream_id(&channel.url) {
            channel
                .unknown_attributes
                .entry("stream-id".to_string())
                .or_insert(stream_id);
        }
    }
}

/// One Xtream account's playlist: fresh cache first, then the
/// server (writing the cache), then the stale cache on failure.
fn load_xtream_account(
    client: &XtreamClient,
    cfg: &XtreamConfig,
    cache_dir: Option<&std::path::Path>,
    force: bool,
) -> (Vec<Channel>, Vec<String>, Vec<String>) {
    let cache_file = cache_dir.map(|dir| {
        crate::cache::file_for(
            dir,
            &crate::cache::cache_key(&["xtream", &cfg.base_url, &cfg.username]),
        )
    });
    let fresh = if force {
        None
    } else {
        cache_file.as_ref().and_then(|path| {
            crate::cache::read(path, Some(CACHE_TTL), std::time::SystemTime::now())
        })
    };
    let fetched = match fresh {
        Some(text) => Ok(text),
        None => client.fetch_playlist().inspect(|text| {
            if let Some(path) = &cache_file {
                let _ = crate::cache::write(path, text);
            }
        }),
    };
    match fetched {
        Ok(text) => {
            let mut parsed = crate::parse_m3u(&text);
            tag_xtream_channels(&mut parsed, cfg);
            (parsed, client.epg_urls(), Vec::new())
        }
        Err(error) => {
            let stale = cache_file
                .as_ref()
                .and_then(|path| crate::cache::read(path, None, std::time::SystemTime::now()));
            match stale {
                Some(text) => {
                    let mut parsed = crate::parse_m3u(&text);
                    tag_xtream_channels(&mut parsed, cfg);
                    (
                        parsed,
                        client.epg_urls(),
                        vec![format!(
                            "{}: {error}; using the cached playlist",
                            client.describe()
                        )],
                    )
                }
                None => (
                    Vec::new(),
                    Vec::new(),
                    vec![format!("{}: {error}", client.describe())],
                ),
            }
        }
    }
}

/// One Stalker account's channel list, with the same cache
/// policy as the Xtream path (channels serialized as JSON).
fn load_stalker_account(
    client: &StalkerClient,
    cfg: &StalkerConfig,
    cache_dir: Option<&std::path::Path>,
    now: i64,
    force: bool,
) -> (Vec<Channel>, Vec<String>, Vec<String>) {
    let cache_file = cache_dir.map(|dir| {
        crate::cache::file_for(
            dir,
            &crate::cache::cache_key(&["stalker", &cfg.base_url, &cfg.mac]),
        )
    });
    let read_cache = |fresh_only: bool| -> Option<(Vec<Channel>, Vec<String>)> {
        let path = cache_file.as_ref()?;
        let text = crate::cache::read(
            path,
            fresh_only.then_some(CACHE_TTL),
            std::time::SystemTime::now(),
        )?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let found: Vec<Channel> = serde_json::from_value(value.get("channels")?.clone()).ok()?;
        let urls: Vec<String> = serde_json::from_value(value.get("epg_urls")?.clone()).ok()?;
        Some((found, urls))
    };
    if !force {
        if let Some((found, urls)) = read_cache(true) {
            return (found, urls, Vec::new());
        }
    }
    match client.fetch_channels(now) {
        Ok((found, urls)) => {
            if let Some(path) = &cache_file {
                let payload = serde_json::json!({
                    "channels": found,
                    "epg_urls": urls,
                });
                let _ = crate::cache::write(path, &payload.to_string());
            }
            (found, urls, Vec::new())
        }
        Err(error) => match read_cache(false) {
            Some((found, urls)) => (
                found,
                urls,
                vec![format!(
                    "{}: {error}; using the cached channel list",
                    client.describe()
                )],
            ),
            None => (
                Vec::new(),
                Vec::new(),
                vec![format!("{}: {error}", client.describe())],
            ),
        },
    }
}

impl ProviderWorker {
    #[must_use]
    pub fn new() -> Self {
        let (job_sender, job_receiver) = std::sync::mpsc::channel::<ProviderJob>();
        let (event_sender, event_receiver) = std::sync::mpsc::channel::<ProviderEvent>();
        std::thread::spawn(move || {
            let mut clients: BTreeMap<String, (ProviderAccount, Client)> = BTreeMap::new();
            let mut cache_dir: Option<std::path::PathBuf> = None;
            while let Ok(job) = job_receiver.recv() {
                if let ProviderJob::SetCacheDir(dir) = job {
                    cache_dir = Some(dir);
                    continue;
                }
                let event = match job {
                    ProviderJob::Load { accounts, force } => {
                        handle_load(accounts, &mut clients, cache_dir.as_deref(), force)
                    }
                    ProviderJob::SetCacheDir(_) => unreachable!(),
                    ProviderJob::Resolve {
                        request,
                        provider_id,
                        data,
                    } => {
                        let result = match clients.get(&provider_id) {
                            Some((_, Client::Stalker(client))) => {
                                client.resolve_stream(&data, now_epoch())
                            }
                            _ => Err("No Stalker portal for this channel".to_string()),
                        };
                        ProviderEvent::Resolved { request, result }
                    }
                    ProviderJob::ResolveCatchup {
                        request,
                        provider_id,
                        data,
                        start_text,
                        duration_minutes,
                    } => {
                        let result = match clients.get(&provider_id) {
                            Some((_, Client::Stalker(client))) => client.resolve_catchup(
                                &data,
                                &start_text,
                                duration_minutes,
                                now_epoch(),
                            ),
                            _ => Err("No Stalker portal for this channel".to_string()),
                        };
                        ProviderEvent::Resolved { request, result }
                    }
                    ProviderJob::Account { provider_id } => {
                        let (report, max_connections) = match clients.get(&provider_id) {
                            Some((account, client)) => {
                                let payload = match client {
                                    Client::Xtream(c) => c.account_info(),
                                    Client::Stalker(c) => c.account_info(now_epoch()),
                                };
                                match payload {
                                    Ok(p) => (
                                        Ok(format_account_report(account, &p, now_epoch())),
                                        max_connections_in(&p),
                                    ),
                                    Err(e) => (Err(e), None),
                                }
                            }
                            None => (Err("Unknown provider account".to_string()), None),
                        };
                        ProviderEvent::Account {
                            report,
                            max_connections,
                            provider_id,
                        }
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

    /// Set the directory provider playlists are cached in.
    pub fn set_cache_dir(&self, dir: std::path::PathBuf) {
        let _ = self.jobs.send(ProviderJob::SetCacheDir(dir));
    }

    pub fn load(&self, accounts: Vec<ProviderAccount>) {
        let _ = self.jobs.send(ProviderJob::Load {
            accounts,
            force: false,
        });
    }

    /// Load bypassing the fresh provider cache (B13 forced refresh);
    /// the stale cache still serves when a fetch fails.
    pub fn load_fresh(&self, accounts: Vec<ProviderAccount>) {
        let _ = self.jobs.send(ProviderJob::Load {
            accounts,
            force: true,
        });
    }

    pub fn resolve_catchup(
        &self,
        request: u64,
        provider_id: String,
        data: BTreeMap<String, String>,
        start_text: String,
        duration_minutes: i64,
    ) {
        let _ = self.jobs.send(ProviderJob::ResolveCatchup {
            request,
            provider_id,
            data,
            start_text,
            duration_minutes,
        });
    }

    pub fn resolve(&self, request: u64, provider_id: String, data: BTreeMap<String, String>) {
        let _ = self.jobs.send(ProviderJob::Resolve {
            request,
            provider_id,
            data,
        });
    }

    pub fn account(&self, provider_id: String) {
        let _ = self.jobs.send(ProviderJob::Account { provider_id });
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<ProviderEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Account report (port of account_info.py formatting)
// ---------------------------------------------------------------------------

const SECRET_KEY_PARTS: &[&str] = &[
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "auth",
];

fn is_secret_key(lowered: &str) -> bool {
    SECRET_KEY_PARTS.iter().any(|part| lowered.contains(part))
}

/// The server address with any credentials it carries stripped.
#[must_use]
pub fn redact_server_url(url: &str, username: &str, password: &str) -> String {
    let mut text = url.trim().to_string();
    if let Some((_, rest)) = text.split_once("://") {
        if let Some((_, after)) = rest.split_once('@') {
            let scheme = text.split("://").next().unwrap_or("http").to_string();
            text = format!("{scheme}://{after}");
        }
    }
    // Path/query credentials (path-form Xtream URLs embed user/password).
    let redact = |text: &str| -> String {
        text.split('/')
            .map(|seg| {
                if !username.is_empty() && seg == username
                    || !password.is_empty() && seg == password
                {
                    "***"
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    };
    let mut text = redact(&text);
    if let Some((head, query_string)) = text.clone().split_once('?') {
        let pairs: Vec<String> = query_string
            .split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                if (k == "username" && v == username) || (k == "password" && v == password) {
                    format!("{k}=***")
                } else {
                    pair.to_string()
                }
            })
            .collect();
        text = format!("{}?{}", head, pairs.join("&"));
    }
    text
}

/// A MAC with everything but the first two octets hidden.
#[must_use]
pub fn mask_mac(mac: &str) -> String {
    let text = mac.trim();
    if text.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() >= 3 {
        let mut masked = vec![parts[0], parts[1]];
        masked.extend(vec!["**"; parts.len() - 2]);
        masked.join(":")
    } else {
        text.to_string()
    }
}

#[allow(clippy::cast_possible_truncation)] // Python int(float()) truncates identically
fn as_timestamp(value: Option<&Value>) -> Option<i64> {
    let raw = value?;
    let parsed = match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    let ts = parsed as i64;
    (ts > 0).then_some(ts)
}

fn local_datetime(epoch: i64) -> String {
    chrono::DateTime::from_timestamp(epoch, 0).map_or_else(
        || epoch.to_string(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

fn truthy_flag(value: Option<&Value>) -> bool {
    truthy(value)
}

fn status_label(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "active" => "Active".to_string(),
        "expired" => "Expired".to_string(),
        "banned" => "Banned".to_string(),
        "disabled" => "Disabled".to_string(),
        "pending" => "Pending".to_string(),
        _ => raw.to_string(),
    }
}

fn extra_lines(data: &Value, handled: &[&str]) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(obj) = data.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if handled.contains(&key.as_str()) || is_secret_key(&key.to_lowercase()) {
                continue;
            }
            let value = &obj[key];
            let text = match value {
                Value::Array(items) => items
                    .iter()
                    .map(value_text)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
                Value::Object(_) => continue,
                Value::Bool(b) => {
                    if *b {
                        "Yes".to_string()
                    } else {
                        "No".to_string()
                    }
                }
                other => value_text(other),
            };
            if !text.trim().is_empty() {
                lines.push(format!("{key}: {}", text.trim()));
            }
        }
    }
    lines
}

/// The linear, screen-reader-friendly account report for one provider.
#[must_use]
pub fn format_account_report(account: &ProviderAccount, payload: &Value, now: i64) -> String {
    let (kind_label, name, base, username, mac) = match account {
        ProviderAccount::Xtream(cfg) => (
            "Xtream Codes",
            cfg.name.clone().unwrap_or_else(|| cfg.username.clone()),
            cfg.base_url.clone(),
            cfg.username.clone(),
            String::new(),
        ),
        ProviderAccount::Stalker(cfg) => (
            "Stalker Portal",
            cfg.name.clone().unwrap_or_else(|| cfg.username.clone()),
            cfg.base_url.clone(),
            cfg.username.clone(),
            cfg.mac.clone(),
        ),
    };
    let (username_for_redact, password) = match account {
        ProviderAccount::Xtream(cfg) => (cfg.username.clone(), cfg.password.clone()),
        ProviderAccount::Stalker(cfg) => (cfg.username.clone(), cfg.password.clone()),
    };
    let mut lines = vec![
        format!(
            "Account: {}",
            if name.is_empty() {
                host_of(&base)
            } else {
                name
            }
        ),
        format!("Type: {kind_label}"),
        format!(
            "Server: {}",
            redact_server_url(&base, &username_for_redact, &password)
        ),
    ];
    if !username.is_empty() {
        lines.push(format!("Username: {username}"));
    }
    if !mac.is_empty() {
        lines.push(format!("MAC address: {}", mask_mac(&mac)));
    }
    match account {
        ProviderAccount::Xtream(_) => lines.extend(xtream_report(payload, now)),
        ProviderAccount::Stalker(_) => lines.extend(stalker_report(payload)),
    }
    if payload.is_null() || payload.as_object().is_some_and(serde_json::Map::is_empty) {
        lines.push(String::new());
        lines.push("The server did not report any account details.".to_string());
    }
    lines.join("\n")
}

fn expiry_lines(value: Option<&Value>, now: i64) -> Vec<String> {
    let Some(ts) = as_timestamp(value) else {
        return vec!["Expires: Never (no expiry date set)".to_string()];
    };
    let mut lines = vec![format!("Expires: {}", local_datetime(ts))];
    let days = (ts - now) / 86_400;
    if ts >= now {
        lines.push(format!("Days remaining: {days}"));
    } else {
        lines.push(format!("Days since expiry: {}", (now - ts) / 86_400));
    }
    lines
}

/// The account's declared connection limit from an account-info
/// payload (`user_info.max_connections`, string or number).
/// `None` when the provider does not declare one — callers must
/// not assume a capacity they were never told (issue #134).
#[must_use]
pub fn max_connections_in(payload: &Value) -> Option<i64> {
    let raw = payload.get("user_info")?.get("max_connections")?;
    match raw {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
    .filter(|limit| *limit > 0)
}

/// The identity of a provider account for connection accounting:
/// same host and same username means the same account (the rule
/// `account_info.py` uses).
#[must_use]
pub fn account_key(base_url: &str, username: &str) -> String {
    format!("{}|{}", host_of(base_url).to_lowercase(), username)
}

impl ProviderAccount {
    /// This account's connection-accounting key.
    #[must_use]
    pub fn account_key(&self) -> String {
        match self {
            Self::Xtream(cfg) => account_key(&cfg.base_url, &cfg.username),
            Self::Stalker(cfg) => account_key(&cfg.base_url, &cfg.mac),
        }
    }
}

/// The connection-accounting key of the account a channel
/// belongs to, when it can be determined: an explicit provider
/// id on the channel wins; otherwise an Xtream live URL that
/// sits on an account's host and carries its username as a path
/// segment identifies the account. `None` means the channel's
/// account is unknown and no capacity may be assumed for it.
#[must_use]
pub fn channel_account_key(
    channel: &crate::Channel,
    accounts: &[ProviderAccount],
) -> Option<String> {
    if let Some(provider_id) = channel.unknown_attributes.get("provider-id") {
        if let Some(account) = accounts
            .iter()
            .find(|a| a.provider_id().as_deref() == Some(provider_id.as_str()))
        {
            return Some(account.account_key());
        }
    }
    let url = channel.url.trim();
    if url.is_empty() {
        return None;
    }
    let host = host_of(url).to_lowercase();
    let segments: Vec<&str> = url.split('/').collect();
    for account in accounts {
        let ProviderAccount::Xtream(cfg) = account else {
            continue;
        };
        if host_of(&cfg.base_url).to_lowercase() == host
            && !cfg.username.is_empty()
            && segments.iter().any(|seg| *seg == cfg.username)
        {
            return Some(account.account_key());
        }
    }
    None
}

fn xtream_report(payload: &Value, now: i64) -> Vec<String> {
    let user = payload.get("user_info").cloned().unwrap_or(Value::Null);
    let server = payload.get("server_info").cloned().unwrap_or(Value::Null);
    let mut lines = vec![String::new(), "Subscription".to_string()];
    let auth_failed = user.get("auth").is_some() && !truthy_flag(user.get("auth"));
    if auth_failed {
        lines.push(
            "Status: Authentication failed – the server rejected these credentials".to_string(),
        );
    } else if let Some(status) = user.get("status").and_then(|v| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }) {
        lines.push(format!("Status: {}", status_label(&status)));
    } else {
        lines.push("Status: Not reported by the server".to_string());
    }
    if !auth_failed {
        if user.is_object() {
            lines.extend(expiry_lines(user.get("exp_date"), now));
        } else {
            lines.push("Expires: Not reported by the server".to_string());
        }
    }
    if user.get("is_trial").is_some() {
        lines.push(format!(
            "Trial account: {}",
            if truthy_flag(user.get("is_trial")) {
                "Yes"
            } else {
                "No"
            }
        ));
    }
    for (key, label) in [
        ("active_cons", "Connections in use"),
        ("max_connections", "Maximum connections"),
    ] {
        if let Some(v) = user.get(key) {
            if !matches!(v, Value::Null) && !value_text(v).is_empty() {
                lines.push(format!("{label}: {}", value_text(v)));
            }
        }
    }
    if let Some(created) = as_timestamp(user.get("created_at")) {
        lines.push(format!("Created: {}", local_datetime(created)));
    }
    if let Some(formats) = user.get("allowed_output_formats").and_then(Value::as_array) {
        if !formats.is_empty() {
            let joined = formats
                .iter()
                .map(value_text)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Allowed formats: {joined}"));
        }
    }
    if let Some(message) = user
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        lines.push(format!("Message from provider: {message}"));
    }
    let extras = extra_lines(
        &user,
        &[
            "username",
            "password",
            "auth",
            "status",
            "exp_date",
            "is_trial",
            "active_cons",
            "max_connections",
            "created_at",
            "allowed_output_formats",
            "message",
        ],
    );
    if server.is_object() {
        lines.extend(xtream_server_lines(&server));
    }
    if !extras.is_empty() {
        lines.push(String::new());
        lines.push("Other details reported by the server".to_string());
        lines.extend(extras);
    }
    lines
}

/// The Xtream report's server block.
fn xtream_server_lines(server: &Value) -> Vec<String> {
    let mut lines = vec![String::new(), "Server".to_string()];
    if let Some(t) = server.get("time_now") {
        lines.push(format!("Server time: {}", value_text(t)));
    }
    if let Some(t) = server.get("timezone") {
        lines.push(format!("Server time zone: {}", value_text(t)));
    }
    if let Some(host) = server.get("url").and_then(Value::as_str) {
        let port = server.get("port").map(value_text).unwrap_or_default();
        if port.is_empty() {
            lines.push(format!("Server address: {host}"));
        } else {
            lines.push(format!("Server address: {host}:{port}"));
        }
    }
    lines.extend(extra_lines(
        server,
        &[
            "time_now",
            "timezone",
            "url",
            "port",
            "https_port",
            "server_protocol",
        ],
    ));
    lines
}

fn stalker_report(payload: &Value) -> Vec<String> {
    let mut lines = vec![String::new(), "Subscription".to_string()];
    if let Some(status) = payload.get("status") {
        if !matches!(status, Value::Null) && !value_text(status).is_empty() {
            let text = value_text(status);
            let label = match text.trim() {
                "0" => "Disabled".to_string(),
                "1" => "Active".to_string(),
                other => status_label(other),
            };
            lines.push(format!("Status: {label}"));
        }
    }
    let end_date = payload
        .get("end_date")
        .map(value_text)
        .unwrap_or_default()
        .trim()
        .to_string();
    if end_date.is_empty() {
        lines.push("Expires: Not reported by the server".to_string());
    } else if let Some(ts) = as_timestamp(payload.get("end_date")) {
        lines.push(format!("Expires: {}", local_datetime(ts)));
    } else {
        lines.push(format!("Expires: {end_date}"));
    }
    if let Some(plan) = payload.get("tariff_plan") {
        if !value_text(plan).is_empty() {
            lines.push(format!("Package: {}", value_text(plan)));
        }
    }
    for (key, label) in [("account_balance", "Account balance"), ("phone", "Phone")] {
        if let Some(v) = payload.get(key) {
            if !matches!(v, Value::Null) && !value_text(v).is_empty() {
                lines.push(format!("{label}: {}", value_text(v)));
            }
        }
    }
    let extras = extra_lines(
        payload,
        &[
            "status",
            "end_date",
            "tariff_plan",
            "account_balance",
            "phone",
            "mac",
            "fname",
            "login",
        ],
    );
    if !extras.is_empty() {
        lines.push(String::new());
        lines.push("Other details reported by the server".to_string());
        lines.extend(extras);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalization_drops_query_and_fragment() {
        // B16 regression.
        assert_eq!(
            normalize_base_url("http://example.com:8080/get.php?username=u&password=p"),
            "http://example.com:8080"
        );
        assert_eq!(
            normalize_base_url("https://example.com/c/#section"),
            "https://example.com/c"
        );
        assert_eq!(
            normalize_base_url("example.com/portal.php?x=1"),
            "http://example.com"
        );
    }

    #[test]
    fn portal_token_accepts_top_level_and_nested_shapes() {
        // B15 regression: {"token":"abc"} must work like {"js":{...}}.
        let top = serde_json::json!({"token": "abc"});
        assert_eq!(portal_token(&top), Some("abc"));
        let nested = serde_json::json!({"js": {"token": "def"}});
        assert_eq!(portal_token(&nested), Some("def"));
        let both = serde_json::json!({"token": "abc", "js": {"token": "def"}});
        assert_eq!(portal_token(&both), Some("abc"));
        let empty = serde_json::json!({"js": {"token": ""}});
        assert_eq!(portal_token(&empty), None);
        let rejected = serde_json::json!({"js": null});
        assert_eq!(portal_token(&rejected), None);
    }

    #[test]
    fn mac_sanitize_validate_and_form_errors() {
        // B06 regression.
        assert_eq!(sanitize_mac("00-1a-79-12-34-56"), "00:1A:79:12:34:56");
        assert_eq!(sanitize_mac("001a79123456"), "00:1A:79:12:34:56");
        assert_eq!(sanitize_mac("00:1a:79:12:34:56"), "00:1A:79:12:34:56");
        assert!(mac_is_valid("00:1A:79:12:34:56"));
        assert!(!mac_is_valid("00:1A:79:12:34"));
        assert!(!mac_is_valid("00:1A:79:12:34:ZZ"));
        let mac = random_mag_mac();
        assert!(mac.starts_with("00:1A:79:"));
        assert!(mac_is_valid(&mac));
        assert_eq!(
            account_form_error("xtream", " ", "u", "p", "").as_deref(),
            Some("Username, password, and URL are required.")
        );
        assert_eq!(
            account_form_error("xtream", "http://h", "", "p", "").as_deref(),
            Some("Username, password, and URL are required.")
        );
        assert!(account_form_error("xtream", "http://h", "u", "p", "").is_none());
        assert_eq!(
            account_form_error("stalker", "", "", "", "00:1A:79:12:34:56").as_deref(),
            Some("Portal URL is required.")
        );
        assert!(account_form_error("stalker", "http://h", "", "", "00-1a-79-12-34-56").is_none());
        assert!(account_form_error("stalker", "http://h", "", "", "12:34").is_some());
    }

    #[test]
    fn account_edit_merge_preserves_fields_absent_from_the_form() {
        // B04 regression: a name-only edit keeps credentials,
        // output/type, auto-EPG and unknown retained settings.
        let existing = serde_json::json!({
            "type": "stalker",
            "id": "stalker-1",
            "name": "Portal",
            "base_url": "http://portal.example",
            "mac": "00:1A:79:12:34:56",
            "username": "mag-user",
            "password": "mag-pass",
            "auto_epg": false,
            "custom_note": "keep me"
        });
        let mut updates = serde_json::Map::new();
        updates.insert("name".to_string(), serde_json::json!("Renamed"));
        let merged = merge_account_fields(&existing, updates);
        assert_eq!(merged["name"], "Renamed");
        assert_eq!(merged["username"], "mag-user");
        assert_eq!(merged["password"], "mag-pass");
        assert_eq!(merged["mac"], "00:1A:79:12:34:56");
        assert_eq!(merged["auto_epg"], false);
        assert_eq!(merged["custom_note"], "keep me");
        assert_eq!(merged["id"], "stalker-1");
    }

    #[test]
    fn xtream_channels_carry_scope_and_stream_id() {
        // B01 + B10 regression (mapper half).
        let cfg = XtreamConfig {
            provider_id: Some("xtream-9".to_string()),
            ..XtreamConfig::default()
        };
        let mut parsed = crate::parse_m3u(
            "#EXTINF:-1,Movie\nhttp://h/movie/u/p/42.mp4\n#EXTINF:-1,Live\nhttp://h/live/u/p/7.ts\n",
        );
        tag_xtream_channels(&mut parsed, &cfg);
        assert_eq!(
            parsed[0].unknown_attributes[crate::sources::PLAYLIST_ID_ATTRIBUTE],
            "xtream-9"
        );
        assert_eq!(parsed[0].unknown_attributes["stream-id"], "42");
        assert_eq!(parsed[1].unknown_attributes["stream-id"], "7");
        // The favorite key now matches what Python computes for the
        // same entry (provider type, id, stream id, name).
        assert_eq!(
            crate::favorites::channel_key(&parsed[0]),
            "xtream|xtream-9|42|Movie"
        );
    }

    #[test]
    fn base_url_normalization() {
        assert_eq!(normalize_base_url("example.com"), "http://example.com");
        assert_eq!(
            normalize_base_url("http://user:pw@example.com:8080/portal.php"),
            "http://example.com:8080"
        );
        assert_eq!(
            normalize_base_url("https://example.com/c/"),
            "https://example.com/c"
        );
    }

    #[test]
    fn xtream_urls() {
        let client = XtreamClient::new(XtreamConfig {
            base_url: "http://example.com:8080".to_string(),
            username: "u s".to_string(),
            password: "p&w".to_string(),
            ..XtreamConfig::default()
        });
        assert_eq!(
            client.playlist_url(),
            "http://example.com:8080/get.php?username=u+s&password=p%26w&type=m3u_plus&output=ts"
        );
        assert_eq!(
            client.vod_stream_url("42", "mkv"),
            "http://example.com:8080/movie/u%20s/p%26w/42.mkv"
        );
    }

    #[test]
    fn stalker_channel_mapping() {
        // Portal payload rows map to channels with resolution data kept.
        let row = serde_json::json!({
            "name": "Portal News", "cmd": "ffmpeg http://x/1",
            "tv_genre_title": "News", "epg_id": "news.tv",
            "use_http_tmp_link": 1, "id": 7, "archive": 1,
            "tv_archive_duration": 3
        });
        let name = string_field(&row, "name").expect("name");
        assert_eq!(name, "Portal News");
        assert!(truthy(row.get("archive")));
    }

    #[test]
    fn scope_label_uses_python_form_and_fallbacks() {
        let named = ProviderAccount::Xtream(XtreamConfig {
            base_url: "http://example.com".to_string(),
            username: "u".to_string(),
            name: Some("Home line".to_string()),
            ..XtreamConfig::default()
        });
        assert_eq!(named.scope_label(), "Xtream Codes – Home line");
        let unnamed = ProviderAccount::Xtream(XtreamConfig {
            base_url: "http://example.com".to_string(),
            username: "u".to_string(),
            ..XtreamConfig::default()
        });
        assert_eq!(unnamed.scope_label(), "Xtream Codes – u");
        let bare = ProviderAccount::Stalker(StalkerConfig {
            base_url: "http://portal.example".to_string(),
            ..StalkerConfig::default()
        });
        assert_eq!(bare.scope_label(), "Stalker Portal – http://portal.example");
        let empty = ProviderAccount::Stalker(StalkerConfig::default());
        assert_eq!(empty.scope_label(), "Stalker Portal – Provider");
    }

    #[test]
    fn report_redacts_and_masks() {
        let report = format_account_report(
            &ProviderAccount::Stalker(StalkerConfig {
                base_url: "http://portal.example".to_string(),
                mac: "00:1A:79:AA:BB:CC".to_string(),
                username: "bob".to_string(),
                ..StalkerConfig::default()
            }),
            &serde_json::json!({"status": 1, "end_date": "2030-01-01", "tariff_plan": "Full", "password_hint": "x"}),
            1_762_000_000,
        );
        assert!(report.contains("MAC address: 00:1A:**:**:**"), "{report}");
        assert!(report.contains("Status: Active"), "{report}");
        assert!(!report.contains("password_hint"), "{report}");
        assert_eq!(
            redact_server_url(
                "http://user:pw@example.com/get.php?username=u&password=p",
                "u",
                "p"
            ),
            "http://example.com/get.php?username=***&password=***"
        );
    }

    #[test]
    fn xtream_report_lines() {
        let report = format_account_report(
            &ProviderAccount::Xtream(XtreamConfig {
                base_url: "http://example.com".to_string(),
                username: "u".to_string(),
                ..XtreamConfig::default()
            }),
            &serde_json::json!({
                "user_info": {"auth": 1, "status": "Active", "exp_date": "1798761600", "is_trial": "0", "max_connections": "2"},
                "server_info": {"url": "example.com", "port": "8080", "timezone": "UTC"}
            }),
            1_762_000_000,
        );
        assert!(report.contains("Status: Active"), "{report}");
        assert!(report.contains("Days remaining:"), "{report}");
        assert!(
            report.contains("Server address: example.com:8080"),
            "{report}"
        );
    }

    #[test]
    fn config_entries_parse() {
        let xtream = ProviderAccount::from_config(&serde_json::json!({
            "type": "xtream", "url": "http://x", "username": "u", "password": "p", "id": "prov1"
        }))
        .expect("xtream");
        assert_eq!(xtream.provider_id().as_deref(), Some("prov1"));
        assert!(ProviderAccount::from_config(&serde_json::json!({"type": "m3u"})).is_none());
    }

    #[test]
    fn resolution_results_carry_their_request_id() {
        // Issue #68: with no portal loaded every resolution fails,
        // but each result must still arrive tagged with the exact
        // request id it was dispatched with, in dispatch order, so
        // callers can never consume each other's results.
        let worker = ProviderWorker::new();
        let data = std::collections::BTreeMap::new();
        worker.resolve(41, "missing".to_string(), data.clone());
        worker.resolve_catchup(
            42,
            "missing".to_string(),
            data.clone(),
            "2026-01-01 00:00:00".to_string(),
            30,
        );
        worker.resolve(43, "missing".to_string(), data);
        let mut ids = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ids.len() < 3 && std::time::Instant::now() < deadline {
            match worker.try_recv() {
                Some(ProviderEvent::Resolved { request, result }) => {
                    assert!(result.is_err());
                    ids.push(request);
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        assert_eq!(ids, vec![41, 42, 43]);
    }

    fn bare_channel(name: &str, url: &str) -> crate::Channel {
        crate::Channel {
            name: name.to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn max_connections_reads_strings_and_numbers() {
        let payload = serde_json::json!({"user_info": {"max_connections": "2"}});
        assert_eq!(max_connections_in(&payload), Some(2));
        let payload = serde_json::json!({"user_info": {"max_connections": 1}});
        assert_eq!(max_connections_in(&payload), Some(1));
        // Undeclared or useless limits are never assumed.
        assert_eq!(
            max_connections_in(&serde_json::json!({"user_info": {}})),
            None
        );
        let payload = serde_json::json!({"user_info": {"max_connections": "0"}});
        assert_eq!(max_connections_in(&payload), None);
        assert_eq!(max_connections_in(&serde_json::json!({})), None);
    }

    #[test]
    fn channel_account_key_matches_by_id_then_url() {
        let account = ProviderAccount::Xtream(XtreamConfig {
            base_url: "http://panel.example.com:8080".to_string(),
            username: "alice".to_string(),
            provider_id: Some("prov-1".to_string()),
            ..XtreamConfig::default()
        });
        let accounts = vec![account];
        let mut by_id = bare_channel("A", "");
        by_id
            .unknown_attributes
            .insert("provider-id".to_string(), "prov-1".to_string());
        assert_eq!(
            channel_account_key(&by_id, &accounts),
            Some("panel.example.com:8080|alice".to_string())
        );
        let by_url = bare_channel("B", "http://panel.example.com:8080/alice/pass/123.ts");
        assert_eq!(
            channel_account_key(&by_url, &accounts),
            Some("panel.example.com:8080|alice".to_string())
        );
        // A different host, or the same host without the
        // account's username, belongs to no known account.
        let foreign = bare_channel("C", "http://other.example.com/alice/pass/123.ts");
        assert_eq!(channel_account_key(&foreign, &accounts), None);
        let no_user = bare_channel("D", "http://panel.example.com:8080/bob/pass/123.ts");
        assert_eq!(channel_account_key(&no_user, &accounts), None);
    }
}
