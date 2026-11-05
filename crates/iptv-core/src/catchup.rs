//! Catch-up (archive) playback resolution, ported from Python's
//! `catchup_urls.py` and the `client_epg.py` resolution rules: whether a
//! channel has catch-up, whether a programme is inside the window, and the
//! exact archive URL for a finished programme.

use chrono::{Datelike, NaiveDateTime, TimeZone, Timelike};

use crate::playback::split_stream_modifiers;
use crate::Channel;

fn attr<'a>(channel: &'a Channel, key: &str) -> &'a str {
    channel
        .unknown_attributes
        .get(key)
        .map_or("", String::as_str)
}

#[allow(clippy::cast_possible_truncation)] // Python int(float()) truncates identically
fn parse_days(value: &str) -> Option<i64> {
    let parsed: f64 = value.trim().parse().ok()?;
    Some(parsed as i64)
}

/// The M3U `timeshift="N"` archive window in days, when positive.
#[must_use]
pub fn positive_timeshift_days(channel: &Channel) -> Option<i64> {
    let days = parse_days(attr(channel, "timeshift"))?;
    (days > 0).then_some(days)
}

/// The inferred template for providers that mark archive capability with
/// only `timeshift` (teleelevidenie's dynamic playlists).
#[must_use]
pub fn implicit_catchup_source(channel: &Channel) -> String {
    if positive_timeshift_days(channel).is_none() {
        return String::new();
    }
    let raw_url = channel.url.split('|').next().unwrap_or("").trim();
    let host = url_host(raw_url).to_lowercase();
    let host = host.trim_end_matches('.');
    if host == "teleelevidenie.com" || host.ends_with(".teleelevidenie.com") {
        "?utc=${start}&lutc=${timestamp}".to_string()
    } else {
        String::new()
    }
}

fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or(rest);
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    hostport.split(':').next().unwrap_or(hostport).to_string()
}

/// Port of `_channel_has_catchup`.
#[must_use]
pub fn channel_has_catchup(channel: &Channel) -> bool {
    if !attr(channel, "catchup-source").is_empty() || !attr(channel, "catchup").is_empty() {
        return true;
    }
    if !implicit_catchup_source(channel).is_empty() {
        return true;
    }
    if attr(channel, "provider-type") == "stalker" {
        return is_truthy(attr(channel, "allow_timeshift")) || is_truthy(attr(channel, "archive"));
    }
    false
}

fn is_truthy(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes")
}

/// Port of `_within_catchup_window`: no stated window means no limit, and
/// an unparseable window is not a reason to refuse.
#[must_use]
pub fn within_catchup_window(channel: &Channel, start: i64, now: i64) -> bool {
    let days =
        parse_days(attr(channel, "catchup-days")).or_else(|| positive_timeshift_days(channel));
    let Some(days) = days else {
        return true;
    };
    start >= now - days * 86_400
}

/// The wall-clock reading of an instant on the provider's archive clock:
/// the channel's `catchup-timezone` when it names UTC or a known IANA
/// zone, else local time.
fn utc_dt(epoch: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .timestamp_opt(epoch, 0)
        .single()
        .unwrap_or_else(|| chrono::Utc.timestamp_opt(0, 0).unwrap())
}

fn wall_naive(epoch: i64, tz_name: &str) -> NaiveDateTime {
    let name = tz_name.trim();
    if matches!(
        name.to_uppercase().as_str(),
        "UTC" | "GMT" | "Z" | "ETC/UTC" | "ETC/GMT"
    ) {
        return utc_dt(epoch).naive_utc();
    }
    if !name.is_empty() {
        if let Ok(tz) = name.parse::<chrono_tz::Tz>() {
            return utc_dt(epoch).with_timezone(&tz).naive_local();
        }
    }
    utc_dt(epoch).with_timezone(&chrono::Local).naive_local()
}

fn fmt_wall(naive: NaiveDateTime, pattern: &str) -> String {
    // Only the two patterns the reference uses.
    if pattern == "%Y%m%d%H%M%S" {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            naive.year(),
            naive.month(),
            naive.day(),
            naive.hour(),
            naive.minute(),
            naive.second()
        )
    } else {
        format!(
            "{:04}-{:02}-{:02}:{:02}-{:02}",
            naive.year(),
            naive.month(),
            naive.day(),
            naive.hour(),
            naive.minute()
        )
    }
}

const TEMPLATE_TOKENS: &[(&str, &str)] = &[
    ("${start}", "start"),
    ("${timestamp}", "lutc"),
    ("${duration}", "duration"),
    ("${end}", "end"),
    ("{utc}", "start"),
    ("{lutc}", "lutc"),
    ("{utcstart}", "start"),
    ("{utcend}", "end"),
    ("{duration}", "duration"),
    ("{S}", "start"),
    ("{E}", "end"),
    ("{L}", "lutc"),
    ("{D}", "duration"),
    ("{Y}", "start_year"),
    ("{m}", "start_month"),
    ("{d}", "start_day"),
    ("{H}", "start_hour"),
    ("{M}", "start_minute"),
    ("{y}", "end_year"),
    ("{e}", "end_day"),
    ("{h}", "end_hour"),
    ("{i}", "end_minute"),
];

fn token_value(
    kind: &str,
    start: i64,
    stop: i64,
    now: i64,
    wall_start: NaiveDateTime,
    wall_end: NaiveDateTime,
) -> String {
    match kind {
        "start" => start.to_string(),
        "end" => stop.to_string(),
        "lutc" => now.to_string(),
        "duration" => (stop - start).max(1).to_string(),
        "start_year" => format!("{:04}", wall_start.year()),
        "start_month" => format!("{:02}", wall_start.month()),
        "start_day" => format!("{:02}", wall_start.day()),
        "start_hour" => format!("{:02}", wall_start.hour()),
        "start_minute" => format!("{:02}", wall_start.minute()),
        "end_year" => format!("{:04}", wall_end.year()),
        "end_day" => format!("{:02}", wall_end.day()),
        "end_hour" => format!("{:02}", wall_end.hour()),
        "end_minute" => format!("{:02}", wall_end.minute()),
        _ => String::new(),
    }
}

/// Fill a `catchup-source` template in, or None when the source is not a
/// template (port of `_expand_catchup_template`).
#[must_use]
pub fn expand_catchup_template(
    source: &str,
    start: i64,
    stop: i64,
    offset_hours: f64,
    tz_name: &str,
    now: i64,
) -> Option<String> {
    let text = source.trim();
    if text.is_empty()
        || !TEMPLATE_TOKENS
            .iter()
            .any(|(token, _)| text.contains(token))
    {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // whole-hour offsets in practice
    let shift = (offset_hours * 3600.0) as i64;
    let start = start - shift;
    let stop = stop - shift;
    let wall_start = wall_naive(start, tz_name);
    let wall_end = wall_naive(stop, tz_name);
    let mut expanded = text.to_string();
    for (token, kind) in TEMPLATE_TOKENS {
        if expanded.contains(token) {
            let value = token_value(kind, start, stop, now, wall_start, wall_end);
            expanded = expanded.replace(token, &value);
        }
    }
    Some(expanded)
}

/// Percent-encode a User-Agent the way Python's `urllib.parse.quote` does
/// with its default safe set (`/` plus the unreserved characters).
fn quote_ua(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The stream id at the end of a URL path (port of `_extract_stream_id`).
#[must_use]
pub fn extract_stream_id(url: &str) -> String {
    let path = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let clean: String = path
        .chars()
        .take_while(|c| *c != '?' && *c != '#')
        .collect();
    let parts: Vec<&str> = clean.split('/').filter(|p| !p.is_empty()).collect();
    let leading_digits =
        |part: &str| -> String { part.chars().take_while(char::is_ascii_digit).collect() };
    if let Some(last) = parts.last() {
        let digits = leading_digits(last);
        if !digits.is_empty() {
            return digits;
        }
    }
    if parts.len() >= 2 {
        return leading_digits(parts[parts.len() - 2]);
    }
    String::new()
}

/// The Xtream archive URL implied by a live URL (port of
/// `_xtream_timeshift_url`): `/live/U/P/ID` becomes
/// `/timeshift/U/P/MINUTES/START/ID.ts`, keeping the query and the
/// header-modifier tail.
#[must_use]
pub fn xtream_timeshift_url(channel: &Channel, start: i64, stop: i64) -> String {
    use std::fmt::Write as _;
    let (raw_url, modifiers) = match channel.url.split_once('|') {
        Some((head, tail)) => (head, tail),
        None => (channel.url.as_str(), ""),
    };
    let raw_url = raw_url.trim();
    let Some((scheme, rest)) = raw_url.split_once("://") else {
        return String::new();
    };
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let (authority, path_and_query) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => return String::new(),
    };
    if authority.is_empty() {
        return String::new();
    }
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (user, password, id_part) = match segments.as_slice() {
        [u, p, id] => (*u, *p, *id),
        [live, u, p, id] if *live == "live" => (*u, *p, *id),
        _ => return String::new(),
    };
    let stream_id: String = id_part
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if stream_id.is_empty() || stream_id.len() != id_part.split('.').next().unwrap_or("").len() {
        return String::new();
    }
    if let Some(ext) = id_part.split_once('.').map(|(_, e)| e) {
        if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return String::new();
        }
    }
    let duration = ((stop - start) / 60).max(1);
    #[allow(clippy::cast_possible_truncation)] // whole-hour offsets in practice
    let offset_seconds = attr(channel, "catchup-offset")
        .parse::<f64>()
        .map_or(0, |h| (h * 3600.0) as i64);
    let wall = wall_naive(start - offset_seconds, attr(channel, "catchup-timezone"));
    let stamp = fmt_wall(wall, "%Y-%m-%d:%H-%M");
    let mut url = format!(
        "{scheme}://{authority}/timeshift/{user}/{password}/{duration}/{stamp}/{stream_id}.ts"
    );
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let mut modifiers = modifiers.trim().to_string();
    let ua = attr(channel, "http-user-agent");
    if !ua.is_empty() {
        let (_, tail) = split_stream_modifiers(&format!("x|{modifiers}"));
        let has_ua = tail.get("User-Agent").is_some()
            || tail
                .extra
                .iter()
                .any(|l| l.to_lowercase().starts_with("user-agent:"));
        if !has_ua {
            if !modifiers.is_empty() {
                modifiers.push('|');
            }
            let _ = write!(modifiers, "User-Agent={}", quote_ua(ua));
        }
    }
    if modifiers.is_empty() {
        url
    } else {
        format!("{url}|{modifiers}")
    }
}

/// Build the generic archive URL for a programme (port of
/// `_build_generic_catchup_url`); None when none can be built.
#[must_use]
pub fn build_catchup_url(channel: &Channel, start: i64, stop: i64, now: i64) -> Option<String> {
    use std::fmt::Write as _;
    if stop <= start {
        return None;
    }
    let mut source = attr(channel, "catchup-source").to_string();
    if source.is_empty() {
        source = implicit_catchup_source(channel);
    }
    if source.is_empty() {
        if attr(channel, "catchup").trim().to_lowercase() != "xc" {
            return None;
        }
        let url = xtream_timeshift_url(channel, start, stop);
        return if url.is_empty() { None } else { Some(url) };
    }
    let offset_hours = attr(channel, "catchup-offset")
        .parse::<f64>()
        .unwrap_or(0.0);
    let tz = attr(channel, "catchup-timezone");
    if let Some(expanded) = expand_catchup_template(&source, start, stop, offset_hours, tz, now) {
        let mut url = if expanded.to_lowercase().starts_with("http://")
            || expanded.to_lowercase().starts_with("https://")
        {
            expanded
        } else {
            let base = channel.url.split('|').next().unwrap_or("").trim_end();
            if base.contains('?') && expanded.starts_with('?') {
                format!("{}&{}", base, &expanded[1..])
            } else {
                format!("{base}{expanded}")
            }
        };
        let ua = attr(channel, "http-user-agent");
        if !ua.is_empty() && !url.contains('|') {
            let _ = write!(url, "|User-Agent={}", quote_ua(ua));
        }
        return Some(url);
    }
    // Path style: <source>/<start>/<duration>/ (or flussonic).
    let mut stream_id = attr(channel, "stream-id").to_string();
    if stream_id.is_empty() {
        stream_id = extract_stream_id(&channel.url);
    }
    if stream_id.is_empty() {
        return None;
    }
    let mut src = source.trim_end_matches('/').to_string();
    if src.is_empty() {
        return None;
    }
    let last = src.rsplit('/').next().unwrap_or("");
    if !last.chars().all(|c| c.is_ascii_digit()) || last.is_empty() {
        src = format!("{src}/{stream_id}");
    }
    #[allow(clippy::cast_possible_truncation)] // whole-hour offsets in practice
    let shift = (offset_hours * 3600.0) as i64;
    let start_local = wall_naive(start - shift, tz);
    let duration = ((stop - start) / 60).max(1);
    let ctype = {
        let t = attr(channel, "catchup-type");
        if t.is_empty() {
            attr(channel, "catchup")
        } else {
            t
        }
    }
    .to_lowercase();
    let mut url = if ctype == "flussonic" {
        format!(
            "{src}/{}-{duration}.m3u8",
            fmt_wall(start_local, "%Y%m%d%H%M%S")
        )
    } else {
        format!(
            "{src}/{}/{duration}/",
            fmt_wall(start_local, "%Y-%m-%d:%H-%M")
        )
    };
    let ua = attr(channel, "http-user-agent");
    if !ua.is_empty() {
        if url.contains('|') {
            let _ = write!(url, "&User-Agent={}", quote_ua(ua));
        } else {
            let _ = write!(url, "|User-Agent={}", quote_ua(ua));
        }
    }
    Some(url)
}

/// How a programme resolves for playback (port of `_resolve_show_url`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowResolution {
    /// Play the channel's live URL.
    Live(String),
    /// Play this archive URL.
    Catchup(String),
    /// Resolve the live link through the Stalker portal.
    StalkerLive,
    /// Resolve an archive link through the Stalker portal.
    StalkerCatchup {
        start_text: String,
        duration_minutes: i64,
    },
}

/// Plain words for an ffmpeg exit code, or None when the code is not
/// a known provider failure (port of `_ffmpeg_exit_reason` in
/// `catchup_urls.py`). ffmpeg exits with the `AVERROR` of whatever
/// stopped it, and Windows reports that as an unsigned 32-bit number
/// (a refused download reads "code 3436169992", which is HTTP 403),
/// so an over-large code is wrapped back to signed first.
#[must_use]
pub fn ffmpeg_exit_reason(code: i64) -> Option<&'static str> {
    let mut code = code;
    if code > i64::from(i32::MAX) {
        code -= 1 << 32;
    }
    Some(match code {
        -858_797_304 => {
            "The provider refused access (HTTP 403 Forbidden). Many providers allow only one stream per account at a time: stop other playback or downloads from this account and try again."
        }
        -825_242_872 => {
            "The provider asked for a login (HTTP 401 Unauthorized). Check the playlist's user name and password."
        }
        -875_574_520 => {
            "The provider does not have this programme (HTTP 404 Not Found). It may be older than the provider's archive."
        }
        -808_465_656 | -959_591_672 | -1_482_175_736 => {
            "The provider rejected the request (HTTP 4xx error)."
        }
        -1_482_175_992 => "The provider's server had an error (HTTP 5xx). Try again later.",
        -1_094_995_529 => "The stream sent data that could not be read.",
        -138 | -110 | -10060 => "The connection to the provider timed out.",
        -107 | -111 | -10061 => "The provider's server refused the connection.",
        _ => return None,
    })
}

/// Resolve a programme against the channel's catch-up capability.
///
/// # Errors
///
/// Returns the reference's messages when the channel has no catch-up, the
/// programme is outside the window, or its times are invalid.
pub fn resolve_show_url(
    channel: &Channel,
    start: i64,
    stop: i64,
    now: i64,
) -> Result<ShowResolution, String> {
    let stalker = attr(channel, "provider-type") == "stalker";
    if start <= now && now <= stop {
        return Ok(if stalker {
            ShowResolution::StalkerLive
        } else {
            ShowResolution::Live(channel.url.clone())
        });
    }
    if stop < now {
        if !channel_has_catchup(channel) {
            return Err("This channel does not provide catch-up streaming.".to_string());
        }
        if !within_catchup_window(channel, start, now) {
            return Err("This programme is older than the catch-up window allows.".to_string());
        }
        if stop <= start {
            return Err("This programme's start and end times are invalid.".to_string());
        }
        if stalker {
            let local = chrono::Utc
                .timestamp_opt(start, 0)
                .single()
                .map_or_else(
                    || chrono::Local.timestamp_opt(0, 0).unwrap(),
                    |dt| dt.with_timezone(&chrono::Local),
                )
                .naive_local();
            return Ok(ShowResolution::StalkerCatchup {
                start_text: fmt_wall(local, "%Y-%m-%d:%H-%M"),
                duration_minutes: ((stop - start) / 60).max(1),
            });
        }
        return build_catchup_url(channel, start, stop, now)
            .map(ShowResolution::Catchup)
            .ok_or_else(|| "Unable to construct catch-up URL for this programme.".to_string());
    }
    // Future programme: the live stream, which starts when available.
    Ok(if stalker {
        ShowResolution::StalkerLive
    } else {
        ShowResolution::Live(channel.url.clone())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn ffmpeg_exit_reasons_match_python() {
        // Every code in Python's `_FFMPEG_EXIT_REASONS`, checked
        // against the oracle's own answers.
        assert!(ffmpeg_exit_reason(-858_797_304)
            .is_some_and(|r| r.starts_with("The provider refused access (HTTP 403")));
        // The same code as Windows reports it (unsigned 32-bit).
        assert_eq!(
            ffmpeg_exit_reason(3_436_169_992),
            ffmpeg_exit_reason(-858_797_304)
        );
        assert!(
            ffmpeg_exit_reason(-825_242_872).is_some_and(|r| r.contains("HTTP 401 Unauthorized"))
        );
        assert!(ffmpeg_exit_reason(-875_574_520).is_some_and(|r| r.contains("HTTP 404 Not Found")));
        for code in [-808_465_656, -959_591_672, -1_482_175_736] {
            assert_eq!(
                ffmpeg_exit_reason(code),
                Some("The provider rejected the request (HTTP 4xx error)."),
                "{code}"
            );
        }
        assert!(ffmpeg_exit_reason(-1_482_175_992).is_some_and(|r| r.contains("HTTP 5xx")));
        assert_eq!(
            ffmpeg_exit_reason(-1_094_995_529),
            Some("The stream sent data that could not be read.")
        );
        for code in [-138, -110, -10060] {
            assert_eq!(
                ffmpeg_exit_reason(code),
                Some("The connection to the provider timed out."),
                "{code}"
            );
        }
        for code in [-107, -111, -10061] {
            assert_eq!(
                ffmpeg_exit_reason(code),
                Some("The provider's server refused the connection."),
                "{code}"
            );
        }
        assert_eq!(ffmpeg_exit_reason(-1), None);
        assert_eq!(ffmpeg_exit_reason(1), None);
        assert_eq!(ffmpeg_exit_reason(0), None);
        // The one-stream refusal is the 403 sentence, word for word.
        assert_eq!(
            ffmpeg_exit_reason(-858_797_304),
            Some(crate::catchup_direct::ONE_STREAM_REFUSAL)
        );
    }

    use super::*;
    use std::collections::BTreeMap;

    fn channel(url: &str, attrs: &[(&str, &str)]) -> Channel {
        Channel {
            name: "Chan".to_string(),
            group: String::new(),
            url: url.to_string(),
            tvg_id: String::new(),
            tvg_name: String::new(),
            tvg_logo: String::new(),
            unknown_attributes: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    // 2026-11-01 18:00 UTC.
    const START: i64 = 1_793_556_000;
    const STOP: i64 = START + 3600;

    #[test]
    fn template_expansion_uses_epochs_and_wall_clock() {
        let out = expand_catchup_template(
            "?utc=${start}&lutc=${timestamp}&d={D}&at={Y}-{m}-{d} {H}:{M}",
            START,
            STOP,
            0.0,
            "UTC",
            START + 7200,
        )
        .expect("template");
        assert_eq!(
            out,
            format!(
                "?utc={START}&lutc={}&d=3600&at=2026-11-01 18:00",
                START + 7200
            )
        );
    }

    #[test]
    fn xtream_timeshift_from_live_url() {
        let ch = channel(
            "http://example.com:8080/live/user/pass/123.ts",
            &[("catchup", "xc")],
        );
        let url = xtream_timeshift_url(&ch, START, STOP);
        assert_eq!(
            url,
            format!(
                "http://example.com:8080/timeshift/user/pass/60/{}/123.ts",
                fmt_wall(wall_naive(START, ""), "%Y-%m-%d:%H-%M")
            )
        );
        // Catch-up timezone UTC pins the stamp exactly.
        let ch_utc = channel(
            "http://example.com/live/user/pass/123.ts",
            &[("catchup", "xc"), ("catchup-timezone", "UTC")],
        );
        assert_eq!(
            xtream_timeshift_url(&ch_utc, START, STOP),
            "http://example.com/timeshift/user/pass/60/2026-11-01:18-00/123.ts"
        );
    }

    #[test]
    fn teleelevidenie_implicit_template_appends() {
        let ch = channel(
            "https://teleelevidenie.com/chan/index.m3u8",
            &[("timeshift", "3")],
        );
        assert!(channel_has_catchup(&ch));
        let resolved = resolve_show_url(&ch, START, STOP, STOP + 60).expect("resolve");
        match resolved {
            ShowResolution::Catchup(url) => {
                assert!(
                    url.starts_with("https://teleelevidenie.com/chan/index.m3u8?utc="),
                    "{url}"
                );
            }
            other => panic!("expected catchup, got {other:?}"),
        }
    }

    #[test]
    fn window_and_errors() {
        let ch = channel(
            "http://example.com/live/u/p/1.ts",
            &[("catchup", "xc"), ("catchup-days", "2")],
        );
        let old = resolve_show_url(&ch, START - 10 * 86_400, START - 10 * 86_400 + 3600, STOP);
        assert_eq!(
            old.expect_err("too old"),
            "This programme is older than the catch-up window allows."
        );
        let plain = channel("http://example.com/x", &[]);
        assert_eq!(
            resolve_show_url(&plain, START, STOP, STOP + 60).expect_err("no catchup"),
            "This channel does not provide catch-up streaming."
        );
        // Live right now resolves live.
        assert_eq!(
            resolve_show_url(&ch, START, STOP, START + 10).expect("live"),
            ShowResolution::Live("http://example.com/live/u/p/1.ts".to_string())
        );
    }

    #[test]
    fn flussonic_path_style() {
        let ch = channel(
            "http://example.com/chan/index.m3u8",
            &[
                ("catchup-source", "http://example.com/chan"),
                ("catchup-type", "flussonic"),
                ("catchup-timezone", "UTC"),
                ("stream-id", "chan"),
            ],
        );
        // stream-id "chan" is not digits; source's last segment is not a
        // number either, so the id is appended before the archive stamp.
        let url = build_catchup_url(&ch, START, STOP, STOP + 60).expect("url");
        assert_eq!(url, "http://example.com/chan/chan/20261101180000-60.m3u8");
    }
}
